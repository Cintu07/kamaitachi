//! Claude (messages api) and chatgpt (responses api) share one chat view, history format
//! and export. Answers stream as server-sent events; each event's json goes to the scanner
//! while the next bytes are in flight, and text lands in the tab's arena as it arrives.
//! Switching provider mid-chat starts a new branch of the same conversation.

use std::sync::OnceLock;

use super::*;
use crate::json::{Paths, Scanner};
use crate::net::sse::{Sse, SseSink};
use crate::net::Host;

const C_TYPE: u16 = 1;
const C_TEXT: u16 = 2;
const C_STOP: u16 = 3;
const C_MODEL: u16 = 4;
const C_IN: u16 = 5;
const C_CACHE: u16 = 6;
const C_OUT: u16 = 7;
const C_ERR: u16 = 8;

pub const ANTHROPIC_VERSION: &str = "2026-10-21";
pub const FALLBACK_BETA: &str = "server-side-fallback-2026-07-01";

crate::json_paths!(CLAUDE {
    "type" => C_TYPE, Field;
    "delta.text" => C_TEXT, Field;
    "delta.stop_reason" => C_STOP, Field;
    "message.model" => C_MODEL, Field;
    "message.usage.input_tokens" => C_IN, Field;
    "message.usage.cache_read_input_tokens" => C_CACHE, Field;
    "usage.input_tokens" => C_IN, Field;
    "usage.output_tokens" => C_OUT, Field;
    "error.message" => C_ERR, Field;
});

crate::json_paths!(OPENAI {
    "type" => C_TYPE, Field;
    "delta" => C_TEXT, Field;
    "response.model" => C_MODEL, Field;
    "response.usage.input_tokens" => C_IN, Field;
    "response.usage.input_tokens_details.cached_tokens" => C_CACHE, Field;
    "response.usage.output_tokens" => C_OUT, Field;
    "response.incomplete_details.reason" => C_STOP, Field;
    "response.error.message" => C_ERR, Field;
    "message" => C_ERR, Field;
});

fn paths(s: Service) -> &'static Paths {
    static P: OnceLock<[Paths; 2]> = OnceLock::new();
    let p = P.get_or_init(|| [Paths::compile(CLAUDE), Paths::compile(OPENAI)]);
    if s == Service::Claude { &p[0] } else { &p[1] }
}

/// Models that accept the server-side refusal fallback.
const FALLBACK_MODELS: [&str; 5] = ["claude-fable-5-2", "claude-fable-5-1", "claude-opus-5-5", "claude-opus-5", "claude-sonnet-5-5"];

fn takes_effort(model: &str) -> bool {
    ["claude-opus-5", "claude-sonnet-5", "claude-fable", "claude-mythos", "claude-haiku-5", "claude-opus-4-6", "claude-opus-4-7", "claude-opus-4-8", "claude-sonnet-4-6"]
        .iter()
        .any(|p| model.starts_with(p))
}

pub struct ChatState {
    pub chat: u32,
    pub service: Service,
    pub model: String,
    pub system: String,
    pub msgs: Vec<(Role, Span)>,
    pub busy: bool,
}

pub struct ChatJob {
    pub tab: TabId,
    pub service: Service,
    scanner: Scanner<'static>,
    sse: Sse,
    st: Stream,
}

struct Stream {
    service: Service,
    field: u16,
    small: [u8; 160],
    small_len: usize,
    ev: [u8; 48],
    ev_len: usize,
    writing: bool,
    text_start: u32,
    grew: bool,
    in_tok: u64,
    cache_tok: u64,
    out_tok: u64,
    model: String,
    stop: String,
    err: String,
    overflow: bool,
}

struct JsonCtx<'a> {
    st: &'a mut Stream,
    arena: &'a TextArena,
}

impl Sink for JsonCtx<'_> {
    fn field_start(&mut self, f: u16, _s: bool) {
        self.st.field = f;
        self.st.small_len = 0;
        self.st.writing = f == C_TEXT
            && match self.st.service {
                Service::Claude => true,
                _ => &self.st.ev[..self.st.ev_len] == b"response.output_text.delta",
            };
    }
    fn bytes(&mut self, b: &[u8]) {
        if self.st.writing {
            if self.arena.push(b) {
                self.st.grew = true;
            } else {
                self.st.overflow = true;
            }
        } else {
            let n = b.len().min(self.st.small.len() - self.st.small_len);
            let l = self.st.small_len;
            self.st.small[l..l + n].copy_from_slice(&b[..n]);
            self.st.small_len += n;
        }
    }
    fn field_end(&mut self, f: u16) {
        let v = &self.st.small[..self.st.small_len];
        let num = parse_u32(v) as u64;
        match f {
            C_TYPE => {
                let n = v.len().min(48);
                self.st.ev[..n].copy_from_slice(&v[..n]);
                self.st.ev_len = n;
            }
            C_IN => self.st.in_tok = self.st.in_tok.max(num),
            C_CACHE => self.st.cache_tok = self.st.cache_tok.max(num),
            C_OUT => self.st.out_tok = self.st.out_tok.max(num),
            C_MODEL => self.st.model = String::from_utf8_lossy(v).into_owned(),
            C_STOP => self.st.stop = String::from_utf8_lossy(v).into_owned(),
            C_ERR => {
                if v.is_empty() {
                    return;
                }
                if !self.st.err.is_empty() {
                    self.st.err.push_str("; ");
                }
                self.st.err.push_str(&String::from_utf8_lossy(v));
            }
            _ => {}
        }
        self.st.writing = false;
    }
}

struct SseCtx<'a> {
    scanner: &'a mut Scanner<'static>,
    st: &'a mut Stream,
    arena: &'a TextArena,
}

impl SseSink for SseCtx<'_> {
    fn begin(&mut self) {
        self.scanner.reset();
        self.st.ev_len = 0;
    }
    fn data(&mut self, b: &[u8]) {
        self.scanner.feed(b, &mut JsonCtx { st: self.st, arena: self.arena });
    }
    fn dispatch(&mut self) {
        self.scanner.finish(&mut JsonCtx { st: self.st, arena: self.arena });
    }
}

pub fn on_body(sh: &mut Shared, job: &mut ChatJob, body: &[u8]) {
    let Some(arena) = sh.arena(job.tab) else { return };
    job.sse.feed(body, &mut SseCtx { scanner: &mut job.scanner, st: &mut job.st, arena: &arena });
    if job.st.grew {
        job.st.grew = false;
        // one reply per network read, not per token: the ui redraws one row
        sh.out.push(Reply::Grow { tab: job.tab, len: arena.writer_len() - job.st.text_start });
    }
}

// ---------------------------------------------------------------- log

pub struct LoggedMsg<'a> {
    pub chat: u32,
    pub role: u8,
    pub service: u8,
    pub ts: u32,
    pub cost: u32,
    pub model: &'a [u8],
    pub text: &'a [u8],
}

pub fn read_msg(p: &[u8]) -> Option<LoggedMsg<'_>> {
    if p.first() != Some(&REC_MSG) {
        return None;
    }
    let mut r = RecReader::new(&p[1..]);
    Some(LoggedMsg { chat: r.u32()?, role: r.u8()?, service: r.u8()?, ts: r.u32()?, cost: r.u32()?, model: r.str()?, text: r.str()? })
}

fn log_msg(sh: &mut Shared, service: Service, chat: u32, role: Role, model: &[u8], text: &[u8], cost: u32) {
    sh.rec.start(REC_MSG).u32(chat).u8(role as u8).u8(service as u8).u32(sys::now_unix() as u32).u32(cost).str(model).str(text);
    if let Some(l) = sh.logs.for_service(service) {
        let _ = l.append(&sh.rec.b);
    }
}

/// Every message of a chat, following branches back to where they split.
fn load_chat(sh: &mut Shared, chat: u32, depth: u32) -> Vec<(u8, u8, Vec<u8>, Vec<u8>, u32)> {
    let mut out = Vec::new();
    if depth > 16 {
        return out;
    }
    // the branch point, if this chat is a branch
    let mut parent = None;
    if let Some(log) = sh.logs.meta() {
        if let Ok(Some(map)) = log.map() {
            for (_, p) in records(map.bytes()) {
                if p[0] == REC_BRANCH {
                    let mut r = RecReader::new(&p[1..]);
                    if let (Some(c), Some(par), Some(upto)) = (r.u32(), r.u32(), r.u32()) {
                        if c == chat {
                            parent = Some((par, upto));
                        }
                    }
                }
            }
        }
    }
    if let Some((par, upto)) = parent {
        let mut prev = load_chat(sh, par, depth + 1);
        prev.truncate(upto as usize);
        out.extend(prev);
    }
    let mut own = Vec::new();
    for svc in [Service::Claude, Service::ChatGpt] {
        let Some(log) = sh.logs.for_service(svc) else { continue };
        let Ok(Some(map)) = log.map() else { continue };
        for (_, p) in records(map.bytes()) {
            if let Some(m) = read_msg(p) {
                if m.chat == chat {
                    own.push((m.role, m.service, m.model.to_vec(), m.text.to_vec(), m.ts));
                }
            }
        }
    }
    own.sort_by_key(|m| m.4);
    out.extend(own);
    out
}

fn note(sh: &mut Shared, tab: TabId, text: &str) {
    let Some(arena) = sh.arena(tab) else { return };
    let span = arena.append(text.as_bytes()).unwrap_or_default();
    sh.out.push(Reply::Msg { tab, msg: Msg { role: Role::Note, provider: 0, text: span, model: Span::EMPTY, cost_micros: 0 } });
}

// ---------------------------------------------------------------- commands

pub fn open(sh: &mut Shared, tab: TabId) {
    let Some(Source::Chat { service, chat }) = sh.tabs.get(&tab).map(|t| t.src.clone()) else { return };
    let last_key = format!("chat.last.{}", service.name());
    let id = match chat {
        0 => sh.ad.meta.get(&last_key).and_then(|v| v.parse().ok()).unwrap_or_else(|| new_chat_id(sh)),
        1 => new_chat_id(sh),
        c => c,
    };
    let model = if service == Service::Claude { sh.cfg.claude_model.clone() } else { sh.cfg.openai_model.clone() };
    let system = sh.cfg.system("default").unwrap_or("").to_string();
    let arena = sh.arena(tab).unwrap();
    let mut msgs = Vec::new();
    for (role, svc, model, text, _) in load_chat(sh, id, 0) {
        let r = match role {
            0 => Role::User,
            1 => Role::Assistant,
            _ => Role::Note,
        };
        let m = arena.append(&model).unwrap_or_default();
        let t = arena.append(&text).unwrap_or_default();
        msgs.push((r, t));
        sh.out.push(Reply::Msg { tab, msg: Msg { role: r, provider: svc, text: t, model: m, cost_micros: 0 } });
    }
    let t = sh.tabs.get_mut(&tab).unwrap();
    t.src = Source::Chat { service, chat: id };
    t.chat = Some(ChatState { chat: id, service, model, system, msgs, busy: false });
    sh.out.push(Reply::ChatId { tab, service, chat: id });
    super::done(sh, tab, false);
}

fn key_for(sh: &Shared, s: Service) -> Option<Vec<u8>> {
    let k = if s == Service::Claude { &sh.secrets.anthropic } else { &sh.secrets.openai };
    if k.is_empty() { None } else { Some(k.as_bytes().to_vec()) }
}

pub fn send(sh: &mut Shared, tab: TabId, text: &str, model: &str, system: &str) {
    let Some(arena) = sh.arena(tab) else { return };
    let Some(st) = sh.tabs.get_mut(&tab).and_then(|t| t.chat.as_mut()) else { return };
    if st.busy {
        return sh.status(Some(tab), Level::Warn, "still answering the last message");
    }
    if !model.is_empty() {
        st.model = model.to_string();
    }
    if !system.is_empty() {
        st.system = system.to_string();
    }
    let (service, chat, model) = (st.service, st.chat, st.model.clone());
    let Some(key) = key_for(sh, service) else {
        let name = if service == Service::Claude { "anthropic" } else { "openai" };
        return sh.status(Some(tab), Level::Error, &format!("{}: no api key. run `hayate key set {name}`", service.name()));
    };

    let user = arena.append(text.as_bytes()).unwrap_or_default();
    sh.out.push(Reply::Msg { tab, msg: Msg { role: Role::User, provider: service as u8, text: user, model: Span::EMPTY, cost_micros: 0 } });
    log_msg(sh, service, chat, Role::User, b"", text.as_bytes(), 0);
    set_meta(sh, &format!("chat.last.{}", service.name()), &chat.to_string());

    let st = sh.tabs.get_mut(&tab).unwrap().chat.as_mut().unwrap();
    st.msgs.push((Role::User, user));
    let (body, in_bytes) = request_body(&sh.cfg, st, &arena);
    let max = if service == Service::Claude { sh.cfg.claude_max_tokens } else { sh.cfg.openai_max_tokens } as u64;
    let price = sh.cfg.price(&model).cloned();
    let Some(price) = price else {
        return sh.status(Some(tab), Level::Error, &format!("no price for {model} in the config; add `price.{model} = in out`"));
    };
    // tokens at $X per million cost X micro-dollars each
    let est = ((in_bytes as u64 / 3) as f64 * price.input + max as f64 * price.output) as u64;
    if let Err(e) = sh.allow(service, est, 0) {
        note(sh, tab, &format!("not sent: {e}"));
        return sh.status(Some(tab), Level::Warn, &e);
    }

    let m = arena.append(model.as_bytes()).unwrap_or_default();
    let start = arena.writer_len();
    sh.out.push(Reply::Msg { tab, msg: Msg { role: Role::Assistant, provider: service as u8, text: Span { off: start, len: 0 }, model: m, cost_micros: 0 } });

    let mut headers: Vec<(&'static str, Vec<u8>)> = vec![("Content-Type", b"application/json".to_vec()), ("Accept", b"text/event-stream".to_vec())];
    let (host, target) = if service == Service::Claude {
        headers.push(("x-api-key", key));
        headers.push(("anthropic-version", ANTHROPIC_VERSION.as_bytes().to_vec()));
        if FALLBACK_MODELS.contains(&model.as_str()) {
            headers.push(("anthropic-beta", FALLBACK_BETA.as_bytes().to_vec()));
        }
        (Host::Anthropic, b"/v1/messages".to_vec())
    } else {
        let mut b = b"Bearer ".to_vec();
        b.extend_from_slice(&key);
        headers.push(("Authorization", b));
        (Host::OpenAi, b"/v1/responses".to_vec())
    };
    let job = ChatJob {
        tab,
        service,
        scanner: Scanner::new(paths(service)),
        sse: Sse::new(),
        st: Stream {
            service,
            field: 0,
            small: [0; 160],
            small_len: 0,
            ev: [0; 48],
            ev_len: 0,
            writing: false,
            text_start: start,
            grew: false,
            in_tok: 0,
            cache_tok: 0,
            out_tok: 0,
            model: model.clone(),
            stop: String::new(),
            err: String::new(),
            overflow: false,
        },
    };
    sh.tabs.get_mut(&tab).unwrap().chat.as_mut().unwrap().busy = true;
    sh.followups.push(Req {
        tab: Some(tab),
        service,
        host,
        method: "POST",
        target,
        headers,
        body,
        what: format!("{model} message"),
        job: Job::Chat(Box::new(job)),
        est_micros: est,
        units: 0,
        idle_timeout: 300,
    });
}

/// The request body with the whole conversation. Returns it and the bytes of history.
fn request_body(cfg: &crate::config::Config, st: &ChatState, arena: &TextArena) -> (Vec<u8>, usize) {
    let mut b = Vec::with_capacity(4096);
    let mut hist = 0usize;
    let model = st.model.as_str();
    b.extend_from_slice(b"{\"model\":");
    crate::json::push_str(&mut b, model);
    b.extend_from_slice(b",\"stream\":true");
    if st.service == Service::Claude {
        b.extend_from_slice(format!(",\"max_tokens\":{}", cfg.claude_max_tokens).as_bytes());
        if takes_effort(model) {
            b.extend_from_slice(b",\"output_config\":{\"effort\":");
            crate::json::push_str(&mut b, &cfg.claude_effort);
            b.push(b'}');
        }
        if FALLBACK_MODELS.contains(&model) {
            b.extend_from_slice(b",\"fallbacks\":\"default\"");
        }
        if !st.system.is_empty() {
            b.extend_from_slice(b",\"system\":");
            crate::json::push_str(&mut b, &st.system);
        }
        b.extend_from_slice(b",\"messages\":[");
    } else {
        b.extend_from_slice(format!(",\"store\":false,\"max_output_tokens\":{}", cfg.openai_max_tokens).as_bytes());
        if !st.system.is_empty() {
            b.extend_from_slice(b",\"instructions\":");
            crate::json::push_str(&mut b, &st.system);
        }
        b.extend_from_slice(b",\"input\":[");
    }
    let mut first = true;
    for (role, span) in &st.msgs {
        let text = arena.str_writer(*span);
        if text.is_empty() || *role == Role::Note {
            continue;
        }
        if !first {
            b.push(b',');
        }
        first = false;
        let r = if *role == Role::User { "user" } else { "assistant" };
        b.extend_from_slice(b"{\"role\":\"");
        b.extend_from_slice(r.as_bytes());
        b.extend_from_slice(b"\",\"content\":");
        crate::json::push_str(&mut b, text);
        b.push(b'}');
        hist += text.len();
    }
    b.extend_from_slice(b"]}");
    (b, hist + st.system.len())
}

pub fn done(sh: &mut Shared, job: ChatJob) -> u32 {
    let tab = job.tab;
    let s = job.st;
    let Some(arena) = sh.arena(tab) else { return 0 };
    let span = Span { off: s.text_start, len: arena.writer_len() - s.text_start };
    let req_model = sh.tabs.get(&tab).and_then(|t| t.chat.as_ref()).map(|c| c.model.clone()).unwrap_or_default();
    let model = if s.model.is_empty() { req_model } else { s.model.clone() };
    let price = sh.cfg.price(&model).cloned().or_else(|| sh.cfg.price(&s.model).cloned());
    let micros = match price {
        Some(p) => {
            let (fresh_in, cached) = if s.service == Service::Claude {
                (s.in_tok, s.cache_tok)
            } else {
                (s.in_tok.saturating_sub(s.cache_tok), s.cache_tok)
            };
            (fresh_in as f64 * p.input + cached as f64 * p.input * 0.1 + s.out_tok as f64 * p.output) as u64
        }
        None => 0,
    };
    let service = s.service;
    let text = arena.peek(span).to_vec();
    if let Some(st) = sh.tabs.get_mut(&tab).and_then(|t| t.chat.as_mut()) {
        st.busy = false;
        st.msgs.push((Role::Assistant, span));
        let chat = st.chat;
        log_msg(sh, service, chat, Role::Assistant, model.as_bytes(), &text, micros as u32);
    }
    sh.charge(service, micros, 0);
    sh.out.push(Reply::Grow { tab, len: span.len });
    if !s.err.is_empty() {
        sh.status(Some(tab), Level::Error, &format!("{}: {}", service.name(), s.err));
    } else if s.stop == "refusal" {
        sh.status(Some(tab), Level::Warn, "the model declined this request (refusal)");
    } else if s.stop == "max_tokens" || s.stop == "max_output_tokens" {
        sh.status(Some(tab), Level::Warn, "answer cut off at max tokens (raise *.max_tokens in the config)");
    } else if s.overflow {
        sh.status(Some(tab), Level::Warn, "this chat hit its 1 mb tab budget; start a new chat with n");
    } else {
        sh.status(
            Some(tab),
            Level::Info,
            &format!("{model} · {} in · {} out · ${:.4}", s.in_tok + s.cache_tok, s.out_tok, micros as f64 / 1e6),
        );
    }
    super::done(sh, tab, false);
    micros as u32
}

pub fn failed(sh: &mut Shared, tab: TabId) {
    if let Some(st) = sh.tabs.get_mut(&tab).and_then(|t| t.chat.as_mut()) {
        st.busy = false;
    }
}

pub fn branch(sh: &mut Shared, tab: TabId, service: Service, model: &str) {
    let Some(st) = sh.tabs.get_mut(&tab).and_then(|t| t.chat.as_mut()) else { return };
    if st.busy {
        return sh.status(Some(tab), Level::Warn, "wait for the answer to finish first");
    }
    let model = if model.is_empty() {
        if service == Service::Claude { sh.cfg.claude_model.clone() } else { sh.cfg.openai_model.clone() }
    } else {
        model.to_string()
    };
    let st = sh.tabs.get_mut(&tab).unwrap().chat.as_mut().unwrap();
    if service != st.service {
        let (old, upto) = (st.chat, st.msgs.len() as u32);
        let new = new_chat_id(sh);
        sh.rec.start(REC_BRANCH).u32(new).u32(old).u32(upto);
        if let Some(l) = sh.logs.meta() {
            let _ = l.append(&sh.rec.b);
        }
        let t = sh.tabs.get_mut(&tab).unwrap();
        t.src = Source::Chat { service, chat: new };
        let st = t.chat.as_mut().unwrap();
        st.chat = new;
        st.service = service;
        st.model = model.clone();
        set_meta(sh, &format!("chat.last.{}", service.name()), &new.to_string());
        sh.out.push(Reply::ChatId { tab, service, chat: new });
        note(sh, tab, &format!("now {} · {model} · a new branch with the history so far", service.name()));
    } else {
        st.model = model.clone();
        note(sh, tab, &format!("model {model}"));
    }
}
