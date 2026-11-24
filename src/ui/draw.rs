//! Painting. A sidebar of tabs on the left, a header, the view in a centered column, and a
//! hint bar along the bottom. One accent color; everything else is a few greys. Nothing
//! here allocates: text is formatted into stack buffers and painted straight into cells.

use core::fmt::Write as _;

use super::*;
use crate::arena::{self, ARENA_NAMES, BUDGETS};
use crate::html::{K_HR, K_LI, K_PRE, K_QUOTE, K_ROW};
use crate::render::{color::*, measure, Style, BOLD, ITALIC, UNDER};
use crate::text::str_width;

const SIDEBAR: u16 = 26;
const SPIN: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

#[derive(Clone, Copy)]
struct Layout {
    sb: u16,
    x0: u16,
    x1: u16,
    view_top: u16,
    /// exclusive: the hint bar's row
    bottom: u16,
}

fn layout(w: u16, h: u16) -> Layout {
    let sb = if w >= 100 { SIDEBAR } else { 0 };
    Layout { sb, x0: sb, x1: w, view_top: 2, bottom: h - 1 }
}

/// A centered reading column inside the area: (x, width).
fn column(l: &Layout, max: u16) -> (u16, u16) {
    let aw = l.x1 - l.x0;
    let w = max.min(aw.saturating_sub(8)).max(16.min(aw));
    (l.x0 + (aw - w) / 2, w)
}

fn put(s: &mut Screen, x: u16, y: u16, max_x: u16, text: &str, st: Style) -> u16 {
    s.back.text(&mut s.pool, x, y, max_x, text, st)
}

fn put_center(s: &mut Screen, x0: u16, x1: u16, y: u16, text: &str, st: Style) -> u16 {
    let w = str_width(text) as u16;
    let x = x0 + (x1 - x0).saturating_sub(w) / 2;
    put(s, x, y, x1, text, st)
}

fn put_right(s: &mut Screen, x1: u16, y: u16, text: &str, st: Style) -> u16 {
    let w = str_width(text) as u16;
    put(s, x1.saturating_sub(w), y, x1, text, st)
}

fn service_color(s: Service) -> u8 {
    match s {
        Service::X => X_DOT,
        Service::YouTube => YT,
        Service::Claude => CLAUDE,
        Service::ChatGpt => GPT,
        Service::Local => HISTORY,
        Service::Web => WEB,
    }
}

/// A box with rounded corners filled with the surface color. Returns the inner origin.
fn panel(s: &mut Screen, x: u16, y: u16, w: u16, h: u16, title: &str, border: u8) -> (u16, u16) {
    let bg = SURFACE;
    s.back.fill_rect(x, y, x + w, y + h, Style::new(BODY, bg, 0));
    let b = Style::new(border, bg, 0);
    s.back.fill(y, x + 1, x + w - 1, '─', b);
    s.back.fill(y + h - 1, x + 1, x + w - 1, '─', b);
    for yy in y + 1..y + h - 1 {
        s.back.fill(yy, x, x + 1, '│', b);
        s.back.fill(yy, x + w - 1, x + w, '│', b);
    }
    s.back.fill(y, x, x + 1, '╭', b);
    s.back.fill(y, x + w - 1, x + w, '╮', b);
    s.back.fill(y + h - 1, x, x + 1, '╰', b);
    s.back.fill(y + h - 1, x + w - 1, x + w, '╯', b);
    if !title.is_empty() {
        let tx = put(s, x + 2, y, x + w - 2, " ", b);
        let tx = put(s, tx, y, x + w - 2, title, Style::new(FG, bg, BOLD));
        put(s, tx, y, x + w - 2, " ", b);
    }
    (x + 2, y + 1)
}

/// The tab's name for the sidebar and the header.
fn tab_title<const N: usize>(t: &Tab, out: &mut Fmt<N>) {
    let _ = match &t.src {
        Source::XHome => write!(out, "Home"),
        Source::XMentions => write!(out, "Mentions"),
        Source::XThread { .. } => write!(out, "Thread"),
        Source::XSearch { q } => write!(out, "“{q}”"),
        Source::YtSubs => write!(out, "Subscriptions"),
        Source::YtComments { .. } => write!(out, "Comments"),
        Source::YtSearch { q } => write!(out, "“{q}”"),
        Source::Chat { service: Service::Claude, .. } => write!(out, "Claude"),
        Source::Chat { .. } => write!(out, "ChatGPT"),
        Source::LocalSearch { q } => write!(out, "History “{q}”"),
        Source::Start => write!(out, "New tab"),
        Source::Web { url } => {
            let title = match &t.store {
                Store::Page(p) => t.arena.str(p.title),
                _ => "",
            };
            if title.is_empty() {
                let host = url.trim_start_matches("https://").split('/').next().unwrap_or("");
                write!(out, "{host}")
            } else {
                write!(out, "{title}")
            }
        }
    };
}

/// Rows a feed entry takes, its gap included.
fn entry_height(arena: &TextArena, f: &Feed, i: usize, cw: u16) -> u16 {
    let indent = 3 * f.depth[i].min(6) as u16;
    let tw = cw.saturating_sub(indent).max(10);
    let video = f.flags[i] & F_VIDEO != 0;
    let mut h = measure(arena.str(f.text[i]), tw) + 1;
    if !video && (f.metric[i] > 0 || f.flags[i] & F_HAS_MEDIA != 0) {
        h += 1;
    }
    if f.flags[i] & F_ROOT != 0 && !f.extra[i].is_empty() {
        h += 1 + measure(arena.str(f.extra[i]), tw).min(12);
    }
    if f.flags[i] & F_SHOW_MEDIA != 0 {
        h += 1;
    }
    h + 1
}

/// A table row that starts with a rank ("12."), as on link aggregators.
fn ranked(text: &str) -> bool {
    let t = text.trim_start().as_bytes();
    let d = t.iter().take_while(|c| c.is_ascii_digit()).count();
    d > 0 && d < 5 && t.get(d) == Some(&b'.')
}

/// 1: a ranked row (a story's title), 2: a row continuing one (its points, snippet or
/// address), 0: anything else.
fn row_role(arena: &TextArena, p: &Page, i: usize) -> u8 {
    let b = &p.blocks[i];
    if b.kind != K_ROW {
        return 0;
    }
    if ranked(arena.str(b.text)) {
        return 1;
    }
    for j in (i.saturating_sub(4)..i).rev() {
        let pb = &p.blocks[j];
        if pb.kind != K_ROW {
            return 0;
        }
        if ranked(arena.str(pb.text)) {
            return 2;
        }
    }
    0
}

/// Rows a page block takes, its margins included.
fn block_height(arena: &TextArena, p: &Page, i: usize, cw: u16) -> u16 {
    let b = &p.blocks[i];
    let (indent, _) = block_indent(b);
    let rows = if b.kind == K_HR { 1 } else { measure(arena.str(b.text), cw.saturating_sub(indent).max(10)) };
    let top = (matches!(b.kind, 1 | 2) && i > 0) as u16;
    let next = p.blocks.get(i + 1).map(|n| n.kind);
    // rows of one ranked item stay together; a gap comes before the next rank
    let ends_item = row_role(arena, p, i) != 0 && p.blocks.get(i + 1).is_some_and(|_| row_role(arena, p, i + 1) == 1);
    let tight = matches!(b.kind, K_LI | K_ROW) && next == Some(b.kind) && !ends_item;
    top + rows + if tight { 0 } else { 1 }
}

fn block_indent(b: &PageBlock) -> (u16, u16) {
    match b.kind {
        K_LI => (2 * b.depth.max(1) as u16, 0),
        K_QUOTE => (2, 0),
        K_PRE => (1, 1),
        _ => (0, 0),
    }
}

/// The scroll fields of a tab, borrowed apart from its rows.
struct Scroll<'a> {
    cursor: &'a mut usize,
    top: &'a mut usize,
    top_row: &'a mut u16,
    pending: &'a mut i32,
    follow: bool,
}

/// Applies wheel/page scrolling and, when following, scrolls just enough rows to show
/// item `cursor`. With `clamp`, a cursor scrolled out of sight moves into view.
fn scroll_view(v: Scroll, n: usize, view_h: u16, clamp: bool, ht: &dyn Fn(usize) -> u16) {
    if n == 0 {
        return;
    }
    let Scroll { cursor, top, top_row, pending, follow } = v;
    *cursor = (*cursor).min(n - 1);
    if *top >= n {
        *top = n - 1;
        *top_row = 0;
    }
    let mut d = core::mem::take(pending);
    while d > 0 {
        if *top_row + 1 < ht(*top) {
            *top_row += 1;
        } else if *top + 1 < n {
            *top += 1;
            *top_row = 0;
        } else {
            break;
        }
        d -= 1;
    }
    while d < 0 {
        if *top_row > 0 {
            *top_row -= 1;
        } else if *top > 0 {
            *top -= 1;
            *top_row = ht(*top).saturating_sub(1);
        } else {
            break;
        }
        d += 1;
    }
    if follow {
        if *cursor < *top || (*cursor == *top && *top_row > 0) {
            *top = *cursor;
            *top_row = 0;
        } else {
            let mut rows = (ht(*top) - *top_row) as u32;
            for i in *top + 1..=*cursor {
                rows += ht(i) as u32;
            }
            while rows > view_h as u32 && *top < *cursor {
                if *top_row + 1 < ht(*top) {
                    *top_row += 1;
                } else {
                    *top += 1;
                    *top_row = 0;
                }
                rows -= 1;
            }
        }
    } else if clamp {
        let first = if *top_row > 0 && *top + 1 < n { *top + 1 } else { *top };
        let mut last = *top;
        let mut rows = (ht(*top) - *top_row) as u32;
        while last + 1 < n && rows + ht(last + 1) as u32 <= view_h as u32 {
            last += 1;
            rows += ht(last) as u32;
        }
        *cursor = (*cursor).clamp(first.min(last), last.max(first));
    }
}

impl App {
    pub fn render(&mut self) {
        self.age_tabs();
        self.frame.reset();
        self.screen.begin();
        self.hits.n = 0;
        for r in self.row_entry.iter_mut() {
            *r = u32::MAX;
        }
        let (w, h) = (self.screen.back.w, self.screen.back.h);
        self.screen.back.fill_rect(0, 0, w, h, Style::new(BODY, BG, 0));
        if w < 40 || h < 8 {
            put(&mut self.screen, 0, 0, w, "make the window bigger", Style::fg(MUTED));
            return;
        }
        let l = layout(w, h);
        if l.sb > 0 {
            self.draw_sidebar(&l);
        }
        self.draw_header(&l);
        if !self.tabs.is_empty() {
            if matches!(self.mode, Mode::Compose { .. }) {
                self.draw_compose(&l);
            } else {
                match self.tabs[self.active].store {
                    Store::Feed(_) => self.draw_feed(&l),
                    Store::Chat(_) => self.draw_chat(&l),
                    Store::Page(_) => self.draw_page(&l),
                    Store::Start(_) => self.draw_start(&l),
                }
            }
        }
        self.draw_hints(&l);
        match self.mode {
            Mode::Palette(kind) => self.draw_palette(kind),
            Mode::Overlay(o) => self.draw_overlay(o),
            _ => {}
        }
        self.frames += 1;
    }

    fn draw_sidebar(&mut self, l: &Layout) {
        let App { screen: s, tabs, active, hits, spend, cfg, .. } = self;
        let h = l.bottom;
        let sb = l.sb;
        s.back.fill_rect(0, 0, sb, h, Style::new(BODY, SURFACE, 0));
        s.back.fill(0, sb - 1, sb, ' ', Style::new(BORDER, BG, 0));
        for y in 0..h {
            s.back.fill(y, sb - 1, sb, '│', Style::new(BORDER, BG, 0));
        }
        let x = put(s, 2, 0, sb, "疾風", Style::new(ACCENT, SURFACE, BOLD));
        put(s, x + 1, 0, sb, "hayate", Style::new(FG, SURFACE, BOLD));
        put(s, 2, 2, sb, "Tabs", Style::new(FAINT, SURFACE, 0));
        let mut y = 3u16;
        let last_row = h.saturating_sub(5);
        for (i, t) in tabs.iter().enumerate() {
            if y >= last_row {
                break;
            }
            let on = i == *active;
            let bg = if on { SEL_BG } else { SURFACE };
            if on {
                s.back.fill(y, 0, sb - 1, ' ', Style::new(BODY, bg, 0));
                put(s, 0, y, 1, "▎", Style::new(ACCENT, bg, 0));
            }
            let dot_color = service_color(t.service());
            let dot = if t.loading { SPIN[t.spin as usize % 10] } else if t.src == Source::Start { "○" } else { "●" };
            put(s, 2, y, sb, dot, Style::new(if t.src == Source::Start { FAINT } else { dot_color }, bg, 0));
            let mut name = Fmt::<96>::new();
            tab_title(t, &mut name);
            let fg = if on {
                FG
            } else if t.state == TabState::Archived {
                FAINT
            } else {
                BAR_FG
            };
            put(s, 4, y, sb - 4, name.as_str(), Style::new(fg, bg, if on { BOLD } else { 0 }));
            if i < 9 {
                let mut k = Fmt::<4>::new();
                let _ = write!(k, "{}", i + 1);
                put(s, sb - 3, y, sb - 1, k.as_str(), Style::new(FAINT, bg, 0));
            }
            hits.add(Hit { y, x0: 0, x1: sb - 1, kind: H_TAB, arg: i as u32, span: Span::EMPTY });
            y += 1;
        }
        y += 1;
        if y < last_row {
            let x = put(s, 2, y, sb, "+", Style::new(MUTED, SURFACE, 0));
            put(s, x + 1, y, sb, "New tab", Style::new(MUTED, SURFACE, 0));
            put_right(s, sb - 2, y, "ctrl+t", Style::new(FAINT, SURFACE, 0));
            hits.add(Hit { y, x0: 0, x1: sb - 1, kind: H_NEWTAB, arg: 0, span: Span::EMPTY });
        }
        // usage at the foot
        let m = sys::process_memory();
        let mut v = Fmt::<32>::new();
        let _ = write!(v, "{:.1} MB", m.working_set as f64 / 1048576.0);
        let fy = h - 3;
        put(s, 2, fy, sb, "Memory", Style::new(FAINT, SURFACE, 0));
        put_right(s, sb - 2, fy, v.as_str(), Style::new(MUTED, SURFACE, 0));
        let total = (spend.0 + spend.1 + spend.2) as f64 / 1e6;
        let cap: f64 = cfg.cap_usd.iter().sum();
        let mut v = Fmt::<32>::new();
        let _ = write!(v, "${total:.2} of ${cap:.0}");
        put(s, 2, fy + 1, sb, "Spend", Style::new(FAINT, SURFACE, 0));
        put_right(s, sb - 2, fy + 1, v.as_str(), Style::new(MUTED, SURFACE, 0));
    }

    fn draw_header(&mut self, l: &Layout) {
        let App { screen: s, tabs, active, hits, .. } = self;
        let y = 0;
        if l.sb == 0 {
            // narrow: the tabs become a strip
            let mut x = 1u16;
            for (i, t) in tabs.iter().enumerate() {
                let mut name = Fmt::<64>::new();
                tab_title(t, &mut name);
                let on = i == *active;
                let x0 = x;
                x = put(s, x, y, l.x1, if on { "● " } else { "  " }, Style::fg(service_color(t.service())));
                x = put(s, x, y, (x + 18).min(l.x1), name.as_str(), Style::fg(if on { FG } else { MUTED }).with(if on { BOLD } else { 0 }));
                hits.add(Hit { y, x0, x1: x, kind: H_TAB, arg: i as u32, span: Span::EMPTY });
                x += 2;
                if x >= l.x1 {
                    break;
                }
            }
        } else if let Some(t) = tabs.get(*active) {
            let mut x = l.x0 + 3;
            let color = service_color(t.service());
            if t.src != Source::Start {
                x = put(s, x, y, l.x1, "●", Style::fg(color));
                x += 1;
            }
            let mut name = Fmt::<160>::new();
            tab_title(t, &mut name);
            let right_room = l.x1.saturating_sub(24);
            x = put(s, x, y, right_room, name.as_str(), Style::fg(FG).with(BOLD));
            let mut sub = Fmt::<200>::new();
            match &t.store {
                Store::Feed(f) => {
                    let what = match t.service() {
                        Service::YouTube => "videos",
                        Service::Local => "matches",
                        _ => "posts",
                    };
                    let _ = write!(sub, "{} · {} {what}", t.service().name(), f.len());
                }
                Store::Chat(_) => {
                    let _ = write!(sub, "{}", t.model);
                }
                Store::Page(p) => {
                    let u = if p.url.is_empty() {
                        match &t.src {
                            Source::Web { url } => url.trim_start_matches("https://"),
                            _ => "",
                        }
                    } else {
                        t.arena.str(p.url).trim_start_matches("https://")
                    };
                    let _ = write!(sub, "{u}");
                }
                Store::Start(_) => {}
            }
            if !sub.as_str().is_empty() {
                put(s, x + 2, y, right_room, sub.as_str(), Style::fg(MUTED));
            }
            let mut right = Fmt::<64>::new();
            if !t.back.is_empty() || !t.fwd.is_empty() {
                let _ = write!(right, "{}  {}", if t.back.is_empty() { " " } else { "←" }, if t.fwd.is_empty() { " " } else { "→" });
            }
            put_right(s, l.x1 - 2, y, right.as_str(), Style::fg(FAINT));
        }
        s.back.fill(1, l.x0 + 1, l.x1 - 1, '─', Style::fg(BORDER));
    }

    fn draw_hints(&mut self, l: &Layout) {
        let App { screen: s, tabs, active, mode, status, status_level, status_tab, .. } = self;
        let w = l.x1;
        let y = l.bottom;
        let bar = Style::new(BAR_FG, BAR_BG, 0);
        s.back.fill(y, 0, w, ' ', bar);
        let t = tabs.get(*active);
        let hints: &[(&str, &str)] = match mode {
            Mode::Palette(_) => &[("↑↓", "select"), ("enter", "run"), ("esc", "close")],
            Mode::Compose { .. } => &[("ctrl+s", "post"), ("enter", "new line"), ("esc", "cancel")],
            Mode::Overlay(_) => &[("esc", "close")],
            Mode::Normal => match t.map(|t| &t.store) {
                Some(Store::Feed(_)) => &[("j k", "move"), ("enter", "open"), ("/", "search"), ("ctrl+k", "commands"), ("t", "new tab"), ("?", "keys")],
                Some(Store::Page(_)) => &[("j k", "scroll"), ("tab", "links"), ("enter", "follow"), ("bksp", "back"), ("ctrl+l", "address"), ("ctrl+k", "commands")],
                Some(Store::Chat(c)) if c.focused => &[("enter", "send"), ("shift+enter", "new line"), ("esc", "leave input"), ("ctrl+k", "commands")],
                Some(Store::Chat(_)) => &[("i", "type"), ("j k", "scroll"), ("n", "new chat"), (":model", "switch"), ("ctrl+k", "commands")],
                Some(Store::Start(_)) => &[("type", "search or address"), ("tab", "shortcuts"), ("enter", "go"), ("ctrl+k", "commands")],
                None => &[("ctrl+t", "new tab")],
            },
        };
        // the message on the right decides how much room the hints get
        let mut right = Fmt::<160>::new();
        let mut rst = Style::new(MUTED, BAR_BG, 0);
        let mine = status_tab.is_none() || t.is_some_and(|t| Some(t.id) == *status_tab);
        if !status.as_str().is_empty() && mine {
            let (c, dot) = match status_level {
                Level::Info => (MUTED, ""),
                Level::Warn => (WARN, "● "),
                Level::Error => (ERR, "● "),
            };
            let _ = write!(right, "{dot}{}", status.as_str());
            rst = Style::new(c, BAR_BG, 0);
        } else if let Some(t) = t {
            if t.loading {
                let _ = write!(right, "{} loading", SPIN[t.spin as usize % 10]);
                rst = Style::new(ACCENT, BAR_BG, 0);
            } else if let Store::Page(p) = &t.store {
                if let Some(l) = p.focus.and_then(|i| p.links.get(i)) {
                    let _ = write!(right, "{}", t.arena.str(l.href).trim_start_matches("https://"));
                    rst = Style::new(LINK, BAR_BG, 0);
                }
            }
        }
        let rw = (str_width(right.as_str()) as u16).min(w / 2);
        let limit = w.saturating_sub(rw + 3);
        let mut x = 2u16;
        for (k, label) in hints {
            let need = (str_width(k) + str_width(label) + 3) as u16;
            if x + need > limit {
                break;
            }
            x = put(s, x, y, limit, k, Style::new(BAR_FG, BAR_BG, BOLD));
            x = put(s, x + 1, y, limit, label, Style::new(FAINT, BAR_BG, 0));
            x += 3;
        }
        if rw > 0 {
            put(s, w - rw - 2, y, w - 1, right.as_str(), rst);
        }
    }

    fn empty_state(&mut self, l: &Layout, title: &str, body: &str, hint: &str) {
        let s = &mut self.screen;
        let mid = l.view_top + (l.bottom - l.view_top) / 2;
        let y = mid.saturating_sub(2);
        put_center(s, l.x0, l.x1, y, title, Style::fg(FG).with(BOLD));
        put_center(s, l.x0, l.x1, y + 1, body, Style::fg(MUTED));
        put_center(s, l.x0, l.x1, y + 3, hint, Style::fg(FAINT));
    }

    fn draw_feed(&mut self, l: &Layout) {
        let (cx, cw) = column(l, 84);
        let view_h = l.bottom - l.view_top;
        {
            let t = &self.tabs[self.active];
            let Store::Feed(f) = &t.store else { return };
            if f.is_empty() {
                let mut what = Fmt::<64>::new();
                tab_title(t, &mut what);
                if t.loading {
                    let mut msg = Fmt::<96>::new();
                    let _ = write!(msg, "{} Loading {}", SPIN[t.spin as usize % 10], what.as_str());
                    let s = &mut self.screen;
                    let mid = l.view_top + view_h / 2;
                    put_center(s, l.x0, l.x1, mid, msg.as_str(), Style::fg(MUTED));
                    return;
                }
                if let Some(e) = t.error {
                    let (title, hint) = match t.service() {
                        Service::X => ("Connect X", "run  hayate auth x  in a terminal, then press R"),
                        Service::YouTube => ("Connect YouTube", "run  hayate auth youtube  or set youtube.channels, then press R"),
                        _ => ("Could not load this", "R tries again"),
                    };
                    let body = e;
                    return self.empty_state(l, title, body.as_str(), hint);
                }
                return self.empty_state(l, "Nothing here yet", "new posts show up here as they arrive", "R refreshes");
            }
        }
        self.feed_rows(l, cx, cw, view_h);
    }
}

impl App {
    fn feed_rows(&mut self, l: &Layout, cx: u16, cw: u16, view_h: u16) {
        let App { screen: s, tabs, active, hits, row_entry, authors, authors_arena, .. } = self;
        let t = &mut tabs[*active];
        let Store::Feed(f) = &t.store else { return };
        let arena: &TextArena = &t.arena;
        let n = f.len();
        scroll_view(
            Scroll { cursor: &mut t.cursor, top: &mut t.top, top_row: &mut t.top_row, pending: &mut t.pending, follow: t.follow },
            n,
            view_h,
            true,
            &|i| entry_height(arena, f, i, cw),
        );
        let now = sys::now_unix();
        let service = t.src.service();
        let (cursor, top, top_row) = (t.cursor, t.top, t.top_row);
        let bottom = l.bottom as i32;
        let vt = l.view_top as i32;
        let mut y = vt - top_row as i32;
        let mut i = top;
        while i < n && y < bottom {
            let eh = entry_height(arena, f, i, cw) as i32;
            let sel = i == cursor;
            let bg = if sel { SEL_BG } else { BG };
            let depth = f.depth[i].min(6) as u16;
            let indent = 3 * depth;
            let x = cx + indent;
            let tw = cw.saturating_sub(indent).max(10);
            let card_end = y + eh - 1; // the last row is the gap
            if sel {
                for yy in y.max(vt)..card_end.min(bottom) {
                    s.back.fill(yy as u16, cx.saturating_sub(2), cx + cw + 2, ' ', Style::new(BODY, bg, 0));
                    s.back.fill(yy as u16, cx.saturating_sub(2), cx.saturating_sub(1), '▎', Style::new(ACCENT, bg, 0));
                }
            }
            if depth > 0 {
                for yy in y.max(vt)..card_end.min(bottom) {
                    s.back.fill(yy as u16, x - 2, x - 1, '│', Style::new(BORDER, bg, 0));
                }
            }
            for yy in y.max(vt)..(y + eh).min(bottom) {
                if let Some(r) = row_entry.get_mut(yy as usize) {
                    *r = i as u32;
                }
            }
            let (name, handle) = match authors.get(f.author[i] as usize) {
                Some(&(nm, h)) => (authors_arena.str(nm), authors_arena.str(h)),
                None => ("", ""),
            };
            let name = if name.is_empty() { handle } else { name };
            let flags = f.flags[i];
            let mut yy = y;
            if flags & F_VIDEO != 0 {
                // a video: the title leads, the channel and numbers follow
                yy += paint_text(s, hits, arena, f.text[i], x, yy, tw, vt, bottom, Style::new(FG, bg, BOLD)) as i32;
                if yy >= vt && yy < bottom {
                    let mut meta = Fmt::<160>::new();
                    let _ = write!(meta, "{name} · ");
                    rel_time(&mut meta, now, f.ts[i]);
                    if f.metric[i] > 0 {
                        let _ = write!(meta, " · ");
                        compact_num(&mut meta, f.metric[i]);
                        let _ = write!(meta, " views");
                    }
                    let mx = put(s, x, yy as u16, x + tw, meta.as_str(), Style::new(MUTED, bg, 0));
                    if f.position[i] > 0 {
                        let p = f.position[i];
                        let mut w = Fmt::<32>::new();
                        let _ = write!(w, "  ▶ {}:{:02}", p / 60, p % 60);
                        put(s, mx, yy as u16, x + tw, w.as_str(), Style::new(ACCENT, bg, 0));
                    }
                }
                yy += 1;
            } else {
                if yy >= vt && yy < bottom {
                    let nm = if name.is_empty() { "unknown" } else { name };
                    let mut hx = put(s, x, yy as u16, x + tw, nm, Style::new(AUTHOR, bg, BOLD));
                    let mut meta = Fmt::<96>::new();
                    if !handle.is_empty() && service == Service::X {
                        let _ = write!(meta, " @{handle}");
                    }
                    let _ = write!(meta, " · ");
                    rel_time(&mut meta, now, f.ts[i]);
                    hx = put(s, hx, yy as u16, x + tw, meta.as_str(), Style::new(MUTED, bg, 0));
                    if flags & F_LOCAL_HIT != 0 && !handle.is_empty() && service == Service::Local {
                        let _ = put(s, hx + 2, yy as u16, x + tw, handle, Style::new(HISTORY, bg, 0));
                    }
                }
                yy += 1;
                let body = if flags & F_ROOT != 0 { Style::new(FG, bg, 0) } else { Style::new(BODY, bg, 0) };
                yy += paint_text(s, hits, arena, f.text[i], x, yy, tw, vt, bottom, body) as i32;
                if f.metric[i] > 0 || flags & F_HAS_MEDIA != 0 {
                    if yy >= vt && yy < bottom {
                        let mut m = Fmt::<64>::new();
                        if f.metric[i] > 0 {
                            let _ = write!(m, "{} ", "♥");
                            compact_num(&mut m, f.metric[i]);
                            let _ = write!(m, "   ");
                        }
                        if flags & F_HAS_MEDIA != 0 && flags & F_SHOW_MEDIA == 0 {
                            let _ = write!(m, "▣ media  i");
                        }
                        let liked = if flags & F_LIKED != 0 { ACCENT } else { FAINT };
                        put(s, x, yy as u16, x + tw, m.as_str(), Style::new(liked, bg, 0));
                    }
                    yy += 1;
                }
            }
            if flags & F_ROOT != 0 && !f.extra[i].is_empty() {
                yy += 1;
                let rows = paint_text(s, hits, arena, f.extra[i], x, yy, tw, vt, bottom.min(yy + 12), Style::new(MUTED, bg, 0)) as i32;
                yy += rows.min(12);
            }
            if flags & F_SHOW_MEDIA != 0 {
                if yy >= vt && yy < bottom {
                    let kind = if service == Service::X { 1 } else { 3 };
                    let label = if service == Service::X { "→ open media on x.com" } else { "→ thumbnail on i.ytimg.com" };
                    let x1 = put(s, x, yy as u16, x + tw, label, Style::new(LINK, bg, UNDER));
                    hits.add(Hit { y: yy as u16, x0: x, x1, kind: H_ENTRY, arg: i as u32, span: Span { off: 0, len: kind } });
                }
            }
            y += eh;
            i += 1;
        }
        if i >= n && y < bottom && !t.more {
            put_center(s, cx, cx + cw, y as u16, "· end ·", Style::fg(FAINT));
        }
    }

    fn draw_chat(&mut self, l: &Layout) {
        let (cx, cw) = column(l, 76);
        let App { screen: s, tabs, active, hits, .. } = self;
        let t = &mut tabs[*active];
        let service = t.src.service();
        let model_name = t.model.as_str();
        let Store::Chat(c) = &mut t.store else { return };
        let arena: &TextArena = &t.arena;
        let dot = service_color(service);
        let who = if service == Service::Claude { "Claude" } else { "ChatGPT" };

        // the composer: a rounded box that grows with its text, up to 8 rows
        let iw = cw.saturating_sub(4).max(10);
        let input = c.input.as_str();
        let in_rows = measure(input, iw).clamp(1, 8);
        let box_h = in_rows + 2;
        let by = l.bottom - box_h;
        let border = if c.focused { MUTED } else { BORDER };
        let bx = cx;
        let bw = cw;
        s.back.fill_rect(bx, by, bx + bw, by + box_h, Style::new(BODY, BG, 0));
        let b = Style::new(border, BG, 0);
        s.back.fill(by, bx + 1, bx + bw - 1, '─', b);
        s.back.fill(by + box_h - 1, bx + 1, bx + bw - 1, '─', b);
        for yy in by + 1..by + box_h - 1 {
            s.back.fill(yy, bx, bx + 1, '│', b);
            s.back.fill(yy, bx + bw - 1, bx + bw, '│', b);
        }
        s.back.fill(by, bx, bx + 1, '╭', b);
        s.back.fill(by, bx + bw - 1, bx + bw, '╮', b);
        s.back.fill(by + box_h - 1, bx, bx + 1, '╰', b);
        s.back.fill(by + box_h - 1, bx + bw - 1, bx + bw, '╯', b);
        let mut label = Fmt::<96>::new();
        let _ = write!(label, " {model_name} ");
        put_right(s, bx + bw - 2, by, label.as_str(), Style::new(FAINT, BG, 0));
        let foot = if c.busy { " writing… " } else { " enter send · shift+enter new line " };
        put_right(s, bx + bw - 2, by + box_h - 1, foot, Style::new(FAINT, BG, 0));
        if input.is_empty() {
            let mut ph = Fmt::<64>::new();
            let _ = write!(ph, "Message {who}…");
            put(s, bx + 2, by + 1, bx + bw - 2, ph.as_str(), Style::new(FAINT, BG, 0));
            if c.focused {
                s.cursor = Some((bx + 2, by + 1));
            }
        } else {
            let total = measure(input, iw);
            let skip = total.saturating_sub(in_rows);
            let mut pos = 0usize;
            let mut row = 0u16;
            let mut cur = (bx + 2, by + 1);
            loop {
                let (end, next) = next_line(input, pos, iw as usize);
                if row >= skip {
                    let yy = by + 1 + row - skip;
                    put(s, bx + 2, yy, bx + bw - 2, &input[pos..end], Style::new(FG, BG, 0));
                    if c.input.cur >= pos && c.input.cur <= end {
                        cur = (bx + 2 + str_width(&input[pos..c.input.cur]) as u16, yy);
                    }
                }
                row += 1;
                if next >= input.len() {
                    if c.input.cur == input.len() && input.ends_with('\n') {
                        cur = (bx + 2, by + 1 + row.saturating_sub(skip).min(in_rows - 1));
                    }
                    break;
                }
                pos = next;
            }
            if c.focused {
                s.cursor = Some(cur);
            }
        }

        let top = l.view_top as i32;
        let bottom = by as i32 - 1;
        if c.msgs.is_empty() && !c.has_key {
            let mid = (l.view_top + by) / 2;
            let (name, var) = if service == Service::Claude { ("anthropic", "ANTHROPIC_API_KEY") } else { ("openai", "OPENAI_API_KEY") };
            let mut t1 = Fmt::<64>::new();
            let _ = write!(t1, "Connect {who}");
            put_center(s, l.x0, l.x1, mid.saturating_sub(2), t1.as_str(), Style::fg(FG).with(BOLD));
            let mut t2 = Fmt::<96>::new();
            let _ = write!(t2, "hayate key set {name}");
            put_center(s, l.x0, l.x1, mid, t2.as_str(), Style::fg(dot));
            let mut t3 = Fmt::<128>::new();
            let _ = write!(t3, "run it in a terminal, or set {var}, then reopen this tab");
            put_center(s, l.x0, l.x1, mid + 1, t3.as_str(), Style::fg(FAINT));
            return;
        }
        if c.msgs.is_empty() {
            let mid = (l.view_top + by) / 2;
            put_center(s, l.x0, l.x1, mid.saturating_sub(1), "What can I help with?", Style::fg(FG).with(BOLD));
            let mut sub = Fmt::<96>::new();
            let _ = write!(sub, "{who} · {model_name}");
            put_center(s, l.x0, l.x1, mid, sub.as_str(), Style::fg(dot));
            put_center(s, l.x0, l.x1, mid + 2, "answers stream in as they are written", Style::fg(FAINT));
            return;
        }
        // messages from the bottom up
        let mut y = bottom + c.scroll as i32;
        let last = c.msgs.len() - 1;
        let mut i = c.msgs.len();
        let bubble_max = (cw * 3 / 4).max(20);
        while i > 0 && y > top {
            i -= 1;
            let m = c.msgs[i];
            match m.role {
                Role::User => {
                    let text = arena.str(m.text);
                    // a bubble on the right, as wide as its longest line
                    let mut widest = 0u16;
                    let mut rows = 0u16;
                    let mut pos = 0;
                    loop {
                        let (end, next) = next_line(text, pos, bubble_max as usize - 2);
                        widest = widest.max(str_width(&text[pos..end]) as u16);
                        rows += 1;
                        if next >= text.len() {
                            break;
                        }
                        pos = next;
                    }
                    let bw = widest + 2;
                    let h = rows as i32 + 1;
                    let ty = y - h;
                    let x0 = cx + cw - bw;
                    for r in 0..rows as i32 {
                        let yy = ty + r;
                        if yy >= top && yy < bottom {
                            s.back.fill(yy as u16, x0, x0 + bw, ' ', Style::new(FG, SURFACE, 0));
                        }
                    }
                    paint_text(s, hits, arena, m.text, x0 + 1, ty, bw - 2, top, bottom, Style::new(FG, SURFACE, 0));
                    y = ty;
                }
                Role::Assistant => {
                    let cached = i == last && m.text.len > 0;
                    let rows = if m.text.len == 0 {
                        1
                    } else if cached {
                        c.wrap.update(arena.str(m.text), cw, i) as i32
                    } else {
                        measure(arena.str(m.text), cw) as i32
                    };
                    let h = rows + 2;
                    let ty = y - h;
                    if ty >= top && ty < bottom {
                        let busy = c.busy && i == last;
                        let mark = if busy { SPIN[(t.spin as usize + m.text.len as usize / 64) % 10] } else { "●" };
                        let x = put(s, cx, ty as u16, cx + cw, mark, Style::fg(dot));
                        let x = put(s, x + 1, ty as u16, cx + cw, who, Style::fg(FG).with(BOLD));
                        put(s, x + 2, ty as u16, cx + cw, arena.str(m.model), Style::fg(FAINT));
                    }
                    let body = ty + 1;
                    if m.text.len == 0 {
                        if body >= top && body < bottom {
                            put(s, cx, body as u16, cx + cw, "…", Style::fg(FAINT));
                        }
                    } else if cached {
                        let skip = (top - body).max(0) as usize;
                        let first = skip.min(c.wrap.starts.len() - 1);
                        let (pos, code) = (c.wrap.starts[first] as usize, c.wrap.code[first]);
                        paint_from(s, hits, arena, m.text, pos, code, cx, body + first as i32, cw, top, bottom, Style::new(BODY, BG, 0));
                    } else {
                        paint_text(s, hits, arena, m.text, cx, body, cw, top, bottom, Style::new(BODY, BG, 0));
                    }
                    y = ty;
                }
                Role::Note => {
                    let rows = measure(arena.str(m.text), cw) as i32;
                    let ty = y - rows - 1;
                    if ty >= top && ty < bottom {
                        put_center(s, cx, cx + cw, ty as u16, arena.str(m.text), Style::fg(FAINT).with(ITALIC));
                    }
                    y = ty;
                }
            }
        }
    }

    fn draw_page(&mut self, l: &Layout) {
        let (cx, cw) = column(l, 80);
        let view_h = l.bottom - l.view_top;
        {
            let t = &self.tabs[self.active];
            let Store::Page(p) = &t.store else { return };
            if p.blocks.is_empty() {
                let host = match &t.src {
                    Source::Web { url } => url.trim_start_matches("https://").split('/').next().unwrap_or(""),
                    _ => "",
                };
                if t.loading {
                    let mut msg = Fmt::<160>::new();
                    let _ = write!(msg, "{} Loading {host}", SPIN[t.spin as usize % 10]);
                    let mid = l.view_top + view_h / 2;
                    put_center(&mut self.screen, l.x0, l.x1, mid, msg.as_str(), Style::fg(MUTED));
                    return;
                }
                let err = t.error;
                let body = err.map(|e| e).unwrap_or(Line::new("the page had no text hayate can show"));
                return self.empty_state(l, "Could not show this page", body.as_str(), "r reloads · o opens it in your browser · backspace goes back");
            }
        }
        let App { screen: s, tabs, active, hits, .. } = self;
        let t = &mut tabs[*active];
        let Store::Page(p) = &mut t.store else { return };
        let arena: &TextArena = &t.arena;
        // a link focused with tab scrolls its block into view
        if let Some(fl) = p.focus {
            if t.follow {
                let b = p.blocks.partition_point(|b| b.first_link + b.links <= fl as u32);
                t.cursor = b.min(p.blocks.len() - 1);
            }
        }
        let n = p.blocks.len();
        {
            let pr: &Page = p;
            scroll_view(
                Scroll { cursor: &mut t.cursor, top: &mut t.top, top_row: &mut t.top_row, pending: &mut t.pending, follow: t.follow },
                n,
                view_h,
                false,
                &|i| block_height(arena, pr, i, cw),
            );
        }
        let vt = l.view_top as i32;
        let bottom = l.bottom as i32;
        let mut y = vt - t.top_row as i32;
        let mut i = t.top;
        while i < n && y < bottom {
            let b = p.blocks[i];
            let h = block_height(arena, p, i, cw) as i32;
            let mut yy = y + (matches!(b.kind, 1 | 2) && i > 0) as i32;
            let (indent, _) = block_indent(&b);
            let x = cx + indent;
            let tw = cw.saturating_sub(indent).max(10);
            let links = &p.links[(b.first_link as usize).min(p.links.len())..((b.first_link + b.links) as usize).min(p.links.len())];
            let role = row_role(arena, p, i);
            let (st, link) = match b.kind {
                K_ROW if role == 1 => (Style::fg(FAINT), Style::fg(FAINT)),
                K_ROW if role == 2 => (Style::fg(MUTED), Style::fg(MUTED)),
                1 => (Style::fg(FG).with(BOLD), Style::fg(FG).with(BOLD)),
                2 => (Style::fg(FG).with(BOLD), Style::fg(LINK).with(BOLD)),
                3..=6 => (Style::fg(BODY).with(BOLD), Style::fg(LINK).with(BOLD)),
                K_PRE => (Style::new(CODE, CODE_BG, 0), Style::new(LINK, CODE_BG, 0)),
                K_QUOTE => (Style::fg(MUTED).with(ITALIC), Style::fg(LINK)),
                _ => (Style::fg(BODY), Style::fg(LINK)),
            };
            match b.kind {
                K_HR => {
                    if yy >= vt && yy < bottom {
                        s.back.fill(yy as u16, cx, cx + cw, '─', Style::fg(BORDER));
                    }
                }
                _ => {
                    let rows = if b.kind == K_PRE { measure(arena.str(b.text), tw) as i32 } else { 0 };
                    if b.kind == K_PRE {
                        for r in 0..rows {
                            let ry = yy + r;
                            if ry >= vt && ry < bottom {
                                s.back.fill(ry as u16, cx, cx + cw, ' ', Style::new(CODE, CODE_BG, 0));
                            }
                        }
                    }
                    if b.kind == K_LI && yy >= vt && yy < bottom {
                        let bullet = if b.depth > 1 { "◦" } else { "•" };
                        put(s, x - 2, yy as u16, x, bullet, Style::fg(FAINT));
                    }
                    let start = yy;
                    // on a ranked row the first link is the title; the rest (the site) recede
                    let lead = if role == 1 { Style::fg(FG).with(BOLD) } else { link };
                    let r = paint_rich(s, hits, arena, b.text, links, b.first_link, p.focus, x, yy, tw, vt, bottom, st, lead, link);
                    if b.kind == K_QUOTE {
                        for q in start..start + r as i32 {
                            if q >= vt && q < bottom {
                                s.back.fill(q as u16, cx, cx + 1, '│', Style::fg(BORDER));
                            }
                        }
                    }
                    yy += r as i32;
                }
            }
            let _ = yy;
            y += h;
            i += 1;
        }
    }

    fn draw_start(&mut self, l: &Layout) {
        let (cx, cw) = column(l, 64);
        let App { screen: s, tabs, active, hits, .. } = self;
        let t = &tabs[*active];
        let Store::Start(sp) = &t.store else { return };
        let view_h = l.bottom - l.view_top;
        let y0 = l.view_top + view_h.saturating_sub(16) / 2;
        // the name
        let name_w = 4 + 1 + 6;
        let nx = cx + cw.saturating_sub(name_w) / 2;
        let x = put(s, nx, y0, cx + cw, "疾風", Style::fg(ACCENT).with(BOLD));
        put(s, x + 1, y0, cx + cw, "hayate", Style::fg(FG).with(BOLD));
        put_center(s, cx, cx + cw, y0 + 1, "the web and your feeds, as text", Style::fg(FAINT));
        // the search box
        let by = y0 + 3;
        let focused = sp.focus == 0;
        let border = if focused { MUTED } else { BORDER };
        let b = Style::new(border, BG, 0);
        s.back.fill(by, cx + 1, cx + cw - 1, '─', b);
        s.back.fill(by + 2, cx + 1, cx + cw - 1, '─', b);
        s.back.fill(by, cx, cx + 1, '╭', b);
        s.back.fill(by, cx + cw - 1, cx + cw, '╮', b);
        s.back.fill(by + 2, cx, cx + 1, '╰', b);
        s.back.fill(by + 2, cx + cw - 1, cx + cw, '╯', b);
        s.back.fill(by + 1, cx, cx + 1, '│', b);
        s.back.fill(by + 1, cx + cw - 1, cx + cw, '│', b);
        let ix = put(s, cx + 2, by + 1, cx + cw - 2, "/", Style::fg(FAINT));
        let text = sp.input.as_str();
        if text.is_empty() {
            put(s, ix + 1, by + 1, cx + cw - 2, "Search the web or type an address", Style::fg(FAINT));
            if focused {
                s.cursor = Some((ix + 1, by + 1));
            }
        } else {
            // keep the end of long input in view
            let room = (cx + cw - 3).saturating_sub(ix + 1) as usize;
            let mut start = 0;
            while str_width(&text[start..sp.input.cur.max(start)]) > room {
                start += text[start..].chars().next().map_or(1, char::len_utf8);
            }
            put(s, ix + 1, by + 1, cx + cw - 2, &text[start..], Style::fg(FG));
            if focused {
                s.cursor = Some((ix + 1 + str_width(&text[start..sp.input.cur]) as u16, by + 1));
            }
        }
        // shortcuts
        let tile_w = cw / 4;
        let rows: [(&str, u16, usize); 2] = [("Your services", by + 4, 0), ("Read", by + 7, 4)];
        for (label, ly, first) in rows {
            put(s, cx, ly, cx + cw, label, Style::fg(FAINT));
            for k in 0..4 {
                let idx = first + k;
                let tx = cx + k as u16 * tile_w;
                let on = sp.focus == idx + 1;
                let bg = if on { SEL_BG } else { BG };
                s.back.fill(ly + 1, tx, tx + tile_w - 1, ' ', Style::new(BODY, bg, 0));
                if on {
                    put(s, tx, ly + 1, tx + 1, "▎", Style::new(ACCENT, bg, 0));
                }
                let (name, color) = if first == 0 {
                    [("X", X_DOT), ("YouTube", YT), ("Claude", CLAUDE), ("ChatGPT", GPT)][k]
                } else {
                    (SITES[k].0, WEB)
                };
                let mut x = tx + 2;
                if first == 0 {
                    x = put(s, x, ly + 1, tx + tile_w - 1, "●", Style::new(color, bg, 0)) + 1;
                }
                put(s, x, ly + 1, tx + tile_w - 1, name, Style::new(if on { FG } else { BODY }, bg, if on { BOLD } else { 0 }));
                hits.add(Hit { y: ly + 1, x0: tx, x1: tx + tile_w - 1, kind: H_TILE, arg: idx as u32, span: Span::EMPTY });
            }
        }
        put_center(s, cx, cx + cw, by + 11, "ctrl+k commands  ·  ctrl+l address  ·  ctrl+t new tab  ·  ? keys", Style::fg(FAINT));
    }

    fn draw_compose(&mut self, l: &Layout) {
        let (cx, cw) = column(l, 72);
        let reply = match &self.mode {
            Mode::Compose { reply_to } => reply_to.as_ref().map(|r| r.1.clone()),
            _ => None,
        };
        let s = &mut self.screen;
        let mut title = Fmt::<96>::new();
        let _ = match &reply {
            Some(h) => write!(title, "Reply to @{h}"),
            None => write!(title, "New post"),
        };
        put(s, cx, l.view_top + 1, cx + cw, title.as_str(), Style::fg(FG).with(BOLD));
        let text = self.compose.as_str();
        let iw = cw.saturating_sub(4);
        let rows = measure(text, iw).clamp(4, (l.bottom - l.view_top).saturating_sub(8).max(4));
        let (ix, iy) = panel(s, cx, l.view_top + 3, cw, rows + 2, "", MUTED);
        // the panel uses the surface color; the editor reads better on the page color
        s.back.fill_rect(ix - 1, iy, ix + iw + 1, iy + rows, Style::new(FG, SURFACE, 0));
        let mut pos = 0usize;
        let mut row = 0u16;
        let mut cur = (ix, iy);
        loop {
            let (end, next) = next_line(text, pos, iw as usize);
            if row < rows {
                put(s, ix, iy + row, ix + iw, &text[pos..end], Style::new(FG, SURFACE, 0));
            }
            if self.compose.cur >= pos && self.compose.cur <= end {
                cur = (ix + str_width(&text[pos..self.compose.cur]) as u16, iy + row.min(rows - 1));
            }
            row += 1;
            if next >= text.len() {
                if text.ends_with('\n') && self.compose.cur == text.len() {
                    cur = (ix, iy + row.min(rows - 1));
                }
                break;
            }
            pos = next;
        }
        if text.is_empty() {
            put(s, ix, iy, ix + iw, "What is happening?", Style::new(FAINT, SURFACE, 0));
        }
        let count = x_weight(text);
        let mut foot = Fmt::<64>::new();
        let _ = write!(foot, "{count} / 280");
        let fy = iy + rows + 1;
        put_right(s, cx + cw, fy + 1, foot.as_str(), Style::fg(if count > 280 { ERR } else { MUTED }));
        put(s, cx, fy + 1, cx + cw, "ctrl+s posts · esc cancels", Style::fg(FAINT));
        s.cursor = Some(cur);
    }

    fn draw_palette(&mut self, kind: PalKind) {
        let mut items = [Act::Quit; 32];
        let n = self.palette_items(kind, &mut items);
        let (w, h) = (self.screen.back.w, self.screen.back.h);
        let pw = 72.min(w.saturating_sub(6));
        let px = (w - pw) / 2;
        let py = (h / 6).max(2);
        let max_rows = 10.min(h.saturating_sub(py + 6)) as usize;
        let rows = n.min(max_rows).max(1);
        let ph = 3 + rows as u16 + 2;
        let title = match kind {
            PalKind::Commands => "Commands",
            PalKind::Address => "Go to",
            PalKind::Search => "Search",
        };
        let svc = self.tabs.get(self.active).map(|t| t.service()).unwrap_or(Service::Web);
        let q = self.input.as_str();
        let s = &mut self.screen;
        let (ix, iy) = panel(s, px, py, pw, ph, title, BORDER);
        let iw = pw - 4;
        let x = put(s, ix, iy, ix + iw, "›", Style::new(ACCENT, SURFACE, BOLD)) + 1;
        if q.is_empty() {
            let ph_text = match kind {
                PalKind::Commands => "Type a command, an address or a search",
                PalKind::Address => "Search the web or enter an address",
                PalKind::Search => "Search this service, your history or the web",
            };
            put(s, x, iy, ix + iw, ph_text, Style::new(FAINT, SURFACE, 0));
            s.cursor = Some((x, iy));
        } else {
            put(s, x, iy, ix + iw, q, Style::new(FG, SURFACE, 0));
            s.cursor = Some(((x + str_width(&q[..self.input.cur.min(q.len())]) as u16).min(ix + iw), iy));
        }
        s.back.fill(iy + 1, px + 1, px + pw - 1, '─', Style::new(BORDER, SURFACE, 0));
        s.back.fill(iy + 1, px, px + 1, '├', Style::new(BORDER, SURFACE, 0));
        s.back.fill(iy + 1, px + pw - 1, px + pw, '┤', Style::new(BORDER, SURFACE, 0));
        let sel = self.pal_sel.min(n.saturating_sub(1));
        let first = if sel >= rows { sel + 1 - rows } else { 0 };
        if n == 0 {
            put(s, ix, iy + 2, ix + iw, "No matches", Style::new(FAINT, SURFACE, 0));
        }
        for r in 0..rows.min(n) {
            let k = first + r;
            let act = items[k];
            let y = iy + 2 + r as u16;
            let on = k == sel;
            let bg = if on { SEL_BG } else { SURFACE };
            s.back.fill(y, px + 1, px + pw - 1, ' ', Style::new(BODY, bg, 0));
            if on {
                put(s, px + 1, y, px + 2, "▎", Style::new(ACCENT, bg, 0));
            }
            let mut label = Fmt::<200>::new();
            let (icon, hint): (&str, &str) = match act {
                Act::Go => {
                    let _ = write!(label, "Go to {q}");
                    ("→", "enter")
                }
                Act::WebSearch => {
                    let _ = write!(label, "Search the web for “{q}”");
                    ("/", "")
                }
                Act::ServiceSearch => {
                    let _ = write!(label, "Search {} for “{q}”", svc.name());
                    ("/", if svc == Service::YouTube { "100 units" } else { "" })
                }
                Act::HistorySearch => {
                    let _ = write!(label, "Search your history for “{q}”");
                    ("/", "")
                }
                Act::Run => {
                    let _ = write!(label, ":{q}");
                    ("›", "run")
                }
                a => {
                    let (l, k, _) = ITEMS.iter().find(|(_, _, x)| *x == a).copied().unwrap_or(("", "", a));
                    let _ = write!(label, "{l}");
                    (" ", k)
                }
            };
            put(s, ix, y, ix + 2, icon, Style::new(if on { ACCENT } else { FAINT }, bg, 0));
            let hw = str_width(hint) as u16;
            put(s, ix + 2, y, ix + iw - hw - 1, label.as_str(), Style::new(if on { FG } else { BODY }, bg, if on { BOLD } else { 0 }));
            put_right(s, ix + iw, y, hint, Style::new(FAINT, bg, 0));
        }
        put(s, ix, py + ph - 2, ix + iw, "↑↓ select   enter run   esc close", Style::new(FAINT, SURFACE, 0));
    }

    fn draw_overlay(&mut self, o: Overlay) {
        let (w, h) = (self.screen.back.w, self.screen.back.h);
        let st = Style::new(BODY, SURFACE, 0);
        let mu = Style::new(MUTED, SURFACE, 0);
        let fa = Style::new(FAINT, SURFACE, 0);
        let fg = Style::new(FG, SURFACE, BOLD);
        match o {
            Overlay::Mem => {
                let pw = 66.min(w - 4);
                let (x, mut y) = panel(&mut self.screen, (w - pw) / 2, (h.saturating_sub(20)) / 2, pw, 20, "Memory", BORDER);
                let x1 = x + pw - 4;
                put(&mut self.screen, x, y, x1, "arena            used        high      budget", fa);
                y += 1;
                for (i, name) in ARENA_NAMES.iter().enumerate() {
                    let id = [ArenaId::Frame, ArenaId::Net, ArenaId::TabText, ArenaId::Store, ArenaId::Pixels][i];
                    let used = if id == ArenaId::Store { self.store_bytes() } else { arena::used(id) };
                    let high = if id == ArenaId::Store { used } else { arena::high(id) };
                    let mut f = Fmt::<96>::new();
                    let _ = write!(f, "{:<11} ", name);
                    kb(&mut f, used, 11);
                    kb(&mut f, high, 12);
                    kb(&mut f, BUDGETS[i], 12);
                    let over = used > BUDGETS[i];
                    put(&mut self.screen, x, y, x1, f.as_str(), if over { Style::new(ERR, SURFACE, 0) } else { st });
                    y += 1;
                }
                let m = sys::process_memory();
                y += 1;
                let mut f = Fmt::<96>::new();
                let _ = write!(f, "{:.1} MB", m.working_set as f64 / 1048576.0);
                put(&mut self.screen, x, y, x1, "working set", mu);
                put(&mut self.screen, x + 16, y, x1, f.as_str(), fg);
                let mut f = Fmt::<96>::new();
                let _ = write!(f, "peak {:.1} MB · private {:.1} MB", mb(m.peak_working_set), mb(m.private_bytes));
                put(&mut self.screen, x + 28, y, x1, f.as_str(), mu);
                y += 2;
                let (p50, p99, max) = self.hist.stats(false);
                let mut f = Fmt::<96>::new();
                let _ = write!(f, "p50 {p50} µs · p99 {p99} µs · max {max} µs");
                put(&mut self.screen, x, y, x1, "frame", mu);
                put(&mut self.screen, x + 16, y, x1, f.as_str(), st);
                y += 1;
                let (p50, p99, max) = self.hist.stats(true);
                let mut f = Fmt::<96>::new();
                let _ = write!(f, "p50 {p50} µs · p99 {p99} µs · max {max} µs");
                put(&mut self.screen, x, y, x1, "key to screen", mu);
                put(&mut self.screen, x + 16, y, x1, f.as_str(), st);
                y += 2;
                let (mut vis, mut hid, mut sl, mut ar) = (0, 0, 0, 0);
                for t in &self.tabs {
                    match t.state {
                        TabState::Visible => vis += 1,
                        TabState::Hidden => hid += 1,
                        TabState::Asleep => sl += 1,
                        TabState::Archived => ar += 1,
                    }
                }
                let mut f = Fmt::<96>::new();
                let _ = write!(f, "{vis} visible · {hid} hidden · {sl} asleep · {ar} archived");
                put(&mut self.screen, x, y, x1, "tabs", mu);
                put(&mut self.screen, x + 16, y, x1, f.as_str(), st);
                y += 1;
                let mut f = Fmt::<96>::new();
                let _ = write!(f, "{:.1} ms", self.startup_us as f64 / 1000.0);
                put(&mut self.screen, x, y, x1, "start", mu);
                put(&mut self.screen, x + 16, y, x1, f.as_str(), st);
                y += 2;
                put(&mut self.screen, x, y, x1, "images show as links for now; the pixels arena stays empty", fa);
            }
            Overlay::Net => {
                let pw = 100.min(w - 4);
                let ph = 24.min(h - 2);
                let (x, mut y) = panel(&mut self.screen, (w - pw) / 2, (h - ph) / 2, pw, ph, "Requests", BORDER);
                let x1 = x + pw - 4;
                put(&mut self.screen, x, y, x1, "service  status   out kb   in kb   ttfb ms  total ms     cost  request", fa);
                y += 1;
                let n = self.net_log.len();
                let show = n.min(ph as usize - 4);
                for k in 0..show {
                    let e = if n < NET_LOG { self.net_log[n - 1 - k] } else { self.net_log[(self.net_n + NET_LOG - 1 - k) % NET_LOG] };
                    let mut f = Fmt::<200>::new();
                    let _ = write!(f, "{:<8} {:>6} {:>8.1} {:>7.1} {:>9} {:>9} ", e.service.name(), e.status, e.bytes_out as f64 / 1024.0, e.bytes_in as f64 / 1024.0, e.ttfb_ms, e.total_ms);
                    if e.service == Service::YouTube {
                        let _ = write!(f, "{:>6} u", e.cost);
                    } else {
                        let _ = write!(f, "${:>7.4}", e.cost as f64 / 1e6);
                    }
                    let _ = write!(f, "  {}", e.what.as_str());
                    let style = if e.status >= 400 || e.status == 0 { Style::new(ERR, SURFACE, 0) } else { st };
                    put(&mut self.screen, x, y, x1, f.as_str(), style);
                    y += 1;
                }
                if n == 0 {
                    put(&mut self.screen, x, y, x1, "no requests yet", fa);
                }
            }
            Overlay::Spend => {
                let pw = 60.min(w - 4);
                let (x, mut y) = panel(&mut self.screen, (w - pw) / 2, (h.saturating_sub(10)) / 2, pw, 10, "Spend this month", BORDER);
                let x1 = x + pw - 4;
                let rows = [("Claude", self.spend.0, self.cfg.cap_usd[0], CLAUDE), ("ChatGPT", self.spend.1, self.cfg.cap_usd[1], GPT), ("X", self.spend.2, self.cfg.cap_usd[2], X_DOT)];
                for (name, used, cap, c) in rows {
                    let nx = put(&mut self.screen, x, y, x1, "●", Style::new(c, SURFACE, 0));
                    put(&mut self.screen, nx + 1, y, x1, name, st);
                    let mut f = Fmt::<64>::new();
                    let _ = write!(f, "${:.3}", used as f64 / 1e6);
                    let warn = used as f64 / 1e6 > cap * 0.8;
                    put(&mut self.screen, x + 14, y, x1, f.as_str(), if warn { Style::new(WARN, SURFACE, BOLD) } else { fg });
                    let mut f = Fmt::<64>::new();
                    let _ = write!(f, "of ${cap:.2}");
                    put(&mut self.screen, x + 26, y, x1, f.as_str(), mu);
                    y += 1;
                }
                let nx = put(&mut self.screen, x, y, x1, "●", Style::new(YT, SURFACE, 0));
                put(&mut self.screen, nx + 1, y, x1, "YouTube", st);
                let mut f = Fmt::<64>::new();
                let _ = write!(f, "{}", self.spend.3);
                put(&mut self.screen, x + 14, y, x1, f.as_str(), fg);
                let mut f = Fmt::<64>::new();
                let _ = write!(f, "of {} units today", self.cfg.yt_daily_quota);
                put(&mut self.screen, x + 26, y, x1, f.as_str(), mu);
                y += 2;
                put(&mut self.screen, x, y, x1, "caps live in the config; a call over a cap is refused", fa);
            }
            Overlay::Help => {
                let rows: [(&str, &str); 18] = [
                    ("ctrl+k", "commands: everything, searchable"),
                    ("ctrl+l", "go to an address or search the web"),
                    ("ctrl+t  ctrl+w", "new tab, close tab"),
                    ("ctrl+tab  alt+1..9", "next tab, jump to a tab"),
                    ("j k  ↑ ↓", "move, or scroll a page"),
                    ("space  pgup pgdn", "a screen at a time"),
                    ("enter", "open: thread, video, link"),
                    ("tab  shift+tab", "on a page: next and previous link"),
                    ("bksp  L", "back, forward"),
                    ("r", "reload a page; reply to a post"),
                    ("/", "search"),
                    ("c", "comments of a video"),
                    ("i  l  n", "media link, like, new post or chat"),
                    ("o", "open in your system browser"),
                    ("u", "reopen the last closed tab"),
                    ("e", "export this view to markdown"),
                    (":model  :system", "in a chat: switch model or prompt"),
                    ("ctrl+c", "quit"),
                ];
                let ph = rows.len() as u16 + 3;
                let pw = 64.min(w - 4);
                let (x, mut y) = panel(&mut self.screen, (w - pw) / 2, (h.saturating_sub(ph)) / 2, pw, ph, "Keys", BORDER);
                let x1 = x + pw - 4;
                for (k, d) in rows {
                    put(&mut self.screen, x, y, x + 20, k, fg);
                    put(&mut self.screen, x + 21, y, x1, d, mu);
                    y += 1;
                }
            }
        }
    }
}

// ---------------------------------------------------------------- painting text

/// Paints wrapped text with urls underlined and clickable. Returns rows used.
#[allow(clippy::too_many_arguments)]
fn paint_text(s: &mut Screen, hits: &mut Hits, arena: &TextArena, span: Span, x: u16, y: i32, width: u16, top: i32, bottom: i32, st: Style) -> u16 {
    paint_from(s, hits, arena, span, 0, false, x, y, width, top, bottom, st)
}

/// Paints from byte `pos0` of the text, which starts a line, with the code-fence state
/// at that line. Lines inside ``` fences get the code colors.
#[allow(clippy::too_many_arguments)]
fn paint_from(s: &mut Screen, hits: &mut Hits, arena: &TextArena, span: Span, pos0: usize, code0: bool, x: u16, y: i32, width: u16, top: i32, bottom: i32, st: Style) -> u16 {
    let text = arena.str(span);
    let mut pos = pos0.min(text.len());
    let mut rows = 0u16;
    let mut code = code0;
    loop {
        let (end, next) = next_line(text, pos, width as usize);
        let line = &text[pos..end];
        let yy = y + rows as i32;
        let fence = line.starts_with("```");
        if yy >= top && yy < bottom {
            let lst = if code || fence {
                s.back.fill(yy as u16, x, x + width, ' ', Style::new(CODE, CODE_BG, 0));
                Style::new(if fence { FAINT } else { CODE }, CODE_BG, 0)
            } else {
                st
            };
            let mut col = x;
            let mut rest = line;
            let mut off = pos;
            while !rest.is_empty() {
                match rest.find("https://").or_else(|| rest.find("http://")) {
                    Some(p) => {
                        col = put(s, col, yy as u16, x + width, &rest[..p], lst);
                        let url_len = rest[p..].find(|c: char| c.is_whitespace() || c == ')' || c == '"' || c == '>').unwrap_or(rest.len() - p);
                        let url = &rest[p..p + url_len];
                        let x1 = put(s, col, yy as u16, x + width, url, Style::new(LINK, lst.bg, 0));
                        hits.add(Hit { y: yy as u16, x0: col, x1, kind: H_URL, arg: 0, span: Span { off: span.off + (off + p) as u32, len: url_len as u32 } });
                        col = x1;
                        off += p + url_len;
                        rest = &rest[p + url_len..];
                    }
                    None => {
                        put(s, col, yy as u16, x + width, rest, lst);
                        break;
                    }
                }
            }
        }
        if fence {
            code = !code;
        }
        rows += 1;
        if next >= text.len() || yy >= bottom {
            break;
        }
        pos = next;
    }
    rows.max(1)
}

/// Paints a page block: wrapped text with its links styled and clickable. Returns rows.
#[allow(clippy::too_many_arguments)]
fn paint_rich(
    s: &mut Screen,
    hits: &mut Hits,
    arena: &TextArena,
    span: Span,
    links: &[PageLink],
    first_link: u32,
    focus: Option<usize>,
    x: u16,
    y: i32,
    width: u16,
    top: i32,
    bottom: i32,
    st: Style,
    lead_st: Style,
    link_st: Style,
) -> u16 {
    let text = arena.str(span);
    let mut pos = 0usize;
    let mut rows = 0u16;
    loop {
        let (end, next) = next_line(text, pos, width as usize);
        let yy = y + rows as i32;
        if yy >= top && yy < bottom {
            let a0 = span.off + pos as u32;
            let a1 = span.off + end as u32;
            let mut col = x;
            let mut cur = pos;
            for (k, l) in links.iter().enumerate() {
                let la = l.start.max(a0);
                let lb = (l.start + l.len).min(a1);
                if la >= lb {
                    continue;
                }
                let (ra, rb) = ((la - span.off) as usize, (lb - span.off) as usize);
                if ra > cur {
                    if let Some(t) = text.get(cur..ra) {
                        col = put(s, col, yy as u16, x + width, t, st);
                    }
                }
                let focused = focus == Some(first_link as usize + k);
                let base = if k == 0 { lead_st } else { link_st };
                let ls = if focused { Style::new(FG, FOCUS_BG, base.attr | UNDER) } else { base };
                let c0 = col;
                if let Some(t) = text.get(ra..rb) {
                    col = put(s, col, yy as u16, x + width, t, ls);
                }
                hits.add(Hit { y: yy as u16, x0: c0, x1: col, kind: H_PAGELINK, arg: first_link + k as u32, span: Span::EMPTY });
                cur = rb;
            }
            if cur < end {
                if let Some(t) = text.get(cur..end) {
                    put(s, col, yy as u16, x + width, t, st);
                }
            }
        }
        rows += 1;
        if next >= text.len() || yy >= bottom {
            break;
        }
        pos = next;
    }
    rows.max(1)
}

fn mb(b: usize) -> f64 {
    b as f64 / 1048576.0
}

fn kb<const N: usize>(f: &mut Fmt<N>, b: usize, width: usize) {
    if b >= 1048576 {
        let _ = write!(f, "{:>w$.1} MB", b as f64 / 1048576.0, w = width - 3);
    } else {
        let _ = write!(f, "{:>w$.0} kB", b as f64 / 1024.0, w = width - 3);
    }
}

fn rel_time<const N: usize>(f: &mut Fmt<N>, now: u64, ts: u32) {
    if ts == 0 {
        let _ = write!(f, "?");
        return;
    }
    let d = now.saturating_sub(ts as u64);
    let _ = match d {
        0..60 => write!(f, "now"),
        60..3600 => write!(f, "{}m", d / 60),
        3600..86400 => write!(f, "{}h", d / 3600),
        86400..2_592_000 => write!(f, "{}d", d / 86400),
        _ => {
            const M: [&str; 12] = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];
            let (y, m, day) = crate::adapters::civil_from_days(ts as i64 / 86400);
            let (ny, _, _) = crate::adapters::civil_from_days(now as i64 / 86400);
            if y == ny { write!(f, "{} {}", M[m as usize - 1], day) } else { write!(f, "{} {} {}", M[m as usize - 1], day, y) }
        }
    };
}

fn compact_num<const N: usize>(f: &mut Fmt<N>, n: u32) {
    let _ = if n >= 1_000_000 {
        write!(f, "{:.1}M", n as f64 / 1e6)
    } else if n >= 10_000 {
        write!(f, "{}K", n / 1000)
    } else if n >= 1000 {
        write!(f, "{:.1}K", n as f64 / 1e3)
    } else {
        write!(f, "{n}")
    };
}

pub(super) fn iso_date(ts: u32) -> String {
    let (y, m, d) = crate::adapters::civil_from_days(ts as i64 / 86400);
    let s = ts % 86400;
    format!("{y}-{m:02}-{d:02} {:02}:{:02}", s / 3600, s % 3600 / 60)
}

/// x counts most characters as 1, wide (cjk, emoji) as 2, and every url as 23.
pub fn x_weight(s: &str) -> usize {
    let mut n = 0;
    for word in s.split_inclusive(char::is_whitespace) {
        let w = word.trim_end();
        if w.starts_with("https://") || w.starts_with("http://") {
            n += 23 + (word.len() - w.len());
            continue;
        }
        for g in crate::text::graphemes(word) {
            n += if g.width >= 2 { 2 } else { 1 };
        }
    }
    n
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn x_weights() {
        assert_eq!(x_weight("hello"), 5);
        assert_eq!(x_weight("日本"), 4);
        assert_eq!(x_weight("see https://example.com/a/very/long/path ok"), 4 + 23 + 3);
    }
}
