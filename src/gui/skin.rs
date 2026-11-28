//! The colours the window is drawn in: marble by day, slate at night, and one accent.
//! The colours themselves, and why they are what they are, are in colour.rs.

use super::colour;
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

/// The look before the settings are read: marble with Aegean.
const START: Skin = Skin {
    light: true,
    frame: [0xf3, 0xf1, 0xec, 255],
    field: [0xff, 0xff, 0xff, 255],
    wash: [0xe6, 0xe2, 0xda, 255],
    hover: [0xec, 0xe9, 0xe3, 255],
    text: [0x1b, 0x22, 0x2c, 255],
    muted: [0x6b, 0x70, 0x78, 255],
    faint: [0xb9, 0xb6, 0xae, 255],
    hairline: [0xdf, 0xdb, 0xd3, 255],
    accent: [0x1f, 0x6f, 0xc0, 255],
    ink: [0x1f, 0x6f, 0xc0, 255],
    gold: [0xb3, 0x8b, 0x3e, 255],
};

/// A look built from one of the colour ladders and an accent.
fn build(light: bool, accent: usize) -> Skin {
    let l = if light { colour::marble() } else { colour::night() };
    let a = colour::accent(accent, light);
    Skin { light, frame: l.frame, field: l.field, wash: l.wash, hover: l.hover, text: l.text, muted: l.muted, faint: l.faint, hairline: l.hairline, accent: a, ink: a, gold: l.gold }
}

/// (name, by day, at night) for each accent, for the settings page.
pub fn accents() -> Vec<(&'static str, Rgba, Rgba)> {
    colour::ACCENT_HUES.iter().enumerate().map(|(i, a)| (a.0, colour::accent(i, true), colour::accent(i, false))).collect()
}

thread_local! {
    static CURRENT: Cell<Skin> = const { Cell::new(START) };
    /// whether this window can be glass: only the cpu painter keeps per-pixel alpha
    static GLASS: Cell<bool> = const { Cell::new(false) };
}

/// Says whether the window can show glass at all.
pub fn glass_possible(on: bool) {
    GLASS.with(|g| g.set(on));
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

/// (frame, card, lines) of a look, for the small pictures on the settings page.
pub fn preview(light: bool) -> (Rgba, Rgba, Rgba) {
    let s = build(light, 0);
    (s.frame, s.field, s.faint)
}

/// Works out the colours from the settings. Call at start and after a setting changes.
pub fn refresh() {
    let p = prefs::get();
    let light = match p.theme {
        Theme::Light => true,
        Theme::Dark => false,
        Theme::System => windows_light(),
    };
    let mut s = build(light, p.accent);
    if p.contrast {
        s.muted = s.text;
    }
    // on glass the marble is a tint over whatever is behind the window
    if p.glass && GLASS.with(|g| g.get()) {
        s.frame[3] = (p.tint * 255 / 100) as u8;
    }
    CURRENT.with(|c| c.set(s));
}
