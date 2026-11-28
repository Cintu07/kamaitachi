//! https urls: parsing what a person types, resolving links against a page, and the
//! request target. Only https on port 443; http is upgraded.

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Url {
    pub host: String,
    /// path and query, always starting with '/'
    pub path: String,
}

impl Url {
    pub fn parse(s: &str) -> Option<Url> {
        let s = s.trim();
        let rest = if let Some(r) = s.strip_prefix("https://") {
            r
        } else if let Some(r) = s.strip_prefix("http://") {
            r
        } else if s.contains("://") {
            return None;
        } else {
            s
        };
        let (hostport, path) = match rest.find(['/', '?', '#']) {
            Some(i) => (&rest[..i], &rest[i..]),
            None => (rest, "/"),
        };
        let host = hostport.rsplit('@').next()?.to_ascii_lowercase();
        let host = host.strip_suffix(":443").or_else(|| host.strip_suffix(":80")).unwrap_or(&host).to_string();
        if host.is_empty() || host.contains(':') || !host.contains('.') || host.contains(' ') {
            return None;
        }
        let path = path.split('#').next().unwrap_or("/");
        let path = if path.starts_with('?') { format!("/{path}") } else if path.is_empty() { "/".into() } else { path.to_string() };
        Some(Url { host, path: normalize(&path) })
    }

    /// A link on this page, made absolute. None for links that go nowhere useful
    /// (javascript:, mailto:, an anchor on the same page).
    pub fn join(&self, href: &str) -> Option<Url> {
        let href = href.trim();
        let low = href.get(..11).unwrap_or(href).to_ascii_lowercase();
        if href.is_empty() || href.starts_with('#') || low.starts_with("javascript:") || low.starts_with("mailto:") || low.starts_with("data:") || low.starts_with("tel:") {
            return None;
        }
        if href.starts_with("https://") || href.starts_with("http://") {
            return Url::parse(href);
        }
        if let Some(r) = href.strip_prefix("//") {
            return Url::parse(r);
        }
        if href.contains("://") {
            return None;
        }
        let href = href.split('#').next().unwrap_or("");
        let path = if href.starts_with('/') {
            href.to_string()
        } else if href.starts_with('?') {
            format!("{}{}", self.path.split('?').next().unwrap_or("/"), href)
        } else {
            let dir = self.path.split('?').next().unwrap_or("/");
            let dir = &dir[..dir.rfind('/').map_or(0, |i| i + 1)];
            format!("{dir}{href}")
        };
        Some(Url { host: self.host.clone(), path: normalize(&path) })
    }

    /// The request target: the path with spaces and non-ascii percent-encoded.
    pub fn target(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(self.path.len());
        for &b in self.path.as_bytes() {
            if b <= b' ' || b >= 0x7f || b == b'"' || b == b'<' || b == b'>' || b == b'`' {
                const H: &[u8; 16] = b"0123456789ABCDEF";
                out.push(b'%');
                out.push(H[(b >> 4) as usize]);
                out.push(H[(b & 15) as usize]);
            } else {
                out.push(b);
            }
        }
        out
    }

    pub fn display(&self) -> String {
        if self.path == "/" { self.host.clone() } else { format!("{}{}", self.host, self.path) }
    }

    pub fn full(&self) -> String {
        format!("https://{}{}", self.host, self.path)
    }

    /// A value of a query parameter, percent-decoded.
    pub fn param(&self, key: &str) -> Option<String> {
        let q = self.path.split_once('?')?.1;
        let v = q.split('&').find_map(|kv| kv.strip_prefix(key).and_then(|v| v.strip_prefix('=')))?;
        Some(percent_decode(v))
    }
}

/// Resolves "." and ".." in the path part.
fn normalize(path: &str) -> String {
    let (p, q) = match path.find('?') {
        Some(i) => (&path[..i], &path[i..]),
        None => (path, ""),
    };
    if !p.contains("/.") {
        return path.to_string();
    }
    let mut segs: Vec<&str> = Vec::new();
    for s in p.split('/').skip(1) {
        match s {
            "." => {}
            ".." => {
                segs.pop();
            }
            s => segs.push(s),
        }
    }
    let trailing = p.ends_with('/') || p.ends_with("/.") || p.ends_with("/..");
    let mut out = String::from("/");
    out.push_str(&segs.join("/"));
    if trailing && !out.ends_with('/') {
        out.push('/');
    }
    out.push_str(q);
    out
}

pub fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            if let Ok(v) = u8::from_str_radix(&s[i + 1..i + 3], 16) {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(if b[i] == b'+' { b' ' } else { b[i] });
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// The search engine's address, with %s where the words go.
static SEARCH: std::sync::RwLock<String> = std::sync::RwLock::new(String::new());

/// Sends searches somewhere else than DuckDuckGo Lite.
pub fn set_search(template: &str) {
    if let Ok(mut s) = SEARCH.write() {
        *s = template.to_string();
    }
}

/// Text made safe for a query string.
pub fn encode(q: &str) -> String {
    let mut e = Vec::new();
    crate::net::http::push_query(&mut e, q.trim());
    String::from_utf8_lossy(&e).into_owned()
}

/// A web search for the text, always (even when it looks like an address).
pub fn search(q: &str) -> Url {
    let template = SEARCH.read().map(|s| s.clone()).unwrap_or_default();
    let template = if template.contains("%s") { template } else { "https://lite.duckduckgo.com/lite/?q=%s".to_string() };
    Url::parse(&template.replace("%s", &encode(q))).unwrap_or_else(|| Url { host: "lite.duckduckgo.com".into(), path: format!("/lite/?q={}", encode(q)) })
}

/// Words that send a search straight to a site: "w kamaitachi" goes to Wikipedia.
/// A ! in front works too, as in "!w kamaitachi".
pub const SHORTCUTS: &[(&str, &str, &str)] = &[
    ("w", "Wikipedia", "https://en.wikipedia.org/w/index.php?search=%s"),
    ("yt", "YouTube", "https://www.youtube.com/results?search_query=%s"),
    ("gh", "GitHub", "https://github.com/search?q=%s"),
    ("r", "Reddit", "https://www.reddit.com/search/?q=%s"),
    ("a", "Amazon", "https://www.amazon.com/s?k=%s"),
    ("m", "Maps", "https://www.google.com/maps/search/%s"),
    ("g", "Google", "https://www.google.com/search?q=%s"),
    ("i", "Images", "https://duckduckgo.com/?ia=images&iax=images&q=%s"),
    ("hn", "Hacker News", "https://hn.algolia.com/?q=%s"),
    ("so", "Stack Overflow", "https://stackoverflow.com/search?q=%s"),
    ("mdn", "MDN", "https://developer.mozilla.org/en-US/search?q=%s"),
    ("rs", "docs.rs", "https://docs.rs/releases/search?query=%s"),
    ("cr", "crates.io", "https://crates.io/search?q=%s"),
    ("x", "X", "https://x.com/search?q=%s"),
];

/// The site a shortcut at the start of typed text goes to, and the address.
pub fn shortcut(s: &str) -> Option<(&'static str, Url)> {
    let (word, rest) = s.trim().split_once(' ')?;
    let word = word.strip_prefix('!').unwrap_or(word).to_ascii_lowercase();
    let rest = rest.trim();
    if rest.is_empty() {
        return None;
    }
    let (_, name, template) = SHORTCUTS.iter().find(|(k, _, _)| *k == word)?;
    Some((name, Url::parse(&template.replace("%s", &encode(rest)))?))
}

/// Whether typed text is an address rather than words to search for.
pub fn looks_like_address(s: &str) -> bool {
    let s = s.trim();
    !s.contains(' ') && s.contains('.') && Url::parse(s).is_some()
}

/// Codes that only say where you clicked from. Taking them out changes nothing on the page.
const TRACKERS: &[&str] = &["gclid", "gclsrc", "dclid", "gbraid", "wbraid", "fbclid", "msclkid", "mc_eid", "mc_cid", "igshid", "yclid", "twclid", "ttclid", "_hsenc", "_hsmi", "mkt_tok", "oly_enc_id", "oly_anon_id", "vero_id", "srsltid"];

impl Url {
    /// The same address with click trackers taken out of its query.
    pub fn without_trackers(&self) -> Url {
        let Some((path, query)) = self.path.split_once('?') else { return self.clone() };
        let kept: Vec<&str> = query
            .split('&')
            .filter(|kv| {
                let k = kv.split('=').next().unwrap_or("").to_ascii_lowercase();
                !(k.starts_with("utm_") || TRACKERS.contains(&k.as_str()))
            })
            .collect();
        let path = if kept.is_empty() { path.to_string() } else { format!("{path}?{}", kept.join("&")) };
        Url { host: self.host.clone(), path }
    }
}

/// What the address bar does with input: an address, or a web search.
pub fn from_input(s: &str) -> Url {
    let s = s.trim();
    if let Some((_, u)) = shortcut(s) {
        return u;
    }
    if !s.contains(' ') && s.contains('.') {
        if let Some(u) = Url::parse(s) {
            return u;
        }
    }
    search(s)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shortcuts_go_straight_to_a_site() {
        assert_eq!(from_input("w sickle weasel").full(), "https://en.wikipedia.org/w/index.php?search=sickle%20weasel");
        assert_eq!(from_input("!gh rust lang").host, "github.com");
        assert_eq!(shortcut("w"), None);
        assert!(shortcut("zz something").is_none());
        // a sentence that starts with a short word is still a search
        assert_eq!(from_input("a").host, search("a").host);
    }

    #[test]
    fn click_trackers_come_out() {
        let u = Url::parse("https://shop.com/p?id=7&utm_source=x&UTM_medium=y&fbclid=abc&color=red").unwrap();
        assert_eq!(u.without_trackers().full(), "https://shop.com/p?id=7&color=red");
        assert_eq!(Url::parse("https://a.com/?gclid=1").unwrap().without_trackers().full(), "https://a.com/");
        assert_eq!(Url::parse("https://a.com/x").unwrap().without_trackers().full(), "https://a.com/x");
    }

    #[test]
    fn parses_what_people_type() {
        let u = Url::parse("news.ycombinator.com").unwrap();
        assert_eq!((u.host.as_str(), u.path.as_str()), ("news.ycombinator.com", "/"));
        let u = Url::parse("http://En.Wikipedia.org/wiki/Rust_(programming_language)#History").unwrap();
        assert_eq!(u.full(), "https://en.wikipedia.org/wiki/Rust_(programming_language)");
        assert!(Url::parse("not a url").is_none());
        assert!(Url::parse("ftp://x.org").is_none());
        assert_eq!(Url::parse("x.org?a=1").unwrap().path, "/?a=1");
    }

    #[test]
    fn joins_links() {
        let base = Url::parse("https://a.com/dir/page?x=1").unwrap();
        assert_eq!(base.join("other").unwrap().full(), "https://a.com/dir/other");
        assert_eq!(base.join("../up").unwrap().full(), "https://a.com/up");
        assert_eq!(base.join("/root").unwrap().full(), "https://a.com/root");
        assert_eq!(base.join("?p=2").unwrap().full(), "https://a.com/dir/page?p=2");
        assert_eq!(base.join("//cdn.b.com/x").unwrap().full(), "https://cdn.b.com/x");
        assert_eq!(base.join("item?id=5#c").unwrap().full(), "https://a.com/dir/item?id=5");
        assert!(base.join("#top").is_none());
        assert!(base.join("javascript:void(0)").is_none());
        assert!(base.join("mailto:a@b.c").is_none());
    }

    #[test]
    fn targets_and_search() {
        let u = Url { host: "a.com".into(), path: "/a b/é".into() };
        assert_eq!(u.target(), b"/a%20b/%C3%A9".to_vec());
        let s = from_input("rust arenas");
        assert_eq!(s.host, "lite.duckduckgo.com");
        assert_eq!(s.param("q").as_deref(), Some("rust arenas"));
        assert_eq!(from_input("lobste.rs").host, "lobste.rs");
        assert_eq!(percent_decode("https%3A%2F%2Fx.com%2Fa"), "https://x.com/a");
    }
}
