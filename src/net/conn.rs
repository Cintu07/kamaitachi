//! One keep-alive connection per host, on rustls. Connect and handshake block (once per
//! session per host); after that the socket is non-blocking and tied to a winsock event,
//! so the net thread can sleep in one wait across every socket and its command event.
//! The event is Windows only. Elsewhere (the Android build) a socket armed for reading
//! is just non-blocking, and only the blocking one-off requests are used.

use std::io::{ErrorKind, Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
#[cfg(windows)]
use std::os::windows::io::AsRawSocket;
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use rustls::pki_types::ServerName;
use rustls::{ClientConfig, ClientConnection, RootCertStore};
#[cfg(windows)]
use windows_sys::Win32::Foundation::HANDLE;
#[cfg(windows)]
use windows_sys::Win32::Networking::WinSock::*;

use crate::arena::{self, ArenaId, NET_CONN_BUDGET};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Host {
    Anthropic,
    OpenAi,
    Google,
    GoogleAuth,
    X,
    /// any https site the browser visits
    Web(&'static str),
}

impl Host {
    pub fn name(self) -> &'static str {
        match self {
            Host::Anthropic => "api.anthropic.com",
            Host::OpenAi => "api.openai.com",
            Host::Google => "www.googleapis.com",
            Host::GoogleAuth => "oauth2.googleapis.com",
            Host::X => "api.x.com",
            Host::Web(h) => h,
        }
    }

    /// A host for a page. Names are kept for the life of the process (one copy each).
    pub fn web(name: &str) -> Host {
        static HOSTS: std::sync::Mutex<Vec<&'static str>> = std::sync::Mutex::new(Vec::new());
        let mut v = HOSTS.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(h) = v.iter().find(|h| **h == name) {
            return Host::Web(h);
        }
        let s: &'static str = Box::leak(name.to_string().into_boxed_str());
        v.push(s);
        Host::Web(s)
    }
}

fn tls_config() -> Arc<ClientConfig> {
    static CFG: OnceLock<Arc<ClientConfig>> = OnceLock::new();
    CFG.get_or_init(|| {
        let mut roots = RootCertStore::empty();
        roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
        let provider = Arc::new(rustls::crypto::ring::default_provider());
        let cfg = ClientConfig::builder_with_provider(provider)
            .with_safe_default_protocol_versions()
            .expect("tls 1.2 and 1.3 with ring")
            .with_root_certificates(roots)
            .with_no_client_auth();
        Arc::new(cfg)
    })
    .clone()
}

pub struct Conn {
    pub host: Host,
    sock: TcpStream,
    tls: Option<Box<ClientConnection>>,
    #[cfg(windows)]
    event: WSAEVENT,
    armed: bool,
    pub busy: bool,
    pub uses: u32,
    pub last_used: Instant,
    buf: Box<[u8]>,
}

unsafe impl Send for Conn {}

#[derive(Debug)]
pub struct ReadResult {
    pub eof: bool,
}

impl Conn {
    /// Connects (tls unless `plain_addr` redirects to a local replay server).
    pub fn open(host: Host, plain_addr: Option<&str>) -> std::io::Result<Conn> {
        let sock = match plain_addr {
            Some(a) => TcpStream::connect(a)?,
            None => {
                let mut last = None;
                let mut found = None;
                for addr in (host.name(), 443).to_socket_addrs()? {
                    match TcpStream::connect_timeout(&addr, Duration::from_secs(10)) {
                        Ok(s) => {
                            found = Some(s);
                            break;
                        }
                        Err(e) => last = Some(e),
                    }
                }
                found.ok_or_else(|| last.unwrap_or_else(|| std::io::Error::other("no address")))?
            }
        };
        sock.set_nodelay(true)?;
        sock.set_read_timeout(Some(Duration::from_secs(20)))?;
        let mut tls = None;
        if plain_addr.is_none() {
            let name = ServerName::try_from(host.name().to_string()).map_err(std::io::Error::other)?;
            let mut c = ClientConnection::new(tls_config(), name).map_err(std::io::Error::other)?;
            let mut s = &sock;
            while c.is_handshaking() {
                c.complete_io(&mut s)?;
            }
            tls = Some(Box::new(c));
        }
        #[cfg(windows)]
        let event = unsafe { WSACreateEvent() };
        arena::add(ArenaId::Net, NET_CONN_BUDGET as isize);
        Ok(Conn {
            host,
            sock,
            tls,
            #[cfg(windows)]
            event,
            armed: false,
            busy: false,
            uses: 0,
            last_used: Instant::now(),
            buf: vec![0u8; NET_CONN_BUDGET].into_boxed_slice(),
        })
    }

    fn disarm(&mut self) -> std::io::Result<()> {
        if self.armed {
            #[cfg(windows)]
            unsafe {
                WSAEventSelect(self.sock.as_raw_socket() as SOCKET, 0, 0);
            }
            self.armed = false;
        }
        self.sock.set_nonblocking(false)
    }

    /// Sends a whole request (blocking), then arms the socket for event-driven reads.
    pub fn send(&mut self, bytes: &[u8]) -> std::io::Result<()> {
        self.disarm()?;
        match &mut self.tls {
            Some(c) => {
                c.writer().write_all(bytes)?;
                let mut s = &self.sock;
                while c.wants_write() {
                    c.write_tls(&mut s)?;
                }
            }
            None => (&self.sock).write_all(bytes)?,
        }
        self.uses += 1;
        self.last_used = Instant::now();
        #[cfg(windows)]
        unsafe {
            if WSAEventSelect(self.sock.as_raw_socket() as SOCKET, self.event, (FD_READ | FD_CLOSE) as i32) != 0 {
                return Err(std::io::Error::last_os_error());
            }
        }
        #[cfg(not(windows))]
        self.sock.set_nonblocking(true)?;
        self.armed = true;
        Ok(())
    }

    /// Reads everything available right now, handing plaintext to `f`.
    pub fn read_ready(&mut self, f: &mut dyn FnMut(&[u8])) -> std::io::Result<ReadResult> {
        #[cfg(windows)]
        unsafe {
            let mut ne: WSANETWORKEVENTS = core::mem::zeroed();
            WSAEnumNetworkEvents(self.sock.as_raw_socket() as SOCKET, self.event, &mut ne);
        }
        let mut eof = false;
        match &mut self.tls {
            None => loop {
                match (&self.sock).read(&mut self.buf) {
                    Ok(0) => {
                        eof = true;
                        break;
                    }
                    Ok(n) => f(&self.buf[..n]),
                    Err(e) if e.kind() == ErrorKind::WouldBlock => break,
                    Err(e) => return Err(e),
                }
            },
            Some(c) => {
                let mut s = &self.sock;
                loop {
                    let mut got = false;
                    match c.read_tls(&mut s) {
                        Ok(0) => eof = true,
                        Ok(_) => got = true,
                        Err(e) if e.kind() == ErrorKind::WouldBlock => {}
                        Err(e) => return Err(e),
                    }
                    c.process_new_packets().map_err(std::io::Error::other)?;
                    if c.wants_write() {
                        let _ = c.write_tls(&mut s);
                    }
                    loop {
                        match c.reader().read(&mut self.buf) {
                            Ok(0) => {
                                eof = true;
                                break;
                            }
                            Ok(n) => f(&self.buf[..n]),
                            Err(e) if e.kind() == ErrorKind::WouldBlock => break,
                            Err(e) if e.kind() == ErrorKind::UnexpectedEof => {
                                eof = true;
                                break;
                            }
                            Err(e) => return Err(e),
                        }
                    }
                    if !got || eof {
                        break;
                    }
                }
            }
        }
        self.last_used = Instant::now();
        Ok(ReadResult { eof })
    }
}

impl Drop for Conn {
    fn drop(&mut self) {
        let _ = self.disarm();
        if let Some(c) = &mut self.tls {
            c.send_close_notify();
            let mut s = &self.sock;
            let _ = c.write_tls(&mut s);
        }
        #[cfg(windows)]
        unsafe {
            WSACloseEvent(self.event);
        }
        arena::add(ArenaId::Net, -(NET_CONN_BUDGET as isize));
    }
}

/// A one-off blocking request on its own connection, for auth flows. Returns status and
/// body.
pub fn simple(host: Host, plain: Option<&str>, method: &str, target: &[u8], headers: &[(&str, &[u8])], body: &[u8]) -> std::io::Result<(u16, Vec<u8>)> {
    full(host, plain, method, target, headers, body).map(|(r, b)| (r.status, b))
}

/// Connections kept open after a request, for the next one to the same site. Opening
/// one costs a TCP and a TLS handshake, half a second to a far server; a page and its
/// stylesheets and images mostly come from one or two sites.
fn idle() -> &'static std::sync::Mutex<Vec<Conn>> {
    static IDLE: OnceLock<std::sync::Mutex<Vec<Conn>>> = OnceLock::new();
    IDLE.get_or_init(Default::default)
}

/// Servers drop idle connections after a while; one older than this is not trusted.
const IDLE_FOR: Duration = Duration::from_secs(25);
const IDLE_MOST: usize = 12;

fn take_idle(host: Host) -> Option<Conn> {
    let mut v = idle().lock().unwrap_or_else(|e| e.into_inner());
    v.retain(|c| c.last_used.elapsed() < IDLE_FOR);
    let i = v.iter().position(|c| c.host == host)?;
    Some(v.swap_remove(i))
}

fn put_idle(c: Conn) {
    let mut v = idle().lock().unwrap_or_else(|e| e.into_inner());
    if v.len() >= IDLE_MOST {
        v.remove(0);
    }
    v.push(c);
}

/// One blocking request; the response head (status, location, content type) and body.
/// It goes on a connection kept from an earlier request to the same site when there is
/// one, and if that turns out to have been closed by the server, on a new one.
pub fn full(host: Host, plain: Option<&str>, method: &str, target: &[u8], headers: &[(&str, &[u8])], body: &[u8]) -> std::io::Result<(super::http::Response, Vec<u8>)> {
    if plain.is_none()
        && let Some(c) = take_idle(host)
        && let Ok(r) = exchange(c, host, method, target, headers, body)
    {
        return Ok(r);
    }
    exchange(Conn::open(host, plain)?, host, method, target, headers, body)
}

fn exchange(mut c: Conn, host: Host, method: &str, target: &[u8], headers: &[(&str, &[u8])], body: &[u8]) -> std::io::Result<(super::http::Response, Vec<u8>)> {
    let mut req = Vec::new();
    super::http::write_request(&mut req, method, host.name(), target, headers, body);
    c.send(&req)?;
    c.disarm()?;
    let mut resp = super::http::Response::new();
    let mut out = Vec::new();
    let started = Instant::now();
    let mut closed = false;
    loop {
        let mut n = 0usize;
        let mut err = None;
        {
            let buf = &mut c.buf;
            let r = match &mut c.tls {
                Some(t) => {
                    let mut s = &c.sock;
                    if t.wants_read() && !closed {
                        match t.read_tls(&mut s) {
                            Ok(0) => closed = true,
                            Ok(_) => {}
                            Err(e) => err = Some(e),
                        }
                        if let Err(e) = t.process_new_packets() {
                            err = Some(std::io::Error::other(e));
                        }
                    }
                    t.reader().read(buf)
                }
                None => (&c.sock).read(buf),
            };
            match r {
                Ok(0) => closed = true,
                Ok(k) => n = k,
                Err(e) if e.kind() == ErrorKind::WouldBlock => {}
                Err(e) if e.kind() == ErrorKind::UnexpectedEof => closed = true,
                Err(e) => err = Some(e),
            }
            if n > 0 {
                resp.feed(&buf[..n], &mut |_, _, b| out.extend_from_slice(b)).map_err(|e| std::io::Error::other(format!("{e:?}")))?;
            }
        }
        if resp.done() {
            if !resp.close && !closed {
                c.last_used = Instant::now();
                put_idle(c);
            }
            return Ok((resp, out));
        }
        if let Some(e) = err {
            return Err(e);
        }
        if n == 0 {
            if closed {
                // a kept connection the server had already closed: nothing came at all
                if out.is_empty() && !resp.head_done && c.uses > 1 {
                    return Err(std::io::Error::new(ErrorKind::ConnectionReset, "kept connection was closed"));
                }
                return match resp.eof() {
                    Ok(()) => Ok((resp, out)),
                    Err(_) => Err(std::io::Error::new(ErrorKind::UnexpectedEof, "connection closed mid-response")),
                };
            }
            if started.elapsed() > Duration::from_secs(30) {
                return Err(std::io::Error::new(ErrorKind::TimedOut, "no response"));
            }
        }
    }
}

#[cfg(windows)]
impl Conn {
    /// The handle the net thread waits on for this socket.
    pub fn wait_handle(&self) -> HANDLE {
        self.event as HANDLE
    }
}
