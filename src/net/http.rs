//! HTTP/1.1 by hand. Requests are written into one buffer; responses are parsed as bytes
//! arrive, in any chunk sizes, with content-length, chunked, or read-until-close bodies.

use crate::render::push_num;

pub fn write_request(out: &mut Vec<u8>, method: &str, host: &str, target: &[u8], headers: &[(&str, &[u8])], body: &[u8]) {
    out.clear();
    out.extend_from_slice(method.as_bytes());
    out.push(b' ');
    out.extend_from_slice(target);
    out.extend_from_slice(b" HTTP/1.1\r\nHost: ");
    out.extend_from_slice(host.as_bytes());
    out.extend_from_slice(b"\r\n");
    if !headers.iter().any(|(k, _)| k.eq_ignore_ascii_case("user-agent")) {
        out.extend_from_slice(b"User-Agent: hayate/0.1\r\n");
    }
    out.extend_from_slice(b"Accept-Encoding: identity\r\nConnection: keep-alive\r\n");
    if !body.is_empty() || method == "POST" || method == "PUT" {
        out.extend_from_slice(b"Content-Length: ");
        push_num(out, body.len() as u32);
        out.extend_from_slice(b"\r\n");
    }
    for (k, v) in headers {
        out.extend_from_slice(k.as_bytes());
        out.extend_from_slice(b": ");
        out.extend_from_slice(v);
        out.extend_from_slice(b"\r\n");
    }
    out.extend_from_slice(b"\r\n");
    out.extend_from_slice(body);
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum HttpError {
    BadStatusLine,
    BadChunk,
    ClosedEarly,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum St {
    StatusLine,
    Headers,
    Len,
    ChunkSize,
    ChunkExt,
    ChunkData,
    ChunkDataCr,
    ChunkDataLf,
    Trailers,
    UntilClose,
    Done,
}

const LINE_MAX: usize = 1024;

/// One response being parsed. Fixed size; reusable for the next response on the
/// same keep-alive connection via `reset`.
pub struct Response {
    st: St,
    line: [u8; LINE_MAX],
    line_len: usize,
    line_overflow: bool,
    rem: u64,
    chunk_digits: u8,
    pub status: u16,
    pub content_length: Option<u64>,
    pub chunked: bool,
    pub close: bool,
    /// seconds from a retry-after header
    pub retry_after: u32,
    /// unix time from x-rate-limit-reset (x) when present
    pub rate_reset: u64,
    pub sse: bool,
    /// 0 unknown, 1 html, 2 plain text, 3 something else
    pub ctype: u8,
    /// a compressed body we cannot read (we ask for identity)
    pub encoded: bool,
    /// the Location header of a redirect
    pub location: Vec<u8>,
    pub head_done: bool,
    no_body: bool,
}

impl Default for Response {
    fn default() -> Self {
        Self::new()
    }
}

impl Response {
    pub fn new() -> Response {
        Response {
            st: St::StatusLine,
            line: [0; LINE_MAX],
            line_len: 0,
            line_overflow: false,
            rem: 0,
            chunk_digits: 0,
            status: 0,
            content_length: None,
            chunked: false,
            close: false,
            retry_after: 0,
            rate_reset: 0,
            sse: false,
            ctype: 0,
            encoded: false,
            location: Vec::new(),
            head_done: false,
            no_body: false,
        }
    }

    pub fn reset(&mut self) {
        *self = Response::new();
    }

    pub fn done(&self) -> bool {
        self.st == St::Done
    }

    /// Reads a line into self.line. Returns the position after the line ending, or None
    /// when the line continues past this chunk.
    fn take_line(&mut self, data: &[u8], mut i: usize) -> Option<usize> {
        while i < data.len() {
            let b = data[i];
            i += 1;
            if b == b'\n' {
                if self.line_len > 0 && self.line[self.line_len - 1] == b'\r' {
                    self.line_len -= 1;
                }
                return Some(i);
            }
            if self.line_len < LINE_MAX {
                self.line[self.line_len] = b;
                self.line_len += 1;
            } else {
                self.line_overflow = true;
            }
        }
        None
    }

    fn header(&mut self) {
        let line = &self.line[..self.line_len];
        let Some(colon) = line.iter().position(|&b| b == b':') else { return };
        let name = &line[..colon];
        let val = trim(&line[colon + 1..]);
        let is = |n: &str| name.eq_ignore_ascii_case(n.as_bytes());
        if is("content-length") {
            self.content_length = parse_u64(val);
        } else if is("transfer-encoding") {
            self.chunked = contains_ci(val, b"chunked");
        } else if is("connection") {
            self.close = contains_ci(val, b"close");
        } else if is("retry-after") {
            self.retry_after = parse_u64(val).unwrap_or(0) as u32;
        } else if is("x-rate-limit-reset") {
            self.rate_reset = parse_u64(val).unwrap_or(0);
        } else if is("content-type") {
            self.sse = contains_ci(val, b"text/event-stream");
            self.ctype = if contains_ci(val, b"html") {
                1
            } else if contains_ci(val, b"text/plain") || contains_ci(val, b"text/markdown") {
                2
            } else {
                3
            };
        } else if is("content-encoding") {
            self.encoded = !val.eq_ignore_ascii_case(b"identity");
        } else if is("location") {
            self.location = val.to_vec();
        }
    }

    /// Feeds bytes. Body bytes go to `body` with the response status. Returns how many bytes were used: fewer than
    /// `data.len()` only when the response ended inside this chunk.
    pub fn feed(&mut self, data: &[u8], body: &mut dyn FnMut(u16, u8, &[u8])) -> Result<usize, HttpError> {
        let mut i = 0usize;
        while i < data.len() {
            match self.st {
                St::Done => return Ok(i),
                St::StatusLine => {
                    let Some(next) = self.take_line(data, i) else { return Ok(data.len()) };
                    i = next;
                    let line = &self.line[..self.line_len];
                    if line.is_empty() {
                        continue; // tolerate a stray crlf between responses
                    }
                    if line.len() < 12 || !line.starts_with(b"HTTP/1.") {
                        return Err(HttpError::BadStatusLine);
                    }
                    self.status = parse_u64(&line[9..12]).ok_or(HttpError::BadStatusLine)? as u16;
                    self.line_len = 0;
                    self.line_overflow = false;
                    self.st = St::Headers;
                }
                St::Headers => {
                    let Some(next) = self.take_line(data, i) else { return Ok(data.len()) };
                    i = next;
                    if self.line_len == 0 && !self.line_overflow {
                        if (100..200).contains(&self.status) {
                            // informational: the real response follows
                            let keep = self.status;
                            self.reset();
                            let _ = keep;
                            continue;
                        }
                        self.head_done = true;
                        self.no_body = self.status == 204 || self.status == 304;
                        self.st = if self.no_body {
                            St::Done
                        } else if self.chunked {
                            St::ChunkSize
                        } else if let Some(n) = self.content_length {
                            self.rem = n;
                            if n == 0 { St::Done } else { St::Len }
                        } else {
                            self.close = true;
                            St::UntilClose
                        };
                        self.rem = if self.st == St::Len { self.rem } else { 0 };
                        self.chunk_digits = 0;
                    } else {
                        if !self.line_overflow {
                            self.header();
                        }
                        self.line_len = 0;
                        self.line_overflow = false;
                    }
                }
                St::Len => {
                    let take = ((data.len() - i) as u64).min(self.rem) as usize;
                    body(self.status, self.ctype, &data[i..i + take]);
                    i += take;
                    self.rem -= take as u64;
                    if self.rem == 0 {
                        self.st = St::Done;
                    }
                }
                St::UntilClose => {
                    body(self.status, self.ctype, &data[i..]);
                    i = data.len();
                }
                St::ChunkSize => {
                    let b = data[i];
                    i += 1;
                    let v = match b {
                        b'0'..=b'9' => Some(b - b'0'),
                        b'a'..=b'f' => Some(b - b'a' + 10),
                        b'A'..=b'F' => Some(b - b'A' + 10),
                        _ => None,
                    };
                    match v {
                        Some(v) => {
                            if self.chunk_digits >= 15 {
                                return Err(HttpError::BadChunk);
                            }
                            self.rem = self.rem << 4 | v as u64;
                            self.chunk_digits += 1;
                        }
                        None if self.chunk_digits == 0 => return Err(HttpError::BadChunk),
                        None => {
                            if b == b'\n' {
                                self.end_chunk_size();
                            } else {
                                self.st = St::ChunkExt;
                            }
                        }
                    }
                }
                St::ChunkExt => {
                    // skip ";ext=val" and the \r up to the \n
                    let b = data[i];
                    i += 1;
                    if b == b'\n' {
                        self.end_chunk_size();
                    }
                }
                St::ChunkData => {
                    let take = ((data.len() - i) as u64).min(self.rem) as usize;
                    body(self.status, self.ctype, &data[i..i + take]);
                    i += take;
                    self.rem -= take as u64;
                    if self.rem == 0 {
                        self.st = St::ChunkDataCr;
                    }
                }
                St::ChunkDataCr => {
                    let b = data[i];
                    i += 1;
                    match b {
                        b'\r' => self.st = St::ChunkDataLf,
                        b'\n' => self.next_chunk(),
                        _ => return Err(HttpError::BadChunk),
                    }
                }
                St::ChunkDataLf => {
                    let b = data[i];
                    i += 1;
                    if b != b'\n' {
                        return Err(HttpError::BadChunk);
                    }
                    self.next_chunk();
                }
                St::Trailers => {
                    let Some(next) = self.take_line(data, i) else { return Ok(data.len()) };
                    i = next;
                    if self.line_len == 0 {
                        self.st = St::Done;
                    }
                    self.line_len = 0;
                }
            }
        }
        Ok(i)
    }

    fn end_chunk_size(&mut self) {
        if self.rem == 0 {
            self.line_len = 0;
            self.st = St::Trailers;
        } else {
            self.st = St::ChunkData;
        }
    }

    fn next_chunk(&mut self) {
        self.rem = 0;
        self.chunk_digits = 0;
        self.st = St::ChunkSize;
    }

    /// The connection closed. Fine for a read-until-close body, an error otherwise.
    pub fn eof(&mut self) -> Result<(), HttpError> {
        match self.st {
            St::Done => Ok(()),
            St::UntilClose => {
                self.st = St::Done;
                Ok(())
            }
            _ => Err(HttpError::ClosedEarly),
        }
    }
}

fn trim(b: &[u8]) -> &[u8] {
    let s = b.iter().position(|c| !c.is_ascii_whitespace()).unwrap_or(b.len());
    let e = b.iter().rposition(|c| !c.is_ascii_whitespace()).map_or(s, |p| p + 1);
    &b[s..e]
}

fn parse_u64(b: &[u8]) -> Option<u64> {
    let b = trim(b);
    if b.is_empty() || b.len() > 19 {
        return None;
    }
    let mut v = 0u64;
    for &c in b {
        if !c.is_ascii_digit() {
            return None;
        }
        v = v * 10 + (c - b'0') as u64;
    }
    Some(v)
}

fn contains_ci(hay: &[u8], needle: &[u8]) -> bool {
    hay.windows(needle.len()).any(|w| w.eq_ignore_ascii_case(needle))
}

/// Percent-encodes a query parameter value.
pub fn push_query(out: &mut Vec<u8>, s: &str) {
    const H: &[u8; 16] = b"0123456789ABCDEF";
    for &b in s.as_bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
            out.push(b);
        } else {
            out.push(b'%');
            out.push(H[(b >> 4) as usize]);
            out.push(H[(b & 15) as usize]);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(chunks: &[&[u8]]) -> (Response, Vec<u8>, usize) {
        let mut r = Response::new();
        let mut body = Vec::new();
        let mut used = 0;
        for c in chunks {
            used = r.feed(c, &mut |_, _, b| body.extend_from_slice(b)).unwrap();
        }
        (r, body, used)
    }

    #[test]
    fn content_length() {
        let raw = b"HTTP/1.1 200 OK\r\nContent-Length: 5\r\nX-Rate-Limit-Reset: 1700000000\r\n\r\nhelloEXTRA";
        let (r, body, used) = parse(&[raw]);
        assert!(r.done());
        assert_eq!(body, b"hello");
        assert_eq!(used, raw.len() - 5);
        assert_eq!(r.rate_reset, 1_700_000_000);
    }

    #[test]
    fn chunked_in_every_split() {
        let raw: &[u8] = b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nContent-Type: text/event-stream\r\n\r\n5;x=y\r\nhello\r\n7\r\n, world\r\n0\r\nTrailer: v\r\n\r\n";
        for size in 1..raw.len() {
            let chunks: Vec<&[u8]> = raw.chunks(size).collect();
            let (r, body, _) = parse(&chunks);
            assert!(r.done(), "size {size}");
            assert!(r.sse);
            assert_eq!(body, b"hello, world", "size {size}");
        }
    }

    #[test]
    fn continue_then_429() {
        let raw = b"HTTP/1.1 100 Continue\r\n\r\nHTTP/1.1 429 Too Many Requests\r\nRetry-After: 30\r\nContent-Length: 2\r\n\r\n{}";
        let (r, body, _) = parse(&[raw]);
        assert_eq!(r.status, 429);
        assert_eq!(r.retry_after, 30);
        assert_eq!(body, b"{}");
    }

    #[test]
    fn until_close_and_errors() {
        let mut r = Response::new();
        let mut body = Vec::new();
        r.feed(b"HTTP/1.1 200 OK\r\n\r\nabc", &mut |_, _, b| body.extend_from_slice(b)).unwrap();
        assert!(r.eof().is_ok());
        assert_eq!(body, b"abc");
        let mut r = Response::new();
        assert!(r.feed(b"garbage\r\n", &mut |_, _, _| {}).is_err());
        let mut r = Response::new();
        r.feed(b"HTTP/1.1 200 OK\r\nContent-Length: 9\r\n\r\nabc", &mut |_, _, _| {}).unwrap();
        assert_eq!(r.eof(), Err(HttpError::ClosedEarly));
    }

    #[test]
    fn request_bytes() {
        let mut out = Vec::new();
        write_request(&mut out, "POST", "api.x.com", b"/2/tweets", &[("Authorization", b"Bearer t")], b"{}");
        let s = String::from_utf8(out).unwrap();
        assert!(s.starts_with("POST /2/tweets HTTP/1.1\r\nHost: api.x.com\r\n"));
        assert!(s.contains("Content-Length: 2\r\n"));
        assert!(s.ends_with("Authorization: Bearer t\r\n\r\n{}"));
    }
}
