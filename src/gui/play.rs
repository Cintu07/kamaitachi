//! The fun part. kamaitachi counts a few things you would never count yourself (ads
//! cut down, phishing pages stopped, pages read without Chromium) and turns them into
//! a spirit that grows, stamps you collect, and skins you unlock.
//!
//! Nothing here nags. There are no streaks to lose and no notifications. The counts
//! live in one small text file in %APPDATA%\hayate, never leave the machine, and the
//! whole thing stays well under the 1 MB it is allowed.

use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Clone, Default, Debug, PartialEq)]
pub struct Stats {
    pub blocked: u64,
    pub threats: u64,
    pub lite_pages: u64,
    pub full_pages: u64,
    pub switches: u64,
    /// different days kamaitachi was opened on
    pub days: u64,
    pub last_day: u64,
    pub night_owl: bool,
    pub own_wallpaper: bool,
    /// stamps earned, with the day each one came
    pub stamps: Vec<(String, u64)>,
}

pub struct Stamp {
    pub id: &'static str,
    pub title: &'static str,
    /// one line about what it took
    pub what: &'static str,
    earned: fn(&Stats) -> bool,
}

pub const STAMPS: [Stamp; 10] = [
    Stamp { id: "first-cut", title: "First cut", what: "blocked your first ad or tracker", earned: |s| s.blocked >= 1 },
    Stamp { id: "hundred", title: "A hundred cuts", what: "100 ads and trackers blocked", earned: |s| s.blocked >= 100 },
    Stamp { id: "thousand", title: "A thousand cuts", what: "1,000 blocked", earned: |s| s.blocked >= 1_000 },
    Stamp { id: "whirlwind", title: "Whirlwind", what: "10,000 blocked", earned: |s| s.blocked >= 10_000 },
    Stamp { id: "ward", title: "Ward", what: "stopped a malware or phishing page", earned: |s| s.threats >= 1 },
    Stamp { id: "reader", title: "Reader", what: "read 100 pages without Chromium", earned: |s| s.lite_pages >= 100 },
    Stamp { id: "both-blades", title: "Both blades", what: "moved a site between engines by hand", earned: |s| s.switches >= 1 },
    Stamp { id: "night-owl", title: "Night owl", what: "was browsing between 2 and 4 in the morning", earned: |s| s.night_owl },
    Stamp { id: "regular", title: "Regular", what: "opened kamaitachi on 7 different days", earned: |s| s.days >= 7 },
    Stamp { id: "own-sky", title: "Your own sky", what: "dropped in a wallpaper of your own", earned: |s| s.own_wallpaper },
];

pub fn today() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs() / 86400).unwrap_or(0)
}

fn file() -> std::path::PathBuf {
    let base = std::env::var("APPDATA").unwrap_or_else(|_| ".".into());
    std::path::Path::new(&base).join("hayate").join("stats.txt")
}

impl Stats {
    /// Reads the counts, and marks today as a day kamaitachi was used.
    pub fn load() -> Stats {
        let mut s = Stats::parse(&std::fs::read_to_string(file()).unwrap_or_default());
        let d = today();
        if s.last_day != d {
            s.days += 1;
            s.last_day = d;
        }
        s
    }

    pub fn parse(text: &str) -> Stats {
        let mut s = Stats::default();
        for line in text.lines() {
            let mut w = line.split_whitespace();
            let (Some(k), Some(v)) = (w.next(), w.next()) else { continue };
            let n = v.parse::<u64>().unwrap_or(0);
            match k {
                "blocked" => s.blocked = n,
                "threats" => s.threats = n,
                "lite_pages" => s.lite_pages = n,
                "full_pages" => s.full_pages = n,
                "switches" => s.switches = n,
                "days" => s.days = n,
                "last_day" => s.last_day = n,
                "night_owl" => s.night_owl = n == 1,
                "own_wallpaper" => s.own_wallpaper = n == 1,
                "stamp" => s.stamps.push((v.to_string(), w.next().and_then(|d| d.parse().ok()).unwrap_or(0))),
                _ => {}
            }
        }
        s
    }

    pub fn text(&self) -> String {
        let mut t = format!(
            "blocked {}\nthreats {}\nlite_pages {}\nfull_pages {}\nswitches {}\ndays {}\nlast_day {}\nnight_owl {}\nown_wallpaper {}\n",
            self.blocked, self.threats, self.lite_pages, self.full_pages, self.switches, self.days, self.last_day, self.night_owl as u8, self.own_wallpaper as u8
        );
        for (id, day) in &self.stamps {
            t.push_str(&format!("stamp {id} {day}\n"));
        }
        t
    }

    pub fn save(&self) {
        let p = file();
        if let Some(d) = p.parent() {
            let _ = std::fs::create_dir_all(d);
        }
        let _ = std::fs::write(p, self.text());
    }

    pub fn has(&self, id: &str) -> bool {
        self.stamps.iter().any(|(s, _)| s == id)
    }

    /// Hands out any stamps that were just earned and returns them.
    pub fn award(&mut self) -> Vec<&'static Stamp> {
        let mut new = Vec::new();
        for st in &STAMPS {
            if !self.has(st.id) && (st.earned)(self) {
                self.stamps.push((st.id.to_string(), today()));
                new.push(st);
            }
        }
        new
    }
}

/// The spirit's forms, by ads and trackers blocked over its whole life.
pub const FORMS: [(&str, u64); 5] = [("pup", 0), ("blade", 100), ("gale", 1_000), ("whirlwind", 10_000), ("three-tail", 100_000)];

/// (form index, form name, how many it needs for the next one)
pub fn form(blocked: u64) -> (usize, &'static str, Option<u64>) {
    let i = FORMS.iter().rposition(|(_, at)| blocked >= *at).unwrap_or(0);
    (i, FORMS[i].0, FORMS.get(i + 1).map(|f| f.1))
}

/// The spirit, 16 by 10. `#` is fur, `o` is the eye, `b` is blade and `m` is where
/// the mouth opens when it eats. Forms add tails and blades on top of this.
pub const SPRITE: [&str; 10] = [
    "...........##...",
    "..........####..",
    ".........##o###.",
    "........######m#",
    ".......#######..",
    "......#######...",
    "..#########.....",
    "..##.##..##.##..",
    "..##.##..##.##..",
    "................",
];

/// Extra tails, one per row offset, drawn left of the body as the spirit grows.
pub const TAIL: [&str; 3] = ["##......", ".###....", "...#####"];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counts_survive_a_round_trip() {
        let mut s = Stats { blocked: 1234, threats: 2, lite_pages: 150, days: 3, night_owl: true, ..Default::default() };
        let got = s.award();
        assert!(got.iter().any(|st| st.id == "thousand"));
        assert!(!got.iter().any(|st| st.id == "whirlwind"));
        assert_eq!(Stats::parse(&s.text()), s);
        assert!(s.award().is_empty(), "a stamp comes once");
    }

    #[test]
    fn forms_follow_the_count() {
        assert_eq!(form(0).1, "pup");
        assert_eq!(form(999).1, "blade");
        assert_eq!(form(1_000).1, "gale");
        assert_eq!(form(250_000), (4, "three-tail", None));
    }

    /// The whole game has to fit in the 1 MB it was given. With every stamp earned the
    /// file is a few hundred bytes and the struct a few hundred more.
    #[test]
    fn the_game_stays_under_a_megabyte() {
        let mut s = Stats { blocked: u64::MAX / 2, threats: 9, lite_pages: 9_999, switches: 9, days: 999, night_owl: true, own_wallpaper: true, ..Default::default() };
        s.award();
        assert_eq!(s.stamps.len(), STAMPS.len());
        let bytes = s.text().len() + std::mem::size_of::<Stats>() + s.stamps.iter().map(|x| x.0.capacity() + 32).sum::<usize>();
        assert!(bytes < 4096, "{bytes} bytes");
        assert!(bytes < 1 << 20);
    }
}
