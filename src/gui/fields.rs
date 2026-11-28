//! Using a form on a page: typing in its boxes, ticking, picking from a list, and sending
//! it. What a control holds lives in the page's document; this is the keyboard, the mouse
//! and the drawing of the one control in use.
//!
//! Typing never lays the page out again. The box being typed in is drawn from its own
//! small layout, and when you leave it that layout is written over the one in the page.

use super::*;
use crate::edit::{Edit, Step};
use crate::web::form::{self, Control};
use crate::web::layout::{PLACEHOLDER, PRESS};
use crate::web::Field;

/// The control in use.
pub(super) struct Focus {
    tab: u64,
    node: u32,
    edit: Edit,
    typed: bool,
    /// how far the text is scrolled inside its box
    scroll: (f32, f32),
    /// the mouse is down and selecting
    drag: bool,
}

/// The open list of a select.
pub(super) struct Menu {
    node: u32,
    choices: Vec<form::Choice>,
    at: usize,
    top: usize,
    /// window css px
    r: Rect,
}

const ROW: f32 = 26.0;
const ROWS: usize = 10;

/// How a key is held with others.
#[derive(Clone, Copy, Default)]
pub(super) struct Mods {
    pub ctrl: bool,
    pub shift: bool,
    pub alt: bool,
}

/// The box in use, laid out for one frame.
pub(super) struct Live {
    pub field: u32,
    pub list: DisplayList,
    pub caret: Option<Rect>,
    pub marked: Vec<Rect>,
    pub clip: Rect,
}

/// A position in characters as one in utf-16 units, which text layout counts in.
fn utf16(s: &str, chars: usize) -> usize {
    s.chars().take(chars).map(char::len_utf16).sum()
}

fn chars(s: &str, utf16: usize) -> usize {
    let mut seen = 0;
    s.chars()
        .take_while(|c| {
            let before = seen < utf16;
            seen += c.len_utf16();
            before
        })
        .count()
}

/// Applies an editing key. Returns whether it was one.
pub(super) fn edit_key(e: &mut Edit, vk: VIRTUAL_KEY, m: Mods) -> bool {
    let side = |one, word| if m.ctrl { word } else { one };
    match vk {
        VK_LEFT => e.step(side(Step::Left, Step::WordLeft), m.shift),
        VK_RIGHT => e.step(side(Step::Right, Step::WordRight), m.shift),
        VK_HOME => e.step(side(Step::LineStart, Step::Start), m.shift),
        VK_END => e.step(side(Step::LineEnd, Step::End), m.shift),
        VK_BACK => e.delete(side(Step::Left, Step::WordLeft)),
        VK_DELETE => e.delete(side(Step::Right, Step::WordRight)),
        VK_A if m.ctrl => e.select_all(),
        VK_Z if m.ctrl => e.undo(),
        _ => return false,
    }
    true
}

impl App {
    /// A point in the window, as one on the page.
    fn on_page(&self, x: f32, y: f32) -> (f32, f32) {
        self.to_page(x, y)
    }

    /// The control under the mouse that can be used, with its box.
    pub(super) fn control_at(&self, x: f32, y: f32) -> Option<(u32, Control, Rect)> {
        if !self.in_page(x, y) {
            return None;
        }
        let (px, py) = self.on_page(x, y);
        let t = self.tab();
        let dl = t.dl.as_ref()?;
        let dom = &t.page.as_ref()?.doc.dom;
        let (f, r) = dl.field_at(px, py)?;
        let node = dl.fields[f as usize].node;
        form::enabled(dom, node).then(|| Some((node, form::control(dom, node)?, r)))?
    }

    fn focused_here(&self) -> Option<&Focus> {
        self.focus.as_ref().filter(|f| f.tab == self.tab().id)
    }

    /// Whether keys go to a box on the page.
    pub(super) fn typing(&self) -> bool {
        self.focused_here().is_some_and(|f| f.typed)
    }

    /// The box in use: its number in the page's list, what it looks like and where it is.
    fn box_of(&self, node: u32) -> Option<(u32, Field, Rect)> {
        let dl = self.tab().dl.as_ref()?;
        let i = dl.fields.iter().position(|f| f.node == node)? as u32;
        let r = dl.items.iter().find_map(|it| match it {
            Item::Field { r, field, .. } if *field == i => Some(*r),
            _ => None,
        })?;
        Some((i, dl.fields[i as usize].clone(), r))
    }

    fn focus_node(&mut self, node: u32) {
        self.blur();
        self.addr_focused = false;
        let tab = self.tab().id;
        let Some(page) = self.tab().page.as_ref() else { return };
        let dom = &page.doc.dom;
        let Some(c) = form::control(dom, node) else { return };
        let mut edit = Edit::default();
        if c.typed() {
            edit = Edit::new(&form::value(dom, node));
            edit.multiline = c == Control::Area;
            edit.limit = dom.attr(node, crate::web::dom::atom("maxlength")).and_then(|m| m.trim().parse().ok());
        }
        self.focus = Some(Focus { tab, node, edit, typed: c.typed(), scroll: (0.0, 0.0), drag: false });
        unsafe {
            let _ = SetFocus(Some(self.hwnd));
        }
    }

    /// Focuses the box a page asked to have ready, unless something else is in use.
    pub(super) fn autofocus(&mut self) {
        if self.addr_focused || self.focus.is_some() {
            return;
        }
        let Some(page) = self.tab().page.as_ref() else { return };
        let dom = &page.doc.dom;
        let wants = crate::web::dom::atom("autofocus");
        let node = (0..dom.nodes.len() as u32).find(|&n| dom.attr(n, wants).is_some() && form::control(dom, n).is_some_and(Control::typed));
        if let Some(n) = node.filter(|&n| self.box_of(n).is_some()) {
            self.focus_node(n);
        }
    }

    /// Leaves the box in use, and writes what it holds into the page's picture.
    pub(super) fn blur(&mut self) {
        self.menu = None;
        let Some(f) = self.focus.take() else { return };
        if !f.typed || f.tab != self.tab().id {
            return;
        }
        let Some((i, field, r)) = self.box_of(f.node) else { return };
        let hint = self.hint(f.node);
        let (para, color) = if f.edit.is_empty() { (field.para(&hint, PLACEHOLDER), PLACEHOLDER) } else { (field.para(&field.shown(&f.edit.text()), field.color), field.color) };
        let h = self.dw.layout(&para, r.w.max(1.0), 0.0, 0.0, None).height;
        let mut own = DisplayList::new();
        self.dw.layout(&para, r.w.max(1.0), r.x, field.top(r, h), Some(&mut own));
        let Some(dl) = self.tab_mut().dl.as_mut() else { return };
        let Some(at) = dl.items.iter().position(|it| matches!(it, Item::Field { field, .. } if *field == i)) else { return };
        let from = dl.glyphs.len() as u32;
        dl.glyphs.extend(own.glyphs.iter().copied());
        if let Some(Item::Text { start, len, color: c, .. }) = dl.items.get_mut(at + 1) {
            (*start, *len, *c) = (from, own.glyphs.len() as u32, color);
        }
    }

    fn hint(&self, node: u32) -> String {
        self.tab().page.as_ref().and_then(|p| p.doc.dom.attr(node, crate::web::dom::t::placeholder)).unwrap_or("").to_string()
    }

    /// Lays out the box in use for this frame, scrolled so the caret is inside it.
    pub(super) fn live(&mut self) -> Option<Live> {
        let (node, typed) = self.focused_here().map(|f| (f.node, f.typed))?;
        let (field_no, field, r) = self.box_of(node)?;
        if !typed {
            return Some(Live { field: u32::MAX, list: DisplayList::new(), caret: None, marked: Vec::new(), clip: r });
        }
        let hint = self.hint(node);
        let f = self.focus.as_mut()?;
        let shown = field.shown(&f.edit.text());
        let para = field.para(&shown, field.color);
        // an empty box still has a line for the caret to stand on
        let measure = if shown.is_empty() { field.para("x", field.color) } else { para.clone() };
        let w = r.w.max(1.0);
        let h = self.dw.layout(&measure, w, 0.0, 0.0, None).height;
        let top = field.top(r, h);
        let at = utf16(&shown, f.edit.caret());
        let c = self.dw.caret(&measure, w, if shown.is_empty() { 0 } else { at });
        if field.multiline {
            f.scroll.1 = f.scroll.1.min(c.y).max(c.y + c.h - r.h).max(0.0);
        } else if field.align == 1 {
            f.scroll.0 = f.scroll.0.min(c.x).max(c.x + 2.0 - r.w).max(0.0);
        }
        let (dx, dy) = (r.x - f.scroll.0, top - f.scroll.1);
        let marked = match f.edit.selection() {
            Some((a, b)) => self.dw.marked(&para, w, utf16(&shown, a), utf16(&shown, b)).into_iter().map(|m| Rect::new(m.x + dx, m.y + dy, m.w, m.h)).collect(),
            None => Vec::new(),
        };
        let mut list = DisplayList::new();
        if shown.is_empty() {
            self.dw.layout(&field.para(&hint, PLACEHOLDER), w, r.x, top, Some(&mut list));
        } else {
            self.dw.layout(&para, w, dx, dy, Some(&mut list));
        }
        Some(Live { field: field_no, list, caret: Some(Rect::new(c.x + dx, c.y + dy, 1.5, c.h)), marked, clip: Rect::new(r.x - 1.0, r.y - 1.0, r.w + 2.0, r.h + 2.0) })
    }

    /// Which character of the box in use is under a point on the page.
    fn char_at(&mut self, px: f32, py: f32) -> Option<usize> {
        let node = self.focused_here()?.node;
        let (_, field, r) = self.box_of(node)?;
        let f = self.focus.as_ref()?;
        let shown = field.shown(&f.edit.text());
        let scroll = f.scroll;
        let para = field.para(&shown, field.color);
        let w = r.w.max(1.0);
        let h = self.dw.layout(&para, w, 0.0, 0.0, None).height;
        let i = self.dw.index_at(&para, w, px - r.x + scroll.0, py - field.top(r, h) + scroll.1);
        Some(chars(&shown, i))
    }

    /// Whether the box in use hides what is typed.
    pub(super) fn password(&self) -> bool {
        self.focused_here().is_some_and(|f| self.control_of(f.node) == Some(Control::Password))
    }

    // ---- the address bar ----

    /// The address being typed, as one line of text.
    fn addr_para(&self) -> Para {
        let font = Font { family: Arc::from(UI_FONT), size: 17.0, weight: 400, italic: false };
        Field { node: 0, font, color: [0; 4], align: 1, line_h: 0.0, multiline: false, password: false }.para(&self.edit.text(), [0; 4])
    }

    /// The part of the address that fits in `width` with the caret in it, where the caret
    /// is, and what is selected.
    pub(super) fn addr_view(&mut self, width: f32) -> (String, f32, Option<(f32, f32)>) {
        let (s, p) = (self.edit.text(), self.addr_para());
        let start = self.addr_start(width);
        let x0 = self.dw.caret(&p, 1.0e5, start).x;
        let mut x = |c: usize| (self.dw.caret(&p, 1.0e5, utf16(&s, c)).x - x0).clamp(0.0, width);
        let caret = x(self.edit.caret());
        let marked = self.edit.selection().map(|(a, b)| (x(a), x(b)));
        (s.chars().skip(chars(&s, start)).collect(), caret, marked)
    }

    /// The utf-16 position the address bar starts drawing at, so the caret fits.
    fn addr_start(&mut self, width: f32) -> usize {
        let p = self.addr_para();
        let at = utf16(&self.edit.text(), self.edit.caret());
        let over = self.dw.caret(&p, 1.0e5, at).x - (width - 4.0);
        // a whole address that was just selected is read from its start
        if over <= 0.0 || self.edit.selection().is_some_and(|(a, _)| a == 0) {
            return 0;
        }
        (self.dw.index_at(&p, 1.0e5, over, 2.0) + 1).min(at)
    }

    /// The character of the address under the mouse.
    pub(super) fn addr_char(&mut self, x: f32) -> usize {
        let Some(r) = self.hits.iter().find(|h| h.1 == Hit::Addr).map(|h| h.0) else { return 0 };
        let (inset, spare) = (46.0, 62.0);
        let start = self.addr_start(r.w - spare);
        let p = self.addr_para();
        let x0 = self.dw.caret(&p, 1.0e5, start).x;
        let i = self.dw.index_at(&p, 1.0e5, x / self.scale - r.x - inset + x0, 2.0);
        chars(&self.edit.text(), i)
    }

    // ---- mouse ----

    /// A click on the page. Returns whether a control or the open list used it.
    pub(super) fn click_field(&mut self, x: f32, y: f32, m: Mods, double: bool) -> bool {
        if self.menu.is_some() {
            self.click_menu(x, y);
            return true;
        }
        let Some((node, c, _)) = self.control_at(x, y) else {
            self.blur();
            return false;
        };
        if !c.typed() {
            self.activate(node);
            return true;
        }
        if self.focused_here().is_none_or(|f| f.node != node) {
            self.focus_node(node);
        }
        self.addr_focused = false;
        let (px, py) = self.on_page(x, y);
        let at = self.char_at(px, py);
        if let (Some(at), Some(f)) = (at, self.focus.as_mut()) {
            if double {
                f.edit.select_word(at);
            } else {
                f.edit.place(at, m.shift);
            }
            f.drag = !double;
            unsafe {
                SetCapture(self.hwnd);
            }
        }
        true
    }

    /// Selects while the mouse is held down. Returns whether it is.
    pub(super) fn drag_field(&mut self, x: f32, y: f32) -> bool {
        if !self.focused_here().is_some_and(|f| f.drag) {
            return false;
        }
        let (px, py) = self.on_page(x, y);
        let at = self.char_at(px, py);
        if let (Some(at), Some(f)) = (at, self.focus.as_mut()) {
            f.edit.place(at, true);
        }
        true
    }

    pub(super) fn drop_field(&mut self) {
        if let Some(f) = self.focus.as_mut()
            && std::mem::take(&mut f.drag)
        {
            unsafe {
                let _ = ReleaseCapture();
            }
        }
    }

    // ---- keyboard ----

    /// A key while a control is in use. Returns whether it was used.
    pub(super) fn field_key(&mut self, vk: VIRTUAL_KEY, m: Mods) -> bool {
        if self.menu.is_some() {
            return self.menu_key(vk);
        }
        let Some((node, typed, multiline)) = self.focused_here().map(|f| (f.node, f.typed, f.edit.multiline)) else { return false };
        if vk == VK_TAB && !m.ctrl {
            self.next_field(node, m.shift);
            return true;
        }
        if vk == VK_ESCAPE {
            self.blur();
            return true;
        }
        if !typed {
            match vk {
                VK_SPACE | VK_RETURN => self.activate(node),
                VK_DOWN | VK_UP if self.control_of(node) == Some(Control::Select) => self.activate(node),
                _ => return false,
            }
            return true;
        }
        match vk {
            VK_RETURN if multiline && !m.ctrl => self.type_text("\n"),
            VK_RETURN => self.enter(node),
            VK_UP | VK_DOWN if multiline => self.move_line(if vk == VK_UP { -1.0 } else { 1.0 }, m.shift),
            VK_UP | VK_DOWN => {}
            _ => {
                let Some(f) = self.focus.as_mut() else { return false };
                if !edit_key(&mut f.edit, vk, m) {
                    return false;
                }
                self.keep_value();
            }
        }
        true
    }

    /// One line up or down, to the character nearest the caret.
    fn move_line(&mut self, way: f32, select: bool) {
        let Some(c) = self.live().and_then(|l| l.caret) else { return };
        let at = self.char_at(c.x, c.y + c.h / 2.0 + way * c.h);
        if let (Some(at), Some(f)) = (at, self.focus.as_mut()) {
            f.edit.place(at, select);
        }
    }

    /// Types into the box in use.
    pub(super) fn type_text(&mut self, s: &str) {
        let Some(f) = self.focus.as_mut() else { return };
        if f.typed {
            f.edit.insert(s);
            self.keep_value();
        }
    }

    pub(super) fn focused_edit(&mut self) -> Option<&mut Edit> {
        let tab = self.tab().id;
        self.focus.as_mut().filter(|f| f.typed && f.tab == tab).map(|f| &mut f.edit)
    }

    /// Writes the box in use into the document. A box that only shows its value is left.
    pub(super) fn keep_value(&mut self) {
        let Some((node, text)) = self.focused_here().map(|f| (f.node, f.edit.text())) else { return };
        let Some(page) = self.tab_mut().page.as_mut() else { return };
        let dom = &mut page.doc.dom;
        if dom.attr(node, crate::web::dom::atom("readonly")).is_none() {
            return form::set_value(dom, node, &text);
        }
        let old = form::value(dom, node);
        if old != text
            && let Some(f) = self.focus.as_mut()
        {
            let at = f.edit.caret();
            f.edit = Edit::new(&old);
            f.edit.place(at, false);
        }
    }

    fn control_of(&self, node: u32) -> Option<Control> {
        form::control(&self.tab().page.as_ref()?.doc.dom, node)
    }

    /// Tab: the next control on the page, in the order they are drawn.
    fn next_field(&mut self, node: u32, back: bool) {
        let t = self.tab();
        let (Some(dl), Some(page)) = (&t.dl, &t.page) else { return };
        let nodes: Vec<u32> = dl
            .items
            .iter()
            .filter_map(|it| if let Item::Field { field, .. } = it { Some(dl.fields[*field as usize].node) } else { None })
            .filter(|&n| form::enabled(&page.doc.dom, n))
            .collect();
        let Some(i) = nodes.iter().position(|&n| n == node) else { return };
        let n = nodes[(i + if back { nodes.len() - 1 } else { 1 }) % nodes.len()];
        self.focus_node(n);
        if let Some(f) = self.focus.as_mut() {
            f.edit.select_all();
        }
        self.show_box(n);
    }

    /// Scrolls the page if a control is off screen.
    fn show_box(&mut self, node: u32) {
        let Some((_, _, r)) = self.box_of(node) else { return };
        let (top, h) = (self.tab().scroll, self.view_h());
        if r.y < top + 20.0 || r.y + r.h > top + h - 20.0 {
            self.scroll_to(r.y - h / 3.0);
        }
    }

    // ---- doing things ----

    /// Uses a control the way a click on it does.
    fn activate(&mut self, node: u32) {
        let Some(page) = self.tab_mut().page.as_mut() else { return };
        let dom = &mut page.doc.dom;
        let Some(c) = form::control(dom, node).filter(|_| form::enabled(dom, node)) else { return };
        match c {
            Control::Check | Control::Radio => {
                form::toggle(dom, node);
                self.focus_node(node);
                self.restyle();
            }
            Control::Select => {
                self.focus_node(node);
                self.open_menu(node);
            }
            Control::Submit => {
                if let Some(f) = form::owner(dom, node) {
                    self.send(f, Some(node));
                }
            }
            Control::Reset => {
                if let Some(f) = form::owner(dom, node) {
                    form::reset(dom, f);
                    self.blur();
                    self.restyle();
                }
            }
            Control::Button => self.say("that button needs the full engine: ctrl+shift+e"),
            Control::File => self.say("sending files needs the full engine: ctrl+shift+e"),
            Control::Text | Control::Password | Control::Area => self.focus_node(node),
            Control::Hidden => {}
        }
    }

    /// Answers a `press:` address: a button, a label, or the top of a <details>.
    pub(super) fn press(&mut self, address: &str) {
        use crate::web::dom::t;
        let Some(node) = address.strip_prefix(PRESS).and_then(|n| n.parse::<u32>().ok()) else { return };
        let Some(page) = self.tab_mut().page.as_mut() else { return };
        let dom = &mut page.doc.dom;
        match dom.tag(node) {
            t::label => {
                if let Some(c) = form::labelled(dom, node) {
                    self.activate(c);
                }
            }
            t::summary => {
                let d = dom.parent(node);
                if dom.attr(d, t::open).is_some() {
                    dom.remove_attr(d, t::open);
                } else {
                    dom.set_attr(d, t::open, "");
                }
                self.restyle();
            }
            _ => self.activate(node),
        }
    }

    /// Enter in a one-line box sends its form the way its first button would.
    fn enter(&mut self, node: u32) {
        let Some(page) = self.tab().page.as_ref() else { return };
        let dom = &page.doc.dom;
        let Some(f) = form::owner(dom, node) else { return };
        match form::default_button(dom, f) {
            Some(b) if !form::enabled(dom, b) => {}
            b => self.send(f, b),
        }
    }

    fn send(&mut self, f: u32, button: Option<u32>) {
        let t = self.tab();
        let Some(page) = t.page.as_ref() else { return };
        let doc = &page.doc;
        if let Some((node, why)) = form::problem(&doc.dom, f, button) {
            self.say(why);
            self.focus_node(node);
            return self.show_box(node);
        }
        let Some(out) = form::submit(&doc.dom, f, button, &doc.url, &doc.base) else { return };
        let from = doc.url.host.clone();
        let c = self.cur;
        self.focus = None;
        self.open(c, load::Request { url: out.url, body: out.body, from }, true);
    }

    /// A short note at the bottom of the window.
    pub(super) fn say(&mut self, s: &str) {
        self.toast = Some((s.to_string(), Instant::now()));
        unsafe {
            SetTimer(Some(self.hwnd), 4, 500, None);
        }
    }

    /// Cascades and lays out the page again after its document changed.
    pub(super) fn restyle(&mut self) {
        let (vw, vh) = (self.view_w(), self.view_h());
        let c = self.cur;
        let t = &mut self.tabs[c];
        let Some(page) = t.page.as_mut() else { return };
        page.styles = Cascade { dom: &page.doc.dom, sheets: &page.doc.sheets, vw, vh }.run();
        let dl = layout::layout(&page.doc.dom, &page.styles, &mut self.dw, &mut page.images, &page.doc.base, vw, vh);
        t.dl = Some(dl);
        t.vw = vw;
        let max = self.max_scroll();
        let t = self.tab_mut();
        t.scroll = t.scroll.min(max);
        t.target = t.target.min(max);
    }

    // ---- the list of a select ----

    fn open_menu(&mut self, node: u32) {
        let Some(page) = self.tab().page.as_ref() else { return };
        let choices = form::options(&page.doc.dom, node);
        let Some((_, _, r)) = self.box_of(node) else { return };
        if choices.is_empty() {
            return;
        }
        let at = choices.iter().position(|c| c.selected).unwrap_or(0);
        let widest = choices.iter().map(|c| c.label.clone()).collect::<Vec<_>>().iter().map(|l| self.ui_text(l, 13.0, 400, UI_FONT, [0; 4]).1).fold(0.0, f32::max);
        // the list is drawn at the size of the interface, under the box or above it
        let z = self.zoom;
        let b = self.page_box();
        let (x, y) = (b.x + r.x * z, b.y + (r.y - self.tab().scroll) * z);
        let (w, h) = (self.w as f32 / self.scale, self.h as f32 / self.scale);
        let mh = choices.len().min(ROWS) as f32 * ROW + 8.0;
        let mw = (widest + 34.0).max(r.w * z + 8.0).min(w - 16.0);
        let below = y + r.h * z + 5.0;
        let my = if below + mh < h { below } else { (y - 5.0 - mh).max(b.y) };
        let top = at.saturating_sub(ROWS / 2).min(choices.len().saturating_sub(ROWS));
        self.menu = Some(Menu { node, choices, at, top, r: Rect::new((x - 4.0).clamp(4.0, (w - mw - 4.0).max(4.0)), my, mw, mh) });
    }

    /// The row of the open list under a point.
    fn menu_row(&self, x: f32, y: f32) -> Option<usize> {
        let m = self.menu.as_ref()?;
        let (x, y) = (x / self.scale, y / self.scale);
        let i = m.top + ((y - m.r.y - 4.0) / ROW).max(0.0) as usize;
        (m.r.contains(x, y) && i < m.choices.len()).then_some(i)
    }

    fn click_menu(&mut self, x: f32, y: f32) {
        match self.menu_row(x, y) {
            Some(i) => {
                if let Some(m) = self.menu.as_mut() {
                    m.at = i;
                }
                self.pick();
            }
            None => self.menu = None,
        }
    }

    pub(super) fn hover_menu(&mut self, x: f32, y: f32) -> bool {
        let row = self.menu_row(x, y);
        let Some(m) = self.menu.as_mut() else { return false };
        match row.filter(|i| *i != m.at) {
            Some(i) => {
                m.at = i;
                true
            }
            None => false,
        }
    }

    pub(super) fn scroll_menu(&mut self, rows: i32) -> bool {
        let Some(m) = self.menu.as_mut() else { return false };
        let last = m.choices.len().saturating_sub(ROWS) as i32;
        m.top = (m.top as i32 + rows).clamp(0, last) as usize;
        true
    }

    fn menu_key(&mut self, vk: VIRTUAL_KEY) -> bool {
        let Some(m) = self.menu.as_mut() else { return false };
        let last = m.choices.len() - 1;
        m.at = match vk {
            VK_DOWN => (m.at + 1).min(last),
            VK_UP => m.at.saturating_sub(1),
            VK_NEXT => (m.at + ROWS).min(last),
            VK_PRIOR => m.at.saturating_sub(ROWS),
            VK_HOME => 0,
            VK_END => last,
            VK_RETURN | VK_SPACE => {
                self.pick();
                return true;
            }
            VK_ESCAPE | VK_TAB => {
                self.menu = None;
                return true;
            }
            _ => return false,
        };
        m.top = m.top.min(m.at).max((m.at + 1).saturating_sub(ROWS));
        true
    }

    /// Typing a letter goes to the next option that starts with it.
    pub(super) fn menu_char(&mut self, ch: char) -> bool {
        let Some(m) = self.menu.as_mut() else { return false };
        let n = m.choices.len();
        let starts = |i: usize| m.choices[i].label.chars().next().is_some_and(|c| c.to_lowercase().eq(ch.to_lowercase()));
        if let Some(i) = (1..=n).map(|d| (m.at + d) % n).find(|&i| starts(i)) {
            m.at = i;
            m.top = m.top.min(i).max((i + 1).saturating_sub(ROWS));
        }
        true
    }

    fn pick(&mut self) {
        let Some(m) = self.menu.take() else { return };
        let c = &m.choices[m.at];
        if c.disabled {
            return;
        }
        if let Some(page) = self.tab_mut().page.as_mut() {
            form::choose(&mut page.doc.dom, m.node, c.node);
            self.restyle();
        }
    }

    pub(super) fn draw_menu(&mut self) {
        let Some(m) = self.menu.take() else { return };
        let s = sk();
        self.rect(Rect::new(m.r.x - 1.0, m.r.y + 2.0, m.r.w + 2.0, m.r.h + 2.0), [0, 0, 0, 40], 6.0);
        self.rect(m.r, s.field, 5.0);
        let k = self.scale;
        self.gpu.ring(m.r.x * k, m.r.y * k, m.r.w * k, m.r.h * k, 1.0, s.faint, 5.0 * k, gpu::NO_CLIP);
        for (i, c) in m.choices.iter().enumerate().skip(m.top).take(ROWS) {
            let y = m.r.y + 4.0 + (i - m.top) as f32 * ROW;
            if i == m.at {
                self.rect(Rect::new(m.r.x + 4.0, y, m.r.w - 8.0, ROW), s.hover, 3.0);
            }
            if c.selected {
                self.rect(Rect::new(m.r.x + 10.0, y + ROW / 2.0 - 2.5, 5.0, 5.0), s.ink, 2.5);
            }
            self.label(&c.label, m.r.x + 22.0, y + 4.0, 13.0, 400, if c.disabled { s.faint } else { s.text }, m.r.w - 30.0, UI_FONT);
        }
        if m.choices.len() > ROWS {
            let track = m.r.h - 8.0;
            let th = track * ROWS as f32 / m.choices.len() as f32;
            let y = m.r.y + 4.0 + track * m.top as f32 / m.choices.len() as f32;
            self.rect(Rect::new(m.r.x + m.r.w - 6.0, y, 3.0, th), s.faint, 1.5);
        }
        self.menu = Some(m);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn characters_and_utf16_agree() {
        let s = "a😀b";
        assert_eq!((utf16(s, 0), utf16(s, 2), utf16(s, 3)), (0, 3, 4));
        assert_eq!((chars(s, 0), chars(s, 1), chars(s, 3), chars(s, 4), chars(s, 9)), (0, 1, 2, 3, 3));
    }
}
