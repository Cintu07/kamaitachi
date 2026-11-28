//! Reading view: the article of a page and nothing else, on paper, in a book face.
//!
//! The article is found the way most readers find it. Every paragraph with a sentence
//! or two in it gives its length to the box it sits in, and half of that to the box
//! around that one. The box with the most is the article. Then only plain things are
//! copied out of it: headings, paragraphs, lists, quotes, code and links.

use super::dom::{t, Dom, Kind, NodeId, NONE};
use super::load::Doc;
use std::collections::HashMap;

fn text_len(dom: &Dom, n: NodeId) -> usize {
    let mut s = String::new();
    dom.text_content(n, &mut s);
    s.split_whitespace().map(|w| w.len() + 1).sum()
}

/// Boxes that are around an article and never part of it.
fn chrome(dom: &Dom, n: NodeId) -> bool {
    if matches!(dom.tag(n), t::nav | t::aside | t::footer | t::form | t::script | t::style | t::noscript | t::template | t::svg | t::button | t::iframe | t::object) {
        return true;
    }
    if dom.attr(n, t::hidden).is_some() || dom.attr(n, t::aria_hidden).is_some_and(|v| v == "true") {
        return true;
    }
    let names = format!("{} {}", dom.attr(n, t::class).unwrap_or(""), dom.attr(n, t::id).unwrap_or("")).to_ascii_lowercase();
    ["share", "comment", "related", "promo", "newsletter", "sidebar", "advert", "subscribe", "navbox", "mw-editsection", "reference", "noprint", "cookie", "toc"].iter().any(|w| names.contains(w))
}

/// The box that holds the article, if the page has one.
fn article(dom: &Dom) -> Option<NodeId> {
    let mut score: HashMap<NodeId, usize> = HashMap::new();
    for n in 0..dom.nodes.len() as NodeId {
        if !matches!(dom.tag(n), t::p | t::pre | t::blockquote) {
            continue;
        }
        let len = text_len(dom, n);
        if len < 40 {
            continue;
        }
        let parent = dom.parent(n);
        if parent == NONE {
            continue;
        }
        *score.entry(parent).or_default() += len;
        let grand = dom.parent(parent);
        if grand != NONE {
            *score.entry(grand).or_default() += len / 2;
        }
    }
    let (best, points) = score.into_iter().filter(|(n, _)| !chrome(dom, *n)).max_by_key(|(n, s)| (*s, std::cmp::Reverse(*n)))?;
    (points >= 400).then_some(best)
}

fn esc(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

/// Copies the plain parts of `n` into `out` as html.
fn copy(dom: &Dom, n: NodeId, doc: &Doc, out: &mut String, depth: u32) {
    if depth > 60 {
        return;
    }
    for c in dom.children(n) {
        match &dom.nodes[c as usize].kind {
            Kind::Text(s) => out.push_str(&esc(s)),
            Kind::Elem(_) if chrome(dom, c) => {}
            Kind::Elem(e) => {
                let tag = match e.tag {
                    t::h1 | t::h2 => "h2",
                    t::h3 | t::h4 | t::h5 | t::h6 => "h3",
                    t::p => "p",
                    t::ul => "ul",
                    t::ol => "ol",
                    t::li => "li",
                    t::blockquote => "blockquote",
                    t::pre => "pre",
                    t::code => "code",
                    t::b | t::strong => "b",
                    t::i | t::em => "i",
                    t::sup => "sup",
                    t::sub => "sub",
                    t::br => {
                        out.push_str("<br>");
                        continue;
                    }
                    t::figcaption => "figcaption",
                    t::table | t::img | t::picture | t::video | t::audio | t::canvas => continue,
                    t::a => {
                        // only links to other pages; nothing that reaches into the browser
                        let href = dom.attr(c, t::href).and_then(|h| doc.base.join(h)).map(|u| u.full()).filter(|h| !h.contains("://kamaitachi.local"));
                        match href {
                            Some(h) => {
                                out.push_str(&format!("<a href=\"{}\">", esc(&h)));
                                copy(dom, c, doc, out, depth + 1);
                                out.push_str("</a>");
                            }
                            None => copy(dom, c, doc, out, depth + 1),
                        }
                        continue;
                    }
                    _ => {
                        copy(dom, c, doc, out, depth + 1);
                        continue;
                    }
                };
                out.push_str(&format!("<{tag}>"));
                copy(dom, c, doc, out, depth + 1);
                out.push_str(&format!("</{tag}>"));
            }
            Kind::Doc => {}
        }
    }
}

/// The reading view of a page, as a document of its own, or None when no article was
/// found. `colors` are paper, ink for text, ink for links and a faint grey.
pub fn view(doc: &Doc, serif: &str, colors: [String; 4]) -> Option<String> {
    let dom = &doc.dom;
    let n = article(dom)?;
    let mut title = String::new();
    if let Some(tn) = dom.find(t::title) {
        dom.text_content(tn, &mut title);
    }
    let title = title.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut body = String::new();
    copy(dom, n, doc, &mut body, 0);
    let words = text_len(dom, n) / 6;
    let [paper, text, link, faint] = colors;
    Some(format!(
        r#"<!doctype html><html><head><title>{t}</title><style>
html {{ background: {paper}; color: {text}; font-family: "{serif}", Georgia, serif; }}
body {{ margin: 0; }}
main {{ max-width: 660px; margin: 0 auto; padding: 56px 32px 120px; font-size: 19px; line-height: 1.62; }}
.host {{ font-family: "CMU Typewriter Text", Consolas, monospace; font-size: 13px; color: {faint}; margin: 0; }}
h1 {{ font-size: 36px; line-height: 1.2; font-weight: 400; margin: 10px 0 34px; }}
h2 {{ font-size: 25px; font-weight: 400; margin: 40px 0 10px; }}
h3 {{ font-size: 20px; font-weight: 700; margin: 30px 0 6px; }}
p, ul, ol, blockquote, pre {{ margin: 0 0 18px; }}
blockquote {{ padding-left: 20px; border-left: 2px solid {faint}; font-style: italic; }}
pre, code {{ font-family: "CMU Typewriter Text", Consolas, monospace; font-size: 15px; }}
pre {{ white-space: pre-wrap; }}
figcaption {{ display: block; font-size: 15px; color: {faint}; margin: 0 0 18px; }}
a {{ color: {link}; text-decoration: none; }}
sup {{ font-size: 12px; }}
</style></head><body><main><p class="host">{host} · {min} min</p><h1>{t}</h1>{body}</main></body></html>"#,
        t = esc(&title),
        host = esc(doc.url.host.trim_start_matches("www.")),
        min = (words / 230).max(1),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::url::Url;

    #[test]
    fn the_article_and_none_of_the_rest() {
        let long = "The weasel rides the whirlwind and cuts without a sound, and nobody sees it go. ".repeat(4);
        let html = format!(
            "<title>Kamaitachi</title><nav><p>{long}</p></nav><div class=content><h1>Kamaitachi</h1><p>{long}</p><p>{long}<a href=/wiki/Yokai>yokai</a> <a href='https://kamaitachi.local/forget'>x</a></p>\
             <div class=share>share this</div><img src=a.png><p>{long}</p></div><aside><p>{long}</p></aside>"
        );
        let doc = crate::web::load::from_html(&html, Url::parse("https://en.wikipedia.org/wiki/K").unwrap(), 1);
        let v = view(&doc, "CMU Serif", ["#fff".into(), "#000".into(), "#00f".into(), "#999".into()]).expect("an article");
        assert!(v.contains("<h2>Kamaitachi</h2>") && v.contains("href=\"https://en.wikipedia.org/wiki/Yokai\""));
        assert!(!v.contains("share this") && !v.contains("kamaitachi.local") && !v.contains("a.png"));
        // three paragraphs of four sentences: the nav and the aside stay out
        assert_eq!(v.matches(&long[..30]).count(), 12);
    }

    #[test]
    fn a_page_with_no_article_has_no_view() {
        let doc = crate::web::load::from_html("<ul><li><a href=/a>a</a><li><a href=/b>b</a></ul>", Url::parse("https://x.com/").unwrap(), 1);
        assert!(view(&doc, "serif", Default::default()).is_none());
    }
}
