//! The settings page, at kamaitachi.local/settings. It is a page like any other, laid
//! out by the light engine: every choice is a link to kamaitachi.local/set, which only
//! pages the browser made itself may follow, and the box at the top finds a setting by
//! the words in it.

use super::prefs::{self, SEARCH};
use super::skin::ACCENTS;
use super::*;

/// One row: its key, title, a line about it, and its choices as (value, label).
struct Row {
    key: &'static str,
    title: &'static str,
    detail: &'static str,
    choices: Vec<(String, String)>,
}

fn onoff() -> Vec<(String, String)> {
    vec![("on".into(), "On".into()), ("off".into(), "Off".into())]
}

fn pairs(v: &[(&str, &str)]) -> Vec<(String, String)> {
    v.iter().map(|(a, b)| (a.to_string(), b.to_string())).collect()
}

fn row(key: &'static str, title: &'static str, detail: &'static str, choices: Vec<(String, String)>) -> Row {
    Row { key, title, detail, choices }
}

fn sections() -> Vec<(&'static str, Vec<Row>)> {
    let accents: Vec<(String, String)> = ACCENTS.iter().enumerate().map(|(i, a)| (i.to_string(), a.0.to_string())).collect();
    let engines: Vec<(String, String)> = SEARCH.iter().enumerate().map(|(i, e)| (i.to_string(), e.0.to_string())).collect();
    vec![
        (
            "Appearance",
            vec![
                row("theme", "Light and dark", "Marble by day, the night sea in the dark, or whatever Windows uses", pairs(&[("light", "Marble"), ("dark", "Night"), ("system", "Like Windows")])),
                row("accent", "Accent", "The colour of focus, selection and pages loading", accents),
                row("side", "Tabs", "The column of tabs on the left of the page, or on the right", pairs(&[("left", "Left"), ("right", "Right")])),
                row("column", "Column width", "How wide the column of tabs is. Ctrl+S puts it away", pairs(&[("200", "Narrow"), ("232", "Normal"), ("280", "Wide"), ("340", "Wider")])),
                row("roomy", "Roomier tabs", "Taller rows and larger titles in the column", onoff()),
                row("card", "The page as a card", "A little room around the page, with rounded corners. Off, it goes edge to edge", onoff()),
                row("address", "Always show the address", "The page's address at the top of the column. Off, Ctrl+L brings it up", onoff()),
                row("loading_line", "Loading line", "A thin line along the top of a page while it loads", onoff()),
                row("read_fill", "Show how far you have read", "A tab fills with colour as you read down its page", onoff()),
                row("dim_sleeping", "Dim sleeping tabs", "A tab that has let its page go fades until you go to it", onoff()),
                row("tab_numbers", "Tab numbers while Ctrl is held", "Which tab each Ctrl+1 to Ctrl+9 picks", onoff()),
                row("contrast", "High contrast text", "Tab titles in full black or white, instead of grey", onoff()),
                row("meander", "Greek key", "A band of the running meander along the foot of the column", onoff()),
                row("veins", "Marble veins", "Faint veins through the marble of the column", onoff()),
            ],
        ),
        (
            "Tabs",
            vec![
                row("restore", "Bring back last time's tabs", "Each one loads only when you go to it, so they cost nothing until then", onoff()),
                row("sleep_after", "Sleep tabs you aren't using", "A resting tab lets its page go and comes back where you left it", pairs(&[("0", "Never"), ("10", "10 min"), ("30", "30 min"), ("60", "1 hour")])),
                row("close_after", "Close tabs you leave alone", "Ctrl+Shift+T brings one back", pairs(&[("0", "Never"), ("12", "12 hours"), ("24", "A day"), ("168", "A week")])),
                row("close_goes_back", "Closing a tab goes back to the one before", "Off, you land on the tab beside it", onoff()),
                row("beside", "Links open beside their page", "A link opened in a new tab goes right after the tab it came from", onoff()),
            ],
        ),
        (
            "Browsing",
            vec![
                row("search", "Search with", "Where words typed in the address field go", engines),
                row("zoom", "Page zoom", "Where every page starts. Ctrl+ and Ctrl- change it", pairs(&[("90", "90%"), ("100", "100%"), ("110", "110%"), ("125", "125%"), ("150", "150%")])),
                row("smooth", "Smooth scrolling", "Each notch of the wheel glides over a few frames instead of jumping", onoff()),
                row("backspace_back", "Backspace goes back", "Backspace goes back a page, as older browsers did", onoff()),
            ],
        ),
        (
            "Privacy",
            vec![
                row("block_ads", "Block ads and trackers", "Stopped before they load, in both engines", onoff()),
                row("clean_links", "Remove click trackers from links", "Takes tracking codes like utm_source, gclid and fbclid out of addresses", onoff()),
                row("gpc", "Tell sites not to sell or share your data", "Sends Global Privacy Control with every page", onoff()),
            ],
        ),
    ]
}

/// The keys, listed so the find box can find them too.
const KEYS: &[(&str, &str)] = &[
    ("Ctrl+L or Ctrl+K", "go somewhere, or to an open tab"),
    ("Ctrl+T, Ctrl+W", "new tab, close tab"),
    ("Ctrl+Shift+T", "open the last closed tab again"),
    ("Ctrl+Tab, Ctrl+1 to 9", "next tab, a tab by number"),
    ("Ctrl+S", "put the column of tabs away, or bring it back"),
    ("Ctrl+Shift+E", "this site in the other engine"),
    ("Ctrl+Shift+H", "hide a box on this site for good"),
    ("Ctrl+Shift+R", "reading view"),
    ("Ctrl+M", "mute a page in Chromium"),
    ("Ctrl+,", "these settings"),
];

fn esc(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

/// The page, showing only what matches `find` when there is something in it.
pub(super) fn html(find: &str) -> String {
    let s = sk();
    let p = prefs::get();
    let q = find.trim().to_lowercase();
    let matches = |t: &str, d: &str| q.is_empty() || q.split_whitespace().all(|w| t.to_lowercase().contains(w) || d.to_lowercase().contains(w));
    let mut body = String::new();
    for (name, rows) in sections() {
        let rows: Vec<&Row> = rows.iter().filter(|r| matches(r.title, r.detail) || (r.key == "search" && matches("search engine google bing", ""))).collect();
        if rows.is_empty() {
            continue;
        }
        body.push_str(&format!("<h2>{name}</h2><div class=group>"));
        for r in rows {
            let now = p.value(r.key);
            let mut choices = String::new();
            for (v, label) in &r.choices {
                let swatch = if r.key == "accent" {
                    let c = ACCENTS[v.parse::<usize>().unwrap_or(0)];
                    let c = if s.light { c.1 } else { c.2 };
                    format!("<i style=\"background:#{:02x}{:02x}{:02x}\"></i>", c[0], c[1], c[2])
                } else {
                    String::new()
                };
                if *v == now {
                    choices.push_str(&format!("<b class=c>{swatch}{}</b>", esc(label)));
                } else {
                    choices.push_str(&format!("<a class=c href=\"https://kamaitachi.local/set?{}={}\">{swatch}{}</a>", r.key, v, esc(label)));
                }
            }
            body.push_str(&format!("<div class=row><div class=t><b>{}</b><span>{}</span></div><div class=cs>{choices}</div></div>", esc(r.title), esc(r.detail)));
        }
        body.push_str("</div>");
    }
    let keys: Vec<&(&str, &str)> = KEYS.iter().filter(|(k, d)| matches(k, d)).collect();
    if !keys.is_empty() {
        body.push_str("<h2>Keys</h2><div class=group>");
        for (k, d) in keys {
            body.push_str(&format!("<div class=row><div class=t><b>{}</b><span>{}</span></div></div>", esc(k), esc(d)));
        }
        body.push_str("</div>");
    }
    let kept = crate::net::cookies::sites();
    if q.is_empty() || matches("cookies sign-ins forget clear", "") {
        body.push_str(&format!(
            "<h2>Clear</h2><div class=group><div class=row><div class=t><b>Cookies and sign-ins</b><span>Kept for {kept} site{}. Chromium keeps its own</span></div><div class=cs><a class=c href=\"https://kamaitachi.local/forget\">Sign out of everything</a></div></div></div>",
            if kept == 1 { "" } else { "s" }
        ));
    }
    if body.is_empty() {
        body = format!("<p class=none>No setting has \u{201c}{}\u{201d} in it.</p>", esc(find.trim()));
    }
    let hex = |c: Rgba| format!("#{:02x}{:02x}{:02x}", c[0], c[1], c[2]);
    format!(
        r#"<!doctype html><html><head><title>Settings</title><style>
html {{ background: {bg}; color: {text}; font-family: "Segoe UI Variable Text", "Segoe UI", sans-serif; }}
body {{ margin: 0; }}
main {{ max-width: 760px; margin: 0 auto; padding: 44px 32px 90px; }}
h1 {{ font-family: "CMU Serif", Georgia, serif; font-weight: 400; font-size: 30px; letter-spacing: 3px; margin: 0 0 18px; }}
form {{ margin: 0 0 26px; }}
input {{ width: 100%; box-sizing: border-box; font-size: 15px; padding: 9px 12px; border: 1px solid {line}; border-radius: 9px; background: {field}; color: {text}; }}
h2 {{ font-size: 12px; font-weight: 600; letter-spacing: 1.5px; text-transform: uppercase; color: {muted}; margin: 30px 0 8px 4px; }}
.group {{ background: {field}; border: 1px solid {line}; border-radius: 12px; }}
.row {{ display: flex; align-items: center; padding: 12px 16px; border-top: 1px solid {line}; }}
.row:first-child {{ border-top: 0; }}
.t {{ flex: 1 1 auto; padding-right: 16px; }}
.t b {{ display: block; font-size: 14px; font-weight: 500; }}
.t span {{ display: block; font-size: 12.5px; color: {muted}; margin-top: 2px; }}
.cs {{ flex: 0 0 auto; text-align: right; max-width: 380px; }}
.c {{ display: inline-block; font-size: 12.5px; font-weight: 400; padding: 4px 10px; margin: 2px 0 2px 4px; border: 1px solid {line}; border-radius: 13px; color: {text}; text-decoration: none; }}
b.c {{ background: {accent}; border-color: {accent}; color: {on_accent}; }}
.c i {{ display: inline-block; width: 9px; height: 9px; border-radius: 5px; margin-right: 6px; }}
.none {{ color: {muted}; }}
</style></head><body><main><h1>SETTINGS</h1><form action="https://kamaitachi.local/settings"><input name=q value="{find}" placeholder="Find a setting" autofocus></form>{body}</main></body></html>"#,
        bg = hex(s.frame),
        field = hex(s.field),
        text = hex(s.text),
        muted = hex(s.muted),
        line = hex(s.hairline),
        accent = hex(s.accent),
        on_accent = if s.light { "#ffffff" } else { "#10141a" },
        find = esc(find),
    )
}

impl App {
    pub(super) fn show_settings(&mut self, i: usize, find: &str, push: bool) {
        let t = &mut self.tabs[i];
        if push && !t.url.is_empty() && !t.url.starts_with(SETTINGS) {
            t.back.push((t.url.clone(), t.scroll));
            t.fwd.clear();
        }
        let keep = if t.url.starts_with(SETTINGS) { t.scroll } else { 0.0 };
        t.full_mode = false;
        t.url = if find.is_empty() { SETTINGS.into() } else { format!("{SETTINGS}?q={}", crate::url::encode(find)) };
        self.show_html(i, &html(find), "Settings");
        let t = &mut self.tabs[i];
        t.scroll = keep;
        t.target = keep;
    }

    /// Takes in a choice from the settings page: `key=value`.
    pub(super) fn take_setting(&mut self, query: &str) {
        let Some((k, v)) = query.split_once('=') else { return };
        if prefs::set(k, &crate::url::percent_decode(v)) {
            self.apply_prefs();
        }
    }

    /// Puts the settings into effect.
    pub(super) fn apply_prefs(&mut self) {
        let p = prefs::get();
        skin::refresh();
        crate::url::set_search(SEARCH[p.search].1);
        crate::web::block::set_on(p.block_ads);
        crate::web::load::set_gpc(p.gpc);
        self.frame_colors();
        self.wall = None;
        self.wall_checked = false;
        if self.tabs.is_empty() {
            return;
        }
        self.sync_size();
        self.relayout_due = true;
        self.relayout();
        self.update_views();
    }

    /// The window's own title bar in the marble's colour, dark when the look is.
    pub(super) fn frame_colors(&self) {
        let s = sk();
        let dark: BOOL = (!s.light).into();
        let cap = COLORREF((s.frame[0] as u32) | (s.frame[1] as u32) << 8 | (s.frame[2] as u32) << 16);
        let text = COLORREF((s.muted[0] as u32) | (s.muted[1] as u32) << 8 | (s.muted[2] as u32) << 16);
        unsafe {
            let _ = DwmSetWindowAttribute(self.hwnd, DWMWA_USE_IMMERSIVE_DARK_MODE, &dark as *const BOOL as *const _, 4);
            let _ = DwmSetWindowAttribute(self.hwnd, DWMWA_CAPTION_COLOR, &cap as *const COLORREF as *const _, 4);
            let _ = DwmSetWindowAttribute(self.hwnd, DWMWA_TEXT_COLOR, &text as *const COLORREF as *const _, 4);
            let _ = DwmSetWindowAttribute(self.hwnd, DWMWA_BORDER_COLOR, &cap as *const COLORREF as *const _, 4);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finding_a_setting_keeps_only_what_matches() {
        let all = html("");
        assert!(all.contains("Roomier tabs") && all.contains("Search with") && all.contains("Ctrl+Shift+T"));
        let few = html("zoom");
        assert!(few.contains("Page zoom") && !few.contains("Roomier tabs"));
        assert!(html("google").contains("Search with"));
        assert!(html("weasel stamps").contains("No setting has"));
        // a choice that is in use is not a link
        assert!(all.contains("<b class=c>Marble</b>") || all.contains("<b class=c>Like Windows</b>"));
    }
}
