//! A seeded mutation fuzzer on stable rust for the four parsers that read bytes from
//! outside: http framing, the sse splitter, the json scanner and the log reader. Each
//! starts from valid input and flips, inserts, deletes and splices bytes; the only
//! requirement is no panic and no runaway output. The cargo-fuzz targets in fuzz/ run
//! the same parsers for hours under wsl.

use hayate::json::{Paths, Scanner, Sink};
use hayate::net::http::Response;
use hayate::net::sse::{Sse, SseSink};
use hayate::records::records;

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
}

fn mutate(r: &mut Rng, seed: &[u8]) -> Vec<u8> {
    let mut v = seed.to_vec();
    for _ in 0..1 + r.next() % 8 {
        if v.is_empty() {
            v.push(r.next() as u8);
            continue;
        }
        let i = (r.next() as usize) % v.len();
        match r.next() % 5 {
            0 => v[i] = r.next() as u8,
            1 => v.insert(i, r.next() as u8),
            2 => {
                v.remove(i);
            }
            3 => {
                let j = (r.next() as usize) % v.len();
                let (a, b) = (i.min(j), i.max(j));
                let piece = v[a..b].to_vec();
                v.splice(i..i, piece);
            }
            _ => {
                const T: &[u8] = b"{}[]\":,\\\r\n0u";
                v[i] = T[(r.next() % T.len() as u64) as usize]
            }
        }
    }
    v
}

fn chunks(r: &mut Rng, v: &[u8]) -> Vec<Vec<u8>> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < v.len() {
        let n = 1 + (r.next() as usize) % 64;
        out.push(v[i..(i + n).min(v.len())].to_vec());
        i += n;
    }
    out
}

const ITERS: usize = 200_000;

#[test]
fn http_framing_survives_garbage() {
    let seeds: [&[u8]; 3] = [
        b"HTTP/1.1 200 OK\r\nContent-Length: 5\r\n\r\nhello",
        b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n5;x\r\nhello\r\n0\r\n\r\n",
        b"HTTP/1.1 100 Continue\r\n\r\nHTTP/1.1 429 X\r\nRetry-After: 9\r\n\r\nbody",
    ];
    let mut r = Rng(1);
    for k in 0..ITERS {
        let input = mutate(&mut r, seeds[k % 3]);
        let mut resp = Response::new();
        let mut total = 0usize;
        for c in chunks(&mut r, &input) {
            match resp.feed(&c, &mut |_, _, b| total += b.len()) {
                Ok(_) => {}
                Err(_) => break,
            }
        }
        let _ = resp.eof();
        assert!(total <= input.len());
    }
}

struct Null(usize);
impl SseSink for Null {
    fn begin(&mut self) {}
    fn data(&mut self, b: &[u8]) {
        self.0 += b.len();
    }
    fn dispatch(&mut self) {}
}
impl Sink for Null {
    fn field_start(&mut self, _: u16, _: bool) {}
    fn bytes(&mut self, b: &[u8]) {
        self.0 += b.len();
    }
    fn field_end(&mut self, _: u16) {}
}

#[test]
fn sse_survives_garbage() {
    let seed = b"event: x\ndata: {\"a\":1}\n\n: ping\r\ndata:one\r\ndata: two\r\n\r\n";
    let mut r = Rng(2);
    for _ in 0..ITERS {
        let input = mutate(&mut r, seed);
        let mut s = Sse::new();
        let mut n = Null(0);
        for c in chunks(&mut r, &input) {
            s.feed(&c, &mut n);
        }
        assert!(n.0 <= input.len() * 2);
    }
}

hayate::json_paths!(P {
    "data[*]" => 1, Record;
    "data[*].id" => 1, Field;
    "data[*].text" => 2, Field;
    "data[*].m.likes" => 3, Field;
    "includes.users[*].name" => 4, Field;
    "meta.next" => 5, Field;
});

#[test]
fn json_scanner_survives_garbage() {
    let seed = r#"{"data":[{"id":"1","text":"hé😀 \"q\"","m":{"likes":4}},{"skip":[1,{"a":"]}"}]}],"includes":{"users":[{"name":"x"}]},"meta":{"next":"t"}}"#.as_bytes();
    let paths = Paths::compile(P);
    let mut r = Rng(3);
    for _ in 0..ITERS {
        let input = mutate(&mut r, seed);
        let mut sc = Scanner::new(&paths);
        let mut n = Null(0);
        for c in chunks(&mut r, &input) {
            sc.feed(&c, &mut n);
        }
        sc.finish(&mut n);
        // decoding never grows output beyond 4 bytes per input byte
        assert!(n.0 <= input.len() * 4);
    }
}

#[test]
fn log_reader_survives_garbage() {
    let mut seed = Vec::new();
    for payload in [&b"\x01abc"[..], b"\x02hello world", b"\x05k\x00\x00\x00"] {
        seed.extend_from_slice(&(payload.len() as u32).to_le_bytes());
        seed.extend_from_slice(&hayate::records::crc32(payload).to_le_bytes());
        seed.extend_from_slice(payload);
    }
    let mut r = Rng(4);
    for _ in 0..ITERS {
        let input = mutate(&mut r, &seed);
        let mut n = 0;
        for (_, p) in records(&input) {
            n += p.len();
            let _ = hayate::adapters::read_post(p);
            let _ = hayate::adapters::chat::read_msg(p);
        }
        assert!(n <= input.len());
    }
}
