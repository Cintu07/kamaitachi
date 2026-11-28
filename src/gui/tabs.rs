//! Tabs that outlast the window: last session's tabs come back asleep, costing a line of
//! text each until you look at one; a closed tab can be opened again with its history;
//! and a few letters of a tab's name or address find it from the address field.

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
        places::save();
        let mut out = format!("cur\t{}\n", self.cur);
        for t in self.tabs.iter().filter(|t| !t.peek) {
            // a tab still asleep keeps the place it had; a pinned one says so with a p
            let at = t.restore.filter(|_| t.asleep).unwrap_or(t.scroll);
            out.push_str(&format!("{}{:.0}\t{}\n", if t.pinned { "p" } else { "" }, at, if t.url.is_empty() { START } else { &t.url }));
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
            t.pinned = a.starts_with('p');
            t.restore = Some(a.trim_start_matches('p').parse().unwrap_or(0.0));
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

    // ---- finding a tab ----

    /// Tabs whose name or address has what was typed, best first: the words as they
    /// are, then the letters in order with gaps; the ones used last first within each.
    pub(super) fn tabs_matching(&self, q: &str) -> Vec<usize> {
        let q = q.to_lowercase();
        let mut hits: Vec<(u32, std::time::Duration, usize)> = self
            .tabs
            .iter()
            .enumerate()
            .filter(|(_, t)| !t.peek)
            .filter_map(|(i, t)| {
                let hay = format!("{} {}", t.title, t.url.trim_start_matches("https://")).to_lowercase();
                let score = match hay.find(&q) {
                    Some(p) => p as u32,
                    None => {
                        let mut it = hay.chars();
                        if !q.chars().all(|c| it.any(|h| h == c)) {
                            return None;
                        }
                        1000
                    }
                };
                Some((score, t.used.elapsed(), i))
            })
            .collect();
        hits.sort();
        hits.into_iter().map(|h| h.2).collect()
    }

    pub(super) fn pick_tab(&mut self, i: usize) {
        self.blur();
        self.cur = i;
        self.update_title();
        self.update_views();
    }
}
