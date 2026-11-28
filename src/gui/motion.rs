//! Small movements that make the window feel alive: a new tab grows into the column, a
//! closed one folds away, the address field, menus and peeks fade up into place, and
//! notes slide in. Each is a start time and a length; while one is running the window
//! draws every frame, and when none is it goes back to drawing only when asked.

use super::*;

/// How long each kind of motion takes, in milliseconds.
pub(super) const OPEN_MS: f32 = 180.0;
pub(super) const CLOSE_MS: f32 = 160.0;
pub(super) const PANEL_MS: f32 = 150.0;
pub(super) const NOTE_MS: f32 = 180.0;

/// A closed tab still folding away in the column.
pub(super) struct Ghost {
    pub title: String,
    pub letter: String,
    /// the tab it sat after, so it folds in the right place; 0 for the top
    pub after: u64,
    pub at: Instant,
}

#[derive(Default)]
pub(super) struct Motion {
    /// tabs still growing in, by id
    pub opened: Vec<(u64, Instant)>,
    pub ghosts: Vec<Ghost>,
    /// when the address field, a menu or a peek came up
    pub panel: Option<Instant>,
    pub menu: Option<Instant>,
    pub peek: Option<Instant>,
}

/// Quick out, slow in: most of the way at once, then easing into place.
pub(super) fn ease(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    1.0 - (1.0 - t).powi(3)
}

/// How far through a motion that started at `at` is, 0 to 1, eased.
pub(super) fn part(at: Option<Instant>, ms: f32) -> f32 {
    at.map_or(1.0, |a| ease(a.elapsed().as_secs_f32() * 1000.0 / ms))
}

fn running(at: Instant, ms: f32) -> bool {
    at.elapsed().as_secs_f32() * 1000.0 < ms
}

impl Motion {
    /// Whether anything is still moving, so the window should keep drawing.
    pub fn busy(&self, toast: Option<Instant>) -> bool {
        self.opened.iter().any(|(_, a)| running(*a, OPEN_MS))
            || self.ghosts.iter().any(|g| running(g.at, CLOSE_MS))
            || [(self.panel, PANEL_MS), (self.menu, PANEL_MS), (self.peek, PANEL_MS * 1.4), (toast, NOTE_MS)].iter().any(|(a, ms)| a.is_some_and(|a| running(a, *ms)))
    }

    /// Lets go of motions that have finished.
    pub fn tidy(&mut self) {
        self.opened.retain(|(_, a)| running(*a, OPEN_MS));
        self.ghosts.retain(|g| running(g.at, CLOSE_MS));
    }

    /// How far a tab has grown in, 0 to 1.
    pub fn grown(&self, id: u64) -> f32 {
        self.opened.iter().find(|(t, _)| *t == id).map_or(1.0, |(_, a)| ease(a.elapsed().as_secs_f32() * 1000.0 / OPEN_MS))
    }
}

/// A colour with its alpha scaled, for fading something in.
pub(super) fn fade(c: Rgba, k: f32) -> Rgba {
    [c[0], c[1], c[2], (c[3] as f32 * k.clamp(0.0, 1.0)) as u8]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn easing_starts_fast_and_lands_softly() {
        assert_eq!(ease(0.0), 0.0);
        assert_eq!(ease(1.0), 1.0);
        assert!(ease(0.5) > 0.8);
        assert_eq!(ease(2.0), 1.0);
    }
}
