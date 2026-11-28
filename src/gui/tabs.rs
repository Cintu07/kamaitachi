//! Tabs that outlast the window: last session's tabs come back asleep, costing a line of
//! text each until you look at one; a closed tab can be opened again with its history;
//! and Ctrl+K finds a tab by a few letters of its name or address.

use super::*;

/// %APPDATA%\hayate\session.txt: the tab in front, then one tab a line.
fn session_file() -> std::path::PathBuf {
    wall_dir().join("session.txt")
}

/// A tab that was closed, kept so Ctrl+Shift+T can bring it back where it was.
pub(super) struct Closed {
    url: String,
    scroll: f32,
    back: Vec<(String, f32)>,
    fwd: Vec<(String, f32)>,
    at: usize,
}

const CLOSED: usize = 25;

/// The tab finder.
pub(super) struct Finder {
    pub edit: Edit,
    at: usize,
}

const ROWS: usize = 9;
const ROW: f32 = 34.0;

impl App {
    // ---- the session ----

    /// Something about the tabs changed: write them down a moment later.
    pub(super) fn session_changed(&mut self) {
        if self.shot.is_none() {
            unsafe {
                SetTimer(Some(self.hwnd), 5, 1500, None);
            }
        }
    }

    pub(super) fn save_session(&self) {
        if self.shot.is_some() {
            return;
        }
        let mut out = format!("cur\t{}\n", self.cur);
        for t in &self.tabs {
            // a tab still asleep keeps the place it had
            let at = t.restore.filter(|_| t.asleep).unwrap_or(t.scroll);
            out.push_str(&format!("{:.0}\t{}\n", at, if t.url.is_empty() { START } else { &t.url }));
        }
        let _ = std::fs::write(session_file(), out);
    }

    /// Puts last session's tabs back, asleep. Returns whether there were any.
    pub(super) fn restore_session(&mut self) -> bool {
        let Ok(text) = std::fs::read_to_string(session_file()) else { return false };
        let mut cur = 0;
        for line in text.lines() {
            let Some((a, b)) = line.split_once('\t') else { continue };
            if a == "cur" {
                cur = b.parse().unwrap_or(0);
                continue;
            }
            let id = self.next_id;
            self.next_id += 1;
            let mut t = Tab::new(id);
            t.url = b.to_string();
            t.title = if b == START { "New tab".into() } else { host_of(b).trim_start_matches("www.").to_string() };
            t.restore = Some(a.parse().unwrap_or(0.0));
            t.asleep = true;
            self.tabs.push(t);
        }
        self.cur = cur.min(self.tabs.len().saturating_sub(1));
        !self.tabs.is_empty()
    }

    /// Loads the tab in front if it is still asleep.
    pub(super) fn wake_asleep(&mut self) {
        let c = self.cur;
        if self.tabs.get(c).is_some_and(|t| t.asleep) {
            self.tabs[c].asleep = false;
            let url = self.tabs[c].url.clone();
            self.load(c, url, false);
        }
    }

    // ---- closed tabs ----

    pub(super) fn remember_closed(&mut self, i: usize) {
        let t = &self.tabs[i];
        if (t.url == START || t.url.is_empty()) && t.back.is_empty() {
            return;
        }
        if self.closed.len() == CLOSED {
            self.closed.remove(0);
        }
        let scroll = t.restore.filter(|_| t.asleep).unwrap_or(t.scroll);
        self.closed.push(Closed { url: t.url.clone(), scroll, back: t.back.clone(), fwd: t.fwd.clone(), at: i });
    }

    pub(super) fn reopen_closed(&mut self) {
        let Some(c) = self.closed.pop() else { return self.say("no closed tabs to bring back") };
        let id = self.next_id;
        self.next_id += 1;
        let mut t = Tab::new(id);
        (t.back, t.fwd, t.restore) = (c.back, c.fwd, Some(c.scroll));
        let at = c.at.min(self.tabs.len());
        self.tabs.insert(at, t);
        self.cur = at;
        self.load(at, c.url, false);
        self.update_views();
    }

    // ---- the tab finder ----

    pub(super) fn open_finder(&mut self) {
        self.blur();
        self.addr_focused = false;
        self.finder = Some(Finder { edit: Edit::default(), at: 0 });
    }

    /// Tabs that match what was typed, best first. With nothing typed, the ones looked
    /// at most recently come first, after the one in front, so Ctrl+K and Enter goes
    /// back to the tab before.
    fn found(&self) -> Vec<usize> {
        let Some(f) = &self.finder else { return Vec::new() };
        let q = f.edit.text().to_lowercase();
        let mut hits: Vec<(u32, std::time::Duration, usize)> = self
            .tabs
            .iter()
            .enumerate()
            .filter_map(|(i, t)| {
                let hay = format!("{} {}", t.title, t.url.trim_start_matches("https://")).to_lowercase();
                let score = if q.is_empty() {
                    (i == self.cur) as u32
                } else if let Some(p) = hay.find(&q) {
                    p as u32
                } else {
                    // the letters in order, with gaps
                    let mut it = hay.chars();
                    if !q.chars().all(|c| it.any(|h| h == c)) {
                        return None;
                    }
                    1000
                };
                Some((score, t.used.elapsed(), i))
            })
            .collect();
        hits.sort();
        hits.into_iter().map(|h| h.2).collect()
    }

    pub(super) fn finder_key(&mut self, vk: VIRTUAL_KEY, m: Mods) -> bool {
        if self.finder.is_none() {
            return false;
        }
        let found = self.found();
        let Some(f) = self.finder.as_mut() else { return false };
        match vk {
            VK_ESCAPE => self.finder = None,
            VK_DOWN | VK_TAB => f.at = (f.at + 1).min(found.len().saturating_sub(1)),
            VK_UP => f.at = f.at.saturating_sub(1),
            VK_RETURN => {
                if let Some(&i) = found.get(f.at) {
                    self.finder = None;
                    self.pick_tab(i);
                }
            }
            _ => {
                if fields::edit_key(&mut f.edit, vk, m) {
                    f.at = 0;
                } else if !m.ctrl {
                    // letters arrive as characters
                    return true;
                } else {
                    return false;
                }
            }
        }
        true
    }

    pub(super) fn finder_char(&mut self, c: char) -> bool {
        let Some(f) = self.finder.as_mut() else { return false };
        f.edit.insert(&c.to_string());
        f.at = 0;
        true
    }

    pub(super) fn pick_tab(&mut self, i: usize) {
        self.blur();
        self.cur = i;
        self.update_title();
        self.update_views();
    }

    /// Where the finder is, in window css px.
    fn finder_rect(&self, rows: usize) -> Rect {
        let w = self.w as f32 / self.scale;
        let fw = (w - 48.0).min(560.0);
        Rect::new((w - fw) / 2.0, TOP + 28.0, fw, 50.0 + rows.max(1) as f32 * ROW + 8.0)
    }

    /// A click while the finder is open. Returns whether the finder took it.
    pub(super) fn finder_click(&mut self, x: f32, y: f32) -> bool {
        if self.finder.is_none() {
            return false;
        }
        let found = self.found();
        let r = self.finder_rect(found.len().min(ROWS));
        let (x, y) = (x / self.scale, y / self.scale);
        let row = ((y - r.y - 50.0) / ROW).floor();
        self.finder = None;
        if r.contains(x, y) && row >= 0.0
            && let Some(&i) = found.get(row as usize)
        {
            self.pick_tab(i);
        }
        true
    }

    pub(super) fn draw_finder(&mut self) {
        let Some(f) = self.finder.take() else { return };
        let found = self.found_for(&f);
        let s = sk();
        let r = self.finder_rect(found.len().min(ROWS));
        let font = if s.drawn { text::TYPEWRITER } else { UI_FONT };
        let k = self.scale;
        self.rect(Rect::new(r.x - 1.0, r.y + 3.0, r.w + 2.0, r.h + 2.0), [0, 0, 0, 36], 8.0);
        self.rect(r, s.field, 7.0);
        self.gpu.ring(r.x * k, r.y * k, r.w * k, r.h * k, 1.0, s.faint, 7.0 * k, gpu::NO_CLIP);
        let typed = f.edit.text();
        if typed.is_empty() {
            self.label("find a tab", r.x + 18.0, r.y + 15.0, 15.0, 400, s.faint, r.w - 36.0, font);
        } else {
            self.label(&typed, r.x + 18.0, r.y + 15.0, 15.0, 400, s.text, r.w - 36.0, font);
        }
        let caret = self.ui_text(&f.edit.before(), 15.0, 400, font, s.text).1;
        self.rect(Rect::new(r.x + 18.5 + caret, r.y + 14.0, 1.6, 21.0), s.ink, 0.8);
        self.rect(Rect::new(r.x + 12.0, r.y + 46.0, r.w - 24.0, 1.0), s.hover, 0.0);
        let first = f.at.saturating_sub(ROWS - 1);
        for (row, &i) in found.iter().enumerate().skip(first).take(ROWS) {
            let y = r.y + 50.0 + (row - first) as f32 * ROW;
            if row == f.at {
                self.rect(Rect::new(r.x + 6.0, y + 2.0, r.w - 12.0, ROW - 4.0), s.hover, 4.0);
            }
            let t = &self.tabs[i];
            let host = if t.url == START { String::new() } else { host_of(&t.url).trim_start_matches("www.").to_string() };
            let title = t.title.clone();
            let hw = if host.is_empty() { 0.0 } else { self.ui_text(&host, 12.0, 400, font, s.muted).1.min(r.w * 0.4) };
            self.label(&title, r.x + 18.0, y + 8.0, 14.0, 400, s.text, r.w - hw - 52.0, font);
            self.label(&host, r.x + r.w - 18.0 - hw, y + 10.0, 12.0, 400, s.muted, hw + 2.0, font);
        }
        if found.is_empty() {
            self.label("no tab matches", r.x + 18.0, r.y + 58.0, 13.0, 400, s.faint, r.w - 36.0, font);
        }
        self.finder = Some(f);
    }

    fn found_for(&mut self, f: &Finder) -> Vec<usize> {
        let keep = self.finder.replace(Finder { edit: f.edit.clone(), at: f.at });
        let v = self.found();
        self.finder = keep;
        v
    }
}
