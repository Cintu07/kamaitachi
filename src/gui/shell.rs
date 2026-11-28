//! The window around the pages, laid out the quiet way: a column of tabs on white
//! marble, the page as a card with rounded corners beside it, and an address field that
//! comes up over the page when you want to go somewhere (Ctrl+L, Ctrl+T or Ctrl+K).
//!
//! Every size here is css px; the painter wants device px, so they are multiplied by
//! the window's scale at the last moment.

use super::brush::Pencil;
use super::prefs::{self, Side};
use super::*;

/// Room around the card on its three free sides.
const GAP: f32 = 8.0;
const RADIUS: f32 = 10.0;
/// The address field's widest.
const FIELD_W: f32 = 560.0;
const ROW: f32 = 36.0;

/// One line under the address field.
#[derive(Clone, Debug, PartialEq)]
pub(super) enum Suggest {
    /// go to what was typed: an address, or a search for the words
    Go(String),
    /// an open tab whose name or address matches
    Tab(usize),
}

impl App {
    // ---- where things are ----

    pub(super) fn column_w(&self) -> f32 {
        if self.column_hidden { 0.0 } else { prefs::get().column }
    }

    /// The tab column, in window css px.
    pub(super) fn column_box(&self) -> Rect {
        let (w, h) = (self.w as f32 / self.scale, self.h as f32 / self.scale);
        let cw = self.column_w();
        match prefs::get().side {
            Side::Left => Rect::new(0.0, 0.0, cw, h),
            Side::Right => Rect::new(w - cw, 0.0, cw, h),
        }
    }

    /// Where the page goes, in window css px.
    pub(super) fn page_box(&self) -> Rect {
        let p = prefs::get();
        let (w, h) = (self.w as f32 / self.scale, self.h as f32 / self.scale);
        let g = if p.card { GAP } else { 0.0 };
        let cw = self.column_w();
        let x = if p.side == Side::Left && cw > 0.0 { cw } else { g };
        let right = if p.side == Side::Right && cw > 0.0 { w - cw } else { w - g };
        Rect::new(x, g, (right - x).max(1.0), (h - 2.0 * g).max(1.0))
    }

    /// The page's top-left corner in device px.
    pub(super) fn page_origin(&self) -> (f32, f32) {
        let b = self.page_box();
        (b.x * self.scale, b.y * self.scale)
    }

    pub(super) fn in_page(&self, x: f32, y: f32) -> bool {
        let b = self.page_box();
        b.contains(x / self.scale, y / self.scale)
    }

    /// A point in the window (device px) as a point on the page (css px of the page).
    pub(super) fn to_page(&self, x: f32, y: f32) -> (f32, f32) {
        let (ox, oy) = self.page_origin();
        let k = self.k();
        ((x - ox) / k, (y - oy) / k + self.tab().scroll)
    }

    fn row_h(&self) -> f32 {
        if prefs::get().roomy { 38.0 } else { 32.0 }
    }

    // ---- the card ----

    /// The card's face, before the page is drawn on it. Square: the corners are rounded
    /// afterwards, over whatever the page drew there.
    pub(super) fn draw_card(&mut self, bg: Rgba) {
        let b = self.page_box();
        self.rect(b, bg, 0.0);
    }

    /// Rounds the card's corners and edges it with a hairline. Only the four corners are
    /// curves, and each is drawn clipped to its own square: a ring as large as the card
    /// would make the painter visit every pixel inside it.
    pub(super) fn draw_card_edges(&mut self) {
        if !prefs::get().card {
            return;
        }
        let (b, k, s) = (self.page_box(), self.scale, sk());
        let r = RADIUS;
        let (x0, y0, x1, y1) = (b.x * k, b.y * k, (b.x + b.w) * k, (b.y + b.h) * k);
        let rk = r * k;
        let corners = [[x0, y0, x0 + rk, y0 + rk], [x1 - rk, y0, x1, y0 + rk], [x0, y1 - rk, x0 + rk, y1], [x1 - rk, y1 - rk, x1, y1]];
        self.gpu.round_corners(x0, y0, x1 - x0, y1 - y0, rk, s.frame);
        for clip in corners {
            self.gpu.ring(x0, y0, x1 - x0, y1 - y0, 1.0, s.hairline, rk, clip);
        }
        let line = s.hairline;
        self.gpu.rect(x0 + rk, y0, x1 - x0 - 2.0 * rk, 1.0, line, 0.0, gpu::NO_CLIP);
        self.gpu.rect(x0 + rk, y1 - 1.0, x1 - x0 - 2.0 * rk, 1.0, line, 0.0, gpu::NO_CLIP);
        self.gpu.rect(x0, y0 + rk, 1.0, y1 - y0 - 2.0 * rk, line, 0.0, gpu::NO_CLIP);
        self.gpu.rect(x1 - 1.0, y0 + rk, 1.0, y1 - y0 - 2.0 * rk, line, 0.0, gpu::NO_CLIP);
    }

    /// A thin line along the card's top while the page loads.
    pub(super) fn draw_loading(&mut self) {
        let t = self.tab();
        if !t.loading || !prefs::get().loading_line {
            return;
        }
        let part = if t.dl.is_some() || t.full.is_some() { 0.72 } else { 0.3 };
        let b = self.page_box();
        let r = if prefs::get().card { RADIUS } else { 0.0 };
        self.rect(Rect::new(b.x + r, b.y, (b.w - 2.0 * r) * part, 2.0), sk().accent, 1.0);
    }

    // ---- the column ----

    /// How far down its page a tab has been read, 0 to 1, if it is long enough to say.
    fn read_part(&self, i: usize) -> Option<f32> {
        let t = &self.tabs[i];
        let dl = t.dl.as_ref()?;
        let vh = self.view_h();
        (dl.height > vh * 1.5).then(|| ((t.scroll + vh) / dl.height).clamp(0.0, 1.0))
    }

    /// The letter a tab shows in its square: the first of its site's name.
    fn letter(t: &Tab) -> String {
        if t.url == START || t.url.is_empty() {
            return "·".into();
        }
        let host = host_of(&t.url);
        let name = host.trim_start_matches("www.");
        name.chars().find(|c| c.is_alphanumeric()).map(|c| c.to_uppercase().to_string()).unwrap_or_else(|| "·".into())
    }

    pub(super) fn draw_column(&mut self) {
        if self.column_hidden {
            return;
        }
        let (p, s, col, k) = (prefs::get(), sk(), self.column_box(), self.scale);
        let (x0, w, h) = (col.x, col.w, col.h);
        let hover = self.hover;
        if p.veins && s.frame[3] == 255 {
            self.draw_veins(col);
        }
        if p.side == Side::Right || !p.card {
            let edge = if p.side == Side::Left { x0 + w - 1.0 } else { x0 };
            self.rect(Rect::new(edge, 0.0, 1.0, h), s.hairline, 0.0);
        }
        // back, forward, reload
        let (can_back, can_fwd) = self.can_go();
        let loading = self.tab().loading;
        for (i, (hit, on, glyph)) in [(Hit::Back, can_back, '\u{E72B}'), (Hit::Fwd, can_fwd, '\u{E72A}'), (Hit::Reload, true, if loading { '\u{E711}' } else { '\u{E72C}' })].into_iter().enumerate() {
            let r = Rect::new(x0 + 10.0 + i as f32 * 30.0, 9.0, 28.0, 28.0);
            if hover == hit && on {
                self.rect(r, s.hover, 7.0);
            }
            self.icon(glyph, r.x + 14.0, r.y + 14.0, 13.0, if on { s.muted } else { s.faint });
            self.hits.push((r, hit));
        }
        let mut y = 44.0;
        if p.address {
            let r = Rect::new(x0 + 10.0, y, w - 20.0, 32.0);
            self.rect(r, if hover == Hit::Addr && !self.addr_focused { s.wash } else { s.hover }, 9.0);
            if !self.addr_focused {
                self.hits.push((r, Hit::Addr));
            }
            let url = self.tab().url.clone();
            let full = self.tab().full_mode;
            // lite or full: the engine this page runs in, and a click moves it
            let chip = if full { "full" } else { "lite" };
            let cw = self.ui_text(chip, 11.0, 600, UI_FONT, s.muted).1 + 14.0;
            let ours = url.starts_with("https://kamaitachi.local/");
            let on_page = url != START && !url.is_empty();
            let room = r.w - 20.0 - if on_page && !ours { cw + 6.0 } else { 0.0 };
            if on_page {
                let rest = url.trim_start_matches("https://");
                let (host, path) = rest.split_at(rest.find('/').unwrap_or(rest.len()));
                let host = host.trim_start_matches("www.").to_string();
                let path = if path == "/" { String::new() } else { path.to_string() };
                let hw = self.label(&host, r.x + 11.0, r.y + 8.0, 13.0, 500, s.text, room, UI_FONT);
                self.label(&path, r.x + 11.0 + hw, r.y + 8.0, 13.0, 400, s.muted, (room - hw).max(0.0), UI_FONT);
                if !ours {
                    let c = Rect::new(r.x + r.w - cw - 5.0, r.y + 6.0, cw, 20.0);
                    self.rect(c, if hover == Hit::Engine { s.wash } else { s.field }, 6.0);
                    self.label(chip, c.x + 7.0, c.y + 3.0, 11.0, 600, if full { s.accent } else { s.muted }, cw, UI_FONT);
                    self.hits.push((c, Hit::Engine));
                }
            } else {
                self.icon('\u{E721}', r.x + 18.0, r.y + 16.0, 11.0, s.faint);
                self.label("Search or type an address", r.x + 31.0, r.y + 8.0, 13.0, 400, s.faint, room - 20.0, UI_FONT);
            }
            y += 44.0;
        } else {
            y += 4.0;
        }
        // the tabs, then a row for a new one
        let rh = self.row_h();
        let size = if p.roomy { 14.0 } else { 13.0 };
        let top = y;
        let bottom = h - 46.0;
        let ctrl = self.ctrl_held && p.tab_numbers;
        let n = self.tabs.len();
        let fits = ((bottom - top) / rh).floor().max(1.0) as usize;
        self.col_scroll = self.col_scroll.min(n.saturating_sub(fits.saturating_sub(1)));
        for i in self.col_scroll..n {
            let ry = top + (i - self.col_scroll) as f32 * rh;
            if ry + rh > bottom {
                break;
            }
            let r = Rect::new(x0 + 8.0, ry, w - 16.0, rh - 2.0);
            let cur = i == self.cur;
            let over = hover == Hit::Tab(i) || hover == Hit::Close(i) || hover == Hit::Mute(i);
            if cur {
                self.rect(r, s.wash, 8.0);
            } else if over {
                self.rect(r, s.hover, 8.0);
            }
            if p.read_fill
                && let Some(part) = self.read_part(i).filter(|f| *f > 0.02)
            {
                // a grey wash, darker the further down the page you are
                let a = if s.light { 14 } else { 18 };
                self.rect(Rect::new(r.x, r.y, r.w * part, r.h), [s.text[0], s.text[1], s.text[2], a], 8.0);
            }
            let t = &self.tabs[i];
            let (asleep, full, muted) = (t.asleep || (t.page.is_none() && t.dl.is_none() && !t.full_mode && t.url != START), t.full.is_some(), t.muted);
            let title = t.title.clone();
            let letter = Self::letter(t);
            let sq = Rect::new(r.x + 8.0, r.y + (r.h - 18.0) / 2.0, 18.0, 18.0);
            self.rect(sq, if cur { s.field } else { s.hover }, 5.0);
            self.gpu.ring(sq.x * k, sq.y * k, sq.w * k, sq.h * k, 1.0, s.hairline, 5.0 * k, gpu::NO_CLIP);
            let (mark, mark_c) = if ctrl && i < 9 { ((i + 1).to_string(), s.accent) } else { (letter, if full { s.accent } else { s.muted }) };
            let mw = self.ui_text(&mark, 10.5, 600, UI_FONT, mark_c).1;
            self.label(&mark, sq.x + (18.0 - mw) / 2.0, sq.y + 2.5, 10.5, 600, mark_c, 18.0, UI_FONT);
            let dim = asleep && p.dim_sleeping && !cur;
            let color = if cur || p.contrast { s.text } else if dim { s.faint } else { s.muted };
            let end = if cur || over { 26.0 } else { 6.0 } + if muted { 20.0 } else { 0.0 };
            self.label(&title, r.x + 34.0, r.y + (r.h - size * 1.33) / 2.0, size, if cur { 500 } else { 400 }, color, r.w - 34.0 - end, UI_FONT);
            if muted {
                let m = Rect::new(r.x + r.w - end + 2.0, r.y + (r.h - 18.0) / 2.0, 18.0, 18.0);
                self.icon('\u{E74F}', m.x + 9.0, m.y + 9.0, 11.0, if hover == Hit::Mute(i) { s.text } else { s.muted });
                self.hits.push((m, Hit::Mute(i)));
            }
            self.hits.push((r, Hit::Tab(i)));
            if cur || over {
                let c = Rect::new(r.x + r.w - 23.0, r.y + (r.h - 20.0) / 2.0, 20.0, 20.0);
                if hover == Hit::Close(i) {
                    self.rect(c, s.wash, 5.0);
                }
                self.icon('\u{E711}', c.x + 10.0, c.y + 10.0, 9.5, s.muted);
                self.hits.push((c, Hit::Close(i)));
            }
        }
        let shown = (n - self.col_scroll).min(fits);
        let ny = top + shown as f32 * rh;
        if ny + rh <= bottom + rh {
            let r = Rect::new(x0 + 8.0, ny.min(bottom), w - 16.0, rh - 2.0);
            if hover == Hit::NewTab {
                self.rect(r, s.hover, 8.0);
            }
            self.icon('\u{E710}', r.x + 17.0, r.y + r.h / 2.0, 10.0, s.muted);
            self.label("New tab", r.x + 34.0, r.y + (r.h - size * 1.33) / 2.0, size, 400, s.muted, r.w - 40.0, UI_FONT);
            self.hits.push((r, Hit::NewTab));
        }
        // the foot: settings, then what this all costs
        let fy = h - 40.0;
        let gear = Rect::new(x0 + 10.0, fy, 28.0, 28.0);
        if hover == Hit::Settings {
            self.rect(gear, s.hover, 7.0);
        }
        self.icon('\u{E713}', gear.x + 14.0, gear.y + 14.0, 13.0, s.muted);
        self.hits.push((gear, Hit::Settings));
        let mut foot = format!("{:.1} MB", private_mb() + self.full_mb);
        if self.tab().blocked > 0 {
            foot.push_str(&format!("  ·  {} blocked", self.tab().blocked));
        }
        self.label(&foot, x0 + 44.0, fy + 6.0, 12.0, 400, s.muted, w - 54.0, UI_FONT);
        if p.meander {
            self.meander(x0 + 12.0, h - 9.0, w - 24.0, s.gold);
        }
    }

    fn can_go(&self) -> (bool, bool) {
        let t = self.tab();
        match &t.full {
            Some(f) if t.full_mode => full::can(&f.wv),
            _ => (!t.back.is_empty(), !t.fwd.is_empty()),
        }
    }

    /// A band of Greek key, the running meander on temple friezes, `w` wide with its
    /// foot at `y`. Each turn is a few one-pixel lines, so it costs nothing to keep.
    pub(super) fn meander(&mut self, x: f32, y: f32, w: f32, c: Rgba) {
        let u = 1.3;
        let step = 6.0 * u;
        let c = [c[0], c[1], c[2], 170];
        let line = 0.9;
        let turns = (w / step).floor() as i32;
        let x0 = x + (w - turns as f32 * step) / 2.0;
        // one turn on a grid of u: up, across, down, back in, and up a little
        let segs: [(f32, f32, f32, f32); 5] = [(0.0, 0.0, 0.0, 5.0), (0.0, 5.0, 4.0, 5.0), (4.0, 5.0, 4.0, 2.0), (4.0, 2.0, 2.0, 2.0), (2.0, 2.0, 2.0, 3.0)];
        for i in 0..turns {
            let bx = x0 + i as f32 * step;
            for (ax, ay, bx2, by) in segs {
                let (l, r) = (bx + ax.min(bx2) * u, bx + ax.max(bx2) * u);
                let (t, b) = (y - ay.max(by) * u, y - ay.min(by) * u);
                self.rect(Rect::new(l, t, (r - l).max(line), (b - t).max(line)), c, 0.0);
            }
        }
        self.rect(Rect::new(x0, y, turns as f32 * step, line), c, 0.0);
    }

    /// Faint veins through the marble of the column, drawn as soft lines.
    fn draw_veins(&mut self, col: Rect) {
        let s = sk();
        let a = if s.light { 0.16 } else { 0.22 };
        let Some(mut c) = self.gpu.canvas(self.scale) else { return };
        c.clip(col.x, col.y, col.w, col.h);
        for v in 0..2u32 {
            let start = col.x - 20.0 + v as f32 * col.w * 0.6;
            let pts: Vec<(f32, f32)> = (0..9)
                .map(|i| {
                    let t = i as f32 / 8.0;
                    let y = col.y + t * col.h;
                    let x = start + t * col.w * 0.55 + (t * 6.3 + v as f32 * 1.7).sin() * 14.0;
                    (x, y)
                })
                .collect();
            c.pencil(&pts, Pencil { width: 0.7 + v as f32 * 0.2, color: s.faint, alpha: a, wobble: 0.9 }, 300 + v);
        }
    }

    // ---- the new tab ----

    pub(super) fn draw_start(&mut self) {
        let (b, s) = (self.page_box(), sk());
        if let Some(img) = self.wallpaper() {
            let (iw, ih) = img_size(&img);
            let k = self.scale;
            let fit = (b.w * k / iw as f32).max(b.h * k / ih as f32);
            let (dw, dh) = (iw as f32 * fit, ih as f32 * fit);
            let r = if prefs::get().card { RADIUS * k } else { 0.0 };
            self.gpu.draw_image(&img, b.x * k + (b.w * k - dw) / 2.0, b.y * k + (b.h * k - dh) / 2.0, dw, dh, 1.0, r, [b.x * k, b.y * k, (b.x + b.w) * k, (b.y + b.h) * k]);
        }
        if self.addr_focused {
            return;
        }
        // the name, cut in capitals the way names are cut in stone
        let name = "KAMAITACHI";
        let face = text::CARVED;
        let size = (b.h * 0.05).clamp(26.0, 40.0);
        let spacing = size * 0.22;
        let widths: Vec<f32> = name.chars().map(|c| self.ui_text(&c.to_string(), size, 400, face, s.text).1).collect();
        let total: f32 = widths.iter().sum::<f32>() + spacing * (widths.len() - 1) as f32;
        let mid = b.x + b.w / 2.0;
        let y = b.y + b.h * 0.38;
        let mut x = mid - total / 2.0;
        for (c, w) in name.chars().zip(&widths) {
            self.label(&c.to_string(), x, y, size, 400, s.text, w + 2.0, face);
            x += w + spacing;
        }
        self.meander(mid - 66.0, y + size * 1.6, 132.0, s.gold);
        let line = format!("{:.1} MB in use", private_mb() + self.full_mb);
        let lw = self.ui_text(&line, 13.0, 400, UI_FONT, s.muted).1;
        self.label(&line, mid - lw / 2.0, y + size * 1.6 + 20.0, 13.0, 400, s.muted, lw + 4.0, UI_FONT);
        let hint = "Ctrl+L to go somewhere";
        let hw = self.ui_text(hint, 12.0, 400, UI_FONT, s.faint).1;
        self.label(hint, mid - hw / 2.0, b.y + b.h - 44.0, 12.0, 400, s.faint, hw + 4.0, UI_FONT);
    }

    // ---- the address field ----

    /// What the field offers under what was typed.
    pub(super) fn suggestions(&self) -> Vec<Suggest> {
        let q = self.edit.text();
        let q = q.trim();
        let mut v = Vec::new();
        if q.is_empty() {
            let mut recent: Vec<usize> = (0..self.tabs.len()).filter(|&i| i != self.cur).collect();
            recent.sort_by_key(|&i| self.tabs[i].used.elapsed());
            v.extend(recent.into_iter().take(6).map(Suggest::Tab));
            return v;
        }
        v.push(Suggest::Go(q.to_string()));
        v.extend(self.tabs_matching(q).into_iter().filter(|&i| i != self.cur).take(5).map(Suggest::Tab));
        v
    }

    fn field_box(&self, rows: usize) -> Rect {
        let b = self.page_box();
        let w = (b.w - 48.0).min(FIELD_W);
        Rect::new(b.x + (b.w - w) / 2.0, b.y + (b.h * 0.16).max(40.0), w, 54.0 + if rows > 0 { rows as f32 * ROW + 12.0 } else { 0.0 })
    }

    pub(super) fn draw_address(&mut self) {
        if !self.addr_focused {
            return;
        }
        let s = sk();
        let b = self.page_box();
        // the page fades back while you choose where to go; a new tab has nothing to fade
        if self.tab().url != START {
            let f = s.frame;
            self.rect(b, [f[0], f[1], f[2], 120], 0.0);
        }
        let list = self.suggestions();
        let r = self.field_box(list.len());
        let k = self.scale;
        self.rect(Rect::new(r.x - 2.0, r.y + 4.0, r.w + 4.0, r.h + 2.0), [0, 0, 0, if s.light { 22 } else { 60 }], 14.0);
        self.rect(r, s.field, 12.0);
        self.gpu.ring(r.x * k, r.y * k, r.w * k, r.h * k, 1.0, s.hairline, 12.0 * k, gpu::NO_CLIP);
        let input = Rect::new(r.x, r.y, r.w, 54.0);
        self.hits.push((r, Hit::Panel));
        self.hits.push((input, Hit::Addr));
        self.icon('\u{E721}', r.x + 24.0, r.y + 27.0, 14.0, s.muted);
        let (shown, caret, marked) = self.addr_view(r.w - 62.0);
        let tx = r.x + 46.0;
        if let Some((a, z)) = marked {
            let ac = s.accent;
            self.rect(Rect::new(tx + a, r.y + 15.0, z - a, 25.0), [ac[0], ac[1], ac[2], 60], 3.0);
        }
        if shown.is_empty() {
            self.label("Search or type an address", tx, r.y + 15.0, 17.0, 400, s.faint, r.w - 62.0, UI_FONT);
        } else {
            self.label(&shown, tx, r.y + 15.0, 17.0, 400, s.text, r.w - 62.0, UI_FONT);
        }
        if marked.is_none() {
            self.rect(Rect::new(tx + caret, r.y + 15.0, 1.6, 25.0), s.ink, 0.8);
        }
        if list.is_empty() {
            return;
        }
        self.rect(Rect::new(r.x + 14.0, r.y + 54.0, r.w - 28.0, 1.0), s.hairline, 0.0);
        let engine = prefs::SEARCH[prefs::get().search].0;
        for (i, sug) in list.iter().enumerate() {
            let rr = Rect::new(r.x + 6.0, r.y + 60.0 + i as f32 * ROW, r.w - 12.0, ROW - 2.0);
            if i == self.addr_pick {
                self.rect(rr, s.wash, 8.0);
            } else if self.hover == Hit::Suggest(i) {
                self.rect(rr, s.hover, 8.0);
            }
            self.hits.push((rr, Hit::Suggest(i)));
            let (glyph, text, side) = match sug {
                Suggest::Go(q) if crate::url::looks_like_address(q) => ('\u{E774}', q.clone(), "Go".to_string()),
                Suggest::Go(q) => ('\u{E721}', q.clone(), format!("Search {engine}")),
                Suggest::Tab(t) => ('\u{E8A5}', self.tabs[*t].title.clone(), "Switch to tab".to_string()),
            };
            self.icon(glyph, rr.x + 18.0, rr.y + rr.h / 2.0, 12.0, s.muted);
            let sw = self.ui_text(&side, 12.0, 400, UI_FONT, s.faint).1;
            self.label(&text, rr.x + 40.0, rr.y + 8.0, 14.0, 400, s.text, rr.w - 60.0 - sw, UI_FONT);
            self.label(&side, rr.x + rr.w - 12.0 - sw, rr.y + 10.0, 12.0, 400, s.faint, sw + 2.0, UI_FONT);
        }
    }

    /// Up, down and Enter while the address field is up. Returns whether it used the key.
    pub(super) fn address_key(&mut self, vk: VIRTUAL_KEY) -> bool {
        let n = self.suggestions().len();
        match vk {
            VK_DOWN if n > 0 => self.addr_pick = (self.addr_pick + 1).min(n - 1),
            VK_UP => self.addr_pick = self.addr_pick.saturating_sub(1),
            VK_RETURN => self.take_suggestion(self.addr_pick),
            VK_ESCAPE => self.addr_focused = false,
            _ => return false,
        }
        true
    }

    pub(super) fn take_suggestion(&mut self, i: usize) {
        match self.suggestions().get(i).cloned() {
            Some(Suggest::Tab(t)) => {
                self.addr_focused = false;
                self.pick_tab(t);
            }
            Some(Suggest::Go(q)) => self.navigate(&q, false),
            None => {}
        }
    }

    // ---- notes at the foot of the page ----

    pub(super) fn draw_notes(&mut self) {
        let (b, s) = (self.page_box(), sk());
        let k = self.scale;
        if let Some((msg, _)) = self.toast.clone() {
            let mw = self.ui_text(&msg, 13.0, 400, UI_FONT, s.text).1;
            let r = Rect::new(b.x + (b.w - mw - 32.0) / 2.0, b.y + b.h - 54.0, mw + 32.0, 34.0);
            self.rect(Rect::new(r.x, r.y + 2.0, r.w, r.h), [0, 0, 0, 24], 10.0);
            self.rect(r, s.field, 10.0);
            self.gpu.ring(r.x * k, r.y * k, r.w * k, r.h * k, 1.0, s.hairline, 10.0 * k, gpu::NO_CLIP);
            self.label(&msg, r.x + 16.0, r.y + 8.5, 13.0, 400, s.text, mw + 4.0, UI_FONT);
        }
        if let Some(l) = self.hover_link.clone().filter(|l| !l.starts_with(layout::PRESS)) {
            let t = l.trim_start_matches("https://").to_string();
            let lw = self.ui_text(&t, 12.0, 400, UI_FONT, s.muted).1;
            let bw = (lw + 20.0).min(b.w * 0.6);
            let r = Rect::new(b.x + 6.0, b.y + b.h - 30.0, bw, 24.0);
            self.rect(r, s.field, 7.0);
            self.gpu.ring(r.x * k, r.y * k, r.w * k, r.h * k, 1.0, s.hairline, 7.0 * k, gpu::NO_CLIP);
            self.label(&t, r.x + 10.0, r.y + 5.0, 12.0, 400, s.muted, bw - 20.0, UI_FONT);
        }
    }
}
