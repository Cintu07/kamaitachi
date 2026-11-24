//! The joseon skin: the chrome and the new tab as a sheet of paper on a desk,
//! drawn in graphite and cobalt ink with a little sky blue watercolor. It needs the
//! cpu renderer, because the brush draws into the frame's pixels; on Direct3D the
//! flat chrome in mod.rs is used instead.
//!
//! Every stroke has a fixed seed, so a line wobbles the same way on every frame and
//! nothing shimmers while you move the mouse.

use super::brush::{self, Canvas, Pencil, Pt};
use super::play::{self, STAMPS};
use super::skin::Skin;
use super::text::{SERIF, TYPEWRITER};
use super::*;

/// one more color than the skin has: the red of a seal, used sparingly
const RUST: Rgba = [0xc4, 0x4d, 0x2f, 255];

fn graphite(s: &Skin) -> Pencil {
    Pencil { width: 1.15, color: s.muted, alpha: 0.9, wobble: 0.45 }
}

fn ink(s: &Skin) -> Pencil {
    Pencil { width: 1.35, color: s.ink, alpha: 0.95, wobble: 0.5 }
}

fn arrow(c: &mut Canvas, cx: f32, cy: f32, dir: f32, p: Pencil, seed: u32) {
    c.pencil(&[(cx - dir * 7.0, cy), (cx + dir * 7.0, cy + 0.3)], p, seed);
    c.pencil(&[(cx + dir * 2.5, cy - 4.8), (cx + dir * 7.2, cy + 0.2), (cx + dir * 2.3, cy + 5.0)], p, seed + 1);
}

fn cross(c: &mut Canvas, cx: f32, cy: f32, r: f32, p: Pencil, seed: u32) {
    c.pencil(&[(cx - r, cy - r), (cx + r, cy + r)], p, seed);
    c.pencil(&[(cx + r, cy - r * 1.05), (cx - r * 0.95, cy + r)], p, seed + 1);
}

fn plus(c: &mut Canvas, cx: f32, cy: f32, r: f32, p: Pencil, seed: u32) {
    c.pencil(&[(cx - r, cy + 0.2), (cx + r, cy - 0.2)], p, seed);
    c.pencil(&[(cx + 0.2, cy - r), (cx - 0.2, cy + r)], p, seed + 1);
}

/// A reload mark: most of a circle, with the arrowhead where it ends.
fn again(c: &mut Canvas, cx: f32, cy: f32, p: Pencil, seed: u32) {
    let r = 6.0;
    let pts: Vec<Pt> = (0..=26)
        .map(|i| {
            let a = -1.2 + i as f32 / 26.0 * 5.0;
            (cx + r * a.cos(), cy + r * a.sin())
        })
        .collect();
    c.pencil(&pts, p, seed);
    // the arrowhead points on round the way the line was going
    let end = -1.2 + 5.0f32;
    let (ex, ey) = (cx + r * end.cos(), cy + r * end.sin());
    let (tx, ty) = (-end.sin(), end.cos());
    let (nx, ny) = (end.cos(), end.sin());
    c.pencil(&[(ex - tx * 4.0 + nx * 3.5, ey - ty * 4.0 + ny * 3.5), (ex + tx * 0.8, ey + ty * 0.8), (ex - tx * 4.0 - nx * 3.5, ey - ty * 4.0 - ny * 3.5)], p, seed + 1);
}

/// The weasel, facing right mid-leap, as a shape to stamp. About 38 by 16.
const WEASEL: [Pt; 27] = [
    (0.0, 9.0),
    (3.0, 6.5),
    (7.0, 5.5),
    (11.0, 6.5),
    (15.0, 5.0),
    (20.0, 4.5),
    (25.0, 5.2),
    (28.5, 4.0),
    (30.0, 1.0),
    (31.5, 3.0),
    (34.5, 3.6),
    (38.5, 6.2),
    (35.0, 7.8),
    (31.5, 9.0),
    (29.0, 11.0),
    (31.5, 14.5),
    (30.0, 15.5),
    (27.0, 12.5),
    (22.0, 11.5),
    (17.0, 11.8),
    (14.5, 15.5),
    (12.5, 15.8),
    (12.8, 12.0),
    (11.0, 9.2),
    (6.5, 8.5),
    (3.0, 9.6),
    (0.5, 9.4),
];

/// The spirit at (x, y): an ink stamp of the weasel, with whatever it has grown.
fn spirit(c: &mut Canvas, x: f32, y: f32, form: usize, eating: bool, s: &Skin) {
    let mut body: Vec<Pt> = WEASEL.iter().map(|&(px, py)| (x + px, y + py)).collect();
    if eating {
        // the jaw drops open
        body[12] = (x + 37.0, y + 10.2);
    }
    // extra tails sweep up behind as it grows
    let tails = [0, 1, 1, 2, 3][form];
    for t in 0..tails {
        let lift = 3.0 + t as f32 * 3.2;
        let sweep = [(x + 10.0, y + 6.0), (x + 5.0, y + 5.5 - lift * 0.5), (x + 1.0, y + 6.5 - lift), (x - 3.0, y + 4.0 - lift)];
        c.pencil(&sweep, Pencil { width: 2.2, ..ink(s) }, 400 + t as u32);
    }
    c.stamp(&body, s.ink, 0.95, 77);
    c.stamp(&brush::ellipse(x + 33.2, y + 4.9, 0.9, 0.9, 8), s.panel, 1.0, 78);
    if form >= 1 {
        // the blade on its tail, in sky blue
        let arc: Vec<Pt> = (0..=10)
            .map(|i| {
                let a = 2.2 + i as f32 / 10.0 * 1.9;
                (x + 1.0 + 6.0 * a.cos(), y + 12.0 + 6.0 * a.sin())
            })
            .collect();
        c.pencil(&arc, Pencil { width: 1.8, color: s.full, alpha: 0.9, wobble: 0.2 }, 90);
    }
    if form >= 3 {
        let sp = brush::spiral(x + 19.0, y + 8.0, 6.0, 21.0, 1.3, 0.6);
        c.pencil(&sp, Pencil { width: 0.9, alpha: 0.35, ..graphite(s) }, 91);
    }
    if eating {
        c.stamp(&brush::ellipse(x + 40.0, y + 9.0, 1.2, 1.2, 8), RUST, 1.0, 92);
    }
}

/// A crescent moon: the circle at (cx, cy) with another one bitten out of its left.
fn crescent(cx: f32, cy: f32, r: f32) -> Vec<Pt> {
    let (d, ri) = (r * 0.5, r * 0.88);
    let a = ((ri * ri - r * r - d * d) / (2.0 * r * d)).clamp(-1.0, 1.0).acos();
    let b = ((d * d + ri * ri - r * r) / (2.0 * d * ri)).clamp(-1.0, 1.0).acos();
    let mut pts = Vec::new();
    for i in 0..=24 {
        let t = -a + 2.0 * a * i as f32 / 24.0;
        pts.push((cx + r * t.cos(), cy + r * t.sin()));
    }
    for i in 0..=24 {
        let t = b - 2.0 * b * i as f32 / 24.0;
        pts.push((cx - d + ri * t.cos(), cy + ri * t.sin()));
    }
    pts
}

/// The mark for one stamp in the book, as cobalt ink.
fn stamp_mark(c: &mut Canvas, id: &str, cx: f32, cy: f32, r: f32, col: Rgba, seed: u32) {
    let inkp = Pencil { width: 1.6, color: col, alpha: 0.95, wobble: 0.3 };
    match id {
        "first-cut" => c.stamp(&brush::star(cx, cy, r, 0.22, 4, 0.0), col, 0.95, seed),
        "hundred" => c.stamp(&brush::star(cx, cy, r, 0.42, 5, 0.0), col, 0.95, seed),
        "thousand" => c.stamp(&brush::star(cx, cy, r, 0.3, 8, 0.2), col, 0.95, seed),
        "whirlwind" => c.pencil(&brush::spiral(cx, cy, 1.0, r, 2.4, 0.0), Pencil { width: 2.4, ..inkp }, seed),
        "ward" => {
            c.pencil(&brush::loop_around(cx, cy, r, r, 0.4), Pencil { width: 1.4, ..inkp }, seed);
            c.stamp(&brush::star(cx, cy, r * 0.55, 0.42, 5, 0.0), col, 0.95, seed + 1);
        }
        "reader" => c.stamp(&crescent(cx, cy, r * 0.85), col, 0.95, seed),
        "both-blades" => {
            c.stamp(&brush::star(cx - r * 0.3, cy + r * 0.15, r * 0.75, 0.2, 4, 0.0), col, 0.95, seed);
            c.stamp(&brush::star(cx + r * 0.45, cy - r * 0.35, r * 0.5, 0.2, 4, 0.4), col, 0.95, seed + 1);
        }
        "night-owl" => {
            c.stamp(&crescent(cx - r * 0.1, cy, r * 0.8), col, 0.95, seed);
            c.stamp(&brush::star(cx + r * 0.55, cy - r * 0.5, r * 0.3, 0.42, 5, 0.0), col, 0.95, seed + 1);
        }
        "regular" => {
            c.stamp(&brush::ellipse(cx, cy, r * 0.48, r * 0.48, 24), col, 0.95, seed);
            c.stamp(&brush::star(cx, cy, r, 0.62, 12, 0.13), col, 0.55, seed + 1);
        }
        _ => c.stamp(&brush::star(cx, cy, r, 0.14, 6, 0.0), col, 0.95, seed),
    }
}

/// A leaf, `len` long, pointing at `angle` radians.
fn leaf(cx: f32, cy: f32, len: f32, angle: f32) -> Vec<Pt> {
    let (s, c) = angle.sin_cos();
    let mut pts = Vec::new();
    for side in [1.0f32, -1.0] {
        for i in 0..=12 {
            let t = if side > 0.0 { i as f32 / 12.0 } else { 1.0 - i as f32 / 12.0 };
            let (lx, ly) = (len * (t - 0.5), side * len * 0.27 * (std::f32::consts::PI * t).sin().powf(0.8));
            pts.push((cx + lx * c - ly * s, cy + lx * s + ly * c));
        }
    }
    pts
}

impl App {
    /// Lays out one line of chrome text in any family and style.
    fn words_layout(&mut self, s: &str, size: f32, weight: u16, italic: bool, family: &str, color: Rgba) -> (DisplayList, f32) {
        let txt: Vec<u16> = s.encode_utf16().collect();
        let len = txt.len() as u32;
        let p = Para { text: txt, runs: vec![Run { start: 0, len, font: Font { family: Arc::from(family), size, weight, italic }, color, deco: 0, link: u32::MAX }], nowrap: true, ..Default::default() };
        let mut dl = DisplayList::new();
        let po = self.dw.layout(&p, 1.0e5, 0.0, 0.0, Some(&mut dl));
        (dl, po.width)
    }

    /// Where the stamp book sits on the new tab.
    fn book(&self) -> Rect {
        let (w, h) = (self.w as f32 / self.scale, self.h as f32 / self.scale);
        let bw = 58.0 * STAMPS.len() as f32;
        Rect::new(w / 2.0 - bw / 2.0, h - 118.0, bw, 58.0)
    }

    fn measure(&mut self, s: &str, size: f32, italic: bool, family: &str) -> f32 {
        self.words_layout(s, size, 400, italic, family, [0, 0, 0, 255]).1
    }

    /// Draws a line of text with its top-left at (x, y), cut off at `max` wide.
    #[allow(clippy::too_many_arguments)]
    fn words(&mut self, s: &str, x: f32, y: f32, size: f32, italic: bool, family: &str, color: Rgba, max: f32) -> f32 {
        let (dl, w) = self.words_layout(s, size, 400, italic, family, color);
        let k = self.scale;
        let clip = [x * k, -1.0e9, (x + max) * k, 1.0e9];
        for it in &dl.items {
            if let Item::Text { start, len, color, .. } = *it {
                let c = gpu::rgba(color, 1.0);
                for g in &dl.glyphs[start as usize..(start + len) as usize] {
                    self.gpu.glyph(g.face, g.size * k, g.id, (x + g.x) * k, (y + g.y) * k, c, clip);
                }
            }
        }
        w
    }

    pub(super) fn draw_chrome_drawn(&mut self) {
        let k = self.scale;
        let (w, h) = (self.w as f32 / k, self.h as f32 / k);
        let s = sk();
        self.hits.clear();
        let n = self.tabs.len();
        let x0 = 44.0;
        let tw = ((w - x0 - 44.0 - 72.0) / n as f32).clamp(70.0, 236.0);
        let cur = self.cur;
        let hover = self.hover;
        let gp = graphite(&s);
        let faint = Pencil { alpha: 0.45, ..gp };
        // icons get a softer lead pressed harder, so they read at a glance
        let icon = Pencil { width: 1.6, color: s.muted, alpha: 1.0, wobble: 0.35 };
        let (can_back, can_fwd, loading, full_mode, blocked) = {
            let t = self.tab();
            (
                !t.back.is_empty() || t.full.as_ref().is_some_and(|f| full::can(&f.wv).0),
                !t.fwd.is_empty() || t.full.as_ref().is_some_and(|f| full::can(&f.wv).1),
                t.loading,
                t.full_mode,
                t.blocked,
            )
        };
        let on_page = !matches!(self.tab().url.as_str(), START | "");
        let tabs: Vec<(String, bool, bool, bool)> = self.tabs.iter().map(|t| (t.title.clone(), t.loading, t.parked, t.full_mode)).collect();

        // the right end of the toolbar, laid out from the edge in
        let budget: f32 = std::env::var("HAYATE_BUDGET_MB").ok().and_then(|v| v.parse().ok()).unwrap_or(500.0);
        let mine = private_mb();
        let total = mine + self.full_mb;
        let mem = if self.full_mb > 0.5 { format!("{:.0} + {:.0} MB", mine, self.full_mb) } else { format!("{mine:.1} MB") };
        let mem_w = self.measure(&mem, 13.0, false, TYPEWRITER);
        let meter = Rect::new(w - 14.0 - 44.0, TAB_H + 19.0, 44.0, 9.0);
        let mem_x = meter.x - 10.0 - mem_w;
        let mut rx = mem_x;
        let cut = (on_page && blocked > 0).then(|| format!("{blocked} cut"));
        let mut cut_x = 0.0;
        if let Some(t) = &cut {
            let cw = self.measure(t, 13.0, false, TYPEWRITER);
            rx -= cw + 18.0;
            cut_x = rx;
        }
        let sw = Rect::new(rx - 18.0 - 92.0, TAB_H + 9.0, 92.0, 28.0);
        if on_page {
            rx = sw.x;
        }
        let ar = Rect::new(122.0, TAB_H + 7.0, (rx - 122.0 - 18.0).max(80.0), 32.0);
        let focused = self.edit.focused;
        let (form, _, _) = play::form(self.stats.blocked);
        let eating = self.ate.is_some_and(|t| (t.elapsed().as_millis() / 90) % 2 == 0);
        let spirit_at = (w - 56.0, 11.0);
        let spirit_r = Rect::new(spirit_at.0 - 8.0, 4.0, 54.0, TAB_H - 6.0);
        let nr = Rect::new(x0 + n as f32 * tw + 2.0, 10.0, 24.0, 24.0);

        // ---- paper and pencil
        if let Some(mut c) = self.gpu.canvas(k) {
            c.paper(0.0, 0.0, w, TAB_H, s.strip, 0.06);
            c.paper(0.0, TAB_H, w, BAR_H, s.panel, 0.04);
            // the brand: one cobalt sparkle, like a blade catching light
            c.stamp(&brush::star(23.0, 21.0, 11.0, 0.2, 4, 0.0), s.ink, 0.95, 5);
            c.stamp(&brush::star(32.5, 12.0, 3.2, 0.25, 4, 0.3), s.ink, 0.9, 6);
            // the active tab is a sheet laid on the desk, open into the toolbar below
            let ax0 = x0 + cur as f32 * tw;
            let ax1 = ax0 + tw - 6.0;
            c.pencil(&[(-3.0, TAB_H + 0.5), (ax0, TAB_H + 0.5)], faint, 11);
            c.pencil(&[(ax1, TAB_H + 0.5), (w + 3.0, TAB_H + 0.5)], faint, 12);
            c.paper(ax0 + 0.5, 7.0, tw - 7.0, TAB_H - 6.0, s.panel, 0.04);
            c.pencil(&[(ax0, TAB_H + 1.0), (ax0 - 0.3, 5.0)], gp, 13);
            c.pencil(&[(ax0 - 3.0, 7.0), (ax1 + 2.0, 6.6)], gp, 14);
            c.pencil(&[(ax1, 4.5), (ax1 + 0.3, TAB_H + 1.0)], gp, 15);
            for (i, (_, loading, parked, full)) in tabs.iter().enumerate() {
                let x = x0 + i as f32 * tw;
                let cy = 23.0;
                // a dot for each tab: inked when it is the one you are on
                let dot = brush::ellipse(x + 13.0, cy, 2.7, 2.7, 12);
                if *loading {
                    c.stamp(&dot, s.accent, 1.0, 20 + i as u32);
                } else if *parked {
                    c.pencil(&brush::loop_around(x + 13.0, cy, 2.6, 2.6, 1.0), faint, 20 + i as u32);
                } else if i == cur {
                    c.stamp(&dot, if *full { s.full } else { s.ink }, 1.0, 20 + i as u32);
                } else {
                    c.pencil(&brush::loop_around(x + 13.0, cy, 2.6, 2.6, 1.0), Pencil { color: if *full { s.full } else { s.muted }, ..faint }, 20 + i as u32);
                }
                if i != cur && i + 1 != cur && i + 1 < n {
                    c.pencil(&[(x + tw - 3.0, 15.0), (x + tw - 3.2, 31.0)], faint, 40 + i as u32);
                }
                let hov = hover == Hit::Tab(i) || hover == Hit::Close(i);
                if (i == cur || hov) && tw > 96.0 {
                    let cx = x + tw - 6.0 - 16.0;
                    if hover == Hit::Close(i) {
                        c.stamp(&brush::ellipse(cx, cy, 9.0, 9.0, 20), s.hover, 1.0, 60);
                    }
                    cross(&mut c, cx, cy, 3.8, Pencil { width: 1.3, ..icon }, 70 + i as u32);
                }
            }
            if hover == Hit::NewTab {
                c.stamp(&brush::ellipse(nr.x + 12.0, nr.y + 12.0, 11.0, 11.0, 20), s.hover, 1.0, 80);
            }
            plus(&mut c, nr.x + 12.0, nr.y + 12.0, 6.0, icon, 81);
            // back, forward, reload
            for (i, (hit, on)) in [(Hit::Back, can_back), (Hit::Fwd, can_fwd), (Hit::Reload, true)].into_iter().enumerate() {
                let (cx, cy) = (25.0 + i as f32 * 34.0, TAB_H + 23.0);
                if hover == hit && on {
                    c.stamp(&brush::ellipse(cx, cy, 14.0, 13.0, 24), s.hover, 1.0, 100 + i as u32);
                }
                let p = if !on { Pencil { alpha: 0.4, ..icon } } else if hover == hit { Pencil { color: s.text, ..icon } } else { icon };
                match hit {
                    Hit::Back => arrow(&mut c, cx, cy, -1.0, p, 110),
                    Hit::Fwd => arrow(&mut c, cx, cy, 1.0, p, 120),
                    _ if loading => cross(&mut c, cx, cy, 5.0, p, 130),
                    _ => again(&mut c, cx, cy, p, 140),
                }
            }
            // the address: a ruled line to write on, inked when you are writing
            let base = ar.y + ar.h - 3.0;
            if focused {
                let selected = self.edit.all && !self.edit.text.is_empty();
                if selected {
                    c.wash(&[(ar.x + 4.0, ar.y + 3.0), (ar.x + ar.w * 0.6, ar.y + 2.0), (ar.x + ar.w * 0.6, ar.y + ar.h - 5.0), (ar.x + 4.0, ar.y + ar.h - 4.0)], s.accent, 0.5, 150);
                }
                c.sketch(&[(ar.x, base), (ar.x + ar.w, base - 0.4)], ink(&s), 151);
            } else {
                let p = if hover == Hit::Addr { gp } else { Pencil { alpha: 0.7, ..gp } };
                c.pencil(&[(ar.x, base), (ar.x + ar.w, base - 0.4)], p, 152);
            }
            // lite or full: the one in use is circled, like an answer on a form
            if on_page {
                let (lx, fx) = (sw.x + 24.0, sw.x + 68.0);
                let (on, off) = if full_mode { (fx, lx) } else { (lx, fx) };
                c.pencil(&brush::loop_around(on, sw.y + 14.0, 23.0, 12.0, 2.6), ink(&s), 160);
                if hover == Hit::Engine {
                    c.pencil(&brush::loop_around(off, sw.y + 14.0, 22.0, 11.0, 2.4), faint, 161);
                }
            }
            // memory: a small box filled with hatching as the budget is used up
            let used = (total / budget).clamp(0.02, 1.0);
            let hot = used > 0.8;
            c.sketch_box(meter.x, meter.y, meter.w, meter.h, Pencil { width: 1.0, ..gp }, 170);
            let fill = [(meter.x, meter.y), (meter.x + meter.w * used, meter.y), (meter.x + meter.w * used, meter.y + meter.h), (meter.x, meter.y + meter.h)];
            let hp = Pencil { width: 1.1, color: if hot { RUST } else { s.ink }, alpha: 0.85, wobble: 0.2 };
            c.hatch(&fill, 60.0, 2.6, hp, 171);
            // the toolbar's lower edge, and a loading line drawn along it
            c.pencil(&[(-3.0, TOP - 1.0), (w + 3.0, TOP - 1.4)], Pencil { alpha: 0.6, ..gp }, 180);
            if loading {
                let prog = if self.tabs[cur].dl.is_some() || self.tabs[cur].full.is_some() { 0.72 } else { 0.28 };
                c.pencil(&[(0.0, TOP - 1.2), (w * prog, TOP - 1.5)], Pencil { width: 2.0, ..ink(&s) }, 181);
            }
            if hover == Hit::Spirit {
                c.stamp(&brush::ellipse(spirit_r.x + 27.0, 21.0, 30.0, 15.0, 28), s.hover, 1.0, 190);
            }
            spirit(&mut c, spirit_at.0, spirit_at.1, form, eating, &s);
        }

        // ---- words
        for (i, (title, _, _, _)) in tabs.iter().enumerate() {
            let x = x0 + i as f32 * tw;
            let r = Rect::new(x, 7.0, tw - 6.0, TAB_H - 7.0);
            let active = i == cur;
            let hov = hover == Hit::Tab(i) || hover == Hit::Close(i);
            self.hits.push((r, Hit::Tab(i)));
            self.words(&format!("{:02}", i + 1), x + 21.0, 15.5, 10.5, false, TYPEWRITER, s.faint, 20.0);
            let show_close = (active || hov) && tw > 96.0;
            let tmax = (r.w - 44.0 - if show_close { 24.0 } else { 0.0 }).max(0.0);
            self.words(title, x + 40.0, 14.0, 13.0, false, TYPEWRITER, if active { s.text } else { s.muted }, tmax);
            if show_close {
                self.hits.push((Rect::new(x + tw - 6.0 - 26.0, 13.0, 20.0, 20.0), Hit::Close(i)));
            }
        }
        self.hits.push((spirit_r, Hit::Spirit));
        if !on_page {
            // on the new tab, the stamp book opens the den too
            let b = self.book();
            self.hits.push((b, Hit::Spirit));
        }
        self.hits.push((nr, Hit::NewTab));
        for (i, hit) in [Hit::Back, Hit::Fwd, Hit::Reload].into_iter().enumerate() {
            self.hits.push((Rect::new(10.0 + i as f32 * 34.0, TAB_H + 8.0, 30.0, 30.0), hit));
        }
        self.hits.push((ar, Hit::Addr));
        let ty = ar.y + 6.0;
        if focused {
            let text: String = self.edit.text.iter().collect();
            let before: String = self.edit.text[..self.edit.caret].iter().collect();
            if text.is_empty() {
                self.words("search, or type an address", ar.x + 2.0, ty + 1.0, 14.0, false, TYPEWRITER, s.faint, ar.w - 8.0);
            } else {
                self.words(&text, ar.x + 2.0, ty, 15.0, false, TYPEWRITER, s.text, ar.w - 8.0);
            }
            if !self.edit.all {
                let bw = self.measure(&before, 15.0, false, TYPEWRITER);
                self.rect(Rect::new(ar.x + 2.5 + bw, ar.y + 5.0, 1.6, 20.0), s.ink, 0.8);
            }
        } else {
            let url = self.tab().url.clone();
            if url == START || url.is_empty() {
                self.words("search, or type an address", ar.x + 2.0, ty + 1.0, 14.0, false, TYPEWRITER, s.faint, ar.w - 8.0);
            } else {
                let rest = url.trim_start_matches("https://");
                let (host, path) = match rest.find('/') {
                    Some(p) => (&rest[..p], &rest[p..]),
                    None => (rest, ""),
                };
                let host = host.trim_start_matches("www.").to_string();
                let path = if path == "/" { String::new() } else { path.to_string() };
                let hw = self.words(&host, ar.x + 2.0, ty, 15.0, false, TYPEWRITER, s.ink, ar.w - 8.0);
                self.words(&path, ar.x + 2.0 + hw, ty, 15.0, false, TYPEWRITER, s.muted, (ar.w - 8.0 - hw).max(0.0));
            }
        }
        if on_page {
            let (lc, fc) = if full_mode { (s.faint, s.text) } else { (s.text, s.faint) };
            self.words("lite", sw.x + 9.0, sw.y + 6.0, 13.0, false, TYPEWRITER, lc, 40.0);
            self.words("full", sw.x + 53.0, sw.y + 6.0, 13.0, false, TYPEWRITER, fc, 40.0);
            self.hits.push((sw, Hit::Engine));
        }
        if let Some(t) = &cut {
            self.words(t, cut_x, TAB_H + 15.0, 13.0, false, TYPEWRITER, s.ink, 200.0);
        }
        self.words(&mem, mem_x, TAB_H + 15.0, 13.0, false, TYPEWRITER, s.text, mem_w + 4.0);

        // ---- slips of paper on top of everything
        if let Some((msg, _)) = self.toast.clone() {
            let mw = self.measure(&msg, 13.0, false, TYPEWRITER);
            let r = Rect::new((w - mw - 56.0) / 2.0, h - 58.0, mw + 56.0, 36.0);
            if let Some(mut c) = self.gpu.canvas(k) {
                c.paper(r.x, r.y, r.w, r.h, s.field, 0.04);
                c.sketch_box(r.x, r.y, r.w, r.h, Pencil { width: 1.0, ..gp }, 200);
                c.stamp(&brush::star(r.x + 20.0, r.y + 18.0, 9.0, 0.42, 5, 0.0), s.ink, 0.95, 201);
            }
            self.words(&msg, r.x + 38.0, r.y + 10.0, 13.0, false, TYPEWRITER, s.text, mw + 4.0);
        }
        let tip = if hover == Hit::Spirit {
            let (_, name, next) = play::form(self.stats.blocked);
            Some(match next {
                Some(nx) => format!("{name} · {} cut · grows at {nx}", self.stats.blocked),
                None => format!("{name} · {} cut", self.stats.blocked),
            })
        } else {
            None
        };
        if let Some(t) = tip {
            let lw = self.measure(&t, 12.0, false, TYPEWRITER);
            let r = Rect::new(w - lw - 32.0, TAB_H + 50.0, lw + 22.0, 26.0);
            self.slip(r);
            self.words(&t, r.x + 11.0, r.y + 6.0, 12.0, false, TYPEWRITER, s.ink, lw + 2.0);
        }
        if let Some(l) = self.hover_link.clone() {
            let t = l.trim_start_matches("https://").to_string();
            let lw = self.measure(&t, 12.0, false, TYPEWRITER);
            let bw = (lw + 22.0).min(w * 0.6);
            let r = Rect::new(8.0, h - 33.0, bw, 25.0);
            self.slip(r);
            self.words(&t, r.x + 11.0, r.y + 5.5, 12.0, false, TYPEWRITER, s.ink, bw - 22.0);
        }
    }

    /// A small piece of paper with a pencil edge.
    fn slip(&mut self, r: Rect) {
        let s = sk();
        if let Some(mut c) = self.gpu.canvas(self.scale) {
            c.paper(r.x, r.y, r.w, r.h, s.field, 0.04);
            c.sketch_box(r.x, r.y, r.w, r.h, Pencil { width: 0.9, ..graphite(&s) }, (r.w as u32) ^ 0x51);
        } else {
            self.rect(r, s.field, 4.0);
        }
    }

    /// The new tab: a sheet of paper with a sky blue wash, the name in serif italic,
    /// a line of what kamaitachi has done, and the stamp book along the bottom.
    pub(super) fn draw_sheet(&mut self) {
        let k = self.scale;
        let (w, h) = (self.w as f32 / k, self.h as f32 / k);
        let s = sk();
        let hh = h - TOP;
        let mid = w / 2.0;
        let title_y = TOP + hh * 0.26;
        let title = "kamaitachi";
        let tsize = (hh * 0.11).clamp(44.0, 84.0);
        let title_w = self.measure(title, tsize, true, SERIF);
        let line = format!(
            "{:.1} MB in use  ·  {} ads and trackers cut  ·  day {}",
            private_mb() + self.full_mb,
            self.stats.blocked,
            self.stats.days.max(1)
        );
        let line_w = self.measure(&line, 13.0, false, TYPEWRITER);
        let sub = "a browser that stays light";
        let sub_w = self.measure(sub, 15.0, false, TYPEWRITER);
        let book = self.book();
        let (book_x, book_y, book_w) = (book.x, book.y, book.w);
        let cell = 58.0;
        let month = {
            let mut st = windows_sys::Win32::Foundation::SYSTEMTIME { wYear: 0, wMonth: 0, wDayOfWeek: 0, wDay: 0, wHour: 0, wMinute: 0, wSecond: 0, wMilliseconds: 0 };
            unsafe { windows_sys::Win32::System::SystemInformation::GetLocalTime(&mut st) };
            st.wMonth
        };
        let earned: Vec<bool> = STAMPS.iter().map(|st| self.stats.has(st.id)).collect();
        let gp = graphite(&s);
        let Some(mut c) = self.gpu.canvas(k) else { return };
        c.clip(0.0, TOP, w, hh);
        c.paper(0.0, TOP, w, hh, [0xf7, 0xf3, 0xe9, 255], 0.05);
        // the sky: a big wash up and to the right, a faint one low on the left
        c.wash(&brush::ellipse(w * 0.74, TOP + hh * 0.2, w * 0.3, hh * 0.2, 18), s.accent, 0.6, 2024);
        c.wash(&brush::ellipse(w * 0.14, TOP + hh * 0.82, w * 0.16, hh * 0.1, 14), s.accent, 0.28, 2025);
        // a whirlwind for the weasel to ride, pencilled beside the name
        let sx = mid - title_w / 2.0 - tsize * 0.75;
        c.pencil(&brush::spiral(sx, title_y + tsize * 0.55, 3.0, tsize * 0.42, 2.2, 0.8), Pencil { width: 1.2, alpha: 0.7, ..gp }, 31);
        // stars, as if stamped by hand
        let stars: [(f32, f32, f32, usize, f32, bool); 7] = [
            (0.12, 0.16, 13.0, 4, 0.2, false),
            (0.2, 0.42, 7.0, 5, 0.42, false),
            (0.86, 0.5, 9.0, 4, 0.22, false),
            (0.64, 0.1, 6.0, 8, 0.32, false),
            (0.92, 0.18, 16.0, 4, 0.18, false),
            (0.07, 0.6, 5.0, 4, 0.25, true),
            (0.78, 0.64, 6.5, 5, 0.42, false),
        ];
        for (i, (fx, fy, r, pts, inner, red)) in stars.into_iter().enumerate() {
            c.stamp(&brush::star(w * fx, TOP + hh * fy, r, inner, pts, i as f32 * 0.3), if red { RUST } else { s.ink }, 0.92, 300 + i as u32);
        }
        // a pencil stroke under the name
        let uy = title_y + tsize * 1.18;
        c.sketch(&[(mid - title_w * 0.42, uy + 2.0), (mid, uy), (mid + title_w * 0.44, uy + 1.5)], Pencil { width: 1.3, alpha: 0.75, ..gp }, 32);
        // the stamp book: earned ones in ink, the rest waiting as faint rings
        for (i, st) in STAMPS.iter().enumerate() {
            let cx = book_x + cell * (i as f32 + 0.5);
            let cy = book_y + 20.0;
            if earned[i] {
                stamp_mark(&mut c, st.id, cx, cy, 15.0, s.ink, 500 + i as u32 * 7);
            } else {
                c.pencil(&brush::loop_around(cx, cy, 14.0, 14.0, i as f32), Pencil { alpha: 0.35, ..gp }, 600 + i as u32);
            }
        }
        // something for the time of year, drawn in pencil
        for i in 0..9u32 {
            let fx = [0.05, 0.27, 0.36, 0.58, 0.69, 0.83, 0.95, 0.44, 0.16][i as usize];
            let fy = [0.3, 0.68, 0.9, 0.78, 0.92, 0.84, 0.72, 0.6, 0.94][i as usize];
            let (x, y) = (w * fx, TOP + hh * fy);
            if (y - book_y - 20.0).abs() < 34.0 && (x - mid).abs() < book_w / 2.0 + 20.0 {
                continue;
            }
            let turn = i as f32 * 1.37;
            match month {
                9..=11 => {
                    let l = leaf(x, y, 22.0 + (i % 3) as f32 * 6.0, turn);
                    let col = if i % 4 == 1 { RUST } else { s.muted };
                    c.hatch(&l, 35.0 + i as f32 * 20.0, 2.6, Pencil { width: 0.8, color: col, alpha: 0.6, wobble: 0.2 }, 700 + i);
                    c.pencil(&{ let mut p = l.clone(); p.push(l[0]); p }, Pencil { color: col, alpha: 0.85, ..gp }, 720 + i);
                    let (sn, cs) = turn.sin_cos();
                    let half = (22.0 + (i % 3) as f32 * 6.0) * 0.5;
                    c.pencil(&[(x - cs * half * 1.2, y - sn * half * 1.2), (x + cs * half, y + sn * half)], Pencil { color: col, alpha: 0.7, ..gp }, 740 + i);
                }
                12 | 1 | 2 => {
                    for j in 0..3 {
                        let a = turn + j as f32 * std::f32::consts::PI / 3.0;
                        let (sn, cs) = a.sin_cos();
                        c.pencil(&[(x - cs * 6.0, y - sn * 6.0), (x + cs * 6.0, y + sn * 6.0)], Pencil { color: s.ink_dim, ..gp }, 760 + i * 3 + j);
                    }
                }
                3..=5 => {
                    for j in 0..5 {
                        let a = turn + j as f32 * std::f32::consts::TAU / 5.0;
                        c.pencil(&brush::loop_around(x + a.cos() * 5.0, y + a.sin() * 5.0, 4.0, 4.0, a), Pencil { color: RUST, alpha: 0.55, ..gp }, 800 + i * 5 + j);
                    }
                }
                _ => {
                    c.stamp(&brush::ellipse(x, y, 2.0, 2.0, 10), s.ink, 0.9, 850 + i);
                    c.pencil(&brush::loop_around(x, y, 6.0, 6.0, turn), Pencil { alpha: 0.35, ..gp }, 860 + i);
                }
            }
        }
        self.words(title, mid - title_w / 2.0, title_y, tsize, true, SERIF, s.ink, title_w + 8.0);
        self.words(sub, mid - sub_w / 2.0, uy + 16.0, 15.0, false, TYPEWRITER, s.muted, sub_w + 4.0);
        self.words(&line, mid - line_w / 2.0, uy + 44.0, 13.0, false, TYPEWRITER, s.text, line_w + 4.0);
        let note = "stamps  ·  click the weasel for the den";
        let note_w = self.measure(note, 11.5, false, TYPEWRITER);
        self.words(note, mid - note_w / 2.0, book_y + 46.0, 11.5, false, TYPEWRITER, s.faint, note_w + 4.0);
    }
}
