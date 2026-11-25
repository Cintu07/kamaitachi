//! Cookies for the light engine, so a site can remember who signed in. Only the site in
//! the address bar may set or read them: an image or stylesheet from somewhere else gets
//! none, which is most of what tracking cookies are for.
//!
//! A cookie here is kept only when the rules of RFC 6265bis let it in. The light engine
//! runs no scripts and speaks only https, so HttpOnly and Secure hold without any work.

use crate::web::block::site;
use std::sync::{Mutex, OnceLock};

const MAX_BYTES: usize = 4096;
const MAX_PER_SITE: usize = 120;
const MAX_TOTAL: usize = 2000;
/// Nothing is kept longer than 400 days, as in Chrome and Firefox.
const MAX_AGE: u64 = 400 * 86_400;

/// Endings that many owners share. A cookie for the whole of one would be read by all of
/// them, so a Domain naming one of these is refused. The full public suffix list is
/// 200 KB; these are the shared hosts people sign in to, and the two-part country endings
/// are covered by [`site`].
const SHARED: &[&str] = &[
    "github.io", "gitlab.io", "pages.dev", "workers.dev", "netlify.app", "vercel.app", "herokuapp.com", "appspot.com", "web.app", "firebaseapp.com", "blogspot.com", "azurewebsites.net",
    "cloudfront.net", "amazonaws.com", "fly.dev", "onrender.com", "glitch.me", "repl.co", "replit.app", "ngrok.io", "ngrok-free.app", "wordpress.com", "tumblr.com", "myshopify.com",
    "surge.sh", "deno.dev", "r2.dev", "streamlit.app", "hf.space", "neocities.org", "codeberg.page", "sourceforge.io", "readthedocs.io",
];

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum SameSite {
    Strict,
    Lax,
    None,
}

#[derive(Clone, PartialEq, Debug)]
struct Cookie {
    name: String,
    value: String,
    /// the host that set it, or the domain it asked for
    host: String,
    /// only that exact host, not the ones under it
    host_only: bool,
    path: String,
    same: SameSite,
    /// unix seconds; 0 for a cookie that goes when the browser closes
    until: u64,
}

/// Why a request is being made, which decides the cookies it may carry.
#[derive(Clone, Copy, Debug)]
pub struct Purpose<'a> {
    /// host of the page in the address bar, or of the page whose link or form led here;
    /// empty when the address was typed
    pub from: &'a str,
    /// the document itself, rather than something a page includes
    pub page: bool,
    pub post: bool,
}

#[derive(Default)]
struct Jar {
    all: Vec<Cookie>,
    changed: bool,
}

fn jar() -> &'static Mutex<Jar> {
    static JAR: OnceLock<Mutex<Jar>> = OnceLock::new();
    JAR.get_or_init(Default::default)
}

fn lock() -> std::sync::MutexGuard<'static, Jar> {
    jar().lock().unwrap_or_else(|e| e.into_inner())
}

fn now() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

/// The Cookie header for a request, or None when it carries none.
pub fn header(host: &str, path: &str, why: Purpose) -> Option<String> {
    lock().header(host, path, why, now())
}

/// Takes in one Set-Cookie header.
pub fn store(line: &str, host: &str, path: &str, why: Purpose) {
    lock().store(line, host, path, why, now());
}

/// How many sites have something kept.
pub fn sites() -> usize {
    let j = lock();
    let mut s: Vec<&str> = j.all.iter().map(|c| site(&c.host)).collect();
    s.sort_unstable();
    s.dedup();
    s.len()
}

/// Signs out of everything.
pub fn forget() {
    let mut j = lock();
    j.all.clear();
    j.changed = true;
}

/// What has to be written to disk, if anything changed since the last call. Cookies that
/// end with the session are left out.
pub fn unsaved() -> Option<String> {
    let mut j = lock();
    if !std::mem::take(&mut j.changed) {
        return None;
    }
    Some(j.write(now()))
}

/// Puts back what [`unsaved`] wrote in an earlier run.
pub fn restore(text: &str) {
    lock().read(text, now());
}

impl Jar {
    fn header(&mut self, host: &str, path: &str, why: Purpose, now: u64) -> Option<String> {
        let same_site = why.from.is_empty() || site(why.from) == site(host);
        if !same_site && !why.page {
            return None;
        }
        self.all.retain(|c| c.until == 0 || c.until > now);
        let mut send: Vec<&Cookie> = self
            .all
            .iter()
            .filter(|c| if c.host_only { c.host == host } else { under(host, &c.host) })
            .filter(|c| path_matches(path, &c.path))
            .filter(|c| match c.same {
                SameSite::None => true,
                SameSite::Lax => same_site || !why.post,
                SameSite::Strict => same_site,
            })
            .collect();
        // the most specific path first; that is the one a server reads
        send.sort_by_key(|c| std::cmp::Reverse(c.path.len()));
        (!send.is_empty()).then(|| send.iter().map(|c| if c.name.is_empty() { c.value.clone() } else { format!("{}={}", c.name, c.value) }).collect::<Vec<_>>().join("; "))
    }

    fn store(&mut self, line: &str, host: &str, path: &str, why: Purpose, now: u64) {
        if !why.page && site(why.from) != site(host) {
            return;
        }
        let Some(c) = parse(line, host, path, now) else { return };
        // SameSite cookies may not be set by a request that another site's form made
        if c.same != SameSite::None && why.post && !why.from.is_empty() && site(why.from) != site(host) {
            return;
        }
        let old = self.all.iter().position(|o| o.name == c.name && o.host == c.host && o.host_only == c.host_only && o.path == c.path);
        if let Some(i) = old {
            let was = self.all.remove(i);
            self.changed |= was != c;
        } else {
            self.changed = true;
        }
        if c.until != 0 && c.until <= now {
            return;
        }
        let s = site(&c.host).to_string();
        if self.all.iter().filter(|o| site(&o.host) == s).count() >= MAX_PER_SITE
            && let Some(i) = self.all.iter().position(|o| site(&o.host) == s)
        {
            self.all.remove(i);
        }
        if self.all.len() >= MAX_TOTAL {
            self.all.remove(0);
        }
        self.all.push(c);
    }

    /// One cookie a line, tab separated. Neither a tab nor a line break can be in one.
    fn write(&self, now: u64) -> String {
        let mut out = String::new();
        for c in self.all.iter().filter(|c| c.until > now) {
            let same = match c.same {
                SameSite::Strict => 's',
                SameSite::Lax => 'l',
                SameSite::None => 'n',
            };
            out.push_str(&format!("{}\t{}\t{}\t{same}\t{}\t{}\t{}\n", c.host, if c.host_only { '=' } else { '.' }, c.path, c.until, c.name, c.value));
        }
        out
    }

    fn read(&mut self, text: &str, now: u64) {
        for l in text.lines() {
            let f: Vec<&str> = l.splitn(7, '\t').collect();
            let [host, only, path, same, until, name, value] = f[..] else { continue };
            let until = until.parse().unwrap_or(0);
            if until <= now {
                continue;
            }
            let same = match same {
                "s" => SameSite::Strict,
                "n" => SameSite::None,
                _ => SameSite::Lax,
            };
            self.all.push(Cookie { name: name.into(), value: value.into(), host: host.into(), host_only: only == "=", path: path.into(), same, until });
        }
    }
}

/// Whether `host` is `domain` or a host under it.
fn under(host: &str, domain: &str) -> bool {
    host == domain || host.strip_suffix(domain).is_some_and(|h| h.ends_with('.'))
}

fn path_matches(path: &str, cookie: &str) -> bool {
    let path = path.split(['?', '#']).next().unwrap_or("/");
    path.strip_prefix(cookie).is_some_and(|rest| rest.is_empty() || cookie.ends_with('/') || rest.starts_with('/'))
}

/// The directory a cookie belongs to when it does not say.
fn default_path(path: &str) -> String {
    let p = path.split('?').next().unwrap_or("/");
    match p.rfind('/') {
        Some(i) if i > 0 => p[..i].to_string(),
        _ => "/".to_string(),
    }
}

fn parse(line: &str, host: &str, path: &str, now: u64) -> Option<Cookie> {
    if line.len() > MAX_BYTES || line.bytes().any(|b| b < 0x20 || b == 0x7f) {
        return None;
    }
    let mut parts = line.split(';');
    let (name, value) = parts.next()?.split_once('=').unwrap_or(("", line.split(';').next()?));
    let (name, value) = (name.trim(), value.trim());
    if name.is_empty() && value.is_empty() {
        return None;
    }
    let mut c = Cookie { name: name.into(), value: value.into(), host: host.into(), host_only: true, path: default_path(path), same: SameSite::Lax, until: 0 };
    let (mut secure, mut max_age, mut has_path) = (false, None, false);
    for a in parts {
        let (k, v) = a.split_once('=').unwrap_or((a, ""));
        let v = v.trim();
        match k.trim().to_ascii_lowercase().as_str() {
            "domain" => {
                let d = v.trim_start_matches('.').to_ascii_lowercase();
                if d.is_empty() {
                    continue;
                }
                // a site may widen a cookie to its own site, and no further
                let d_is_shared = !d.contains('.') || SHARED.contains(&d.as_str()) || site(&d).len() > d.len() || (d != host && site(host).len() > d.len());
                if !under(host, &d) || (d_is_shared && d != host) {
                    return None;
                }
                c.host_only = d_is_shared;
                c.host = d;
            }
            "path" if v.starts_with('/') => {
                c.path = v.into();
                has_path = v == "/";
            }
            "secure" => secure = true,
            "max-age" => max_age = v.parse::<i64>().ok(),
            "expires" => c.until = date(v).map_or(0, |t| t.max(1)),
            "samesite" => {
                c.same = match v.to_ascii_lowercase().as_str() {
                    "strict" => SameSite::Strict,
                    "none" => SameSite::None,
                    _ => SameSite::Lax,
                }
            }
            _ => {}
        }
    }
    if let Some(a) = max_age {
        c.until = if a <= 0 { 1 } else { now + a as u64 };
    }
    if c.until > now + MAX_AGE {
        c.until = now + MAX_AGE;
    }
    if c.same == SameSite::None && !secure {
        c.same = SameSite::Lax;
    }
    // the two name prefixes are promises the server makes about how it was set
    let low = c.name.to_ascii_lowercase();
    if low.starts_with("__secure-") && !secure {
        return None;
    }
    if low.starts_with("__host-") && !(secure && c.host_only && has_path) {
        return None;
    }
    Some(c)
}

/// Unix time of a cookie date. Servers write these in several shapes, so the pieces are
/// found by what they look like and not by where they are.
fn date(s: &str) -> Option<u64> {
    const MONTHS: [&str; 12] = ["jan", "feb", "mar", "apr", "may", "jun", "jul", "aug", "sep", "oct", "nov", "dec"];
    let (mut day, mut month, mut year, mut time) = (None, None, None, None);
    for tok in s.split(|c: char| !(c.is_ascii_alphanumeric() || c == ':')).filter(|t| !t.is_empty()) {
        let low = tok.to_ascii_lowercase();
        if tok.contains(':') {
            let mut p = tok.split(':').map(|n| n.parse::<u64>().ok());
            if let (Some(Some(h)), Some(Some(m)), Some(Some(sec))) = (p.next(), p.next(), p.next()) {
                time = Some(h * 3600 + m * 60 + sec);
            }
        } else if let Some(m) = low.get(..3).and_then(|m| MONTHS.iter().position(|x| *x == m)) {
            month = Some(m as u64 + 1);
        } else if let Ok(n) = tok.parse::<u64>() {
            if day.is_none() && tok.len() <= 2 {
                day = Some(n);
            } else if year.is_none() {
                year = Some(match n {
                    0..=69 => 2000 + n,
                    70..=99 => 1900 + n,
                    _ => n,
                });
            }
        }
    }
    let (d, m, y, t) = (day?, month?, year?, time?);
    if !(1..=31).contains(&d) || !(1601..=9999).contains(&y) || t >= 86_400 {
        return None;
    }
    // days since 1970, counting years from March so a leap day is the last of its year
    let (y, m) = if m <= 2 { (y as i64 - 1, m as i64 + 9) } else { (y as i64, m as i64 - 3) };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + (153 * m + 2) / 5 + d as i64 - 1;
    let days = era * 146_097 + doe - 719_468;
    Some((days * 86_400 + t as i64).max(0) as u64)
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: u64 = 1_800_000_000;
    const TYPED: Purpose = Purpose { from: "", page: true, post: false };

    fn visit(from: &str) -> Purpose<'_> {
        Purpose { from, page: true, post: false }
    }

    fn jar(lines: &[&str], host: &str, path: &str) -> Jar {
        let mut j = Jar::default();
        for l in lines {
            j.store(l, host, path, TYPED, NOW);
        }
        j
    }

    #[test]
    fn a_sign_in_is_sent_back_to_the_host_that_set_it() {
        let mut j = jar(&["sid=abc; Path=/; Secure; HttpOnly", "theme=paper"], "news.example.com", "/login/now");
        assert_eq!(j.header("news.example.com", "/", TYPED, NOW).as_deref(), Some("sid=abc"));
        // no Path means the directory it came from, and the longer path goes first
        assert_eq!(j.header("news.example.com", "/login/x?y", TYPED, NOW).as_deref(), Some("theme=paper; sid=abc"));
        assert_eq!(j.header("news.example.com", "/loginx", TYPED, NOW).as_deref(), Some("sid=abc"));
        assert_eq!(j.header("example.com", "/", TYPED, NOW), None);
        assert_eq!(j.header("evil-news.example.com", "/", TYPED, NOW), None);
    }

    #[test]
    fn a_domain_reaches_subdomains_but_never_a_shared_ending() {
        let mut j = jar(&["a=1; Domain=.example.co.uk; Path=/"], "www.example.co.uk", "/");
        assert!(j.header("shop.example.co.uk", "/", TYPED, NOW).is_some());
        assert!(j.header("notexample.co.uk", "/", TYPED, NOW).is_none());
        for (d, host) in [("co.uk", "a.co.uk"), ("com", "a.com"), ("github.io", "me.github.io"), ("other.com", "a.com"), ("workers.dev", "a.b.workers.dev")] {
            let j = jar(&[&format!("x=1; Domain={d}; Path=/")], host, "/");
            assert!(j.all.is_empty(), "{host} set a cookie for {d}");
        }
    }

    #[test]
    fn other_sites_cannot_use_a_sign_in() {
        let mut j = jar(&["lax=1; Path=/", "strict=1; Path=/; SameSite=Strict", "none=1; Path=/; SameSite=None; Secure"], "bank.com", "/");
        let forged = Purpose { from: "evil.com", page: true, post: true };
        assert_eq!(j.header("bank.com", "/pay", forged, NOW).as_deref(), Some("none=1"));
        assert_eq!(j.header("bank.com", "/", visit("evil.com"), NOW).as_deref(), Some("lax=1; none=1"));
        assert_eq!(j.header("bank.com", "/", visit("www.bank.com"), NOW).as_deref(), Some("lax=1; strict=1; none=1"));
        // an image on another page neither sends nor sets anything
        let image = Purpose { from: "blog.org", page: false, post: false };
        assert_eq!(j.header("bank.com", "/pixel.png", image, NOW), None);
        j.store("track=1; Path=/; SameSite=None; Secure", "bank.com", "/", image, NOW);
        assert_eq!(j.all.len(), 3);
    }

    #[test]
    fn cookies_expire_and_can_be_removed() {
        let mut j = jar(&["a=1; Path=/; Max-Age=60", "b=1; Path=/; Expires=Wed, 21 Oct 2015 07:28:00 GMT"], "x.com", "/");
        assert_eq!(j.header("x.com", "/", TYPED, NOW + 59).as_deref(), Some("a=1"));
        assert_eq!(j.header("x.com", "/", TYPED, NOW + 61), None);
        let mut j = jar(&["a=1; Path=/", "a=2; Path=/", "b=1; Path=/", "b=; Path=/; Max-Age=0"], "x.com", "/");
        assert_eq!(j.header("x.com", "/", TYPED, NOW).as_deref(), Some("a=2"));
    }

    #[test]
    fn dates_in_the_shapes_servers_write() {
        assert_eq!(date("Wed, 21 Oct 2015 07:28:00 GMT"), Some(1_445_412_480));
        assert_eq!(date("Wednesday, 21-Oct-15 07:28:00 GMT"), Some(1_445_412_480));
        assert_eq!(date("Thu, 01 Jan 1970 00:00:00 GMT"), Some(0));
        assert_eq!(date("Thu, 29 Feb 2024 12:00:00 GMT"), Some(1_709_208_000));
        assert_eq!(date("tomorrow"), None);
    }

    #[test]
    fn prefixed_names_must_keep_their_promise() {
        for bad in ["__Host-a=1; Path=/", "__Host-a=1; Secure; Path=/x", "__Host-a=1; Secure; Path=/; Domain=x.com", "__Secure-a=1", "=", "a=\u{1}"] {
            assert!(parse(bad, "x.com", "/", NOW).is_none(), "{bad:?}");
        }
        assert!(parse("__Host-a=1; Secure; Path=/", "x.com", "/", NOW).is_some());
        assert!(parse(&format!("a={}", "x".repeat(5000)), "x.com", "/", NOW).is_none());
    }

    #[test]
    fn only_lasting_cookies_go_to_disk() {
        let j = jar(&["s=1; Path=/", "keep=me; Path=/; Max-Age=1000; SameSite=Strict"], "x.com", "/");
        let text = j.write(NOW);
        assert_eq!(text, "x.com\t=\t/\ts\t1800001000\tkeep\tme\n");
        let mut back = Jar::default();
        back.read(&text, NOW);
        assert_eq!(back.all, j.all[1..]);
        let mut late = Jar::default();
        late.read(&text, NOW + 2000);
        assert!(late.all.is_empty());
    }
}
