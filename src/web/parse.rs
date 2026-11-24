//! HTML to a DOM. The tokenizer follows the spec's states loosely; the tree builder keeps
//! the parts that change what a page looks like (implied html/head/body, void elements,
//! the tags that close an open <p>, <li>, <td> and friends, raw text elements) and drops
//! the adoption agency and foster parenting, which matter for broken markup only.
//! Scripting is off, so <noscript> content is parsed as markup.

use super::dom::{atom, t, Dom, Elem, Kind, NodeId, NONE};
use std::borrow::Cow;

const VOID: &[u32] = &[t::area, t::base, t::br, t::col, t::embed, t::hr, t::img, t::input, t::link, t::meta, t::param, t::source, t::track, t::wbr];
/// Tags that close an open <p> when they start.
const CLOSES_P: &[u32] = &[
    t::address, t::article, t::aside, t::blockquote, t::details, t::dialog, t::div, t::dl, t::fieldset, t::figcaption, t::figure, t::footer, t::form,
    t::h1, t::h2, t::h3, t::h4, t::h5, t::h6, t::header, t::hgroup, t::hr, t::main, t::menu, t::nav, t::ol, t::p, t::pre, t::section, t::table,
    t::ul, t::li, t::dd, t::dt, t::center, t::summary,
];
const HEAD_TAGS: &[u32] = &[t::title, t::meta, t::link, t::style, t::script, t::base, t::template, t::noscript];
const RAW: &[u32] = &[t::script, t::style, t::title, t::textarea, t::xmp, t::iframe, t::noembed, t::noframes, t::template];
const MAX_DEPTH: usize = 384;

struct Builder {
    dom: Dom,
    stack: Vec<NodeId>,
    html: NodeId,
    head: NodeId,
    body: NodeId,
    foreign: usize,
}

pub fn parse(src: &str) -> Dom {
    let mut b = Builder { dom: Dom::new(), stack: Vec::new(), html: NONE, head: NONE, body: NONE, foreign: 0 };
    b.html = b.dom.push(0, Kind::Elem(Box::new(Elem { tag: t::html, attrs: Vec::new(), id: NONE, classes: Vec::new() })));
    b.head = b.dom.push(b.html, Kind::Elem(Box::new(Elem { tag: t::head, attrs: Vec::new(), id: NONE, classes: Vec::new() })));
    b.stack.push(b.html);
    let head = src.trim_start_matches('\u{feff}').trim_start();
    b.dom.quirks = !head.get(..9).is_some_and(|d| d.eq_ignore_ascii_case("<!doctype"));
    let s = src.as_bytes();
    let mut i = 0usize;
    while i < s.len() {
        let Some(lt) = memchr(b'<', &s[i..]) else {
            b.text(&decode(&src[i..]));
            break;
        };
        if lt > 0 {
            b.text(&decode(&src[i..i + lt]));
        }
        i += lt;
        let rest = &s[i..];
        if rest.starts_with(b"<!--") {
            i = find(s, i + 4, b"-->").map_or(s.len(), |e| e + 3);
        } else if rest.starts_with(b"<![CDATA[") {
            let e = find(s, i + 9, b"]]>").unwrap_or(s.len());
            b.text(&src[i + 9..e]);
            i = (e + 3).min(s.len());
        } else if rest.starts_with(b"<!") || rest.starts_with(b"<?") {
            i = memchr(b'>', &s[i..]).map_or(s.len(), |e| i + e + 1);
        } else if rest.starts_with(b"</") {
            let (name, end) = read_name(s, i + 2);
            i = memchr(b'>', &s[end..]).map_or(s.len(), |e| end + e + 1);
            if !name.is_empty() {
                b.end(atom(&name));
            }
        } else if rest.len() > 1 && rest[1].is_ascii_alphabetic() {
            let (name, mut j) = read_name(s, i + 1);
            let mut attrs: Vec<(u32, String)> = Vec::new();
            let mut self_closing = false;
            loop {
                while j < s.len() && (s[j].is_ascii_whitespace() || s[j] == b'/') {
                    if s[j] == b'/' && s.get(j + 1) == Some(&b'>') {
                        self_closing = true;
                    }
                    j += 1;
                }
                if j >= s.len() || s[j] == b'>' {
                    j += 1;
                    break;
                }
                let st = j;
                while j < s.len() && !s[j].is_ascii_whitespace() && !matches!(s[j], b'/' | b'>' | b'=') {
                    j += 1;
                }
                if j == st {
                    j += 1; // a stray '=' at the start of a name
                    continue;
                }
                let an = src[st..j].to_ascii_lowercase();
                while j < s.len() && s[j].is_ascii_whitespace() {
                    j += 1;
                }
                let mut val = String::new();
                if j < s.len() && s[j] == b'=' {
                    j += 1;
                    while j < s.len() && s[j].is_ascii_whitespace() {
                        j += 1;
                    }
                    if j < s.len() && (s[j] == b'"' || s[j] == b'\'') {
                        let q = s[j];
                        let e = memchr(q, &s[j + 1..]).map_or(s.len(), |e| j + 1 + e);
                        val = decode(&src[j + 1..e]).into_owned();
                        j = (e + 1).min(s.len());
                    } else {
                        let st = j;
                        while j < s.len() && !s[j].is_ascii_whitespace() && s[j] != b'>' {
                            j += 1;
                        }
                        val = decode(&src[st..j]).into_owned();
                    }
                }
                let a = atom(&an);
                if !attrs.iter().any(|(k, _)| *k == a) {
                    attrs.push((a, val));
                }
            }
            i = j.min(s.len());
            let tag = atom(&name);
            let raw = b.start(tag, attrs, self_closing);
            if raw {
                // raw text runs to the matching end tag, whatever it contains
                let close = format!("</{name}");
                let e = find_ci(s, i, close.as_bytes()).unwrap_or(s.len());
                if matches!(tag, t::title | t::textarea | t::style) {
                    let body = &src[i..e];
                    if tag == t::style {
                        b.text(body);
                    } else {
                        b.text(&decode(body));
                    }
                }
                b.end(tag);
                i = memchr(b'>', &s[e..]).map_or(s.len(), |x| e + x + 1);
            }
        } else {
            b.text("<");
            i += 1;
        }
    }
    b.dom
}

fn memchr(c: u8, s: &[u8]) -> Option<usize> {
    s.iter().position(|&x| x == c)
}

fn find(s: &[u8], from: usize, pat: &[u8]) -> Option<usize> {
    if from >= s.len() {
        return None;
    }
    s[from..].windows(pat.len()).position(|w| w == pat).map(|p| from + p)
}

fn find_ci(s: &[u8], from: usize, pat: &[u8]) -> Option<usize> {
    if from >= s.len() {
        return None;
    }
    s[from..].windows(pat.len()).position(|w| w.eq_ignore_ascii_case(pat)).map(|p| from + p)
}

fn read_name(s: &[u8], mut j: usize) -> (String, usize) {
    let st = j;
    while j < s.len() && !s[j].is_ascii_whitespace() && s[j] != b'>' && s[j] != b'/' {
        j += 1;
    }
    (String::from_utf8_lossy(&s[st..j]).to_ascii_lowercase(), j)
}

impl Builder {
    fn cur(&self) -> NodeId {
        *self.stack.last().unwrap_or(&self.html)
    }

    fn in_stack(&self, tag: u32, stop: &[u32]) -> Option<usize> {
        for (k, &n) in self.stack.iter().enumerate().rev() {
            let tg = self.dom.tag(n);
            if tg == tag {
                return Some(k);
            }
            if stop.contains(&tg) {
                return None;
            }
        }
        None
    }

    fn pop_to(&mut self, k: usize) {
        while self.stack.len() > k {
            if let Some(n) = self.stack.pop()
                && matches!(self.dom.tag(n), t::svg | t::math)
            {
                self.foreign = self.foreign.saturating_sub(1);
            }
        }
    }

    fn close(&mut self, tag: u32, stop: &[u32]) {
        if let Some(k) = self.in_stack(tag, stop) {
            self.pop_to(k);
        }
    }

    fn ensure_body(&mut self) {
        if self.body == NONE {
            self.body = self.dom.push(self.html, Kind::Elem(Box::new(Elem { tag: t::body, attrs: Vec::new(), id: NONE, classes: Vec::new() })));
            self.stack.truncate(1);
            self.stack.push(self.body);
        }
    }

    fn merge_attrs(&mut self, n: NodeId, attrs: Vec<(u32, String)>) {
        if let Kind::Elem(e) = &mut self.dom.nodes[n as usize].kind {
            for (k, v) in attrs {
                if !e.attrs.iter().any(|(a, _)| *a == k) {
                    e.attrs.push((k, v));
                }
            }
            index(e);
        }
    }

    /// Returns true when the element's content is raw text.
    fn start(&mut self, tag: u32, attrs: Vec<(u32, String)>, self_closing: bool) -> bool {
        match tag {
            t::html => {
                self.merge_attrs(self.html, attrs);
                return false;
            }
            t::head => return false,
            t::body => {
                self.ensure_body();
                self.merge_attrs(self.body, attrs);
                return false;
            }
            _ => {}
        }
        let parent = if self.body == NONE && HEAD_TAGS.contains(&tag) {
            self.head
        } else {
            self.ensure_body();
            if self.foreign == 0 {
                self.implied_ends(tag);
            }
            self.cur()
        };
        let mut e = Elem { tag, attrs, id: NONE, classes: Vec::new() };
        index(&mut e);
        let n = self.dom.push(parent, Kind::Elem(Box::new(e)));
        if parent == self.head && self.body == NONE {
            // head content does not stay open, except raw text handled by the caller
            return RAW.contains(&tag);
        }
        let void = VOID.contains(&tag) || (self_closing && (self.foreign > 0 || matches!(tag, t::svg | t::math))) || self.stack.len() >= MAX_DEPTH;
        if !void {
            self.stack.push(n);
            if matches!(tag, t::svg | t::math) {
                self.foreign += 1;
            }
        }
        !void && RAW.contains(&tag) && self.foreign == 0
    }

    fn implied_ends(&mut self, tag: u32) {
        const SCOPE: &[u32] = &[t::table, t::td, t::th, t::html, t::button, t::caption, t::object];
        if CLOSES_P.contains(&tag) {
            self.close(t::p, SCOPE);
        }
        match tag {
            t::li => self.close(t::li, &[t::ul, t::ol, t::menu, t::table, t::td, t::th]),
            t::dt | t::dd => {
                let k = self.in_stack(t::dt, &[t::dl, t::table]).max(self.in_stack(t::dd, &[t::dl, t::table]));
                if let Some(k) = k {
                    self.pop_to(k);
                }
            }
            t::option => {
                if self.dom.tag(self.cur()) == t::option {
                    self.stack.pop();
                }
            }
            t::optgroup => {
                self.close(t::option, &[t::select]);
                self.close(t::optgroup, &[t::select]);
            }
            t::tr => {
                self.close(t::td, &[t::table, t::tr]);
                self.close(t::th, &[t::table, t::tr]);
                self.close(t::tr, &[t::table]);
            }
            t::td | t::th => {
                self.close(t::td, &[t::table, t::tr]);
                self.close(t::th, &[t::table, t::tr]);
                // a cell outside any row gets one
                let ct = self.dom.tag(self.cur());
                if matches!(ct, t::table | t::tbody | t::thead | t::tfoot) {
                    let tr = self.dom.push(self.cur(), Kind::Elem(Box::new(Elem { tag: t::tr, attrs: Vec::new(), id: NONE, classes: Vec::new() })));
                    self.stack.push(tr);
                }
            }
            t::thead | t::tbody | t::tfoot => {
                for x in [t::td, t::th, t::tr, t::thead, t::tbody, t::tfoot] {
                    self.close(x, &[t::table]);
                }
            }
            t::h1 | t::h2 | t::h3 | t::h4 | t::h5 | t::h6 => {
                if matches!(self.dom.tag(self.cur()), t::h1 | t::h2 | t::h3 | t::h4 | t::h5 | t::h6) {
                    self.stack.pop();
                }
            }
            t::a => self.close(t::a, &[t::table, t::td, t::th]),
            _ => {}
        }
    }

    fn end(&mut self, tag: u32) {
        match tag {
            t::html | t::body | t::head => {}
            t::br => {
                self.start(t::br, Vec::new(), false);
            }
            _ => {
                if self.foreign > 0 && !matches!(tag, t::svg | t::math) {
                    // inside svg every element closes where it says
                    if let Some(k) = self.in_stack(tag, &[t::svg, t::math]) {
                        self.pop_to(k);
                    }
                    return;
                }
                let stop: &[u32] = match tag {
                    t::p | t::li | t::dd | t::dt => &[t::table, t::td, t::th, t::ul, t::ol, t::dl, t::button],
                    t::td | t::th | t::tr | t::tbody | t::thead | t::tfoot => &[t::table],
                    _ => &[],
                };
                self.close(tag, stop);
            }
        }
    }

    fn text(&mut self, s: &str) {
        if s.is_empty() {
            return;
        }
        if self.body == NONE {
            let cur = self.cur();
            let raw_parent = self.dom.nodes[self.head as usize].last;
            // raw text inside head (title, style) goes to the element just opened
            if raw_parent != NONE && cur == self.html && matches!(self.dom.tag(raw_parent), t::title | t::style) && self.dom.nodes[raw_parent as usize].first == NONE {
                self.dom.push(raw_parent, Kind::Text(s.to_string()));
                return;
            }
            if s.bytes().all(|c| c.is_ascii_whitespace()) {
                return;
            }
            self.ensure_body();
        }
        let cur = self.cur();
        let last = self.dom.nodes[cur as usize].last;
        if last != NONE
            && let Kind::Text(prev) = &mut self.dom.nodes[last as usize].kind
        {
            prev.push_str(s);
            return;
        }
        self.dom.push(cur, Kind::Text(s.to_string()));
    }
}

/// Fills the id and class index of an element from its attributes.
fn index(e: &mut Elem) {
    e.id = NONE;
    e.classes.clear();
    for (k, v) in &e.attrs {
        if *k == t::id && !v.is_empty() {
            e.id = atom(v);
        } else if *k == t::class {
            for c in v.split_ascii_whitespace() {
                e.classes.push(atom(c));
            }
        }
    }
}

/// Character references in text and attribute values.
pub fn decode(s: &str) -> Cow<'_, str> {
    if !s.contains('&') {
        return Cow::Borrowed(s);
    }
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(p) = rest.find('&') {
        out.push_str(&rest[..p]);
        rest = &rest[p..];
        let b = rest.as_bytes();
        if b.len() > 2 && b[1] == b'#' {
            let (hex, st) = if b[2] == b'x' || b[2] == b'X' { (true, 3) } else { (false, 2) };
            let mut j = st;
            while j < b.len() && (if hex { b[j].is_ascii_hexdigit() } else { b[j].is_ascii_digit() }) && j < st + 8 {
                j += 1;
            }
            if j > st {
                let v = u32::from_str_radix(&rest[st..j], if hex { 16 } else { 10 }).unwrap_or(0xFFFD);
                let v = match v {
                    0x80..=0x9F => WIN1252[(v - 0x80) as usize] as u32,
                    0 => 0xFFFD,
                    v => v,
                };
                out.push(char::from_u32(v).unwrap_or('\u{FFFD}'));
                rest = &rest[if b.get(j) == Some(&b';') { j + 1 } else { j }..];
                continue;
            }
        } else {
            let mut j = 1;
            while j < b.len() && j < 32 && b[j].is_ascii_alphanumeric() {
                j += 1;
            }
            let name = &rest[1..j];
            let semi = b.get(j) == Some(&b';');
            if let Some(c) = named(name)
                && (semi || matches!(name, "amp" | "lt" | "gt" | "quot" | "nbsp" | "copy" | "reg"))
            {
                out.push_str(c);
                rest = &rest[if semi { j + 1 } else { j }..];
                continue;
            }
        }
        out.push('&');
        rest = &rest[1..];
    }
    out.push_str(rest);
    Cow::Owned(out)
}

const WIN1252: [char; 32] = [
    '€', '\u{81}', '‚', 'ƒ', '„', '…', '†', '‡', 'ˆ', '‰', 'Š', '‹', 'Œ', '\u{8D}', 'Ž', '\u{8F}', '\u{90}', '‘', '’', '“', '”', '•', '–', '—', '˜', '™', 'š',
    '›', 'œ', '\u{9D}', 'ž', 'Ÿ',
];

fn named(n: &str) -> Option<&'static str> {
    Some(match n {
        "amp" => "&",
        "lt" => "<",
        "gt" => ">",
        "quot" => "\"",
        "apos" => "'",
        "nbsp" => "\u{a0}",
        "copy" => "©",
        "reg" => "®",
        "trade" => "™",
        "hellip" => "…",
        "mdash" => "—",
        "ndash" => "–",
        "lsquo" => "‘",
        "rsquo" => "’",
        "sbquo" => "‚",
        "ldquo" => "“",
        "rdquo" => "”",
        "bdquo" => "„",
        "laquo" => "«",
        "raquo" => "»",
        "lsaquo" => "‹",
        "rsaquo" => "›",
        "middot" => "·",
        "bull" => "•",
        "deg" => "°",
        "plusmn" => "±",
        "times" => "×",
        "divide" => "÷",
        "frac12" => "½",
        "frac14" => "¼",
        "frac34" => "¾",
        "sup1" => "¹",
        "sup2" => "²",
        "sup3" => "³",
        "micro" => "µ",
        "para" => "¶",
        "sect" => "§",
        "cent" => "¢",
        "pound" => "£",
        "yen" => "¥",
        "euro" => "€",
        "iexcl" => "¡",
        "iquest" => "¿",
        "shy" => "\u{ad}",
        "larr" => "←",
        "rarr" => "→",
        "uarr" => "↑",
        "darr" => "↓",
        "harr" => "↔",
        "hearts" => "♥",
        "dagger" => "†",
        "Dagger" => "‡",
        "prime" => "′",
        "Prime" => "″",
        "minus" => "−",
        "le" => "≤",
        "ge" => "≥",
        "ne" => "≠",
        "asymp" => "≈",
        "infin" => "∞",
        "zwj" => "\u{200d}",
        "zwnj" => "\u{200c}",
        "thinsp" => "\u{2009}",
        "ensp" => "\u{2002}",
        "emsp" => "\u{2003}",
        "lrm" => "\u{200e}",
        "rlm" => "\u{200f}",
        "eacute" => "é",
        "egrave" => "è",
        "aacute" => "á",
        "agrave" => "à",
        "iacute" => "í",
        "oacute" => "ó",
        "uacute" => "ú",
        "ntilde" => "ñ",
        "ccedil" => "ç",
        "uuml" => "ü",
        "ouml" => "ö",
        "auml" => "ä",
        "szlig" => "ß",
        "alpha" => "α",
        "beta" => "β",
        "gamma" => "γ",
        "delta" => "δ",
        "pi" => "π",
        "mu" => "μ",
        "lambda" => "λ",
        "sigma" => "σ",
        "omega" => "ω",
        "check" => "✓",
        "star" => "☆",
        "starf" => "★",
        "nbhy" => "\u{2011}",
        "NewLine" => "\n",
        "Tab" => "\t",
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dump(d: &Dom, n: NodeId, depth: usize, out: &mut String) {
        for c in d.children(n) {
            match &d.nodes[c as usize].kind {
                Kind::Elem(e) => {
                    out.push_str(&format!("{}<{}>\n", "  ".repeat(depth), super::super::dom::name(e.tag)));
                    dump(d, c, depth + 1, out);
                }
                Kind::Text(s) => out.push_str(&format!("{}{:?}\n", "  ".repeat(depth), s)),
                Kind::Doc => {}
            }
        }
    }

    #[test]
    fn implied_structure() {
        let d = parse("<title>t &amp; u</title><p>one<p>two<ul><li>a<li>b</ul><table><td>x<td>y</table>");
        let mut s = String::new();
        dump(&d, 0, 0, &mut s);
        assert_eq!(
            s,
            "<html>\n  <head>\n    <title>\n      \"t & u\"\n  <body>\n    <p>\n      \"one\"\n    <p>\n      \"two\"\n    <ul>\n      <li>\n        \"a\"\n      <li>\n        \"b\"\n    <table>\n      <tr>\n        <td>\n          \"x\"\n        <td>\n          \"y\"\n"
        );
    }

    #[test]
    fn raw_text_and_attributes() {
        let d = parse(r#"<div class="a  b" id=x data-k='v'><script>if (a < b) { "</div>" }</script>text</div>"#);
        let div = d.find(t::div).unwrap();
        let e = d.elem(div).unwrap();
        assert_eq!(e.classes.len(), 2);
        assert_eq!(d.attr(div, t::id), Some("x"));
        let mut s = String::new();
        d.text_content(div, &mut s);
        assert_eq!(s, "text");
    }

    #[test]
    fn entities() {
        assert_eq!(decode("a&nbsp;b &#x1F600; &#8212; &amp &bogus; &lt;"), "a\u{a0}b 😀 — & &bogus; <");
    }
}
