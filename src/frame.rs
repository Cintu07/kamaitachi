//! Pixels on the cpu: boxes with rounded corners, rings, images and glyph masks,
//! drawn into one buffer of 0RGB words. The Windows renderer (gui/soft.rs) and the
//! Android app both draw with this; the only difference between them is how the
//! buffer reaches the screen.

use std::sync::Arc;

/// A clip box in device px: [x0, y0, x1, y1].
pub type Clip = [f32; 4];

pub const NO_CLIP: Clip = [-1.0e9, -1.0e9, 1.0e9, 1.0e9];

/// A decoded image, premultiplied BGRA packed as ARGB words.
#[derive(Clone)]
pub struct Image {
    pub px: Arc<Vec<u32>>,
    pub w: u32,
    pub h: u32,
}

impl Image {
    /// From premultiplied BGRA bytes, the way image decoders hand them over.
    pub fn from_bgra(w: u32, h: u32, bgra: &[u8]) -> Image {
        let px: Vec<u32> = bgra.chunks_exact(4).map(|c| u32::from_le_bytes([c[0], c[1], c[2], c[3]])).collect();
        Image { px: Arc::new(px), w, h }
    }
}

/// A glyph as coverage, 0..255 a pixel. `left` and `top` place it relative to the
/// pen, which sits on the baseline.
pub struct Mask {
    pub w: u16,
    pub h: u16,
    pub left: i16,
    pub top: i16,
    pub a: Box<[u8]>,
}

impl Mask {
    pub fn empty() -> Mask {
        Mask { w: 0, h: 0, left: 0, top: 0, a: Box::new([]) }
    }
}

/// Premultiplied 0..255 channels.
#[derive(Clone, Copy)]
struct Px {
    r: u32,
    g: u32,
    b: u32,
    a: u32,
}

fn premul(c: [u8; 4]) -> Px {
    let a = c[3] as u32;
    Px { r: c[0] as u32 * a / 255, g: c[1] as u32 * a / 255, b: c[2] as u32 * a / 255, a }
}

#[inline]
fn over(d: u32, s: Px) -> u32 {
    if s.a == 0 {
        return d;
    }
    if s.a >= 255 {
        return (s.r << 16) | (s.g << 8) | s.b;
    }
    let ia = 255 - s.a;
    let dr = (d >> 16) & 255;
    let dg = (d >> 8) & 255;
    let db = d & 255;
    let r = s.r + (dr * ia + 127) / 255;
    let g = s.g + (dg * ia + 127) / 255;
    let b = s.b + (db * ia + 127) / 255;
    (r.min(255) << 16) | (g.min(255) << 8) | b.min(255)
}

#[inline]
fn scale(s: Px, cov: u32) -> Px {
    Px { r: s.r * cov / 255, g: s.g * cov / 255, b: s.b * cov / 255, a: s.a * cov / 255 }
}

/// Coverage of a pixel center by a rounded rectangle, 0..1.
#[inline]
fn rr_cov(px: f32, py: f32, cx: f32, cy: f32, hw: f32, hh: f32, r: f32) -> f32 {
    let dx = (px - cx).abs() - hw + r;
    let dy = (py - cy).abs() - hh + r;
    let ox = dx.max(0.0);
    let oy = dy.max(0.0);
    let dist = (ox * ox + oy * oy).sqrt() + dx.max(dy).min(0.0) - r;
    (0.5 - dist).clamp(0.0, 1.0)
}

fn bilinear(img: &Image, fx: f32, fy: f32) -> u32 {
    let x0 = fx.floor().clamp(0.0, (img.w - 1) as f32) as u32;
    let y0 = fy.floor().clamp(0.0, (img.h - 1) as f32) as u32;
    let x1 = (x0 + 1).min(img.w - 1);
    let y1 = (y0 + 1).min(img.h - 1);
    let tx = ((fx - x0 as f32).clamp(0.0, 1.0) * 256.0) as u32;
    let ty = ((fy - y0 as f32).clamp(0.0, 1.0) * 256.0) as u32;
    let p = |x: u32, y: u32| img.px[(y * img.w + x) as usize];
    let lerp = |a: u32, b: u32, t: u32| -> u32 {
        let mut out = 0;
        for sh in [0, 8, 16, 24] {
            let ca = (a >> sh) & 255;
            let cb = (b >> sh) & 255;
            out |= ((ca * (256 - t) + cb * t) >> 8) << sh;
        }
        out
    };
    lerp(lerp(p(x0, y0), p(x1, y0), tx), lerp(p(x0, y1), p(x1, y1), tx), ty)
}

/// The text contrast curve: lifts thin strokes a little so small type stays dark.
pub fn text_lut() -> [u8; 256] {
    let mut lut = [0u8; 256];
    for (i, v) in lut.iter_mut().enumerate() {
        *v = ((i as f32 / 255.0).powf(0.82) * 1.06 * 255.0).round().min(255.0) as u8;
    }
    lut
}

#[derive(Default)]
pub struct Frame {
    pub w: u32,
    pub h: u32,
    pub buf: Vec<u32>,
}

impl Frame {
    pub fn resize(&mut self, w: u32, h: u32) {
        if w == self.w && h == self.h {
            return;
        }
        self.w = w;
        self.h = h;
        self.buf = vec![0; (w * h) as usize];
        self.buf.shrink_to_fit();
    }

    pub fn fill(&mut self, c: [u8; 4]) {
        self.buf.fill(((c[0] as u32) << 16) | ((c[1] as u32) << 8) | c[2] as u32);
    }

    /// Integer pixel bounds of a box inside a clip and the frame.
    fn bounds(&self, x: f32, y: f32, w: f32, h: f32, clip: Clip) -> Option<(usize, usize, usize, usize)> {
        let x0 = x.max(clip[0]).max(0.0).floor() as i64;
        let y0 = y.max(clip[1]).max(0.0).floor() as i64;
        let x1 = (x + w).min(clip[2]).min(self.w as f32).ceil() as i64;
        let y1 = (y + h).min(clip[3]).min(self.h as f32).ceil() as i64;
        (x1 > x0 && y1 > y0).then_some((x0 as usize, y0 as usize, x1 as usize, y1 as usize))
    }

    #[allow(clippy::too_many_arguments)]
    pub fn rect(&mut self, x: f32, y: f32, w: f32, h: f32, color: [u8; 4], radius: f32, clip: Clip) {
        if w <= 0.0 || h <= 0.0 || color[3] == 0 {
            return;
        }
        let r = radius.min(w / 2.0).min(h / 2.0).max(0.0);
        let s = premul(color);
        let stride = self.w as usize;
        if r < 0.5 {
            // crisp: snap to whole pixels
            let (sx, sy) = (x.round(), y.round());
            let (sw, sh) = ((x + w).round() - sx, (y + h).round() - sy);
            let Some((x0, y0, x1, y1)) = self.bounds(sx, sy, sw.max(1.0), sh.max(1.0), clip) else { return };
            for yy in y0..y1 {
                let row = &mut self.buf[yy * stride + x0..yy * stride + x1];
                if s.a >= 255 {
                    row.fill((s.r << 16) | (s.g << 8) | s.b);
                } else {
                    for p in row {
                        *p = over(*p, s);
                    }
                }
            }
            return;
        }
        let Some((x0, y0, x1, y1)) = self.bounds(x, y, w, h, clip) else { return };
        let (cx, cy, hw, hh) = (x + w / 2.0, y + h / 2.0, w / 2.0, h / 2.0);
        for yy in y0..y1 {
            let py = yy as f32 + 0.5;
            let edge_row = py < y + r || py > y + h - r;
            let base = yy * stride;
            for xx in x0..x1 {
                let px = xx as f32 + 0.5;
                let cov = if !edge_row && px > x + r && px < x + w - r { 1.0 } else { rr_cov(px, py, cx, cy, hw, hh, r) };
                if cov <= 0.0 {
                    continue;
                }
                let d = &mut self.buf[base + xx];
                *d = over(*d, if cov >= 1.0 { s } else { scale(s, (cov * 255.0) as u32) });
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub fn ring(&mut self, x: f32, y: f32, w: f32, h: f32, width: f32, color: [u8; 4], radius: f32, clip: Clip) {
        if w <= 0.0 || h <= 0.0 || color[3] == 0 || width <= 0.0 {
            return;
        }
        let r = radius.min(w / 2.0).min(h / 2.0).max(0.0);
        let s = premul(color);
        let Some((x0, y0, x1, y1)) = self.bounds(x, y, w, h, clip) else { return };
        let (cx, cy, hw, hh) = (x + w / 2.0, y + h / 2.0, w / 2.0, h / 2.0);
        let ir = (r - width).max(0.0);
        let (ihw, ihh) = ((hw - width).max(0.0), (hh - width).max(0.0));
        let band = r.max(width) + 1.0;
        let stride = self.w as usize;
        for yy in y0..y1 {
            let py = yy as f32 + 0.5;
            let mid = py > y + band && py < y + h - band;
            for xx in x0..x1 {
                let px = xx as f32 + 0.5;
                if mid && px > x + band && px < x + w - band {
                    continue;
                }
                let outer = rr_cov(px, py, cx, cy, hw, hh, r);
                if outer <= 0.0 {
                    continue;
                }
                let inner = if ihw > 0.0 && ihh > 0.0 { rr_cov(px, py, cx, cy, ihw, ihh, ir) } else { 0.0 };
                let cov = outer * (1.0 - inner);
                if cov <= 0.0 {
                    continue;
                }
                let d = &mut self.buf[yy * stride + xx];
                *d = over(*d, scale(s, (cov * 255.0) as u32));
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub fn image(&mut self, img: &Image, x: f32, y: f32, w: f32, h: f32, alpha: f32, radius: f32, clip: Clip) {
        if w <= 0.0 || h <= 0.0 || img.w == 0 || img.h == 0 {
            return;
        }
        let (x, y) = (x.round(), y.round());
        let Some((x0, y0, x1, y1)) = self.bounds(x, y, w.round().max(1.0), h.round().max(1.0), clip) else { return };
        let a = (alpha.clamp(0.0, 1.0) * 255.0) as u32;
        let r = radius.min(w / 2.0).min(h / 2.0).max(0.0);
        let (cx, cy, hw, hh) = (x + w / 2.0, y + h / 2.0, w / 2.0, h / 2.0);
        let (sx, sy) = (img.w as f32 / w, img.h as f32 / h);
        let exact = (img.w as f32 - w).abs() < 1.0 && (img.h as f32 - h).abs() < 1.0;
        let stride = self.w as usize;
        for yy in y0..y1 {
            let py = yy as f32 + 0.5;
            for xx in x0..x1 {
                let px = xx as f32 + 0.5;
                let s = if exact {
                    let ix = ((xx as f32 - x) as u32).min(img.w - 1);
                    let iy = ((yy as f32 - y) as u32).min(img.h - 1);
                    img.px[(iy * img.w + ix) as usize]
                } else {
                    bilinear(img, (px - x) * sx - 0.5, (py - y) * sy - 0.5)
                };
                let mut sp = Px { r: (s >> 16) & 255, g: (s >> 8) & 255, b: s & 255, a: s >> 24 };
                let mut cov = a;
                if r > 0.5 {
                    cov = (cov as f32 * rr_cov(px, py, cx, cy, hw, hh, r)) as u32;
                }
                if cov < 255 {
                    sp = scale(sp, cov);
                }
                let d = &mut self.buf[yy * stride + xx];
                *d = over(*d, sp);
            }
        }
    }

    /// Blends a glyph mask with its pen at (x, y), in `color` (0..1 channels).
    pub fn mask(&mut self, g: &Mask, x: i64, y: i64, color: [f32; 4], clip: Clip, lut: &[u8; 256]) {
        if g.w == 0 {
            return;
        }
        let gx = x + g.left as i64;
        let gy = y + g.top as i64;
        let cx0 = (clip[0].max(0.0) as i64).max(gx);
        let cy0 = (clip[1].max(0.0) as i64).max(gy);
        let cx1 = (clip[2].min(self.w as f32) as i64).min(gx + g.w as i64);
        let cy1 = (clip[3].min(self.h as f32) as i64).min(gy + g.h as i64);
        if cx1 <= cx0 || cy1 <= cy0 {
            return;
        }
        let s = Px { r: (color[0] * 255.0) as u32, g: (color[1] * 255.0) as u32, b: (color[2] * 255.0) as u32, a: (color[3] * 255.0) as u32 };
        let stride = self.w as usize;
        let gw = g.w as i64;
        for yy in cy0..cy1 {
            let arow = ((yy - gy) * gw) as usize;
            let drow = yy as usize * stride;
            for xx in cx0..cx1 {
                let cov = g.a[arow + (xx - gx) as usize];
                if cov == 0 {
                    continue;
                }
                let c = lut[cov as usize] as u32;
                let d = &mut self.buf[drow + xx as usize];
                *d = over(*d, scale(s, c));
            }
        }
    }
}
