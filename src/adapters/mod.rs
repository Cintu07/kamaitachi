//! Adapters: one per service. Each is a table of endpoints, an auth method, the json paths
//! to keep, a pagination cursor and a cost model. This module holds what they share: the
//! feed sink that turns matched fields into rows, the log, authors, secrets and time.

pub mod chat;
pub mod web;
pub mod x;
pub mod youtube;

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use crate::json::{Scanner, Sink};
use crate::msg::{Command, Level, Line, Reply, Service, Source, TabId};
use crate::net::{month_of, yt_day_of, Req, Shared};
use crate::store::*;
use crate::sys::{self, Locked};

// ---------------------------------------------------------------- field roles

// Field ids are roles: every adapter maps its json paths onto these.
pub const SID: u16 = 1;
pub const TEXT: u16 = 2;
pub const TEXT_LONG: u16 = 3;
pub const EXTRA: u16 = 4;
pub const TIME: u16 = 5;
pub const METRIC: u16 = 6;
pub const AKEY: u16 = 7;
pub const ANAME: u16 = 8;
pub const AHANDLE: u16 = 9;
pub const REF_ID: u16 = 10;
pub const REF_TYPE: u16 = 11;
pub const MEDIA: u16 = 12;
pub const NEXT: u16 = 13;
pub const NEWEST: u16 = 14;
pub const OLDEST: u16 = 15;
pub const ERR: u16 = 16;
pub const UKEY: u16 = 17;
pub const UNAME: u16 = 18;
pub const UHANDLE: u16 = 19;
pub const COUNT: u16 = 20;

// record ids
pub const R_POST: u16 = 1;
pub const R_USER: u16 = 2;
pub const R_REPLY: u16 = 3;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Dest {
    Tab,
    Author,
    Small,
}

fn dest(role: u16) -> Dest {
    match role {
        SID | TEXT | TEXT_LONG | EXTRA => Dest::Tab,
        ANAME | AHANDLE | UNAME | UHANDLE => Dest::Author,
        _ => Dest::Small,
    }
}

// ---------------------------------------------------------------- jobs

pub enum Job {
    Feed(Box<FeedJob>),
    Chat(Box<chat::ChatJob>),
    Web(Box<web::WebJob>),
    /// a small response collected whole (auth, ids, a post result)
    Small(Small, Vec<u8>),
}

pub enum Small {
    XMe { tab: TabId },
    XPosted { tab: TabId },
    XLiked { tab: TabId, sid: String },
    YtSubs { tab: TabId, acc: Vec<String> },
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum FeedKind {
    XTimeline,
    XSearch,
    XThreadRoot,
    XThreadReplies,
    YtPlaylist,
    YtSearch,
    YtVideo,
    YtComments,
}

pub struct FeedJob {
    pub scanner: Scanner<'static>,
    pub st: FeedState,
}

impl FeedJob {
    pub fn new(paths: &'static crate::json::Paths, tab: TabId, kind: FeedKind, service: Service, html: bool) -> Box<FeedJob> {
        Box::new(FeedJob { scanner: Scanner::new(paths), st: FeedState::new(tab, kind, service, html) })
    }
}

#[derive(Clone, Copy)]
pub struct Small40 {
    pub b: [u8; 40],
    pub n: u8,
}

impl Default for Small40 {
    fn default() -> Self {
        Small40 { b: [0; 40], n: 0 }
    }
}

impl Small40 {
    pub fn set(&mut self, s: &[u8]) {
        let n = s.len().min(40);
        self.b[..n].copy_from_slice(&s[..n]);
        self.n = n as u8;
    }
    pub fn get(&self) -> &[u8] {
        &self.b[..self.n as usize]
    }
    pub fn str(&self) -> &str {
        crate::text::utf8_prefix(self.get())
    }
}

#[derive(Clone, Copy, Default)]
pub struct Draft {
    pub sid: Span,
    pub text: Span,
    pub text_long: Span,
    pub extra: Span,
    pub ts: u32,
    pub metric: u32,
    pub akey: Small40,
    pub aname: Span,
    pub ahandle: Span,
    pub parent: Small40,
    ref_id: Small40,
    ref_type: Small40,
    pub flags: u16,
    pub depth: u8,
    emitted: bool,
}

#[derive(Default)]
struct UserDraft {
    key: Small40,
    name: Span,
    handle: Span,
}

/// Up to 10 bytes after an '&', waiting for the ';' of an html entity.
#[derive(Default)]
struct Ent {
    buf: [u8; 10],
    n: usize,
    on: bool,
}

pub struct FeedState {
    pub tab: TabId,
    pub kind: FeedKind,
    pub service: Service,
    html: bool,
    field: u16,
    fstart: u32,
    small: [u8; 256],
    small_len: usize,
    drafts: [Draft; 2],
    level: usize,
    user: UserDraft,
    ent: Ent,
    pub next: String,
    pub newest: String,
    pub oldest: String,
    pub err: String,
    /// records seen, including duplicates (what the service bills)
    pub raw: u32,
    /// rows emitted
    pub count: u32,
    pub overflow: bool,
    /// when set, rows are collected (threads are ordered before they are shown)
    pub collect: Option<Vec<(Post, u32)>>,
    pub extra_flags: u16,
    /// where each open record started writing, and how much it wrote, in the tab and
    /// author arenas: a duplicate record is rolled back so it costs no memory
    mark: [(u32, u32, u32, u32); 2],
}

impl FeedState {
    pub fn new(tab: TabId, kind: FeedKind, service: Service, html: bool) -> FeedState {
        FeedState {
            tab,
            kind,
            service,
            html,
            field: 0,
            fstart: 0,
            small: [0; 256],
            small_len: 0,
            drafts: [Draft::default(); 2],
            level: 0,
            user: UserDraft::default(),
            ent: Ent::default(),
            next: String::new(),
            newest: String::new(),
            oldest: String::new(),
            err: String::new(),
            raw: 0,
            count: 0,
            overflow: false,
            collect: None,
            extra_flags: 0,
            mark: [(0, 0, 0, 0); 2],
        }
    }
}

struct FeedCtx<'a> {
    st: &'a mut FeedState,
    sh: &'a mut Shared,
    arena: &'a TextArena,
}

impl FeedCtx<'_> {
    fn target(&self) -> &TextArena {
        if dest(self.st.field) == Dest::Author { &self.sh.authors.arena } else { self.arena }
    }

    fn push(&mut self, b: &[u8]) {
        let to_author = dest(self.st.field) == Dest::Author;
        let ok = self.target().push(b);
        if !ok {
            self.st.overflow = true;
            return;
        }
        let m = &mut self.st.mark[self.st.level];
        if to_author { m.3 += b.len() as u32 } else { m.1 += b.len() as u32 }
    }

    fn set_mark(&mut self, level: usize) {
        self.st.mark[level] = (self.arena.writer_len(), 0, self.sh.authors.arena.writer_len(), 0);
    }

    /// Gives back what a record wrote, if its bytes are still the last ones written.
    fn rollback(&mut self, level: usize, tab: bool, author: bool) {
        let (ts, tn, as_, an) = self.st.mark[level];
        if tab && self.arena.writer_len() == ts + tn {
            self.arena.truncate(ts);
            self.st.mark[level].1 = 0;
        }
        let aa = &self.sh.authors.arena;
        if author && aa.writer_len() == as_ + an {
            aa.truncate(as_);
            self.st.mark[level].3 = 0;
        }
    }

    /// Writes field bytes, decoding html entities for text fields of services that send
    /// them escaped (x post text, youtube titles).
    fn write(&mut self, mut b: &[u8]) {
        let decode = self.st.html && matches!(self.st.field, TEXT | TEXT_LONG | EXTRA | ANAME);
        if !decode {
            self.push(b);
            return;
        }
        while !b.is_empty() {
            if !self.st.ent.on {
                match b.iter().position(|&c| c == b'&') {
                    None => {
                        self.push(b);
                        return;
                    }
                    Some(p) => {
                        self.push(&b[..p]);
                        self.st.ent.on = true;
                        self.st.ent.n = 0;
                        b = &b[p + 1..];
                    }
                }
            } else {
                let c = b[0];
                b = &b[1..];
                if c == b';' {
                    let n = self.st.ent.n;
                    let name = self.st.ent.buf;
                    self.st.ent.on = false;
                    match decode_entity(&name[..n]) {
                        Some(ch) => {
                            let mut t = [0u8; 4];
                            self.push(ch.encode_utf8(&mut t).as_bytes());
                        }
                        None => {
                            self.push(b"&");
                            self.push(&name[..n]);
                            self.push(b";");
                        }
                    }
                } else if (c.is_ascii_alphanumeric() || c == b'#') && self.st.ent.n < 10 {
                    let n = self.st.ent.n;
                    self.st.ent.buf[n] = c;
                    self.st.ent.n += 1;
                } else {
                    self.flush_ent();
                    self.push(&[c]);
                }
            }
        }
    }

    fn flush_ent(&mut self) {
        if self.st.ent.on {
            self.st.ent.on = false;
            let n = self.st.ent.n;
            let buf = self.st.ent.buf;
            self.push(b"&");
            self.push(&buf[..n]);
        }
    }

    fn draft(&mut self) -> &mut Draft {
        &mut self.st.drafts[self.st.level]
    }

    fn emit(&mut self, level: usize) {
        let d = self.st.drafts[level];
        if d.emitted {
            return;
        }
        self.st.drafts[level].emitted = true;
        self.st.raw += 1;
        let tab = self.st.tab;
        let sid = self.arena.peek(d.sid);
        if sid.is_empty() {
            return self.rollback(level, true, true);
        }
        let h = self.sh.interner.intern(sid);
        let Some(t) = self.sh.tabs.get_mut(&tab) else { return };
        if !t.seen.insert(h) {
            // already shown: this record's bytes go back
            return self.rollback(level, true, true);
        }
        let text = if !d.text_long.is_empty() { d.text_long } else { d.text };
        let akey = d.akey;
        let (author, fresh) = self.sh.authors.resolve_spans(&mut self.sh.interner, akey.get(), d.aname, d.ahandle, &mut self.sh.out);
        if !fresh {
            self.rollback(level, false, true);
        }
        let mut flags = d.flags | self.st.extra_flags;
        if self.st.service == Service::YouTube && matches!(self.st.kind, FeedKind::YtPlaylist | FeedKind::YtSearch | FeedKind::YtVideo) {
            flags |= F_VIDEO;
        }
        let position = if flags & F_VIDEO != 0 { self.sh.ad.positions.get(sid).copied().unwrap_or(0) } else { 0 };
        let post = Post { sid: d.sid, author, ts: d.ts, text, extra: d.extra, metric: d.metric, position, flags, depth: d.depth };
        // track the newest and oldest ids held, for since_id / until_id paging
        let t = self.sh.tabs.get_mut(&tab).unwrap();
        let sid_s = crate::text::utf8_prefix(sid);
        if id_newer(sid_s, &t.newest_sid) {
            t.newest_sid = sid_s.to_string();
        }
        if t.oldest_sid.is_empty() || id_newer(&t.oldest_sid, sid_s) {
            t.oldest_sid = sid_s.to_string();
        }
        if t.oldest_ts == 0 || post.ts < t.oldest_ts {
            t.oldest_ts = post.ts;
        }
        let key = t.key;
        log_post(self.sh, self.st.service, key, &post, self.arena, d.parent.get());
        self.st.count += 1;
        let parent = if d.parent.n > 0 { self.sh.interner.intern(d.parent.get()) } else { u32::MAX };
        match &mut self.st.collect {
            Some(v) => v.push((post, parent)),
            None => self.sh.out.push(Reply::Post { tab, post }),
        }
    }
}

impl Sink for FeedCtx<'_> {
    fn record_start(&mut self, rec: u16) {
        match rec {
            R_POST => {
                self.st.level = 0;
                self.st.drafts[0] = Draft::default();
                self.set_mark(0);
            }
            R_REPLY => {
                // a youtube comment's replies: show the top comment first
                self.emit(0);
                self.st.level = 1;
                self.st.drafts[1] = Draft { depth: 1, ..Draft::default() };
                self.set_mark(1);
            }
            R_USER => {
                self.st.user = UserDraft::default();
                self.set_mark(0);
            }
            _ => {}
        }
    }

    fn record_end(&mut self, rec: u16) {
        match rec {
            R_POST => {
                self.emit(0);
            }
            R_REPLY => {
                self.emit(1);
                self.st.level = 0;
            }
            R_USER => {
                let u = std::mem::take(&mut self.st.user);
                let (_, fresh) = self.sh.authors.resolve_spans(&mut self.sh.interner, u.key.get(), u.name, u.handle, &mut self.sh.out);
                if !fresh {
                    self.rollback(0, false, true);
                }
            }
            _ => {}
        }
    }

    fn field_start(&mut self, field: u16, _string: bool) {
        self.st.field = field;
        self.st.small_len = 0;
        self.st.ent.on = false;
        self.st.fstart = self.target().writer_len();
    }

    fn bytes(&mut self, b: &[u8]) {
        match dest(self.st.field) {
            Dest::Small => {
                let n = b.len().min(self.st.small.len() - self.st.small_len);
                let l = self.st.small_len;
                self.st.small[l..l + n].copy_from_slice(&b[..n]);
                self.st.small_len += n;
            }
            _ => self.write(b),
        }
    }

    fn field_end(&mut self, field: u16) {
        self.flush_ent();
        let span = Span { off: self.st.fstart, len: self.target().writer_len() - self.st.fstart };
        let small_len = self.st.small_len;
        let small = self.st.small;
        let small = &small[..small_len];
        match field {
            SID => self.draft().sid = span,
            TEXT => self.draft().text = span,
            TEXT_LONG => self.draft().text_long = span,
            EXTRA => self.draft().extra = span,
            TIME => self.draft().ts = parse_iso(small),
            METRIC => self.draft().metric = parse_u32(small),
            AKEY => self.draft().akey.set(small),
            ANAME => self.draft().aname = span,
            AHANDLE => self.draft().ahandle = span,
            REF_ID | REF_TYPE => {
                let d = self.draft();
                if field == REF_ID { d.ref_id.set(small) } else { d.ref_type.set(small) }
                if d.ref_id.n > 0 && d.ref_type.n > 0 {
                    if d.ref_type.get() == b"replied_to" {
                        d.parent = d.ref_id;
                    }
                    d.ref_id.n = 0;
                    d.ref_type.n = 0;
                }
            }
            MEDIA => self.draft().flags |= F_HAS_MEDIA,
            NEXT => self.st.next = String::from_utf8_lossy(small).into_owned(),
            NEWEST => self.st.newest = String::from_utf8_lossy(small).into_owned(),
            OLDEST => self.st.oldest = String::from_utf8_lossy(small).into_owned(),
            ERR => {
                if !self.st.err.is_empty() {
                    self.st.err.push_str("; ");
                }
                self.st.err.push_str(&String::from_utf8_lossy(small));
            }
            UKEY => self.st.user.key.set(small),
            UNAME => self.st.user.name = span,
            UHANDLE => self.st.user.handle = span,
            _ => {}
        }
    }
}

fn decode_entity(name: &[u8]) -> Option<char> {
    Some(match name {
        b"amp" => '&',
        b"lt" => '<',
        b"gt" => '>',
        b"quot" => '"',
        b"apos" => '\'',
        b"nbsp" => ' ',
        [b'#', b'x' | b'X', hex @ ..] => char::from_u32(u32::from_str_radix(std::str::from_utf8(hex).ok()?, 16).ok()?)?,
        [b'#', dec @ ..] => char::from_u32(std::str::from_utf8(dec).ok()?.parse().ok()?)?,
        _ => return None,
    })
}

/// True when id `a` is newer than `b` (x ids are numbers that grow over time).
pub fn id_newer(a: &str, b: &str) -> bool {
    b.is_empty() || (a.len(), a) > (b.len(), b)
}

// ---------------------------------------------------------------- authors

/// Author names live once, in a shared arena; rows hold a 4-byte index.
pub struct Authors {
    pub arena: Arc<TextArena>,
    map: HashMap<u32, u32>,
    named: Vec<bool>,
    /// name, handle and service key of each author, for writing log records
    info: Vec<(Span, Span, Vec<u8>)>,
}

impl Authors {
    pub fn new(arena: Arc<TextArena>) -> Authors {
        Authors { arena, map: HashMap::new(), named: Vec::new(), info: Vec::new() }
    }

    fn idx(&mut self, interner: &mut Interner, key: &[u8]) -> u32 {
        let k = interner.intern(key);
        let next = self.named.len() as u32;
        let idx = *self.map.entry(k).or_insert(next);
        if idx == next {
            self.named.push(false);
            self.info.push((Span::EMPTY, Span::EMPTY, key.to_vec()));
        }
        idx
    }

    /// An author's name, handle and key as bytes.
    pub fn bytes(&self, idx: u32) -> (Vec<u8>, Vec<u8>, Vec<u8>) {
        match self.info.get(idx as usize) {
            Some((n, h, k)) => (self.arena.peek(*n).to_vec(), self.arena.peek(*h).to_vec(), k.clone()),
            None => (vec![], vec![], vec![]),
        }
    }

    /// For names already written into the author arena while scanning. Returns the
    /// author's index and whether these spans became its name (false: they are spare).
    pub fn resolve_spans(&mut self, interner: &mut Interner, key: &[u8], name: Span, handle: Span, out: &mut Vec<Reply>) -> (u32, bool) {
        let key = if key.is_empty() { self.arena.peek(name).to_vec() } else { key.to_vec() };
        if key.is_empty() {
            return (u32::MAX, false);
        }
        let idx = self.idx(interner, &key);
        if !self.named[idx as usize] && !(name.is_empty() && handle.is_empty()) {
            self.named[idx as usize] = true;
            self.info[idx as usize].0 = name;
            self.info[idx as usize].1 = handle;
            out.push(Reply::Author { idx, name, handle });
            return (idx, true);
        }
        (idx, false)
    }

    /// For names coming from the log: written only if this author is new.
    pub fn resolve_bytes(&mut self, interner: &mut Interner, key: &[u8], name: &[u8], handle: &[u8], out: &mut Vec<Reply>) -> u32 {
        let key = if key.is_empty() { name } else { key };
        if key.is_empty() {
            return u32::MAX;
        }
        let idx = self.idx(interner, key);
        if !self.named[idx as usize] && !(name.is_empty() && handle.is_empty()) {
            let n = self.arena.append(name).unwrap_or_default();
            let h = self.arena.append(handle).unwrap_or_default();
            self.named[idx as usize] = true;
            self.info[idx as usize].0 = n;
            self.info[idx as usize].1 = h;
            out.push(Reply::Author { idx, name: n, handle: h });
        }
        idx
    }
}

// ---------------------------------------------------------------- per-tab state

pub struct NetTab {
    pub arena: Arc<TextArena>,
    pub src: Source,
    pub key: u32,
    pub seen: HashSet<u32>,
    pub newest_sid: String,
    pub oldest_sid: String,
    pub oldest_ts: u32,
    pub next_token: String,
    pub pending: u32,
    pub chat: Option<chat::ChatState>,
    pub thread: Vec<(Post, u32)>,
    pub log_exhausted: bool,
}

/// Small persistent state: the meta key/values and video positions from the log.
#[derive(Default)]
pub struct State {
    pub meta: HashMap<String, String>,
    pub positions: HashMap<Vec<u8>, u32>,
    pub chat_seq: u32,
}

// ---------------------------------------------------------------- secrets

pub struct Secrets {
    pub anthropic: Locked,
    pub openai: Locked,
    pub yt_key: Locked,
    pub yt_token: Locked,
    pub yt_exp: u64,
    pub x_token: Locked,
    pub x_exp: u64,
}

impl Secrets {
    pub fn load() -> Secrets {
        let mut s = Secrets {
            anthropic: Locked::new(),
            openai: Locked::new(),
            yt_key: Locked::new(),
            yt_token: Locked::new(),
            yt_exp: 0,
            x_token: Locked::new(),
            x_exp: 0,
        };
        if let Ok(v) = std::env::var("HAYATE_TEST_KEYS") {
            // replay tests: fake keys so every adapter takes its signed-in path
            if v == "1" {
                for l in [&mut s.anthropic, &mut s.openai, &mut s.yt_key, &mut s.yt_token, &mut s.x_token] {
                    l.set(b"test-key");
                }
                s.yt_exp = u64::MAX;
                s.x_exp = u64::MAX;
                return s;
            }
        }
        // credential manager first; the usual environment variables as a fallback
        if !sys::cred_read("anthropic", &mut s.anthropic) {
            if let Ok(v) = std::env::var("ANTHROPIC_API_KEY") {
                s.anthropic.set(v.as_bytes());
            }
        }
        if !sys::cred_read("openai", &mut s.openai) {
            if let Ok(v) = std::env::var("OPENAI_API_KEY") {
                s.openai.set(v.as_bytes());
            }
        }
        sys::cred_read("youtube-key", &mut s.yt_key);
        let mut tmp = Locked::new();
        if sys::cred_read("x-access", &mut tmp) {
            if let Some((exp, tok)) = split_token(tmp.as_bytes()) {
                s.x_exp = exp;
                s.x_token.set(tok);
            }
        }
        s
    }
}

/// Whether hayate can use a service at all: a key, a sign-in, or (youtube) channels in
/// the config. Reads credential manager and drops what it read.
pub fn has_credentials(s: Service) -> bool {
    if std::env::var("HAYATE_TEST_KEYS").is_ok_and(|v| v == "1") {
        return true;
    }
    let mut l = Locked::new();
    match s {
        Service::Claude => sys::cred_read("anthropic", &mut l) || std::env::var("ANTHROPIC_API_KEY").is_ok(),
        Service::ChatGpt => sys::cred_read("openai", &mut l) || std::env::var("OPENAI_API_KEY").is_ok(),
        Service::X => sys::cred_read("x-refresh", &mut l),
        Service::YouTube => sys::cred_read("youtube-key", &mut l) || sys::cred_read("youtube-refresh", &mut l),
        Service::Local | Service::Web => true,
    }
}

/// Stored tokens are "<unix expiry> <token>".
pub fn split_token(b: &[u8]) -> Option<(u64, &[u8])> {
    let sp = b.iter().position(|&c| c == b' ')?;
    let exp = std::str::from_utf8(&b[..sp]).ok()?.parse().ok()?;
    Some((exp, &b[sp + 1..]))
}

// ---------------------------------------------------------------- log helpers

pub fn log_post(sh: &mut Shared, service: Service, feed: u32, p: &Post, arena: &TextArena, parent: &[u8]) {
    let (aname, ahandle, akey) = sh.authors.bytes(p.author);
    sh.rec
        .start(REC_POST)
        .u32(feed)
        .u32(p.ts)
        .u32(p.metric)
        .u32(p.flags as u32)
        .u8(p.depth)
        .str(arena.peek(p.sid))
        .str(&akey)
        .str(&aname)
        .str(&ahandle)
        .str(arena.peek(p.text))
        .str(arena.peek(p.extra))
        .str(parent);
    if let Some(l) = sh.logs.for_service(service) {
        let _ = l.append(&sh.rec.b);
    }
}

pub struct LoggedPost<'a> {
    pub feed: u32,
    pub ts: u32,
    pub metric: u32,
    pub flags: u32,
    pub depth: u8,
    pub sid: &'a [u8],
    pub akey: &'a [u8],
    pub aname: &'a [u8],
    pub ahandle: &'a [u8],
    pub text: &'a [u8],
    pub extra: &'a [u8],
    pub parent: &'a [u8],
}

pub fn read_post(payload: &[u8]) -> Option<LoggedPost<'_>> {
    if payload.first() != Some(&REC_POST) {
        return None;
    }
    let mut r = RecReader::new(&payload[1..]);
    Some(LoggedPost {
        feed: r.u32()?,
        ts: r.u32()?,
        metric: r.u32()?,
        flags: r.u32()?,
        depth: r.u8()?,
        sid: r.str()?,
        akey: r.str()?,
        aname: r.str()?,
        ahandle: r.str()?,
        text: r.str()?,
        extra: r.str()?,
        parent: r.str()?,
    })
}

/// Writes a logged post into a tab and emits it. Returns false if it was already shown.
pub fn emit_logged(sh: &mut Shared, tab: TabId, lp: &LoggedPost, extra_flags: u16, collect: bool) -> bool {
    let Some(t) = sh.tabs.get(&tab) else { return false };
    let arena = t.arena.clone();
    let h = sh.interner.intern(lp.sid);
    if !sh.tabs.get_mut(&tab).unwrap().seen.insert(h) {
        return false;
    }
    let author = sh.authors.resolve_bytes(&mut sh.interner, lp.akey, lp.aname, lp.ahandle, &mut sh.out);
    let sid = arena.append(lp.sid).unwrap_or_default();
    let text = arena.append(lp.text).unwrap_or_default();
    let extra = arena.append(lp.extra).unwrap_or_default();
    let flags = lp.flags as u16 | F_FROM_CACHE | extra_flags;
    let position = if flags & F_VIDEO != 0 { sh.ad.positions.get(lp.sid).copied().unwrap_or(0) } else { 0 };
    let post = Post { sid, author, ts: lp.ts, text, extra, metric: lp.metric, position, flags, depth: lp.depth };
    let t = sh.tabs.get_mut(&tab).unwrap();
    let sid_s = crate::text::utf8_prefix(lp.sid);
    if id_newer(sid_s, &t.newest_sid) {
        t.newest_sid = sid_s.to_string();
    }
    if t.oldest_sid.is_empty() || id_newer(&t.oldest_sid, sid_s) {
        t.oldest_sid = sid_s.to_string();
    }
    if t.oldest_ts == 0 || lp.ts < t.oldest_ts {
        t.oldest_ts = lp.ts;
    }
    if collect {
        let parent = if lp.parent.is_empty() { u32::MAX } else { sh.interner.intern(lp.parent) };
        sh.tabs.get_mut(&tab).unwrap().thread.push((post, parent));
    } else {
        sh.out.push(Reply::Post { tab, post });
    }
    true
}

/// Restores up to `limit` of a feed's posts from the log, newest first, older than
/// `before_ts` when given. No network.
pub fn restore(sh: &mut Shared, tab: TabId, limit: usize, before_ts: Option<u32>) -> usize {
    let Some(t) = sh.tabs.get(&tab) else { return 0 };
    let (key, service) = (t.key, t.src.service());
    let Some(log) = sh.logs.for_service(service) else { return 0 };
    let Ok(Some(map)) = log.map() else { return 0 };
    let bytes = map.bytes();
    // pass 1: the offsets of matching records, keeping the last `limit * 2`
    let mut offs: std::collections::VecDeque<usize> = std::collections::VecDeque::new();
    for (at, p) in records(bytes) {
        if let Some(lp) = read_post(p) {
            if lp.feed == key && before_ts.is_none_or(|b| lp.ts < b) {
                offs.push_back(at);
                if offs.len() > limit * 2 {
                    offs.pop_front();
                }
            }
        }
    }
    let mut n = 0;
    for &at in offs.iter().rev() {
        let mut it = records(&bytes[at..]);
        let Some((_, p)) = it.next() else { continue };
        let Some(lp) = read_post(p) else { continue };
        if emit_logged(sh, tab, &lp, 0, false) {
            n += 1;
            if n >= limit {
                break;
            }
        }
    }
    n
}

/// Finds a post by its service id anywhere in a service's log (a post fetched once is
/// never fetched again).
pub fn find_logged(sh: &mut Shared, service: Service, sid: &[u8]) -> Option<Vec<u8>> {
    let log = sh.logs.for_service(service)?;
    let map = log.map().ok()??;
    let mut found = None;
    for (_, p) in records(map.bytes()) {
        if let Some(lp) = read_post(p) {
            if lp.sid == sid {
                found = Some(p.to_vec());
            }
        }
    }
    found
}

/// Reads spend, meta and positions from the logs at start.
pub fn load_spend(sh: &mut Shared) {
    let now = sys::now_unix();
    let (month, day) = (month_of(now), yt_day_of(now));
    sh.spend.month = month;
    sh.spend.yt_day = day;
    if let Some(log) = sh.logs.meta() {
        if let Ok(Some(map)) = log.map() {
            for (_, p) in records(map.bytes()) {
                let mut r = RecReader::new(&p[1..]);
                match p[0] {
                    REC_SPEND => {
                        let (Some(svc), Some(micros), Some(units), Some(ts)) = (r.u8(), r.u64(), r.u32(), r.u32()) else { continue };
                        if month_of(ts as u64) == month {
                            if let Some(i) = crate::net::spend_slot(service_from(svc)) {
                                sh.spend.micros[i] += micros;
                            }
                        }
                        if yt_day_of(ts as u64) == day {
                            sh.spend.yt_units += units;
                        }
                    }
                    REC_META => {
                        let (Some(k), Some(v)) = (r.str(), r.str()) else { continue };
                        sh.ad.meta.insert(String::from_utf8_lossy(k).into_owned(), String::from_utf8_lossy(v).into_owned());
                    }
                    _ => {}
                }
            }
        }
    }
    if let Some(log) = sh.logs.for_service(Service::YouTube) {
        if let Ok(Some(map)) = log.map() {
            for (_, p) in records(map.bytes()) {
                if p[0] == REC_POS {
                    let mut r = RecReader::new(&p[1..]);
                    if let (Some(v), Some(s)) = (r.str(), r.u32()) {
                        sh.ad.positions.insert(v.to_vec(), s);
                    }
                }
            }
        }
    }
}

pub fn set_meta(sh: &mut Shared, k: &str, v: &str) {
    sh.ad.meta.insert(k.to_string(), v.to_string());
    sh.rec.start(REC_META).str(k.as_bytes()).str(v.as_bytes());
    if let Some(l) = sh.logs.meta() {
        let _ = l.append(&sh.rec.b);
    }
}

pub fn service_from(b: u8) -> Service {
    match b {
        0 => Service::X,
        1 => Service::YouTube,
        2 => Service::Claude,
        3 => Service::ChatGpt,
        _ => Service::Local,
    }
}

// ---------------------------------------------------------------- dispatch

pub fn startup(_sh: &mut Shared) {}

pub fn command(sh: &mut Shared, cmd: Command) {
    match cmd {
        Command::Open { tab, arena, src } => {
            let key = key_hash(&src.feed_key());
            sh.tabs.insert(
                tab,
                NetTab {
                    arena,
                    src: src.clone(),
                    key,
                    seen: HashSet::new(),
                    newest_sid: String::new(),
                    oldest_sid: String::new(),
                    oldest_ts: 0,
                    next_token: String::new(),
                    pending: 0,
                    chat: None,
                    thread: Vec::new(),
                    log_exhausted: false,
                },
            );
            match src.service() {
                Service::X => x::open(sh, tab),
                Service::YouTube => youtube::open(sh, tab),
                Service::Claude | Service::ChatGpt => chat::open(sh, tab),
                Service::Local => local_search(sh, tab),
                Service::Web => web::open(sh, tab),
            }
        }
        Command::More { tab } => match sh.tabs.get(&tab).map(|t| t.src.service()) {
            Some(Service::X) => x::more(sh, tab),
            Some(Service::YouTube) => youtube::more(sh, tab),
            Some(_) => done(sh, tab, false),
            None => {}
        },
        Command::Refresh { tab } => match sh.tabs.get(&tab).map(|t| t.src.service()) {
            Some(Service::X) => x::refresh(sh, tab),
            Some(Service::YouTube) => youtube::refresh(sh, tab),
            Some(Service::Web) => web::open(sh, tab),
            Some(_) => done(sh, tab, false),
            None => {}
        },
        Command::Send { tab, text, model, system } => chat::send(sh, tab, &text, &model, &system),
        Command::Branch { tab, service, model } => chat::branch(sh, tab, service, &model),
        Command::System { tab, text } => {
            if let Some(c) = sh.tabs.get_mut(&tab).and_then(|t| t.chat.as_mut()) {
                c.system = text.to_string();
            }
        }
        Command::Post { tab, text, reply_to } => x::post(sh, tab, &text, reply_to.as_deref()),
        Command::Like { tab, sid } => x::like(sh, tab, &sid),
        Command::Close { tab } => {
            if let Some(t) = sh.tabs.remove(&tab) {
                t.arena.decommit();
            }
        }
        Command::Position { video, secs } => {
            sh.ad.positions.insert(video.as_bytes().to_vec(), secs);
            sh.rec.start(REC_POS).str(video.as_bytes()).u32(secs);
            if let Some(l) = sh.logs.for_service(Service::YouTube) {
                let _ = l.append(&sh.rec.b);
            }
        }
        Command::Shutdown => {}
    }
}

pub fn done(sh: &mut Shared, tab: TabId, more: bool) {
    sh.out.push(Reply::Done { tab, more });
}

pub fn on_body(sh: &mut Shared, req: &mut Req, ctype: u8, body: &[u8]) {
    match &mut req.job {
        Job::Feed(j) => {
            let Some(arena) = sh.arena(j.st.tab) else { return };
            let j = &mut **j;
            j.scanner.feed(body, &mut FeedCtx { st: &mut j.st, sh, arena: &arena });
        }
        Job::Chat(j) => chat::on_body(sh, j, body),
        Job::Web(j) => web::on_body(sh, j, ctype, body),
        Job::Small(_, buf) => {
            if buf.len() < 1 << 20 {
                buf.extend_from_slice(body);
            }
        }
    }
}

/// A request finished with a 2xx. Returns its cost for :net (micro-dollars or units).
pub fn on_done(sh: &mut Shared, req: &mut Req) -> u32 {
    let service = req.service;
    let units = req.units;
    match std::mem::replace(&mut req.job, Job::Small(Small::XPosted { tab: u16::MAX }, Vec::new())) {
        Job::Feed(mut j) => {
            if let Some(arena) = sh.arena(j.st.tab) {
                j.scanner.finish(&mut FeedCtx { st: &mut j.st, sh, arena: &arena });
            }
            if j.st.overflow {
                sh.status(Some(j.st.tab), Level::Warn, "this tab hit its 1 mb text budget; close it (q) and reopen to start fresh");
            }
            match service {
                Service::X => x::feed_done(sh, j.st),
                _ => youtube::feed_done(sh, j.st, units),
            }
        }
        Job::Chat(j) => chat::done(sh, *j),
        Job::Web(j) => web::done(sh, *j),
        Job::Small(kind, body) => match kind {
            Small::XMe { tab } => x::me_done(sh, tab, &body),
            Small::XPosted { tab } => x::posted(sh, tab, &body),
            Small::XLiked { tab, sid } => x::liked(sh, tab, &sid),
            Small::YtSubs { tab, acc } => youtube::subs_done(sh, tab, &body, acc),
        },
    }
}

/// A 3xx with a Location. True when the adapter followed it.
pub fn on_redirect(sh: &mut Shared, req: &mut Req, location: &str) -> bool {
    match &req.job {
        Job::Web(j) => web::redirect(sh, j, location),
        _ => false,
    }
}

pub fn on_fail(sh: &mut Shared, req: &Req, status: u16, msg: &str, retry_at: u64) {
    let tab = req.tab;
    let mut line = msg.to_string();
    if retry_at > 0 {
        let wait = retry_at.saturating_sub(sys::now_unix());
        line.push_str(&format!(" · retry in {}", fmt_dur(wait)));
    }
    sh.out.push(Reply::Status { tab, level: Level::Error, line: Line::new(&line), retry_at });
    if let Job::Chat(j) = &req.job {
        chat::failed(sh, j.tab);
    }
    if status == 401 && req.service == Service::X {
        // the stored token is stale; the next call refreshes it
        sh.secrets.x_exp = 0;
    }
    if let Some(tab) = tab {
        if let Some(t) = sh.tabs.get_mut(&tab) {
            t.pending = t.pending.saturating_sub(1);
            if t.pending > 0 {
                return;
            }
        }
        done(sh, tab, false);
    }
}

/// Pulls a readable message out of an error response body from any of the four services.
pub fn error_text(body: &[u8]) -> String {
    crate::json_paths!(E {
        "error.message" => 1, Field;
        "error" => 1, Field;
        "detail" => 1, Field;
        "title" => 2, Field;
        "errors[*].message" => 1, Field;
        "errors[*].detail" => 1, Field;
        "message" => 1, Field;
        "error_description" => 1, Field;
    });
    let c = crate::json::collect(E, body);
    let mut s = c.get(1).or(c.get(2)).map(str::to_string).unwrap_or_default();
    if s.is_empty() {
        s = String::from_utf8_lossy(&body[..body.len().min(100)]).trim().to_string();
    }
    s.truncate(100);
    s
}

// ---------------------------------------------------------------- local search

/// Search across everything read: scans every mapped log with a simd byte search.
fn local_search(sh: &mut Shared, tab: TabId) {
    let Some(Source::LocalSearch { q }) = sh.tabs.get(&tab).map(|t| t.src.clone()) else { return };
    let needle = q.as_bytes().to_vec();
    let mut hits = 0usize;
    for svc in [Service::X, Service::YouTube, Service::Claude, Service::ChatGpt] {
        let Some(log) = sh.logs.for_service(svc) else { continue };
        let Ok(Some(map)) = log.map() else { continue };
        let bytes = map.bytes();
        let mut found: Vec<usize> = Vec::new();
        for (at, p) in records(bytes) {
            if find_ci(p, &needle).is_some() {
                found.push(at);
            }
        }
        for &at in found.iter().rev() {
            if hits >= 200 {
                break;
            }
            let Some((_, p)) = records(&bytes[at..]).next() else { continue };
            if let Some(lp) = read_post(p) {
                if find_ci(lp.text, &needle).is_some() || find_ci(lp.aname, &needle).is_some() {
                    if emit_logged(sh, tab, &lp, F_LOCAL_HIT, false) {
                        hits += 1;
                    }
                }
            } else if p[0] == REC_MSG {
                if let Some(m) = chat::read_msg(p) {
                    if find_ci(m.text, &needle).is_some() {
                        let who: &[u8] = if m.role == Role::User as u8 { b"you" } else { m.model };
                        let mut sid = b"msg:".to_vec();
                        sid.extend_from_slice(&(at as u64).to_le_bytes().map(|b| b'a' + (b % 26)));
                        let lp = LoggedPost {
                            feed: 0,
                            ts: m.ts,
                            metric: 0,
                            flags: 0,
                            depth: 0,
                            sid: &sid,
                            akey: who,
                            aname: who,
                            ahandle: svc.name().as_bytes(),
                            text: m.text,
                            extra: b"",
                            parent: b"",
                        };
                        if emit_logged(sh, tab, &lp, F_LOCAL_HIT, false) {
                            hits += 1;
                        }
                    }
                }
            }
        }
    }
    sh.status(Some(tab), Level::Info, &format!("{hits} matches in local history"));
    done(sh, tab, false);
}

// ---------------------------------------------------------------- time and numbers

pub fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = (m as i64 + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d as i64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + doe - 719468
}

pub fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// "2026-11-24T10:20:30.000Z" to unix seconds. Zero for anything unparseable.
pub fn parse_iso(s: &[u8]) -> u32 {
    fn n(s: &[u8], a: usize, b: usize) -> Option<u32> {
        std::str::from_utf8(s.get(a..b)?).ok()?.parse().ok()
    }
    let f = || -> Option<u32> {
        let (y, mo, d, h, mi, se) = (n(s, 0, 4)?, n(s, 5, 7)?, n(s, 8, 10)?, n(s, 11, 13)?, n(s, 14, 16)?, n(s, 17, 19)?);
        let days = days_from_civil(y as i64, mo, d);
        Some((days * 86400 + (h * 3600 + mi * 60 + se) as i64) as u32)
    };
    f().unwrap_or(0)
}

pub fn parse_u32(s: &[u8]) -> u32 {
    let mut v: u64 = 0;
    for &c in s {
        if !c.is_ascii_digit() {
            break;
        }
        v = (v * 10 + (c - b'0') as u64).min(u32::MAX as u64);
    }
    v as u32
}

pub fn fmt_dur(secs: u64) -> String {
    if secs < 60 {
        format!("{secs} s")
    } else if secs < 3600 {
        format!("{} min", secs / 60)
    } else {
        format!("{} h {} min", secs / 3600, secs % 3600 / 60)
    }
}

/// A local id for a new chat.
pub fn new_chat_id(sh: &mut Shared) -> u32 {
    sh.ad.chat_seq = sh.ad.chat_seq.wrapping_add(1);
    let t = sys::now_unix() as u32;
    (t.rotate_left(7) ^ sh.ad.chat_seq.wrapping_mul(0x9E37_79B9)) | 2
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn iso_time() {
        assert_eq!(parse_iso(b"1970-01-01T00:00:00Z"), 0);
        assert_eq!(parse_iso(b"2026-11-24T10:20:30.000Z"), 1795515630);
        assert_eq!(civil_from_days(days_from_civil(2024, 2, 29)), (2024, 2, 29));
        assert_eq!(parse_iso(b"garbage"), 0);
    }

    #[test]
    fn entities_and_ids() {
        assert_eq!(decode_entity(b"amp"), Some('&'));
        assert_eq!(decode_entity(b"#39"), Some('\''));
        assert_eq!(decode_entity(b"#x1F600"), Some('😀'));
        assert!(id_newer("1800000000000000001", "999"));
        assert!(!id_newer("17", "18"));
    }
}
