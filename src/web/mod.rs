//! hayate's web engine: html and css to a display list, with no javascript. The pipeline
//! is the one every engine has (Ladybird documents it well): parse to a DOM, cascade
//! styles, lay out boxes in formatting contexts, record a display list. Pixels and text
//! shaping live in `gui`; this module only knows sizes and positions.

pub mod block;
pub mod css;
pub mod dom;
pub mod layout;
pub mod load;
pub mod parse;
pub mod style;

use std::sync::Arc;
pub use style::Rgba;

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl Rect {
    pub fn new(x: f32, y: f32, w: f32, h: f32) -> Rect {
        Rect { x, y, w, h }
    }
    pub fn contains(&self, px: f32, py: f32) -> bool {
        px >= self.x && py >= self.y && px < self.x + self.w && py < self.y + self.h
    }
    pub fn intersect(&self, o: &Rect) -> Rect {
        let x0 = self.x.max(o.x);
        let y0 = self.y.max(o.y);
        let x1 = (self.x + self.w).min(o.x + o.w);
        let y1 = (self.y + self.h).min(o.y + o.h);
        Rect { x: x0, y: y0, w: (x1 - x0).max(0.0), h: (y1 - y0).max(0.0) }
    }
}

/// One positioned glyph, baseline origin in document px.
#[derive(Clone, Copy, Debug)]
pub struct Glyph {
    pub x: f32,
    pub y: f32,
    pub id: u16,
    pub face: u16,
    pub size: f32,
}

#[derive(Clone, Copy, Debug)]
pub enum Item {
    Rect { r: Rect, color: Rgba, radius: f32, clip: u16 },
    /// a border of one width and color, possibly rounded
    Ring { r: Rect, width: f32, color: Rgba, radius: f32, clip: u16 },
    /// glyphs[start..start + len], one color
    Text { start: u32, len: u32, color: Rgba, clip: u16 },
    Image { r: Rect, img: u32, alpha: u8, radius: f32, clip: u16 },
}

impl Item {
    pub fn clip(&self) -> u16 {
        match *self {
            Item::Rect { clip, .. } | Item::Ring { clip, .. } | Item::Text { clip, .. } | Item::Image { clip, .. } => clip,
        }
    }
    pub fn set_clip(&mut self, c: u16) {
        match self {
            Item::Rect { clip, .. } | Item::Ring { clip, .. } | Item::Text { clip, .. } | Item::Image { clip, .. } => *clip = c,
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct Link {
    pub r: Rect,
    pub href: u32,
}

/// What a page draws, in document coordinates (css px).
#[derive(Clone, Default)]
pub struct DisplayList {
    pub items: Vec<Item>,
    pub glyphs: Vec<Glyph>,
    /// clips[0] is "no clip"
    pub clips: Vec<Rect>,
    pub links: Vec<Link>,
    pub hrefs: Vec<String>,
    pub height: f32,
    pub width: f32,
    /// the canvas: the root or body background
    pub bg: Rgba,
    pub title: String,
}

pub const NO_CLIP: Rect = Rect { x: -1.0e9, y: -1.0e9, w: 2.0e9, h: 2.0e9 };

impl DisplayList {
    pub fn new() -> DisplayList {
        DisplayList { clips: vec![NO_CLIP], bg: [255, 255, 255, 255], ..Default::default() }
    }

    /// Approximate heap bytes held.
    pub fn bytes(&self) -> usize {
        self.items.capacity() * std::mem::size_of::<Item>()
            + self.glyphs.capacity() * std::mem::size_of::<Glyph>()
            + self.links.capacity() * std::mem::size_of::<Link>()
            + self.hrefs.iter().map(|h| h.capacity() + 24).sum::<usize>()
    }

    pub fn link_at(&self, x: f32, y: f32) -> Option<&str> {
        self.links.iter().rev().find(|l| l.r.contains(x, y)).map(|l| self.hrefs[l.href as usize].as_str())
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Font {
    pub family: Arc<str>,
    pub size: f32,
    pub weight: u16,
    pub italic: bool,
}

/// A styled range of a paragraph's utf-16 text.
#[derive(Clone, Debug)]
pub struct Run {
    pub start: u32,
    pub len: u32,
    pub font: Font,
    pub color: Rgba,
    /// bit 1 underline, bit 2 line-through
    pub deco: u8,
    /// index into DisplayList::hrefs, or u32::MAX
    pub link: u32,
}

/// An atomic inline box (image, inline-block) sitting on a line at a U+FFFC.
#[derive(Clone, Copy, Debug)]
pub struct Obj {
    pub pos: u32,
    pub w: f32,
    pub h: f32,
}

/// Inline content to break into lines.
#[derive(Clone, Debug, Default)]
pub struct Para {
    pub text: Vec<u16>,
    pub runs: Vec<Run>,
    pub objs: Vec<Obj>,
    /// background ranges (inline elements with a background)
    pub bgs: Vec<(u32, u32, Rgba)>,
    /// 0 start, 1 left, 2 right, 3 center, 4 justify, 5 end
    pub align: u8,
    /// px; 0 for the font's own
    pub line_h: f32,
    pub nowrap: bool,
}

pub struct ParaOut {
    pub height: f32,
    pub width: f32,
    /// top-left of each object, relative to the paragraph origin
    pub objs: Vec<(f32, f32)>,
}

/// Line breaking and glyph placement, provided by the platform (DirectWrite on windows).
pub trait TextEngine {
    /// Lays out `p` in `width` at (x, y). With `out`, glyphs, decorations and link rects
    /// are recorded there; without it this only measures.
    fn layout(&mut self, p: &Para, width: f32, x: f32, y: f32, out: Option<&mut DisplayList>) -> ParaOut;
    /// (min-content, max-content) widths.
    fn widths(&mut self, p: &Para) -> (f32, f32);
}
