//! Rows to terminal cells. Two buffers, a row-hash diff, and one write per frame.
//! Nothing in the steady-state path allocates: every buffer is sized at start or resize.

use crate::text::{graphemes, next_line, utf8_prefix};

pub const BOLD: u8 = 1;
pub const DIM: u8 = 2;
pub const ITALIC: u8 = 4;
pub const UNDER: u8 = 8;
pub const REVERSE: u8 = 16;

/// Palette slots. A cell holds a slot index; the theme maps slots to rgb.
pub mod color {
    pub const BG: u8 = 0;
    pub const FG: u8 = 1;
    pub const MUTED: u8 = 2;
    pub const ACCENT: u8 = 3;
    pub const AUTHOR: u8 = 4;
    pub const LINK: u8 = 5;
    pub const OK: u8 = 6;
    pub const WARN: u8 = 7;
    pub const ERR: u8 = 8;
    pub const BAR_BG: u8 = 9;
    pub const BAR_FG: u8 = 10;
    pub const SEL_BG: u8 = 11;
    pub const USER: u8 = 12;
    pub const BOT: u8 = 13;
    pub const BORDER: u8 = 14;
    pub const CODE: u8 = 15;
    pub const BODY: u8 = 16;
    pub const FAINT: u8 = 17;
    pub const SURFACE: u8 = 18;
    pub const X_DOT: u8 = 19;
    pub const YT: u8 = 20;
    pub const CLAUDE: u8 = 21;
    pub const GPT: u8 = 22;
    pub const WEB: u8 = 23;
    pub const ACCENT_BG: u8 = 24;
    pub const CODE_BG: u8 = 25;
    pub const FOCUS_BG: u8 = 26;
    pub const HISTORY: u8 = 27;
}

pub const SLOTS: usize = 32;

/// The theme: near-black neutrals, one warm accent, and a color per service.
pub const THEME: [(&str, [u8; 3]); SLOTS] = [
    ("bg", [0x0a, 0x0a, 0x0b]),
    ("fg", [0xed, 0xed, 0xee]),
    ("muted", [0x8a, 0x8a, 0x93]),
    ("accent", [0xff, 0x6b, 0x1a]),
    ("author", [0xf4, 0xf4, 0xf5]),
    ("link", [0x7f, 0xb0, 0xff]),
    ("ok", [0x3e, 0xcf, 0x8e]),
    ("warn", [0xf5, 0xb8, 0x3d]),
    ("err", [0xff, 0x5a, 0x5f]),
    ("bar_bg", [0x11, 0x11, 0x13]),
    ("bar_fg", [0xa3, 0xa3, 0xab]),
    ("sel_bg", [0x1a, 0x1a, 0x1e]),
    ("user", [0xed, 0xed, 0xee]),
    ("bot", [0xd9, 0x77, 0x57]),
    ("border", [0x26, 0x26, 0x2b]),
    ("code", [0xe8, 0xc0, 0x7d]),
    ("body", [0xcd, 0xcd, 0xd3]),
    ("faint", [0x56, 0x56, 0x5e]),
    ("surface", [0x14, 0x14, 0x16]),
    ("x", [0xed, 0xed, 0xee]),
    ("youtube", [0xff, 0x4b, 0x4b]),
    ("claude", [0xd9, 0x77, 0x57]),
    ("chatgpt", [0x10, 0xa3, 0x7f]),
    ("web", [0x7f, 0xb0, 0xff]),
    ("accent_bg", [0x3a, 0x1d, 0x0b]),
    ("code_bg", [0x12, 0x12, 0x14]),
    ("focus_bg", [0x22, 0x30, 0x4a]),
    ("history", [0xb4, 0x8c, 0xff]),
    ("spare28", [0x0a, 0x0a, 0x0b]),
    ("spare29", [0x0a, 0x0a, 0x0b]),
    ("spare30", [0x0a, 0x0a, 0x0b]),
    ("spare31", [0x0a, 0x0a, 0x0b]),
];

pub fn default_palette() -> [[u8; 3]; SLOTS] {
    let mut p = [[0u8; 3]; SLOTS];
    for (i, (_, c)) in THEME.iter().enumerate() {
        p[i] = *c;
    }
    p
}

/// One terminal cell, 8 bytes: a code point or grapheme handle, two palette indexes, style.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(C)]
pub struct Cell {
    /// 21-bit code point, or GRAPHEME|handle for multi-codepoint clusters,
    /// or WIDE_TAIL for the second cell of a wide cluster.
    pub ch: u32,
    pub fg: u8,
    pub bg: u8,
    pub attr: u8,
    pub _pad: u8,
}

const _: () = assert!(core::mem::size_of::<Cell>() == 8);

pub const GRAPHEME: u32 = 1 << 31;
pub const WIDE_TAIL: u32 = 0;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Style {
    pub fg: u8,
    pub bg: u8,
    pub attr: u8,
}

impl Style {
    pub const fn new(fg: u8, bg: u8, attr: u8) -> Self {
        Style { fg, bg, attr }
    }
    pub const fn fg(fg: u8) -> Self {
        Style { fg, bg: color::BG, attr: 0 }
    }
    pub const fn with(self, attr: u8) -> Self {
        Style { attr: self.attr | attr, ..self }
    }
    pub const fn on(self, bg: u8) -> Self {
        Style { bg, ..self }
    }
}

pub const BLANK: Cell = Cell { ch: ' ' as u32, fg: color::BODY, bg: color::BG, attr: 0, _pad: 0 };

/// Multi-codepoint clusters live here, deduplicated, so a cell stays 8 bytes.
/// When full it is cleared and the next frame is a full redraw.
pub struct GraphemePool {
    bytes: Box<[u8]>,
    used: usize,
    entries: Box<[(u32, u16)]>,
    count: usize,
    index: Box<[u32]>, // open addressing: entry+1, 0 = empty
    pub generation: u32,
}

const POOL_BYTES: usize = 32 * 1024;
const POOL_ENTRIES: usize = 2048;
const POOL_INDEX: usize = 4096;

impl GraphemePool {
    pub fn new() -> Self {
        GraphemePool {
            bytes: vec![0u8; POOL_BYTES].into_boxed_slice(),
            used: 0,
            entries: vec![(0u32, 0u16); POOL_ENTRIES].into_boxed_slice(),
            count: 0,
            index: vec![0u32; POOL_INDEX].into_boxed_slice(),
            generation: 0,
        }
    }

    fn hash(s: &[u8]) -> usize {
        let mut h: u32 = 0x811c9dc5;
        for &b in s {
            h = (h ^ b as u32).wrapping_mul(0x01000193);
        }
        h as usize
    }

    pub fn intern(&mut self, s: &[u8]) -> u32 {
        if s.len() > 255 {
            return '\u{FFFD}' as u32;
        }
        let mut slot = Self::hash(s) % POOL_INDEX;
        loop {
            let e = self.index[slot];
            if e == 0 {
                break;
            }
            let (off, len) = self.entries[(e - 1) as usize];
            if &self.bytes[off as usize..off as usize + len as usize] == s {
                return GRAPHEME | (e - 1);
            }
            slot = (slot + 1) % POOL_INDEX;
        }
        if self.count >= POOL_ENTRIES || self.used + s.len() > POOL_BYTES || self.count * 2 >= POOL_INDEX {
            self.clear();
            return self.intern(s);
        }
        let off = self.used;
        self.bytes[off..off + s.len()].copy_from_slice(s);
        self.used += s.len();
        self.entries[self.count] = (off as u32, s.len() as u16);
        self.count += 1;
        self.index[slot] = self.count as u32;
        GRAPHEME | (self.count as u32 - 1)
    }

    pub fn get(&self, h: u32) -> &[u8] {
        let (off, len) = self.entries[(h & !GRAPHEME) as usize];
        &self.bytes[off as usize..off as usize + len as usize]
    }

    fn clear(&mut self) {
        self.used = 0;
        self.count = 0;
        self.index.fill(0);
        self.generation = self.generation.wrapping_add(1);
    }
}

/// A grid of cells the views paint into.
pub struct Grid {
    pub w: u16,
    pub h: u16,
    pub cells: Vec<Cell>,
}

impl Grid {
    pub fn new(w: u16, h: u16) -> Self {
        Grid { w, h, cells: vec![BLANK; w as usize * h as usize] }
    }

    pub fn resize(&mut self, w: u16, h: u16) {
        self.w = w;
        self.h = h;
        self.cells.clear();
        self.cells.resize(w as usize * h as usize, BLANK);
    }

    pub fn clear(&mut self) {
        self.cells.fill(BLANK);
    }

    #[inline]
    pub fn row(&self, y: u16) -> &[Cell] {
        let w = self.w as usize;
        &self.cells[y as usize * w..(y as usize + 1) * w]
    }

    #[inline]
    pub fn set(&mut self, x: u16, y: u16, c: Cell) {
        if x < self.w && y < self.h {
            self.cells[y as usize * self.w as usize + x as usize] = c;
        }
    }

    pub fn fill(&mut self, y: u16, x0: u16, x1: u16, ch: char, st: Style) {
        if y >= self.h {
            return;
        }
        let c = Cell { ch: ch as u32, fg: st.fg, bg: st.bg, attr: st.attr, _pad: 0 };
        for x in x0..x1.min(self.w) {
            self.set(x, y, c);
        }
    }

    pub fn fill_rect(&mut self, x0: u16, y0: u16, x1: u16, y1: u16, st: Style) {
        for y in y0..y1.min(self.h) {
            self.fill(y, x0, x1, ' ', st);
        }
    }

    /// Paints one line of text from x up to (not including) max_x. Returns the column
    /// after the last cell written. Text that does not fit is cut with an ellipsis.
    pub fn text(&mut self, pool: &mut GraphemePool, x: u16, y: u16, max_x: u16, s: &str, st: Style) -> u16 {
        let max_x = max_x.min(self.w);
        if y >= self.h || x >= max_x {
            return x;
        }
        let mut col = x;
        for g in graphemes(s) {
            let piece = &s[g.start..g.end];
            let b = piece.as_bytes()[0];
            let (ch, w) = if b == b'\t' || b == b'\n' || b == b'\r' {
                (' ' as u32, 1)
            } else if g.width == 0 {
                continue;
            } else if piece.len() == 1 || piece.chars().nth(1).is_none() {
                (piece.chars().next().unwrap() as u32, g.width as u16)
            } else {
                (pool.intern(piece.as_bytes()), g.width as u16)
            };
            if col + w > max_x {
                // cut: the last cell of the box becomes an ellipsis. if that cell is the
                // tail of a wide cluster, the ellipsis replaces the cluster's head instead.
                let mk = |ch: char| Cell { ch: ch as u32, fg: st.fg, bg: st.bg, attr: st.attr, _pad: 0 };
                let at = max_x - 1;
                let idx = y as usize * self.w as usize + at as usize;
                if at > x && col > at && self.cells[idx].ch == WIDE_TAIL {
                    self.set(at - 1, y, mk('…'));
                    self.set(at, y, mk(' '));
                } else {
                    self.set(at, y, mk('…'));
                }
                return max_x;
            }
            self.set(col, y, Cell { ch, fg: st.fg, bg: st.bg, attr: st.attr, _pad: 0 });
            if w == 2 {
                self.set(col + 1, y, Cell { ch: WIDE_TAIL, fg: st.fg, bg: st.bg, attr: st.attr, _pad: 0 });
            }
            col += w;
        }
        col
    }

    /// Word-wraps text into the box. Paints at most max_rows rows starting at y and
    /// returns how many rows the whole text needs (so it also measures when y is off screen).
    pub fn wrap(&mut self, pool: &mut GraphemePool, x: u16, y: i32, width: u16, max_rows: u16, s: &str, st: Style) -> u16 {
        let mut pos = 0usize;
        let mut rows = 0u16;
        while pos < s.len() || rows == 0 {
            let (end, next) = next_line(s, pos, width as usize);
            let line_y = y + rows as i32;
            if rows < max_rows && line_y >= 0 && line_y < self.h as i32 {
                self.text(pool, x, line_y as u16, x + width, &s[pos..end], st);
            }
            rows += 1;
            if next >= s.len() {
                break;
            }
            pos = next;
            if rows >= max_rows && max_rows != u16::MAX {
                break;
            }
        }
        rows
    }
}

/// Counts wrapped rows without painting.
pub fn measure(s: &str, width: u16) -> u16 {
    let mut pos = 0usize;
    let mut rows = 0u16;
    loop {
        let (_, next) = next_line(s, pos, width as usize);
        rows += 1;
        if next >= s.len() {
            return rows;
        }
        pos = next;
    }
}

pub fn as_str(b: &[u8]) -> &str {
    utf8_prefix(b)
}

/// The front and back buffers plus everything needed to turn their difference into
/// escape sequences.
pub struct Screen {
    pub back: Grid,
    front: Grid,
    front_hash: Vec<u64>,
    pub pool: GraphemePool,
    pool_gen: u32,
    pub out: Vec<u8>,
    pub palette: [[u8; 3]; SLOTS],
    force_full: bool,
    pub cursor: Option<(u16, u16)>,
    /// cells written last frame, for tests and :mem
    pub last_cells_written: u32,
    pub last_bytes: u32,
}

const OUT_CAP: usize = 512 * 1024;

impl Screen {
    pub fn new(w: u16, h: u16, palette: [[u8; 3]; SLOTS]) -> Self {
        Screen {
            back: Grid::new(w, h),
            front: Grid::new(w, h),
            front_hash: vec![0; h as usize],
            pool: GraphemePool::new(),
            pool_gen: 0,
            out: Vec::with_capacity(OUT_CAP),
            palette,
            force_full: true,
            cursor: None,
            last_cells_written: 0,
            last_bytes: 0,
        }
    }

    pub fn resize(&mut self, w: u16, h: u16) {
        self.back.resize(w, h);
        self.front.resize(w, h);
        self.front_hash.clear();
        self.front_hash.resize(h as usize, 0);
        self.force_full = true;
    }

    pub fn invalidate(&mut self) {
        self.force_full = true;
    }

    /// Start a frame: clear the back buffer.
    pub fn begin(&mut self) {
        self.back.clear();
        self.cursor = None;
    }

    fn row_hash(row: &[Cell]) -> u64 {
        // fnv-1a over the 8-byte cells, read as u64
        let mut h: u64 = 0xcbf29ce484222325;
        for c in row {
            let v = c.ch as u64 | (c.fg as u64) << 32 | (c.bg as u64) << 40 | (c.attr as u64) << 48;
            h = (h ^ v).wrapping_mul(0x100000001b3);
        }
        h
    }

    /// Diffs back against front and writes the escape sequences into self.out.
    /// Returns the bytes to write to the console in one call.
    pub fn present(&mut self) -> &[u8] {
        self.out.clear();
        if self.pool.generation != self.pool_gen {
            self.pool_gen = self.pool.generation;
            self.force_full = true;
        }
        let w = self.back.w;
        let h = self.back.h;
        let mut cur_style: Option<(u8, u8, u8)> = None;
        let mut cur_pos: Option<(u16, u16)> = None;
        let mut written = 0u32;
        // synchronized output and hide the cursor while drawing
        self.out.extend_from_slice(b"\x1b[?2026h\x1b[?25l");
        for y in 0..h {
            let hash = Self::row_hash(self.back.row(y));
            if !self.force_full && hash == self.front_hash[y as usize] {
                continue;
            }
            self.front_hash[y as usize] = hash;
            let mut x = 0u16;
            while x < w {
                let i = y as usize * w as usize + x as usize;
                let c = self.back.cells[i];
                if !self.force_full && c == self.front.cells[i] {
                    x += 1;
                    continue;
                }
                if c.ch == WIDE_TAIL {
                    // the head cell drew it; make sure the head is written
                    x += 1;
                    continue;
                }
                if cur_pos != Some((x, y)) {
                    self.out.extend_from_slice(b"\x1b[");
                    push_num(&mut self.out, y as u32 + 1);
                    self.out.push(b';');
                    push_num(&mut self.out, x as u32 + 1);
                    self.out.push(b'H');
                }
                let st = (c.fg, c.bg, c.attr);
                if cur_style != Some(st) {
                    self.sgr(st);
                    cur_style = Some(st);
                }
                let mut adv = 1u16;
                if c.ch & GRAPHEME != 0 {
                    let b = self.pool.get(c.ch);
                    self.out.extend_from_slice(b);
                } else {
                    let ch = char::from_u32(c.ch).unwrap_or('\u{FFFD}');
                    let mut buf = [0u8; 4];
                    self.out.extend_from_slice(ch.encode_utf8(&mut buf).as_bytes());
                }
                if x + 1 < w && self.back.cells[i + 1].ch == WIDE_TAIL {
                    adv = 2;
                }
                written += 1;
                x += adv;
                cur_pos = Some((x, y));
            }
        }
        self.out.extend_from_slice(b"\x1b[0m");
        if let Some((cx, cy)) = self.cursor {
            self.out.extend_from_slice(b"\x1b[");
            push_num(&mut self.out, cy as u32 + 1);
            self.out.push(b';');
            push_num(&mut self.out, cx as u32 + 1);
            self.out.extend_from_slice(b"H\x1b[?25h");
        }
        self.out.extend_from_slice(b"\x1b[?2026l");
        self.force_full = false;
        core::mem::swap(&mut self.front, &mut self.back);
        self.last_cells_written = written;
        self.last_bytes = self.out.len() as u32;
        &self.out
    }

    fn sgr(&mut self, (fg, bg, attr): (u8, u8, u8)) {
        let o = &mut self.out;
        o.extend_from_slice(b"\x1b[0");
        if attr & BOLD != 0 {
            o.extend_from_slice(b";1");
        }
        if attr & DIM != 0 {
            o.extend_from_slice(b";2");
        }
        if attr & ITALIC != 0 {
            o.extend_from_slice(b";3");
        }
        if attr & UNDER != 0 {
            o.extend_from_slice(b";4");
        }
        if attr & REVERSE != 0 {
            o.extend_from_slice(b";7");
        }
        let f = self.palette[(fg as usize) % SLOTS];
        let b = self.palette[(bg as usize) % SLOTS];
        o.extend_from_slice(b";38;2;");
        push_rgb(o, f);
        o.extend_from_slice(b";48;2;");
        push_rgb(o, b);
        o.push(b'm');
    }

    /// The front buffer with colors, as json rows of [text, fg, bg, attr, width], for
    /// rendering screenshots outside the terminal.
    pub fn dump_json(&self) -> String {
        let hex = |c: [u8; 3]| format!("#{:02x}{:02x}{:02x}", c[0], c[1], c[2]);
        let mut s = format!("{{\"w\":{},\"h\":{},\"rows\":[", self.front.w, self.front.h);
        for y in 0..self.front.h {
            if y > 0 {
                s.push(',');
            }
            s.push('[');
            let row = self.front.row(y);
            let mut first = true;
            for (x, c) in row.iter().enumerate() {
                if c.ch == WIDE_TAIL {
                    continue;
                }
                let text = if c.ch & GRAPHEME != 0 {
                    String::from_utf8_lossy(self.pool.get(c.ch)).into_owned()
                } else {
                    char::from_u32(c.ch).unwrap_or(' ').to_string()
                };
                let wide = row.get(x + 1).is_some_and(|n| n.ch == WIDE_TAIL);
                if !first {
                    s.push(',');
                }
                first = false;
                let mut t = Vec::new();
                crate::json::push_str(&mut t, &text);
                s.push_str(&format!(
                    "[{},\"{}\",\"{}\",{},{}]",
                    String::from_utf8_lossy(&t),
                    hex(self.palette[c.fg as usize % SLOTS]),
                    hex(self.palette[c.bg as usize % SLOTS]),
                    c.attr,
                    if wide { 2 } else { 1 }
                ));
            }
            s.push(']');
        }
        s.push_str("]}");
        s
    }

    /// The front buffer as text, one line per row. For golden-frame tests.
    pub fn front_text(&self) -> String {
        let mut s = String::new();
        for y in 0..self.front.h {
            for c in self.front.row(y) {
                if c.ch == WIDE_TAIL {
                    continue;
                }
                if c.ch & GRAPHEME != 0 {
                    s.push_str(core::str::from_utf8(self.pool.get(c.ch)).unwrap());
                } else {
                    s.push(char::from_u32(c.ch).unwrap_or('?'));
                }
            }
            while s.ends_with(' ') {
                s.pop();
            }
            s.push('\n');
        }
        s
    }
}

fn push_rgb(o: &mut Vec<u8>, c: [u8; 3]) {
    push_num(o, c[0] as u32);
    o.push(b';');
    push_num(o, c[1] as u32);
    o.push(b';');
    push_num(o, c[2] as u32);
}

/// Writes a decimal number without allocating.
pub fn push_num(o: &mut Vec<u8>, mut n: u32) {
    let mut buf = [0u8; 10];
    let mut i = buf.len();
    loop {
        i -= 1;
        buf[i] = b'0' + (n % 10) as u8;
        n /= 10;
        if n == 0 {
            break;
        }
    }
    o.extend_from_slice(&buf[i..]);
}

/// A fixed buffer that implements fmt::Write, for formatting status text in a frame
/// without allocating.
pub struct Fmt<const N: usize> {
    buf: [u8; N],
    len: usize,
}

impl<const N: usize> Fmt<N> {
    pub fn new() -> Self {
        Fmt { buf: [0; N], len: 0 }
    }
    pub fn as_str(&self) -> &str {
        utf8_prefix(&self.buf[..self.len])
    }
    pub fn clear(&mut self) {
        self.len = 0;
    }
}

impl<const N: usize> core::fmt::Write for Fmt<N> {
    fn write_str(&mut self, s: &str) -> core::fmt::Result {
        let n = s.len().min(N - self.len);
        self.buf[self.len..self.len + n].copy_from_slice(&s.as_bytes()[..n]);
        self.len += n;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pal() -> [[u8; 3]; SLOTS] {
        [[0, 0, 0]; SLOTS]
    }

    #[test]
    fn diff_writes_only_changed_cells() {
        let mut s = Screen::new(20, 3, pal());
        s.begin();
        s.back.text(&mut s.pool, 0, 0, 20, "hello", Style::fg(1));
        s.present();
        assert_eq!(s.front_text(), "hello\n\n\n");
        s.begin();
        s.back.text(&mut s.pool, 0, 0, 20, "hallo", Style::fg(1));
        s.present();
        assert_eq!(s.last_cells_written, 1);
        s.begin();
        s.back.text(&mut s.pool, 0, 0, 20, "hallo", Style::fg(1));
        s.present();
        assert_eq!(s.last_cells_written, 0);
    }

    #[test]
    fn wide_and_clusters_at_line_end() {
        let mut s = Screen::new(6, 2, pal());
        s.begin();
        // 2 + 2 + 2 = 6 fits exactly
        s.back.text(&mut s.pool, 0, 0, 6, "日本語", Style::fg(1));
        // too wide: cut with an ellipsis, wide char never split across the edge
        s.back.text(&mut s.pool, 0, 1, 6, "ab日本語", Style::fg(1));
        s.present();
        assert_eq!(s.front_text(), "日本語\nab日…\n");
    }

    #[test]
    fn emoji_zwj_cluster_is_one_cell_pair() {
        let mut s = Screen::new(10, 1, pal());
        s.begin();
        let fam = "\u{1F468}\u{200D}\u{1F469}\u{200D}\u{1F467}x";
        let end = s.back.text(&mut s.pool, 0, 0, 10, fam, Style::fg(1));
        assert_eq!(end, 3);
        s.present();
        assert_eq!(s.front_text(), format!("{fam}\n"));
    }

    #[test]
    fn wrap_counts_rows() {
        assert_eq!(measure("aaa bbb ccc", 7), 2);
        assert_eq!(measure("", 7), 1);
    }
}
