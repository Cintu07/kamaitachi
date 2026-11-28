//! Where you have been and what you kept: history and bookmarks. Both are small text
//! files in %APPDATA%\hayate, read on first use, and both have a page of their own with
//! a box that searches them. The address field offers them as you type.

use super::*;
use std::cell::RefCell;

/// The most places history keeps; the ones visited least lately go first.
const KEEP: usize = 4000;

#[derive(Clone)]
pub(super) struct Place {
    pub url: String,
    pub title: String,
    /// unix seconds of the last visit
    pub last: u64,
    pub visits: u32,
}

#[derive(Default)]
struct Places {
    loaded: bool,
    history: Vec<Place>,
    marks: Vec<(String, String)>,
    dirty: bool,
}

thread_local! {
    static PLACES: RefCell<Places> = RefCell::new(Places::default());
}

fn file(name: &str) -> std::path::PathBuf {
    wall_dir().join(name)
}

fn with<R>(f: impl FnOnce(&mut Places) -> R) -> R {
    PLACES.with(|p| {
        let mut p = p.borrow_mut();
        if !p.loaded {
            p.loaded = true;
            let text = std::fs::read_to_string(file("history.txt")).unwrap_or_default();
            p.history = text
                .lines()
                .filter_map(|l| {
                    let mut f = l.splitn(4, '\t');
                    let (last, visits, url, title) = (f.next()?.parse().ok()?, f.next()?.parse().ok()?, f.next()?, f.next().unwrap_or(""));
                    Some(Place { url: url.into(), title: title.into(), last, visits })
                })
                .collect();
            let text = std::fs::read_to_string(file("bookmarks.txt")).unwrap_or_default();
            p.marks = text.lines().filter_map(|l| l.split_once('\t')).map(|(u, t)| (u.into(), t.into())).collect();
        }
        f(&mut p)
    })
}

/// Pages worth remembering: not the new tab, not the browser's own.
fn worth(url: &str) -> bool {
    url.starts_with("https://") && !url.starts_with("https://kamaitachi.local/")
}

fn clean(s: &str) -> String {
    s.replace(['\t', '\n', '\r'], " ")
}

/// Notes a visit, or a better title for one.
pub(super) fn visit(url: &str, title: &str) {
    if !worth(url) {
        return;
    }
    let now = crate::sys::now_unix();
    with(|p| {
        match p.history.iter_mut().find(|v| v.url == url) {
            Some(v) => {
                if now - v.last > 30 {
                    v.visits += 1;
                }
                v.last = now;
                if !title.is_empty() {
                    v.title = clean(title);
                }
            }
            None => p.history.push(Place { url: url.into(), title: clean(title), last: now, visits: 1 }),
        }
        if p.history.len() > KEEP {
            p.history.sort_by_key(|v| std::cmp::Reverse(v.last));
            p.history.truncate(KEEP);
        }
        p.dirty = true;
    });
}

/// Writes history if it changed.
pub(super) fn save() {
    with(|p| {
        if !std::mem::take(&mut p.dirty) {
            return;
        }
        let text: String = p.history.iter().map(|v| format!("{}\t{}\t{}\t{}\n", v.last, v.visits, v.url, v.title)).collect();
        let _ = std::fs::write(file("history.txt"), text);
    });
}

pub(super) fn clear_history() {
    with(|p| {
        p.history.clear();
        p.dirty = true;
    });
    save();
}

pub(super) fn marked(url: &str) -> bool {
    with(|p| p.marks.iter().any(|m| m.0 == url))
}

/// Bookmarks the page, or takes the bookmark away. Returns whether it is kept now.
pub(super) fn toggle_mark(url: &str, title: &str) -> bool {
    let on = with(|p| match p.marks.iter().position(|m| m.0 == url) {
        Some(i) => {
            p.marks.remove(i);
            false
        }
        None => {
            p.marks.push((url.into(), clean(title)));
            true
        }
    });
    let text: String = with(|p| p.marks.iter().map(|(u, t)| format!("{u}\t{t}\n")).collect());
    let _ = std::fs::write(file("bookmarks.txt"), text);
    on
}

fn words_in(q: &str, url: &str, title: &str) -> bool {
    let hay = format!("{} {}", url.trim_start_matches("https://"), title).to_lowercase();
    q.split_whitespace().all(|w| hay.contains(&w.to_lowercase()))
}

/// Bookmarks and places you went that have what was typed, best first: bookmarks, then
/// by how often and how lately.
pub(super) fn matching(q: &str, most: usize) -> Vec<(String, String, bool)> {
    if q.trim().is_empty() {
        return Vec::new();
    }
    with(|p| {
        let now = crate::sys::now_unix();
        let mut out: Vec<(String, String, bool)> = p.marks.iter().filter(|m| words_in(q, &m.0, &m.1)).map(|m| (m.0.clone(), m.1.clone(), true)).collect();
        let mut seen: Vec<&Place> = p.history.iter().filter(|v| words_in(q, &v.url, &v.title) && !out.iter().any(|o| o.0 == v.url)).collect();
        // a visit counts for less the older it is: half after a week
        let score = |v: &Place| v.visits as f64 / (1.0 + (now.saturating_sub(v.last)) as f64 / 604_800.0);
        seen.sort_by(|a, b| score(b).total_cmp(&score(a)));
        out.extend(seen.into_iter().map(|v| (v.url.clone(), v.title.clone(), false)));
        out.truncate(most);
        out
    })
}

/// Seconds this machine's clock is ahead of UTC, from what Windows says the time is in
/// both.
fn local_offset() -> i64 {
    use windows_sys::Win32::Foundation::SYSTEMTIME;
    use windows_sys::Win32::System::SystemInformation::{GetLocalTime, GetSystemTime};
    let z = SYSTEMTIME { wYear: 0, wMonth: 0, wDayOfWeek: 0, wDay: 0, wHour: 0, wMinute: 0, wSecond: 0, wMilliseconds: 0 };
    let (mut l, mut u) = (z, z);
    unsafe {
        GetLocalTime(&mut l);
        GetSystemTime(&mut u);
    }
    let mins = |t: &SYSTEMTIME| t.wDay as i64 * 1440 + t.wHour as i64 * 60 + t.wMinute as i64;
    let mut d = mins(&l) - mins(&u);
    // across the end of a month one of them is on day 1
    if d > 720 {
        d -= 1440 * (l.wDay as i64 - 1);
    } else if d < -720 {
        d += 1440 * (u.wDay as i64 - 1);
    }
    (d.clamp(-14 * 60, 14 * 60)) * 60
}

fn esc(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

/// A page of the browser's own with a heading, a box that searches it, and rows.
fn page(title: &str, action: &str, find: &str, hint: &str, rows: &str, extra: &str) -> String {
    let s = sk();
    let hex = |c: Rgba| format!("#{:02x}{:02x}{:02x}", c[0], c[1], c[2]);
    format!(
        r#"<!doctype html><html><head><title>{title}</title><style>
html {{ background: {page}; color: {text}; font-family: "Inter", "Segoe UI", sans-serif; font-size: 14px; }}
body {{ margin: 0; }}
main {{ max-width: 720px; margin: 0 auto; padding: 48px 28px 90px; }}
h1 {{ font-family: "Cinzel", Georgia, serif; font-weight: 400; font-size: 22px; letter-spacing: 2px; margin: 0 0 18px; }}
form {{ margin: 0 0 18px; }}
input {{ width: 100%; box-sizing: border-box; font-size: 14px; padding: 9px 12px; border: 1px solid {line}; border-radius: 9px; background: {field}; color: {text}; }}
h2 {{ font-family: "Cinzel", Georgia, serif; font-size: 12.5px; font-weight: 700; letter-spacing: 1.6px; color: {muted}; margin: 26px 0 4px; }}
.row {{ display: flex; align-items: center; padding: 9px 0; border-bottom: 1px solid {line}; }}
.row .go {{ flex: 1 1 auto; min-width: 0; color: {text}; text-decoration: none; }}
.row b {{ display: block; font-weight: 500; }}
.row span {{ display: block; font-size: 12.5px; color: {muted}; margin-top: 2px; }}
.row i {{ flex: 0 0 auto; font-style: normal; font-size: 12px; color: {muted}; padding-left: 16px; }}
.row a.x {{ flex: 0 0 auto; font-size: 12.5px; color: {muted}; padding-left: 16px; text-decoration: none; }}
.btn {{ display: inline-block; font-size: 13px; padding: 6px 12px; border: 1px solid {line}; border-radius: 8px; color: {text}; text-decoration: none; margin-top: 24px; }}
.none {{ color: {muted}; }}
</style></head><body><main><h1>{heading}</h1><form action="{action}"><input name=q value="{find}" placeholder="{hint}"></form>{rows}{extra}</main></body></html>"#,
        heading = title.to_uppercase(),
        page = hex(if s.light { [0xfb, 0xfa, 0xf7, 255] } else { s.field }),
        field = hex(s.field),
        text = hex(s.text),
        muted = hex(s.muted),
        line = hex(s.hairline),
        find = esc(find),
    )
}

fn row(url: &str, title: &str, side: &str, remove: Option<String>) -> String {
    let shown = if title.is_empty() { url.trim_start_matches("https://") } else { title };
    let x = remove.map_or(String::new(), |r| format!("<a class=x href=\"{r}\">Remove</a>"));
    format!(
        "<div class=row><a class=go href=\"{}\"><b>{}</b><span>{}</span></a><i>{}</i>{x}</div>",
        esc(url),
        esc(shown),
        esc(url.trim_start_matches("https://")),
        esc(side)
    )
}

/// The day a unix time falls on, in words: today, yesterday, or the date.
fn day_name(t: u64, today: u64) -> String {
    let d = t / 86_400;
    match today / 86_400 - d {
        0 => "TODAY".into(),
        1 => "YESTERDAY".into(),
        _ => {
            let (y, m, dd) = crate::adapters::civil_from_days(d as i64);
            const M: [&str; 12] = ["JANUARY", "FEBRUARY", "MARCH", "APRIL", "MAY", "JUNE", "JULY", "AUGUST", "SEPTEMBER", "OCTOBER", "NOVEMBER", "DECEMBER"];
            format!("{} {} {y}", dd, M[(m as usize).saturating_sub(1).min(11)])
        }
    }
}

pub(super) fn history_html(find: &str) -> String {
    let off = local_offset();
    let local = |t: u64| (t as i64 + off).max(0) as u64;
    let now = local(crate::sys::now_unix());
    let rows = with(|p| {
        let mut list: Vec<&Place> = p.history.iter().filter(|v| words_in(find, &v.url, &v.title)).collect();
        list.sort_by_key(|v| std::cmp::Reverse(v.last));
        let mut out = String::new();
        let mut last_day = String::new();
        for v in list.into_iter().take(400) {
            let at = local(v.last);
            let day = day_name(at, now);
            if day != last_day {
                out.push_str(&format!("<h2>{day}</h2>"));
                last_day = day;
            }
            let secs = at % 86_400;
            out.push_str(&row(&v.url, &v.title, &format!("{:02}:{:02}", secs / 3600, secs / 60 % 60), None));
        }
        out
    });
    let rows = if rows.is_empty() { "<p class=none>Nothing here yet.</p>".to_string() } else { rows };
    page("History", "https://kamaitachi.local/history", find, "Search history", &rows, "<a class=btn href=\"https://kamaitachi.local/clearhistory\">Clear all history</a>")
}

pub(super) fn bookmarks_html(find: &str) -> String {
    let rows = with(|p| {
        p.marks
            .iter()
            .rev()
            .filter(|m| words_in(find, &m.0, &m.1))
            .map(|(u, t)| row(u, t, "", Some(format!("https://kamaitachi.local/unmark?u={}", crate::url::encode(u)))))
            .collect::<String>()
    });
    let rows = if rows.is_empty() { "<p class=none>No bookmarks yet. Ctrl+D keeps the page you are on.</p>".to_string() } else { rows };
    page("Bookmarks", "https://kamaitachi.local/bookmarks", find, "Search bookmarks", &rows, "")
}

/// Your Downloads folder.
fn downloads_dir() -> std::path::PathBuf {
    let home = std::env::var("USERPROFILE").unwrap_or_else(|_| ".".into());
    std::path::Path::new(&home).join("Downloads")
}

/// Writes a file into Downloads under a name not taken yet, "report (2).pdf" if
/// "report.pdf" is, and notes it in downloads.txt. Returns where it went.
pub(super) fn keep_download(name: &str, body: &[u8], from: &str) -> Result<std::path::PathBuf, String> {
    let dir = downloads_dir();
    let _ = std::fs::create_dir_all(&dir);
    let (stem, ext) = match name.rsplit_once('.') {
        Some((a, b)) if !a.is_empty() => (a.to_string(), format!(".{b}")),
        _ => (name.to_string(), String::new()),
    };
    let mut path = dir.join(name);
    let mut n = 1;
    while path.exists() {
        n += 1;
        path = dir.join(format!("{stem} ({n}){ext}"));
    }
    std::fs::write(&path, body).map_err(|e| format!("could not save {name}: {e}"))?;
    let line = format!("{}\t{}\t{}\t{}\n", crate::sys::now_unix(), body.len(), path.display(), clean(from));
    use std::io::Write;
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(file("downloads.txt")) {
        let _ = f.write_all(line.as_bytes());
    }
    Ok(path)
}

/// Opens a downloaded file with its program, or shows it in its folder. Only files in
/// the downloads list, so a page cannot ask for anything else to be run.
pub(super) fn open_file(path: &str, folder: bool) {
    let text = std::fs::read_to_string(file("downloads.txt")).unwrap_or_default();
    if !text.lines().any(|l| l.split('\t').nth(2) == Some(path)) {
        return;
    }
    use windows::core::HSTRING;
    use windows::Win32::UI::Shell::ShellExecuteW;
    use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;
    unsafe {
        if folder {
            let args = HSTRING::from(format!("/select,\"{path}\""));
            ShellExecuteW(None, &HSTRING::from("open"), &HSTRING::from("explorer.exe"), &args, None, SW_SHOWNORMAL);
        } else {
            ShellExecuteW(None, &HSTRING::from("open"), &HSTRING::from(path), None, None, SW_SHOWNORMAL);
        }
    }
}

fn size_words(n: u64) -> String {
    match n {
        0..1024 => format!("{n} B"),
        1024..1_048_576 => format!("{:.0} KB", n as f64 / 1024.0),
        _ => format!("{:.1} MB", n as f64 / 1_048_576.0),
    }
}

pub(super) fn downloads_html(find: &str) -> String {
    let text = std::fs::read_to_string(file("downloads.txt")).unwrap_or_default();
    let mut rows = String::new();
    for l in text.lines().rev().take(300) {
        let mut f = l.splitn(4, '\t');
        let (Some(_), Some(size), Some(path), from) = (f.next(), f.next(), f.next(), f.next().unwrap_or("")) else { continue };
        let name = std::path::Path::new(path).file_name().map_or(path.to_string(), |n| n.to_string_lossy().into_owned());
        if !words_in(find, &name, from) {
            continue;
        }
        let gone = !std::path::Path::new(path).exists();
        let p = crate::url::encode(path);
        let acts = if gone {
            "<i>moved or deleted</i>".to_string()
        } else {
            format!("<a class=x href=\"https://kamaitachi.local/openfile?f={p}\">Open</a><a class=x href=\"https://kamaitachi.local/showfile?f={p}\">Show in folder</a>")
        };
        rows.push_str(&format!(
            "<div class=row><div class=go><b>{}</b><span>{} · {}</span></div>{acts}</div>",
            esc(&name),
            size_words(size.parse().unwrap_or(0)),
            esc(from.trim_start_matches("https://"))
        ));
    }
    let rows = if rows.is_empty() { "<p class=none>Files you download show up here.</p>".to_string() } else { rows };
    page("Downloads", "https://kamaitachi.local/downloads", find, "Search downloads", &rows, "")
}

impl App {
    /// Bookmarks the page in front, or takes the bookmark away.
    pub(super) fn bookmark(&mut self) {
        let (url, title) = (self.tab().url.clone(), self.tab().title.clone());
        if !worth(&url) {
            return self.say("this page can't be bookmarked");
        }
        let on = toggle_mark(&url, &title);
        self.say(if on { "bookmarked" } else { "bookmark taken away" });
    }

    /// The browser's own page `name` (history, bookmarks) in tab `i`.
    pub(super) fn show_place(&mut self, i: usize, name: &str, find: &str, push: bool) {
        let t = &mut self.tabs[i];
        let url = format!("https://kamaitachi.local/{name}");
        if push && !t.url.is_empty() && !t.url.starts_with(&url) {
            t.back.push((t.url.clone(), t.scroll));
            t.fwd.clear();
        }
        t.full_mode = false;
        t.url = if find.is_empty() { url } else { format!("{url}?q={}", crate::url::encode(find)) };
        let (html, title) = match name {
            "history" => (history_html(find), "History"),
            "downloads" => (downloads_html(find), "Downloads"),
            _ => (bookmarks_html(find), "Bookmarks"),
        };
        self.show_html(i, &html, title);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn words_match_in_any_order() {
        assert!(words_in("rust blog", "https://blog.rust-lang.org/x", "Announcing Rust"));
        assert!(!words_in("rust python", "https://blog.rust-lang.org/x", "Announcing Rust"));
        assert!(words_in("", "https://a.com", ""));
    }

    #[test]
    fn days_have_names() {
        let now = 1_800_000_000;
        assert_eq!(day_name(now, now), "TODAY");
        assert_eq!(day_name(now - 86_400, now), "YESTERDAY");
        assert_eq!(day_name(0, now), "1 JANUARY 1970");
    }
}
