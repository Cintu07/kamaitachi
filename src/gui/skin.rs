//! Skins: the colors the chrome is drawn in. A skin is twenty-odd numbers, so having
//! five of them costs nothing, and switching is just a different struct.
//!
//! joseon is the default: warm paper, graphite, cobalt ink and a wash of sky blue,
//! drawn by hand (see brush.rs). night is the dark amber look kamaitachi had first.
//! The other three are earned by blocking things.

use crate::web::Rgba;
use std::cell::Cell;

#[derive(Clone, Copy, Debug)]
pub struct Skin {
    pub name: &'static str,
    /// what you have to do before it unlocks, as ads and trackers blocked
    pub unlock_at: u64,
    pub light: bool,
    /// drawn in pencil and ink on paper, when the cpu renderer is on
    pub drawn: bool,
    /// the tab strip, the toolbar under it, and the address field inside that
    pub strip: Rgba,
    pub panel: Rgba,
    pub field: Rgba,
    pub hover: Rgba,
    pub key: Rgba,
    pub key_hover: Rgba,
    pub text: Rgba,
    pub muted: Rgba,
    pub faint: Rgba,
    pub accent: Rgba,
    /// the address text, and its dimmer path
    pub ink: Rgba,
    pub ink_dim: Rgba,
    /// the light for a tab in the full engine
    pub full: Rgba,
    pub bevel_hi: Rgba,
    pub bevel_lo: Rgba,
    pub led_off: Rgba,
    /// scanline alpha over the chrome; 0 for none
    pub scan: u8,
}

pub const JOSEON: Skin = Skin {
    name: "joseon",
    unlock_at: 0,
    light: true,
    drawn: true,
    // a desk, a sheet of paper on it, and a fresher sheet for the address
    strip: [0xe8, 0xe1, 0xd1, 255],
    panel: [0xf6, 0xf2, 0xe7, 255],
    field: [0xfb, 0xf8, 0xf1, 255],
    hover: [0xe6, 0xde, 0xcb, 255],
    key: [0xef, 0xe9, 0xdb, 255],
    key_hover: [0xe6, 0xde, 0xcb, 255],
    // warm black for words, graphite for the rest
    text: [0x23, 0x21, 0x1d, 255],
    muted: [0x66, 0x61, 0x58, 255],
    faint: [0xa3, 0x9c, 0x8e, 255],
    // the sky blue is for washes; the cobalt is the ink
    accent: [0x9c, 0xc8, 0xea, 255],
    ink: [0x2a, 0x3d, 0x91, 255],
    ink_dim: [0x78, 0x82, 0xae, 255],
    full: [0x3a, 0x8c, 0xc4, 255],
    bevel_hi: [255, 255, 255, 210],
    bevel_lo: [0x8b, 0x77, 0x55, 55],
    led_off: [0xd9, 0xd1, 0xc0, 255],
    scan: 0,
};

pub const NIGHT: Skin = Skin {
    name: "night",
    unlock_at: 0,
    light: false,
    drawn: false,
    strip: [0x08, 0x08, 0x0b, 255],
    panel: [0x11, 0x11, 0x15, 255],
    field: [0x06, 0x06, 0x08, 255],
    hover: [0x22, 0x21, 0x26, 255],
    key: [0x17, 0x17, 0x1c, 255],
    key_hover: [0x22, 0x21, 0x26, 255],
    text: [0xec, 0xeb, 0xe6, 255],
    muted: [0x8b, 0x8a, 0x85, 255],
    faint: [0x4f, 0x4e, 0x4b, 255],
    accent: [0xff, 0x6b, 0x1a, 255],
    ink: [0xff, 0xc8, 0x8f, 255],
    ink_dim: [0x8f, 0x72, 0x56, 255],
    full: [0x6f, 0xe3, 0xff, 255],
    bevel_hi: [255, 255, 255, 18],
    bevel_lo: [0, 0, 0, 170],
    led_off: [0x34, 0x33, 0x36, 255],
    scan: 4,
};

pub const WAVE: Skin = Skin {
    name: "wave",
    unlock_at: 100,
    light: false,
    drawn: false,
    strip: [0x10, 0x1a, 0x33, 255],
    panel: [0x17, 0x24, 0x44, 255],
    field: [0x0b, 0x12, 0x26, 255],
    hover: [0x22, 0x33, 0x5c, 255],
    key: [0x1c, 0x2b, 0x52, 255],
    key_hover: [0x26, 0x38, 0x66, 255],
    text: [0xf2, 0xea, 0xd8, 255],
    muted: [0xa8, 0xb0, 0xc4, 255],
    faint: [0x5d, 0x6a, 0x8c, 255],
    accent: [0xe9, 0xe1, 0xcc, 255],
    ink: [0xf2, 0xea, 0xd8, 255],
    ink_dim: [0x8f, 0x9d, 0xbf, 255],
    full: [0x7f, 0xc8, 0xf0, 255],
    bevel_hi: [255, 255, 255, 22],
    bevel_lo: [0, 0, 0, 150],
    led_off: [0x30, 0x3d, 0x60, 255],
    scan: 3,
};

pub const NEON: Skin = Skin {
    name: "neon",
    unlock_at: 1_000,
    light: false,
    drawn: false,
    strip: [0x0c, 0x06, 0x14, 255],
    panel: [0x14, 0x0a, 0x22, 255],
    field: [0x08, 0x04, 0x0e, 255],
    hover: [0x2a, 0x14, 0x40, 255],
    key: [0x1d, 0x0f, 0x30, 255],
    key_hover: [0x2a, 0x14, 0x40, 255],
    text: [0xf4, 0xe8, 0xff, 255],
    muted: [0xa9, 0x8c, 0xc4, 255],
    faint: [0x5a, 0x45, 0x75, 255],
    accent: [0xff, 0x3d, 0xb4, 255],
    ink: [0x5d, 0xf2, 0xff, 255],
    ink_dim: [0x8a, 0x6a, 0xb0, 255],
    full: [0x5d, 0xf2, 0xff, 255],
    bevel_hi: [255, 160, 255, 26],
    bevel_lo: [0, 0, 0, 170],
    led_off: [0x35, 0x22, 0x4a, 255],
    scan: 6,
};

/// the four greens of the 1989 handheld, nothing else
pub const GAMEBOY: Skin = Skin {
    name: "gameboy",
    unlock_at: 10_000,
    light: true,
    drawn: false,
    strip: [0x8b, 0xac, 0x0f, 255],
    panel: [0x9b, 0xbc, 0x0f, 255],
    field: [0xa8, 0xc6, 0x30, 255],
    hover: [0x8b, 0xac, 0x0f, 255],
    key: [0x9b, 0xbc, 0x0f, 255],
    key_hover: [0x8b, 0xac, 0x0f, 255],
    text: [0x0f, 0x38, 0x0f, 255],
    muted: [0x30, 0x62, 0x30, 255],
    faint: [0x30, 0x62, 0x30, 180],
    accent: [0x0f, 0x38, 0x0f, 255],
    ink: [0x0f, 0x38, 0x0f, 255],
    ink_dim: [0x30, 0x62, 0x30, 255],
    full: [0x0f, 0x38, 0x0f, 255],
    bevel_hi: [0xc8, 0xe0, 0x60, 160],
    bevel_lo: [0x0f, 0x38, 0x0f, 90],
    led_off: [0x8b, 0xac, 0x0f, 255],
    scan: 10,
};

pub const ALL: [Skin; 5] = [JOSEON, NIGHT, WAVE, NEON, GAMEBOY];

thread_local! {
    static CURRENT: Cell<Skin> = const { Cell::new(JOSEON) };
}

/// The skin the window is drawn with right now.
pub fn sk() -> Skin {
    CURRENT.with(|c| c.get())
}

fn file() -> std::path::PathBuf {
    let base = std::env::var("APPDATA").unwrap_or_else(|_| ".".into());
    std::path::Path::new(&base).join("hayate").join("skin.txt")
}

pub fn by_name(name: &str) -> Option<Skin> {
    ALL.iter().copied().find(|s| s.name == name)
}

/// Puts on a skin and remembers it for next time.
pub fn wear(s: Skin) {
    CURRENT.with(|c| c.set(s));
    let _ = std::fs::write(file(), s.name);
}

/// Whatever skin was on last time, if it is still unlocked.
pub fn load(blocked: u64) {
    let name = std::fs::read_to_string(file()).unwrap_or_default();
    if let Some(s) = by_name(name.trim()).filter(|s| s.unlock_at <= blocked) {
        CURRENT.with(|c| c.set(s));
    }
}

/// The next skin you have unlocked, for cycling with a key.
pub fn next(blocked: u64) -> Skin {
    let cur = sk().name;
    let open: Vec<Skin> = ALL.iter().copied().filter(|s| s.unlock_at <= blocked).collect();
    let i = open.iter().position(|s| s.name == cur).unwrap_or(0);
    open[(i + 1) % open.len()]
}
