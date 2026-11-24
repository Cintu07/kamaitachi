//! The browser part: any https page, read as text. The html streams through the
//! extractor straight into the tab's arena; blocks and links go to the ui as they finish,
//! so a long page shows its top before the rest has arrived.

use super::*;
use crate::html::{Html, PageSink, K_PRE};
use crate::net::Host;
use crate::store::{PageBlock, PageLink};
use crate::url::Url;

const MAX_REDIRECTS: u8 = 8;

pub struct WebJob {
    pub tab: TabId,
    pub url: Url,
    html: Html,
    st: PageState,
    redirects: u8,
    started: bool,
    kind: u8,
}

#[derive(Default)]
struct PageState {
    open: bool,
    kind: u8,
    depth: u8,
    start: u32,
    pending: Option<String>,
    link_at: Option<(u32, String)>,
    links: Vec<(u32, u32, String)>,
    nlinks: u32,
    nblocks: u32,
    title: String,
    overflow: bool,
}

struct PageCtx<'a> {
    st: &'a mut PageState,
    out: &'a mut Vec<Reply>,
    arena: &'a TextArena,
    base: &'a Url,
    tab: TabId,
}

impl PageCtx<'_> {
    fn push(&mut self, b: &[u8]) {
        if !self.arena.push(b) {
            self.st.overflow = true;
        }
    }
}

impl PageSink for PageCtx<'_> {
    fn block_start(&mut self, kind: u8, depth: u8) {
        self.st.open = true;
        self.st.kind = kind;
        self.st.depth = depth;
        self.st.start = self.arena.writer_len();
        self.st.links.clear();
    }

    fn text(&mut self, b: &[u8]) {
        if let Some(href) = self.st.pending.take() {
            self.st.link_at = Some((self.arena.writer_len(), href));
        }
        self.push(b);
    }

    fn block_end(&mut self) {
        if !self.st.open {
            return;
        }
        self.link_end();
        self.st.open = false;
        let text = Span { off: self.st.start, len: self.arena.writer_len() - self.st.start };
        if text.len == 0 && self.st.kind != crate::html::K_HR {
            return;
        }
        // the block's text is complete, so hrefs can go into the arena after it
        let first = self.st.nlinks;
        let links = std::mem::take(&mut self.st.links);
        for (start, len, href) in &links {
            let Some(h) = self.arena.append(href.as_bytes()) else {
                self.st.overflow = true;
                break;
            };
            self.out.push(Reply::Link { tab: self.tab, link: PageLink { start: *start, len: *len, href: h } });
            self.st.nlinks += 1;
        }
        self.st.links = links;
        let block = PageBlock { kind: self.st.kind, depth: self.st.depth, text, first_link: first, links: self.st.nlinks - first };
        self.out.push(Reply::Block { tab: self.tab, block });
        self.st.nblocks += 1;
    }

    fn link_start(&mut self, href: &[u8]) {
        let href = crate::text::utf8_prefix(href);
        self.st.pending = self.base.join(href).map(|u| u.full());
    }

    fn link_end(&mut self) {
        self.st.pending = None;
        if let Some((start, href)) = self.st.link_at.take() {
            let len = self.arena.writer_len() - start;
            if len > 0 {
                self.st.links.push((start, len, href));
            }
        }
    }

    fn title(&mut self, b: &[u8]) {
        if self.st.title.len() < 300 {
            self.st.title.push_str(crate::text::utf8_prefix(b));
        }
    }

    fn reset(&mut self) {
        // the ui drops what it has; link numbering starts again
        self.st.nlinks = 0;
        self.st.nblocks = 0;
        self.st.links.clear();
        let block = PageBlock { kind: crate::html::K_RESET, ..PageBlock::default() };
        self.out.push(Reply::Block { tab: self.tab, block });
    }
}

fn request(tab: TabId, url: Url, redirects: u8) -> Req {
    let host = Host::web(&url.host);
    Req {
        tab: Some(tab),
        service: Service::Web,
        host,
        method: "GET",
        target: url.target(),
        headers: vec![
            ("Accept", b"text/html,text/plain;q=0.9,*/*;q=0.1".to_vec()),
            ("Accept-Language", b"en".to_vec()),
        ],
        body: Vec::new(),
        what: url.display(),
        job: Job::Web(Box::new(WebJob { tab, url, html: Html::new(), st: PageState::default(), redirects, started: false, kind: 0 })),
        est_micros: 0,
        units: 0,
        idle_timeout: 20,
    }
}

pub fn open(sh: &mut Shared, tab: TabId) {
    let Some(Source::Web { url }) = sh.tabs.get(&tab).map(|t| t.src.clone()) else {
        // the start page has nothing to load
        return super::done(sh, tab, false);
    };
    match Url::parse(&url) {
        Some(u) => sh.followups.push(request(tab, u, 0)),
        None => {
            sh.status(Some(tab), Level::Error, &format!("not an address: {url}"));
            super::done(sh, tab, false);
        }
    }
}

pub fn on_body(sh: &mut Shared, job: &mut WebJob, ctype: u8, body: &[u8]) {
    let Some(arena) = sh.arena(job.tab) else { return };
    if !job.started {
        job.started = true;
        job.kind = ctype;
        if ctype == 2 {
            // plain text: one preformatted block
            let mut ctx = PageCtx { st: &mut job.st, out: &mut sh.out, arena: &arena, base: &job.url, tab: job.tab };
            ctx.block_start(K_PRE, 0);
        }
    }
    let mut ctx = PageCtx { st: &mut job.st, out: &mut sh.out, arena: &arena, base: &job.url, tab: job.tab };
    match job.kind {
        0 | 1 => job.html.feed(body, &mut ctx),
        2 => ctx.text(body),
        _ => {}
    }
}

pub fn done(sh: &mut Shared, job: WebJob) -> u32 {
    let WebJob { tab, url, mut html, mut st, kind, .. } = job;
    let Some(arena) = sh.arena(tab) else { return 0 };
    {
        let mut ctx = PageCtx { st: &mut st, out: &mut sh.out, arena: &arena, base: &url, tab };
        if kind == 2 {
            ctx.block_end();
        } else {
            html.finish(&mut ctx);
        }
    }
    let title = arena.append(st.title.trim().as_bytes()).unwrap_or_default();
    let u = arena.append(url.full().as_bytes()).unwrap_or_default();
    sh.out.push(Reply::PageInfo { tab, title, url: u });
    if kind == 3 {
        sh.status(Some(tab), Level::Warn, "this address is not a web page (an image or a file); o opens it in your browser");
    } else if st.overflow {
        sh.status(Some(tab), Level::Warn, "page cut at the 1 mb tab budget");
    } else {
        let kb = arena.writer_len() as f64 / 1024.0;
        sh.status(Some(tab), Level::Info, &format!("{} blocks · {} links · {:.0} kb of text", st.nblocks, st.nlinks, kb));
    }
    super::done(sh, tab, false);
    0
}

/// A 3xx: follow it on the same tab. Returns false when it cannot be followed.
pub fn redirect(sh: &mut Shared, job: &WebJob, location: &str) -> bool {
    if job.redirects >= MAX_REDIRECTS {
        sh.status(Some(job.tab), Level::Error, "too many redirects");
        return false;
    }
    let Some(next) = job.url.join(location) else { return false };
    sh.followups.push(request(job.tab, next, job.redirects + 1));
    true
}
