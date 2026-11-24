//! The drawn plates: the weasel's five forms, the ten stamps and the doodles for
//! each season. They were made for kamaitachi with an image model as black ink on
//! white, then picked and cleaned up, and live in assets/art as small grayscale
//! PNGs, 77 KB for all of them.
//!
//! Each one is decoded into a Mask (one byte a pixel) the first time it is needed,
//! and the skin colors it while it is printed. The new tab's plates come to about
//! 150 KB decoded and are let go as soon as no new tab is showing. The spirit stays,
//! because it lives in the tab strip; it is 32 KB.

use super::brush::Mask;
use std::cell::RefCell;
use std::rc::Rc;

mod rects {
    include!("../../assets/art/rects.rs");
}
pub use rects::{AUTUMN, SPIRITS, SPRING, SUMMER, WINTER};

const SPIRIT_STRIP: &[u8] = include_bytes!("../../assets/art/spirits.png");
const STAMP_ROW: &[u8] = include_bytes!("../../assets/art/stamps.png");
const SEASONS: [&[u8]; 4] = [
    include_bytes!("../../assets/art/spring.png"),
    include_bytes!("../../assets/art/summer.png"),
    include_bytes!("../../assets/art/autumn.png"),
    include_bytes!("../../assets/art/winter.png"),
];

/// Each stamp is a 96 px square in its row, in the order of play::STAMPS.
pub const STAMP_CELL: u32 = 96;

#[derive(Default)]
struct Plates {
    stamps: Option<Rc<Mask>>,
    season: Option<(usize, Rc<Mask>)>,
    spirits: Option<Rc<Mask>>,
}

thread_local! {
    static PLATES: RefCell<Plates> = RefCell::new(Plates::default());
}

/// Decodes a plate. Ink is dark, so coverage is 255 minus the gray, read from the
/// blue byte of each BGRA pixel.
fn decode(bytes: &[u8]) -> Option<Rc<Mask>> {
    let (w, h, px) = super::image::decode(bytes, u32::MAX, u32::MAX)?;
    Some(Rc::new(Mask { w, h, a: px.iter().step_by(4).map(|b| 255 - b).collect() }))
}

pub fn stamps() -> Option<Rc<Mask>> {
    PLATES.with(|p| {
        let mut p = p.borrow_mut();
        if p.stamps.is_none() {
            p.stamps = decode(STAMP_ROW);
        }
        p.stamps.clone()
    })
}

pub fn spirits() -> Option<Rc<Mask>> {
    PLATES.with(|p| {
        let mut p = p.borrow_mut();
        if p.spirits.is_none() {
            p.spirits = decode(SPIRIT_STRIP);
        }
        p.spirits.clone()
    })
}

/// The doodles for a season: 0 spring, 1 summer, 2 autumn, 3 winter.
pub fn season(q: usize) -> Option<(Rc<Mask>, &'static [[u16; 4]])> {
    let rects: &'static [[u16; 4]] = match q {
        0 => &SPRING,
        1 => &SUMMER,
        2 => &AUTUMN,
        _ => &WINTER,
    };
    PLATES.with(|p| {
        let mut p = p.borrow_mut();
        if p.season.as_ref().is_none_or(|s| s.0 != q) {
            p.season = decode(SEASONS[q.min(3)]).map(|m| (q, m));
        }
        p.season.as_ref().map(|s| (s.1.clone(), rects))
    })
}

/// Lets the new tab's plates go. The spirit stays.
pub fn release_sheet() {
    PLATES.with(|p| {
        let mut p = p.borrow_mut();
        p.stamps = None;
        p.season = None;
    });
}
