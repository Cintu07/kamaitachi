//! Forms: what each control holds, and the request a form makes when it is sent. Typing
//! changes the document itself (the `value` of an input, the text of a textarea), so
//! nothing has to keep a second copy of the page's state. The value a control started
//! with is kept beside it the first time it changes, for reset buttons.

use super::dom::{atom, t, Dom, Kind, NodeId, NONE};
use crate::url::Url;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Control {
    Text,
    Password,
    Area,
    Check,
    Radio,
    Select,
    Submit,
    Reset,
    /// a button that only a script would do something with
    Button,
    File,
    Hidden,
}

impl Control {
    /// Whether it is a box to type in.
    pub fn typed(self) -> bool {
        matches!(self, Control::Text | Control::Password | Control::Area)
    }
}

pub fn control(dom: &Dom, n: NodeId) -> Option<Control> {
    let ty = || dom.attr(n, t::type_).unwrap_or("").to_ascii_lowercase();
    Some(match dom.tag(n) {
        t::textarea => Control::Area,
        t::select => Control::Select,
        t::button => match ty().as_str() {
            "reset" => Control::Reset,
            "button" => Control::Button,
            _ => Control::Submit,
        },
        t::input => match ty().as_str() {
            "password" => Control::Password,
            "checkbox" => Control::Check,
            "radio" => Control::Radio,
            "submit" | "image" => Control::Submit,
            "reset" => Control::Reset,
            "button" => Control::Button,
            "file" => Control::File,
            "hidden" => Control::Hidden,
            _ => Control::Text,
        },
        _ => return None,
    })
}

/// A control that can be used: not disabled, and not in a disabled fieldset.
pub fn enabled(dom: &Dom, n: NodeId) -> bool {
    if dom.attr(n, t::disabled).is_some() {
        return false;
    }
    let mut p = dom.parent(n);
    while p != NONE {
        if dom.tag(p) == t::fieldset && dom.attr(p, t::disabled).is_some() {
            return false;
        }
        p = dom.parent(p);
    }
    true
}

pub fn value(dom: &Dom, n: NodeId) -> String {
    match control(dom, n) {
        Some(Control::Area) => {
            let mut s = String::new();
            dom.text_content(n, &mut s);
            // the line break right after <textarea> is not part of the text
            s.strip_prefix('\n').map(str::to_string).unwrap_or(s)
        }
        Some(Control::Select) => options(dom, n).into_iter().find(|o| o.selected).map_or(String::new(), |o| o.value),
        Some(Control::Check | Control::Radio) => dom.attr(n, t::value).unwrap_or("on").to_string(),
        _ => dom.attr(n, t::value).unwrap_or("").to_string(),
    }
}

/// Where a control's first value is kept. No page can write an attribute with this name.
fn was() -> u32 {
    atom("\u{1}was")
}

pub fn set_value(dom: &mut Dom, n: NodeId, v: &str) {
    if dom.attr(n, was()).is_none() {
        let old = value(dom, n);
        dom.set_attr(n, was(), &old);
    }
    put_value(dom, n, v);
}

fn put_value(dom: &mut Dom, n: NodeId, v: &str) {
    if dom.tag(n) != t::textarea {
        return dom.set_attr(n, t::value, v);
    }
    // value() drops one leading line break, so a text that starts with one needs two
    let v = if v.starts_with('\n') { format!("\n{v}") } else { v.to_string() };
    let first = dom.nodes[n as usize].first;
    match dom.nodes.get_mut(first as usize).map(|c| &mut c.kind) {
        Some(Kind::Text(s)) => *s = v,
        _ => {
            dom.push(n, Kind::Text(v));
        }
    }
}

pub struct Choice {
    pub node: NodeId,
    pub label: String,
    pub value: String,
    pub selected: bool,
    pub disabled: bool,
}

/// The options of a select. One with nothing marked shows its first usable option, as
/// long as it can only hold one.
pub fn options(dom: &Dom, n: NodeId) -> Vec<Choice> {
    fn gather(dom: &Dom, n: NodeId, off: bool, out: &mut Vec<Choice>) {
        for c in dom.children(n) {
            let off = off || dom.attr(c, t::disabled).is_some();
            match dom.tag(c) {
                t::optgroup => gather(dom, c, off, out),
                t::option => {
                    let mut s = String::new();
                    dom.text_content(c, &mut s);
                    let text = s.split_whitespace().collect::<Vec<_>>().join(" ");
                    out.push(Choice {
                        node: c,
                        value: dom.attr(c, t::value).unwrap_or(&text).to_string(),
                        label: dom.attr(c, t::label).unwrap_or(&text).to_string(),
                        selected: dom.attr(c, t::selected).is_some(),
                        disabled: off,
                    });
                }
                _ => {}
            }
        }
    }
    let mut v = Vec::new();
    gather(dom, n, false, &mut v);
    if dom.attr(n, t::multiple).is_none() {
        // the last one marked wins
        let pick = v.iter().rposition(|o| o.selected).or_else(|| v.iter().position(|o| !o.disabled));
        for (i, o) in v.iter_mut().enumerate() {
            o.selected = Some(i) == pick;
        }
    }
    v
}

/// Picks an option. In a list that takes several, picks or unpicks it.
pub fn choose(dom: &mut Dom, select: NodeId, option: NodeId) {
    let many = dom.attr(select, t::multiple).is_some();
    let all: Vec<NodeId> = options(dom, select).iter().map(|o| o.node).collect();
    if dom.attr(select, was()).is_none() {
        let marked = all.iter().filter(|o| dom.attr(**o, t::selected).is_some());
        let old = marked.map(|o| o.to_string()).collect::<Vec<_>>().join(" ");
        dom.set_attr(select, was(), &old);
    }
    for o in all {
        let on = dom.attr(o, t::selected).is_some();
        let want = if many { on != (o == option) } else { o == option };
        set_flag(dom, o, t::selected, want);
    }
}

fn set_flag(dom: &mut Dom, n: NodeId, flag: u32, on: bool) {
    let is = dom.attr(n, flag).is_some();
    if on && !is {
        dom.set_attr(n, flag, "");
    } else if is && !on {
        dom.remove_attr(n, flag);
    }
}

fn set_checked(dom: &mut Dom, n: NodeId, on: bool) {
    if dom.attr(n, was()).is_none() {
        let old = if dom.attr(n, t::checked).is_some() { "1" } else { "" };
        dom.set_attr(n, was(), old);
    }
    set_flag(dom, n, t::checked, on);
}

/// Ticks or clears a checkbox, or picks a radio button and clears the rest of its group.
pub fn toggle(dom: &mut Dom, n: NodeId) {
    if control(dom, n) == Some(Control::Check) {
        let on = dom.attr(n, t::checked).is_some();
        return set_checked(dom, n, !on);
    }
    let group = dom.attr(n, t::name).map(str::to_string);
    let form = owner(dom, n);
    for o in 0..dom.nodes.len() as NodeId {
        let same = o != n && group.is_some() && control(dom, o) == Some(Control::Radio) && dom.attr(o, t::name) == group.as_deref() && owner(dom, o) == form;
        if same && dom.attr(o, t::checked).is_some() {
            set_checked(dom, o, false);
        }
    }
    set_checked(dom, n, true);
}

/// The form a control belongs to.
pub fn owner(dom: &Dom, n: NodeId) -> Option<NodeId> {
    if let Some(id) = dom.attr(n, t::form) {
        return by_id(dom, id).filter(|f| dom.tag(*f) == t::form);
    }
    let mut p = dom.parent(n);
    let mut table = NONE;
    while p != NONE {
        match dom.tag(p) {
            t::form => return Some(p),
            t::table => table = p,
            _ => {}
        }
        p = dom.parent(p);
    }
    // <table><form><tr>: the parser closes the form at once, and the controls in the rows
    // after it still belong to it
    (table..n).rev().find(|&f| dom.tag(f) == t::form && dom.children(f).next().is_none())
}

fn by_id(dom: &Dom, id: &str) -> Option<NodeId> {
    let a = super::dom::lookup(id)?;
    (0..dom.nodes.len() as NodeId).find(|&n| dom.elem(n).is_some_and(|e| e.id == a))
}

/// The control a label is for.
pub fn labelled(dom: &Dom, label: NodeId) -> Option<NodeId> {
    let usable = |n: &NodeId| !matches!(control(dom, *n), None | Some(Control::Hidden));
    if let Some(id) = dom.attr(label, atom("for")) {
        return by_id(dom, id).filter(usable);
    }
    inside(dom, label).into_iter().find(usable)
}

/// Everything under `n`, in document order.
fn inside(dom: &Dom, n: NodeId) -> Vec<NodeId> {
    let mut out = Vec::new();
    for c in dom.children(n) {
        out.push(c);
        out.extend(inside(dom, c));
    }
    out
}

fn controls_of(dom: &Dom, form: NodeId) -> Vec<NodeId> {
    (0..dom.nodes.len() as NodeId).filter(|&n| control(dom, n).is_some() && owner(dom, n) == Some(form)).collect()
}

/// The button Enter presses: the form's first submit button.
pub fn default_button(dom: &Dom, form: NodeId) -> Option<NodeId> {
    controls_of(dom, form).into_iter().find(|&n| control(dom, n) == Some(Control::Submit))
}

pub fn reset(dom: &mut Dom, form: NodeId) {
    for n in controls_of(dom, form) {
        let Some(old) = dom.attr(n, was()).map(str::to_string) else { continue };
        match control(dom, n) {
            Some(Control::Check | Control::Radio) => set_checked(dom, n, old == "1"),
            Some(Control::Select) => {
                let marked: Vec<NodeId> = old.split_whitespace().filter_map(|o| o.parse().ok()).collect();
                for o in options(dom, n) {
                    set_flag(dom, o.node, t::selected, marked.contains(&o.node));
                }
            }
            _ => put_value(dom, n, &old),
        }
    }
}

/// The first control that stops the form from being sent, and what to say about it.
pub fn problem(dom: &Dom, form: NodeId, button: Option<NodeId>) -> Option<(NodeId, &'static str)> {
    if dom.attr(form, atom("novalidate")).is_some() || button.is_some_and(|b| dom.attr(b, atom("formnovalidate")).is_some()) {
        return None;
    }
    let all = controls_of(dom, form);
    all.iter().find_map(|&n| Some((n, complaint(dom, n, &all)?)))
}

fn complaint(dom: &Dom, n: NodeId, all: &[NodeId]) -> Option<&'static str> {
    let c = control(dom, n)?;
    if !enabled(dom, n) || dom.attr(n, atom("readonly")).is_some() {
        return None;
    }
    let v = value(dom, n);
    if dom.attr(n, atom("required")).is_some() {
        let name = dom.attr(n, t::name);
        let picked = |o: &&NodeId| control(dom, **o) == Some(Control::Radio) && dom.attr(**o, t::name) == name && dom.attr(**o, t::checked).is_some();
        match c {
            Control::Check if dom.attr(n, t::checked).is_none() => return Some("this has to be ticked"),
            Control::Radio if !all.iter().any(|o| picked(&o)) => return Some("pick one of these"),
            Control::Select if v.is_empty() => return Some("pick one of these"),
            _ if c.typed() && v.is_empty() => return Some("fill this in first"),
            _ => {}
        }
    }
    if v.is_empty() || !c.typed() {
        return None;
    }
    let email = dom.attr(n, t::type_).is_some_and(|ty| ty.eq_ignore_ascii_case("email"));
    if email && !v.split_once('@').is_some_and(|(a, b)| !a.is_empty() && !b.is_empty() && !b.contains('@')) {
        return Some("that is not an email address");
    }
    let least = dom.attr(n, atom("minlength")).and_then(|m| m.trim().parse().ok()).unwrap_or(0);
    if dom.attr(n, was()).is_some() && v.encode_utf16().count() < least {
        return Some("that is too short");
    }
    None
}

/// A request a form makes.
#[derive(Clone, Debug, PartialEq)]
pub struct Send {
    pub url: Url,
    /// content type and body of a POST; None for a GET
    pub body: Option<(String, Vec<u8>)>,
}

/// What pressing `button` (or Enter) sends, or None when the form goes nowhere. `doc` is
/// the page's own address, where a form with no action goes.
pub fn submit(dom: &Dom, form: NodeId, button: Option<NodeId>, doc: &Url, base: &Url) -> Option<Send> {
    let setting = |on_button: &str, on_form: &str| button.and_then(|b| dom.attr(b, atom(on_button))).or(dom.attr(form, atom(on_form))).unwrap_or("").trim();
    let method = setting("formmethod", "method").to_ascii_lowercase();
    if method == "dialog" {
        return None;
    }
    let action = setting("formaction", "action");
    let mut url = if action.is_empty() { doc.clone() } else { base.join(action)? };
    let fields = entries(dom, form, button);
    if method != "post" {
        let path = url.path.split('?').next().unwrap_or("/").to_string();
        url.path = format!("{path}?{}", String::from_utf8_lossy(&urlencoded(&fields)));
        return Some(Send { url, body: None });
    }
    let body = match setting("formenctype", "enctype").to_ascii_lowercase().as_str() {
        "multipart/form-data" => multipart(&fields),
        "text/plain" => ("text/plain".to_string(), fields.iter().map(|(k, v)| format!("{k}={v}\r\n")).collect::<String>().into_bytes()),
        _ => ("application/x-www-form-urlencoded".to_string(), urlencoded(&fields)),
    };
    Some(Send { url, body: Some(body) })
}

/// The names and values a form sends, in document order.
fn entries(dom: &Dom, form: NodeId, button: Option<NodeId>) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for n in controls_of(dom, form) {
        let Some(c) = control(dom, n) else { continue };
        if !enabled(dom, n) || (c == Control::Submit && button != Some(n)) {
            continue;
        }
        let name = dom.attr(n, t::name).unwrap_or("");
        if c == Control::Submit && dom.attr(n, t::type_).is_some_and(|v| v.eq_ignore_ascii_case("image")) {
            // a picture that submits sends where it was clicked
            let dot = if name.is_empty() { String::new() } else { format!("{name}.") };
            out.push((format!("{dot}x"), "1".into()));
            out.push((format!("{dot}y"), "1".into()));
            continue;
        }
        if name.is_empty() {
            continue;
        }
        match c {
            Control::Reset | Control::Button => {}
            Control::Check | Control::Radio if dom.attr(n, t::checked).is_none() => {}
            Control::Select => out.extend(options(dom, n).into_iter().filter(|o| o.selected && !o.disabled).map(|o| (name.to_string(), o.value))),
            Control::Area => out.push((name.into(), value(dom, n).replace('\n', "\r\n"))),
            Control::Hidden if name == "_charset_" => out.push((name.into(), "UTF-8".into())),
            _ => out.push((name.into(), value(dom, n))),
        }
    }
    out
}

fn urlencoded(fields: &[(String, String)]) -> Vec<u8> {
    fn put(out: &mut Vec<u8>, s: &str) {
        for b in s.bytes() {
            match b {
                b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9' | b'*' | b'-' | b'.' | b'_' => out.push(b),
                b' ' => out.push(b'+'),
                _ => out.extend_from_slice(format!("%{b:02X}").as_bytes()),
            }
        }
    }
    let mut out = Vec::new();
    for (k, v) in fields {
        if !out.is_empty() {
            out.push(b'&');
        }
        put(&mut out, k);
        out.push(b'=');
        put(&mut out, v);
    }
    out
}

fn multipart(fields: &[(String, String)]) -> (String, Vec<u8>) {
    // the boundary must not appear in any value, so it grows until it does not
    let mut mark = String::from("----kamaitachi");
    while fields.iter().any(|(k, v)| k.contains(&mark) || v.contains(&mark)) {
        mark.push('7');
    }
    let mut out = String::new();
    for (k, v) in fields {
        let k = k.replace('"', "%22").replace('\r', "%0D").replace('\n', "%0A");
        out.push_str(&format!("--{mark}\r\nContent-Disposition: form-data; name=\"{k}\"\r\n\r\n{v}\r\n"));
    }
    out.push_str(&format!("--{mark}--\r\n"));
    (format!("multipart/form-data; boundary={mark}"), out.into_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::web::parse::parse;

    fn find(dom: &Dom, name: &str) -> NodeId {
        (0..dom.nodes.len() as NodeId).find(|&n| dom.attr(n, t::name) == Some(name)).unwrap_or_else(|| panic!("no {name}"))
    }

    fn send(dom: &Dom, button: Option<NodeId>) -> Option<Send> {
        let doc = Url::parse("https://site.com/dir/page?old=1").unwrap();
        submit(dom, dom.find(t::form).unwrap(), button, &doc, &doc)
    }

    fn type_in(dom: &mut Dom, name: &str, v: &str) {
        let n = find(dom, name);
        set_value(dom, n, v);
    }

    #[test]
    fn a_search_becomes_a_query() {
        let mut d = parse("<form action=/search?gone=1><input name=q><input type=hidden name=hl value=en><input type=submit name=go value=Go></form>");
        type_in(&mut d, "q", "kama itachi & 鎌鼬");
        assert_eq!(send(&d, None).unwrap().url.full(), "https://site.com/search?q=kama+itachi+%26+%E9%8E%8C%E9%BC%AC&hl=en");
        let go = find(&d, "go");
        assert!(send(&d, Some(go)).unwrap().url.path.ends_with("&hl=en&go=Go"));
    }

    #[test]
    fn a_sign_in_is_posted() {
        let mut d = parse("<form method=POST action=login><input name=acct><input type=password name=pw><input type=checkbox name=keep checked><input type=checkbox name=no><button>in</button></form>");
        type_in(&mut d, "acct", "me");
        type_in(&mut d, "pw", "a=b&c");
        let s = send(&d, None).unwrap();
        assert_eq!(s.url.full(), "https://site.com/dir/login");
        assert_eq!(s.body, Some(("application/x-www-form-urlencoded".into(), b"acct=me&pw=a%3Db%26c&keep=on".to_vec())));
    }

    #[test]
    fn no_action_goes_back_to_the_page() {
        let d = parse("<form method=post><input name=a value=1></form>");
        assert_eq!(send(&d, None).unwrap().url.full(), "https://site.com/dir/page?old=1");
        let d = parse("<form action='javascript:x()'><input name=a></form>");
        assert!(send(&d, None).is_none());
    }

    #[test]
    fn choices_and_text_areas() {
        let mut d = parse(
            "<form method=post><select name=s><option value=1>one<optgroup label=g><option>two<option disabled>three</optgroup></select>\
             <input type=radio name=r value=a checked><input type=radio name=r value=b id=b><textarea name=t>\nhi</textarea>\
             <input name=off disabled value=x><fieldset disabled><input name=off2 value=x></fieldset></form>",
        );
        assert_eq!(value(&d, find(&d, "t")), "hi");
        let s = find(&d, "s");
        assert_eq!(value(&d, s), "1");
        let two = options(&d, s)[1].node;
        choose(&mut d, s, two);
        let b = by_id(&d, "b").unwrap();
        toggle(&mut d, b);
        type_in(&mut d, "t", "\na\nb");
        assert_eq!(value(&d, find(&d, "t")), "\na\nb");
        assert_eq!(send(&d, None).unwrap().body.unwrap().1, b"s=two&r=b&t=%0D%0Aa%0D%0Ab".to_vec());
        let f = d.find(t::form).unwrap();
        reset(&mut d, f);
        assert_eq!(send(&d, None).unwrap().body.unwrap().1, b"s=1&r=a&t=hi".to_vec());
    }

    #[test]
    fn controls_find_their_form() {
        let d = parse("<input name=far form=f><form id=f action=/x></form><table><form action=/y><tr><td><input name=cell></table>");
        let f = by_id(&d, "f").unwrap();
        assert_eq!(owner(&d, find(&d, "far")), Some(f));
        let cell = owner(&d, find(&d, "cell")).expect("a form in a table");
        assert_eq!(d.attr(cell, atom("action")), Some("/y"));
        let d = parse("<label for=e>Email</label><input id=e name=e><label>Keep <input type=checkbox name=k></label>");
        let labels: Vec<NodeId> = (0..d.nodes.len() as NodeId).filter(|&n| d.tag(n) == t::label).collect();
        assert_eq!(labels.iter().map(|&l| labelled(&d, l)).collect::<Vec<_>>(), [Some(find(&d, "e")), Some(find(&d, "k"))]);
    }

    #[test]
    fn a_form_says_what_is_missing() {
        let mut d = parse("<form><input name=e type=email required><input type=radio name=r required><button formnovalidate name=skip>x</button></form>");
        let f = d.find(t::form).unwrap();
        let (e, r, skip) = (find(&d, "e"), find(&d, "r"), find(&d, "skip"));
        assert_eq!(problem(&d, f, None).map(|p| p.0), Some(e));
        type_in(&mut d, "e", "me@");
        assert_eq!(problem(&d, f, None).map(|p| p.0), Some(e));
        type_in(&mut d, "e", "me@x");
        assert_eq!(problem(&d, f, None).map(|p| p.0), Some(r));
        assert_eq!(problem(&d, f, Some(skip)), None);
        toggle(&mut d, r);
        assert_eq!(problem(&d, f, None), None);
    }

    #[test]
    fn multipart_bodies() {
        let mut d = parse("<form method=post enctype=multipart/form-data><input name='a\"b' value=1><input type=hidden name=c value='----kamaitachi'></form>");
        type_in(&mut d, "a\"b", "x y");
        let (ty, body) = send(&d, None).unwrap().body.unwrap();
        assert_eq!(ty, "multipart/form-data; boundary=----kamaitachi7");
        assert!(String::from_utf8(body).unwrap().starts_with("------kamaitachi7\r\nContent-Disposition: form-data; name=\"a%22b\"\r\n\r\nx y\r\n"));
    }
}
