//! The settings page, at kamaitachi.local/settings. It is a page like any other, laid
//! out by the light engine: sections down the left, the settings of one section on the
//! right, and a box above the sections that finds a setting by its words.
//!
//! Every change is an address under kamaitachi.local/set, which only pages the browser
//! made itself may open. A switch is a link to it. A list is a form, and on these pages
//! a form with `data-auto` goes as soon as something is picked in its list.

use super::prefs::{self, SEARCH};
use super::skin::{self, Skin};
use super::*;

enum Kind {
    Switch,
    List(Vec<(String, String)>),
    Themes,
    Accents,
}

struct Row {
    key: &'static str,
    title: &'static str,
    detail: &'static str,
    kind: Kind,
}

fn list(v: &[(&str, &str)]) -> Kind {
    Kind::List(v.iter().map(|(a, b)| (a.to_string(), b.to_string())).collect())
}

fn row(key: &'static str, title: &'static str, detail: &'static str, kind: Kind) -> Row {
    Row { key, title, detail, kind }
}

/// (id, name, rows)
fn sections() -> Vec<(&'static str, &'static str, Vec<Row>)> {
    let engines = Kind::List(SEARCH.iter().enumerate().map(|(i, e)| (i.to_string(), e.0.to_string())).collect());
    vec![
        (
            "look",
            "Appearance",
            vec![
                row("theme", "Theme", "", Kind::Themes),
                row("accent", "Accent", "The colour of focus, selection and pages loading", Kind::Accents),
                row("glass", "Glass", "See what is behind the window, blurred, through the tabs and the frame", Kind::Switch),
                row("tint", "Transparency", "How much of the marble lies on the glass", list(&[("30", "Clear"), ("45", "Light"), ("60", "Frosted"), ("80", "Nearly solid")])),
                row("side", "Tabs", "The column of tabs beside the page", list(&[("left", "On the left"), ("right", "On the right")])),
                row("column", "Column width", "Ctrl+S puts the column away", list(&[("200", "Narrow"), ("232", "Normal"), ("280", "Wide"), ("340", "Wider")])),
                row("autohide", "Hide the column until the mouse reaches the edge", "The page takes the whole window, and the tabs slide out over it", Kind::Switch),
                row("darken", "Darken pages that stay light", "In Night, pages with no dark look of their own are darkened too", Kind::Switch),
                row("roomy", "Roomier tabs", "Taller rows and larger titles in the column", Kind::Switch),
                row("card", "The page as a card", "A little room around the page, with rounded corners", Kind::Switch),
                row("address", "Always show the address", "At the top of the column. Off, Ctrl+L brings it up", Kind::Switch),
                row("loading_line", "Loading line", "A thin line along the top of a page while it loads", Kind::Switch),
                row("read_fill", "Show how far you have read", "A tab fills with grey as you read down its page", Kind::Switch),
                row("dim_sleeping", "Dim sleeping tabs", "A tab that has let its page go fades until you go to it", Kind::Switch),
                row("tab_numbers", "Tab numbers while Ctrl is held", "Which tab each Ctrl+1 to Ctrl+9 picks", Kind::Switch),
                row("contrast", "High contrast text", "Tab titles in full black or white, instead of grey", Kind::Switch),
                row("meander", "Greek key", "A band of the running meander along the foot of the column", Kind::Switch),
                row("veins", "Marble veins", "Faint veins through the marble, when it is not glass", Kind::Switch),
            ],
        ),
        (
            "tabs",
            "Tabs",
            vec![
                row("restore", "Bring back last time's tabs", "Each one loads only when you go to it", Kind::Switch),
                row("sleep_after", "Sleep tabs you aren't using", "A resting tab lets its page go and comes back where you left it", list(&[("0", "Never"), ("10", "After 10 minutes"), ("30", "After 30 minutes"), ("60", "After an hour")])),
                row("close_after", "Close tabs you leave alone", "Ctrl+Shift+T brings one back", list(&[("0", "Never"), ("12", "After 12 hours"), ("24", "After a day"), ("168", "After a week")])),
                row("close_goes_back", "Closing a tab goes back to the one before", "Off, you land on the tab beside it", Kind::Switch),
                row("keep_window", "Keep the window when its last tab closes", "Off, closing the last tab closes the window", Kind::Switch),
                row("beside", "Links open beside their page", "A link opened in a new tab goes right after the tab it came from", Kind::Switch),
            ],
        ),
        (
            "browsing",
            "Browsing",
            vec![
                row("search", "Search with", "Where words typed in the address field go", engines),
                row("zoom", "Page zoom", "Where every page starts. Ctrl+ and Ctrl- change it", list(&[("90", "90%"), ("100", "100%"), ("110", "110%"), ("125", "125%"), ("150", "150%")])),
                row("smooth", "Smooth scrolling", "Each notch of the wheel glides over a few frames instead of jumping", Kind::Switch),
                row("backspace_back", "Backspace goes back", "As older browsers did", Kind::Switch),
            ],
        ),
        (
            "privacy",
            "Privacy",
            vec![
                row("block_ads", "Block ads and trackers", "Stopped before they load, in both engines", Kind::Switch),
                row("clean_links", "Remove click trackers from links", "Takes codes like utm_source, gclid and fbclid out of addresses", Kind::Switch),
                row("gpc", "Tell sites not to sell or share your data", "Sends Global Privacy Control with every page", Kind::Switch),
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
    ("Ctrl+F", "find on this page"),
    ("Shift+click a link", "peek at it in a card over the page"),
    ("Right-click a tab", "pin, duplicate, mute, sleep, close others"),
    ("Ctrl+Shift+C, Ctrl+Shift+V", "copy the address, paste an address and go"),
    ("Ctrl+Z", "open the last closed tab again"),
    ("Ctrl+D", "bookmark this page"),
    ("Ctrl+H", "history"),
    ("Ctrl+Shift+O", "bookmarks"),
    ("Ctrl+J", "downloads"),
    ("Ctrl+,", "these settings"),
];

fn esc(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

fn hex(c: Rgba) -> String {
    format!("#{:02x}{:02x}{:02x}", c[0], c[1], c[2])
}

fn set_link(key: &str, value: &str, section: &str) -> String {
    format!("https://kamaitachi.local/set?{key}={value}&amp;s={section}")
}

/// A small picture of the window in one theme: the column, three tab lines, and the card.
fn theme_card(value: &str, name: &str, look: (Rgba, Rgba, Rgba), on: bool, section: &str) -> String {
    let (frame, card, line) = look;
    let inner = format!(
        "<div class=mini style=\"background:{f}\"><div class=mc><i style=\"background:{l}\"></i><i style=\"background:{l};width:60%\"></i><i style=\"background:{l};width:75%\"></i></div><div class=mk style=\"background:{c}\"></div></div><span>{name}</span>",
        f = hex(frame),
        c = hex(card),
        l = hex(line),
    );
    if on {
        format!("<div class=\"theme on\">{inner}</div>")
    } else {
        format!("<a class=theme href=\"{}\">{inner}</a>", set_link("theme", value, section))
    }
}

fn control(r: &Row, p: &prefs::Prefs, s: &Skin, section: &str) -> String {
    let now = p.value(r.key);
    match &r.kind {
        Kind::Switch => {
            let on = now == "on";
            format!("<a class=\"sw{}\" href=\"{}\"><i></i></a>", if on { " on" } else { "" }, set_link(r.key, if on { "off" } else { "on" }, section))
        }
        Kind::List(choices) => {
            let opts: String = choices.iter().map(|(v, l)| format!("<option value=\"{v}\"{}>{}</option>", if *v == now { " selected" } else { "" }, esc(l))).collect();
            format!("<form action=\"https://kamaitachi.local/set\" data-auto><input type=hidden name=s value={section}><select name={}>{opts}</select></form>", r.key)
        }
        Kind::Accents => skin::accents()
            .iter()
            .enumerate()
            .map(|(i, a)| {
                let c = hex(if s.light { a.1 } else { a.2 });
                if i.to_string() == now {
                    format!("<b class=\"dot on\" style=\"border-color:{c}\"><i style=\"background:{c}\"></i></b>")
                } else {
                    format!("<a class=dot href=\"{}\"><i style=\"background:{c}\"></i></a>", set_link("accent", &i.to_string(), section))
                }
            })
            .collect(),
        Kind::Themes => String::new(),
    }
}

/// The page: one section, or everything that matches `find`.
pub(super) fn html(find: &str, section: &str) -> String {
    let s = sk();
    let p = prefs::get();
    let q = find.trim().to_lowercase();
    let all = sections();
    let section = if all.iter().any(|x| x.0 == section) || section == "keys" { section } else { "look" };
    let matches = |t: &str, d: &str| q.split_whitespace().all(|w| t.to_lowercase().contains(w) || d.to_lowercase().contains(w));
    let mut nav = String::new();
    for (id, name) in all.iter().map(|x| (x.0, x.1)).chain([("keys", "Keys")]) {
        if q.is_empty() && id == section {
            nav.push_str(&format!("<b class=nav>{name}</b>"));
        } else {
            nav.push_str(&format!("<a class=nav href=\"https://kamaitachi.local/settings?s={id}\">{name}</a>"));
        }
    }
    let mut body = String::new();
    for (id, name, rows) in &all {
        let wanted = |r: &&Row| {
            if q.is_empty() {
                return *id == section;
            }
            matches(r.title, r.detail) || (r.key == "search" && matches("search engine google bing duckduckgo", "")) || (r.key == "theme" && matches("theme light dark night marble", ""))
        };
        let rows: Vec<&Row> = rows.iter().filter(wanted).collect();
        if rows.is_empty() {
            continue;
        }
        body.push_str(&format!("<h2>{}</h2>", name.to_uppercase()));
        for r in rows {
            if let Kind::Themes = r.kind {
                let now = p.value("theme");
                let (m, n) = (skin::preview(true), skin::preview(false));
                body.push_str("<div class=themes>");
                body.push_str(&theme_card("light", "Marble", m, now == "light", id));
                body.push_str(&theme_card("dark", "Night", n, now == "dark", id));
                body.push_str(&theme_card("system", "Like Windows", if s.light { m } else { n }, now == "system", id));
                body.push_str("</div>");
                continue;
            }
            let detail = if r.detail.is_empty() { String::new() } else { format!("<span>{}</span>", esc(r.detail)) };
            body.push_str(&format!("<div class=row><div class=t><b>{}</b>{detail}</div><div class=cs>{}</div></div>", esc(r.title), control(r, &p, &s, id)));
        }
    }
    let keys: Vec<&(&str, &str)> = KEYS.iter().filter(|(k, d)| if q.is_empty() { section == "keys" } else { matches(k, d) }).collect();
    if !keys.is_empty() {
        body.push_str("<h2>KEYS</h2>");
        for (k, d) in keys {
            body.push_str(&format!("<div class=row><div class=t><b>{}</b></div><div class=cs><span class=k>{}</span></div></div>", esc(d), esc(k)));
        }
    }
    if (q.is_empty() && section == "privacy") || (!q.is_empty() && matches("cookies sign-ins sign out forget clear", "")) {
        let kept = crate::net::cookies::sites();
        body.push_str(&format!(
            "<div class=row><div class=t><b>Cookies and sign-ins</b><span>Kept for {kept} site{}. Chromium keeps its own</span></div><div class=cs><a class=btn href=\"https://kamaitachi.local/forget\">Sign out of everything</a></div></div>",
            if kept == 1 { "" } else { "s" }
        ));
    }
    if body.is_empty() {
        body = format!("<p class=none>No setting has \u{201c}{}\u{201d} in it.</p>", esc(find.trim()));
    }
    let on_accent = hex(super::colour::on_accent(s.light));
    format!(
        r#"<!doctype html><html><head><title>Settings</title><style>
html {{ background: {page}; color: {text}; font-family: "Inter", "Segoe UI", sans-serif; font-size: 14px; }}
body {{ margin: 0; }}
.wrap {{ display: flex; max-width: 900px; margin: 0 auto; padding: 48px 28px 90px; }}
aside {{ flex: 0 0 196px; padding-right: 36px; }}
main {{ flex: 1 1 auto; min-width: 0; }}
h1 {{ font-family: "Cinzel", Georgia, serif; font-weight: 400; font-size: 22px; letter-spacing: 2px; margin: 2px 0 20px 10px; }}
form {{ margin: 0; }}
aside input {{ width: 100%; box-sizing: border-box; font-size: 13px; padding: 7px 10px; border: 1px solid {line}; border-radius: 8px; background: {field}; color: {text}; margin-bottom: 14px; }}
.nav {{ display: block; padding: 7px 10px; margin: 1px 0; border-radius: 7px; color: {muted}; text-decoration: none; font-weight: 400; }}
b.nav {{ background: {wash}; color: {text}; font-weight: 500; }}
h2 {{ font-family: "Cinzel", Georgia, serif; font-size: 12.5px; font-weight: 700; letter-spacing: 1.6px; color: {muted}; margin: 4px 0 6px; }}
.gap {{ height: 28px; }}
.row {{ display: flex; align-items: center; padding: 13px 0; border-bottom: 1px solid {line}; }}
.t {{ flex: 1 1 auto; padding-right: 20px; }}
.t b {{ display: block; font-weight: 500; font-size: 14px; }}
.t span {{ display: block; font-size: 12.5px; color: {muted}; margin-top: 3px; }}
.cs {{ flex: 0 0 auto; text-align: right; }}
select {{ font-size: 13px; padding: 5px 10px; border: 1px solid {line}; border-radius: 8px; background: {field}; color: {text}; }}
.sw {{ display: inline-block; width: 36px; height: 20px; border-radius: 10px; background: {line}; }}
.sw i {{ display: block; width: 16px; height: 16px; border-radius: 8px; background: {knob}; margin: 2px; }}
.sw.on {{ background: {accent}; }}
.sw.on i {{ margin-left: 18px; background: {on_accent}; }}
.dot {{ display: inline-block; width: 18px; height: 18px; padding: 2px; margin-left: 6px; border: 2px solid {page}; border-radius: 13px; }}
.dot i {{ display: block; width: 18px; height: 18px; border-radius: 9px; }}
.themes {{ display: flex; padding: 8px 0 16px; border-bottom: 1px solid {line}; }}
.theme {{ flex: 1 1 0; margin-right: 12px; color: {text}; text-decoration: none; text-align: center; font-size: 12.5px; }}
.theme:last-child {{ margin-right: 0; }}
.mini {{ display: flex; height: 76px; border-radius: 10px; border: 2px solid {line}; margin-bottom: 7px; }}
.theme.on .mini {{ border-color: {accent}; }}
.theme.on span {{ font-weight: 500; }}
.mc {{ width: 30%; padding: 12px 0 0 8px; }}
.mc i {{ display: block; height: 4px; width: 85%; border-radius: 2px; margin-bottom: 6px; }}
.mk {{ flex: 1 1 auto; margin: 6px 6px 6px 0; border-radius: 5px; }}
.k {{ font-size: 12.5px; color: {muted}; padding: 3px 8px; border: 1px solid {line}; border-radius: 6px; }}
.btn {{ display: inline-block; font-size: 13px; padding: 6px 12px; border: 1px solid {line}; border-radius: 8px; color: {text}; text-decoration: none; }}
.none {{ color: {muted}; }}
</style></head><body><div class=wrap><aside><h1>SETTINGS</h1><form action="https://kamaitachi.local/settings"><input name=q value="{find}" placeholder="Find a setting"></form>{nav}</aside><main>{body}</main></div></body></html>"#,
        field = hex(s.field),
        page = hex(if s.light { [0xfb, 0xfa, 0xf7, 255] } else { s.field }),
        text = hex(s.text),
        muted = hex(s.muted),
        line = hex(s.hairline),
        wash = hex(s.wash),
        accent = hex(s.accent),
        knob = if s.light { "#ffffff" } else { "#cfd3da" },
        find = esc(find),
    )
    .replace("</div><h2>", "</div><div class=gap></div><h2>")
}

impl App {
    /// Shows the settings in tab `i`: a section, or what matches `find`.
    pub(super) fn show_settings(&mut self, i: usize, find: &str, section: &str, push: bool) {
        let t = &mut self.tabs[i];
        if push && !t.url.is_empty() && !t.url.starts_with(SETTINGS) {
            t.back.push((t.url.clone(), t.scroll));
            t.fwd.clear();
        }
        let same = t.url.starts_with(SETTINGS) && t.url.contains(&format!("s={section}"));
        let keep = if same { t.scroll } else { 0.0 };
        t.full_mode = false;
        t.url = if find.is_empty() { format!("{SETTINGS}?s={section}") } else { format!("{SETTINGS}?q={}", crate::url::encode(find)) };
        self.show_html(i, &html(find, section), "Settings");
        let t = &mut self.tabs[i];
        t.scroll = keep;
        t.target = keep;
    }

    /// Takes in changes from the settings page, `key=value&...`, and says which section
    /// they came from.
    pub(super) fn take_setting(&mut self, query: &str) -> String {
        let mut section = String::from("look");
        let mut changed = false;
        for pair in query.split('&') {
            let Some((k, v)) = pair.split_once('=') else { continue };
            let v = crate::url::percent_decode(v);
            if k == "s" {
                section = v;
            } else {
                changed |= prefs::set(k, &v);
            }
        }
        if changed {
            self.apply_prefs();
        }
        section
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

    /// The window's own title bar in the marble's colour, dark when the look is, and
    /// the glass behind the whole window when it is on.
    pub(super) fn frame_colors(&self) {
        use windows::Win32::UI::Controls::MARGINS;
        let s = sk();
        let glass = s.frame[3] < 255;
        unsafe {
            // Acrylic: Windows blurs what is behind the window and shows it through every
            // pixel the window leaves see-through. The frame has to reach into the client
            // area for that, which -1 margins do
            let m = if glass { MARGINS { cxLeftWidth: -1, cxRightWidth: -1, cyTopHeight: -1, cyBottomHeight: -1 } } else { MARGINS::default() };
            let _ = DwmExtendFrameIntoClientArea(self.hwnd, &m);
            let kind = if glass { DWMSBT_TRANSIENTWINDOW } else { DWMSBT_AUTO };
            let _ = DwmSetWindowAttribute(self.hwnd, DWMWA_SYSTEMBACKDROP_TYPE, &kind as *const _ as *const _, 4);
        }
        let dark: BOOL = (!s.light).into();
        // DWMWA_COLOR_NONE lets the glass show in the title bar too
        let cap = if glass { COLORREF(0xFFFF_FFFE) } else { COLORREF((s.frame[0] as u32) | (s.frame[1] as u32) << 8 | (s.frame[2] as u32) << 16) };
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
    fn a_section_or_what_matches() {
        let look = html("", "look");
        assert!(look.contains("Roomier tabs") && !look.contains("Search with"));
        assert!(look.contains("class=\"theme on\"") && look.contains("<select name=side>"));
        let found = html("zoom", "look");
        assert!(found.contains("Page zoom") && !found.contains("Roomier tabs"));
        assert!(html("google", "look").contains("Search with"));
        assert!(html("dark", "tabs").contains("class=themes"));
        assert!(html("weasel stamps", "look").contains("No setting has"));
        assert!(html("", "nonsense").contains("Roomier tabs"));
    }
}
