//! One drawing interface over two backends: the cpu renderer (the default; no gpu
//! driver loaded) and Direct3D 11 (images and glyphs live in gpu memory).

use super::gpu::{Clip, Gpu};
use super::soft::Soft;
use windows::core::Result;
use windows::Win32::Foundation::HWND;

pub enum Painter {
    Gpu(Box<Gpu>),
    Soft(Box<Soft>),
}

#[derive(Clone)]
pub enum Image {
    Gpu(super::gpu::Image),
    Soft(super::soft::Image),
}

/// Which backend HAYATE_RENDER asks for: "cpu" (default), "gpu", "discrete".
pub fn wanted() -> &'static str {
    match std::env::var("HAYATE_RENDER").as_deref() {
        Ok("gpu") => "gpu",
        Ok("discrete") => "discrete",
        _ => "cpu",
    }
}

impl Painter {
    pub fn new(hwnd: HWND) -> Result<Painter> {
        Ok(match wanted() {
            "cpu" => Painter::Soft(Box::new(Soft::new(hwnd))),
            m => Painter::Gpu(Box::new(Gpu::new(hwnd, m == "discrete")?)),
        })
    }

    /// (image bytes, glyph cache bytes) held in this process
    pub fn held(&self) -> (usize, usize) {
        match self {
            Painter::Gpu(_) => (0, 0),
            Painter::Soft(s) => (s.texture_bytes, s.glyph_bytes),
        }
    }

    pub fn name(&self) -> String {
        match self {
            Painter::Gpu(g) => g.adapter.clone(),
            Painter::Soft(_) => "cpu".into(),
        }
    }

    pub fn resize(&mut self, w: u32, h: u32) {
        match self {
            Painter::Gpu(g) => g.resize(w, h),
            Painter::Soft(s) => s.resize(w, h),
        }
    }

    pub fn image(&mut self, w: u32, h: u32, bgra: &[u8]) -> Result<Image> {
        Ok(match self {
            Painter::Gpu(g) => Image::Gpu(g.image(w, h, bgra)?),
            Painter::Soft(s) => Image::Soft(s.image(w, h, bgra)?),
        })
    }

    pub fn begin(&mut self, clear: [u8; 4]) {
        match self {
            Painter::Gpu(g) => g.begin(clear),
            Painter::Soft(s) => s.begin(clear),
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub fn rect(&mut self, x: f32, y: f32, w: f32, h: f32, color: [u8; 4], radius: f32, clip: Clip) {
        match self {
            Painter::Gpu(g) => g.rect(x, y, w, h, color, radius, clip),
            Painter::Soft(s) => s.rect(x, y, w, h, color, radius, clip),
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub fn ring(&mut self, x: f32, y: f32, w: f32, h: f32, width: f32, color: [u8; 4], radius: f32, clip: Clip) {
        match self {
            Painter::Gpu(g) => g.ring(x, y, w, h, width, color, radius, clip),
            Painter::Soft(s) => s.ring(x, y, w, h, width, color, radius, clip),
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub fn draw_image(&mut self, img: &Image, x: f32, y: f32, w: f32, h: f32, alpha: f32, radius: f32, clip: Clip) {
        match (self, img) {
            (Painter::Gpu(g), Image::Gpu(i)) => g.draw_image(i, x, y, w, h, alpha, radius, clip),
            (Painter::Soft(s), Image::Soft(i)) => s.draw_image(i, x, y, w, h, alpha, radius, clip),
            _ => {}
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub fn glyph(&mut self, face: u16, size: f32, id: u16, x: f32, y: f32, color: [f32; 4], clip: Clip) {
        match self {
            Painter::Gpu(g) => g.glyph(face, size, id, x, y, color, clip),
            Painter::Soft(s) => s.glyph(face, size, id, x, y, color, clip),
        }
    }

    pub fn end(&mut self) -> Result<()> {
        match self {
            Painter::Gpu(g) => g.end(),
            Painter::Soft(s) => s.end(),
        }
    }

    pub fn draw(&mut self) -> Result<()> {
        match self {
            Painter::Gpu(g) => g.draw(),
            Painter::Soft(s) => s.draw(),
        }
    }

    pub fn capture(&mut self) -> Result<(u32, u32, Vec<u8>)> {
        match self {
            Painter::Gpu(g) => g.capture(),
            Painter::Soft(s) => s.capture(),
        }
    }
}
