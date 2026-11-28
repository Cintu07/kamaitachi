//! The colours the window is drawn in: white marble with Aegean blue by day, the same
//! sea at night in the dark. Which one, and the accent colour, come from the settings.

use super::prefs::{self, Theme};
use crate::web::Rgba;
use std::cell::Cell;

#[derive(Clone, Copy, Debug)]
pub struct Skin {
    pub light: bool,
    /// the marble the tabs sit on, around the page
    pub frame: Rgba,
    /// panels raised over everything: the address field, menus, notes
    pub field: Rgba,
    /// the tab you are on
    pub wash: Rgba,
    /// whatever is under the pointer
    pub hover: Rgba,
    pub text: Rgba,
    pub muted: Rgba,
    pub faint: Rgba,
    /// one-pixel lines: the card's edge, rules between groups
    pub hairline: Rgba,
    /// the chosen colour, for focus, selection and what is loading
    pub accent: Rgba,
    /// the accent strong enough for text and the caret
    pub ink: Rgba,
    /// gold leaf, for the Greek key and small marks
    pub gold: Rgba,
}

/// (name, by day, at night)
pub const ACCENTS: [(&str, Rgba, Rgba); 6] = [
    ("Aegean", [0x1f, 0x6f, 0xc0, 255], [0x6f, 0xaa, 0xea, 255]),
    ("Olive", [0x5f, 0x75, 0x2e, 255], [0xa8, 0xbf, 0x6a, 255]),
    ("Terracotta", [0xb5, 0x55, 0x2f, 255], [0xe8, 0x8e, 0x66, 255]),
    ("Porphyry", [0x7a, 0x3e, 0x8f, 255], [0xc0, 0x8c, 0xd6, 255]),
    ("Laurel", [0x2f, 0x7a, 0x5c, 255], [0x6c, 0xc4, 0x9c, 255]),
    ("Gold", [0x9a, 0x74, 0x22, 255], [0xd9, 0xb2, 0x5c, 255]),
];

const MARBLE: Skin = Skin {
    light: true,
    frame: [0xf3, 0xf1, 0xec, 255],
    field: [0xff, 0xff, 0xff, 255],
    wash: [0xe6, 0xe2, 0xda, 255],
    hover: [0xec, 0xe9, 0xe3, 255],
    text: [0x1b, 0x22, 0x2c, 255],
    muted: [0x76, 0x7b, 0x84, 255],
    faint: [0xb9, 0xb6, 0xae, 255],
    hairline: [0xdf, 0xdb, 0xd3, 255],
    accent: ACCENTS[0].1,
    ink: ACCENTS[0].1,
    gold: [0xb3, 0x8b, 0x3e, 255],
};

const NIGHT: Skin = Skin {
    light: false,
    frame: [0x13, 0x17, 0x1d, 255],
    field: [0x1f, 0x25, 0x2e, 255],
    wash: [0x2a, 0x31, 0x3b, 255],
    hover: [0x20, 0x26, 0x2f, 255],
    text: [0xe9, 0xe6, 0xdf, 255],
    muted: [0x93, 0x98, 0xa1, 255],
    faint: [0x55, 0x5b, 0x65, 255],
    hairline: [0x2a, 0x30, 0x39, 255],
    accent: ACCENTS[0].2,
    ink: ACCENTS[0].2,
    gold: [0xcf, 0xa9, 0x58, 255],
};

thread_local! {
    static CURRENT: Cell<Skin> = const { Cell::new(MARBLE) };
}

/// The colours the window is drawn with right now.
pub fn sk() -> Skin {
    CURRENT.with(|c| c.get())
}

/// Whether Windows is set to light apps.
fn windows_light() -> bool {
    use windows_sys::Win32::System::Registry::{RegGetValueW, HKEY_CURRENT_USER, RRF_RT_REG_DWORD};
    let key: Vec<u16> = "Software\\Microsoft\\Windows\\CurrentVersion\\Themes\\Personalize\0".encode_utf16().collect();
    let name: Vec<u16> = "AppsUseLightTheme\0".encode_utf16().collect();
    let mut v = 1u32;
    let mut size = 4u32;
    unsafe { RegGetValueW(HKEY_CURRENT_USER, key.as_ptr(), name.as_ptr(), RRF_RT_REG_DWORD, std::ptr::null_mut(), &mut v as *mut u32 as *mut _, &mut size) };
    v != 0
}

/// Works out the colours from the settings. Call at start and after a setting changes.
pub fn refresh() {
    let p = prefs::get();
    let light = match p.theme {
        Theme::Light => true,
        Theme::Dark => false,
        Theme::System => windows_light(),
    };
    let mut s = if light { MARBLE } else { NIGHT };
    let (_, day, night) = ACCENTS[p.accent.min(ACCENTS.len() - 1)];
    s.accent = if light { day } else { night };
    s.ink = s.accent;
    if p.contrast {
        s.muted = s.text;
    }
    CURRENT.with(|c| c.set(s));
}
