//! The net thread: turns bytes into rows.
//!
//! It sleeps in one blocking wait on its command event and on every live socket. When a
//! socket has data it reads what is there, feeds the http parser, and the parser feeds the
//! adapter's scanner, which writes text straight into the tab's arena. Finished rows go to
//! the ui thread through the reply ring.

pub mod conn;
pub mod http;
pub mod sse;

use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use crate::adapters;
use crate::arena::{Consumer, Producer};
use crate::config::Config;
use crate::msg::{Command, Level, Line, NetEntry, Reply, Service, TabId};
use crate::store::{Interner, Log, RecBuf, TextArena, REC_SPEND};
use crate::sys::{self, WinEvent};
pub use conn::Host;
use conn::Conn;
use http::Response;

pub const MAX_CONNS: usize = 4;

/// One request to send.
pub struct Req {
    pub tab: Option<TabId>,
    pub service: Service,
    pub host: Host,
    pub method: &'static str,
    pub target: Vec<u8>,
    pub headers: Vec<(&'static str, Vec<u8>)>,
    pub body: Vec<u8>,
    pub what: String,
    pub job: adapters::Job,
    /// estimated cost reserved by the spend guard, micro-dollars
    pub est_micros: u64,
    /// youtube quota units this call costs
    pub units: u32,
    /// seconds of silence before the request is called timed out
    pub idle_timeout: u64,
}

struct Active {
    req: Req,
    conn: usize,
    resp: Response,
    started: Instant,
    last: Instant,
    ttfb: Option<u32>,
    bytes_in: u32,
    bytes_out: u32,
    reused: bool,
    err_body: Vec<u8>,
    record: Option<std::fs::File>,
}

/// Month-to-date spend per paid service and youtube units used today.
#[derive(Default, Clone, Copy, Debug)]
pub struct Spend {
    pub micros: [u64; 3], // claude, chatgpt, x
    pub yt_units: u32,
    pub yt_day: u32,
    pub month: u32,
}

pub fn spend_slot(s: Service) -> Option<usize> {
    match s {
        Service::Claude => Some(0),
        Service::ChatGpt => Some(1),
        Service::X => Some(2),
        _ => None,
    }
}

/// Calendar month of a unix time, as year*12+month.
pub fn month_of(t: u64) -> u32 {
    let (y, m, _) = adapters::civil_from_days((t / 86400) as i64);
    (y * 12 + m as i64) as u32
}

/// The youtube quota day (it resets at midnight pacific; this uses utc-8).
pub fn yt_day_of(t: u64) -> u32 {
    (t.saturating_sub(8 * 3600) / 86400) as u32
}

pub struct Logs {
    pub dir: PathBuf,
    pub per: [Option<Log>; 5], // x, youtube, claude, chatgpt, meta
}

impl Logs {
    pub fn open(dir: &std::path::Path) -> Logs {
        let names = ["x.log", "youtube.log", "claude.log", "chatgpt.log", "hayate.log"];
        let per = names.map(|n| Log::open(&dir.join(n)).ok());
        Logs { dir: dir.to_path_buf(), per }
    }
    pub fn for_service(&mut self, s: Service) -> Option<&mut Log> {
        let i = match s {
            Service::X => 0,
            Service::YouTube => 1,
            Service::Claude => 2,
            Service::ChatGpt => 3,
            Service::Local | Service::Web => 4,
        };
        self.per[i].as_mut()
    }
    pub fn meta(&mut self) -> Option<&mut Log> {
        self.per[4].as_mut()
    }
}

/// State the adapters read and write while bytes flow.
pub struct Shared {
    pub cfg: Arc<Config>,
    pub out: Vec<Reply>,
    pub authors: adapters::Authors,
    pub interner: Interner,
    pub tabs: HashMap<TabId, adapters::NetTab>,
    pub logs: Logs,
    pub secrets: adapters::Secrets,
    pub spend: Spend,
    pub rec: RecBuf,
    pub followups: Vec<Req>,
    pub ad: adapters::State,
}

impl Shared {
    pub fn status(&mut self, tab: Option<TabId>, level: Level, s: &str) {
        self.out.push(Reply::Status { tab, level, line: Line::new(s), retry_at: 0 });
    }

    /// The spend guard: refuses a call locally if it could cross the monthly cap.
    pub fn allow(&self, service: Service, est_micros: u64, units: u32) -> Result<(), String> {
        if let Some(i) = spend_slot(service) {
            let cap = (self.cfg.cap_usd[i] * 1e6) as u64;
            if self.spend.micros[i] + est_micros > cap {
                return Err(format!(
                    "{} cap: ${:.2} spent of ${:.2}, this call could cost ${:.3}",
                    service.name(),
                    self.spend.micros[i] as f64 / 1e6,
                    cap as f64 / 1e6,
                    est_micros as f64 / 1e6
                ));
            }
        }
        if service == Service::YouTube && self.spend.yt_units + units > self.cfg.yt_daily_quota {
            return Err(format!("youtube quota: {} of {} units used today", self.spend.yt_units, self.cfg.yt_daily_quota));
        }
        Ok(())
    }

    /// Adds actual cost from a response, stores it in the log, and tells the ui.
    pub fn charge(&mut self, service: Service, micros: u64, units: u32) {
        let now = sys::now_unix();
        let (m, d) = (month_of(now), yt_day_of(now));
        if self.spend.month != m {
            self.spend.month = m;
            self.spend.micros = [0; 3];
        }
        if self.spend.yt_day != d {
            self.spend.yt_day = d;
            self.spend.yt_units = 0;
        }
        if let Some(i) = spend_slot(service) {
            self.spend.micros[i] += micros;
        }
        self.spend.yt_units += units;
        self.rec.start(REC_SPEND).u8(service as u8).u64(micros).u32(units).u32(now as u32);
        if let Some(l) = self.logs.meta() {
            let _ = l.append(&self.rec.b);
        }
        self.push_spend();
    }

    pub fn push_spend(&mut self) {
        let s = self.spend;
        self.out.push(Reply::Spend { claude: s.micros[0], chatgpt: s.micros[1], x: s.micros[2], yt_units: s.yt_units });
    }

    pub fn arena(&self, tab: TabId) -> Option<Arc<TextArena>> {
        self.tabs.get(&tab).map(|t| t.arena.clone())
    }
}

pub struct NetInit {
    pub cfg: Arc<Config>,
    pub cmds: Consumer<Command>,
    pub replies: Producer<Reply>,
    pub cmd_event: Arc<WinEvent>,
    pub ui_event: Arc<WinEvent>,
    pub authors: Arc<TextArena>,
}

pub struct Net {
    cmds: Consumer<Command>,
    replies: Producer<Reply>,
    cmd_event: Arc<WinEvent>,
    ui_event: Arc<WinEvent>,
    pub sh: Shared,
    conns: Vec<Option<Conn>>,
    queue: VecDeque<Req>,
    active: Vec<Active>,
    reqbuf: Vec<u8>,
    record_dir: Option<PathBuf>,
    record_seq: u32,
    handles: Vec<sys::Handle>,
}

pub fn spawn(init: NetInit) -> std::thread::JoinHandle<()> {
    std::thread::Builder::new().name("net".into()).spawn(move || Net::new(init).run()).expect("spawn net thread")
}

impl Net {
    pub fn new(init: NetInit) -> Net {
        let logs = Logs::open(&init.cfg.data_dir);
        let mut sh = Shared {
            cfg: init.cfg.clone(),
            out: Vec::with_capacity(1024),
            authors: adapters::Authors::new(init.authors),
            interner: Interner::new(),
            tabs: HashMap::new(),
            logs,
            secrets: adapters::Secrets::load(),
            spend: Spend::default(),
            rec: RecBuf::new(),
            followups: Vec::new(),
            ad: adapters::State::default(),
        };
        adapters::load_spend(&mut sh);
        sh.push_spend();
        Net {
            cmds: init.cmds,
            replies: init.replies,
            cmd_event: init.cmd_event,
            ui_event: init.ui_event,
            sh,
            conns: (0..MAX_CONNS).map(|_| None).collect(),
            queue: VecDeque::new(),
            active: Vec::new(),
            reqbuf: Vec::with_capacity(64 * 1024),
            record_dir: std::env::var_os("HAYATE_RECORD").map(PathBuf::from),
            record_seq: 0,
            handles: Vec::with_capacity(MAX_CONNS + 1),
        }
    }

    pub fn run(mut self) {
        adapters::startup(&mut self.sh);
        self.flush();
        loop {
            self.handles.clear();
            self.handles.push(self.cmd_event.0);
            for a in &self.active {
                if let Some(c) = &self.conns[a.conn] {
                    self.handles.push(c.wait_handle());
                }
            }
            // no timers when idle: wait forever unless a request could time out
            let now = Instant::now();
            let timeout = self
                .active
                .iter()
                .map(|a| (a.req.idle_timeout * 1000).saturating_sub(now.duration_since(a.last).as_millis() as u64))
                .min()
                .map(|ms| ms.min(u32::MAX as u64 - 1) as u32)
                .unwrap_or(sys::INFINITE_WAIT);
            let _ = sys::wait_any(&self.handles, timeout);

            // commands first
            let mut stop = false;
            while let Some(cmd) = self.cmds.pop() {
                if matches!(cmd, Command::Shutdown) {
                    stop = true;
                    break;
                }
                if let Command::Close { tab } = &cmd {
                    let tab = *tab;
                    self.queue.retain(|r| r.tab != Some(tab));
                }
                adapters::command(&mut self.sh, cmd);
            }
            if stop {
                break;
            }
            // every live socket: read what is there
            let mut i = 0;
            while i < self.active.len() {
                if self.pump(i) {
                    i += 1;
                }
            }
            self.check_timeouts();
            self.queue.extend(self.sh.followups.drain(..));
            self.start_queued();
            self.flush();
        }
        // a clean exit: close connections with close_notify
        self.conns.clear();
    }

    /// Sends finished replies to the ui. Arenas are published first, so a row never
    /// arrives before its text.
    fn flush(&mut self) {
        if self.sh.out.is_empty() {
            return;
        }
        for t in self.sh.tabs.values() {
            t.arena.publish();
        }
        self.sh.authors.arena.publish();
        for r in self.sh.out.drain(..) {
            let mut r = r;
            loop {
                match self.replies.push(r) {
                    Ok(()) => break,
                    Err(back) => {
                        r = back;
                        self.ui_event.set();
                        std::thread::yield_now();
                    }
                }
            }
        }
        self.ui_event.set();
    }

    fn start_queued(&mut self) {
        let mut n = self.queue.len();
        while n > 0 {
            n -= 1;
            let Some(req) = self.queue.pop_front() else { break };
            if let Some(tab) = req.tab {
                if !self.sh.tabs.contains_key(&tab) {
                    continue; // the tab was closed while this waited
                }
            }
            if let Err(req) = self.start(req) {
                self.queue.push_back(req);
            }
        }
    }

    /// Starts a request on a free connection to its host. Hands it back when every
    /// connection is busy.
    fn start(&mut self, req: Req) -> Result<(), Req> {
        let host = req.host;
        let free = self.conns.iter().position(|c| c.as_ref().is_some_and(|c| c.host == host && !c.busy));
        let slot = match free {
            Some(i) => i,
            // one keep-alive connection per host: wait for it
            None if self.conns.iter().any(|c| c.as_ref().is_some_and(|c| c.host == host)) => return Err(req),
            None => {
                let empty = self.conns.iter().position(|c| c.is_none());
                let idle = self.conns.iter().enumerate().filter(|(_, c)| c.as_ref().is_some_and(|c| !c.busy)).min_by_key(|(_, c)| c.as_ref().unwrap().last_used).map(|(i, _)| i);
                let Some(i) = empty.or(idle) else { return Err(req) };
                self.conns[i] = None;
                // the replay server stands in for the four apis; web pages are always real
                let plain = if matches!(host, Host::Web(_)) { None } else { self.sh.cfg.replay_addr.as_deref() };
                match Conn::open(host, plain) {
                    Ok(c) => self.conns[i] = Some(c),
                    Err(e) => {
                        self.fail(&req, 0, &format!("{}: cannot connect: {e}", host.name()), 0);
                        return Ok(());
                    }
                }
                i
            }
        };
        let hdrs: Vec<(&str, &[u8])> = req.headers.iter().map(|(k, v)| (*k, v.as_slice())).collect();
        http::write_request(&mut self.reqbuf, req.method, host.name(), &req.target, &hdrs, &req.body);
        let conn = self.conns[slot].as_mut().unwrap();
        let reused = conn.uses > 0;
        if let Err(e) = conn.send(&self.reqbuf) {
            self.conns[slot] = None;
            if reused {
                // a stale keep-alive connection: try once more on a fresh one
                return Err(req);
            }
            self.fail(&req, 0, &format!("{}: send failed: {e}", host.name()), 0);
            return Ok(());
        }
        conn.busy = true;
        let record = self.record_dir.as_ref().and_then(|d| {
            self.record_seq += 1;
            let path = String::from_utf8_lossy(req.target.split(|&b| b == b'?').next().unwrap_or(b"")).replace(['/', '\\', ':'], "_");
            let name = format!("{:04}-{}{}.http", self.record_seq, host.name(), path);
            std::fs::create_dir_all(d).ok();
            std::fs::File::create(d.join(name)).ok()
        });
        let now = Instant::now();
        self.active.push(Active {
            bytes_out: self.reqbuf.len() as u32,
            req,
            conn: slot,
            resp: Response::new(),
            started: now,
            last: now,
            ttfb: None,
            bytes_in: 0,
            reused,
            err_body: Vec::new(),
            record,
        });
        Ok(())
    }

    /// Reads from one active request's socket. Returns false when the request finished
    /// and was removed (so the caller does not advance its index).
    fn pump(&mut self, i: usize) -> bool {
        let slot = self.active[i].conn;
        let Some(conn) = self.conns[slot].as_mut() else {
            let a = self.active.swap_remove(i);
            self.fail(&a.req, 0, "connection lost", a.bytes_out);
            return false;
        };
        let a = &mut self.active[i];
        let sh = &mut self.sh;
        let mut perr: Option<http::HttpError> = None;
        let res = conn.read_ready(&mut |bytes: &[u8]| {
            if perr.is_some() || a.resp.done() {
                return;
            }
            if a.ttfb.is_none() {
                a.ttfb = Some(a.started.elapsed().as_millis() as u32);
            }
            a.bytes_in += bytes.len() as u32;
            if let Some(f) = &mut a.record {
                use std::io::Write;
                let _ = f.write_all(bytes);
            }
            let resp = &mut a.resp;
            let req = &mut a.req;
            let err_body = &mut a.err_body;
            let r = resp.feed(bytes, &mut |status: u16, ctype: u8, body: &[u8]| {
                if (200..300).contains(&status) {
                    adapters::on_body(sh, req, ctype, body);
                } else if err_body.len() < 8192 {
                    err_body.extend_from_slice(&body[..body.len().min(8192 - err_body.len())]);
                }
            });
            if let Err(e) = r {
                perr = Some(e);
            }
        });
        let a = &mut self.active[i];
        if res.is_ok() && a.bytes_in > 0 {
            a.last = Instant::now();
        }
        let eof = match res {
            Ok(r) => r.eof,
            Err(_) => true,
        };
        if let Some(e) = perr {
            let a = self.active.swap_remove(i);
            self.conns[slot] = None;
            self.fail(&a.req, a.resp.status, &format!("bad response: {e:?}"), a.bytes_out);
            return false;
        }
        if self.active[i].resp.done() {
            let a = self.active.swap_remove(i);
            let close = a.resp.close;
            if let Some(c) = self.conns[slot].as_mut() {
                c.busy = false;
            }
            if close {
                self.conns[slot] = None;
            }
            self.finish(a);
            return false;
        }
        if eof {
            let mut a = self.active.swap_remove(i);
            self.conns[slot] = None;
            if a.resp.eof().is_ok() {
                self.finish(a);
            } else if a.reused && a.bytes_in == 0 {
                // the server had closed this keep-alive connection: resend once
                a.req.idle_timeout = a.req.idle_timeout.max(1);
                self.queue.push_front(a.req);
            } else {
                self.fail(&a.req, a.resp.status, "connection closed mid-response", a.bytes_out);
            }
            return false;
        }
        true
    }

    fn check_timeouts(&mut self) {
        let mut i = 0;
        while i < self.active.len() {
            let a = &self.active[i];
            if a.last.elapsed().as_secs() >= a.req.idle_timeout {
                let a = self.active.swap_remove(i);
                self.conns[a.conn] = None;
                let msg = format!("{}: timed out after {} s", a.req.host.name(), a.req.idle_timeout);
                self.fail(&a.req, 0, &msg, a.bytes_out);
            } else {
                i += 1;
            }
        }
    }

    fn net_entry(&mut self, req: &Req, status: u16, bytes_out: u32, bytes_in: u32, ttfb: u32, total: u32, cost: u32) {
        self.sh.out.push(Reply::Net(NetEntry {
            service: req.service,
            what: Line::new(&req.what),
            status,
            bytes_out,
            bytes_in,
            ttfb_ms: ttfb,
            total_ms: total,
            cost,
        }));
    }

    fn fail(&mut self, req: &Req, status: u16, msg: &str, bytes_out: u32) {
        self.net_entry(req, status, bytes_out, 0, 0, 0, 0);
        adapters::on_fail(&mut self.sh, req, status, msg, 0);
    }

    fn finish(&mut self, mut a: Active) {
        let total = a.started.elapsed().as_millis() as u32;
        let status = a.resp.status;
        if (300..400).contains(&status) && !a.resp.location.is_empty() {
            let loc = String::from_utf8_lossy(&a.resp.location).into_owned();
            if adapters::on_redirect(&mut self.sh, &mut a.req, &loc) {
                self.net_entry(&a.req, status, a.bytes_out, a.bytes_in, a.ttfb.unwrap_or(total), total, 0);
                return;
            }
        }
        if a.resp.encoded && (200..300).contains(&status) {
            adapters::on_fail(&mut self.sh, &a.req, status, "the server sent a compressed page hayate cannot read", 0);
            return;
        }
        let cost = if (200..300).contains(&status) {
            adapters::on_done(&mut self.sh, &mut a.req)
        } else {
            let retry_at = if a.resp.rate_reset > 0 {
                a.resp.rate_reset
            } else if a.resp.retry_after > 0 {
                sys::now_unix() + a.resp.retry_after as u64
            } else {
                0
            };
            let detail = adapters::error_text(&a.err_body);
            let msg = match status {
                401 | 403 => format!("{}: not authorized ({status}) {detail}", a.req.service.name()),
                429 => format!("{}: rate limited {detail}", a.req.service.name()),
                _ => format!("{}: http {status} {detail}", a.req.service.name()),
            };
            adapters::on_fail(&mut self.sh, &a.req, status, &msg, retry_at);
            0
        };
        self.net_entry(&a.req, status, a.bytes_out, a.bytes_in, a.ttfb.unwrap_or(total), total, cost);
    }
}
