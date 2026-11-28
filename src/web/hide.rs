//! Things you hid on a site by pointing at them: a cookie banner, a newsletter box, a
//! column of "more stories". Each is kept as a css selector for its host, in
//! %APPDATA%\hayate\hidden.txt, and goes into the page as a stylesheet before it is
//! laid out, so a hidden box is never drawn at all. Both engines read the same rules.

use super::dom::{name, t, Dom, NodeId, NONE};
use std::sync::{Mutex, OnceLock};

fn file() -> std::path::PathBuf {
    let base = std::env::var("APPDATA").unwrap_or_else(|_| ".".into());
    std::path::Path::new(&base).join("hayate").join("hidden.txt")
}

/// (host, selector), oldest first.
fn rules() -> &'static Mutex<Vec<(String, String)>> {
    static R: OnceLock<Mutex<Vec<(String, String)>>> = OnceLock::new();
    R.get_or_init(|| {
        let text = std::fs::read_to_string(file()).unwrap_or_default();
        let v = text.lines().filter_map(|l| l.split_once('\t')).map(|(h, s)| (h.to_string(), s.to_string())).collect();
        Mutex::new(v)
    })
}

fn save(v: &[(String, String)]) {
    let text: String = v.iter().map(|(h, s)| format!("{h}\t{s}\n")).collect();
    let _ = std::fs::write(file(), text);
}

/// The host rules are kept under: "www." makes no difference.
fn key(host: &str) -> String {
    host.trim_start_matches("www.").to_ascii_lowercase()
}

/// The stylesheet that hides everything chosen on this host; empty when nothing is.
pub fn css(host: &str) -> String {
    let k = key(host);
    let v = rules().lock().unwrap_or_else(|e| e.into_inner());
    v.iter().filter(|(h, _)| *h == k).map(|(_, s)| format!("{s} {{ display: none !important }}\n")).collect()
}

pub fn count(host: &str) -> usize {
    let k = key(host);
    rules().lock().unwrap_or_else(|e| e.into_inner()).iter().filter(|(h, _)| *h == k).count()
}

pub fn add(host: &str, selector: &str) {
    let mut v = rules().lock().unwrap_or_else(|e| e.into_inner());
    let r = (key(host), selector.to_string());
    if !v.contains(&r) {
        v.push(r);
        save(&v);
    }
}

/// Brings back the last thing hidden on this host. Returns whether there was one.
pub fn undo(host: &str) -> bool {
    let k = key(host);
    let mut v = rules().lock().unwrap_or_else(|e| e.into_inner());
    let Some(i) = v.iter().rposition(|(h, _)| *h == k) else { return false };
    v.remove(i);
    save(&v);
    true
}

/// A css name that is safe to write as it is, and does not look made up by a build
/// tool: those change with every deploy, so a rule naming one would stop working.
fn lasting(s: &str) -> bool {
    let digits = s.bytes().filter(u8::is_ascii_digit).count();
    !s.is_empty() && s.len() <= 40 && digits <= 2 && !s.starts_with(|c: char| c.is_ascii_digit() || c == '-') && s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

/// A selector for one element: its tag and lasting classes, with its place among its
/// brothers when those alone would match one of them too, and the same for each box
/// around it up to one with a lasting id, or the body.
pub fn selector(dom: &Dom, n: NodeId) -> String {
    let mut steps: Vec<String> = Vec::new();
    let mut c = n;
    while c != NONE && dom.elem(c).is_some() {
        let tag = dom.tag(c);
        if tag == t::body || tag == t::html {
            steps.push(name(tag).to_string());
            break;
        }
        if let Some(id) = dom.attr(c, t::id).filter(|id| lasting(id)) {
            steps.push(format!("#{id}"));
            break;
        }
        let classes: Vec<&str> = dom.attr(c, t::class).unwrap_or("").split_whitespace().filter(|k| lasting(k)).take(2).collect();
        let mut step = name(tag).to_string();
        for k in &classes {
            step.push('.');
            step.push_str(k);
        }
        let alike = |o: NodeId| dom.tag(o) == tag && classes.iter().all(|k| dom.attr(o, t::class).unwrap_or("").split_whitespace().any(|x| x == *k));
        let parent = dom.parent(c);
        let brothers: Vec<NodeId> = dom.children(parent).filter(|&o| dom.elem(o).is_some()).collect();
        if brothers.iter().any(|&o| o != c && alike(o)) {
            let place = brothers.iter().position(|&o| o == c).unwrap_or(0) + 1;
            step.push_str(&format!(":nth-child({place})"));
        }
        steps.push(step);
        c = parent;
    }
    steps.reverse();
    steps.join(" > ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::web::parse::parse;

    fn by_text(dom: &Dom, text: &str) -> NodeId {
        // the innermost element with exactly that text
        (0..dom.nodes.len() as NodeId)
            .rfind(|&n| {
                let mut s = String::new();
                dom.text_content(n, &mut s);
                dom.elem(n).is_some() && s == text
            })
            .unwrap()
    }

    #[test]
    fn selectors_stop_at_an_id_and_count_only_when_they_must() {
        let d = parse("<div id=main><p>a</p><p class='note x9f3k2a'>b</p><p class=note>c</p><aside class='promo css-1x2y3z'>d</aside></div><div><span>e</span></div>");
        assert_eq!(selector(&d, by_text(&d, "d")), "#main > aside.promo");
        assert_eq!(selector(&d, by_text(&d, "c")), "#main > p.note:nth-child(3)");
        assert_eq!(selector(&d, by_text(&d, "e")), "body > div:nth-child(2) > span");
    }

    #[test]
    fn the_selector_finds_the_element_it_was_made_from() {
        use crate::web::css::parse_sheet;
        use crate::web::style::Cascade;
        use crate::web::style::Display;
        let d = parse("<div id=main><p>a</p><p class=note>b</p><p class=note>c</p></div><div><p>a</p></div>");
        let target = by_text(&d, "c");
        let sheet = parse_sheet(&format!("{} {{ display: none !important }}", selector(&d, target)));
        let st = Cascade { dom: &d, sheets: &[std::sync::Arc::new(sheet)], vw: 800.0, vh: 600.0 }.run();
        let gone: Vec<NodeId> = (0..d.nodes.len() as NodeId).filter(|&n| st.get(n).is_some_and(|s| s.display == Display::None)).collect();
        assert_eq!(gone, [target]);
    }
}
