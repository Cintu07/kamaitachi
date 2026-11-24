//! The browser's side of ad blocking. The matching itself lives in omamori, its own
//! crate. What stays here is what only a browser needs: which lists to fetch, keeping
//! them fresh on disk, one blocker shared by every tab, and the sites a person chose
//! to open even though a list flagged them.

use std::sync::{Arc, Mutex, OnceLock, RwLock};

pub use omamori::{Blocker, Kind, site};

/// (file name, where it comes from, whether it lists malware or phishing)
const LISTS: [(&str, &str, bool); 8] = [
    ("easylist.txt", "https://easylist.to/easylist/easylist.txt", false),
    ("easyprivacy.txt", "https://easylist.to/easylist/easyprivacy.txt", false),
    ("ubo-filters.txt", "https://ublockorigin.github.io/uAssets/filters/filters.txt", false),
    ("ubo-privacy.txt", "https://ublockorigin.github.io/uAssets/filters/privacy.txt", false),
    ("ubo-unbreak.txt", "https://ublockorigin.github.io/uAssets/filters/unbreak.txt", false),
    ("ubo-badware.txt", "https://ublockorigin.github.io/uAssets/filters/badware.txt", true),
    ("urlhaus.txt", "https://malware-filter.gitlab.io/malware-filter/urlhaus-filter-ag-online.txt", true),
    ("phishing.txt", "https://malware-filter.gitlab.io/malware-filter/phishing-filter-ag.txt", true),
];

/// Lists older than this get downloaded again.
const STALE_AFTER_SECS: u64 = 4 * 86400;

fn dir() -> std::path::PathBuf {
    let base = std::env::var("APPDATA").unwrap_or_else(|_| ".".into());
    std::path::Path::new(&base).join("hayate").join("filters")
}

struct Shared {
    blocker: Arc<Blocker>,
    /// the hiding rules every site gets, parsed once and handed to every page
    generic: Arc<super::css::Sheet>,
}

fn global() -> &'static RwLock<Option<Shared>> {
    static G: OnceLock<RwLock<Option<Shared>>> = OnceLock::new();
    G.get_or_init(|| RwLock::new(None))
}

fn off() -> bool {
    std::env::var("HAYATE_NOBLOCK").is_ok()
}

/// The blocker every tab shares, unless blocking is switched off.
pub fn get() -> Option<Arc<Blocker>> {
    if off() {
        return None;
    }
    global().read().ok()?.as_ref().map(|s| s.blocker.clone())
}

pub fn generic_sheet() -> Option<Arc<super::css::Sheet>> {
    if off() {
        return None;
    }
    global().read().ok()?.as_ref().map(|s| s.generic.clone())
}

fn install(b: Blocker) {
    let (every_site, _) = b.hiding("");
    let sheet = super::css::parse_sheet_interned(&every_site);
    if let Ok(mut g) = global().write() {
        *g = Some(Shared { blocker: Arc::new(b), generic: Arc::new(sheet) });
    }
}

/// Starts with omamori's small built-in list right away, then loads the real lists on
/// a background thread so the first page never waits on a download.
pub fn start() {
    install(Blocker::new(&[(omamori::STARTER, false)]));
    let _ = std::thread::Builder::new().name("filters".into()).spawn(|| install(build()));
}

/// Fetches any list that is missing or stale, then builds a blocker from all of them.
/// Runs on whatever thread calls it.
pub fn build() -> Blocker {
    let d = dir();
    let _ = std::fs::create_dir_all(&d);
    let fetch = |u: &str| super::load::fetch_bytes(u, "text/plain").ok().map(|b| String::from_utf8_lossy(&b).into_owned());
    let mut texts = vec![(omamori::STARTER.to_string(), false)];
    for (file, url, danger) in LISTS {
        let p = d.join(file);
        let age = std::fs::metadata(&p).and_then(|m| m.modified()).ok().and_then(|t| t.elapsed().ok());
        let fresh = age.is_some_and(|a| a.as_secs() < STALE_AFTER_SECS);
        if !fresh
            && let Some(text) = fetch(url)
            && text.len() > 1000
        {
            let _ = std::fs::write(&p, omamori::expand(&text, url, &fetch));
        }
        if let Ok(t) = std::fs::read_to_string(&p) {
            texts.push((t, danger));
        }
    }
    let refs: Vec<(&str, bool)> = texts.iter().map(|(s, d)| (s.as_str(), *d)).collect();
    Blocker::new(&refs)
}

fn allowed() -> &'static Mutex<Vec<String>> {
    static A: OnceLock<Mutex<Vec<String>>> = OnceLock::new();
    A.get_or_init(|| Mutex::new(Vec::new()))
}

/// Someone saw the warning and opened the site anyway. That holds until kamaitachi
/// closes, and never gets written down.
pub fn allow(host: &str) {
    allowed().lock().unwrap_or_else(|e| e.into_inner()).push(host.to_ascii_lowercase());
}

/// Should this page be stopped before anything of it loads?
pub fn is_danger(url: &str) -> bool {
    let Some(b) = get() else { return false };
    let host = url.split("://").nth(1).unwrap_or(url).split(['/', '?', '#', ':']).next().unwrap_or("").to_ascii_lowercase();
    if allowed().lock().unwrap_or_else(|e| e.into_inner()).contains(&host) {
        return false;
    }
    b.danger(url)
}

/// `hayate filters`: refreshes the lists and prints what the blocker holds.
pub fn report() -> String {
    let t = std::time::Instant::now();
    let b = build();
    let built = t.elapsed();
    let (every_site, _) = b.hiding("");
    let t2 = std::time::Instant::now();
    let sheet = super::css::parse_sheet_interned(&every_site);
    let sheet_ms = t2.elapsed();
    let probe = std::time::Instant::now();
    let mut n = 0;
    for _ in 0..10_000 {
        if b.blocks("https://securepubads.g.doubleclick.net/gampad/ads?iu=/1234/site&sz=300x250", "www.example-news.com", Kind::Script) {
            n += 1;
        }
        if b.blocks("https://cdn.example-news.com/assets/app.3f9a2c.js", "www.example-news.com", Kind::Script) {
            n += 1;
        }
    }
    let per_us = probe.elapsed().as_secs_f64() * 1e6 / 20_000.0;
    format!(
        "filters: {} network rules ({} as host hashes), {} element-hiding rules\nmalware and phishing: {} rules (pages on them are stopped)\nblocker memory: {:.2} mb, built in {} ms\ngeneric hiding sheet: {} rules parsed in {} ms\nmatch: {per_us:.1} us per request ({n} of 20000 probes blocked)",
        b.counts.0,
        b.host_rules(),
        b.counts.1,
        b.danger_count,
        b.bytes() as f64 / 1048576.0,
        built.as_millis(),
        sheet.rules.len(),
        sheet_ms.as_millis(),
    )
}
