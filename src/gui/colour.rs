//! How the window's colours are chosen.
//!
//! Every colour is written in OKLCH: lightness, chroma (how strong the colour is) and
//! hue. Unlike RGB or HSL, equal steps in OKLCH lightness look like equal steps to the
//! eye, and two colours with the same L and C look equally strong whatever their hue.
//! That gives four rules the palette is built on:
//!
//! 1. One hue for all the neutrals. The marble look is a warm stone, hue 80, with
//!    chroma kept under 0.012, so the greys never turn yellow or blue; the night look
//!    is a cool slate, hue 255. Neutrals are a ladder of lightness steps, each with
//!    one job: the frame, the card, a hovered row, the row you are on, a hairline.
//! 2. Text is measured, not guessed. Body text and secondary text clear WCAG's 4.5:1
//!    on every surface they sit on, and faint text, which only labels empty boxes,
//!    clears 2:1. The tests below check each pair.
//! 3. Accents share one lightness and one chroma and differ only by hue, so Aegean,
//!    laurel and terracotta have the same weight on screen and any of them reads on
//!    the card. Text on an accent clears 4.5:1 too.
//! 4. Colour is kept for meaning: the accent marks focus, the tab you are on, what is
//!    loading and what is chosen. Gold is only for the Greek key.

use crate::web::Rgba;

/// sRGB, gamma encoded, from OKLCH (hue in degrees).
pub fn oklch(l: f32, c: f32, h: f32) -> Rgba {
    let (a, b) = (c * h.to_radians().cos(), c * h.to_radians().sin());
    let l_ = l + 0.396_337_78 * a + 0.215_803_76 * b;
    let m_ = l - 0.105_561_346 * a - 0.063_854_17 * b;
    let s_ = l - 0.089_484_18 * a - 1.291_485_5 * b;
    let (lc, mc, sc) = (l_ * l_ * l_, m_ * m_ * m_, s_ * s_ * s_);
    let r = 4.076_741_7 * lc - 3.307_711_6 * mc + 0.230_969_94 * sc;
    let g = -1.268_438 * lc + 2.609_757_4 * mc - 0.341_319_38 * sc;
    let bl = -0.004_196_086 * lc - 0.703_418_6 * mc + 1.707_614_7 * sc;
    let enc = |v: f32| {
        let v = v.clamp(0.0, 1.0);
        let e = if v <= 0.003_130_8 { 12.92 * v } else { 1.055 * v.powf(1.0 / 2.4) - 0.055 };
        (e * 255.0).round() as u8
    };
    [enc(r), enc(g), enc(bl), 255]
}

/// WCAG relative luminance.
pub fn luminance(c: Rgba) -> f32 {
    let lin = |v: u8| {
        let v = v as f32 / 255.0;
        if v <= 0.040_45 { v / 12.92 } else { ((v + 0.055) / 1.055).powf(2.4) }
    };
    0.2126 * lin(c[0]) + 0.7152 * lin(c[1]) + 0.0722 * lin(c[2])
}

/// WCAG contrast ratio, 1 to 21.
#[cfg(test)]
pub fn contrast(a: Rgba, b: Rgba) -> f32 {
    let (x, y) = (luminance(a), luminance(b));
    (x.max(y) + 0.05) / (x.min(y) + 0.05)
}

/// The neutral ladder of one look, lightest surface first for marble.
pub struct Ladder {
    pub frame: Rgba,
    pub field: Rgba,
    pub hover: Rgba,
    pub wash: Rgba,
    pub hairline: Rgba,
    pub faint: Rgba,
    pub muted: Rgba,
    pub text: Rgba,
    pub gold: Rgba,
}

/// Warm stone: hue 80, barely any chroma.
pub fn marble() -> Ladder {
    let n = |l: f32, c: f32| oklch(l, c, 80.0);
    Ladder {
        frame: n(0.958, 0.006),
        field: n(0.995, 0.002),
        hover: n(0.928, 0.007),
        wash: n(0.900, 0.009),
        hairline: n(0.885, 0.008),
        faint: n(0.690, 0.010),
        muted: n(0.480, 0.012),
        // the text leans to the sea, so it reads as ink rather than mud
        text: oklch(0.250, 0.022, 255.0),
        gold: oklch(0.660, 0.105, 78.0),
    }
}

/// Cool slate: hue 255, a little more chroma so the dark does not look dead.
pub fn night() -> Ladder {
    let n = |l: f32, c: f32| oklch(l, c, 255.0);
    Ladder {
        frame: n(0.190, 0.014),
        field: n(0.232, 0.015),
        hover: n(0.262, 0.016),
        wash: n(0.300, 0.018),
        hairline: n(0.315, 0.015),
        faint: n(0.520, 0.014),
        muted: n(0.720, 0.012),
        text: n(0.945, 0.006),
        gold: oklch(0.780, 0.100, 82.0),
    }
}

/// (name, hue, chroma): all share one lightness per look, so they weigh the same.
/// Chroma is lower where the colour would leave sRGB at that lightness.
pub const ACCENT_HUES: [(&str, f32, f32); 6] = [("Aegean", 248.0, 0.135), ("Laurel", 160.0, 0.105), ("Olive", 122.0, 0.100), ("Gold", 80.0, 0.105), ("Terracotta", 38.0, 0.130), ("Porphyry", 322.0, 0.125)];

/// An accent for a look: darker by day so it reads on white, lighter at night.
pub fn accent(i: usize, light: bool) -> Rgba {
    let (_, h, c) = ACCENT_HUES[i.min(ACCENT_HUES.len() - 1)];
    if light { oklch(0.515, c, h) } else { oklch(0.760, c * 0.85, h) }
}

/// Text on an accent: white by day, the night's frame at night.
pub fn on_accent(light: bool) -> Rgba {
    if light { [255, 255, 255, 255] } else { night().frame }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn white_and_black_come_out_as_they_are() {
        assert_eq!(oklch(1.0, 0.0, 0.0), [255, 255, 255, 255]);
        assert_eq!(oklch(0.0, 0.0, 0.0), [0, 0, 0, 255]);
    }

    #[test]
    fn text_reads_on_every_surface() {
        for (name, l) in [("marble", marble()), ("night", night())] {
            for (surface, bg) in [("frame", l.frame), ("field", l.field), ("hover", l.hover), ("wash", l.wash)] {
                assert!(contrast(l.text, bg) >= 10.0, "{name} text on {surface}: {}", contrast(l.text, bg));
                assert!(contrast(l.muted, bg) >= 4.5, "{name} muted on {surface}: {}", contrast(l.muted, bg));
                assert!(contrast(l.faint, bg) >= 2.0, "{name} faint on {surface}: {}", contrast(l.faint, bg));
            }
            // each step of the ladder is visible against the one before it
            assert!(contrast(l.frame, l.hover) > 1.05 && contrast(l.hover, l.wash) > 1.05, "{name} ladder");
        }
    }

    #[test]
    fn accents_weigh_the_same_and_carry_text() {
        for light in [true, false] {
            let (field, on) = (if light { marble().field } else { night().field }, on_accent(light));
            let lums: Vec<f32> = (0..ACCENT_HUES.len()).map(|i| luminance(accent(i, light))).collect();
            let (lo, hi) = (lums.iter().cloned().fold(1.0, f32::min), lums.iter().cloned().fold(0.0, f32::max));
            assert!(hi / lo < 1.6, "accents differ in weight: {lums:?}");
            for i in 0..ACCENT_HUES.len() {
                let a = accent(i, light);
                assert!(contrast(a, field) >= 3.0, "accent {i} on the card ({light}): {}", contrast(a, field));
                assert!(contrast(on, a) >= 4.5, "text on accent {i} ({light}): {}", contrast(on, a));
            }
        }
    }
}
