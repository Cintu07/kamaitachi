//! The phone's own fonts, from /system/fonts. Nothing is bundled. Each file is
//! memory-mapped once and never copied, so a 20 MB CJK font costs only the pages a
//! page of text actually reads, and the kernel can drop those again.
//!
//! A face here is a font file at one weight and style. Glyph ids only mean something
//! inside one face, so the display list stores the face's number next to each glyph.

use std::collections::HashMap;
use std::os::fd::AsRawFd;
use std::sync::{Mutex, OnceLock};
use swash::{CacheKey, FontRef};

/// Where the fonts are. KAMAITACHI_FONTS points elsewhere, for rendering pages on a
/// computer to check the engine without a phone.
fn dir() -> String {
    std::env::var("KAMAITACHI_FONTS").unwrap_or_else(|_| "/system/fonts".to_string())
}

#[derive(Clone, Copy)]
pub struct Face {
    data: &'static [u8],
    offset: u32,
    key: CacheKey,
    /// the weight to ask a variable font for, or 0 for a font with only one
    pub wght: f32,
}

impl Face {
    pub fn font(&self) -> FontRef<'static> {
        FontRef { data: self.data, offset: self.offset, key: self.key }
    }

    /// Settings for a variable font, empty for a static one.
    pub fn settings(&self) -> Vec<(&'static str, f32)> {
        if self.wght > 0.0 { vec![("wght", self.wght)] } else { Vec::new() }
    }

    pub fn has(&self, ch: char) -> bool {
        self.font().charmap().map(ch) != 0
    }
}

#[derive(Default)]
struct Registry {
    faces: Vec<Face>,
    by_name: HashMap<(String, u16, bool), u16>,
    files: HashMap<String, Option<&'static [u8]>>,
    /// which face had a character none of the usual ones have
    fallback: HashMap<char, u16>,
    /// every font file on the phone, Noto first, for the rare character
    all: Vec<String>,
}

fn registry() -> &'static Mutex<Registry> {
    static R: OnceLock<Mutex<Registry>> = OnceLock::new();
    R.get_or_init(|| Mutex::new(Registry::default()))
}

fn map(path: &str) -> Option<&'static [u8]> {
    let f = std::fs::File::open(path).ok()?;
    let len = f.metadata().ok()?.len() as usize;
    if len == 0 {
        return None;
    }
    unsafe {
        let p = libc::mmap(std::ptr::null_mut(), len, libc::PROT_READ, libc::MAP_PRIVATE, f.as_raw_fd(), 0);
        if p == libc::MAP_FAILED {
            return None;
        }
        // mapped for the life of the app; system fonts do not change under it
        Some(std::slice::from_raw_parts(p as *const u8, len))
    }
}

impl Registry {
    fn file(&mut self, name: &str) -> Option<&'static [u8]> {
        if let Some(d) = self.files.get(name) {
            return *d;
        }
        let d = map(&format!("{}/{name}", dir()));
        self.files.insert(name.to_string(), d);
        d
    }

    /// The face for a file at a weight, made the first time it is asked for.
    fn face(&mut self, name: &str, weight: u16, italic: bool) -> Option<u16> {
        let k = (name.to_string(), weight, italic);
        if let Some(&id) = self.by_name.get(&k) {
            return Some(id);
        }
        let data = self.file(name)?;
        let font = FontRef::from_index(data, 0)?;
        let variable = font.variations().any(|v| v.tag() == swash::tag_from_str_lossy("wght"));
        let face = Face { data, offset: font.offset, key: font.key, wght: if variable { weight as f32 } else { 0.0 } };
        let id = self.faces.len() as u16;
        self.faces.push(face);
        self.by_name.insert(k, id);
        Some(id)
    }

    /// The first of these files the phone has.
    fn first(&mut self, names: &[&str], weight: u16, italic: bool) -> Option<u16> {
        names.iter().find_map(|n| self.face(n, weight, italic))
    }
}

/// The face for a css font-family list at a weight and style. Only the generic shape
/// of the family matters: phones carry Roboto, Noto Serif and a monospace, and a
/// site's web font is not downloaded.
pub fn pick(family: &str, weight: u16, italic: bool) -> u16 {
    let f = family.to_ascii_lowercase();
    let mono = ["mono", "courier", "consol", "menlo", "code"].iter().any(|m| f.contains(m));
    let serif = !mono && (f.contains("serif") && !f.contains("sans-serif") || ["georgia", "times", "garamond", "palatino", "book"].iter().any(|m| f.contains(m)));
    let bold = weight >= 600;
    let mut r = registry().lock().unwrap_or_else(|e| e.into_inner());
    let id = if mono {
        r.first(&["DroidSansMono.ttf", "CutiveMono.ttf"], 400, false)
    } else if serif {
        let names: &[&str] = match (bold, italic) {
            (true, true) => &["NotoSerif-BoldItalic.ttf", "NotoSerif-Bold.ttf"],
            (true, false) => &["NotoSerif-Bold.ttf", "NotoSerif-Regular.ttf"],
            (false, true) => &["NotoSerif-Italic.ttf", "NotoSerif-Regular.ttf"],
            (false, false) => &["NotoSerif-Regular.ttf"],
        };
        r.first(names, weight, italic)
    } else {
        None
    };
    id.or_else(|| {
        let regular = if italic { r.first(&["Roboto-Italic.ttf", "Roboto-Regular.ttf"], weight, italic)? } else { r.face("Roboto-Regular.ttf", weight, italic)? };
        // newer phones have one variable Roboto that does every weight; older ones
        // have a separate bold file
        if bold && r.faces[regular as usize].wght == 0.0 {
            return r.first(&[if italic { "Roboto-BoldItalic.ttf" } else { "Roboto-Bold.ttf" }], weight, italic).or(Some(regular));
        }
        Some(regular)
    })
    .or_else(|| r.first(&["DroidSans.ttf", "NotoSans-Regular.ttf"], weight, italic))
    .unwrap_or(0)
}

/// A face that has `ch`: the one asked for if it can, otherwise whichever font on the
/// phone does, found once and remembered.
pub fn cover(primary: u16, ch: char) -> u16 {
    let mut r = registry().lock().unwrap_or_else(|e| e.into_inner());
    if r.faces.get(primary as usize).is_some_and(|f| f.has(ch)) || ch.is_whitespace() || ch.is_control() {
        return primary;
    }
    if let Some(&id) = r.fallback.get(&ch) {
        return id;
    }
    if r.all.is_empty() {
        let mut names: Vec<String> = std::fs::read_dir(dir())
            .map(|d| d.flatten().map(|e| e.file_name().to_string_lossy().into_owned()).filter(|n| n.ends_with(".ttf") || n.ends_with(".otf") || n.ends_with(".ttc")).collect())
            .unwrap_or_default();
        // color emoji are bitmaps this engine does not draw yet
        names.retain(|n| !n.contains("Emoji"));
        names.sort_by_key(|n| (!n.starts_with("Noto"), n.clone()));
        r.all = names;
    }
    let names = r.all.clone();
    let found = names.iter().find_map(|n| {
        let id = r.face(n, 400, false)?;
        r.faces[id as usize].has(ch).then_some(id)
    });
    let id = found.unwrap_or(primary);
    r.fallback.insert(ch, id);
    id
}

pub fn get(id: u16) -> Option<Face> {
    registry().lock().unwrap_or_else(|e| e.into_inner()).faces.get(id as usize).copied()
}
