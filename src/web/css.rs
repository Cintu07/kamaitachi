//! Stylesheets: rules, selectors and media queries. Declaration values stay as text until
//! the cascade, because `var()` can only be resolved per element.

use super::dom::{atom, lookup, t, Dom, NodeId, NONE};
use std::cell::Cell;

thread_local! {
    static INTERN: Cell<bool> = const { Cell::new(false) };
}

/// A name's id. Sheets shared across pages intern every name they mention; a page's own
/// sheets only look names up, since a name no element has can never match.
fn name_id(s: &str) -> Option<u32> {
    if INTERN.with(|c| c.get()) { Some(atom(s)) } else { lookup(s) }
}

/// Parses a sheet that many pages will share (the user agent's, the blocker's).
pub fn parse_sheet_interned(src: &str) -> Sheet {
    INTERN.with(|c| c.set(true));
    let s = parse_sheet(src);
    INTERN.with(|c| c.set(false));
    s
}

#[derive(Default)]
pub struct Sheet {
    pub rules: Vec<Rule>,
    pub imports: Vec<String>,
    /// media conditions; rule.media indexes this, 0 means "always". Conditions joined
    /// with '\u{1}' must all hold (nested @media).
    pub media: Vec<String>,
}

pub struct Rule {
    pub sels: Vec<Sel>,
    pub decls: Vec<Decl>,
    pub media: u16,
}

#[derive(Clone)]
pub struct Decl {
    pub name: Box<str>,
    pub value: Box<str>,
    pub important: bool,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Comb {
    Desc,
    Child,
    Adj,
    Sib,
}

#[derive(Clone, Debug)]
pub struct AttrSel {
    pub name: u32,
    /// b'\0' present, b'=' equals, b'~' word, b'|' lang prefix, b'^' starts, b'$' ends, b'*' contains
    pub op: u8,
    pub val: String,
    pub ci: bool,
}

#[derive(Clone, Debug)]
pub enum Pc {
    FirstChild,
    LastChild,
    OnlyChild,
    FirstOfType,
    LastOfType,
    OnlyOfType,
    /// a, b, from the end, of type
    Nth(i32, i32, bool, bool),
    Not(Vec<Sel>),
    Is(Vec<Sel>),
    Root,
    Empty,
    Link,
    Checked,
    Disabled,
    Enabled,
    Defined,
}

#[derive(Clone, Default, Debug)]
pub struct Compound {
    pub tag: u32,
    pub id: u32,
    pub classes: Vec<u32>,
    pub attrs: Vec<AttrSel>,
    pub pcs: Vec<Pc>,
    /// names something no element has; never matches
    pub never: bool,
}

#[derive(Clone, Debug)]
pub struct Sel {
    /// right to left: parts[0] is the subject
    pub parts: Vec<Compound>,
    /// combs[i] joins parts[i] and parts[i + 1]
    pub combs: Vec<Comb>,
    pub spec: u32,
    /// 0 the element, 1 ::before, 2 ::after
    pub pseudo: u8,
}

/// Removes comments, keeping strings intact.
fn strip_comments(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = String::with_capacity(s.len());
    let mut i = 0;
    let mut last = 0;
    while i < b.len() {
        match b[i] {
            b'"' | b'\'' => {
                let q = b[i];
                i += 1;
                while i < b.len() && b[i] != q {
                    if b[i] == b'\\' {
                        i += 1;
                    }
                    i += 1;
                }
                i += 1;
            }
            b'/' if b.get(i + 1) == Some(&b'*') => {
                out.push_str(&s[last..i]);
                i = s[i + 2..].find("*/").map_or(b.len(), |e| i + 2 + e + 2);
                last = i;
            }
            _ => i += 1,
        }
    }
    out.push_str(&s[last.min(s.len())..]);
    out
}

/// The index just past the '}' matching the '{' at `open`.
fn block_end(b: &[u8], open: usize) -> usize {
    let mut depth = 0i32;
    let mut i = open;
    while i < b.len() {
        match b[i] {
            b'"' | b'\'' => {
                let q = b[i];
                i += 1;
                while i < b.len() && b[i] != q {
                    if b[i] == b'\\' {
                        i += 1;
                    }
                    i += 1;
                }
            }
            b'\\' => i += 1,
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    return i + 1;
                }
            }
            _ => {}
        }
        i += 1;
    }
    b.len()
}

/// The first of `stops` at nesting depth 0, outside strings.
fn scan_to(b: &[u8], mut i: usize, stops: &[u8]) -> usize {
    let mut depth = 0i32;
    while i < b.len() {
        let c = b[i];
        match c {
            b'"' | b'\'' => {
                i += 1;
                while i < b.len() && b[i] != c {
                    if b[i] == b'\\' {
                        i += 1;
                    }
                    i += 1;
                }
            }
            b'\\' => i += 1,
            _ if depth <= 0 && stops.contains(&c) => return i,
            b'(' | b'[' => depth += 1,
            b')' | b']' => depth -= 1,
            _ => {}
        }
        i += 1;
    }
    b.len()
}

pub fn parse_sheet(src: &str) -> Sheet {
    let s = strip_comments(src);
    let mut sheet = Sheet { media: vec![String::new()], ..Default::default() };
    parse_rules(&s, 0, &mut sheet);
    sheet
}

fn parse_rules(s: &str, media: u16, sheet: &mut Sheet) {
    let b = s.as_bytes();
    let mut i = 0;
    while i < b.len() {
        while i < b.len() && (b[i].is_ascii_whitespace() || b[i] == b';' || b[i] == b'}') {
            i += 1;
        }
        if i >= b.len() {
            break;
        }
        let stop = scan_to(b, i, b"{;");
        let prelude = s[i..stop].trim();
        if stop >= b.len() {
            break;
        }
        if b[stop] == b';' {
            if let Some(rest) = prelude.strip_prefix("@import") {
                if let Some(u) = import_url(rest) {
                    sheet.imports.push(u);
                }
            }
            i = stop + 1;
            continue;
        }
        let end = block_end(b, stop);
        let body = &s[stop + 1..end.saturating_sub(1).max(stop + 1)];
        i = end;
        if let Some(at) = prelude.strip_prefix('@') {
            let name: String = at.chars().take_while(|c| c.is_ascii_alphanumeric() || *c == '-').collect::<String>().to_ascii_lowercase();
            let rest = at[name.len()..].trim();
            match name.as_str() {
                "media" => {
                    let cond = if media == 0 { rest.to_string() } else { format!("{}\u{1}{}", sheet.media[media as usize], rest) };
                    sheet.media.push(cond);
                    let idx = (sheet.media.len() - 1).min(u16::MAX as usize) as u16;
                    parse_rules(body, idx, sheet);
                }
                "supports" => {
                    if !rest.trim_start().to_ascii_lowercase().starts_with("not") && !rest.contains("selector(") {
                        parse_rules(body, media, sheet);
                    }
                }
                "layer" | "document" | "-moz-document" | "scope" | "starting-style" => {
                    if name == "layer" || name == "scope" {
                        parse_rules(body, media, sheet);
                    }
                }
                _ => {}
            }
            continue;
        }
        let sels = parse_sel_list(prelude);
        if sels.is_empty() {
            continue;
        }
        let decls = parse_decls(body);
        if !decls.is_empty() {
            sheet.rules.push(Rule { sels, decls, media });
        }
    }
}

fn import_url(rest: &str) -> Option<String> {
    let r = rest.trim();
    let r = r.strip_prefix("url(").map(|x| x.split(')').next().unwrap_or("")).unwrap_or(r);
    let r = r.trim().trim_matches(|c| c == '"' || c == '\'');
    let r = r.split_whitespace().next()?;
    let r = r.trim_matches(|c| c == '"' || c == '\'');
    (!r.is_empty()).then(|| r.to_string())
}

/// `name: value` pairs of a declaration block; nested rules are skipped.
pub fn parse_decls(body: &str) -> Vec<Decl> {
    let b = body.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < b.len() {
        let end = scan_to(b, i, b";{");
        if end < b.len() && b[end] == b'{' {
            // a nested rule: skip the whole block
            i = block_end(b, end);
            continue;
        }
        let d = &body[i..end];
        i = end + 1;
        let Some(colon) = d.find(':') else { continue };
        let name = d[..colon].trim();
        if name.is_empty() || name.contains(char::is_whitespace) {
            continue;
        }
        let mut val = d[colon + 1..].trim();
        let mut important = false;
        if let Some(p) = val.rfind('!')
            && val[p + 1..].trim().eq_ignore_ascii_case("important")
        {
            important = true;
            val = val[..p].trim_end();
        }
        let name: Box<str> = if name.starts_with("--") { name.into() } else { name.to_ascii_lowercase().into() };
        out.push(Decl { name, value: val.into(), important });
    }
    out
}

/// Splits at a separator that is outside parentheses, brackets and strings.
pub fn split_top(s: &str, sep: u8) -> Vec<&str> {
    let b = s.as_bytes();
    let mut out = Vec::new();
    let mut st = 0;
    loop {
        let e = scan_to(b, st, &[sep]);
        out.push(&s[st..e]);
        if e >= b.len() {
            break;
        }
        st = e + 1;
    }
    out
}

pub fn parse_sel_list(s: &str) -> Vec<Sel> {
    split_top(s, b',').into_iter().filter_map(|p| parse_sel(p.trim())).collect()
}

fn ident(b: &[u8], i: &mut usize) -> String {
    let mut out = String::new();
    while *i < b.len() {
        let c = b[*i];
        if c == b'\\' && *i + 1 < b.len() {
            *i += 1;
            let st = *i;
            while *i < b.len() && *i - st < 6 && b[*i].is_ascii_hexdigit() {
                *i += 1;
            }
            if *i > st {
                let v = u32::from_str_radix(std::str::from_utf8(&b[st..*i]).unwrap_or("0"), 16).unwrap_or(0xFFFD);
                out.push(char::from_u32(v).unwrap_or('\u{FFFD}'));
                if *i < b.len() && b[*i] == b' ' {
                    *i += 1;
                }
            } else {
                let ch = std::str::from_utf8(&b[*i..]).ok().and_then(|s| s.chars().next()).unwrap_or('\\');
                out.push(ch);
                *i += ch.len_utf8();
            }
        } else if c.is_ascii_alphanumeric() || c == b'-' || c == b'_' || c >= 0x80 {
            let len = utf8_len(c);
            out.push_str(std::str::from_utf8(&b[*i..(*i + len).min(b.len())]).unwrap_or(""));
            *i += len;
        } else {
            break;
        }
    }
    out
}

fn utf8_len(c: u8) -> usize {
    match c {
        0xF0..=0xFF => 4,
        0xE0..=0xEF => 3,
        0xC0..=0xDF => 2,
        _ => 1,
    }
}

fn paren_arg<'a>(s: &'a str, b: &[u8], i: &mut usize) -> &'a str {
    // b[*i] == '('
    let st = *i + 1;
    let mut depth = 0;
    while *i < b.len() {
        match b[*i] {
            b'(' => depth += 1,
            b')' => {
                depth -= 1;
                if depth == 0 {
                    break;
                }
            }
            _ => {}
        }
        *i += 1;
    }
    let e = (*i).min(b.len());
    *i = (*i + 1).min(b.len());
    &s[st.min(e)..e]
}

fn nth(arg: &str) -> Option<(i32, i32)> {
    let a = arg.split(" of ").next()?.trim().to_ascii_lowercase().replace(' ', "");
    match a.as_str() {
        "odd" => return Some((2, 1)),
        "even" => return Some((2, 0)),
        _ => {}
    }
    if let Some(p) = a.find('n') {
        let co = match &a[..p] {
            "" | "+" => 1,
            "-" => -1,
            x => x.parse().ok()?,
        };
        let rest = &a[p + 1..];
        let b = if rest.is_empty() { 0 } else { rest.parse().ok()? };
        Some((co, b))
    } else {
        Some((0, a.parse().ok()?))
    }
}

pub fn parse_sel(s: &str) -> Option<Sel> {
    let b = s.as_bytes();
    let mut i = 0;
    let mut parts: Vec<Compound> = Vec::new();
    let mut combs: Vec<Comb> = Vec::new();
    let mut spec = 0u32;
    let mut pseudo = 0u8;
    let mut pending: Option<Comb> = None;
    loop {
        let ws = i < b.len() && b[i].is_ascii_whitespace();
        while i < b.len() && b[i].is_ascii_whitespace() {
            i += 1;
        }
        if i >= b.len() {
            break;
        }
        if matches!(b[i], b'>' | b'+' | b'~') {
            pending = Some(match b[i] {
                b'>' => Comb::Child,
                b'+' => Comb::Adj,
                _ => Comb::Sib,
            });
            i += 1;
            continue;
        }
        if !parts.is_empty() {
            combs.push(pending.take().unwrap_or(if ws { Comb::Desc } else { return None }));
        } else if pending.is_some() {
            return None; // a relative selector (nesting) we do not handle
        }
        if pseudo != 0 {
            return None; // nothing may follow a pseudo-element
        }
        let mut c = Compound { tag: NONE, id: NONE, ..Default::default() };
        let mut any = false;
        while i < b.len() && !b[i].is_ascii_whitespace() && !matches!(b[i], b'>' | b'+' | b'~') {
            any = true;
            match b[i] {
                b'*' => i += 1,
                b'#' => {
                    i += 1;
                    let n = ident(b, &mut i);
                    match name_id(&n) {
                        Some(a) => c.id = a,
                        None => c.never = true,
                    }
                    spec += 1_000_000;
                }
                b'.' => {
                    i += 1;
                    let n = ident(b, &mut i);
                    if n.is_empty() {
                        return None;
                    }
                    match name_id(&n) {
                        Some(a) => c.classes.push(a),
                        None => c.never = true,
                    }
                    spec += 1000;
                }
                b'[' => {
                    let e = scan_to(b, i + 1, b"]");
                    let inner = &s[i + 1..e.min(s.len())];
                    i = (e + 1).min(b.len());
                    spec += 1000;
                    let ib = inner.as_bytes();
                    let mut k = 0;
                    while k < ib.len() && ib[k].is_ascii_whitespace() {
                        k += 1;
                    }
                    let name = ident(ib, &mut k).to_ascii_lowercase();
                    while k < ib.len() && ib[k].is_ascii_whitespace() {
                        k += 1;
                    }
                    let mut op = 0u8;
                    let mut val = String::new();
                    let mut ci = false;
                    if k < ib.len() {
                        if ib[k] == b'=' {
                            op = b'=';
                            k += 1;
                        } else if k + 1 < ib.len() && ib[k + 1] == b'=' {
                            op = ib[k];
                            k += 2;
                        } else {
                            return None;
                        }
                        let rest = inner[k..].trim();
                        let (v, flag) = if let Some(q) = rest.chars().next().filter(|q| *q == '"' || *q == '\'') {
                            let e = rest[1..].find(q).map_or(rest.len(), |e| e + 1);
                            (rest[1..e].to_string(), rest.get(e + 1..).unwrap_or("").trim())
                        } else {
                            let mut parts = rest.split_whitespace();
                            (parts.next().unwrap_or("").to_string(), parts.next().unwrap_or(""))
                        };
                        val = v;
                        ci = flag.eq_ignore_ascii_case("i");
                    }
                    match name_id(&name) {
                        Some(a) => c.attrs.push(AttrSel { name: a, op, val, ci }),
                        None => c.never = true,
                    }
                }
                b':' => {
                    i += 1;
                    let double = i < b.len() && b[i] == b':';
                    if double {
                        i += 1;
                    }
                    let n = ident(b, &mut i).to_ascii_lowercase();
                    let arg = if i < b.len() && b[i] == b'(' { Some(paren_arg(s, b, &mut i)) } else { None };
                    // :not() and :is() count as their arguments, :where() as nothing
                    let mut counts = true;
                    match (n.as_str(), arg) {
                        ("before", None) => pseudo = 1,
                        ("after", None) => pseudo = 2,
                        _ if double => return None,
                        ("first-child", _) => c.pcs.push(Pc::FirstChild),
                        ("last-child", _) => c.pcs.push(Pc::LastChild),
                        ("only-child", _) => c.pcs.push(Pc::OnlyChild),
                        ("first-of-type", _) => c.pcs.push(Pc::FirstOfType),
                        ("last-of-type", _) => c.pcs.push(Pc::LastOfType),
                        ("only-of-type", _) => c.pcs.push(Pc::OnlyOfType),
                        ("root", _) => c.pcs.push(Pc::Root),
                        ("empty", _) => c.pcs.push(Pc::Empty),
                        ("link" | "any-link", _) => c.pcs.push(Pc::Link),
                        ("checked", _) => c.pcs.push(Pc::Checked),
                        ("disabled", _) => c.pcs.push(Pc::Disabled),
                        ("enabled", _) => c.pcs.push(Pc::Enabled),
                        ("defined", _) => c.pcs.push(Pc::Defined),
                        ("first-line" | "first-letter" | "marker" | "placeholder" | "selection" | "backdrop", _) => return None,
                        ("nth-child" | "nth-last-child" | "nth-of-type" | "nth-last-of-type", Some(a)) => {
                            let (x, y) = nth(a)?;
                            c.pcs.push(Pc::Nth(x, y, n.contains("last"), n.contains("type")));
                        }
                        ("not", Some(a)) => {
                            let inner = parse_sel_list(a);
                            spec += inner.iter().map(|s| s.spec).max().unwrap_or(0);
                            c.pcs.push(Pc::Not(inner));
                            counts = false;
                        }
                        ("is" | "matches" | "-webkit-any" | "-moz-any", Some(a)) => {
                            let inner = parse_sel_list(a);
                            if inner.is_empty() {
                                c.never = true;
                            }
                            spec += inner.iter().map(|s| s.spec).max().unwrap_or(0);
                            c.pcs.push(Pc::Is(inner));
                            counts = false;
                        }
                        ("where", Some(a)) => {
                            let inner = parse_sel_list(a);
                            if inner.is_empty() {
                                c.never = true;
                            }
                            c.pcs.push(Pc::Is(inner));
                            counts = false;
                        }
                        // states a page without script or pointer never has
                        _ => c.never = true,
                    }
                    if pseudo != 0 {
                        spec += 1;
                    } else if counts {
                        spec += 1000;
                    }
                }
                b'&' => return None,
                _ => {
                    let n = ident(b, &mut i);
                    if n.is_empty() {
                        return None;
                    }
                    match name_id(&n.to_ascii_lowercase()) {
                        Some(a) => c.tag = a,
                        None => c.never = true,
                    }
                    spec += 1;
                }
            }
        }
        if !any {
            return None;
        }
        parts.push(c);
    }
    if parts.is_empty() || pending.is_some() {
        return None;
    }
    parts.reverse();
    combs.reverse();
    Some(Sel { parts, combs, spec, pseudo })
}

/// Whether `sel` matches element `n`.
pub fn matches(dom: &Dom, sel: &Sel, n: NodeId) -> bool {
    match_from(dom, sel, 0, n)
}

fn match_from(dom: &Dom, sel: &Sel, k: usize, n: NodeId) -> bool {
    if !compound(dom, &sel.parts[k], n) {
        return false;
    }
    if k + 1 == sel.parts.len() {
        return true;
    }
    match sel.combs[k] {
        Comb::Child => dom.parent_elem(n).is_some_and(|p| match_from(dom, sel, k + 1, p)),
        Comb::Desc => {
            let mut p = dom.parent_elem(n);
            while let Some(a) = p {
                if match_from(dom, sel, k + 1, a) {
                    return true;
                }
                p = dom.parent_elem(a);
            }
            false
        }
        Comb::Adj => dom.prev_elem(n).is_some_and(|p| match_from(dom, sel, k + 1, p)),
        Comb::Sib => {
            let mut p = dom.prev_elem(n);
            while let Some(a) = p {
                if match_from(dom, sel, k + 1, a) {
                    return true;
                }
                p = dom.prev_elem(a);
            }
            false
        }
    }
}

fn compound(dom: &Dom, c: &Compound, n: NodeId) -> bool {
    if c.never {
        return false;
    }
    let Some(e) = dom.elem(n) else { return false };
    if c.tag != NONE && c.tag != e.tag {
        return false;
    }
    if c.id != NONE && c.id != e.id {
        return false;
    }
    if !c.classes.iter().all(|k| e.classes.contains(k)) {
        return false;
    }
    for a in &c.attrs {
        let Some(v) = e.attrs.iter().find(|(k, _)| *k == a.name).map(|(_, v)| v.as_str()) else { return false };
        let (v, want) = if a.ci { (v.to_ascii_lowercase(), a.val.to_ascii_lowercase()) } else { (v.to_string(), a.val.clone()) };
        let ok = match a.op {
            0 => true,
            b'=' => v == want,
            b'~' => v.split_ascii_whitespace().any(|w| w == want),
            b'|' => v == want || v.starts_with(&format!("{want}-")),
            b'^' => !want.is_empty() && v.starts_with(&want),
            b'$' => !want.is_empty() && v.ends_with(&want),
            b'*' => !want.is_empty() && v.contains(&want),
            _ => false,
        };
        if !ok {
            return false;
        }
    }
    for p in &c.pcs {
        let ok = match p {
            Pc::FirstChild => dom.prev_elem(n).is_none(),
            Pc::LastChild => dom.next_elem(n).is_none(),
            Pc::OnlyChild => dom.prev_elem(n).is_none() && dom.next_elem(n).is_none(),
            Pc::FirstOfType | Pc::LastOfType | Pc::OnlyOfType => {
                let before = sibs(dom, n, false, true) == 0;
                let after = sibs(dom, n, true, true) == 0;
                match p {
                    Pc::FirstOfType => before,
                    Pc::LastOfType => after,
                    _ => before && after,
                }
            }
            Pc::Nth(a, b, last, of_type) => {
                let pos = sibs(dom, n, *last, *of_type) as i32 + 1;
                if *a == 0 { pos == *b } else { (pos - b) % a == 0 && (pos - b) / a >= 0 }
            }
            Pc::Not(list) => !list.iter().any(|s| s.pseudo == 0 && matches(dom, s, n)),
            Pc::Is(list) => list.iter().any(|s| s.pseudo == 0 && matches(dom, s, n)),
            Pc::Root => e.tag == t::html,
            Pc::Empty => dom.children(n).all(|c| dom.text(c).is_some_and(|s| s.is_empty())),
            Pc::Link => matches!(e.tag, t::a | t::area) && e.attrs.iter().any(|(k, _)| *k == t::href),
            Pc::Checked => e.attrs.iter().any(|(k, _)| *k == t::checked || *k == t::selected),
            Pc::Disabled => e.attrs.iter().any(|(k, _)| *k == t::disabled),
            Pc::Enabled => matches!(e.tag, t::input | t::button | t::select | t::textarea) && !e.attrs.iter().any(|(k, _)| *k == t::disabled),
            Pc::Defined => !super::dom::name(e.tag).contains('-'),
        };
        if !ok {
            return false;
        }
    }
    true
}

/// Element siblings before (or after) `n`, optionally of the same tag.
fn sibs(dom: &Dom, n: NodeId, after: bool, same_tag: bool) -> usize {
    let tag = dom.tag(n);
    let mut k = 0;
    let mut c = if after { dom.next_elem(n) } else { dom.prev_elem(n) };
    while let Some(x) = c {
        if !same_tag || dom.tag(x) == tag {
            k += 1;
        }
        c = if after { dom.next_elem(x) } else { dom.prev_elem(x) };
    }
    k
}

/// Whether a media query list holds for this viewport (css px).
pub fn media_matches(q: &str, vw: f32, vh: f32) -> bool {
    q.split('\u{1}').all(|part| media_list(part, vw, vh))
}

fn media_list(q: &str, vw: f32, vh: f32) -> bool {
    let q = q.trim();
    if q.is_empty() {
        return true;
    }
    split_top(q, b',').into_iter().any(|one| media_one(&one.trim().to_ascii_lowercase(), vw, vh))
}

fn media_one(q: &str, vw: f32, vh: f32) -> bool {
    let (neg, q) = match q.strip_prefix("not ") {
        Some(r) => (true, r.trim()),
        None => (false, q.strip_prefix("only ").unwrap_or(q).trim()),
    };
    let mut ok = true;
    let mut any_or = false;
    let mut or_ok = false;
    for term in q.split(" and ").flat_map(|x| x.split(" or ").map(move |y| (x.contains(" or "), y))) {
        let (is_or, term) = term;
        let term = term.trim();
        let v = if let Some(inner) = term.strip_prefix('(').and_then(|x| x.strip_suffix(')')) {
            feature(inner.trim(), vw, vh)
        } else {
            matches!(term, "screen" | "all" | "")
        };
        if is_or {
            any_or = true;
            or_ok |= v;
        } else {
            ok &= v;
        }
    }
    if any_or {
        ok &= or_ok;
    }
    ok != neg
}

fn css_px(v: &str) -> Option<f32> {
    let v = v.trim();
    let num: String = v.chars().take_while(|c| c.is_ascii_digit() || *c == '.' || *c == '-').collect();
    let n: f32 = num.parse().ok()?;
    let unit = &v[num.len()..];
    Some(match unit.trim() {
        "em" | "rem" => n * 16.0,
        _ => n,
    })
}

fn feature(f: &str, vw: f32, vh: f32) -> bool {
    if let Some((name, val)) = f.split_once(':') {
        let val = val.trim();
        return match name.trim() {
            "min-width" => css_px(val).is_some_and(|x| vw >= x),
            "max-width" => css_px(val).is_some_and(|x| vw <= x),
            "min-height" => css_px(val).is_some_and(|x| vh >= x),
            "max-height" => css_px(val).is_some_and(|x| vh <= x),
            "orientation" => (val == "landscape") == (vw >= vh),
            "prefers-color-scheme" => val == "light",
            "prefers-reduced-motion" => val == "no-preference",
            "hover" | "any-hover" => val == "hover",
            "pointer" | "any-pointer" => val == "fine",
            "scripting" => val == "none",
            "update" => val == "fast",
            "color" => true,
            _ => false,
        };
    }
    // range syntax: (width >= 600px), (400px <= width < 700px)
    let ops = ["<=", ">=", "<", ">", "="];
    let mut toks: Vec<String> = Vec::new();
    let mut rest = f;
    'outer: while !rest.is_empty() {
        for op in ops {
            if let Some(p) = rest.find(op) {
                let before = rest[..p].trim();
                if !before.is_empty() && !ops.iter().any(|o| before.contains(o)) {
                    toks.push(before.to_string());
                    toks.push(op.to_string());
                    rest = &rest[p + op.len()..];
                    continue 'outer;
                }
            }
        }
        toks.push(rest.trim().to_string());
        break;
    }
    if toks.len() < 3 {
        return matches!(f, "color" | "hover" | "any-hover" | "pointer");
    }
    let val = |s: &str| -> Option<f32> {
        match s {
            "width" => Some(vw),
            "height" => Some(vh),
            _ => css_px(s),
        }
    };
    let mut k = 0;
    while k + 2 < toks.len() {
        let (Some(a), Some(b)) = (val(&toks[k]), val(&toks[k + 2])) else { return false };
        let ok = match toks[k + 1].as_str() {
            "<=" => a <= b,
            ">=" => a >= b,
            "<" => a < b,
            ">" => a > b,
            _ => (a - b).abs() < 0.01,
        };
        if !ok {
            return false;
        }
        k += 2;
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::web::parse::parse;

    #[test]
    fn selectors_match() {
        let dom = parse(r#"<div id="main" class="content wide"><ul><li>a</li><li class="x">b</li></ul><p lang="en-US">p</p></div>"#);
        let li2 = (0..dom.nodes.len() as u32).filter(|&n| dom.tag(n) == t::li).nth(1).unwrap();
        let p = dom.find(t::p).unwrap();
        for (s, n, want) in [
            ("#main .x", li2, true),
            ("div > li", li2, false),
            ("ul li:last-child", li2, true),
            ("li:nth-child(2n)", li2, true),
            ("li:first-child", li2, false),
            ("li + li", li2, true),
            ("ul ~ p", p, true),
            ("p[lang|=en]", p, true),
            ("p[lang^='en' i]", p, true),
            ("div:not(.wide) p", p, false),
            (":is(.content, .nope) > p", p, true),
            ("p:hover", p, false),
            (".does-not-exist", p, false),
        ] {
            let sel = parse_sel(s).unwrap_or_else(|| panic!("{s}"));
            assert_eq!(matches(&dom, &sel, n), want, "{s}");
        }
    }

    #[test]
    fn sheet_structure() {
        let s = parse_sheet(
            "/* c */ @import url('a.css'); a { color: red !important; --x: 1px } @media (max-width: 600px) { a { color: blue } } @media print { b { x: y } } .c { d: e; &:hover { f: g } h: i }",
        );
        assert_eq!(s.imports, vec!["a.css".to_string()]);
        assert_eq!(s.rules.len(), 4);
        assert!(s.rules[0].decls[0].important);
        assert_eq!(&*s.rules[0].decls[1].name, "--x");
        assert!(media_matches(&s.media[s.rules[1].media as usize], 500.0, 800.0));
        assert!(!media_matches(&s.media[s.rules[1].media as usize], 900.0, 800.0));
        assert!(!media_matches(&s.media[s.rules[2].media as usize], 900.0, 800.0));
        assert_eq!(s.rules[3].decls.len(), 2);
        assert!(media_matches("(width >= 600px) and (orientation: landscape)", 900.0, 600.0));
        assert!(media_matches("(400px <= width <= 700px)", 500.0, 600.0));
    }

    #[test]
    fn specificity() {
        let a = parse_sel("#a .b c").unwrap();
        assert_eq!(a.spec, 1_001_001);
        let w = parse_sel(":where(#a) c").unwrap();
        assert_eq!(w.spec, 1);
    }
}

