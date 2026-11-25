//! Fetching a document and its stylesheets. Subresources load in parallel on scoped
//! threads; each request is one blocking https exchange.

use super::css::{parse_sheet, parse_sheet_interned, Sheet};
use std::sync::{Arc, OnceLock};
use super::dom::{t, Dom};
use super::parse::parse;
use crate::net::conn::{full, Host};
use crate::net::cookies::{self, Purpose};
use crate::url::Url;

/// What pages see: a chrome-compatible string, since many sites turn away browsers they
/// do not know, with hayate's own name on the end.
pub const UA: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/142.0.0.0 Safari/537.36 hayate/0.1";

pub const UA_CSS: &str = include_str!("ua.css");
/// What browsers still do for pages without a doctype.
pub const QUIRKS_CSS: &str = "table { text-align: start; white-space: normal; line-height: normal; font-weight: normal; font-size: medium; font-style: normal; }";

pub struct Fetched {
    pub url: Url,
    /// 1 html, 2 plain text, 3 other
    pub kind: u8,
    pub body: Vec<u8>,
}

/// A page to open: typed, linked to, or the answer to a form.
#[derive(Clone)]
pub struct Request {
    pub url: Url,
    /// content type and body of a form's POST
    pub body: Option<(String, Vec<u8>)>,
    /// host of the page the link or form was on; empty when the address was typed
    pub from: String,
}

impl Request {
    pub fn get(url: Url) -> Request {
        Request { url, body: None, from: String::new() }
    }
}

/// Opens a page, following redirects, with the cookies its site has set.
fn fetch_page(req: &Request) -> Result<Fetched, String> {
    let accept = "text/html,application/xhtml+xml,text/plain;q=0.9,*/*;q=0.5";
    exchange(&req.url, accept, &req.from, true, req.body.as_ref())
}

/// Something a page on `page_host` includes.
fn fetch(url: &Url, accept: &str, page_host: &str) -> Result<Fetched, String> {
    exchange(url, accept, page_host, false, None)
}

fn exchange(url: &Url, accept: &str, from: &str, page: bool, body: Option<&(String, Vec<u8>)>) -> Result<Fetched, String> {
    let mut u = url.clone();
    let mut body = body;
    let origin = format!("https://{from}");
    let referer = format!("{origin}/");
    for _ in 0..12 {
        let why = Purpose { from, page, post: body.is_some() };
        let cookie = cookies::header(&u.host, &u.path, why);
        let mut headers: Vec<(&str, &[u8])> = vec![("Accept", accept.as_bytes()), ("Accept-Language", b"en-US,en;q=0.8"), ("User-Agent", UA.as_bytes())];
        if let Some(c) = &cookie {
            headers.push(("Cookie", c.as_bytes()));
        }
        if let Some((ty, _)) = body {
            headers.push(("Content-Type", ty.as_bytes()));
            if !from.is_empty() {
                // what a server checks to know its own form sent this
                headers.push(("Origin", origin.as_bytes()));
                headers.push(("Referer", referer.as_bytes()));
            }
        }
        let method = if body.is_some() { "POST" } else { "GET" };
        let sent = body.map_or(&b""[..], |(_, b)| b);
        let (resp, got) = full(Host::web(&u.host), None, method, &u.target(), &headers, sent).map_err(|e| format!("{}: {e}", u.host))?;
        for line in &resp.cookies {
            cookies::store(&String::from_utf8_lossy(line), &u.host, &u.path, why);
        }
        if (300..400).contains(&resp.status) && !resp.location.is_empty() {
            let to = String::from_utf8_lossy(&resp.location).to_string();
            u = u.join(&to).ok_or_else(|| format!("redirect to {to}"))?;
            // a form that worked sends you to a page to look at; only 307 and 308 ask for
            // the form again
            if !matches!(resp.status, 307 | 308) {
                body = None;
            }
            continue;
        }
        if resp.encoded {
            return Err(format!("{} sent a compressed body", u.host));
        }
        if resp.status >= 400 && got.is_empty() {
            return Err(format!("{} answered {}", u.host, resp.status));
        }
        return Ok(Fetched { url: u, kind: resp.ctype, body: got });
    }
    Err("too many redirects".into())
}

/// Bytes of something a page on `page_host` includes: an https url or a data: url.
pub fn fetch_bytes(url: &str, accept: &str, page_host: &str) -> Result<Vec<u8>, String> {
    if let Some(rest) = url.strip_prefix("data:") {
        let (meta, data) = rest.split_once(',').ok_or("bad data url")?;
        return if meta.ends_with(";base64") { base64(data).ok_or_else(|| "bad base64".to_string()) } else { Ok(crate::url::percent_decode(data).into_bytes()) };
    }
    let u = Url::parse(url).ok_or("bad url")?;
    Ok(fetch(&u, accept, page_host)?.body)
}

fn base64(s: &str) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(s.len() * 3 / 4);
    let mut acc = 0u32;
    let mut bits = 0;
    for c in s.bytes() {
        let v = match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' | b'-' => 62,
            b'/' | b'_' => 63,
            b'=' | b' ' | b'\n' | b'\r' => continue,
            b'%' => return None,
            _ => return None,
        } as u32;
        acc = (acc << 6) | v;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
        }
    }
    Some(out)
}

/// Text of a document: utf-8, or windows-1252 when it says so or is not utf-8.
pub fn decode_text(b: &[u8]) -> String {
    let head = String::from_utf8_lossy(&b[..b.len().min(2048)]).to_ascii_lowercase();
    let latin = ["charset=windows-1252", "charset=\"windows-1252\"", "charset=iso-8859-1", "charset=\"iso-8859-1\"", "charset=latin1"].iter().any(|c| head.contains(c));
    match std::str::from_utf8(b) {
        Ok(s) if !latin => s.to_string(),
        _ => b.iter().map(|&c| c as char).collect(),
    }
}

/// Rewrites url(...) in a stylesheet fetched from `sheet_url` to absolute urls, so
/// backgrounds resolve against the sheet and not the page.
fn absolutize(css: &str, sheet_url: &Url) -> String {
    let mut out = String::with_capacity(css.len());
    let mut rest = css;
    while let Some(p) = rest.find("url(") {
        out.push_str(&rest[..p + 4]);
        rest = &rest[p + 4..];
        let e = rest.find(')').unwrap_or(rest.len());
        let raw = rest[..e].trim().trim_matches(|c| c == '"' || c == '\'');
        if raw.starts_with("data:") || raw.starts_with("http") || raw.starts_with('#') {
            out.push_str(&rest[..e]);
        } else if let Some(u) = sheet_url.join(raw) {
            out.push('"');
            out.push_str(&u.full());
            out.push('"');
        }
        rest = &rest[e..];
    }
    out.push_str(rest);
    out
}

pub struct Doc {
    pub url: Url,
    /// for links: the document url or its <base href>
    pub base: Url,
    pub dom: Dom,
    /// the user agent's sheet first, then the page's in order, then the blocker's
    pub sheets: Vec<Arc<Sheet>>,
    /// requests the blocker stopped while loading
    pub blocked: u32,
    pub kind: u8,
}

/// The page and every stylesheet it links, ready to cascade.
pub fn load(req: &Request) -> Result<Doc, String> {
    let f = fetch_page(req)?;
    let text = decode_text(&f.body);
    let html = if f.kind == 2 || (f.kind == 3 && !text.trim_start().starts_with('<')) {
        // plain text shows as preformatted text
        format!("<pre style=\"white-space:pre-wrap\">{}</pre>", text.replace('&', "&amp;").replace('<', "&lt;"))
    } else {
        text
    };
    Ok(from_html(&html, f.url, f.kind))
}

/// Builds a document from markup already in hand (the start page, tests).
pub fn from_html(html: &str, url: Url, kind: u8) -> Doc {
    let dom = parse(html);
    let mut base = url.clone();
    if let Some(b) = dom.find(t::base).and_then(|b| dom.attr(b, t::href))
        && let Some(u) = url.join(b)
    {
        base = u;
    }
    // stylesheets in document order: (external url) or (inline text)
    enum Src {
        Link(String),
        Inline(String),
    }
    let blocker = super::block::get();
    let page_host = url.host.clone();
    let mut blocked = 0u32;
    let mut srcs: Vec<Src> = Vec::new();
    for n in 0..dom.nodes.len() as u32 {
        match dom.tag(n) {
            t::link => {
                let rel = dom.attr(n, t::rel).unwrap_or("").to_ascii_lowercase();
                let media = dom.attr(n, t::media).unwrap_or("").to_ascii_lowercase();
                if rel.split_whitespace().any(|r| r == "stylesheet") && !rel.contains("alternate") && !media.contains("print") && !media.contains("speech")
                    && let Some(u) = dom.attr(n, t::href).and_then(|h| base.join(h))
                {
                    srcs.push(Src::Link(u.full()));
                }
            }
            t::style => {
                let mut s = String::new();
                dom.text_content(n, &mut s);
                srcs.push(Src::Inline(s));
            }
            _ => {}
        }
    }
    if let Some(b) = &blocker {
        srcs.retain(|s| match s {
            Src::Link(u) if b.blocks(u, &page_host, super::block::Kind::Stylesheet) => {
                blocked += 1;
                false
            }
            _ => true,
        });
    }
    let links: Vec<String> = srcs.iter().filter_map(|s| if let Src::Link(u) = s { Some(u.clone()) } else { None }).collect();
    let fetched = fetch_all(&links, &page_host);
    static UA: OnceLock<Arc<Sheet>> = OnceLock::new();
    static QUIRKS: OnceLock<Arc<Sheet>> = OnceLock::new();
    let mut sheets = vec![UA.get_or_init(|| Arc::new(parse_sheet_interned(UA_CSS))).clone()];
    if dom.quirks {
        sheets.push(QUIRKS.get_or_init(|| Arc::new(parse_sheet_interned(QUIRKS_CSS))).clone());
    }

    let mut k = 0;
    for s in srcs {
        let (css, from) = match s {
            Src::Link(_) => {
                let r = fetched[k].clone();
                k += 1;
                match r {
                    Some((u, css)) => (css, u),
                    None => continue,
                }
            }
            Src::Inline(css) => (css, base.clone()),
        };
        let sheet = parse_sheet(&absolutize(&css, &from));
        if !sheet.imports.is_empty() {
            let urls: Vec<String> = sheet.imports.iter().filter_map(|i| from.join(i)).map(|u| u.full()).collect();
            for (u, css) in fetch_all(&urls, &page_host).into_iter().flatten() {
                sheets.push(Arc::new(parse_sheet(&absolutize(&css, &u))));
            }
        }
        sheets.push(Arc::new(sheet));
    }
    // ads and trackers the page would show: hidden
    if let Some(b) = &blocker {
        let specific = b.site_hiding(&page_host);
        if !specific.is_empty() {
            sheets.push(Arc::new(parse_sheet(&specific)));
        }
    }
    Doc { url, base, dom, sheets, kind, blocked }
}

/// Fetches stylesheets in parallel, in order; None for each that failed.
fn fetch_all(urls: &[String], page_host: &str) -> Vec<Option<(Url, String)>> {
    std::thread::scope(|sc| {
        let hs: Vec<_> = urls
            .iter()
            .map(|u| {
                sc.spawn(move || {
                    let url = Url::parse(u)?;
                    let f = fetch(&url, "text/css,*/*;q=0.1", page_host).ok()?;
                    Some((f.url, decode_text(&f.body)))
                })
            })
            .collect();
        hs.into_iter().map(|h| h.join().ok().flatten()).collect()
    })
}

/// Whether a page is really a JavaScript application, so the light engine would show
/// next to nothing: scripts and almost no text, a notice asking for JavaScript, or a
/// bot check. Such pages go to the full engine instead.
pub fn needs_js(doc: &Doc, dl: &super::DisplayList) -> bool {
    let dom = &doc.dom;
    let scripts = (0..dom.nodes.len() as u32).filter(|&n| dom.tag(n) == t::script).count();
    let mut nos = String::new();
    for n in 0..dom.nodes.len() as u32 {
        if dom.tag(n) == t::noscript {
            dom.text_content(n, &mut nos);
        }
    }
    let nos = nos.to_lowercase();
    let warns = nos.contains("javascript") && ["enable", "required", "disabled", "turn on", "needs", "requires", "please"].iter().any(|w| nos.contains(w));
    let challenge = ["Just a moment", "Attention Required", "Verifying you are human", "Checking your browser"].iter().any(|t| dl.title.contains(t));
    let glyphs = dl.glyphs.len();
    (scripts > 0 && glyphs < 250) || (warns && glyphs < 4000) || challenge
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn data_urls_and_css_urls() {
        assert_eq!(fetch_bytes("data:text/plain;base64,aGk=", "*/*", "").unwrap(), b"hi");
        assert_eq!(fetch_bytes("data:,a%20b", "*/*", "").unwrap(), b"a b");
        let u = Url::parse("https://cdn.example.com/css/site.css").unwrap();
        assert_eq!(absolutize("a{background:url(../img/x.png)} b{background:url('data:x')}", &u), "a{background:url(\"https://cdn.example.com/img/x.png\")} b{background:url('data:x')}");
    }

    #[test]
    fn latin1_pages() {
        assert_eq!(decode_text(b"<meta charset=iso-8859-1>caf\xe9"), "<meta charset=iso-8859-1>café");
        assert_eq!(decode_text("café".as_bytes()), "café");
    }
}
