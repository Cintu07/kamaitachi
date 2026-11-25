//! One piece of text being typed into: the address bar, or a box on a page. It knows
//! about a caret, a selection and undo, and nothing about windows or fonts.

/// How far a move or a delete reaches.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Step {
    Left,
    Right,
    WordLeft,
    WordRight,
    /// the start or end of the line the caret is on
    LineStart,
    LineEnd,
    Start,
    End,
}

const UNDOS: usize = 64;

#[derive(Default, Clone)]
pub struct Edit {
    text: Vec<char>,
    caret: usize,
    /// the other end of the selection; equal to the caret when nothing is selected
    anchor: usize,
    /// the most characters a box takes, when it says
    pub limit: Option<usize>,
    /// Enter makes a new line
    pub multiline: bool,
    undo: Vec<(Vec<char>, usize)>,
    /// the last change was typing, so the next letter belongs to the same undo
    typing: bool,
}

impl Edit {
    pub fn new(s: &str) -> Edit {
        let text: Vec<char> = s.chars().collect();
        Edit { caret: text.len(), anchor: text.len(), text, ..Default::default() }
    }

    pub fn text(&self) -> String {
        self.text.iter().collect()
    }

    pub fn is_empty(&self) -> bool {
        self.text.is_empty()
    }

    pub fn caret(&self) -> usize {
        self.caret
    }

    /// The text up to the caret.
    pub fn before(&self) -> String {
        self.text[..self.caret].iter().collect()
    }

    /// The selected range, as character positions.
    pub fn selection(&self) -> Option<(usize, usize)> {
        (self.caret != self.anchor).then(|| (self.caret.min(self.anchor), self.caret.max(self.anchor)))
    }

    pub fn selected(&self) -> String {
        self.selection().map_or(String::new(), |(a, b)| self.text[a..b].iter().collect())
    }

    pub fn select_all(&mut self) {
        self.anchor = 0;
        self.caret = self.text.len();
        self.typing = false;
    }

    /// Selects the word at a position, as a double click does.
    pub fn select_word(&mut self, at: usize) {
        let at = at.min(self.text.len());
        let word = |c: char| c.is_alphanumeric() || c == '_';
        let mut a = at;
        while a > 0 && word(self.text[a - 1]) {
            a -= 1;
        }
        let mut b = at;
        while b < self.text.len() && word(self.text[b]) {
            b += 1;
        }
        self.anchor = a;
        self.caret = b;
        self.typing = false;
    }

    /// Puts the caret somewhere, dragging the selection with it when `select` is set.
    pub fn place(&mut self, at: usize, select: bool) {
        self.caret = at.min(self.text.len());
        if !select {
            self.anchor = self.caret;
        }
        self.typing = false;
    }

    pub fn step(&mut self, s: Step, select: bool) {
        // an arrow with something selected goes to that end of it
        if !select && let Some((a, b)) = self.selection() {
            match s {
                Step::Left => return self.place(a, false),
                Step::Right => return self.place(b, false),
                _ => {}
            }
        }
        self.place(self.target(s), select);
    }

    fn target(&self, s: Step) -> usize {
        let t = &self.text;
        let mut i = self.caret;
        match s {
            Step::Left => i.saturating_sub(1),
            Step::Right => (i + 1).min(t.len()),
            Step::WordLeft => {
                while i > 0 && !t[i - 1].is_alphanumeric() {
                    i -= 1;
                }
                while i > 0 && t[i - 1].is_alphanumeric() {
                    i -= 1;
                }
                i
            }
            Step::WordRight => {
                while i < t.len() && t[i].is_alphanumeric() {
                    i += 1;
                }
                while i < t.len() && !t[i].is_alphanumeric() {
                    i += 1;
                }
                i
            }
            Step::LineStart => t[..i].iter().rposition(|c| *c == '\n').map_or(0, |p| p + 1),
            Step::LineEnd => t[i..].iter().position(|c| *c == '\n').map_or(t.len(), |p| i + p),
            Step::Start => 0,
            Step::End => t.len(),
        }
    }

    fn remember(&mut self) {
        if self.undo.len() == UNDOS {
            self.undo.remove(0);
        }
        self.undo.push((self.text.clone(), self.caret));
    }

    fn remove_selection(&mut self) {
        if let Some((a, b)) = self.selection() {
            self.text.drain(a..b);
            self.caret = a;
            self.anchor = a;
        }
    }

    /// Types or pastes `s` over the selection.
    pub fn insert(&mut self, s: &str) {
        let one_letter = s.chars().count() == 1 && !s.starts_with(char::is_whitespace);
        if !(self.typing && one_letter && self.selection().is_none()) {
            self.remember();
        }
        self.remove_selection();
        for c in s.replace("\r\n", "\n").replace('\r', "\n").chars() {
            // a line break is a space in a box that has only one line
            let c = if c == '\n' && !self.multiline { ' ' } else { c };
            if c.is_control() && c != '\n' && c != '\t' {
                continue;
            }
            if self.limit.is_some_and(|n| self.text.len() >= n) {
                break;
            }
            self.text.insert(self.caret, c);
            self.caret += 1;
        }
        self.anchor = self.caret;
        self.typing = one_letter;
    }

    /// Deletes the selection, or what a step from the caret covers.
    pub fn delete(&mut self, s: Step) {
        if self.selection().is_none() {
            let to = self.target(s);
            if to == self.caret {
                return;
            }
            self.remember();
            self.anchor = to;
        } else {
            self.remember();
        }
        self.remove_selection();
        self.typing = false;
    }

    pub fn undo(&mut self) {
        if let Some((t, c)) = self.undo.pop() {
            self.text = t;
            self.caret = c.min(self.text.len());
            self.anchor = self.caret;
        }
        self.typing = false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn typing_replaces_what_is_selected() {
        let mut e = Edit::new("hello world");
        e.select_word(8);
        assert_eq!(e.selected(), "world");
        e.insert("weasel");
        assert_eq!(e.text(), "hello weasel");
        e.select_all();
        e.insert("x");
        assert_eq!((e.text().as_str(), e.caret()), ("x", 1));
    }

    #[test]
    fn steps_by_letter_word_and_line() {
        let mut e = Edit::new("one two\nthree");
        e.step(Step::WordLeft, false);
        assert_eq!(e.caret(), 8);
        e.step(Step::Left, false);
        e.step(Step::LineStart, true);
        assert_eq!(e.selected(), "one two");
        // an arrow goes to the end of a selection instead of one past it
        e.step(Step::Right, false);
        assert_eq!((e.caret(), e.selection()), (7, None));
        e.delete(Step::WordLeft);
        assert_eq!(e.text(), "one \nthree");
        e.step(Step::End, false);
        e.delete(Step::Right);
        e.delete(Step::Left);
        assert_eq!(e.text(), "one \nthre");
    }

    #[test]
    fn undo_takes_back_a_word_at_a_time() {
        let mut e = Edit::new("");
        for c in ["w", "e", "a", " ", "s", "e"] {
            e.insert(c);
        }
        e.undo();
        assert_eq!(e.text(), "wea ");
        e.undo();
        assert_eq!(e.text(), "wea");
        e.undo();
        e.undo();
        assert_eq!(e.text(), "");
    }

    #[test]
    fn pasted_lines_and_limits() {
        let mut e = Edit::new("");
        e.insert("a\r\nb\n\nc\u{7}");
        assert_eq!(e.text(), "a b  c");
        let mut e = Edit { multiline: true, limit: Some(4), ..Default::default() };
        e.insert("a\r\nb\n\nc");
        assert_eq!(e.text(), "a\nb\n");
    }
}
