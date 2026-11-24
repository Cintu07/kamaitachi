//! Where text and rows live.
//!
//! - `TextArena`: one per tab, 1 mb of reserved address space committed in 64 kb steps.
//!   The net thread is its only writer: post text is copied there from the network buffer
//!   exactly once, already unescaped. The ui thread reads up to the published length.
//! - `Post`, `Msg`: fixed-size rows that point into an arena with spans. A feed is a struct
//!   of arrays of these fields on the ui side.
//! - `Log`: the append-only, checksummed, memory-mapped history per service.

use core::cell::UnsafeCell;
use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};

use crate::arena::{self, ArenaId, TAB_TEXT_BUDGET};
use crate::sys::{Mapped, Reserved};
pub use crate::records::*;

#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
pub struct Span {
    pub off: u32,
    pub len: u32,
}

impl Span {
    pub const EMPTY: Span = Span { off: 0, len: 0 };
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }
}

// ---------------------------------------------------------------- text arena

/// An append-only byte arena with one writer thread and one reader thread.
pub struct TextArena {
    mem: UnsafeCell<Reserved>,
    /// bytes the writer has written (writer-only)
    len: UnsafeCell<u32>,
    /// bytes the reader may read
    published: AtomicU32,
    committed: AtomicU32,
    budget: u32,
}

// SAFETY: the writer side (`push`, `publish`, `decommit`) is only called by the net
// thread; the reader side (`get`) only reads bytes below `published`, which are never
// written again until a decommit, which the ui requests only after it stops reading.
unsafe impl Sync for TextArena {}
unsafe impl Send for TextArena {}

impl TextArena {
    pub fn new() -> TextArena {
        Self::with_budget(TAB_TEXT_BUDGET)
    }

    pub fn with_budget(budget: usize) -> TextArena {
        TextArena {
            mem: UnsafeCell::new(Reserved::new(budget)),
            len: UnsafeCell::new(0),
            published: AtomicU32::new(0),
            committed: AtomicU32::new(0),
            budget: budget as u32,
        }
    }

    /// Writer: current end of written bytes.
    pub fn writer_len(&self) -> u32 {
        unsafe { *self.len.get() }
    }

    /// Writer: appends bytes. Returns false (writing nothing) past the budget.
    pub fn push(&self, b: &[u8]) -> bool {
        unsafe {
            let len = &mut *self.len.get();
            let mem = &mut *self.mem.get();
            let end = *len as usize + b.len();
            if end > self.budget as usize {
                return false;
            }
            let before = mem.committed();
            if !mem.ensure(end) {
                return false;
            }
            let after = mem.committed();
            if after != before {
                arena::add(ArenaId::TabText, (after - before) as isize);
                self.committed.store(after as u32, Ordering::Relaxed);
            }
            core::ptr::copy_nonoverlapping(b.as_ptr(), mem.base().add(*len as usize), b.len());
            *len = end as u32;
            true
        }
    }

    /// Writer: appends and returns the span.
    pub fn append(&self, b: &[u8]) -> Option<Span> {
        let off = self.writer_len();
        if self.push(b) { Some(Span { off, len: b.len() as u32 }) } else { None }
    }

    /// Writer: reads back bytes it has written (published or not).
    pub fn peek(&self, s: Span) -> &[u8] {
        let end = s.off as u64 + s.len as u64;
        if end > self.writer_len() as u64 {
            return &[];
        }
        unsafe { core::slice::from_raw_parts((*self.mem.get()).base().add(s.off as usize), s.len as usize) }
    }

    /// Writer: drops bytes written after `to` (a record that turned out to be a
    /// duplicate). Rows already sent never point past `to`.
    pub fn truncate(&self, to: u32) {
        unsafe {
            let len = &mut *self.len.get();
            if to < *len {
                *len = to;
            }
        }
    }

    /// Writer: its own bytes as text.
    pub fn str_writer(&self, s: Span) -> &str {
        crate::text::utf8_prefix(self.peek(s))
    }

    /// Writer: makes everything written so far visible to the reader.
    pub fn publish(&self) {
        self.published.store(self.writer_len(), Ordering::Release);
    }

    /// Writer: drops every byte and returns the memory to the os.
    pub fn decommit(&self) {
        unsafe {
            let mem = &mut *self.mem.get();
            arena::add(ArenaId::TabText, -(mem.committed() as isize));
            mem.decommit();
            *self.len.get() = 0;
        }
        self.committed.store(0, Ordering::Relaxed);
        self.published.store(0, Ordering::Release);
    }

    /// Reader: the bytes of a span, or empty if it is not published yet.
    pub fn get(&self, s: Span) -> &[u8] {
        let pubd = self.published.load(Ordering::Acquire);
        let end = s.off as u64 + s.len as u64;
        if end > pubd as u64 {
            return &[];
        }
        unsafe { core::slice::from_raw_parts((*self.mem.get()).base().add(s.off as usize), s.len as usize) }
    }

    pub fn str(&self, s: Span) -> &str {
        crate::text::utf8_prefix(self.get(s))
    }

    pub fn committed(&self) -> usize {
        self.committed.load(Ordering::Relaxed) as usize
    }

    pub fn published(&self) -> u32 {
        self.published.load(Ordering::Acquire)
    }
}

impl Default for TextArena {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for TextArena {
    fn drop(&mut self) {
        arena::add(ArenaId::TabText, -(self.mem.get_mut().committed() as isize));
    }
}

// ---------------------------------------------------------------- rows

pub const F_LIKED: u16 = 1;
pub const F_HAS_MEDIA: u16 = 2;
pub const F_SHOW_MEDIA: u16 = 4;
pub const F_FROM_CACHE: u16 = 8;
pub const F_ROOT: u16 = 16;
pub const F_VIDEO: u16 = 32;
pub const F_LOCAL_HIT: u16 = 64;

/// One entry of a feed or thread: a post, a video, a comment or a search hit.
#[derive(Clone, Copy, Default, Debug)]
pub struct Post {
    /// the service's id as text (x post id, youtube video or comment id)
    pub sid: Span,
    /// index into the author table
    pub author: u32,
    /// unix seconds
    pub ts: u32,
    pub text: Span,
    /// media url, video length, or the video a comment belongs to
    pub extra: Span,
    /// likes or views
    pub metric: u32,
    /// resume position in seconds for videos
    pub position: u32,
    pub flags: u16,
    pub depth: u8,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(u8)]
pub enum Role {
    User = 0,
    Assistant = 1,
    Note = 2,
}

/// One block of a web page.
#[derive(Clone, Copy, Debug, Default)]
pub struct PageBlock {
    pub kind: u8,
    pub depth: u8,
    pub text: Span,
    pub first_link: u32,
    pub links: u32,
}

/// A link: a range of a block's text and where it goes.
#[derive(Clone, Copy, Debug, Default)]
pub struct PageLink {
    pub start: u32,
    pub len: u32,
    pub href: Span,
}

/// One chat message.
#[derive(Clone, Copy, Debug)]
pub struct Msg {
    pub role: Role,
    pub provider: u8,
    pub text: Span,
    pub model: Span,
    pub cost_micros: u32,
}

/// Interns a service id (a 19-digit x id, an 11-char youtube id) into a 4-byte handle.
/// Lives on the net thread.
pub struct Interner {
    bytes: Vec<u8>,
    entries: Vec<(u32, u16)>,
    index: Vec<u32>,
}

impl Default for Interner {
    fn default() -> Self {
        Self::new()
    }
}

impl Interner {
    pub fn new() -> Interner {
        Interner { bytes: Vec::with_capacity(64 * 1024), entries: Vec::with_capacity(4096), index: vec![0; 8192] }
    }

    fn hash(s: &[u8]) -> usize {
        let mut h: u64 = 0xcbf29ce484222325;
        for &b in s {
            h = (h ^ b as u64).wrapping_mul(0x100000001b3);
        }
        h as usize
    }

    pub fn get(&self, s: &[u8]) -> Option<u32> {
        let mask = self.index.len() - 1;
        let mut slot = Self::hash(s) & mask;
        loop {
            let e = self.index[slot];
            if e == 0 {
                return None;
            }
            let (off, len) = self.entries[(e - 1) as usize];
            if &self.bytes[off as usize..off as usize + len as usize] == s {
                return Some(e - 1);
            }
            slot = (slot + 1) & mask;
        }
    }

    pub fn intern(&mut self, s: &[u8]) -> u32 {
        if let Some(h) = self.get(s) {
            return h;
        }
        if (self.entries.len() + 1) * 2 > self.index.len() {
            self.grow();
        }
        let off = self.bytes.len() as u32;
        self.bytes.extend_from_slice(s);
        self.entries.push((off, s.len() as u16));
        let h = self.entries.len() as u32;
        let mask = self.index.len() - 1;
        let mut slot = Self::hash(s) & mask;
        while self.index[slot] != 0 {
            slot = (slot + 1) & mask;
        }
        self.index[slot] = h;
        h - 1
    }

    fn grow(&mut self) {
        let n = self.index.len() * 2;
        self.index = vec![0; n];
        let mask = n - 1;
        for (i, &(off, len)) in self.entries.iter().enumerate() {
            let mut slot = Self::hash(&self.bytes[off as usize..off as usize + len as usize]) & mask;
            while self.index[slot] != 0 {
                slot = (slot + 1) & mask;
            }
            self.index[slot] = i as u32 + 1;
        }
    }

    pub fn resolve(&self, h: u32) -> &[u8] {
        let (off, len) = self.entries[h as usize];
        &self.bytes[off as usize..off as usize + len as usize]
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn bytes(&self) -> usize {
        self.bytes.capacity() + self.entries.capacity() * 8 + self.index.len() * 4
    }
}

/// A short stable hash for feed keys like "x:home".
pub fn key_hash(s: &str) -> u32 {
    let mut h: u32 = 0x811c9dc5;
    for &b in s.as_bytes() {
        h = (h ^ b as u32).wrapping_mul(0x01000193);
    }
    h
}

// ---------------------------------------------------------------- log

/// One service's history: length-prefixed, checksummed records appended to one file.
/// A crash can only lose the record being written; opening truncates a torn tail.
pub struct Log {
    pub path: PathBuf,
    file: File,
    pub len: u64,
}

impl Log {
    pub fn open(path: &Path) -> std::io::Result<Log> {
        let mut file = OpenOptions::new().read(true).write(true).create(true).truncate(false).open(path)?;
        let mut all = Vec::new();
        file.read_to_end(&mut all)?;
        let mut it = records(&all);
        for _ in it.by_ref() {}
        let good = it.pos as u64;
        if good != all.len() as u64 {
            // torn or damaged tail: drop it
            file.set_len(good)?;
        }
        file.seek(SeekFrom::Start(good))?;
        Ok(Log { path: path.to_path_buf(), file, len: good })
    }

    pub fn append(&mut self, payload: &[u8]) -> std::io::Result<()> {
        let mut hdr = [0u8; 8];
        hdr[0..4].copy_from_slice(&(payload.len() as u32).to_le_bytes());
        hdr[4..8].copy_from_slice(&crc32(payload).to_le_bytes());
        // one write per record so a crash tears at most this record
        let mut buf = Vec::with_capacity(8 + payload.len());
        buf.extend_from_slice(&hdr);
        buf.extend_from_slice(payload);
        self.file.write_all(&buf)?;
        self.len += buf.len() as u64;
        Ok(())
    }

    /// A read-only mapped view of everything written so far.
    pub fn map(&self) -> std::io::Result<Option<Mapped>> {
        self.file.sync_data().ok();
        Mapped::open(&self.file)
    }
}

// ---------------------------------------------------------------- search

/// Finds `needle` (ascii case-insensitive) in `hay`. Uses sse2 to test 16 bytes at a
/// time for the first byte, then confirms.
pub fn find_ci(hay: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() {
        return Some(0);
    }
    if hay.len() < needle.len() {
        return None;
    }
    let lo = needle[0].to_ascii_lowercase();
    let up = needle[0].to_ascii_uppercase();
    let last = hay.len() - needle.len();
    let mut i = 0usize;
    #[cfg(target_arch = "x86_64")]
    unsafe {
        use core::arch::x86_64::*;
        let vlo = _mm_set1_epi8(lo as i8);
        let vup = _mm_set1_epi8(up as i8);
        while i + 16 <= last + 1 {
            let chunk = _mm_loadu_si128(hay.as_ptr().add(i) as *const __m128i);
            let m = _mm_or_si128(_mm_cmpeq_epi8(chunk, vlo), _mm_cmpeq_epi8(chunk, vup));
            let mut bits = _mm_movemask_epi8(m) as u32;
            while bits != 0 {
                let k = bits.trailing_zeros() as usize;
                let at = i + k;
                if hay[at..at + needle.len()].eq_ignore_ascii_case(needle) {
                    return Some(at);
                }
                bits &= bits - 1;
            }
            i += 16;
        }
    }
    while i <= last {
        let b = hay[i];
        if (b == lo || b == up) && hay[i..i + needle.len()].eq_ignore_ascii_case(needle) {
            return Some(i);
        }
        i += 1;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn arena_publishes_and_budgets() {
        let a = TextArena::with_budget(128 * 1024);
        let s = a.append(b"hello").unwrap();
        assert_eq!(a.get(s), b""); // not published yet
        a.publish();
        assert_eq!(a.get(s), b"hello");
        assert!(a.append(&vec![0u8; 200 * 1024]).is_none());
        assert_eq!(a.committed(), 64 * 1024);
        a.decommit();
        assert_eq!(a.committed(), 0);
        assert_eq!(a.get(s), b"");
    }

    #[test]
    fn interner_roundtrip() {
        let mut i = Interner::new();
        let ids: Vec<String> = (0..10_000).map(|n| format!("17{n:017}")).collect();
        let hs: Vec<u32> = ids.iter().map(|s| i.intern(s.as_bytes())).collect();
        for (s, h) in ids.iter().zip(&hs) {
            assert_eq!(i.intern(s.as_bytes()), *h);
            assert_eq!(i.resolve(*h), s.as_bytes());
        }
    }

    #[test]
    fn log_survives_a_torn_tail() {
        let dir = std::env::temp_dir().join(format!("hayate-log-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("t.log");
        let _ = std::fs::remove_file(&p);
        {
            let mut l = Log::open(&p).unwrap();
            let mut r = RecBuf::new();
            r.start(REC_META).str(b"k").str(b"v1");
            l.append(&r.b).unwrap();
            r.start(REC_META).str(b"k").str(b"v2");
            l.append(&r.b).unwrap();
        }
        // simulate a crash mid-write: half a record at the end
        let full = std::fs::metadata(&p).unwrap().len();
        {
            let mut f = OpenOptions::new().append(true).open(&p).unwrap();
            f.write_all(&[50, 0, 0, 0, 1, 2, 3, 4, 9, 9]).unwrap();
        }
        let l = Log::open(&p).unwrap();
        assert_eq!(l.len, full);
        let m = l.map().unwrap().unwrap();
        let vals: Vec<Vec<u8>> = records(m.bytes())
            .map(|(_, p)| {
                let mut r = RecReader::new(&p[1..]);
                r.str().unwrap();
                r.str().unwrap().to_vec()
            })
            .collect();
        assert_eq!(vals, vec![b"v1".to_vec(), b"v2".to_vec()]);
        drop(m);
        drop(l);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn simd_search() {
        let hay = b"the quick brown fox jumps over the lazy dog, The QUICK one";
        assert_eq!(find_ci(hay, b"quick"), Some(4));
        assert_eq!(find_ci(hay, b"LAZY"), Some(35));
        assert_eq!(find_ci(hay, b"cat"), None);
        assert_eq!(find_ci(&hay[44..], b"the quick"), Some(1));
        let long = vec![b'a'; 1000];
        assert_eq!(find_ci(&long, b"aab"), None);
    }
}
