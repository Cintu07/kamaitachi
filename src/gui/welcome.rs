//! The welcome: on the very first start, a page of four small questions, each answered
//! with one click. Where the tabs live, light or dark, a colour, and whether the
//! wallpaper shows through. Every answer takes effect at once, so the window changes
//! behind the page as you choose; "Start browsing" puts the page away for good.

use super::prefs;
use super::skin;
use super::*;

fn hex(c: Rgba) -> String {
    format!("#{:02x}{:02x}{:02x}", c[0], c[1], c[2])
}

fn pick(key: &str, value: &str) -> String {
    format!("https://kamaitachi.local/set?{key}={value}&amp;s=welcome")
}

/// A choice drawn as a small window: where its column sits, and in which colours.
fn card(key: &str, value: &str, name: &str, on: bool, look: (Rgba, Rgba, Rgba), right: bool) -> String {
    let (frame, card, line) = look;
    let col = format!("<div class=mc><i style=\"background:{l}\"></i><i style=\"background:{l};width:60%\"></i><i style=\"background:{l};width:75%\"></i></div>", l = hex(line));
    let page = format!("<div class=mk style=\"background:{}\"></div>", hex(card));
    let inner = if right { format!("{page}{col}") } else { format!("{col}{page}") };
    let body = format!("<div class=\"mini{}\" style=\"background:{}\">{inner}</div><span>{name}</span>", if right { " r" } else { "" }, hex(frame));
    if on { format!("<div class=\"c on\">{body}</div>") } else { format!("<a class=c href=\"{}\">{body}</a>", pick(key, value)) }
}

pub(super) fn html() -> String {
    let s = sk();
    let p = prefs::get();
    let now = |k: &str| p.value(k);
    let look = skin::preview(s.light);
    let side = format!(
        "{}{}",
        card("side", "left", "On the left", now("side") == "left", look, false),
        card("side", "right", "On the right", now("side") == "right", look, true)
    );
    let theme = format!(
        "{}{}{}",
        card("theme", "light", "Marble", now("theme") == "light", skin::preview(true), false),
        card("theme", "dark", "Night", now("theme") == "dark", skin::preview(false), false),
        card("theme", "system", "Like Windows", now("theme") == "system", look, false)
    );
    let dots: String = skin::accents()
        .iter()
        .enumerate()
        .map(|(i, a)| {
            let c = hex(if s.light { a.1 } else { a.2 });
            if i.to_string() == now("accent") {
                format!("<b class=\"dot on\" style=\"border-color:{c}\"><i style=\"background:{c}\"></i></b>")
            } else {
                format!("<a class=dot href=\"{}\"><i style=\"background:{c}\"></i></a>", pick("accent", &i.to_string()))
            }
        })
        .collect();
    let glass_on = now("glass") == "on";
    let glass = format!(
        "<a class=\"choice{}\" href=\"{}\">Yes, let it show</a><a class=\"choice{}\" href=\"{}\">Keep it solid</a>",
        if glass_on { " on" } else { "" },
        pick("glass", "on"),
        if glass_on { "" } else { " on" },
        pick("glass", "off")
    );
    format!(
        r#"<!doctype html><html><head><title>Welcome</title><style>
html {{ background: {page}; color: {text}; font-family: "Inter", "Segoe UI", sans-serif; font-size: 15px; }}
body {{ margin: 0; }}
main {{ max-width: 620px; margin: 0 auto; padding: 72px 28px 90px; }}
h1 {{ font-family: "CMU Serif", Georgia, serif; font-weight: 400; font-size: 38px; margin: 0 0 10px; }}
.lead {{ color: {muted}; font-size: 16px; line-height: 1.55; margin: 0 0 44px; }}
h2 {{ font-size: 15px; font-weight: 600; margin: 0 0 4px; }}
.q {{ color: {muted}; font-size: 13.5px; margin: 0 0 14px; }}
.row {{ display: flex; margin-bottom: 40px; }}
.c {{ flex: 1 1 0; margin-right: 14px; color: {text}; text-decoration: none; text-align: center; font-size: 13px; }}
.c:last-child {{ margin-right: 0; }}
.mini {{ display: flex; height: 84px; border-radius: 12px; border: 2px solid {line}; margin-bottom: 8px; }}
.c.on .mini {{ border-color: {accent}; }}
.c.on span {{ font-weight: 600; }}
.mc {{ width: 30%; padding: 14px 8px 0 10px; }}
.mc i {{ display: block; height: 4px; width: 85%; border-radius: 2px; margin-bottom: 7px; }}
.mk {{ flex: 1 1 auto; margin: 7px 7px 7px 0; border-radius: 6px; }}
.mini.r .mk {{ margin: 7px 0 7px 7px; }}
.dots {{ margin-bottom: 40px; }}
.dot {{ display: inline-block; width: 22px; height: 22px; padding: 3px; margin-right: 10px; border: 2px solid {page}; border-radius: 16px; }}
.dot i {{ display: block; width: 22px; height: 22px; border-radius: 11px; }}
.choice {{ display: inline-block; padding: 9px 16px; margin-right: 10px; border: 1px solid {line}; border-radius: 10px; color: {text}; text-decoration: none; font-size: 14px; }}
.choice.on {{ border-color: {accent}; color: {accent}; font-weight: 600; }}
.go {{ display: inline-block; margin-top: 52px; padding: 13px 26px; border-radius: 12px; background: {accent}; color: {on_accent}; text-decoration: none; font-weight: 600; font-size: 15px; }}
.small {{ color: {faint}; font-size: 12.5px; margin-top: 16px; }}
</style></head><body><main>
<h1>Welcome.</h1>
<p class=lead>A quiet place to read the web. Four small choices and it is yours; every one of them can be changed later in Settings.</p>
<h2>Where should your tabs live?</h2><p class=q>A column beside the page, on the side you reach for.</p>
<div class=row>{side}</div>
<h2>Light or dark?</h2><p class=q>Marble by day, slate at night, or whatever Windows is doing.</p>
<div class=row>{theme}</div>
<h2>A colour you like</h2><p class=q>It marks what you are on and what is chosen. Nothing else.</p>
<div class=dots>{dots}</div>
<h2>Let your wallpaper show through?</h2><p class=q>The tabs and the frame turn to frosted glass.</p>
<div>{glass}</div>
<a class=go href="https://kamaitachi.local/begin">Start browsing</a>
<p class=small>Ctrl+L goes anywhere. Ctrl+, opens Settings.</p>
</main></body></html>"#,
        page = hex(if s.light { s.field } else { s.frame }),
        text = hex(s.text),
        muted = hex(s.muted),
        faint = hex(s.faint),
        line = hex(s.hairline),
        accent = hex(s.accent),
        on_accent = hex(colour::on_accent(s.light)),
    )
}

impl App {
    pub(super) fn show_welcome(&mut self, i: usize) {
        let keep = if self.tabs[i].url.ends_with("/welcome") { self.tabs[i].scroll } else { 0.0 };
        self.tabs[i].full_mode = false;
        self.tabs[i].url = "https://kamaitachi.local/welcome".into();
        self.show_html(i, &html(), "Welcome");
        let t = &mut self.tabs[i];
        (t.scroll, t.target) = (keep, keep);
    }

    /// The welcome is done: from now on the browser starts on a new tab.
    pub(super) fn begin(&mut self, i: usize) {
        prefs::set("welcomed", "on");
        self.load(i, START.to_string(), false);
        self.focus_addr();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_choice_is_a_link_but_the_chosen_one() {
        let h = html();
        assert!(h.contains("set?side=right&amp;s=welcome") && h.contains("set?theme=dark&amp;s=welcome"));
        assert!(h.contains("class=\"c on\"") && h.contains("kamaitachi.local/begin"));
        assert!(!h.to_lowercase().contains(">kamaitachi"), "the name is not shown");
    }
}
