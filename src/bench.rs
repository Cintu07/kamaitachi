//! The budget tests. Every number in the prd's hard-numbers table is checked here:
//! `hayate bench` starts a local replay server (a separate process, so its memory is not
//! counted), drives a scripted session through the real network stack over a local
//! socket, and fails if any limit is crossed.
//!
//! The replay server answers with synthetic responses shaped like each api's real ones.
//! Set HAYATE_FIXTURES to a folder of responses recorded with HAYATE_RECORD to replay
//! real traffic instead.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::process::{Command as Proc, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::arena::{self, ring, ArenaId, Consumer, MB};
use crate::config::Config;
use crate::json::{Paths, Scanner, Sink};
use crate::msg::{Command, Reply, Service, Source};
use crate::net::{self, NetInit};
use crate::store::TextArena;
use crate::sys::{self, Key, WinEvent};
use crate::ui::{App, Store};

// ---------------------------------------------------------------- synthetic responses

struct Rng(u64);

impl Rng {
    fn new(seed: u64) -> Rng {
        Rng(seed ^ 0x9E37_79B9_7F4A_7C15 | 1)
    }
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn below(&mut self, n: u64) -> u64 {
        self.next() % n.max(1)
    }
}

const WORDS: &[&str] = &[
    "rust", "terminal", "memory", "arena", "cache", "latency", "the", "a", "of", "to", "build", "ship", "fast", "wind", "疾風", "テスト",
    "తెలుగు", "క్షమించండి", "😀", "👨‍👩‍👧", "🇮🇳", "naïve", "café", "—", "\"quoted\"", "line\nbreak", "https://example.com/a?b=1", "&amp;",
    "❤️", "bytes", "pixels", "json", "socket", "fps", "under", "budget", "one", "write", "per", "frame", "日本語のテキスト", "😂😂",
];

fn words(r: &mut Rng, n: usize) -> String {
    let mut s = String::new();
    for i in 0..n {
        if i > 0 {
            s.push(' ');
        }
        s.push_str(WORDS[r.below(WORDS.len() as u64) as usize]);
    }
    s
}

fn js(s: &str) -> String {
    let mut v = Vec::new();
    crate::json::push_str(&mut v, s);
    String::from_utf8(v).unwrap()
}

fn iso(t: u64) -> String {
    let (y, m, d) = crate::adapters::civil_from_days((t / 86400) as i64);
    let s = t % 86400;
    format!("{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}.000Z", s / 3600, s % 3600 / 60, s % 60)
}

fn query<'a>(target: &'a str, key: &str) -> Option<&'a str> {
    let q = target.split_once('?')?.1;
    q.split('&').find_map(|kv| kv.strip_prefix(key).and_then(|v| v.strip_prefix('=')))
}

const X_BASE: u64 = 1_990_000_000_000_000_000;
const X_STREAM: u64 = 20_000;

fn x_post(r: &mut Rng, id: u64, now: u64, reply_to: Option<u64>) -> (String, u64) {
    let author = r.below(40);
    let n = 6 + r.below(40) as usize;
    let mut p = format!(
        "{{\"edit_history_tweet_ids\":[\"{id}\"],\"id\":\"{id}\",\"text\":{},\"author_id\":\"{}\",\"created_at\":\"{}\",\"conversation_id\":\"{id}\",\"public_metrics\":{{\"retweet_count\":{},\"reply_count\":2,\"like_count\":{},\"quote_count\":0,\"bookmark_count\":1,\"impression_count\":{}}}",
        js(&words(r, n)),
        1000 + author,
        iso(now - (X_BASE + X_STREAM * 1000 - id) / 1000 * 97),
        r.below(50),
        r.below(5000),
        r.below(90000)
    );
    if let Some(parent) = reply_to {
        p.push_str(&format!(",\"referenced_tweets\":[{{\"type\":\"replied_to\",\"id\":\"{parent}\"}}]"));
    }
    if r.below(5) == 0 {
        p.push_str(",\"attachments\":{\"media_keys\":[\"3_1\",\"3_2\"]}");
    }
    p.push('}');
    (p, 1000 + author)
}

fn x_users(authors: &[u64]) -> String {
    let mut s = String::from("\"includes\":{\"users\":[");
    let mut seen: Vec<u64> = authors.to_vec();
    seen.sort();
    seen.dedup();
    for (i, a) in seen.iter().enumerate() {
        if i > 0 {
            s.push(',');
        }
        let mut r = Rng::new(*a);
        s.push_str(&format!("{{\"id\":\"{a}\",\"name\":{},\"username\":\"user{a}\"}}", js(&words(&mut r, 2))));
    }
    s.push_str("]}");
    s
}

fn x_timeline(target: &str, now: u64) -> String {
    let max = query(target, "max_results").and_then(|v| v.parse().ok()).unwrap_or(50u64);
    let newest = X_BASE + X_STREAM * 1000;
    let until = query(target, "until_id").and_then(|v| v.parse::<u64>().ok());
    let token = query(target, "pagination_token").and_then(|v| v.get(18..)).and_then(|v| v.parse::<u64>().ok());
    let start = if let Some(u) = until.or(token) {
        u - 1000
    } else if query(target, "since_id").is_some() {
        // nothing new since the last look
        return "{\"meta\":{\"result_count\":0}}".into();
    } else {
        newest
    };
    let mut posts = Vec::new();
    let mut authors = Vec::new();
    for k in 0..max {
        let id = start - k * 1000;
        if id <= X_BASE {
            break;
        }
        let mut r = Rng::new(id);
        let (p, a) = x_post(&mut r, id, now, None);
        posts.push(p);
        authors.push(a);
    }
    let oldest = start - (posts.len() as u64 - 1) * 1000;
    format!(
        "{{\"data\":[{}],{},\"meta\":{{\"result_count\":{},\"newest_id\":\"{start}\",\"oldest_id\":\"{oldest}\",\"next_token\":\"7140dibdnow9c7btw4{oldest}\"}}}}",
        posts.join(","),
        x_users(&authors),
        posts.len()
    )
}

fn x_thread(root: u64, now: u64) -> String {
    let mut posts = Vec::new();
    let mut authors = Vec::new();
    let mut ids = vec![root];
    for k in 1..=30u64 {
        let id = root + k;
        let mut r = Rng::new(id);
        let parent = ids[r.below(ids.len() as u64) as usize];
        let (p, a) = x_post(&mut r, id, now, Some(parent));
        posts.push(p);
        authors.push(a);
        ids.push(id);
    }
    format!("{{\"data\":[{}],{},\"meta\":{{\"result_count\":30}}}}", posts.join(","), x_users(&authors))
}

fn yt_videos(r: &mut Rng, n: u64, now: u64, channel: &str, playlist: bool) -> String {
    let mut items = Vec::new();
    for k in 0..n {
        let vid: String = (0..11).map(|_| b"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789-_"[r.below(64) as usize] as char).collect();
        let title = js(&format!("{} &#39;{}&#39; &amp; more", words(r, 5), words(r, 2)));
        let desc = js(&words(r, 120));
        let when = iso(now - r.below(86400 * 14));
        let thumbs = "{\"default\":{\"url\":\"https://i.ytimg.com/vi/x/default.jpg\",\"width\":120,\"height\":90},\"high\":{\"url\":\"https://i.ytimg.com/vi/x/hq.jpg\",\"width\":480,\"height\":360}}";
        if playlist {
            items.push(format!(
                "{{\"kind\":\"youtube#playlistItem\",\"etag\":\"e{k}\",\"id\":\"pi{k}\",\"snippet\":{{\"publishedAt\":\"{when}\",\"channelId\":\"{channel}\",\"title\":{title},\"description\":{desc},\"thumbnails\":{thumbs},\"channelTitle\":\"Channel {channel}\",\"playlistId\":\"UU{}\",\"position\":{k},\"resourceId\":{{\"kind\":\"youtube#video\",\"videoId\":\"{vid}\"}},\"videoOwnerChannelTitle\":\"Channel {}\",\"videoOwnerChannelId\":\"{channel}\"}}}}",
                &channel[2..],
                &channel[2..8]
            ));
        } else {
            items.push(format!(
                "{{\"kind\":\"youtube#searchResult\",\"etag\":\"e{k}\",\"id\":{{\"kind\":\"youtube#video\",\"videoId\":\"{vid}\"}},\"snippet\":{{\"publishedAt\":\"{when}\",\"channelId\":\"{channel}\",\"title\":{title},\"description\":{desc},\"thumbnails\":{thumbs},\"channelTitle\":\"Channel {}\"}}}}",
                &channel[2..8]
            ));
        }
    }
    format!("{{\"kind\":\"youtube#listResponse\",\"pageInfo\":{{\"totalResults\":{n}}},\"items\":[{}]}}", items.join(","))
}

fn yt_comments(r: &mut Rng, now: u64, page2: bool) -> String {
    let mut items = Vec::new();
    for k in 0..50 {
        let c = |r: &mut Rng, id: String| {
            let n = 4 + r.below(30) as usize;
            format!(
                "{{\"kind\":\"youtube#comment\",\"id\":\"{id}\",\"snippet\":{{\"authorDisplayName\":\"@viewer{}\",\"authorChannelId\":{{\"value\":\"UCviewer{}\"}},\"textDisplay\":{},\"likeCount\":{},\"publishedAt\":\"{}\"}}}}",
                r.below(99),
                r.below(99),
                js(&words(r, n)),
                r.below(900),
                iso(now - r.below(86400 * 3))
            )
        };
        let top = c(r, format!("c{}{k}", page2 as u8));
        let nrep = r.below(4);
        let replies: Vec<String> = (0..nrep).map(|j| c(r, format!("c{}{k}.r{j}", page2 as u8))).collect();
        let mut item = format!("{{\"kind\":\"youtube#commentThread\",\"id\":\"t{k}\",\"snippet\":{{\"videoId\":\"v\",\"topLevelComment\":{top},\"totalReplyCount\":{nrep}}}");
        if nrep > 0 {
            item.push_str(&format!(",\"replies\":{{\"comments\":[{}]}}", replies.join(",")));
        }
        item.push('}');
        items.push(item);
    }
    let next = if page2 { String::new() } else { ",\"nextPageToken\":\"p2\"".into() };
    format!("{{\"items\":[{}]{next}}}", items.join(","))
}

fn sse_claude(r: &mut Rng, tokens: usize) -> Vec<String> {
    let mut ev = vec![
        "event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"id\":\"msg_1\",\"type\":\"message\",\"role\":\"assistant\",\"model\":\"claude-opus-5-5\",\"content\":[],\"stop_reason\":null,\"usage\":{\"input_tokens\":1200,\"cache_read_input_tokens\":0,\"output_tokens\":1}}}\n\n".to_string(),
        "event: content_block_start\ndata: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"thinking\",\"thinking\":\"\"}}\n\n".into(),
        "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"thinking_delta\",\"thinking\":\"\"}}\n\n".into(),
        "event: content_block_stop\ndata: {\"type\":\"content_block_stop\",\"index\":0}\n\n".into(),
        "event: ping\ndata: {\"type\": \"ping\"}\n\n".into(),
        "event: content_block_start\ndata: {\"type\":\"content_block_start\",\"index\":1,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n\n".into(),
    ];
    for i in 0..tokens {
        let w = if i % 40 == 39 { "\n\n".to_string() } else { format!("{} ", WORDS[r.below(WORDS.len() as u64) as usize]) };
        ev.push(format!("event: content_block_delta\ndata: {{\"type\":\"content_block_delta\",\"index\":1,\"delta\":{{\"type\":\"text_delta\",\"text\":{}}}}}\n\n", js(&w)));
    }
    ev.push("event: content_block_stop\ndata: {\"type\":\"content_block_stop\",\"index\":1}\n\n".into());
    ev.push(format!("event: message_delta\ndata: {{\"type\":\"message_delta\",\"delta\":{{\"stop_reason\":\"end_turn\",\"stop_sequence\":null}},\"usage\":{{\"output_tokens\":{tokens}}}}}\n\n"));
    ev.push("event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n".into());
    ev
}

fn sse_openai(r: &mut Rng, tokens: usize) -> Vec<String> {
    let mut ev = vec![
        "event: response.created\ndata: {\"type\":\"response.created\",\"sequence_number\":0,\"response\":{\"id\":\"resp_1\",\"object\":\"response\",\"status\":\"in_progress\",\"model\":\"gpt-5\",\"output\":[]}}\n\n".to_string(),
        "event: response.reasoning_summary_text.delta\ndata: {\"type\":\"response.reasoning_summary_text.delta\",\"delta\":\"not shown\"}\n\n".into(),
    ];
    for _ in 0..tokens {
        let w = format!("{} ", WORDS[r.below(WORDS.len() as u64) as usize]);
        ev.push(format!("event: response.output_text.delta\ndata: {{\"type\":\"response.output_text.delta\",\"item_id\":\"msg_1\",\"output_index\":1,\"content_index\":0,\"delta\":{}}}\n\n", js(&w)));
    }
    ev.push(format!("event: response.completed\ndata: {{\"type\":\"response.completed\",\"response\":{{\"id\":\"resp_1\",\"status\":\"completed\",\"model\":\"gpt-5\",\"usage\":{{\"input_tokens\":900,\"input_tokens_details\":{{\"cached_tokens\":0}},\"output_tokens\":{tokens},\"total_tokens\":{}}}}}}}\n\n", 900 + tokens));
    ev
}

enum Body {
    Whole(String),
    Events(Vec<String>),
}

fn route(method: &str, target: &str, now: u64, tokens: usize) -> (u16, Body) {
    let path = target.split('?').next().unwrap_or("");
    let mut r = Rng::new(crate::store::key_hash(target) as u64);
    let ok = |s: String| (200, Body::Whole(s));
    match (method, path) {
        ("GET", "/2/users/me") => ok("{\"data\":{\"id\":\"42\",\"name\":\"bench\",\"username\":\"bench\"}}".into()),
        ("GET", p) if p.starts_with("/2/users/42/timelines") || p.starts_with("/2/users/42/mentions") => ok(x_timeline(target, now)),
        ("GET", "/2/tweets/search/recent") => {
            let q = query(target, "query").unwrap_or("");
            match q.strip_prefix("conversation_id%3A").and_then(|v| v.parse::<u64>().ok()) {
                Some(root) => ok(x_thread(root, now)),
                None => ok(x_timeline("/x?max_results=25", now)),
            }
        }
        ("GET", "/2/tweets") => {
            let id: u64 = query(target, "ids").and_then(|v| v.parse().ok()).unwrap_or(X_BASE + 1000);
            let (p, a) = x_post(&mut Rng::new(id), id, now, None);
            ok(format!("{{\"data\":[{p}],{}}}", x_users(&[a])))
        }
        ("POST", "/2/tweets") => ok("{\"data\":{\"id\":\"1999999999999999999\",\"text\":\"ok\"}}".into()),
        ("POST", p) if p.ends_with("/likes") => ok("{\"data\":{\"liked\":true}}".into()),
        ("GET", "/youtube/v3/subscriptions") => {
            let items: Vec<String> = (0..30).map(|k| format!("{{\"snippet\":{{\"title\":\"ch{k}\",\"resourceId\":{{\"kind\":\"youtube#channel\",\"channelId\":\"UCbenchchannel{k:08}\"}}}}}}")).collect();
            ok(format!("{{\"items\":[{}]}}", items.join(",")))
        }
        ("GET", "/youtube/v3/playlistItems") => {
            let pl = query(target, "playlistId").unwrap_or("UUx");
            let n = query(target, "maxResults").and_then(|v| v.parse().ok()).unwrap_or(5);
            ok(yt_videos(&mut r, n, now, &format!("UC{}", &pl[2..]), true))
        }
        ("GET", "/youtube/v3/search") => ok(yt_videos(&mut r, 25, now, "UCsearchresult0000000000", false)),
        ("GET", "/youtube/v3/videos") => {
            let id = query(target, "id").unwrap_or("x");
            ok(format!(
                "{{\"items\":[{{\"id\":\"{id}\",\"snippet\":{{\"publishedAt\":\"{}\",\"channelId\":\"UCbenchchannel00000001\",\"title\":{},\"description\":{},\"channelTitle\":\"Channel bench\"}},\"statistics\":{{\"viewCount\":\"123456\",\"likeCount\":\"999\"}}}}]}}",
                iso(now - 7200),
                js(&words(&mut r, 8)),
                js(&words(&mut r, 200))
            ))
        }
        ("GET", "/youtube/v3/commentThreads") => ok(yt_comments(&mut r, now, query(target, "pageToken").is_some())),
        ("POST", "/v1/messages") => (200, Body::Events(sse_claude(&mut r, tokens))),
        ("POST", "/v1/responses") => (200, Body::Events(sse_openai(&mut r, tokens.min(500)))),
        ("POST", "/token") | ("POST", "/2/oauth2/token") => ok("{\"access_token\":\"bench\",\"expires_in\":3600}".into()),
        _ => (404, Body::Whole("{\"error\":{\"message\":\"no such endpoint in the replay server\"}}".into())),
    }
}

fn fixture(dir: &str, target: &str) -> Option<Vec<u8>> {
    let path = target.split('?').next().unwrap_or("").replace(['/', '\\', ':'], "_");
    let mut found = None;
    for e in std::fs::read_dir(dir).ok()?.flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        if name.ends_with(&format!("{path}.http")) {
            found = std::fs::read(e.path()).ok();
        }
    }
    found
}

fn serve_conn(s: TcpStream, tokens: usize) {
    let mut rd = BufReader::new(match s.try_clone() {
        Ok(c) => c,
        Err(_) => return,
    });
    let mut w = s;
    let fixtures = std::env::var("HAYATE_FIXTURES").ok();
    loop {
        let mut line = String::new();
        if rd.read_line(&mut line).unwrap_or(0) == 0 {
            return;
        }
        let mut parts = line.split_whitespace();
        let (method, target) = (parts.next().unwrap_or("").to_string(), parts.next().unwrap_or("").to_string());
        let mut len = 0usize;
        loop {
            let mut h = String::new();
            if rd.read_line(&mut h).unwrap_or(0) == 0 {
                return;
            }
            let h = h.trim_end();
            if h.is_empty() {
                break;
            }
            if let Some((k, v)) = h.split_once(':') {
                if k.eq_ignore_ascii_case("content-length") {
                    len = v.trim().parse().unwrap_or(0);
                }
            }
        }
        let mut body = vec![0u8; len];
        if rd.read_exact(&mut body).is_err() {
            return;
        }
        if let Some(raw) = fixtures.as_deref().and_then(|d| fixture(d, &target)) {
            if w.write_all(&raw).is_err() {
                return;
            }
            continue;
        }
        let (status, b) = route(&method, &target, sys::now_unix(), tokens);
        let ok = match b {
            Body::Whole(s) => {
                let head = format!("HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n", s.len());
                w.write_all(head.as_bytes()).and_then(|_| w.write_all(s.as_bytes()))
            }
            Body::Events(evs) => {
                let mut r = w.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nTransfer-Encoding: chunked\r\n\r\n");
                let pace = std::env::var("HAYATE_REPLAY_PACE_MS").ok().and_then(|v| v.parse::<u64>().ok()).unwrap_or(0);
                for e in evs {
                    if r.is_err() {
                        break;
                    }
                    if pace > 0 {
                        // demo mode: tokens arrive at the speed a model writes them
                        std::thread::sleep(Duration::from_millis(pace));
                    }
                    r = w.write_all(format!("{:x}\r\n{e}\r\n", e.len()).as_bytes());
                }
                r.and_then(|_| w.write_all(b"0\r\n\r\n"))
            }
        };
        if ok.is_err() {
            return;
        }
    }
}

/// `hayate replay-server`: prints its address, then serves until killed.
pub fn serve_stdout() -> Result<(), String> {
    let l = TcpListener::bind("127.0.0.1:0").map_err(|e| e.to_string())?;
    println!("{}", l.local_addr().map_err(|e| e.to_string())?);
    std::io::stdout().flush().ok();
    let tokens = std::env::var("HAYATE_BENCH_TOKENS").ok().and_then(|v| v.parse().ok()).unwrap_or(4000);
    for s in l.incoming().flatten() {
        let _ = s.set_nodelay(true);
        std::thread::spawn(move || serve_conn(s, tokens));
    }
    Ok(())
}

// ---------------------------------------------------------------- probe

/// `hayate probe <idle secs>`: cold start to first frame, then an idle wait, in a fresh
/// process so the numbers are the client's own.
pub fn probe(args: &[String]) -> Result<(), String> {
    let idle: u64 = args.first().and_then(|v| v.parse().ok()).unwrap_or(10);
    // what windows charges this process before hayate does anything
    let floor = sys::process_memory().working_set;
    let dir = std::env::temp_dir().join(format!("hayate-probe-{}", std::process::id()));
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let cfg = Arc::new(Config::defaults_for_tests(&dir));
    let (cmd_tx, cmd_rx) = ring(1024);
    let (rep_tx, rep_rx) = ring(4096);
    let cmd_ev = Arc::new(WinEvent::new());
    let ui_ev = Arc::new(WinEvent::new());
    let authors = Arc::new(TextArena::with_budget(2 * MB));
    let net = net::spawn(NetInit { cfg: cfg.clone(), cmds: cmd_rx, replies: rep_tx, cmd_event: cmd_ev.clone(), ui_event: ui_ev.clone(), authors: authors.clone() });
    let mut app = App::new(cfg, 200, 60, cmd_tx, cmd_ev, authors);
    let _ = app.frame().len();
    let start = sys::since_process_start_us();
    // idle: the same single blocking wait the real loop uses
    while rep_rx.pop().is_some() {}
    let cpu0 = sys::cpu_time_us();
    let t0 = Instant::now();
    while t0.elapsed().as_secs() < idle {
        let left = (idle * 1000).saturating_sub(t0.elapsed().as_millis() as u64) as u32;
        if sys::wait_any(&[ui_ev.0], left).is_some() {
            while let Some(r) = rep_rx.pop() {
                app.apply(r);
            }
        }
    }
    let cpu = sys::cpu_time_us() - cpu0;
    let m = sys::process_memory();
    println!("startup_us={start} idle_ws={} idle_private={} floor_ws={floor} idle_cpu_us={cpu} idle_s={idle}", m.working_set, m.private_bytes);
    app.send(Command::Shutdown);
    let _ = net.join();
    let _ = std::fs::remove_dir_all(&dir);
    Ok(())
}

// ---------------------------------------------------------------- session

struct Session {
    app: App,
    rep: Consumer<Reply>,
    ui_ev: Arc<WinEvent>,
}

impl Session {
    fn drain(&mut self) -> usize {
        let mut n = 0;
        while let Some(r) = self.rep.pop() {
            self.app.apply(r);
            n += 1;
        }
        n
    }

    /// Runs the ui loop until every tab has finished loading.
    fn settle(&mut self, limit: Duration) -> Result<(), String> {
        let t0 = Instant::now();
        loop {
            sys::wait_any(&[self.ui_ev.0], 50);
            if self.drain() > 0 {
                let _ = self.app.frame().len();
            }
            if self.app.tabs.iter().all(|t| !t.loading) {
                return Ok(());
            }
            if t0.elapsed() > limit {
                let stuck: Vec<String> = self.app.tabs.iter().filter(|t| t.loading).map(|t| t.src.feed_key()).collect();
                return Err(format!("tabs still loading after {:?}: {}", limit, stuck.join(", ")));
            }
        }
    }

    fn tab(&self, f: impl Fn(&Source) -> bool) -> Option<usize> {
        self.app.tabs.iter().position(|t| f(&t.src))
    }

    fn first_sid(&self, i: usize) -> String {
        let t = &self.app.tabs[i];
        match &t.store {
            Store::Feed(f) if !f.is_empty() => t.arena.str(f.sid[0]).to_string(),
            _ => String::new(),
        }
    }
}

fn pct(v: &mut [u32], p: usize) -> u32 {
    if v.is_empty() {
        return 0;
    }
    v.sort_unstable();
    v[(v.len() * p / 100).min(v.len() - 1)]
}

struct Row {
    name: &'static str,
    limit: String,
    got: String,
    pass: Option<bool>,
}

/// `hayate bench [--quick]`
pub fn run(args: &[String]) -> Result<(), String> {
    let quick = args.iter().any(|a| a == "--quick");
    let idle_secs = if quick { 2 } else { 10 };
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    // SAFETY: set before any other thread reads the environment
    unsafe { std::env::set_var("HAYATE_TEST_KEYS", "1") };

    let mut server = Proc::new(&exe).arg("replay-server").stdout(Stdio::piped()).spawn().map_err(|e| format!("replay server: {e}"))?;
    let mut addr = String::new();
    BufReader::new(server.stdout.take().unwrap()).read_line(&mut addr).map_err(|e| e.to_string())?;
    let addr = addr.trim().to_string();
    let probe = Proc::new(&exe).args(["probe", &idle_secs.to_string()]).stdout(Stdio::piped()).spawn().map_err(|e| e.to_string())?;

    let dir = std::env::temp_dir().join(format!("hayate-bench-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let mut cfg = Config::defaults_for_tests(&dir);
    cfg.replay_addr = Some(addr.clone());
    cfg.yt_per_channel = 5;
    cfg.cap_usd = [1000.0, 1000.0, 1000.0];
    cfg.yt_daily_quota = 1_000_000;
    let cfg = Arc::new(cfg);

    let (cmd_tx, cmd_rx) = ring(1024);
    let (rep_tx, rep_rx) = ring(4096);
    let cmd_ev = Arc::new(WinEvent::new());
    let ui_ev = Arc::new(WinEvent::new());
    let authors = Arc::new(TextArena::with_budget(2 * MB));
    let net = net::spawn(NetInit { cfg: cfg.clone(), cmds: cmd_rx, replies: rep_tx, cmd_event: cmd_ev.clone(), ui_event: ui_ev.clone(), authors: authors.clone() });
    let app = App::new(cfg.clone(), 200, 60, cmd_tx, cmd_ev, authors);
    let mut s = Session { app, rep: rep_rx, ui_ev };
    let lim = Duration::from_secs(60);

    // four services, then ten tabs
    eprintln!("bench: opening four services");
    for src in [Source::XHome, Source::XMentions, Source::YtSubs, Source::Chat { service: Service::Claude, chat: 1 }, Source::Chat { service: Service::ChatGpt, chat: 1 }] {
        s.app.open(src);
    }
    s.app.focus(0);
    s.settle(lim)?;
    let home = s.tab(|t| *t == Source::XHome).unwrap();
    let subs = s.tab(|t| *t == Source::YtSubs).unwrap();
    let root = s.first_sid(home);
    let video = s.first_sid(subs);
    s.app.open(Source::XThread { root: root.into() });
    s.app.open(Source::YtComments { video: video.into() });
    s.app.open(Source::XSearch { q: "rust".into() });
    s.app.open(Source::YtSearch { q: "terminal".into() });
    s.settle(lim)?;
    s.app.open(Source::LocalSearch { q: "rust".into() });
    s.settle(lim)?;
    eprintln!("bench: {} tabs open", s.app.tabs.len());

    // one text tab: the home feed after 200 posts
    s.app.focus(home);
    let mut key_us = Vec::with_capacity(4096);
    let mut frame_us = Vec::with_capacity(4096);
    let mut one_tab = 0usize;
    eprintln!("bench: scrolling 2000 rows");
    for _ in 0..2000 {
        loop {
            let t = &s.app.tabs[home];
            let len = match &t.store {
                Store::Feed(f) => f.len(),
                _ => 0,
            };
            if t.cursor + 1 < len || !t.loading {
                break;
            }
            sys::wait_any(&[s.ui_ev.0], 50);
            s.drain();
        }
        s.drain();
        let t0 = Instant::now();
        s.app.key(Key::Char('j'));
        let f0 = Instant::now();
        let _ = s.app.frame().len();
        frame_us.push(f0.elapsed().as_micros() as u32);
        key_us.push(t0.elapsed().as_micros() as u32);
        if one_tab == 0 && matches!(&s.app.tabs[home].store, Store::Feed(f) if f.len() >= 200) {
            one_tab = s.app.tabs[home].arena.committed();
        }
    }
    let scrolled = s.app.tabs[home].cursor;
    s.settle(lim)?;

    // allocations across 1000 steady-state frames
    let a0 = arena::thread_allocs();
    for k in 0..1000 {
        s.app.key(Key::Char(if k % 2 == 0 { 'k' } else { 'j' }));
        let _ = s.app.frame().len();
    }
    let allocs = arena::thread_allocs() - a0;

    // full-screen redraws
    let mut redraw_us = Vec::with_capacity(300);
    for _ in 0..300 {
        s.app.screen.invalidate();
        let f0 = Instant::now();
        let _ = s.app.frame().len();
        redraw_us.push(f0.elapsed().as_micros() as u32);
    }

    // stream a 4000-token answer, drawing as tokens land
    eprintln!("bench: streaming an answer");
    let claude = s.tab(|t| matches!(t, Source::Chat { service: Service::Claude, .. })).unwrap();
    s.app.focus(claude);
    for c in "explain arenas".chars() {
        s.app.key(Key::Char(c));
    }
    s.app.key(Key::Enter);
    let mut token_us = Vec::with_capacity(4096);
    let t0 = Instant::now();
    let mut started = false;
    loop {
        sys::wait_any(&[s.ui_ev.0], 100);
        let mut grew = false;
        let a = Instant::now();
        while let Some(r) = s.rep.pop() {
            grew |= matches!(r, Reply::Grow { .. });
            s.app.apply(r);
        }
        if grew {
            let _ = s.app.frame().len();
            token_us.push(a.elapsed().as_micros() as u32);
        }
        started |= s.app.tabs[claude].loading;
        if started && !s.app.tabs[claude].loading {
            break;
        }
        if t0.elapsed() > lim {
            return Err("the streamed answer never finished".into());
        }
    }
    let answer = match &s.app.tabs[claude].store {
        Store::Chat(c) => c.msgs.last().map(|m| m.text.len).unwrap_or(0),
        _ => 0,
    };
    let mem = sys::process_memory();

    // the json scanner on a 1 mb document: no heap, and its own size
    crate::json_paths!(J { "data[*]" => 1, Record; "data[*].text" => 2, Field; });
    struct Count(usize);
    impl Sink for Count {
        fn field_start(&mut self, _f: u16, _s: bool) {}
        fn bytes(&mut self, b: &[u8]) {
            self.0 += b.len();
        }
        fn field_end(&mut self, _f: u16) {}
    }
    let doc = x_timeline("/x?max_results=100", sys::now_unix());
    let mut big = String::from("{\"data\":[");
    while big.len() < MB {
        big.push_str(&doc[9..doc.find("],\"includes\"").unwrap()]);
        big.push(',');
    }
    big.push_str("{}]}");
    let paths = Paths::compile(J);
    let a0 = arena::thread_allocs();
    let mut sc = Scanner::new(&paths);
    let mut cnt = Count(0);
    for chunk in big.as_bytes().chunks(16 * 1024) {
        sc.feed(chunk, &mut cnt);
    }
    let json_allocs = arena::thread_allocs() - a0;
    let json_extra = core::mem::size_of::<Scanner>() + 16 * 1024;

    s.app.send(Command::Shutdown);
    let _ = net.join();
    let _ = server.kill();
    let pout = probe.wait_with_output().map_err(|e| e.to_string())?;
    let pline = String::from_utf8_lossy(&pout.stdout).to_string();
    let field = |k: &str| pline.split_whitespace().find_map(|kv| kv.strip_prefix(k).and_then(|v| v.strip_prefix('='))).and_then(|v| v.parse::<u64>().ok()).unwrap_or(u64::MAX);
    let _ = std::fs::remove_dir_all(&dir);

    let bin = std::fs::metadata(&exe).map(|m| m.len()).unwrap_or(0);
    let release = !cfg!(debug_assertions);
    // a shared ci runner is a slice of a busy machine, so its wall-clock times say
    // more about the neighbours than about hayate. memory and allocations still count
    let timed = std::env::var_os("CI").is_none();
    let mbs = |b: u64| format!("{:.2} mb", b as f64 / MB as f64);
    let ms = |us: u64| format!("{:.2} ms", us as f64 / 1000.0);
    let mut rows = vec![
        Row { name: "binary size, release, stripped", limit: "4 mb".into(), got: mbs(bin), pass: release.then_some(bin <= 4 * MB as u64) },
        Row { name: "cold start to first frame", limit: "40 ms".into(), got: ms(field("startup_us")), pass: timed.then_some(field("startup_us") <= 40_000) },
        Row { name: "idle private bytes, no tabs", limit: "4 mb".into(), got: mbs(field("idle_private")), pass: release.then_some(field("idle_private") <= 4 * MB as u64) },
        Row {
            name: "  idle working set (os floor)",
            limit: "info".into(),
            got: format!("{} ({})", mbs(field("idle_ws")), mbs(field("floor_ws"))),
            pass: None,
        },
        Row { name: "one text tab (200 posts)", limit: "1 mb".into(), got: mbs(one_tab as u64), pass: Some(one_tab > 0 && one_tab <= MB) },
        Row { name: "four services, 10 tabs, images off", limit: "20 mb".into(), got: mbs(mem.peak_working_set as u64), pass: release.then_some(mem.peak_working_set <= 20 * MB) },
        Row { name: "same with image cache full", limit: "30 mb".into(), got: "n/a: images not built".into(), pass: None },
        Row { name: "keypress to screen updated, p99", limit: "4 ms".into(), got: ms(pct(&mut key_us, 99) as u64), pass: timed.then_some(pct(&mut key_us, 99) <= 4000) },
        Row { name: "full-screen redraw 200x60, p99", limit: "1 ms".into(), got: ms(pct(&mut redraw_us, 99) as u64), pass: timed.then_some(pct(&mut redraw_us, 99) <= 1000) },
        Row { name: "scroll, p99 frame (120 fps)", limit: "8.33 ms".into(), got: ms(pct(&mut frame_us, 99) as u64), pass: timed.then_some(pct(&mut frame_us, 99) <= 8333) },
        Row {
            name: "cpu while idle",
            limit: "0%".into(),
            got: format!("{} µs in {} s", field("idle_cpu_us"), idle_secs),
            // cpu time is sampled in 15.6 ms ticks, and on a shared runner one can land on
            // an idle process. here it has to be none
            pass: Some(field("idle_cpu_us") < if timed { 2_000 } else { 16_000 }),
        },
        Row { name: "heap allocs, 1000 steady frames", limit: "0".into(), got: allocs.to_string(), pass: Some(allocs == 0) },
        Row { name: "streamed token to screen, p99", limit: "1 ms".into(), got: ms(pct(&mut token_us, 99) as u64), pass: Some(!token_us.is_empty() && (!timed || pct(&mut token_us, 99) <= 1000)) },
        Row {
            name: "json scanner extra memory per mb",
            limit: "64 kb".into(),
            got: format!("{:.1} kb, {} allocs", json_extra as f64 / 1024.0, json_allocs),
            pass: Some(json_extra <= 64 * 1024 && json_allocs == 0),
        },
    ];
    if !release {
        rows.push(Row { name: "(debug build: memory rows are not judged)", limit: String::new(), got: String::new(), pass: None });
    }
    if !timed {
        rows.push(Row { name: "(ci runner: times are shown, not judged)", limit: String::new(), got: String::new(), pass: None });
    }

    println!();
    println!("hayate bench · {} rows scrolled · {} tabs · {} kb answer streamed", scrolled + 1, s.app.tabs.len(), answer / 1024);
    println!();
    println!("{:<38} {:>9}  {:>24}", "budget", "limit", "measured");
    let mut failed = 0;
    for r in &rows {
        let verdict = match r.pass {
            Some(true) => "pass",
            Some(false) => {
                failed += 1;
                "FAIL"
            }
            None => "",
        };
        println!("{:<38} {:>9}  {:>24}  {verdict}", r.name, r.limit, r.got);
    }
    println!();
    let arenas: Vec<String> = (0..5).map(|i| format!("{} {:.0} kb", crate::arena::ARENA_NAMES[i], arena::high([ArenaId::Frame, ArenaId::Net, ArenaId::TabText, ArenaId::Store, ArenaId::Pixels][i]) as f64 / 1024.0)).collect();
    println!("arena high-water: {}", arenas.join(" · "));
    if failed > 0 {
        return Err(format!("{failed} budget(s) failed"));
    }
    Ok(())
}

// ---------------------------------------------------------------- screenshots

/// `hayate shot <dir> [w h only]`: drives the ui through its screens with demo data for
/// the four services and real pages from the web, and writes each frame's cells and
/// colors as json for `tools/shot.py` to draw.
pub fn shots(args: &[String]) -> Result<(), String> {
    let out = std::path::PathBuf::from(args.first().map(String::as_str).unwrap_or("shots"));
    let w: u16 = args.get(1).and_then(|v| v.parse().ok()).unwrap_or(150);
    let h: u16 = args.get(2).and_then(|v| v.parse().ok()).unwrap_or(44);
    let only: Option<String> = args.get(3).cloned();
    let want = |n: &str| only.as_deref().is_none_or(|o| n.contains(o));
    std::fs::create_dir_all(&out).map_err(|e| e.to_string())?;
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let mut server = Proc::new(&exe).arg("replay-server").env("HAYATE_BENCH_TOKENS", "260").stdout(Stdio::piped()).spawn().map_err(|e| e.to_string())?;
    let mut addr = String::new();
    BufReader::new(server.stdout.take().unwrap()).read_line(&mut addr).map_err(|e| e.to_string())?;
    // SAFETY: set before the net thread starts
    unsafe { std::env::set_var("HAYATE_TEST_KEYS", "1") };
    let dir = std::env::temp_dir().join(format!("hayate-shot-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let mut cfg = Config::defaults_for_tests(&dir);
    cfg.replay_addr = Some(addr.trim().to_string());
    cfg.yt_per_channel = 3;
    let cfg = Arc::new(cfg);
    let (cmd_tx, cmd_rx) = ring(1024);
    let (rep_tx, rep_rx) = ring(4096);
    let cmd_ev = Arc::new(WinEvent::new());
    let ui_ev = Arc::new(WinEvent::new());
    let authors = Arc::new(TextArena::with_budget(2 * MB));
    let net = net::spawn(NetInit { cfg: cfg.clone(), cmds: cmd_rx, replies: rep_tx, cmd_event: cmd_ev.clone(), ui_event: ui_ev.clone(), authors: authors.clone() });
    let app = App::new(cfg, w, h, cmd_tx, cmd_ev, authors);
    let mut s = Session { app, rep: rep_rx, ui_ev };
    let lim = Duration::from_secs(40);
    let save = |s: &mut Session, name: &str| {
        let _ = s.app.frame().len();
        let _ = std::fs::write(out.join(format!("{name}.json")), s.app.screen.dump_json());
        eprintln!("shot: {name}");
    };

    s.app.open(Source::Start);
    s.app.open(Source::XHome);
    s.app.open(Source::YtSubs);
    s.app.open(Source::Chat { service: Service::Claude, chat: 1 });
    s.app.open(Source::Web { url: "https://news.ycombinator.com/".into() });
    s.app.open(Source::Web { url: "https://en.wikipedia.org/wiki/Hayate".into() });
    s.app.focus(0);
    if let Err(e) = s.settle(lim) {
        eprintln!("shot: {e}");
    }
    if want("start") {
        for c in "rust arena allocator".chars() {
            s.app.key(Key::Char(c));
        }
        save(&mut s, "01-start");
        for _ in 0..20 {
            s.app.key(Key::Backspace);
        }
    }
    if want("feed") {
        s.app.focus(1);
        s.app.key(Key::Char('j'));
        s.app.key(Key::Char('j'));
        save(&mut s, "02-x-feed");
    }
    if want("youtube") {
        s.app.focus(2);
        save(&mut s, "03-youtube");
    }
    if want("chat") {
        s.app.focus(3);
        for c in "how do arenas keep a terminal client under 10 mb?".chars() {
            s.app.key(Key::Char(c));
        }
        s.app.key(Key::Enter);
        let t0 = Instant::now();
        let mut started = false;
        while t0.elapsed() < lim {
            sys::wait_any(&[s.ui_ev.0], 50);
            s.drain();
            started |= s.app.tabs[3].loading;
            if started && !s.app.tabs[3].loading {
                break;
            }
        }
        for c in "and what about images?".chars() {
            s.app.key(Key::Char(c));
        }
        save(&mut s, "04-chat");
    }
    if want("hn") {
        s.app.focus(4);
        save(&mut s, "05-hacker-news");
        for _ in 0..3 {
            s.app.key(Key::Tab);
        }
        save(&mut s, "06-hn-link-focus");
    }
    if want("wiki") {
        s.app.focus(5);
        save(&mut s, "07-wikipedia");
    }
    if want("palette") {
        s.app.focus(4);
        s.app.key(Key::Ctrl('k'));
        for c in "hack".chars() {
            s.app.key(Key::Char(c));
        }
        save(&mut s, "08-palette");
        s.app.key(Key::Esc);
    }
    if want("help") {
        s.app.focus(1);
        s.app.key(Key::Char('?'));
        save(&mut s, "09-help");
        s.app.key(Key::Esc);
    }
    if want("search") {
        s.app.open_focus(Source::Start);
        for c in "rust arena allocator".chars() {
            s.app.key(Key::Char(c));
        }
        s.app.key(Key::Enter);
        let _ = s.settle(lim);
        save(&mut s, "11-search");
    }
    if want("narrow") {
        s.app.resize(90, 30);
        s.app.focus(1);
        save(&mut s, "10-narrow");
    }
    s.app.send(Command::Shutdown);
    let _ = net.join();
    let _ = server.kill();
    let _ = std::fs::remove_dir_all(&dir);
    Ok(())
}
