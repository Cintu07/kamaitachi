//! The ui thread: turns rows into cells.
//!
//! On screen: a sidebar of tabs, the active view in a centered reading column, and a
//! hint bar with the keys that work right now. Every frame is painted from the visible
//! rows only, diffed against the last frame, and written in one call. Nothing in the
//! frame path allocates. Drawing lives in `draw.rs`; this file is state and input.

mod draw;

use core::fmt::Write as _;
use std::sync::Arc;

use crate::arena::{ArenaId, Bump, Producer};
use crate::config::Config;
use crate::msg::*;
use crate::render::{Fmt, Screen};
use crate::store::*;
use crate::sys::{self, Key, WinEvent};
use crate::text::{next_line, utf8_prefix};
use crate::url::Url;

pub use draw::x_weight;

// ---------------------------------------------------------------- tabs

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum TabState {
    Visible,
    Hidden,
    /// no pixels at all (images are links for now, so this only marks the lru order)
    Asleep,
    /// text dropped; cursor and scroll kept; reopening reads the log, never the network
    Archived,
}

/// A feed as a struct of arrays: scrolling reads a few contiguous arrays.
#[derive(Default)]
pub struct Feed {
    pub sid: Vec<Span>,
    pub author: Vec<u32>,
    pub ts: Vec<u32>,
    pub text: Vec<Span>,
    pub extra: Vec<Span>,
    pub metric: Vec<u32>,
    pub position: Vec<u32>,
    pub flags: Vec<u16>,
    pub depth: Vec<u8>,
    /// feeds are newest first; threads and comments keep the order they came in
    pub by_time: bool,
}

impl Feed {
    fn with_capacity(n: usize, by_time: bool) -> Feed {
        Feed {
            sid: Vec::with_capacity(n),
            author: Vec::with_capacity(n),
            ts: Vec::with_capacity(n),
            text: Vec::with_capacity(n),
            extra: Vec::with_capacity(n),
            metric: Vec::with_capacity(n),
            position: Vec::with_capacity(n),
            flags: Vec::with_capacity(n),
            depth: Vec::with_capacity(n),
            by_time,
        }
    }

    pub fn len(&self) -> usize {
        self.sid.len()
    }

    pub fn is_empty(&self) -> bool {
        self.sid.is_empty()
    }

    fn insert(&mut self, p: Post) -> usize {
        let root_first = self.flags.first().is_some_and(|f| f & F_ROOT != 0) as usize;
        let at = if p.flags & F_ROOT != 0 {
            0
        } else if self.by_time {
            // newest first: after every row at least as new
            self.ts.partition_point(|&t| t >= p.ts).max(root_first)
        } else {
            self.len()
        };
        self.sid.insert(at, p.sid);
        self.author.insert(at, p.author);
        self.ts.insert(at, p.ts);
        self.text.insert(at, p.text);
        self.extra.insert(at, p.extra);
        self.metric.insert(at, p.metric);
        self.position.insert(at, p.position);
        self.flags.insert(at, p.flags);
        self.depth.insert(at, p.depth);
        at
    }

    #[allow(dead_code)]
    fn clear(&mut self) {
        self.sid.clear();
        self.author.clear();
        self.ts.clear();
        self.text.clear();
        self.extra.clear();
        self.metric.clear();
        self.position.clear();
        self.flags.clear();
        self.depth.clear();
    }

    pub fn bytes(&self) -> usize {
        self.sid.capacity() * (8 + 4 + 4 + 8 + 8 + 4 + 4 + 2 + 1)
    }
}

/// Line starts of the message being streamed, so a token wraps only the new bytes and
/// the frame paints only the visible lines instead of re-wrapping the whole answer.
pub struct WrapCache {
    width: u16,
    msg: usize,
    pub(crate) starts: Vec<u32>,
    pub(crate) code: Vec<bool>,
}

const WRAP_LINES: usize = 16 * 1024;

impl WrapCache {
    fn new() -> WrapCache {
        WrapCache { width: 0, msg: usize::MAX, starts: Vec::with_capacity(WRAP_LINES), code: Vec::with_capacity(WRAP_LINES) }
    }

    /// Brings the line starts up to date with the text. Returns the row count.
    pub(crate) fn update(&mut self, s: &str, width: u16, msg: usize) -> usize {
        let stale = self.width != width || self.msg != msg || self.starts.last().is_none_or(|&l| l as usize > s.len());
        if stale {
            self.width = width;
            self.msg = msg;
            self.starts.clear();
            self.code.clear();
            self.starts.push(0);
            self.code.push(false);
        }
        // the last line may have grown: wrap again from its start
        let mut pos = *self.starts.last().unwrap() as usize;
        let mut code = *self.code.last().unwrap();
        while self.starts.len() < WRAP_LINES {
            let (end, next) = next_line(s, pos, width as usize);
            if next >= s.len() {
                break;
            }
            if s[pos..end].starts_with("```") {
                code = !code;
            }
            self.starts.push(next as u32);
            self.code.push(code);
            pos = next;
        }
        self.starts.len()
    }
}

pub struct ChatView {
    /// false when no api key is stored for this provider
    pub has_key: bool,
    pub wrap: WrapCache,
    pub msgs: Vec<Msg>,
    pub input: Input,
    pub focused: bool,
    /// lines scrolled up from the bottom
    pub scroll: u32,
    pub busy: bool,
}

/// A web page read as text: blocks in order, and the links inside them.
pub struct Page {
    pub blocks: Vec<PageBlock>,
    pub links: Vec<PageLink>,
    pub title: Span,
    pub url: Span,
    /// the link Tab has moved to
    pub focus: Option<usize>,
}

/// A new tab: the search box and shortcuts.
pub struct StartPage {
    pub input: Input,
    /// 0 is the search box, then the tiles
    pub focus: usize,
}

pub enum Store {
    Feed(Feed),
    Chat(ChatView),
    Page(Page),
    Start(StartPage),
}

pub struct Tab {
    pub id: TabId,
    pub src: Source,
    pub arena: Arc<TextArena>,
    pub store: Store,
    pub cursor: usize,
    /// the entry (or block) at the top of the view, and how many of its rows are off screen
    pub top: usize,
    pub top_row: u16,
    /// rows of scrolling asked for by the wheel or page keys, applied at the next frame
    pub pending: i32,
    /// true: the view follows the cursor (j/k). false: the cursor follows the view (wheel)
    pub follow: bool,
    pub loading: bool,
    pub more: bool,
    pub asked_more: bool,
    pub spin: u8,
    pub state: TabState,
    pub last_shown: u64,
    pub model: String,
    /// where this tab has been, for back and forward
    pub back: Vec<Source>,
    pub fwd: Vec<Source>,
    /// the last error for this tab, shown in its empty state
    pub error: Option<Line>,
}

impl Tab {
    fn feed(&self) -> Option<&Feed> {
        match &self.store {
            Store::Feed(f) => Some(f),
            _ => None,
        }
    }
    pub fn service(&self) -> Service {
        self.src.service()
    }
}

/// What a closed tab keeps: enough to reopen it from the log.
pub struct Closed {
    pub src: Source,
    pub cursor: usize,
    pub top: usize,
}

fn store_for(src: &Source) -> Store {
    match src {
        Source::Chat { service, .. } => Store::Chat(ChatView { has_key: crate::adapters::has_credentials(*service), wrap: WrapCache::new(), msgs: Vec::with_capacity(64), input: Input::new(32 * 1024), focused: true, scroll: 0, busy: false }),
        Source::Web { .. } => Store::Page(Page { blocks: Vec::with_capacity(512), links: Vec::with_capacity(256), title: Span::EMPTY, url: Span::EMPTY, focus: None }),
        Source::Start => Store::Start(StartPage { input: Input::new(1024), focus: 0 }),
        _ => {
            let by_time = matches!(src, Source::XHome | Source::XMentions | Source::YtSubs | Source::LocalSearch { .. } | Source::XSearch { .. } | Source::YtSearch { .. });
            Store::Feed(Feed::with_capacity(256, by_time))
        }
    }
}

// ---------------------------------------------------------------- line input

/// A text input with a fixed-capacity buffer.
pub struct Input {
    pub buf: Vec<u8>,
    pub cur: usize,
}

impl Input {
    pub fn new(cap: usize) -> Input {
        Input { buf: Vec::with_capacity(cap), cur: 0 }
    }
    pub fn as_str(&self) -> &str {
        utf8_prefix(&self.buf)
    }
    pub fn clear(&mut self) {
        self.buf.clear();
        self.cur = 0;
    }
    pub fn set(&mut self, s: &str) {
        self.clear();
        let n = s.len().min(self.buf.capacity());
        self.buf.extend_from_slice(utf8_prefix(&s.as_bytes()[..n]).as_bytes());
        self.cur = self.buf.len();
    }
    fn insert(&mut self, c: char) {
        let mut b = [0u8; 4];
        let s = c.encode_utf8(&mut b);
        if self.buf.len() + s.len() > self.buf.capacity() {
            return;
        }
        for (i, &x) in s.as_bytes().iter().enumerate() {
            self.buf.insert(self.cur + i, x);
        }
        self.cur += s.len();
    }
    fn prev_boundary(&self, i: usize) -> usize {
        let mut j = i.saturating_sub(1);
        while j > 0 && (self.buf[j] & 0xC0) == 0x80 {
            j -= 1;
        }
        j
    }
    fn next_boundary(&self, i: usize) -> usize {
        let mut j = (i + 1).min(self.buf.len());
        while j < self.buf.len() && (self.buf[j] & 0xC0) == 0x80 {
            j += 1;
        }
        j
    }
    /// Applies an editing key. Returns false for keys it does not handle.
    fn key(&mut self, k: Key, multiline: bool) -> bool {
        match k {
            Key::Char(c) => self.insert(c),
            Key::ShiftEnter if multiline => self.insert('\n'),
            Key::Backspace => {
                if self.cur > 0 {
                    let p = self.prev_boundary(self.cur);
                    self.buf.drain(p..self.cur);
                    self.cur = p;
                }
            }
            Key::CtrlBackspace => {
                let mut p = self.cur;
                while p > 0 && self.buf[p - 1] == b' ' {
                    p -= 1;
                }
                while p > 0 && self.buf[p - 1] != b' ' && self.buf[p - 1] != b'\n' {
                    p -= 1;
                }
                self.buf.drain(p..self.cur);
                self.cur = p;
            }
            Key::Delete => {
                if self.cur < self.buf.len() {
                    let n = self.next_boundary(self.cur);
                    self.buf.drain(self.cur..n);
                }
            }
            Key::Left => self.cur = self.prev_boundary(self.cur),
            Key::Right => self.cur = self.next_boundary(self.cur),
            Key::Home => {
                while self.cur > 0 && self.buf[self.cur - 1] != b'\n' {
                    self.cur -= 1;
                }
            }
            Key::End => {
                while self.cur < self.buf.len() && self.buf[self.cur] != b'\n' {
                    self.cur += 1;
                }
            }
            Key::CtrlLeft => {
                while self.cur > 0 && self.buf[self.cur - 1] == b' ' {
                    self.cur -= 1;
                }
                while self.cur > 0 && self.buf[self.cur - 1] != b' ' {
                    self.cur -= 1;
                }
            }
            Key::CtrlRight => {
                while self.cur < self.buf.len() && self.buf[self.cur] == b' ' {
                    self.cur += 1;
                }
                while self.cur < self.buf.len() && self.buf[self.cur] != b' ' {
                    self.cur += 1;
                }
            }
            _ => return false,
        }
        true
    }
}

// ---------------------------------------------------------------- modes and the palette

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Overlay {
    Mem,
    Net,
    Spend,
    Help,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PalKind {
    Commands,
    Address,
    Search,
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Mode {
    Normal,
    Palette(PalKind),
    Compose { reply_to: Option<(String, String)> },
    Overlay(Overlay),
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Act {
    Address,
    NewTab,
    CloseTab,
    Reopen,
    XHome,
    XMentions,
    Youtube,
    Claude,
    ChatGpt,
    Site(&'static str),
    Back,
    Forward,
    Reload,
    External,
    Mem,
    Net,
    Spend,
    Export,
    Help,
    Quit,
    /// typed: go to the address typed
    Go,
    /// typed: search the web for what was typed
    WebSearch,
    /// typed: search this service for what was typed
    ServiceSearch,
    /// typed: search local history
    HistorySearch,
    /// typed: run it as a command (model, system, export ...)
    Run,
}

pub const SITES: [(&str, &str); 4] =
    [("Hacker News", "news.ycombinator.com"), ("Lobsters", "lobste.rs"), ("Wikipedia", "en.wikipedia.org/wiki/Special:Random"), ("This Week in Rust", "this-week-in-rust.org")];

pub const ITEMS: [(&str, &str, Act); 21] = [
    ("Open address or search", "ctrl+l", Act::Address),
    ("New tab", "ctrl+t", Act::NewTab),
    ("Close tab", "ctrl+w", Act::CloseTab),
    ("Reopen closed tab", "u", Act::Reopen),
    ("X: home timeline", "", Act::XHome),
    ("X: mentions", "", Act::XMentions),
    ("YouTube: subscriptions", "", Act::Youtube),
    ("Claude: new chat", "", Act::Claude),
    ("ChatGPT: new chat", "", Act::ChatGpt),
    ("Hacker News", "", Act::Site("news.ycombinator.com")),
    ("Wikipedia: random article", "", Act::Site("en.wikipedia.org/wiki/Special:Random")),
    ("Back", "alt+←", Act::Back),
    ("Forward", "alt+→", Act::Forward),
    ("Reload", "r", Act::Reload),
    ("Open in system browser", "o", Act::External),
    ("Memory", ":mem", Act::Mem),
    ("Network requests", ":net", Act::Net),
    ("Spend this month", ":spend", Act::Spend),
    ("Export view to markdown", "e", Act::Export),
    ("Keyboard shortcuts", "?", Act::Help),
    ("Quit", "ctrl+c", Act::Quit),
];

/// Start page tiles after the search box: four services, then the sites.
pub const TILES: usize = 4 + SITES.len();

const COMMANDS: [&str; 11] = ["model", "system", "export", "search", "mem", "net", "spend", "quit", "reopen", "refresh", "help"];

/// Subsequence match, case-insensitive.
fn fuzzy(hay: &str, needle: &str) -> bool {
    let mut h = hay.chars().flat_map(char::to_lowercase);
    needle.chars().flat_map(char::to_lowercase).filter(|c| !c.is_whitespace()).all(|n| h.any(|c| c == n))
}

/// Frame and latency times, kept in a fixed ring of the last 10,000 frames.
pub struct Hist {
    frame_us: Box<[u32]>,
    input_us: Box<[u32]>,
    n: usize,
    ni: usize,
    scratch: Box<[u32]>,
}

pub const HIST_LEN: usize = 10_000;

impl Hist {
    fn new() -> Hist {
        Hist {
            frame_us: vec![0; HIST_LEN].into_boxed_slice(),
            input_us: vec![0; HIST_LEN].into_boxed_slice(),
            n: 0,
            ni: 0,
            scratch: vec![0; HIST_LEN].into_boxed_slice(),
        }
    }
    pub fn frame(&mut self, us: u32) {
        self.frame_us[self.n % HIST_LEN] = us;
        self.n += 1;
    }
    pub fn input(&mut self, us: u32) {
        self.input_us[self.ni % HIST_LEN] = us;
        self.ni += 1;
    }
    /// (p50, p99, max) of frame times or input-to-flush times, microseconds.
    pub fn stats(&mut self, input: bool) -> (u32, u32, u32) {
        let (src, n) = if input { (&self.input_us, self.ni) } else { (&self.frame_us, self.n) };
        let n = n.min(HIST_LEN);
        if n == 0 {
            return (0, 0, 0);
        }
        self.scratch[..n].copy_from_slice(&src[..n]);
        let s = &mut self.scratch[..n];
        s.sort_unstable();
        (s[n / 2], s[(n * 99 / 100).min(n - 1)], s[n - 1])
    }
}

// ---------------------------------------------------------------- click targets

pub const H_URL: u8 = 0; // a url in the arena: span
pub const H_ENTRY: u8 = 1; // a constructed url for feed entry arg: kind in span.len
pub const H_TAB: u8 = 2; // sidebar tab arg
pub const H_TILE: u8 = 3; // start page tile arg
pub const H_PAGELINK: u8 = 4; // link index arg
pub const H_NEWTAB: u8 = 5;

#[derive(Clone, Copy, Default)]
pub struct Hit {
    pub y: u16,
    pub x0: u16,
    pub x1: u16,
    pub kind: u8,
    pub arg: u32,
    pub span: Span,
}

const MAX_HITS: usize = 192;

/// Clickable regions painted this frame.
pub struct Hits {
    items: [Hit; MAX_HITS],
    n: usize,
}

impl Hits {
    pub fn add(&mut self, h: Hit) {
        if self.n < MAX_HITS && h.x1 > h.x0 {
            self.items[self.n] = h;
            self.n += 1;
        }
    }
}

const NET_LOG: usize = 64;

pub struct App {
    pub cfg: Arc<Config>,
    pub screen: Screen,
    frame: Bump,
    pub tabs: Vec<Tab>,
    pub closed: Vec<Closed>,
    pub active: usize,
    next_id: TabId,
    pub authors_arena: Arc<TextArena>,
    pub authors: Vec<(Span, Span)>,
    pub mode: Mode,
    /// the palette's input
    pub input: Input,
    pal_sel: usize,
    pub compose: Input,
    status: Line,
    status_level: Level,
    status_tab: Option<TabId>,
    net_log: Vec<NetEntry>,
    net_n: usize,
    pub spend: (u64, u64, u64, u32),
    pub hist: Hist,
    cmds: Producer<Command>,
    cmd_event: Arc<WinEvent>,
    pub quit: bool,
    hits: Hits,
    /// which feed entry each screen row belongs to, for clicks
    row_entry: Vec<u32>,
    pub playing: Option<(TabId, usize)>,
    pub mpv: Option<crate::mpv::Playing>,
    /// process start to first frame written, microseconds
    pub startup_us: u64,
    pub dirty: bool,
    pub frames: u64,
}

impl App {
    pub fn new(cfg: Arc<Config>, w: u16, h: u16, cmds: Producer<Command>, cmd_event: Arc<WinEvent>, authors_arena: Arc<TextArena>) -> App {
        App {
            screen: Screen::new(w, h, cfg.palette),
            cfg,
            frame: Bump::new(ArenaId::Frame),
            tabs: Vec::with_capacity(32),
            closed: Vec::with_capacity(32),
            active: 0,
            next_id: 1,
            authors_arena,
            authors: Vec::with_capacity(4096),
            mode: Mode::Normal,
            input: Input::new(1024),
            pal_sel: 0,
            compose: Input::new(16 * 1024),
            status: Line::new(""),
            status_level: Level::Info,
            status_tab: None,
            net_log: Vec::with_capacity(NET_LOG),
            net_n: 0,
            spend: (0, 0, 0, 0),
            hist: Hist::new(),
            cmds,
            cmd_event,
            quit: false,
            hits: Hits { items: [Hit::default(); MAX_HITS], n: 0 },
            row_entry: vec![u32::MAX; 512],
            playing: None,
            mpv: None,
            startup_us: 0,
            dirty: true,
            frames: 0,
        }
    }

    pub fn send(&self, c: Command) {
        let mut c = c;
        loop {
            match self.cmds.push(c) {
                Ok(()) => break,
                Err(back) => {
                    c = back;
                    self.cmd_event.set();
                    std::thread::yield_now();
                }
            }
        }
        self.cmd_event.set();
    }

    pub fn say(&mut self, level: Level, s: &str) {
        self.status = Line::new(s);
        self.status_level = level;
        self.status_tab = None;
        self.dirty = true;
    }

    pub fn resize(&mut self, w: u16, h: u16) {
        self.screen.resize(w, h);
        if self.row_entry.len() < h as usize {
            self.row_entry.resize(h as usize, u32::MAX);
        }
        self.dirty = true;
    }

    // ------------------------------------------------------------ tabs

    fn new_id(&mut self) -> TabId {
        let id = self.next_id;
        self.next_id += 1;
        id
    }

    pub fn open(&mut self, src: Source) -> TabId {
        let id = self.new_id();
        let arena = Arc::new(TextArena::new());
        let model = match src.service() {
            Service::Claude => self.cfg.claude_model.clone(),
            Service::ChatGpt => self.cfg.openai_model.clone(),
            _ => String::new(),
        };
        let loading = src != Source::Start;
        self.tabs.push(Tab {
            id,
            src: src.clone(),
            arena: arena.clone(),
            store: store_for(&src),
            cursor: 0,
            top: 0,
            top_row: 0,
            pending: 0,
            follow: true,
            loading,
            more: true,
            asked_more: false,
            spin: 0,
            state: TabState::Hidden,
            last_shown: sys::now_unix(),
            model,
            back: Vec::new(),
            fwd: Vec::new(),
            error: None,
        });
        if loading {
            self.send(Command::Open { tab: id, arena, src });
        }
        self.dirty = true;
        id
    }

    /// Opens a tab and shows it.
    pub fn open_focus(&mut self, src: Source) {
        self.open(src);
        let n = self.tabs.len() - 1;
        self.focus(n);
    }

    /// Points the active tab somewhere else: a web page, a service, the start page.
    /// The old view's memory goes back to the os; `push` records it for back.
    pub fn navigate(&mut self, src: Source, push: bool) {
        if self.tabs.is_empty() {
            return self.open_focus(src);
        }
        let old_id = self.tabs[self.active].id;
        let id = self.new_id();
        let arena = Arc::new(TextArena::new());
        let model = match src.service() {
            Service::Claude => self.cfg.claude_model.clone(),
            Service::ChatGpt => self.cfg.openai_model.clone(),
            _ => String::new(),
        };
        let t = &mut self.tabs[self.active];
        if t.src != Source::Start {
            self.send(Command::Close { tab: old_id });
        }
        let t = &mut self.tabs[self.active];
        if push {
            let prev = core::mem::replace(&mut t.src, src.clone());
            t.back.push(prev);
            t.fwd.clear();
        } else {
            t.src = src.clone();
        }
        t.id = id;
        t.arena = arena.clone();
        t.store = store_for(&src);
        t.cursor = 0;
        t.top = 0;
        t.top_row = 0;
        t.pending = 0;
        t.follow = true;
        t.more = true;
        t.asked_more = false;
        t.error = None;
        t.model = model;
        t.loading = src != Source::Start;
        t.state = TabState::Visible;
        if t.loading {
            self.send(Command::Open { tab: id, arena, src });
        }
        self.status = Line::new("");
        self.dirty = true;
    }

    fn go_back(&mut self) {
        let Some(t) = self.tabs.get_mut(self.active) else { return };
        let Some(prev) = t.back.pop() else { return self.say(Level::Info, "nothing to go back to") };
        let cur = t.src.clone();
        t.fwd.push(cur);
        let fwd = core::mem::take(&mut t.fwd);
        let back = core::mem::take(&mut t.back);
        self.navigate(prev, false);
        let t = &mut self.tabs[self.active];
        t.fwd = fwd;
        t.back = back;
    }

    fn go_forward(&mut self) {
        let Some(t) = self.tabs.get_mut(self.active) else { return };
        let Some(next) = t.fwd.pop() else { return };
        let cur = t.src.clone();
        t.back.push(cur);
        let fwd = core::mem::take(&mut t.fwd);
        let back = core::mem::take(&mut t.back);
        self.navigate(next, false);
        let t = &mut self.tabs[self.active];
        t.fwd = fwd;
        t.back = back;
    }

    /// An address or a search, from the address bar or the start page.
    pub fn go(&mut self, input: &str) {
        let u = crate::url::from_input(input);
        let here = self.tabs.get(self.active).is_some_and(|t| matches!(t.src, Source::Web { .. } | Source::Start));
        let src = Source::Web { url: u.full().into() };
        if here {
            self.navigate(src, true);
        } else {
            self.open_focus(src);
        }
    }

    pub fn focus(&mut self, i: usize) {
        if i >= self.tabs.len() {
            return;
        }
        if let Some(t) = self.tabs.get_mut(self.active) {
            if t.state == TabState::Visible {
                t.state = TabState::Hidden;
            }
            t.last_shown = sys::now_unix();
        }
        self.active = i;
        let t = &mut self.tabs[i];
        if t.state == TabState::Archived {
            // back from disk: a fresh arena filled from the log, no network
            let arena = Arc::new(TextArena::new());
            t.arena = arena.clone();
            t.store = store_for(&t.src);
            t.loading = true;
            let (id, src) = (t.id, t.src.clone());
            self.send(Command::Open { tab: id, arena, src });
        }
        let t = &mut self.tabs[i];
        t.state = TabState::Visible;
        t.last_shown = sys::now_unix();
        if self.status_tab.is_some_and(|s| s != t.id) {
            self.status = Line::new("");
        }
        self.dirty = true;
    }

    fn tab_index(&self, id: TabId) -> Option<usize> {
        self.tabs.iter().position(|t| t.id == id)
    }

    pub fn close_active(&mut self) {
        if self.tabs.is_empty() {
            self.quit = true;
            return;
        }
        let t = self.tabs.remove(self.active);
        if t.src != Source::Start {
            self.send(Command::Close { tab: t.id });
        }
        if self.closed.len() >= 32 {
            self.closed.remove(0);
        }
        self.closed.push(Closed { src: t.src, cursor: t.cursor, top: t.top });
        if self.tabs.is_empty() {
            // never an empty window: a start page takes the last tab's place
            self.open_focus(Source::Start);
            return;
        }
        self.active = self.active.min(self.tabs.len() - 1);
        self.tabs[self.active].state = TabState::Hidden;
        let a = self.active;
        self.focus(a);
    }

    pub fn reopen(&mut self) {
        let Some(c) = self.closed.pop() else { return self.say(Level::Info, "no closed tabs") };
        // a chat's source holds its resolved id, so the same chat comes back
        self.open(c.src);
        let i = self.tabs.len() - 1;
        self.tabs[i].cursor = c.cursor;
        self.tabs[i].top = c.top;
        self.focus(i);
    }

    /// Hidden tabs sleep, and after 24 hours untouched they are archived to disk.
    fn age_tabs(&mut self) {
        let now = sys::now_unix();
        for i in 0..self.tabs.len() {
            if i == self.active {
                continue;
            }
            let t = &mut self.tabs[i];
            if t.state == TabState::Hidden && now > t.last_shown + 600 {
                t.state = TabState::Asleep;
            }
            if t.state == TabState::Asleep && now > t.last_shown + 86_400 && !t.loading && t.src != Source::Start {
                t.state = TabState::Archived;
                let id = t.id;
                t.store = store_for(&Source::Start);
                self.send(Command::Close { tab: id });
            }
        }
    }

    // ------------------------------------------------------------ replies

    pub fn apply(&mut self, r: Reply) {
        self.dirty = true;
        match r {
            Reply::Post { tab, post } => {
                let Some(i) = self.tab_index(tab) else { return };
                let t = &mut self.tabs[i];
                t.spin = t.spin.wrapping_add(1);
                if let Store::Feed(f) = &mut t.store {
                    let at = f.insert(post);
                    // keep the cursor on the same entry when rows arrive above it
                    if at <= t.cursor && f.len() > 1 && (t.cursor > 0 || t.top > 0) {
                        t.cursor += 1;
                        if at < t.top {
                            t.top += 1;
                        }
                    }
                }
            }
            Reply::Author { idx, name, handle } => {
                let i = idx as usize;
                if self.authors.len() <= i {
                    self.authors.resize(i + 1, (Span::EMPTY, Span::EMPTY));
                }
                self.authors[i] = (name, handle);
            }
            Reply::Msg { tab, msg } => {
                let Some(i) = self.tab_index(tab) else { return };
                let t = &mut self.tabs[i];
                if let Store::Chat(c) = &mut t.store {
                    if msg.role == Role::Assistant && msg.text.len == 0 {
                        c.busy = true;
                        t.loading = true;
                    }
                    c.msgs.push(msg);
                    c.scroll = 0;
                }
            }
            Reply::Grow { tab, len } => {
                let Some(i) = self.tab_index(tab) else { return };
                if let Store::Chat(c) = &mut self.tabs[i].store {
                    if let Some(m) = c.msgs.last_mut() {
                        m.text.len = len;
                    }
                }
            }
            Reply::Block { tab, block } => {
                let Some(i) = self.tab_index(tab) else { return };
                let t = &mut self.tabs[i];
                t.spin = t.spin.wrapping_add(1);
                if let Store::Page(p) = &mut t.store {
                    if block.kind == crate::html::K_RESET {
                        p.blocks.clear();
                        p.links.clear();
                        p.focus = None;
                        t.top = 0;
                        t.top_row = 0;
                    } else {
                        p.blocks.push(block);
                    }
                }
            }
            Reply::Link { tab, link } => {
                let Some(i) = self.tab_index(tab) else { return };
                if let Store::Page(p) = &mut self.tabs[i].store {
                    p.links.push(link);
                }
            }
            Reply::PageInfo { tab, title, url } => {
                let Some(i) = self.tab_index(tab) else { return };
                let t = &mut self.tabs[i];
                if let Store::Page(p) = &mut t.store {
                    p.title = title;
                    p.url = url;
                    // the address after redirects is the one back and reload use
                    let u = t.arena.str(url);
                    if !u.is_empty() {
                        t.src = Source::Web { url: u.into() };
                    }
                }
            }
            Reply::Done { tab, more } => {
                let Some(i) = self.tab_index(tab) else { return };
                let t = &mut self.tabs[i];
                t.loading = false;
                t.more = more;
                t.asked_more = false;
                if let Store::Chat(c) = &mut t.store {
                    c.busy = false;
                }
            }
            Reply::Status { tab, level, line, retry_at: _ } => {
                self.status = line;
                self.status_level = level;
                self.status_tab = tab;
                if let Some(i) = tab.and_then(|t| self.tab_index(t)) {
                    if level == Level::Error {
                        self.tabs[i].error = Some(line);
                    }
                }
            }
            Reply::Net(e) => {
                if self.net_log.len() < NET_LOG {
                    self.net_log.push(e);
                } else {
                    self.net_log[self.net_n % NET_LOG] = e;
                }
                self.net_n += 1;
            }
            Reply::Spend { claude, chatgpt, x, yt_units } => self.spend = (claude, chatgpt, x, yt_units),
            Reply::ChatId { tab, service, chat } => {
                if let Some(i) = self.tab_index(tab) {
                    self.tabs[i].src = Source::Chat { service, chat };
                }
            }
            Reply::Liked { tab, sid_hash } => {
                let Some(i) = self.tab_index(tab) else { return };
                let t = &mut self.tabs[i];
                if let Store::Feed(f) = &mut t.store {
                    for j in 0..f.len() {
                        if key_hash(t.arena.str(f.sid[j])) == sid_hash {
                            f.flags[j] |= F_LIKED;
                        }
                    }
                }
            }
        }
    }

    // ------------------------------------------------------------ keys

    pub fn key(&mut self, k: Key) {
        self.dirty = true;
        if k == Key::Ctrl('c') {
            self.quit = true;
            return;
        }
        // keys that work everywhere
        match k {
            Key::Ctrl('k') => return self.palette(PalKind::Commands, ""),
            Key::Ctrl('l') => {
                let cur = self.current_url();
                return self.palette(PalKind::Address, &cur);
            }
            Key::Ctrl('t') => return self.open_focus(Source::Start),
            Key::Ctrl('w') => {
                self.mode = Mode::Normal;
                return self.close_active();
            }
            Key::CtrlTab => return self.cycle(1),
            Key::CtrlShiftTab => return self.cycle(-1),
            Key::Alt(c @ '1'..='9') => return self.focus(c as usize - '1' as usize),
            _ => {}
        }
        match self.mode.clone() {
            Mode::Overlay(_) => {
                if matches!(k, Key::Esc | Key::Char('q') | Key::Enter | Key::Char(':') | Key::Char('?')) {
                    self.mode = Mode::Normal;
                }
            }
            Mode::Palette(kind) => self.key_palette(kind, k),
            Mode::Compose { reply_to } => match k {
                Key::Esc => self.mode = Mode::Normal,
                Key::Ctrl('s') | Key::Ctrl('j') => {
                    let text = self.compose.as_str().trim().to_string();
                    if text.is_empty() {
                        return;
                    }
                    let tab = self.tabs.get(self.active).map(|t| t.id).unwrap_or(0);
                    self.send(Command::Post { tab, text: text.into(), reply_to: reply_to.map(|r| r.0.into()) });
                    self.compose.clear();
                    self.mode = Mode::Normal;
                    self.say(Level::Info, "sending…");
                }
                Key::Enter => {
                    self.compose.key(Key::ShiftEnter, true);
                }
                _ => {
                    self.compose.key(k, true);
                }
            },
            Mode::Normal => self.key_normal(k),
        }
    }

    fn cycle(&mut self, d: i32) {
        if self.tabs.is_empty() {
            return;
        }
        let n = self.tabs.len() as i32;
        let i = ((self.active as i32 + d) % n + n) % n;
        self.focus(i as usize);
    }

    fn current_url(&self) -> String {
        match self.tabs.get(self.active).map(|t| &t.src) {
            Some(Source::Web { url }) => url.to_string(),
            _ => String::new(),
        }
    }

    pub fn palette(&mut self, kind: PalKind, pre: &str) {
        self.input.set(pre);
        self.pal_sel = 0;
        self.mode = Mode::Palette(kind);
    }

    /// The palette's rows for what is typed: typed actions first, then matching commands.
    pub fn palette_items(&self, kind: PalKind, out: &mut [Act; 32]) -> usize {
        let q = self.input.as_str().trim();
        let mut n = 0;
        let mut push = |a: Act, n: &mut usize| {
            if *n < out.len() {
                out[*n] = a;
                *n += 1;
            }
        };
        let svc = self.tabs.get(self.active).map(|t| t.service());
        if !q.is_empty() {
            let looks_url = !q.contains(' ') && q.contains('.') && Url::parse(q).is_some();
            let first = q.split(' ').next().unwrap_or("");
            match kind {
                PalKind::Commands => {
                    if COMMANDS.contains(&first) {
                        push(Act::Run, &mut n);
                    }
                    if looks_url {
                        push(Act::Go, &mut n);
                    }
                    push(Act::WebSearch, &mut n);
                    if matches!(svc, Some(Service::X | Service::YouTube)) {
                        push(Act::ServiceSearch, &mut n);
                    }
                    push(Act::HistorySearch, &mut n);
                }
                PalKind::Address => {
                    if looks_url {
                        push(Act::Go, &mut n);
                    }
                    push(Act::WebSearch, &mut n);
                    push(Act::HistorySearch, &mut n);
                }
                PalKind::Search => {
                    if matches!(svc, Some(Service::X | Service::YouTube)) {
                        push(Act::ServiceSearch, &mut n);
                    }
                    push(Act::HistorySearch, &mut n);
                    push(Act::WebSearch, &mut n);
                }
            }
        }
        if kind == PalKind::Commands {
            for (label, _, act) in ITEMS {
                if q.is_empty() || fuzzy(label, q) {
                    push(act, &mut n);
                }
            }
        }
        n
    }

    fn key_palette(&mut self, kind: PalKind, k: Key) {
        let mut items = [Act::Quit; 32];
        let n = self.palette_items(kind, &mut items);
        match k {
            Key::Esc => self.mode = Mode::Normal,
            Key::Down | Key::Tab | Key::Ctrl('n') => self.pal_sel = if n == 0 { 0 } else { (self.pal_sel + 1) % n },
            Key::Up | Key::BackTab | Key::Ctrl('p') => self.pal_sel = if n == 0 { 0 } else { (self.pal_sel + n - 1) % n },
            Key::Enter => {
                if n == 0 {
                    return;
                }
                let act = items[self.pal_sel.min(n - 1)];
                let q = self.input.as_str().trim().to_string();
                self.mode = Mode::Normal;
                self.run(act, &q);
            }
            _ => {
                if self.input.key(k, false) {
                    self.pal_sel = 0;
                }
            }
        }
    }

    pub fn run(&mut self, act: Act, q: &str) {
        self.dirty = true;
        match act {
            Act::Address => {
                let cur = self.current_url();
                self.palette(PalKind::Address, &cur);
            }
            Act::NewTab => self.open_focus(Source::Start),
            Act::CloseTab => self.close_active(),
            Act::Reopen => self.reopen(),
            Act::XHome => self.service_tab(Source::XHome),
            Act::XMentions => self.service_tab(Source::XMentions),
            Act::Youtube => self.service_tab(Source::YtSubs),
            Act::Claude => self.service_tab(Source::Chat { service: Service::Claude, chat: 1 }),
            Act::ChatGpt => self.service_tab(Source::Chat { service: Service::ChatGpt, chat: 1 }),
            Act::Site(s) => self.go(s),
            Act::Back => self.go_back(),
            Act::Forward => self.go_forward(),
            Act::Reload => self.reload(),
            Act::External => self.open_in_browser(),
            Act::Mem => self.mode = Mode::Overlay(Overlay::Mem),
            Act::Net => self.mode = Mode::Overlay(Overlay::Net),
            Act::Spend => self.mode = Mode::Overlay(Overlay::Spend),
            Act::Export => self.export(""),
            Act::Help => self.mode = Mode::Overlay(Overlay::Help),
            Act::Quit => self.quit = true,
            Act::Go | Act::WebSearch => {
                if act == Act::WebSearch {
                    let u = crate::url::search(q);
                    let here = self.tabs.get(self.active).is_some_and(|t| matches!(t.src, Source::Web { .. } | Source::Start));
                    let src = Source::Web { url: u.full().into() };
                    if here { self.navigate(src, true) } else { self.open_focus(src) }
                } else {
                    self.go(q);
                }
            }
            Act::ServiceSearch => self.search(q, false),
            Act::HistorySearch => self.search(q, true),
            Act::Run => self.command(q),
        }
    }

    /// A service from the start page replaces it; from anywhere else it gets a new tab.
    fn service_tab(&mut self, src: Source) {
        if self.tabs.get(self.active).is_some_and(|t| t.src == Source::Start) {
            self.navigate(src, true);
        } else {
            self.open_focus(src);
        }
    }

    fn reload(&mut self) {
        let Some(t) = self.tabs.get_mut(self.active) else { return };
        match t.src.clone() {
            s @ Source::Web { .. } => self.navigate(s, false),
            Source::Start => {}
            _ => {
                t.loading = true;
                let id = t.id;
                self.send(Command::Refresh { tab: id });
            }
        }
    }

    fn key_normal(&mut self, k: Key) {
        if self.tabs.is_empty() {
            return self.open_focus(Source::Start);
        }
        let active = self.active;
        match &mut self.tabs[active].store {
            Store::Start(sp) => {
                match k {
                    Key::Enter => {
                        let q = sp.input.as_str().trim().to_string();
                        if !q.is_empty() {
                            return self.go(&q);
                        }
                        if sp.focus > 0 {
                            let t = sp.focus - 1;
                            return self.tile(t);
                        }
                    }
                    Key::Tab | Key::Down => sp.focus = (sp.focus + 1) % (TILES + 1),
                    Key::BackTab | Key::Up => sp.focus = (sp.focus + TILES) % (TILES + 1),
                    Key::Esc => sp.input.clear(),
                    Key::Char('?') if sp.input.buf.is_empty() => self.mode = Mode::Overlay(Overlay::Help),
                    _ => {
                        if sp.input.key(k, false) {
                            sp.focus = 0;
                        }
                    }
                }
                return;
            }
            Store::Chat(c) if c.focused => {
                match k {
                    Key::Esc => c.focused = false,
                    Key::Enter => {
                        let text = c.input.as_str().trim().to_string();
                        if text.is_empty() || c.busy {
                            return;
                        }
                        c.input.clear();
                        c.scroll = 0;
                        let tab = self.tabs[active].id;
                        let model = self.tabs[active].model.clone();
                        self.send(Command::Send { tab, text: text.into(), model: model.into(), system: "".into() });
                    }
                    Key::PageUp => c.scroll += 10,
                    Key::PageDown => c.scroll = c.scroll.saturating_sub(10),
                    _ => {
                        c.input.key(k, true);
                    }
                }
                return;
            }
            _ => {}
        }
        let service = self.tabs[active].service();
        let is_page = matches!(self.tabs[active].store, Store::Page(_));
        let is_chat = matches!(self.tabs[active].store, Store::Chat(_));
        // keys every view shares
        match k {
            Key::Char(':') => return self.palette(PalKind::Commands, ""),
            Key::Char('/') => return self.palette(if is_page || is_chat { PalKind::Address } else { PalKind::Search }, ""),
            Key::Char('?') => return self.mode = Mode::Overlay(Overlay::Help),
            Key::Char('t') => return self.open_focus(Source::Start),
            Key::Char('q') => return self.close_active(),
            Key::Char('u') => return self.reopen(),
            Key::Char('e') => return self.export(""),
            Key::Tab if !is_page => return self.cycle(1),
            Key::BackTab if !is_page => return self.cycle(-1),
            Key::Alt('\u{1}') => {}
            _ => {}
        }
        if is_page {
            match k {
                Key::Char('j') | Key::Down => self.scroll_rows(2),
                Key::Char('k') | Key::Up => self.scroll_rows(-2),
                Key::PageDown | Key::Char(' ') | Key::Ctrl('d') => self.page(1),
                Key::PageUp | Key::Ctrl('u') => self.page(-1),
                Key::Char('g') | Key::Home => self.scroll_rows(-1_000_000),
                Key::Char('G') | Key::End => self.scroll_rows(1_000_000),
                Key::Tab => self.focus_link(1),
                Key::BackTab => self.focus_link(-1),
                Key::Enter => self.follow_focused(),
                Key::Backspace | Key::Char('H') | Key::Alt('h') => self.go_back(),
                Key::Char('L') | Key::Alt('l') => self.go_forward(),
                Key::Char('r') | Key::Char('R') => self.reload(),
                Key::Char('o') => self.open_in_browser(),
                Key::Esc => {
                    if let Store::Page(p) = &mut self.tabs[active].store {
                        p.focus = None;
                    }
                }
                _ => {}
            }
            return;
        }
        match k {
            Key::Char('j') | Key::Down => self.move_cursor(1),
            Key::Char('k') | Key::Up => self.move_cursor(-1),
            Key::PageDown | Key::Ctrl('d') | Key::Char(' ') => self.page(1),
            Key::PageUp | Key::Ctrl('u') => self.page(-1),
            Key::Ctrl('e') => self.scroll_rows(1),
            Key::Ctrl('y') => self.scroll_rows(-1),
            Key::Char('g') | Key::Home => self.move_cursor(-1_000_000),
            Key::Char('G') | Key::End => self.move_cursor(1_000_000),
            Key::Char('R') => self.reload(),
            Key::Char('n') => self.new_thing(),
            Key::Backspace | Key::Alt('h') => self.go_back(),
            _ if is_chat => {
                if let Store::Chat(c) = &mut self.tabs[active].store {
                    if matches!(k, Key::Enter | Key::Char('i') | Key::Char('r') | Key::Char('a')) {
                        c.focused = true;
                    }
                }
            }
            Key::Enter => self.open_entry(),
            Key::Char('c') if service == Service::YouTube => self.comments(),
            Key::Char('i') => self.toggle_media(),
            Key::Char('r') => self.reply(),
            Key::Char('l') => self.like(),
            Key::Char('o') => self.open_in_browser(),
            _ => {}
        }
    }

    fn tile(&mut self, i: usize) {
        match i {
            0 => self.service_tab(Source::XHome),
            1 => self.service_tab(Source::YtSubs),
            2 => self.service_tab(Source::Chat { service: Service::Claude, chat: 1 }),
            3 => self.service_tab(Source::Chat { service: Service::ChatGpt, chat: 1 }),
            n if n - 4 < SITES.len() => self.go(SITES[n - 4].1),
            _ => {}
        }
    }

    /// The wheel scrolls the view by rows, like a browser; the selection stays put unless
    /// it scrolls out of sight.
    pub fn wheel(&mut self, d: i16) {
        self.scroll_rows(d as i32);
    }

    fn scroll_rows(&mut self, d: i32) {
        self.dirty = true;
        let Some(t) = self.tabs.get_mut(self.active) else { return };
        match &mut t.store {
            Store::Chat(c) => {
                c.scroll = (c.scroll as i32 - d).max(0) as u32;
                return;
            }
            Store::Start(_) => return,
            _ => {}
        }
        t.pending = t.pending.saturating_add(d);
        t.follow = false;
        self.maybe_more();
    }

    fn page(&mut self, dir: i32) {
        let rows = self.screen.back.h.saturating_sub(5).max(1) as i32;
        self.scroll_rows(dir * rows);
    }

    /// Near the end of what is loaded: ask for one more page.
    fn maybe_more(&mut self) {
        let Some(t) = self.tabs.get_mut(self.active) else { return };
        let Store::Feed(f) = &t.store else { return };
        let near = t.cursor + 5 >= f.len() || t.top + 10 >= f.len();
        if near && t.more && !t.loading && !t.asked_more {
            t.asked_more = true;
            t.loading = true;
            let id = t.id;
            self.send(Command::More { tab: id });
        }
    }

    pub fn click(&mut self, x: u16, y: u16) {
        self.dirty = true;
        let mut found = None;
        for h in &self.hits.items[..self.hits.n] {
            if h.y == y && x >= h.x0 && x < h.x1 {
                found = Some(*h);
            }
        }
        if let Some(h) = found {
            return self.hit(h);
        }
        if let Some(&e) = self.row_entry.get(y as usize) {
            if e != u32::MAX {
                if let Some(t) = self.tabs.get_mut(self.active) {
                    t.cursor = e as usize;
                    t.follow = true;
                }
            }
        }
    }

    fn hit(&mut self, h: Hit) {
        match h.kind {
            H_TAB => self.focus(h.arg as usize),
            H_NEWTAB => self.open_focus(Source::Start),
            H_TILE => self.tile(h.arg as usize),
            H_PAGELINK => {
                if let Some(Store::Page(p)) = self.tabs.get_mut(self.active).map(|t| &mut t.store) {
                    p.focus = Some(h.arg as usize);
                }
                self.follow_focused();
            }
            H_URL => {
                let Some(t) = self.tabs.get(self.active) else { return };
                let url = t.arena.str(h.span).to_string();
                self.go(&url);
            }
            H_ENTRY => {
                let mut u = Fmt::<256>::new();
                self.entry_url(h.arg as usize, h.span.len as u8, &mut u);
                open_url(u.as_str());
            }
            _ => {}
        }
    }

    /// Moves the link focus to the next link at or below the top of the view.
    fn focus_link(&mut self, d: i32) {
        let Some(t) = self.tabs.get_mut(self.active) else { return };
        let top = t.top;
        let Store::Page(p) = &mut t.store else { return };
        if p.links.is_empty() {
            return;
        }
        let n = p.links.len() as i32;
        let next = match p.focus {
            Some(i) => ((i as i32 + d) % n + n) % n,
            None => {
                // the first link in or after the top block
                let first = p.blocks.get(top).map(|b| b.first_link).unwrap_or(0) as i32;
                if d > 0 { first.min(n - 1) } else { (first - 1).max(0) }
            }
        };
        p.focus = Some(next as usize);
        t.follow = true;
    }

    fn follow_focused(&mut self) {
        let Some(t) = self.tabs.get(self.active) else { return };
        let Store::Page(p) = &t.store else { return };
        let Some(i) = p.focus else { return self.say(Level::Info, "tab moves between links; enter follows one") };
        let Some(l) = p.links.get(i) else { return };
        let href = t.arena.str(l.href).to_string();
        // duckduckgo's result links carry the real address in a parameter
        let target = Url::parse(&href)
            .filter(|u| u.host.ends_with("duckduckgo.com") && u.path.starts_with("/l/"))
            .and_then(|u| u.param("uddg"))
            .unwrap_or(href);
        self.navigate(Source::Web { url: target.into() }, true);
    }

    fn move_cursor(&mut self, d: i64) {
        let Some(t) = self.tabs.get_mut(self.active) else { return };
        if let Store::Chat(c) = &mut t.store {
            c.scroll = (c.scroll as i64 - d).max(0) as u32;
            return;
        }
        let Store::Feed(f) = &t.store else { return };
        if f.is_empty() {
            return;
        }
        let n = f.len() as i64;
        t.cursor = (t.cursor as i64 + d).clamp(0, n - 1) as usize;
        t.follow = true;
        t.pending = 0;
        self.maybe_more();
    }

    fn cur_sid(&self) -> Option<(String, usize)> {
        let t = self.tabs.get(self.active)?;
        let f = t.feed()?;
        if f.is_empty() {
            return None;
        }
        Some((t.arena.str(f.sid[t.cursor]).to_string(), t.cursor))
    }

    fn open_entry(&mut self) {
        let Some(t) = self.tabs.get(self.active) else { return };
        let service = t.service();
        let Some((sid, i)) = self.cur_sid() else { return };
        let flags = t.feed().map(|f| f.flags[i]).unwrap_or(0);
        if flags & F_LOCAL_HIT != 0 {
            return;
        }
        match service {
            Service::X => self.open_focus(Source::XThread { root: sid.into() }),
            Service::YouTube if flags & F_VIDEO != 0 => self.play(i),
            _ => {}
        }
    }

    fn comments(&mut self) {
        let Some(t) = self.tabs.get(self.active) else { return };
        let Some((sid, i)) = self.cur_sid() else { return };
        if t.feed().is_some_and(|f| f.flags[i] & F_VIDEO == 0) {
            return;
        }
        self.open_focus(Source::YtComments { video: sid.into() });
    }

    fn toggle_media(&mut self) {
        let Some(t) = self.tabs.get_mut(self.active) else { return };
        let c = t.cursor;
        if let Store::Feed(f) = &mut t.store {
            if c < f.len() {
                f.flags[c] ^= F_SHOW_MEDIA;
            }
        }
    }

    fn reply(&mut self) {
        let Some(t) = self.tabs.get(self.active) else { return };
        if t.service() != Service::X {
            return;
        }
        let Some((sid, i)) = self.cur_sid() else { return };
        let author = t.feed().map(|f| f.author[i]).unwrap_or(u32::MAX);
        let handle = self.author_names(author).1.to_string();
        self.compose.clear();
        self.mode = Mode::Compose { reply_to: Some((sid, handle)) };
    }

    fn like(&mut self) {
        let Some(t) = self.tabs.get(self.active) else { return };
        if t.service() != Service::X {
            return;
        }
        let id = t.id;
        let Some((sid, _)) = self.cur_sid() else { return };
        self.send(Command::Like { tab: id, sid: sid.into() });
    }

    fn new_thing(&mut self) {
        let Some(t) = self.tabs.get(self.active) else { return };
        match t.service() {
            Service::X => {
                self.compose.clear();
                self.mode = Mode::Compose { reply_to: None };
            }
            s @ (Service::Claude | Service::ChatGpt) => self.open_focus(Source::Chat { service: s, chat: 1 }),
            _ => {}
        }
    }

    fn entry_url(&self, i: usize, kind: u8, out: &mut Fmt<256>) {
        let Some(t) = self.tabs.get(self.active) else { return };
        let Some(f) = t.feed() else { return };
        if i >= f.len() {
            return;
        }
        let sid = t.arena.str(f.sid[i]);
        let _ = match kind {
            1 => write!(out, "https://x.com/i/status/{sid}"),
            2 => write!(out, "https://www.youtube.com/watch?v={sid}"),
            3 => write!(out, "https://i.ytimg.com/vi/{sid}/hqdefault.jpg"),
            _ => Ok(()),
        };
    }

    fn open_in_browser(&mut self) {
        let Some(t) = self.tabs.get(self.active) else { return };
        match &t.src {
            Source::Web { url } => open_url(url),
            _ => {
                let kind = match t.service() {
                    Service::X => 1,
                    Service::YouTube => 2,
                    _ => return,
                };
                let mut u = Fmt::<256>::new();
                self.entry_url(t.cursor, kind, &mut u);
                open_url(u.as_str());
            }
        }
    }

    fn play(&mut self, i: usize) {
        let Some(t) = self.tabs.get(self.active) else { return };
        let Some(f) = t.feed() else { return };
        let (sid, pos) = (t.arena.str(f.sid[i]).to_string(), f.position[i]);
        let id = t.id;
        match crate::mpv::play(&self.cfg, &sid, pos) {
            Ok(p) => {
                self.mpv = Some(p);
                self.playing = Some((id, i));
                self.say(Level::Info, "playing in mpv · hayate waits until it closes");
            }
            Err(e) => self.say(Level::Error, &e),
        }
    }

    /// mpv closed: record where it stopped and show it in the feed.
    pub fn played(&mut self, pos: Option<u32>, err: Option<String>) {
        let Some((tab, i)) = self.playing.take() else { return };
        if let Some(e) = err {
            self.say(Level::Error, &e);
        }
        let Some(ti) = self.tab_index(tab) else { return };
        let t = &mut self.tabs[ti];
        if let Store::Feed(f) = &mut t.store {
            if i < f.len() {
                let p = pos.unwrap_or(0);
                f.position[i] = p;
                let video = t.arena.str(f.sid[i]).to_string();
                self.send(Command::Position { video: video.into(), secs: p });
            }
        }
        self.dirty = true;
    }

    fn search(&mut self, q: &str, local: bool) {
        let svc = self.tabs.get(self.active).map(|t| t.service());
        let src = if local || !matches!(svc, Some(Service::X | Service::YouTube)) {
            Source::LocalSearch { q: q.into() }
        } else if svc == Some(Service::YouTube) {
            Source::YtSearch { q: q.into() }
        } else {
            Source::XSearch { q: q.into() }
        };
        self.open_focus(src);
    }

    pub fn command(&mut self, line: &str) {
        let line = line.trim().trim_start_matches(':');
        let mut parts = line.splitn(2, ' ');
        let cmd = parts.next().unwrap_or("");
        let arg = parts.next().unwrap_or("").trim();
        match cmd {
            "mem" => self.mode = Mode::Overlay(Overlay::Mem),
            "net" => self.mode = Mode::Overlay(Overlay::Net),
            "spend" => self.mode = Mode::Overlay(Overlay::Spend),
            "help" | "h" => self.mode = Mode::Overlay(Overlay::Help),
            "q" | "quit" | "qa" => self.quit = true,
            "close" => self.close_active(),
            "reopen" => self.reopen(),
            "new" | "tab" => self.open_focus(Source::Start),
            "refresh" | "reload" => self.reload(),
            "model" => self.model(arg),
            "system" => self.system(arg),
            "export" => self.export(arg),
            "search" => self.search(arg, true),
            "open" | "go" => self.go(arg),
            "" => {}
            _ => self.say(Level::Warn, "unknown command; ctrl+k lists them"),
        }
        self.dirty = true;
    }

    fn model(&mut self, arg: &str) {
        let Some(t) = self.tabs.get_mut(self.active) else { return };
        if !t.service().is_chat() {
            return self.say(Level::Warn, ":model works in a chat tab");
        }
        if arg.is_empty() {
            let s = format!("model: {} · :model <name> switches (claude-* or gpt-*)", t.model);
            return self.say(Level::Info, &s);
        }
        let service = if arg.starts_with("claude") { Service::Claude } else { Service::ChatGpt };
        if self.cfg.price(arg).is_none() {
            let s = format!("no price for {arg}; add `price.{arg} = in out` to the config");
            return self.say(Level::Warn, &s);
        }
        let t = &mut self.tabs[self.active];
        t.model = arg.to_string();
        if let Source::Chat { service: s, .. } = &mut t.src {
            *s = service;
        }
        let id = t.id;
        self.send(Command::Branch { tab: id, service, model: arg.into() });
    }

    fn system(&mut self, arg: &str) {
        let Some(text) = self.cfg.system(arg).map(str::to_string) else {
            let names: Vec<&str> = self.cfg.systems.iter().map(|(n, _)| n.as_str()).collect();
            let s = format!("system presets: {}", names.join(", "));
            return self.say(Level::Info, &s);
        };
        let Some(t) = self.tabs.get(self.active) else { return };
        if !t.service().is_chat() {
            return self.say(Level::Warn, ":system works in a chat tab");
        }
        let id = t.id;
        self.send(Command::System { tab: id, text: text.into() });
        let s = format!("system prompt `{arg}` applies from the next message");
        self.say(Level::Info, &s);
    }

    /// Writes the current view to a markdown file.
    fn export(&mut self, arg: &str) {
        let Some(t) = self.tabs.get(self.active) else { return };
        let mut md = String::new();
        let key = t.src.feed_key().replace([':', '/', '\\', ' ', '?', '&', '='], "-");
        match &t.store {
            Store::Feed(f) => {
                let _ = writeln!(md, "# {}\n", t.src.feed_key());
                for i in 0..f.len() {
                    let (name, handle) = self.author_names(f.author[i]);
                    let indent = "> ".repeat(f.depth[i] as usize);
                    let _ = writeln!(md, "{indent}**{name}** @{handle} · {}\n", draw::iso_date(f.ts[i]));
                    for line in t.arena.str(f.text[i]).lines() {
                        let _ = writeln!(md, "{indent}{line}");
                    }
                    md.push('\n');
                }
            }
            Store::Chat(c) => {
                let _ = writeln!(md, "# {} chat\n", t.service().name());
                for m in &c.msgs {
                    match m.role {
                        Role::User => md.push_str("**you**\n\n"),
                        Role::Assistant => {
                            let _ = writeln!(md, "**{}**\n", t.arena.str(m.model));
                        }
                        Role::Note => md.push_str("_note_\n\n"),
                    }
                    md.push_str(t.arena.str(m.text));
                    md.push_str("\n\n");
                }
            }
            Store::Page(p) => {
                let _ = writeln!(md, "# {}\n\n{}\n", t.arena.str(p.title), t.arena.str(p.url));
                for b in &p.blocks {
                    let text = t.arena.str(b.text);
                    match b.kind {
                        1..=6 => {
                            let _ = writeln!(md, "{} {text}\n", "#".repeat(b.kind as usize + 1));
                        }
                        crate::html::K_LI => {
                            let _ = writeln!(md, "{}- {text}", "  ".repeat(b.depth.saturating_sub(1) as usize));
                        }
                        crate::html::K_PRE => {
                            let _ = writeln!(md, "```\n{text}\n```\n");
                        }
                        crate::html::K_QUOTE => {
                            let _ = writeln!(md, "> {text}\n");
                        }
                        crate::html::K_HR => md.push_str("---\n\n"),
                        _ => {
                            let _ = writeln!(md, "{text}\n");
                        }
                    }
                }
            }
            Store::Start(_) => return self.say(Level::Info, "nothing to export on a new tab"),
        }
        let path = if arg.is_empty() { format!("hayate-{key}.md") } else { arg.to_string() };
        match std::fs::write(&path, md) {
            Ok(()) => {
                let s = format!("wrote {path}");
                self.say(Level::Info, &s)
            }
            Err(e) => {
                let s = format!("{path}: {e}");
                self.say(Level::Error, &s)
            }
        }
    }

    fn author_names(&self, idx: u32) -> (&str, &str) {
        match self.authors.get(idx as usize) {
            Some(&(n, h)) => (self.authors_arena.str(n), self.authors_arena.str(h)),
            None => ("", ""),
        }
    }

    /// Bytes held by row tables on the ui side (the store arena).
    pub fn store_bytes(&self) -> usize {
        let mut n = self.authors.capacity() * 16;
        for t in &self.tabs {
            n += match &t.store {
                Store::Feed(f) => f.bytes(),
                Store::Chat(c) => c.msgs.capacity() * core::mem::size_of::<Msg>() + c.wrap.starts.capacity() * 5,
                Store::Page(p) => p.blocks.capacity() * core::mem::size_of::<PageBlock>() + p.links.capacity() * core::mem::size_of::<PageLink>(),
                Store::Start(_) => 0,
            };
        }
        n
    }

    /// Paints and diffs one frame, returning the bytes to write.
    pub fn frame(&mut self) -> &[u8] {
        self.render();
        crate::arena::set(ArenaId::Store, self.store_bytes());
        self.dirty = false;
        self.screen.present()
    }
}

pub fn open_url(url: &str) {
    if url.starts_with("https://") || url.starts_with("http://") {
        let _ = std::process::Command::new("rundll32").args(["url.dll,FileProtocolHandler", url]).spawn();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn input_editing() {
        let mut i = Input::new(64);
        for c in "héllo wörld".chars() {
            i.key(Key::Char(c), false);
        }
        i.key(Key::CtrlBackspace, false);
        assert_eq!(i.as_str(), "héllo ");
        i.key(Key::Left, false);
        i.key(Key::Left, false);
        i.key(Key::Backspace, false);
        assert_eq!(i.as_str(), "hélo ");
    }

    #[test]
    fn fuzzy_matches_subsequences() {
        assert!(fuzzy("Open address or search", "addr"));
        assert!(fuzzy("Hacker News", "hn"));
        assert!(fuzzy("X: home timeline", "x home"));
        assert!(!fuzzy("Quit", "qx"));
    }
}
