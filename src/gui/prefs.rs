//! Everything you can change about kamaitachi, kept in %APPDATA%\hayate\settings.txt as
//! one `name = value` a line. A line it does not know is kept as it is, so an older
//! version never throws away what a newer one wrote.

use std::cell::{Cell, RefCell};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Theme {
    Light,
    Dark,
    /// whatever Windows is set to
    System,
}

/// Where the tabs go.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Side {
    Left,
    Right,
}

#[derive(Clone, Copy, Debug)]
pub struct Prefs {
    pub theme: Theme,
    /// index into skin::ACCENTS
    pub accent: usize,
    pub side: Side,
    /// css px
    pub column: f32,
    pub roomy: bool,
    pub card: bool,
    pub address: bool,
    pub loading_line: bool,
    /// a band of Greek key along the foot of the column
    pub meander: bool,
    /// faint veins in the marble of the column
    pub veins: bool,
    pub contrast: bool,
    /// each tab fills in as you read down its page
    pub read_fill: bool,
    pub dim_sleeping: bool,
    pub tab_numbers: bool,
    /// index into SEARCH
    pub search: usize,
    /// minutes before a tab you are not using lets its page go; 0 never
    pub sleep_after: u32,
    /// hours before a tab you have not looked at is closed; 0 never
    pub close_after: u32,
    pub restore: bool,
    pub close_goes_back: bool,
    pub beside: bool,
    pub backspace_back: bool,
    pub smooth: bool,
    pub block_ads: bool,
    pub clean_links: bool,
    pub gpc: bool,
    /// percent
    pub zoom: u32,
}

/// (name in the settings, the address a search goes to, with %s for the words)
pub const SEARCH: [(&str, &str); 7] = [
    ("DuckDuckGo Lite", "https://lite.duckduckgo.com/lite/?q=%s"),
    ("DuckDuckGo", "https://duckduckgo.com/?q=%s"),
    ("Google", "https://www.google.com/search?q=%s"),
    ("Bing", "https://www.bing.com/search?q=%s"),
    ("Brave Search", "https://search.brave.com/search?q=%s"),
    ("Startpage", "https://www.startpage.com/do/search?q=%s"),
    ("Kagi", "https://kagi.com/search?q=%s"),
];

impl Default for Prefs {
    fn default() -> Prefs {
        Prefs {
            theme: Theme::Light,
            accent: 0,
            side: Side::Left,
            column: 232.0,
            roomy: false,
            card: true,
            address: true,
            loading_line: true,
            meander: true,
            veins: true,
            contrast: false,
            read_fill: true,
            dim_sleeping: true,
            tab_numbers: true,
            search: 0,
            sleep_after: 30,
            close_after: 0,
            restore: true,
            close_goes_back: true,
            beside: true,
            backspace_back: false,
            smooth: true,
            block_ads: true,
            clean_links: true,
            gpc: true,
            zoom: 100,
        }
    }
}

fn file() -> std::path::PathBuf {
    let base = std::env::var("APPDATA").unwrap_or_else(|_| ".".into());
    std::path::Path::new(&base).join("hayate").join("settings.txt")
}

thread_local! {
    static PREFS: Cell<Prefs> = Cell::new(Prefs::default());
    /// the file as it was read, so lines this version does not know are written back
    static LINES: RefCell<Vec<String>> = const { RefCell::new(Vec::new()) };
}

/// The settings now.
pub fn get() -> Prefs {
    PREFS.with(|p| p.get())
}

/// Reads the settings file. Call once, at start.
pub fn load() {
    let text = std::fs::read_to_string(file()).unwrap_or_default();
    let lines: Vec<String> = text.lines().map(str::to_string).collect();
    let mut p = Prefs::default();
    for l in &lines {
        if let Some((k, v)) = l.split_once('=') {
            p.apply(k.trim(), v.trim());
        }
    }
    PREFS.with(|c| c.set(p));
    LINES.with(|l| *l.borrow_mut() = lines);
}

/// Changes one setting and writes the file. Returns whether the name and value made
/// sense.
pub fn set(key: &str, value: &str) -> bool {
    let mut p = get();
    if !p.apply(key, value) {
        return false;
    }
    PREFS.with(|c| c.set(p));
    LINES.with(|l| {
        let mut lines = l.borrow_mut();
        let line = format!("{key} = {value}");
        match lines.iter_mut().find(|l| l.split_once('=').is_some_and(|(k, _)| k.trim() == key)) {
            Some(l) => *l = line,
            None => lines.push(line),
        }
        let _ = std::fs::write(file(), lines.join("\n") + "\n");
    });
    true
}

fn flag(v: &str) -> Option<bool> {
    match v {
        "on" | "true" | "1" | "yes" => Some(true),
        "off" | "false" | "0" | "no" => Some(false),
        _ => None,
    }
}

impl Prefs {
    /// The value of a setting as the settings page and the file write it.
    pub fn value(&self, key: &str) -> String {
        let on = |b: bool| if b { "on" } else { "off" }.to_string();
        match key {
            "theme" => match self.theme {
                Theme::Light => "light",
                Theme::Dark => "dark",
                Theme::System => "system",
            }
            .into(),
            "accent" => self.accent.to_string(),
            "side" => if self.side == Side::Left { "left" } else { "right" }.into(),
            "column" => format!("{:.0}", self.column),
            "search" => self.search.to_string(),
            "sleep_after" => self.sleep_after.to_string(),
            "close_after" => self.close_after.to_string(),
            "zoom" => self.zoom.to_string(),
            k => on(self.flag(k).unwrap_or(false)),
        }
    }

    fn flag(&self, key: &str) -> Option<bool> {
        Some(match key {
            "roomy" => self.roomy,
            "card" => self.card,
            "address" => self.address,
            "loading_line" => self.loading_line,
            "meander" => self.meander,
            "veins" => self.veins,
            "contrast" => self.contrast,
            "read_fill" => self.read_fill,
            "dim_sleeping" => self.dim_sleeping,
            "tab_numbers" => self.tab_numbers,
            "restore" => self.restore,
            "close_goes_back" => self.close_goes_back,
            "beside" => self.beside,
            "backspace_back" => self.backspace_back,
            "smooth" => self.smooth,
            "block_ads" => self.block_ads,
            "clean_links" => self.clean_links,
            "gpc" => self.gpc,
            _ => return None,
        })
    }

    fn apply(&mut self, key: &str, v: &str) -> bool {
        let num = v.parse::<u32>().ok();
        match key {
            "theme" => {
                self.theme = match v {
                    "light" => Theme::Light,
                    "dark" => Theme::Dark,
                    "system" => Theme::System,
                    _ => return false,
                }
            }
            "accent" => match num.filter(|n| (*n as usize) < super::skin::ACCENTS.len()) {
                Some(n) => self.accent = n as usize,
                None => return false,
            },
            "side" => {
                self.side = match v {
                    "left" => Side::Left,
                    "right" => Side::Right,
                    _ => return false,
                }
            }
            "column" => match num.filter(|n| (176..=440).contains(n)) {
                Some(n) => self.column = n as f32,
                None => return false,
            },
            "search" => match num.filter(|n| (*n as usize) < SEARCH.len()) {
                Some(n) => self.search = n as usize,
                None => return false,
            },
            "sleep_after" => match num {
                Some(n) => self.sleep_after = n,
                None => return false,
            },
            "close_after" => match num {
                Some(n) => self.close_after = n,
                None => return false,
            },
            "zoom" => match num.filter(|n| (50..=300).contains(n)) {
                Some(n) => self.zoom = n,
                None => return false,
            },
            k => {
                let Some(b) = flag(v) else { return false };
                let slot = match k {
                    "roomy" => &mut self.roomy,
                    "card" => &mut self.card,
                    "address" => &mut self.address,
                    "loading_line" => &mut self.loading_line,
                    "meander" => &mut self.meander,
                    "veins" => &mut self.veins,
                    "contrast" => &mut self.contrast,
                    "read_fill" => &mut self.read_fill,
                    "dim_sleeping" => &mut self.dim_sleeping,
                    "tab_numbers" => &mut self.tab_numbers,
                    "restore" => &mut self.restore,
                    "close_goes_back" => &mut self.close_goes_back,
                    "beside" => &mut self.beside,
                    "backspace_back" => &mut self.backspace_back,
                    "smooth" => &mut self.smooth,
                    "block_ads" => &mut self.block_ads,
                    "clean_links" => &mut self.clean_links,
                    "gpc" => &mut self.gpc,
                    _ => return false,
                };
                *slot = b;
            }
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn values_go_in_and_come_back_out() {
        let mut p = Prefs::default();
        for (k, v) in [("theme", "dark"), ("accent", "2"), ("column", "280"), ("roomy", "on"), ("search", "3"), ("zoom", "125"), ("side", "right")] {
            assert!(p.apply(k, v), "{k}");
            assert_eq!(p.value(k), v);
        }
        for (k, v) in [("theme", "pink"), ("column", "9000"), ("roomy", "maybe"), ("nonsense", "on"), ("search", "99")] {
            assert!(!p.apply(k, v), "{k}={v}");
        }
    }
}
