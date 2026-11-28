//! Right-click menus, pinned tabs and peek.
//!
//! A tab's menu pins it, duplicates it, mutes it, lets it sleep, keeps a bookmark,
//! copies its address and closes it or the others. A link's menu opens it in a new tab
//! or in a peek: a card over the page you are reading, which Esc or a click beside it
//! puts away, and "Keep as tab" turns into a tab of its own. Shift+click peeks too.
//!
//! Pinned tabs are kept at the front of the row of tabs and drawn as squares at the top
//! of the column.

use super::*;

#[derive(Clone, Copy, PartialEq, Debug)]
pub(super) enum Act {
    Pin,
    Unpin,
    Duplicate,
    Mute,
    Sleep,
    Bookmark,
    CopyAddress,
    Close,
    CloseOthers,
    NewTab,
    Peek,
    CopyLink,
}

#[derive(Clone, PartialEq, Debug)]
pub(super) enum Target {
    Tab(u64),
    Link(String),
}

pub(super) struct Menu {
    target: Target,
    items: Vec<(&'static str, Act)>,
    /// window css px
    r: Rect,
    hover: Option<usize>,
}

const ROW: f32 = 30.0;

impl App {
    fn index_of(&self, id: u64) -> Option<usize> {
        self.tabs.iter().position(|t| t.id == id)
    }

    /// The tabs the column shows: not a peek.
    pub(super) fn shown(&self, i: usize) -> bool {
        !self.tabs[i].peek
    }

    // ---- menus ----

    /// Opens the menu for whatever is under the mouse. Returns whether there was one.
    pub(super) fn context(&mut self, x: f32, y: f32) -> bool {
        self.mouse_move(x, y);
        let target = match self.hover {
            Hit::Tab(i) | Hit::Close(i) | Hit::Mute(i) => Target::Tab(self.tabs[i].id),
            _ => match self.hover_link.clone().filter(|l| !l.starts_with(layout::PRESS)) {
                Some(l) => Target::Link(l),
                None => return false,
            },
        };
        let items = match &target {
            Target::Tab(id) => {
                let Some(i) = self.index_of(*id) else { return false };
                let t = &self.tabs[i];
                let mut v = vec![(if t.pinned { "Unpin" } else { "Pin" }, if t.pinned { Act::Unpin } else { Act::Pin }), ("Duplicate", Act::Duplicate)];
                if t.full.is_some() {
                    v.push((if t.muted { "Unmute" } else { "Mute" }, Act::Mute));
                }
                if i != self.cur && !t.full_mode && t.page.is_some() {
                    v.push(("Let it sleep", Act::Sleep));
                }
                v.extend([("Bookmark", Act::Bookmark), ("Copy address", Act::CopyAddress), ("Close", Act::Close), ("Close other tabs", Act::CloseOthers)]);
                v
            }
            Target::Link(_) => vec![("Open in new tab", Act::NewTab), ("Peek", Act::Peek), ("Copy link address", Act::CopyLink)],
        };
        let (w, h) = (self.w as f32 / self.scale, self.h as f32 / self.scale);
        let mh = items.len() as f32 * ROW + 10.0;
        let (mx, my) = (x / self.scale, y / self.scale);
        let r = Rect::new(mx.min(w - 214.0).max(4.0), if my + mh < h { my } else { (my - mh).max(4.0) }, 210.0, mh);
        self.tab_menu = Some(Menu { target, items, r, hover: None });
        self.motion.menu = Some(Instant::now());
        self.render();
        true
    }

    /// A click while a menu is open: on an item does it, anywhere else closes the menu.
    pub(super) fn menu_click(&mut self, x: f32, y: f32) -> bool {
        let Some(m) = self.tab_menu.take() else { return false };
        let (x, y) = (x / self.scale, y / self.scale);
        if m.r.contains(x, y) {
            let k = ((y - m.r.y - 5.0) / ROW).floor().max(0.0) as usize;
            if let Some(&(_, act)) = m.items.get(k) {
                self.act(&m.target, act);
            }
        }
        self.render();
        true
    }

    pub(super) fn menu_hover(&mut self, x: f32, y: f32) -> bool {
        let Some(m) = self.tab_menu.as_mut() else { return false };
        let (x, y) = (x / self.scale, y / self.scale);
        let k = m.r.contains(x, y).then(|| ((y - m.r.y - 5.0) / ROW).floor().max(0.0) as usize).filter(|k| *k < m.items.len());
        if k != m.hover {
            m.hover = k;
            self.render();
        }
        true
    }

    pub(super) fn menu_escape(&mut self) -> bool {
        self.tab_menu.take().is_some()
    }

    fn act(&mut self, target: &Target, act: Act) {
        match target {
            Target::Link(url) => match act {
                Act::NewTab => self.open_background(url),
                Act::Peek => self.peek(url),
                Act::CopyLink => self.copy(url),
                _ => {}
            },
            Target::Tab(id) => {
                let Some(i) = self.index_of(*id) else { return };
                match act {
                    Act::Pin | Act::Unpin => self.set_pinned(i, act == Act::Pin),
                    Act::Duplicate => {
                        let (url, scroll) = (self.tabs[i].url.clone(), self.tabs[i].scroll);
                        let keep = self.cur;
                        self.cur = i;
                        self.open_background(&url);
                        self.cur = keep;
                        if let Some(t) = self.tabs.get_mut(i + 1) {
                            t.restore = Some(scroll);
                        }
                    }
                    Act::Mute => {
                        let keep = self.cur;
                        self.cur = i;
                        self.toggle_mute();
                        self.cur = keep;
                    }
                    Act::Sleep => {
                        self.sleep_tab(i);
                        self.say("the tab let its page go");
                    }
                    Act::Bookmark => {
                        let on = places::toggle_mark(&self.tabs[i].url, &self.tabs[i].title);
                        self.say(if on { "bookmarked" } else { "bookmark taken away" });
                    }
                    Act::CopyAddress => {
                        let u = self.tabs[i].url.clone();
                        self.copy(&u);
                    }
                    Act::Close => self.close_tab(i),
                    Act::CloseOthers => {
                        let ids: Vec<u64> = self.tabs.iter().filter(|t| t.id != *id && !t.pinned && !t.peek).map(|t| t.id).collect();
                        for other in ids {
                            if let Some(j) = self.index_of(other) {
                                self.close_tab(j);
                            }
                        }
                    }
                    _ => {}
                }
            }
        }
        self.session_changed();
    }

    pub(super) fn draw_tab_menu(&mut self) {
        let Some(mut m) = self.tab_menu.take() else { return };
        let (s, k) = (sk(), self.scale);
        let t = motion::part(self.motion.menu, motion::PANEL_MS);
        let real = m.r;
        m.r.y -= (1.0 - t) * 6.0;
        self.rect(Rect::new(m.r.x - 1.0, m.r.y + 3.0, m.r.w + 2.0, m.r.h), [0, 0, 0, if s.light { 26 } else { 70 }], 10.0);
        self.rect(m.r, s.field, 9.0);
        self.gpu.ring(m.r.x * k, m.r.y * k, m.r.w * k, m.r.h * k, 1.0, s.hairline, 9.0 * k, gpu::NO_CLIP);
        for (n, (label, _)) in m.items.iter().enumerate() {
            let y = m.r.y + 5.0 + n as f32 * ROW;
            if m.hover == Some(n) {
                self.rect(Rect::new(m.r.x + 5.0, y, m.r.w - 10.0, ROW), s.wash, 6.0);
            }
            self.label(label, m.r.x + 16.0, y + 6.5, 13.5, 400, motion::fade(s.text, t), m.r.w - 30.0, UI_FONT);
        }
        m.r = real;
        self.tab_menu = Some(m);
    }

    // ---- pins ----

    /// Pins a tab, which moves it to the end of the pinned ones, or unpins it, which
    /// moves it just after them.
    pub(super) fn set_pinned(&mut self, i: usize, on: bool) {
        let cur_id = self.tabs[self.cur].id;
        let mut t = self.tabs.remove(i);
        t.pinned = on;
        let at = self.tabs.iter().take_while(|t| t.pinned).count();
        self.tabs.insert(at, t);
        self.cur = self.index_of(cur_id).unwrap_or(0);
        self.session_changed();
    }

    // ---- sleep ----

    /// Lets a tab's page go now, as the timer would after a while.
    pub(super) fn sleep_tab(&mut self, i: usize) {
        if i == self.cur {
            return;
        }
        if self.tabs[i].full.is_some() {
            self.park(i);
        } else {
            self.sleep_lite(i);
        }
    }

    // ---- peek ----

    /// Opens `url` in a card over the page you are on.
    pub(super) fn peek(&mut self, url: &str) {
        self.close_peek();
        let under = self.tabs[self.cur].id;
        let id = self.next_id;
        self.next_id += 1;
        let mut t = Tab::new(id);
        t.peek = true;
        self.tabs.push(t);
        self.peek_under = Some(under);
        self.motion.peek = Some(Instant::now());
        self.cur = self.tabs.len() - 1;
        let from = host_of(&self.tabs[self.index_of(under).unwrap_or(0)].url);
        self.load_from(self.cur, url.to_string(), false, from);
        self.update_views();
    }

    pub(super) fn peeking(&self) -> bool {
        self.tabs.get(self.cur).is_some_and(|t| t.peek)
    }

    /// Puts the peek away and goes back to the page under it.
    pub(super) fn close_peek(&mut self) -> bool {
        let Some(under) = self.peek_under.take() else { return false };
        if let Some(i) = self.tabs.iter().position(|t| t.peek) {
            if let Some(f) = self.tabs[i].full.take() {
                unsafe {
                    let _ = f.ctl.Close();
                }
            }
            self.tabs.remove(i);
        }
        self.cur = self.index_of(under).unwrap_or(0);
        self.update_title();
        self.update_views();
        true
    }

    /// Makes the peek a tab of its own, right after the page it was opened from.
    pub(super) fn keep_peek(&mut self) {
        let Some(under) = self.peek_under.take() else { return };
        let Some(i) = self.tabs.iter().position(|t| t.peek) else { return };
        let mut t = self.tabs.remove(i);
        t.peek = false;
        let at = self.index_of(under).map_or(self.tabs.len(), |u| u + 1);
        self.tabs.insert(at, t);
        self.cur = at;
        self.update_title();
        self.update_views();
        self.session_changed();
    }

    /// The page under the peek, dimmed, with the peek's own card and buttons over it.
    pub(super) fn draw_peek_under(&mut self) {
        let Some(under) = self.peek_under.and_then(|id| self.index_of(id)) else { return };
        let peek = self.cur;
        self.cur = under;
        let bg = self.tab().dl.as_ref().map_or(sk().field, |d| d.bg);
        self.draw_card(bg);
        self.draw_page();
        self.draw_card_edges();
        let b = self.page_box();
        let r = if prefs::get().card { 10.0 } else { 0.0 };
        let t = motion::part(self.motion.peek, motion::PANEL_MS * 1.4);
        self.rect(b, [0, 0, 0, ((if sk().light { 110.0 } else { 150.0 }) * t) as u8], r);
        self.cur = peek;
    }

    pub(super) fn draw_peek_bar(&mut self) {
        if !self.peeking() {
            return;
        }
        let (b, s, k) = (self.page_box(), sk(), self.scale);
        let (title, url) = (self.tab().title.clone(), self.tab().url.clone());
        // a strip of its own above the card: what is in the peek, and what to do with it
        let bar = Rect::new(b.x, b.y - 42.0, b.w, 34.0);
        self.rect(bar, s.field, 9.0);
        self.gpu.ring(bar.x * k, bar.y * k, bar.w * k, bar.h * k, 1.0, s.hairline, 9.0 * k, gpu::NO_CLIP);
        let mut x = bar.x + bar.w - 4.0;
        let mut used = 0.0;
        for (text, hit) in [("Close", Hit::PeekClose), ("Keep as tab", Hit::PeekKeep)] {
            let tw = self.ui_text(text, 13.0, 500, UI_FONT, s.text).1 + 22.0;
            x -= tw;
            let r = Rect::new(x, bar.y + 4.0, tw, 26.0);
            self.rect(r, if self.hover == hit { s.wash } else { s.hover }, 7.0);
            self.label(text, r.x + 11.0, r.y + 5.0, 13.0, 500, s.text, tw, UI_FONT);
            self.hits.push((r, hit));
            x -= 6.0;
            used += tw + 6.0;
        }
        let host = url.trim_start_matches("https://").split('/').next().unwrap_or("").trim_start_matches("www.").to_string();
        let label = if title.is_empty() { host.clone() } else { title };
        let room = bar.w - used - 28.0;
        let lw = self.label(&label, bar.x + 14.0, bar.y + 8.0, 13.5, 500, s.text, room, UI_FONT);
        if lw + 20.0 < room {
            self.label(&host, bar.x + 24.0 + lw, bar.y + 9.0, 12.5, 400, s.muted, room - lw - 10.0, UI_FONT);
        }
    }
}
