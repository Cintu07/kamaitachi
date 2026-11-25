//! Pixels without a gpu driver: the same boxes, rings, glyphs and images, drawn by the
//! cpu into one buffer and handed to the window with one GDI call. On an integrated gpu
//! the Direct3D driver alone commits over 100 mb; this costs the frame (w * h * 4 bytes)
//! plus a glyph cache. The drawing itself lives in crate::frame, shared with Android;
//! what stays here is DirectWrite for glyphs and GDI for the screen.

use crate::frame::{text_lut, Frame, Mask};
use std::collections::HashMap;
use windows::core::Result;
use windows::Win32::Foundation::HWND;
use windows::Win32::Graphics::DirectWrite::*;
use windows::Win32::Graphics::Dwm::DwmFlush;
use windows::Win32::Graphics::Gdi::*;

pub use crate::frame::{Clip, Image};

#[derive(Hash, PartialEq, Eq, Clone, Copy)]
struct GKey {
    face: u16,
    size: u32,
    id: u16,
    sub: u8,
}

pub struct Soft {
    hwnd: HWND,
    frame: Frame,
    index: HashMap<GKey, u32>,
    store: Vec<Mask>,
    pub glyph_bytes: usize,
    lut: [u8; 256],
    dw: IDWriteFactory2,
    pub texture_bytes: usize,
}

impl Soft {
    pub fn new(hwnd: HWND) -> Soft {
        Soft { hwnd, frame: Frame::default(), index: HashMap::new(), store: Vec::new(), glyph_bytes: 0, lut: text_lut(), dw: super::text::factory(), texture_bytes: 0 }
    }

    pub fn resize(&mut self, w: u32, h: u32) {
        self.frame.resize(w, h);
    }

    pub fn image(&mut self, w: u32, h: u32, bgra: &[u8]) -> Result<Image> {
        let img = Image::from_bgra(w, h, bgra);
        self.texture_bytes += img.px.len() * 4;
        Ok(img)
    }

    /// The frame itself, for the brush to draw on.
    pub fn pixels(&mut self) -> (&mut [u32], u32, u32) {
        (&mut self.frame.buf, self.frame.w, self.frame.h)
    }

    pub fn begin(&mut self, clear: [u8; 4]) {
        self.frame.fill(clear);
    }

    #[allow(clippy::too_many_arguments)]
    pub fn rect(&mut self, x: f32, y: f32, w: f32, h: f32, color: [u8; 4], radius: f32, clip: Clip) {
        self.frame.rect(x, y, w, h, color, radius, clip);
    }

    #[allow(clippy::too_many_arguments)]
    pub fn ring(&mut self, x: f32, y: f32, w: f32, h: f32, width: f32, color: [u8; 4], radius: f32, clip: Clip) {
        self.frame.ring(x, y, w, h, width, color, radius, clip);
    }

    #[allow(clippy::too_many_arguments)]
    pub fn draw_image(&mut self, img: &Image, x: f32, y: f32, w: f32, h: f32, alpha: f32, radius: f32, clip: Clip) {
        self.frame.image(img, x, y, w, h, alpha, radius, clip);
    }

    #[allow(clippy::too_many_arguments)]
    pub fn glyph(&mut self, face: u16, size: f32, id: u16, x: f32, y: f32, color: [f32; 4], clip: Clip) {
        let xf = x.floor();
        let sub = ((x - xf) * 4.0).floor() as u8 & 3;
        let key = GKey { face, size: (size * 16.0).round() as u32, id, sub };
        let gi = match self.index.get(&key) {
            Some(&i) => i,
            None => {
                if self.glyph_bytes > 8 << 20 {
                    self.index.clear();
                    self.store.clear();
                    self.glyph_bytes = 0;
                }
                let g = self.raster(face, size, id, sub).unwrap_or_else(Mask::empty);
                self.glyph_bytes += g.a.len() + 32;
                self.store.push(g);
                let i = (self.store.len() - 1) as u32;
                self.index.insert(key, i);
                i
            }
        };
        self.frame.mask(&self.store[gi as usize], xf as i64, y.round() as i64, color, clip, &self.lut);
    }

    fn raster(&mut self, face: u16, size: f32, id: u16, sub: u8) -> Option<Mask> {
        let f = super::text::face(face)?;
        let adv = 0.0f32;
        let mut run = DWRITE_GLYPH_RUN {
            fontFace: std::mem::ManuallyDrop::new(Some(f.0.clone())),
            fontEmSize: size,
            glyphCount: 1,
            glyphIndices: &id,
            glyphAdvances: &adv,
            glyphOffsets: std::ptr::null(),
            isSideways: false.into(),
            bidiLevel: 0,
        };
        let a = unsafe {
            self.dw.CreateGlyphRunAnalysis(
                &run,
                None,
                if size < 20.0 { DWRITE_RENDERING_MODE_NATURAL } else { DWRITE_RENDERING_MODE_NATURAL_SYMMETRIC },
                DWRITE_MEASURING_MODE_NATURAL,
                DWRITE_GRID_FIT_MODE_DEFAULT,
                DWRITE_TEXT_ANTIALIAS_MODE_GRAYSCALE,
                sub as f32 / 4.0,
                0.0,
            )
        };
        unsafe { std::mem::ManuallyDrop::drop(&mut run.fontFace) };
        let a = a.ok()?;
        let b = unsafe { a.GetAlphaTextureBounds(DWRITE_TEXTURE_ALIASED_1x1).ok()? };
        let w = (b.right - b.left).max(0) as u32;
        let h = (b.bottom - b.top).max(0) as u32;
        if w == 0 || h == 0 || w > 600 || h > 600 {
            return Some(Mask::empty());
        }
        let mut px = vec![0u8; (w * h) as usize];
        unsafe { a.CreateAlphaTexture(DWRITE_TEXTURE_ALIASED_1x1, &b, &mut px).ok()? };
        Some(Mask { w: w as u16, h: h as u16, left: b.left as i16, top: b.top as i16, a: px.into_boxed_slice() })
    }

    pub fn draw(&mut self) -> Result<()> {
        Ok(())
    }

    pub fn present(&mut self) -> Result<()> {
        let f = &self.frame;
        unsafe {
            // wait for the compositor's next frame, so animation runs at the display rate
            let _ = DwmFlush();
            let hdc = GetDC(Some(self.hwnd));
            let bmi = BITMAPINFO {
                bmiHeader: BITMAPINFOHEADER {
                    biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                    biWidth: f.w as i32,
                    biHeight: -(f.h as i32),
                    biPlanes: 1,
                    biBitCount: 32,
                    biCompression: BI_RGB.0,
                    ..Default::default()
                },
                ..Default::default()
            };
            SetDIBitsToDevice(hdc, 0, 0, f.w, f.h, 0, 0, 0, f.h, f.buf.as_ptr() as *const _, &bmi, DIB_RGB_COLORS);
            ReleaseDC(Some(self.hwnd), hdc);
        }
        Ok(())
    }

    pub fn end(&mut self) -> Result<()> {
        self.present()
    }

    pub fn capture(&mut self) -> Result<(u32, u32, Vec<u8>)> {
        let f = &self.frame;
        Ok((f.w, f.h, f.buf.iter().flat_map(|p| (p | 0xFF00_0000).to_le_bytes()).collect()))
    }
}
