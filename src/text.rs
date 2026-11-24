//! Display width and grapheme clusters, from tables generated at build time.

mod tables {
    include!(concat!(env!("OUT_DIR"), "/unicode_tables.rs"));
}

fn in2(t: &[(u32, u32)], c: u32) -> bool {
    t.binary_search_by(|&(a, b)| {
        if b < c {
            core::cmp::Ordering::Less
        } else if a > c {
            core::cmp::Ordering::Greater
        } else {
            core::cmp::Ordering::Equal
        }
    })
    .is_ok()
}

fn in3(t: &[(u32, u32, u8)], c: u32) -> u8 {
    match t.binary_search_by(|&(a, b, _)| {
        if b < c {
            core::cmp::Ordering::Less
        } else if a > c {
            core::cmp::Ordering::Greater
        } else {
            core::cmp::Ordering::Equal
        }
    }) {
        Ok(i) => t[i].2,
        Err(_) => 0,
    }
}

/// Cells one code point takes on its own: 0, 1 or 2.
pub fn cp_width(c: u32) -> u8 {
    if c < 0x7f {
        return (c >= 0x20) as u8;
    }
    if c < 0xa0 {
        return 0;
    }
    if in2(&tables::ZERO, c) {
        0
    } else if in2(&tables::WIDE, c) {
        2
    } else {
        1
    }
}

// variants are made from table values by transmute, so most look unused
#[allow(dead_code)]
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(u8)]
enum Gbp {
    Other = 0,
    Cr = 1,
    Lf = 2,
    Control = 3,
    Extend = 4,
    Zwj = 5,
    Ri = 6,
    Prepend = 7,
    SpacingMark = 8,
    L = 9,
    V = 10,
    T = 11,
    Lv = 12,
    Lvt = 13,
}

fn gbp(c: u32) -> Gbp {
    if c < 0x7f {
        return match c {
            0x0d => Gbp::Cr,
            0x0a => Gbp::Lf,
            0..=0x1f => Gbp::Control,
            _ => Gbp::Other,
        };
    }
    // SAFETY: the table only holds values 1..=13, all valid discriminants
    unsafe { core::mem::transmute::<u8, Gbp>(in3(&tables::GBP, c)) }
}

const INCB_LINKER: u8 = 1;
const INCB_CONSONANT: u8 = 2;
const INCB_EXTEND: u8 = 3;

fn is_pict(c: u32) -> bool {
    c >= 0xa9 && in2(&tables::PICT, c)
}

/// One grapheme cluster: its byte range in the source and the cells it takes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Cluster {
    pub start: usize,
    pub end: usize,
    pub width: u8,
}

/// Iterates the extended grapheme clusters of a string (UAX #29, including GB9c for
/// indic conjuncts and GB11 for emoji zwj sequences). Never allocates.
pub struct Graphemes<'a> {
    s: &'a str,
    pos: usize,
}

pub fn graphemes(s: &str) -> Graphemes<'_> {
    Graphemes { s, pos: 0 }
}

impl<'a> Iterator for Graphemes<'a> {
    type Item = Cluster;

    fn next(&mut self) -> Option<Cluster> {
        let bytes = self.s.as_bytes();
        let start = self.pos;
        if start >= bytes.len() {
            return None;
        }
        // ascii fast path: a printable ascii byte followed by ascii is its own cluster
        let b = bytes[start];
        if b >= 0x20 && b < 0x7f && bytes.get(start + 1).is_none_or(|&n| n < 0x80) {
            self.pos += 1;
            return Some(Cluster { start, end: start + 1, width: 1 });
        }

        let mut it = self.s[start..].char_indices();
        let (_, first) = it.next().unwrap();
        let first_u = first as u32;
        let mut prev = gbp(first_u);
        let mut end = start + first.len_utf8();

        let mut ri_count = (prev == Gbp::Ri) as u32;
        // GB11 state: inside ExtPict Extend*, and whether the last was a zwj after that
        let mut pict_seq = is_pict(first_u);
        let mut pict_zwj = false;
        // GB9c state: saw a consonant, then whether a linker followed (with extends)
        let first_incb = in3(&tables::INCB, first_u);
        let mut incb_cons = first_incb == INCB_CONSONANT;
        let mut incb_linker = false;
        let mut has_vs16 = false;

        for (off, ch) in it {
            let c = ch as u32;
            let cur = gbp(c);
            let incb = in3(&tables::INCB, c);
            let boundary = match (prev, cur) {
                (Gbp::Cr, Gbp::Lf) => false,
                (Gbp::Cr | Gbp::Lf | Gbp::Control, _) => true,
                (_, Gbp::Cr | Gbp::Lf | Gbp::Control) => true,
                (Gbp::L, Gbp::L | Gbp::V | Gbp::Lv | Gbp::Lvt) => false,
                (Gbp::Lv | Gbp::V, Gbp::V | Gbp::T) => false,
                (Gbp::Lvt | Gbp::T, Gbp::T) => false,
                (_, Gbp::Extend | Gbp::Zwj) => false,
                (_, Gbp::SpacingMark) => false,
                (Gbp::Prepend, _) => false,
                _ if incb == INCB_CONSONANT && incb_cons && incb_linker => false,
                (Gbp::Zwj, _) if pict_zwj && is_pict(c) => false,
                (Gbp::Ri, Gbp::Ri) => ri_count % 2 == 0,
                _ => true,
            };
            if boundary {
                break;
            }
            end = start + off + ch.len_utf8();
            if c == 0xfe0f {
                has_vs16 = true;
            }
            // advance GB11 state
            if is_pict(c) {
                pict_seq = true;
                pict_zwj = false;
            } else if cur == Gbp::Extend && pict_seq && !pict_zwj {
            } else if cur == Gbp::Zwj && pict_seq {
                pict_zwj = true;
            } else {
                pict_seq = false;
                pict_zwj = false;
            }
            // advance GB9c state
            match incb {
                INCB_CONSONANT => {
                    incb_cons = true;
                    incb_linker = false;
                }
                INCB_LINKER if incb_cons => incb_linker = true,
                INCB_EXTEND if incb_cons => {}
                _ => {
                    incb_cons = false;
                    incb_linker = false;
                }
            }
            if cur == Gbp::Ri {
                ri_count += 1;
            }
            prev = cur;
        }
        self.pos = end;

        let mut width = cp_width(first_u);
        if has_vs16 && is_pict(first_u) {
            width = 2;
        }
        if ri_count >= 2 {
            width = 2;
        }
        if width == 0 && !matches!(gbp(first_u), Gbp::Cr | Gbp::Lf | Gbp::Control) {
            // a lone combining mark still occupies a cell when drawn
            width = 1;
        }
        Some(Cluster { start, end, width })
    }
}

/// Total cells a string takes on one line.
pub fn str_width(s: &str) -> usize {
    graphemes(s).map(|g| g.width as usize).sum()
}

/// Returns the longest valid utf-8 prefix of bytes (text cut by a budget may end mid-char).
pub fn utf8_prefix(b: &[u8]) -> &str {
    match core::str::from_utf8(b) {
        Ok(s) => s,
        // SAFETY: valid_up_to marks a valid utf-8 boundary
        Err(e) => unsafe { core::str::from_utf8_unchecked(&b[..e.valid_up_to()]) },
    }
}

/// Finds where the next wrapped line ends. Returns (end of the line's text, start of the
/// next line). Breaks at the last space that fits, or mid-word when a word is too long.
pub fn next_line(s: &str, start: usize, width: usize) -> (usize, usize) {
    let width = width.max(1);
    let mut used = 0usize;
    let mut last_space: Option<(usize, usize)> = None;
    for g in graphemes(&s[start..]) {
        let gs = start + g.start;
        let ge = start + g.end;
        let b = s.as_bytes()[gs];
        if b == b'\n' || (b == b'\r') {
            let next = if s[gs..].starts_with("\r\n") { gs + 2 } else { gs + 1 };
            return (gs, next);
        }
        let w = if b == b'\t' { 1 } else { g.width as usize };
        if used + w > width {
            if let Some((e, n)) = last_space {
                return (e, n);
            }
            if gs == start {
                // a single cluster wider than the line: take it anyway
                return (ge, ge);
            }
            return (gs, gs);
        }
        used += w;
        if b == b' ' {
            last_space = Some((gs, ge));
        }
    }
    (s.len(), s.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn clusters(s: &str) -> Vec<(&str, u8)> {
        graphemes(s).map(|c| (&s[c.start..c.end], c.width)).collect()
    }

    #[test]
    fn ascii_and_cjk() {
        assert_eq!(str_width("hello"), 5);
        assert_eq!(str_width("日本"), 4);
        assert_eq!(cp_width(0x301), 0);
    }

    #[test]
    fn emoji_sequences_are_one_wide_cluster() {
        // family: man zwj woman zwj girl
        let fam = "\u{1F468}\u{200D}\u{1F469}\u{200D}\u{1F467}";
        assert_eq!(clusters(fam), vec![(fam, 2)]);
        // flag: two regional indicators
        let flag = "\u{1F1EE}\u{1F1F3}";
        assert_eq!(clusters(flag), vec![(flag, 2)]);
        // heart with vs16 is wide
        let heart = "\u{2764}\u{FE0F}";
        assert_eq!(clusters(heart), vec![(heart, 2)]);
        // skin tone modifier
        let wave = "\u{1F44B}\u{1F3FD}";
        assert_eq!(clusters(wave).len(), 1);
    }

    #[test]
    fn telugu_conjuncts_stay_together() {
        // క్ష : ka + virama + ssa is one cluster (GB9c)
        let ksha = "\u{0C15}\u{0C4D}\u{0C37}";
        assert_eq!(clusters(ksha), vec![(ksha, 1)]);
        // తెలుగు : ta+e, la+u, ga+u = three clusters
        assert_eq!(clusters("తెలుగు").len(), 3);
        assert_eq!(str_width("తెలుగు"), 3);
    }

    #[test]
    fn crlf_and_controls() {
        assert_eq!(clusters("a\r\nb").len(), 3);
    }

    #[test]
    fn wrapping() {
        let s = "the quick brown fox";
        assert_eq!(next_line(s, 0, 10), (9, 10));
        assert_eq!(&s[10..next_line(s, 10, 10).0], "brown fox");
        // hard break inside a long word
        assert_eq!(next_line("abcdefghij", 0, 4), (4, 4));
        // newline forces a break
        assert_eq!(next_line("ab\ncd", 0, 10), (2, 3));
        // wide chars don't overflow
        assert_eq!(next_line("日本語", 0, 5), (6, 6));
    }
}
