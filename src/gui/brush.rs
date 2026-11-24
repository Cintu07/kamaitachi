//! Pencil, ink stamps, hatching and watercolor for the chrome, drawn by the cpu
//! straight into the frame.
//!
//! The look is borrowed from p5.brush (github.com/acamposuribe/p5.brush). That
//! library is javascript on WebGL, so using it as is would mean a Chromium process
//! just to draw the toolbar, a hundred MB or more. The ideas fit in plain loops,
//! though. A pencil line is a row of small round stamps along a path that wobbles a
//! little, and the grain of the paper decides how much graphite each pixel catches,
//! so light strokes break up and heavy ones fill in. An ink stamp has edges that
//! follow the grain. Watercolor is the same shape filled a dozen times, each time
//! pushed out of true, the way Tyler Hobbs paints them.
//!
//! Two things are kept between frames: the 64 KB tile of paper grain, and the
//! shape of each watercolor wash (about 70 KB each), which takes 25 ms to work out
//! and only changes when the window does. Everything else lives for one stroke.

use crate::web::Rgba;
use std::cell::RefCell;
use std::sync::OnceLock;

pub type Pt = (f32, f32);

const TILE: usize = 256;

fn hash(x: i32, y: i32, seed: u32) -> u32 {
    let mut h = (x as u32).wrapping_mul(0x8da6_b343) ^ (y as u32).wrapping_mul(0xd816_3841) ^ seed.wrapping_mul(0xcb1a_b31f);
    h ^= h >> 15;
    h = h.wrapping_mul(0x2c1b_3c6d);
    h ^= h >> 12;
    h = h.wrapping_mul(0x297a_2d39);
    h ^ (h >> 15)
}

/// A number in 0..1 that is always the same for the same (x, y, seed).
fn rnd(x: i32, y: i32, seed: u32) -> f32 {
    (hash(x, y, seed) >> 8) as f32 / 16_777_216.0
}

fn smooth(t: f32) -> f32 {
    t * t * (3.0 - 2.0 * t)
}

/// Smooth noise on a lattice that repeats every (px, py) cells, so a tile built from
/// it has no seams.
fn wrap_noise(x: f32, y: f32, px: i32, py: i32, seed: u32) -> f32 {
    let (xi, yi) = (x.floor() as i32, y.floor() as i32);
    let (fx, fy) = (smooth(x - xi as f32), smooth(y - yi as f32));
    let at = |i: i32, j: i32| rnd(i.rem_euclid(px), j.rem_euclid(py), seed);
    let top = at(xi, yi) + (at(xi + 1, yi) - at(xi, yi)) * fx;
    let bot = at(xi, yi + 1) + (at(xi + 1, yi + 1) - at(xi, yi + 1)) * fx;
    top + (bot - top) * fy
}

/// Smooth noise along a line, for the wobble and pressure of a stroke.
fn noise1(t: f32, seed: u32) -> f32 {
    let i = t.floor() as i32;
    let f = smooth(t - i as f32);
    let a = rnd(i, 0, seed);
    a + (rnd(i + 1, 0, seed) - a) * f
}

/// The paper's surface, 0 in the valleys and 255 on the peaks. Fine grit, a softer
/// layer of bumps, and long fibres lying across the sheet.
fn tooth_tile() -> &'static [u8] {
    static T: OnceLock<Box<[u8]>> = OnceLock::new();
    T.get_or_init(|| {
        let n = TILE as i32;
        let mut t = vec![0u8; TILE * TILE];
        for y in 0..n {
            for x in 0..n {
                let fine = rnd(x, y, 7);
                let bump = wrap_noise(x as f32 / 2.0, y as f32 / 2.0, n / 2, n / 2, 11);
                let fibre = wrap_noise(x as f32 / 16.0, y as f32 / 4.0, n / 16, n / 4, 13);
                let v = 0.42 * bump + 0.33 * fine + 0.25 * fibre;
                t[(y * n + x) as usize] = (((v - 0.5) * 1.9 + 0.5).clamp(0.0, 1.0) * 255.0) as u8;
            }
        }
        t.into_boxed_slice()
    })
}

#[inline]
fn tooth(tile: &[u8], x: i32, y: i32) -> f32 {
    tile[(y as usize & (TILE - 1)) * TILE + (x as usize & (TILE - 1))] as f32 / 255.0
}

thread_local! {
    /// washes already worked out, by (seed, where they sit)
    static WASHES: RefCell<Vec<(u64, std::rc::Rc<Layer>)>> = const { RefCell::new(Vec::new()) };
}

/// Coverage for one stroke or shape, over just the box it touches. `s` is how many
/// device pixels one cell spans: 1 for pencil, more for soft washes, which have no
/// detail worth paying full resolution for.
struct Layer {
    x0: i32,
    y0: i32,
    w: i32,
    h: i32,
    s: f32,
    a: Vec<u8>,
}

impl Layer {
    /// A layer over the points' bounding box plus `pad`, cut to the clip. None if
    /// nothing of it would show.
    fn around(pts: &[Pt], pad: f32, s: f32, clip: [i32; 4]) -> Option<Layer> {
        let (mut x0, mut y0, mut x1, mut y1) = (f32::MAX, f32::MAX, f32::MIN, f32::MIN);
        for &(x, y) in pts {
            x0 = x0.min(x);
            y0 = y0.min(y);
            x1 = x1.max(x);
            y1 = y1.max(y);
        }
        let x0 = ((x0 - pad).floor() as i32).max(clip[0]);
        let y0 = ((y0 - pad).floor() as i32).max(clip[1]);
        let x1 = ((x1 + pad).ceil() as i32).min(clip[2]);
        let y1 = ((y1 + pad).ceil() as i32).min(clip[3]);
        if x1 <= x0 || y1 <= y0 {
            return None;
        }
        let w = ((x1 - x0) as f32 / s).ceil() as i32 + 1;
        let h = ((y1 - y0) as f32 / s).ceil() as i32 + 1;
        Some(Layer { x0, y0, w, h, s, a: vec![0; (w * h) as usize] })
    }

    /// The device-pixel box this layer covers.
    fn span(&self) -> (i32, i32, i32, i32) {
        (self.x0, self.y0, self.x0 + ((self.w - 1) as f32 * self.s) as i32, self.y0 + ((self.h - 1) as f32 * self.s) as i32)
    }

    /// Coverage at a device pixel, 0..1, read smoothly between cells.
    fn at(&self, x: i32, y: i32) -> f32 {
        if self.s == 1.0 {
            let (lx, ly) = (x - self.x0, y - self.y0);
            if lx < 0 || ly < 0 || lx >= self.w || ly >= self.h {
                return 0.0;
            }
            return self.a[(ly * self.w + lx) as usize] as f32 / 255.0;
        }
        let fx = (x - self.x0) as f32 / self.s;
        let fy = (y - self.y0) as f32 / self.s;
        let (ix, iy) = (fx.floor() as i32, fy.floor() as i32);
        let get = |i: i32, j: i32| if i < 0 || j < 0 || i >= self.w || j >= self.h { 0.0 } else { self.a[(j * self.w + i) as usize] as f32 };
        let (tx, ty) = (fx - ix as f32, fy - iy as f32);
        let top = get(ix, iy) + (get(ix + 1, iy) - get(ix, iy)) * tx;
        let bot = get(ix, iy + 1) + (get(ix + 1, iy + 1) - get(ix, iy + 1)) * tx;
        (top + (bot - top) * ty) / 255.0
    }

    /// Fills a polygon (device px, even-odd) with four samples per row. `add` piles
    /// coverage up, for washes; otherwise the larger value wins.
    fn fill(&mut self, poly: &[Pt], weight: f32, add: bool) {
        let n = poly.len();
        if n < 3 {
            return;
        }
        let local: Vec<Pt> = poly.iter().map(|&(x, y)| ((x - self.x0 as f32) / self.s, (y - self.y0 as f32) / self.s)).collect();
        let mut row = vec![0f32; self.w as usize];
        let mut xs: Vec<f32> = Vec::with_capacity(16);
        let wf = self.w as f32;
        for r in 0..self.h {
            row.iter_mut().for_each(|v| *v = 0.0);
            let mut any = false;
            for sub in 0..4 {
                let yy = r as f32 + (sub as f32 + 0.5) / 4.0 - 0.5;
                xs.clear();
                for i in 0..n {
                    let (a, b) = (local[i], local[(i + 1) % n]);
                    if (a.1 <= yy && b.1 > yy) || (b.1 <= yy && a.1 > yy) {
                        xs.push(a.0 + (yy - a.1) * (b.0 - a.0) / (b.1 - a.1));
                    }
                }
                xs.sort_by(|a, b| a.total_cmp(b));
                // even-odd: inside between the first and second crossing, the third and fourth...
                for j in (0..xs.len() / 2 * 2).step_by(2) {
                    let (a, b) = ((xs[j] + 0.5).clamp(0.0, wf), (xs[j + 1] + 0.5).clamp(0.0, wf));
                    if b <= a {
                        continue;
                    }
                    any = true;
                    let (ia, ib, last) = (a as usize, b as usize, row.len());
                    if ia == ib {
                        row[ia.min(last - 1)] += (b - a) * 0.25;
                        continue;
                    }
                    row[ia] += (ia as f32 + 1.0 - a) * 0.25;
                    for v in &mut row[ia + 1..ib.min(last)] {
                        *v += 0.25;
                    }
                    if ib < last {
                        row[ib] += (b - ib as f32) * 0.25;
                    }
                }
            }
            if !any {
                continue;
            }
            let base = (r * self.w) as usize;
            for (i, v) in row.iter().enumerate() {
                if *v <= 0.0 {
                    continue;
                }
                let c = (v.min(1.0) * weight * 255.0) as u32;
                let d = &mut self.a[base + i];
                *d = if add { (*d as u32 + c).min(255) as u8 } else { (*d).max(c.min(255) as u8) };
            }
        }
    }

    /// Box blur, `r` cells each way, along rows and then columns.
    fn blur(&mut self, r: i32) {
        let (w, h) = (self.w as usize, self.h as usize);
        let mut tmp = vec![0u8; self.a.len()];
        let n = (2 * r + 1) as u32;
        for y in 0..h {
            let row = &self.a[y * w..(y + 1) * w];
            for x in 0..w {
                let (lo, hi) = (x.saturating_sub(r as usize), (x + r as usize).min(w - 1));
                tmp[y * w + x] = (row[lo..=hi].iter().map(|&v| v as u32).sum::<u32>() / n) as u8;
            }
        }
        for x in 0..w {
            for y in 0..h {
                let (lo, hi) = (y.saturating_sub(r as usize), (y + r as usize).min(h - 1));
                self.a[y * w + x] = ((lo..=hi).map(|j| tmp[j * w + x] as u32).sum::<u32>() / n) as u8;
            }
        }
    }

    /// A pencil line along `pts` (device px): small discs every fraction of the width,
    /// pushed sideways by slow noise and pressed harder in the middle than at the ends.
    fn stroke(&mut self, pts: &[Pt], width: f32, wobble: f32, seed: u32) {
        if pts.len() < 2 {
            return;
        }
        let total: f32 = pts.windows(2).map(|p| dist(p[0], p[1])).sum();
        if total <= 0.0 {
            return;
        }
        let step = (width * 0.3).clamp(0.3, 1.5);
        // short marks taper less, or a small cross would be all ends and no middle
        let taper = (width * 3.0 + 3.0).min(total * 0.2);
        let mut d = 0.0;
        for seg in pts.windows(2) {
            let (a, b) = (seg[0], seg[1]);
            let len = dist(a, b);
            if len <= 0.0 {
                continue;
            }
            let (tx, ty) = ((b.0 - a.0) / len, (b.1 - a.1) / len);
            let mut t = 0.0;
            while t < len {
                let at = d + t;
                let off = (noise1(at / 38.0, seed) - 0.5) * 2.0 * wobble;
                let ends = smooth((at.min(total - at) / taper).clamp(0.0, 1.0));
                let press = (0.45 + 0.55 * ends) * (0.82 + 0.36 * noise1(at / 21.0, seed ^ 0x55));
                let (x, y) = (a.0 + tx * t - ty * off, a.1 + ty * t + tx * off);
                self.disc(x, y, (width * 0.5 * press).max(0.45), press.min(1.0));
                t += step;
            }
            d += len;
        }
    }

    fn disc(&mut self, cx: f32, cy: f32, r: f32, v: f32) {
        let (lx, ly) = (cx - self.x0 as f32, cy - self.y0 as f32);
        let x0 = ((lx - r - 1.0).floor() as i32).max(0);
        let x1 = ((lx + r + 1.0).ceil() as i32).min(self.w - 1);
        let y0 = ((ly - r - 1.0).floor() as i32).max(0);
        let y1 = ((ly + r + 1.0).ceil() as i32).min(self.h - 1);
        for y in y0..=y1 {
            for x in x0..=x1 {
                let dd = ((x as f32 - lx).powi(2) + (y as f32 - ly).powi(2)).sqrt();
                let c = ((r + 0.5 - dd).clamp(0.0, 1.0) * v * 255.0) as u8;
                let p = &mut self.a[(y * self.w + x) as usize];
                *p = (*p).max(c);
            }
        }
    }
}

fn dist(a: Pt, b: Pt) -> f32 {
    ((b.0 - a.0).powi(2) + (b.1 - a.1).powi(2)).sqrt()
}

/// A drawing as ink coverage, one byte a pixel: 255 where the pen went, 0 for bare
/// paper. The skin decides the color when it is printed.
pub struct Mask {
    pub w: u32,
    pub h: u32,
    pub a: Vec<u8>,
}

/// What a pencil line looks like: how wide, how dark, and how unsteady the hand.
#[derive(Clone, Copy)]
pub struct Pencil {
    pub width: f32,
    pub color: Rgba,
    pub alpha: f32,
    pub wobble: f32,
}

/// The frame as something to draw on, in css px. `k` turns css px into device px.
pub struct Canvas<'a> {
    px: &'a mut [u32],
    w: i32,
    h: i32,
    pub k: f32,
    clip: [i32; 4],
    tile: &'static [u8],
}

impl<'a> Canvas<'a> {
    pub fn new(px: &'a mut [u32], w: u32, h: u32, k: f32) -> Canvas<'a> {
        Canvas { px, w: w as i32, h: h as i32, k, clip: [0, 0, w as i32, h as i32], tile: tooth_tile() }
    }

    /// Only draw inside this box (css px) until the next call.
    pub fn clip(&mut self, x: f32, y: f32, w: f32, h: f32) {
        let k = self.k;
        self.clip = [((x * k) as i32).max(0), ((y * k) as i32).max(0), (((x + w) * k).ceil() as i32).min(self.w), (((y + h) * k).ceil() as i32).min(self.h)];
    }

    pub fn unclip(&mut self) {
        self.clip = [0, 0, self.w, self.h];
    }

    fn dev(&self, pts: &[Pt]) -> Vec<Pt> {
        pts.iter().map(|&(x, y)| (x * self.k, y * self.k)).collect()
    }

    #[inline]
    fn blend(&mut self, x: i32, y: i32, c: Rgba, a: f32) {
        if x < self.clip[0] || y < self.clip[1] || x >= self.clip[2] || y >= self.clip[3] || a <= 0.0 {
            return;
        }
        let i = (y * self.w + x) as usize;
        let d = self.px[i];
        let a = (a * c[3] as f32 / 255.0).min(1.0);
        let mix = |s: u8, dv: u32| (s as f32 * a + dv as f32 * (1.0 - a) + 0.5) as u32;
        self.px[i] = (mix(c[0], (d >> 16) & 255) << 16) | (mix(c[1], (d >> 8) & 255) << 8) | mix(c[2], d & 255);
    }

    /// Paper: the base color, with its grain showing by `grain` (about 0.03 to 0.08).
    pub fn paper(&mut self, x: f32, y: f32, w: f32, h: f32, base: Rgba, grain: f32) {
        let k = self.k;
        let x0 = ((x * k) as i32).max(self.clip[0]);
        let y0 = ((y * k) as i32).max(self.clip[1]);
        let x1 = (((x + w) * k).ceil() as i32).min(self.clip[2]);
        let y1 = (((y + h) * k).ceil() as i32).min(self.clip[3]);
        let amp = grain * 255.0;
        // the grain only has 256 heights, so work out the color for each one first.
        // valleys come out a little darker and warmer, as paper does under a lamp
        let lut: [u32; 256] = std::array::from_fn(|i| {
            let t = i as f32 / 255.0 - 0.5;
            let ch = |c: u8, f: f32| (c as f32 + t * amp * f).clamp(0.0, 255.0) as u32;
            (ch(base[0], 0.9) << 16) | (ch(base[1], 1.0) << 8) | ch(base[2], 1.25)
        });
        for yy in y0..y1 {
            let row = (yy * self.w) as usize;
            let trow = &self.tile[(yy as usize & (TILE - 1)) * TILE..][..TILE];
            for xx in x0..x1 {
                self.px[row + xx as usize] = lut[trow[xx as usize & (TILE - 1)] as usize];
            }
        }
    }

    /// A pencil line through `pts` (css px).
    pub fn pencil(&mut self, pts: &[Pt], p: Pencil, seed: u32) {
        let d = self.dev(pts);
        let (w, wob) = (p.width * self.k, p.wobble * self.k);
        let Some(mut l) = Layer::around(&d, w + wob + 2.0, 1.0, self.clip) else { return };
        l.stroke(&d, w, wob, seed);
        self.graphite(&l, p.color, p.alpha, None);
    }

    /// The same line drawn twice by a slightly different hand, which is how a quick
    /// sketch looks.
    pub fn sketch(&mut self, pts: &[Pt], p: Pencil, seed: u32) {
        self.pencil(pts, p, seed);
        let lighter = Pencil { alpha: p.alpha * 0.55, wobble: p.wobble * 1.6 + 0.3, ..p };
        self.pencil(pts, lighter, seed.wrapping_add(7919));
    }

    /// A box drawn the way people draw boxes: four lines that run a little past the
    /// corners.
    pub fn sketch_box(&mut self, x: f32, y: f32, w: f32, h: f32, p: Pencil, seed: u32) {
        let o = 3.0;
        let sides = [[(x - o, y), (x + w + o * 0.4, y)], [(x + w, y - o * 0.5), (x + w, y + h + o)], [(x + w + o * 0.3, y + h), (x - o * 0.6, y + h)], [(x, y + h + o * 0.4), (x, y - o)]];
        for (i, s) in sides.iter().enumerate() {
            self.pencil(s, p, seed.wrapping_add(i as u32 * 31));
        }
    }

    fn graphite(&mut self, l: &Layer, c: Rgba, alpha: f32, mask: Option<&Layer>) {
        let (x0, y0, x1, y1) = l.span();
        for y in y0..=y1 {
            for x in x0..=x1 {
                let cov = l.at(x, y);
                if cov <= 0.0 {
                    continue;
                }
                // a light touch only marks the peaks of the grain; pressing harder
                // reaches down into the valleys too
                let t = tooth(self.tile, x, y);
                let mut g = (cov * 1.35 - (1.0 - t) * 0.75).clamp(0.0, 1.0);
                if let Some(m) = mask {
                    g *= m.at(x, y);
                }
                self.blend(x, y, c, alpha * g);
            }
        }
    }

    /// A rubber stamp or a risograph print: flat ink in the shape, with edges that
    /// follow the paper and a few spots where the ink did not take.
    pub fn stamp(&mut self, poly: &[Pt], c: Rgba, alpha: f32, seed: u32) {
        let d = self.dev(poly);
        let Some(mut l) = Layer::around(&d, 2.0, 1.0, self.clip) else { return };
        l.fill(&d, 1.0, false);
        let (x0, y0, x1, y1) = l.span();
        for y in y0..=y1 {
            for x in x0..=x1 {
                let cov = l.at(x, y);
                if cov <= 0.0 {
                    continue;
                }
                let t = tooth(self.tile, x, y);
                let edge = ((cov - 0.5) * 1.8 + 0.5 + (t - 0.5) * 0.6).clamp(0.0, 1.0);
                let mut body = 0.78 + 0.22 * t;
                if rnd(x, y, seed) < 0.045 {
                    body *= 0.45;
                }
                self.blend(x, y, c, alpha * edge * body);
            }
        }
    }

    /// Parallel pencil lines across a shape, `gap` css px apart at `angle` degrees.
    pub fn hatch(&mut self, poly: &[Pt], angle: f32, gap: f32, p: Pencil, seed: u32) {
        let d = self.dev(poly);
        let Some(mut mask) = Layer::around(&d, 1.0, 1.0, self.clip) else { return };
        mask.fill(&d, 1.0, false);
        let Some(mut lines) = Layer::around(&d, p.width * self.k + 2.0, 1.0, self.clip) else { return };
        let (x0, y0, x1, y1) = mask.span();
        let (cx, cy) = ((x0 + x1) as f32 / 2.0, (y0 + y1) as f32 / 2.0);
        let reach = dist((x0 as f32, y0 as f32), (x1 as f32, y1 as f32)) / 2.0 + 4.0;
        let (s, c) = angle.to_radians().sin_cos();
        let step = gap * self.k;
        let mut o = -reach;
        let mut i = 0u32;
        while o <= reach {
            // each line starts and stops a little differently, as a hand would
            let jit = (rnd(i as i32, 1, seed) - 0.5) * step * 0.35;
            let (px, py) = (cx - s * (o + jit), cy + c * (o + jit));
            let a = (px - c * reach, py - s * reach);
            let b = (px + c * reach, py + s * reach);
            lines.stroke(&[a, b], p.width * self.k, p.wobble * self.k, seed.wrapping_add(i * 97));
            o += step;
            i += 1;
        }
        self.graphite(&lines, p.color, p.alpha, Some(&mask));
    }

    /// Prints part of a drawn plate: `src` is (x, y, w, h) in the mask, and it lands
    /// in the css box (x, y, w, h), scaled smoothly. `grain` is how much the paper
    /// shows through the ink, about 0.15 for a pen line and 0.5 for watercolor.
    #[allow(clippy::too_many_arguments)]
    pub fn print(&mut self, m: &Mask, src: [u32; 4], x: f32, y: f32, w: f32, h: f32, c: Rgba, alpha: f32, grain: f32) {
        let k = self.k;
        let (dx0, dy0) = ((x * k).round() as i32, (y * k).round() as i32);
        let (dw, dh) = ((w * k).round().max(1.0) as i32, (h * k).round().max(1.0) as i32);
        let (x0, y0) = (dx0.max(self.clip[0]), dy0.max(self.clip[1]));
        let (x1, y1) = ((dx0 + dw).min(self.clip[2]), (dy0 + dh).min(self.clip[3]));
        if x1 <= x0 || y1 <= y0 || src[2] == 0 || src[3] == 0 {
            return;
        }
        if src[2] as i32 == dw && src[3] as i32 == dh {
            self.print_exact(m, src, dx0, dy0, (x0, y0, x1, y1), c, alpha, grain);
            return;
        }
        // 16.16 fixed point steps through the source, read between pixels
        let sx = ((src[2] as u64) << 16) / dw as u64;
        let sy = ((src[3] as u64) << 16) / dh as u64;
        let mw = m.w as usize;
        let (lastx, lasty) = (src[0] + src[2] - 1, src[1] + src[3] - 1);
        let ca = (alpha * c[3] as f32 / 255.0 * 256.0) as u32;
        let g = (grain * 256.0) as u32;
        let (cr, cg, cb) = (c[0] as u32, c[1] as u32, c[2] as u32);
        for yy in y0..y1 {
            let fy = ((yy - dy0) as u64 * sy + (sy >> 1)).saturating_sub(1 << 15);
            let iy = (src[1] + (fy >> 16) as u32).min(lasty) as usize;
            let iy1 = (iy as u32 + 1).min(lasty) as usize;
            let ty = ((fy >> 8) & 255) as u32;
            let trow = &self.tile[(yy as usize & (TILE - 1)) * TILE..][..TILE];
            let row = (yy * self.w) as usize;
            for xx in x0..x1 {
                let fx = ((xx - dx0) as u64 * sx + (sx >> 1)).saturating_sub(1 << 15);
                let ix = (src[0] + (fx >> 16) as u32).min(lastx) as usize;
                let ix1 = (ix as u32 + 1).min(lastx) as usize;
                let tx = ((fx >> 8) & 255) as u32;
                let top = m.a[iy * mw + ix] as u32 * (256 - tx) + m.a[iy * mw + ix1] as u32 * tx;
                let bot = m.a[iy1 * mw + ix] as u32 * (256 - tx) + m.a[iy1 * mw + ix1] as u32 * tx;
                let v = (top * (256 - ty) + bot * ty) >> 16;
                if v < 3 {
                    continue;
                }
                // the valleys of the paper take a little less ink
                let t = trow[xx as usize & (TILE - 1)] as u32;
                let paper = 256 - ((g * (255 - t)) >> 8);
                let a = ((((v * ca) >> 8) * paper) >> 8).min(255);
                let i = row + xx as usize;
                let d = self.px[i];
                let mix = |s: u32, dv: u32| (s * a + dv * (255 - a) + 127) / 255;
                self.px[i] = (mix(cr, (d >> 16) & 255) << 16) | (mix(cg, (d >> 8) & 255) << 8) | mix(cb, d & 255);
            }
        }
    }

    /// print() for a plate already decoded at the size it is drawn: one read a pixel.
    #[allow(clippy::too_many_arguments)]
    fn print_exact(&mut self, m: &Mask, src: [u32; 4], dx0: i32, dy0: i32, (x0, y0, x1, y1): (i32, i32, i32, i32), c: Rgba, alpha: f32, grain: f32) {
        let ca = (alpha * c[3] as f32 / 255.0 * 256.0) as u32;
        let g = (grain * 256.0) as u32;
        let (cr, cg, cb) = (c[0] as u32, c[1] as u32, c[2] as u32);
        let mw = m.w as usize;
        for yy in y0..y1 {
            let srow = &m.a[(src[1] as usize + (yy - dy0) as usize) * mw..][..mw];
            let trow = &self.tile[(yy as usize & (TILE - 1)) * TILE..][..TILE];
            let row = (yy * self.w) as usize;
            for xx in x0..x1 {
                let v = srow[src[0] as usize + (xx - dx0) as usize] as u32;
                if v < 3 {
                    continue;
                }
                let t = trow[xx as usize & (TILE - 1)] as u32;
                let paper = 256 - ((g * (255 - t)) >> 8);
                let a = ((((v * ca) >> 8) * paper) >> 8).min(255);
                let i = row + xx as usize;
                let d = self.px[i];
                let mix = |s: u32, dv: u32| (s * a + dv * (255 - a) + 127) / 255;
                self.px[i] = (mix(cr, (d >> 16) & 255) << 16) | (mix(cg, (d >> 8) & 255) << 8) | mix(cb, d & 255);
            }
        }
    }

    /// Watercolor: the shape pushed out of true a dozen times, each layer adding a
    /// little pigment, so the middle is even and the edge goes soft and uneven.
    /// It has no fine detail, so it is worked out at a third of the resolution.
    pub fn wash(&mut self, poly: &[Pt], c: Rgba, alpha: f32, seed: u32) {
        let d = self.dev(poly);
        let size = {
            let (mut x0, mut x1) = (f32::MAX, f32::MIN);
            for p in &d {
                x0 = x0.min(p.0);
                x1 = x1.max(p.0);
            }
            x1 - x0
        };
        let key = d.iter().fold(seed as u64 ^ ((self.clip[2] as u64) << 32), |h, p| {
            (h ^ (p.0 as i32 as u64) ^ ((p.1 as i32 as u64) << 20)).wrapping_mul(0x100_0000_01b3)
        });
        let cached = WASHES.with(|w| w.borrow().iter().find(|e| e.0 == key).map(|e| e.1.clone()));
        let l = match cached {
            Some(l) => l,
            None => {
                let mut rng = seed | 1;
                let base = deform(&d, 3, 0.16, &mut rng);
                let Some(mut l) = Layer::around(&base, size * 0.14, 3.0, self.clip) else { return };
                let layers = 16;
                for _ in 0..layers {
                    let shape = deform(&base, 4, 0.3, &mut rng);
                    l.fill(&shape, 1.25 / layers as f32, true);
                }
                // the paper is wet, so the pigment creeps a little past where it was put
                l.blur(2);
                l.blur(2);
                let l = std::rc::Rc::new(l);
                WASHES.with(|w| {
                    let mut w = w.borrow_mut();
                    // a resize leaves old shapes behind; four is plenty for one sheet
                    if w.len() >= 4 {
                        w.remove(0);
                    }
                    w.push((key, l.clone()));
                });
                l
            }
        };
        // spread the coarse cells back over the pixels, one row at a time, in 8.8
        // fixed point. this loop touches a quarter of the window, so it is kept plain
        let (x0, y0, x1, y1) = l.span();
        let (x0, y0) = (x0.max(self.clip[0]), y0.max(self.clip[1]));
        let (x1, y1) = (x1.min(self.clip[2] - 1), y1.min(self.clip[3] - 1));
        let inv = 256.0 / l.s;
        let lw = l.w as usize;
        let ca = (alpha * c[3] as f32 / 255.0 * 256.0) as u32;
        let (cr, cg, cb) = (c[0] as u32, c[1] as u32, c[2] as u32);
        for y in y0..=y1 {
            let fy = ((y - l.y0) as f32 * inv) as u32;
            let (iy, ty) = ((fy >> 8) as usize, fy & 255);
            if iy + 1 >= l.h as usize {
                continue;
            }
            let (r0, r1) = (&l.a[iy * lw..(iy + 1) * lw], &l.a[(iy + 1) * lw..(iy + 2) * lw]);
            let trow = &self.tile[(y as usize & (TILE - 1)) * TILE..][..TILE];
            let row = (y * self.w) as usize;
            for x in x0..=x1 {
                let fx = ((x - l.x0) as f32 * inv) as u32;
                let (ix, tx) = ((fx >> 8) as usize, fx & 255);
                if ix + 1 >= lw {
                    continue;
                }
                let top = r0[ix] as u32 * (256 - tx) + r0[ix + 1] as u32 * tx;
                let bot = r1[ix] as u32 * (256 - tx) + r1[ix + 1] as u32 * tx;
                let v = (top * (256 - ty) + bot * ty) >> 16;
                if v < 2 {
                    continue;
                }
                // pigment settles into the valleys of the paper
                let t = trow[x as usize & (TILE - 1)] as u32;
                let a = (((v * ca) >> 8) * (294 - t * 115 / 255)) >> 8;
                let a = a.min(255);
                let i = row + x as usize;
                let d = self.px[i];
                let mix = |s: u32, dv: u32| (s * a + dv * (255 - a) + 127) / 255;
                self.px[i] = (mix(cr, (d >> 16) & 255) << 16) | (mix(cg, (d >> 8) & 255) << 8) | mix(cb, d & 255);
            }
        }
    }
}

fn gauss(rng: &mut u32) -> f32 {
    let mut s = 0.0;
    for _ in 0..3 {
        *rng ^= *rng << 13;
        *rng ^= *rng >> 17;
        *rng ^= *rng << 5;
        s += (*rng >> 8) as f32 / 16_777_216.0;
    }
    (s - 1.5) * 1.15
}

/// Splits every edge at its middle and pushes the new point off by a random amount
/// that scales with the edge. Done a few times this turns a clean shape into a
/// believable blot.
fn deform(poly: &[Pt], depth: u32, spread: f32, rng: &mut u32) -> Vec<Pt> {
    let mut cur = poly.to_vec();
    let mut sp = spread;
    for _ in 0..depth {
        let n = cur.len();
        let mut next = Vec::with_capacity(n * 2);
        for i in 0..n {
            let (a, b) = (cur[i], cur[(i + 1) % n]);
            next.push(a);
            let len = dist(a, b);
            let (gx, gy) = (gauss(rng), gauss(rng));
            next.push(((a.0 + b.0) / 2.0 + gx * sp * len, (a.1 + b.1) / 2.0 + gy * sp * len));
        }
        cur = next;
        sp *= 0.75;
    }
    cur
}

/// Points around an ellipse (css px).
pub fn ellipse(cx: f32, cy: f32, rx: f32, ry: f32, n: usize) -> Vec<Pt> {
    (0..n).map(|i| {
        let a = i as f32 / n as f32 * std::f32::consts::TAU;
        (cx + rx * a.cos(), cy + ry * a.sin())
    })
    .collect()
}

/// An ellipse drawn as one open stroke that overlaps where it started, like a ring
/// drawn round something on a form.
pub fn loop_around(cx: f32, cy: f32, rx: f32, ry: f32, start: f32) -> Vec<Pt> {
    let n = 40;
    (0..=n + 6)
        .map(|i| {
            let a = start + i as f32 / n as f32 * std::f32::consts::TAU;
            // the hand drifts outward as it comes round
            let grow = 1.0 + i as f32 / n as f32 * 0.07;
            (cx + rx * grow * a.cos(), cy + ry * grow * a.sin())
        })
        .collect()
}

/// A star with `points` points, the inner corners at `inner` of the radius.
pub fn star(cx: f32, cy: f32, r: f32, inner: f32, points: usize, turn: f32) -> Vec<Pt> {
    (0..points * 2)
        .map(|i| {
            let a = turn + i as f32 / (points * 2) as f32 * std::f32::consts::TAU - std::f32::consts::FRAC_PI_2;
            let rr = if i % 2 == 0 { r } else { r * inner };
            (cx + rr * a.cos(), cy + rr * a.sin())
        })
        .collect()
}

/// An outward spiral, for whirlwinds.
pub fn spiral(cx: f32, cy: f32, r0: f32, r1: f32, turns: f32, start: f32) -> Vec<Pt> {
    let n = (turns * 48.0) as usize;
    (0..=n)
        .map(|i| {
            let t = i as f32 / n as f32;
            let a = start + t * turns * std::f32::consts::TAU;
            let r = r0 + (r1 - r0) * t;
            (cx + r * a.cos(), cy + r * a.sin())
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grain_tile_wraps_without_a_seam() {
        let t = tooth_tile();
        // neighbours across the tile's edge differ no more than neighbours inside it
        let across: f32 = (0..TILE).map(|y| (t[y * TILE] as f32 - t[y * TILE + TILE - 1] as f32).abs()).sum::<f32>() / TILE as f32;
        let inside: f32 = (0..TILE).map(|y| (t[y * TILE + 100] as f32 - t[y * TILE + 101] as f32).abs()).sum::<f32>() / TILE as f32;
        assert!(across < inside * 1.6, "across {across} inside {inside}");
    }

    #[test]
    fn a_filled_square_covers_its_inside_and_nothing_else() {
        let mut px = vec![0xffff_ffffu32; 64 * 64];
        let mut c = Canvas::new(&mut px, 64, 64, 1.0);
        c.stamp(&[(16.0, 16.0), (48.0, 16.0), (48.0, 48.0), (16.0, 48.0)], [0, 0, 0, 255], 1.0, 1);
        let dark = |x: usize, y: usize| px[y * 64 + x] & 0xff < 200;
        assert!(dark(32, 32));
        assert!(!dark(4, 4) && !dark(60, 32) && !dark(32, 60));
    }

    #[test]
    fn a_stroke_marks_its_path() {
        let mut px = vec![0xffff_ffffu32; 100 * 20];
        let mut c = Canvas::new(&mut px, 100, 20, 1.0);
        c.pencil(&[(10.0, 10.0), (90.0, 10.0)], Pencil { width: 2.0, color: [0, 0, 0, 255], alpha: 1.0, wobble: 0.0 }, 3);
        let marked = (20..80).filter(|&x| (9..=11).any(|y| px[y * 100 + x] & 0xff < 230)).count();
        assert!(marked > 50, "{marked} of 60 columns");
        assert!((0..100).all(|x| px[x] & 0xff == 255), "row 0 stays clean");
    }
}
