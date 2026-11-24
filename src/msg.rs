//! What the two threads say to each other. Commands go ui -> net, replies net -> ui, each
//! through one spsc ring. Replies are small fixed-size values; text stays in the arenas.

use std::sync::Arc;

use crate::store::{Msg, Post, Span, TextArena};

pub type TabId = u16;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(u8)]
pub enum Service {
    X = 0,
    YouTube = 1,
    Claude = 2,
    ChatGpt = 3,
    Local = 4,
    Web = 5,
}

pub const SERVICE_NAMES: [&str; 6] = ["x", "youtube", "claude", "chatgpt", "history", "web"];

impl Service {
    pub fn name(self) -> &'static str {
        SERVICE_NAMES[self as usize]
    }
    pub fn is_chat(self) -> bool {
        matches!(self, Service::Claude | Service::ChatGpt)
    }
}

/// What a tab shows and where its rows come from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Source {
    XHome,
    XMentions,
    XThread { root: Box<str> },
    XSearch { q: Box<str> },
    YtSubs,
    YtComments { video: Box<str> },
    YtSearch { q: Box<str> },
    /// a chat; `chat` is a local id that names it in the log
    Chat { service: Service, chat: u32 },
    LocalSearch { q: Box<str> },
    /// a web page, read as text
    Web { url: Box<str> },
    /// a new tab's start page: search box and shortcuts
    Start,
}

impl Source {
    /// A tab named in the config: x:home x:mentions yt:subs claude chatgpt.
    pub fn from_config(s: &str) -> Option<Source> {
        Some(match s {
            "x:home" => Source::XHome,
            "x:mentions" => Source::XMentions,
            "yt:subs" | "youtube" => Source::YtSubs,
            "claude" => Source::Chat { service: Service::Claude, chat: 0 },
            "chatgpt" => Source::Chat { service: Service::ChatGpt, chat: 0 },
            "start" => Source::Start,
            "hn" => Source::Web { url: "https://news.ycombinator.com/".into() },
            s if s.starts_with("web:") => Source::Web { url: crate::url::from_input(&s[4..]).full().into() },
            _ => return None,
        })
    }

    pub fn service(&self) -> Service {
        match self {
            Source::XHome | Source::XMentions | Source::XThread { .. } | Source::XSearch { .. } => Service::X,
            Source::YtSubs | Source::YtComments { .. } | Source::YtSearch { .. } => Service::YouTube,
            Source::Chat { service, .. } => *service,
            Source::LocalSearch { .. } => Service::Local,
            Source::Web { .. } | Source::Start => Service::Web,
        }
    }

    /// The key a feed's posts are stored under in the log.
    pub fn feed_key(&self) -> String {
        match self {
            Source::XHome => "x:home".into(),
            Source::XMentions => "x:mentions".into(),
            Source::XThread { root } => format!("x:thread:{root}"),
            Source::XSearch { q } => format!("x:search:{q}"),
            Source::YtSubs => "yt:subs".into(),
            Source::YtComments { video } => format!("yt:comments:{video}"),
            Source::YtSearch { q } => format!("yt:search:{q}"),
            Source::Chat { chat, .. } => format!("chat:{chat}"),
            Source::LocalSearch { q } => format!("local:{q}"),
            Source::Web { url } => format!("web:{url}"),
            Source::Start => "start".into(),
        }
    }
}

pub enum Command {
    Open { tab: TabId, arena: Arc<TextArena>, src: Source },
    /// scrolled past the end: one more page, from the log first, then the service
    More { tab: TabId },
    /// fetch what is newer than the newest row held
    Refresh { tab: TabId },
    /// send a chat message
    Send { tab: TabId, text: Box<str>, model: Box<str>, system: Box<str> },
    /// switch a chat's provider or model; a provider change starts a new branch
    Branch { tab: TabId, service: Service, model: Box<str> },
    /// a chat's system prompt, from the next message on
    System { tab: TabId, text: Box<str> },
    Post { tab: TabId, text: Box<str>, reply_to: Option<Box<str>> },
    Like { tab: TabId, sid: Box<str> },
    /// archive: the tab's text is dropped (it is all in the log) and its memory returned
    Close { tab: TabId },
    /// a video finished playing at this position
    Position { video: Box<str>, secs: u32 },
    Shutdown,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Level {
    Info,
    Warn,
    Error,
}

/// A short line for the status bar, inline so replies stay fixed size.
#[derive(Clone, Copy)]
pub struct Line {
    pub buf: [u8; 120],
    pub len: u8,
}

impl Line {
    pub fn new(s: &str) -> Line {
        let mut buf = [0u8; 120];
        let s = crate::text::utf8_prefix(&s.as_bytes()[..s.len().min(120)]);
        buf[..s.len()].copy_from_slice(s.as_bytes());
        Line { buf, len: s.len() as u8 }
    }
    pub fn as_str(&self) -> &str {
        crate::text::utf8_prefix(&self.buf[..self.len as usize])
    }
}

impl core::fmt::Debug for Line {
    fn fmt(&self, f: &mut core::fmt::Formatter) -> core::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// One finished request, for :net.
#[derive(Clone, Copy, Debug)]
pub struct NetEntry {
    pub service: Service,
    pub what: Line,
    pub status: u16,
    pub bytes_out: u32,
    pub bytes_in: u32,
    pub ttfb_ms: u32,
    pub total_ms: u32,
    /// money in micro-dollars, or quota units for youtube
    pub cost: u32,
}

pub enum Reply {
    Post { tab: TabId, post: Post },
    /// an author's name and handle, in the shared author arena
    Author { idx: u32, name: Span, handle: Span },
    Msg { tab: TabId, msg: Msg },
    /// the last message of a chat tab grew to `len` bytes
    Grow { tab: TabId, len: u32 },
    /// a request for this tab finished; `more` says whether older rows exist
    Done { tab: TabId, more: bool },
    Status { tab: Option<TabId>, level: Level, line: Line, retry_at: u64 },
    Net(NetEntry),
    /// month-to-date spend in micro-dollars per paid service, youtube units used today
    Spend { claude: u64, chatgpt: u64, x: u64, yt_units: u32 },
    Liked { tab: TabId, sid_hash: u32 },
    /// a block of a web page: a paragraph, heading, list item, code or quote
    Block { tab: TabId, block: crate::store::PageBlock },
    /// a link inside a block's text
    Link { tab: TabId, link: crate::store::PageLink },
    /// a page finished: its title and final address (after redirects)
    PageInfo { tab: TabId, title: Span, url: Span },
    /// the chat a chat tab is showing, once the net thread has resolved "new" or "latest"
    /// to an id, or after a provider switch started a branch
    ChatId { tab: TabId, service: Service, chat: u32 },
}
