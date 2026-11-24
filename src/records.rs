//! The log's record format, platform-independent so the fuzzers can build it anywhere:
//! [length u32][crc32 u32][payload], payload = kind byte + fields.

const fn crc_table() -> [u32; 256] {
    let mut t = [0u32; 256];
    let mut i = 0;
    while i < 256 {
        let mut c = i as u32;
        let mut k = 0;
        while k < 8 {
            c = if c & 1 != 0 { 0xEDB88320 ^ (c >> 1) } else { c >> 1 };
            k += 1;
        }
        t[i] = c;
        i += 1;
    }
    t
}

static CRC: [u32; 256] = crc_table();

pub fn crc32(b: &[u8]) -> u32 {
    let mut c = !0u32;
    for &x in b {
        c = CRC[((c ^ x as u32) & 0xff) as usize] ^ (c >> 8);
    }
    !c
}

// ---------------------------------------------------------------- records

pub const REC_POST: u8 = 1;
pub const REC_MSG: u8 = 2;
pub const REC_SPEND: u8 = 3;
pub const REC_POS: u8 = 4;
pub const REC_META: u8 = 5;
pub const REC_BRANCH: u8 = 6;

/// Builds a record payload: a kind byte, then fields.
pub struct RecBuf {
    pub b: Vec<u8>,
}

impl Default for RecBuf {
    fn default() -> Self {
        Self::new()
    }
}

impl RecBuf {
    pub fn new() -> RecBuf {
        RecBuf { b: Vec::with_capacity(4096) }
    }
    pub fn start(&mut self, kind: u8) -> &mut Self {
        self.b.clear();
        self.b.push(kind);
        self
    }
    pub fn u8(&mut self, v: u8) -> &mut Self {
        self.b.push(v);
        self
    }
    pub fn u32(&mut self, v: u32) -> &mut Self {
        self.b.extend_from_slice(&v.to_le_bytes());
        self
    }
    pub fn u64(&mut self, v: u64) -> &mut Self {
        self.b.extend_from_slice(&v.to_le_bytes());
        self
    }
    pub fn str(&mut self, s: &[u8]) -> &mut Self {
        self.u32(s.len() as u32);
        self.b.extend_from_slice(s);
        self
    }
}

/// Reads fields back out of a record payload. Every read is bounds-checked: a damaged
/// record yields None, never a panic.
pub struct RecReader<'a> {
    pub b: &'a [u8],
    pub i: usize,
}

impl<'a> RecReader<'a> {
    pub fn new(b: &'a [u8]) -> Self {
        RecReader { b, i: 0 }
    }
    pub fn u8(&mut self) -> Option<u8> {
        let v = *self.b.get(self.i)?;
        self.i += 1;
        Some(v)
    }
    pub fn u32(&mut self) -> Option<u32> {
        let s = self.b.get(self.i..self.i + 4)?;
        self.i += 4;
        Some(u32::from_le_bytes(s.try_into().ok()?))
    }
    pub fn u64(&mut self) -> Option<u64> {
        let s = self.b.get(self.i..self.i + 8)?;
        self.i += 8;
        Some(u64::from_le_bytes(s.try_into().ok()?))
    }
    pub fn str(&mut self) -> Option<&'a [u8]> {
        let n = self.u32()? as usize;
        let s = self.b.get(self.i..self.i.checked_add(n)?)?;
        self.i += n;
        Some(s)
    }
}

/// Iterates the valid records of a log image. Stops at the first damaged record.
pub struct Records<'a> {
    b: &'a [u8],
    pub pos: usize,
}

pub fn records(b: &[u8]) -> Records<'_> {
    Records { b, pos: 0 }
}

impl<'a> Iterator for Records<'a> {
    /// (offset of the record, payload)
    type Item = (usize, &'a [u8]);
    fn next(&mut self) -> Option<Self::Item> {
        let at = self.pos;
        let hdr = self.b.get(at..at + 8)?;
        let len = u32::from_le_bytes(hdr[0..4].try_into().unwrap()) as usize;
        let crc = u32::from_le_bytes(hdr[4..8].try_into().unwrap());
        if len == 0 || len > 16 * 1024 * 1024 {
            return None;
        }
        let payload = self.b.get(at + 8..(at + 8).checked_add(len)?)?;
        if crc32(payload) != crc {
            return None;
        }
        self.pos = at + 8 + len;
        Some((at, payload))
    }
}

