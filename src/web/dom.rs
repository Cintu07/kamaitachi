//! The document tree: one arena of nodes linked by index, and one process-wide table of
//! interned names (tags, attributes, classes) so matching compares integers.

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

pub type NodeId = u32;
pub const NONE: u32 = u32::MAX;

macro_rules! seed {
    ($($id:ident $s:literal),* $(,)?) => {
        #[allow(non_camel_case_types, dead_code, clippy::upper_case_acronyms)]
        #[repr(u32)]
        enum Seed { $($id),* }
        /// Names known at compile time, so code can match on them.
        #[allow(non_upper_case_globals, dead_code)]
        pub mod t { $(pub const $id: u32 = super::Seed::$id as u32;)* }
        const SEED: &[&str] = &[$($s),*];
    };
}

seed! {
    html "html", head "head", body "body", title "title", meta "meta", link "link", style "style",
    script "script", noscript "noscript", template "template", base "base", a "a", img "img",
    br "br", p "p", div "div", span "span", li "li", ul "ul", ol "ol", dl "dl", dt "dt", dd "dd",
    table "table", thead "thead", tbody "tbody", tfoot "tfoot", tr "tr", td "td", th "th",
    caption "caption", colgroup "colgroup", col "col", pre "pre", textarea "textarea",
    input "input", button "button", select "select", option "option", optgroup "optgroup",
    svg "svg", math "math", h1 "h1", h2 "h2", h3 "h3", h4 "h4", h5 "h5", h6 "h6", hr "hr",
    blockquote "blockquote", iframe "iframe", video "video", audio "audio", canvas "canvas",
    picture "picture", source "source", form "form", label "label", nav "nav", header "header",
    footer "footer", main "main", article "article", section "section", aside "aside",
    figure "figure", figcaption "figcaption", b "b", strong "strong", i "i", em "em",
    code "code", sup "sup", sub "sub", small "small", wbr "wbr", area "area", embed "embed",
    param "param", track "track", object "object", xmp "xmp", noembed "noembed",
    noframes "noframes", address "address", details "details", summary "summary",
    fieldset "fieldset", legend "legend", menu "menu", hgroup "hgroup", center "center",
    dialog "dialog", frameset "frameset", frame "frame", font "font", mark "mark", kbd "kbd",
    samp "samp", var "var", tt "tt", u "u", s "s", strike "strike", del "del", ins "ins",
    abbr "abbr", cite "cite", q "q", time "time", big "big", nobr "nobr", map "map",
    // attributes
    id "id", class "class", href "href", src "src", rel "rel",
    type_ "type", media "media", width "width", height "height", alt "alt", hidden "hidden",
    colspan "colspan", rowspan "rowspan", start "start", value "value", placeholder "placeholder",
    srcset "srcset", data_src "data-src", name "name", content "content", charset "charset",
    align "align", bgcolor "bgcolor", color "color", size "size", face "face", border "border",
    cellpadding "cellpadding", cellspacing "cellspacing", valign "valign", nowrap "nowrap",
    checked "checked", disabled "disabled", selected "selected", open "open", lang "lang",
    dir "dir", role "role", aria_hidden "aria-hidden", http_equiv "http-equiv", reversed "reversed",
    multiple "multiple",
}

struct Interner {
    map: HashMap<Box<str>, u32>,
    names: Vec<&'static str>,
}

fn interner() -> &'static Mutex<Interner> {
    static I: OnceLock<Mutex<Interner>> = OnceLock::new();
    I.get_or_init(|| {
        let mut it = Interner { map: HashMap::new(), names: Vec::new() };
        for (i, s) in SEED.iter().enumerate() {
            it.map.insert((*s).into(), i as u32);
            it.names.push(s);
        }
        Mutex::new(it)
    })
}

/// The id of a name, adding it on first sight.
pub fn atom(s: &str) -> u32 {
    let mut it = interner().lock().unwrap_or_else(|e| e.into_inner());
    if let Some(&a) = it.map.get(s) {
        return a;
    }
    let a = it.names.len() as u32;
    let leaked: &'static str = Box::leak(s.into());
    it.names.push(leaked);
    it.map.insert(s.into(), a);
    a
}

/// The id of a name only if it was seen before (a selector naming a class no element
/// has can never match).
pub fn lookup(s: &str) -> Option<u32> {
    interner().lock().unwrap_or_else(|e| e.into_inner()).map.get(s).copied()
}

pub fn name(a: u32) -> &'static str {
    interner().lock().unwrap_or_else(|e| e.into_inner()).names.get(a as usize).copied().unwrap_or("")
}

pub struct Elem {
    pub tag: u32,
    pub attrs: Vec<(u32, String)>,
    pub id: u32,
    pub classes: Vec<u32>,
}

pub enum Kind {
    Doc,
    Elem(Box<Elem>),
    Text(String),
}

pub struct Node {
    pub parent: NodeId,
    pub first: NodeId,
    pub last: NodeId,
    pub next: NodeId,
    pub prev: NodeId,
    pub kind: Kind,
}

pub struct Dom {
    pub nodes: Vec<Node>,
    /// no <!doctype html>: old table rules apply
    pub quirks: bool,
}

impl Default for Dom {
    fn default() -> Self {
        Self::new()
    }
}

impl Dom {
    pub fn new() -> Dom {
        Dom { nodes: vec![Node { parent: NONE, first: NONE, last: NONE, next: NONE, prev: NONE, kind: Kind::Doc }], quirks: false }
    }

    pub fn push(&mut self, parent: NodeId, kind: Kind) -> NodeId {
        let id = self.nodes.len() as NodeId;
        let prev = self.nodes[parent as usize].last;
        self.nodes.push(Node { parent, first: NONE, last: NONE, next: NONE, prev, kind });
        if prev == NONE {
            self.nodes[parent as usize].first = id;
        } else {
            self.nodes[prev as usize].next = id;
        }
        self.nodes[parent as usize].last = id;
        id
    }

    pub fn elem(&self, n: NodeId) -> Option<&Elem> {
        match &self.nodes.get(n as usize)?.kind {
            Kind::Elem(e) => Some(e),
            _ => None,
        }
    }

    pub fn tag(&self, n: NodeId) -> u32 {
        self.elem(n).map_or(NONE, |e| e.tag)
    }

    pub fn text(&self, n: NodeId) -> Option<&str> {
        match &self.nodes.get(n as usize)?.kind {
            Kind::Text(s) => Some(s),
            _ => None,
        }
    }

    pub fn attr(&self, n: NodeId, a: u32) -> Option<&str> {
        self.elem(n)?.attrs.iter().find(|(k, _)| *k == a).map(|(_, v)| v.as_str())
    }

    pub fn set_attr(&mut self, n: NodeId, a: u32, v: &str) {
        let Some(Kind::Elem(e)) = self.nodes.get_mut(n as usize).map(|n| &mut n.kind) else { return };
        match e.attrs.iter_mut().find(|(k, _)| *k == a) {
            Some((_, old)) => *old = v.to_string(),
            None => e.attrs.push((a, v.to_string())),
        }
    }

    pub fn remove_attr(&mut self, n: NodeId, a: u32) {
        if let Some(Kind::Elem(e)) = self.nodes.get_mut(n as usize).map(|n| &mut n.kind) {
            e.attrs.retain(|(k, _)| *k != a);
        }
    }

    pub fn parent(&self, n: NodeId) -> NodeId {
        self.nodes[n as usize].parent
    }

    pub fn children(&self, n: NodeId) -> Children<'_> {
        Children { dom: self, cur: self.nodes[n as usize].first }
    }

    /// The parent if it is an element.
    pub fn parent_elem(&self, n: NodeId) -> Option<NodeId> {
        let p = self.nodes[n as usize].parent;
        (p != NONE && self.elem(p).is_some()).then_some(p)
    }

    pub fn prev_elem(&self, n: NodeId) -> Option<NodeId> {
        let mut c = self.nodes[n as usize].prev;
        while c != NONE {
            if self.elem(c).is_some() {
                return Some(c);
            }
            c = self.nodes[c as usize].prev;
        }
        None
    }

    pub fn next_elem(&self, n: NodeId) -> Option<NodeId> {
        let mut c = self.nodes[n as usize].next;
        while c != NONE {
            if self.elem(c).is_some() {
                return Some(c);
            }
            c = self.nodes[c as usize].next;
        }
        None
    }

    /// The first element with this tag, in tree order.
    pub fn find(&self, tag: u32) -> Option<NodeId> {
        (0..self.nodes.len() as NodeId).find(|&n| self.tag(n) == tag)
    }

    pub fn text_content(&self, n: NodeId, out: &mut String) {
        for c in self.children(n) {
            match &self.nodes[c as usize].kind {
                Kind::Text(s) => out.push_str(s),
                Kind::Elem(_) => self.text_content(c, out),
                Kind::Doc => {}
            }
        }
    }
}

pub struct Children<'a> {
    dom: &'a Dom,
    cur: NodeId,
}

impl Iterator for Children<'_> {
    type Item = NodeId;
    fn next(&mut self) -> Option<NodeId> {
        if self.cur == NONE {
            return None;
        }
        let c = self.cur;
        self.cur = self.dom.nodes[c as usize].next;
        Some(c)
    }
}
