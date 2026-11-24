//! A json scanner driven by a compiled set of paths.
//!
//! Each adapter declares the paths it keeps, such as `data[*].text`. They are checked at
//! build time (a malformed path is a compile error) and turned into a small trie once at
//! start. The scanner then walks the bytes once, as they arrive, in any chunk sizes. It
//! copies a value only when the trie says it matched, decoding escapes on the way out,
//! and skips every other subtree by counting brackets. There is no parse tree.

/// What a declared path keeps: a field's value, or the boundaries of a record (one
/// element of a list that a row is built from).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Field,
    Record,
}

#[derive(Clone, Copy, Debug)]
pub struct Spec {
    pub path: &'static str,
    pub id: u16,
    pub kind: Kind,
}

/// True when the path is a run of `name` segments joined by dots, each optionally
/// followed by `[*]`. Usable in const context, so adapters fail to build on a typo.
pub const fn valid_path(p: &str) -> bool {
    let b = p.as_bytes();
    if b.is_empty() {
        return false;
    }
    let mut i = 0;
    // a segment is a name, a [*], or a name followed by [*]s
    let mut need_seg = true;
    while i < b.len() {
        let c = b[i];
        if c == b'[' {
            if i + 2 >= b.len() || b[i + 1] != b'*' || b[i + 2] != b']' {
                return false;
            }
            i += 3;
            need_seg = false;
        } else if c == b'.' {
            if need_seg {
                return false;
            }
            need_seg = true;
            i += 1;
        } else if c.is_ascii_alphanumeric() || c == b'_' || c == b'-' || c == b'@' {
            i += 1;
            need_seg = false;
        } else {
            return false;
        }
    }
    !need_seg
}

/// Declares a const table of specs and asserts every path at compile time.
#[macro_export]
macro_rules! json_paths {
    ($name:ident { $($path:literal => $id:expr, $kind:ident;)* }) => {
        pub const $name: &[$crate::json::Spec] = &[
            $($crate::json::Spec { path: $path, id: $id, kind: $crate::json::Kind::$kind },)*
        ];
        const _: () = {
            $(assert!($crate::json::valid_path($path), concat!("bad json path: ", $path));)*
        };
    };
}

const MAX_NODES: usize = 64;
const NONE: u16 = u16::MAX;
const DEAD: u8 = u8::MAX;
const MAX_DEPTH: usize = 48;
const KEY_MAX: usize = 64;

#[derive(Clone, Copy)]
struct Node {
    key: &'static [u8], // empty = the [*] wildcard
    any: bool,
    first_child: u8,
    next_sibling: u8,
    field: u16,
    record: u16,
}

const EMPTY_NODE: Node = Node { key: b"", any: false, first_child: DEAD, next_sibling: DEAD, field: NONE, record: NONE };

/// The compiled trie of an adapter's paths. Built once; fixed size, no heap.
#[derive(Clone)]
pub struct Paths {
    nodes: [Node; MAX_NODES],
    len: usize,
}

impl Paths {
    pub fn compile(specs: &[Spec]) -> Paths {
        let mut p = Paths { nodes: [EMPTY_NODE; MAX_NODES], len: 1 };
        for s in specs {
            let mut cur = 0u8;
            let b = s.path.as_bytes();
            let mut i = 0;
            while i < b.len() {
                if b[i] == b'.' {
                    i += 1;
                    continue;
                }
                let (key, any, step): (&'static [u8], bool, usize) = if b[i] == b'[' {
                    (b"", true, 3)
                } else {
                    let start = i;
                    let mut j = i;
                    while j < b.len() && b[j] != b'.' && b[j] != b'[' {
                        j += 1;
                    }
                    (&s.path.as_bytes()[start..j], false, j - start)
                };
                cur = p.child_or_add(cur, key, any);
                i += step;
            }
            let n = &mut p.nodes[cur as usize];
            match s.kind {
                Kind::Field => n.field = s.id,
                Kind::Record => n.record = s.id,
            }
        }
        p
    }

    fn child_or_add(&mut self, parent: u8, key: &'static [u8], any: bool) -> u8 {
        let mut c = self.nodes[parent as usize].first_child;
        while c != DEAD {
            let n = &self.nodes[c as usize];
            if n.any == any && n.key == key {
                return c;
            }
            c = n.next_sibling;
        }
        assert!(self.len < MAX_NODES, "too many json path nodes");
        let id = self.len as u8;
        self.len += 1;
        self.nodes[id as usize] = Node { key, any, first_child: DEAD, next_sibling: self.nodes[parent as usize].first_child, field: NONE, record: NONE };
        self.nodes[parent as usize].first_child = id;
        id
    }

    fn child_key(&self, parent: u8, key: &[u8]) -> u8 {
        if parent == DEAD {
            return DEAD;
        }
        let mut c = self.nodes[parent as usize].first_child;
        while c != DEAD {
            let n = &self.nodes[c as usize];
            if !n.any && n.key == key {
                return c;
            }
            c = n.next_sibling;
        }
        DEAD
    }

    fn child_any(&self, parent: u8) -> u8 {
        if parent == DEAD {
            return DEAD;
        }
        let mut c = self.nodes[parent as usize].first_child;
        while c != DEAD {
            if self.nodes[c as usize].any {
                return c;
            }
            c = self.nodes[c as usize].next_sibling;
        }
        DEAD
    }
}

/// Where matched values go.
pub trait Sink {
    fn record_start(&mut self, _rec: u16) {}
    fn record_end(&mut self, _rec: u16) {}
    /// A matched value begins. `string` is false for numbers, true/false and null.
    fn field_start(&mut self, field: u16, string: bool);
    /// Decoded utf-8 bytes of the current field, in one or more pieces.
    fn bytes(&mut self, b: &[u8]);
    fn field_end(&mut self, field: u16);
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum St {
    Value,
    ObjKeyOrEnd,
    ObjKey,
    ObjKeyEsc,
    ObjColon,
    AfterValue,
    ArrValueOrEnd,
    Str,
    StrEsc,
    StrHex,
    Scalar,
    Skip,
    Done,
    Error,
}

#[derive(Clone, Copy)]
struct Frame {
    node: u8,
    array: bool,
}

/// The push scanner. Feed it bytes in any chunk sizes; it keeps its own place.
pub struct Scanner<'p> {
    paths: &'p Paths,
    st: St,
    stack: [Frame; MAX_DEPTH],
    depth: usize,
    key: [u8; KEY_MAX],
    key_len: usize,
    key_overflow: bool,
    vnode: u8,       // node of the value about to start
    field: u16,      // field being captured, or NONE
    hex: u32,
    hex_n: u8,
    hi_surrogate: u32,
    skip_depth: u32,
    skip_str: bool,
    skip_esc: bool,
}

impl<'p> Scanner<'p> {
    pub fn new(paths: &'p Paths) -> Self {
        Scanner {
            paths,
            st: St::Value,
            stack: [Frame { node: DEAD, array: false }; MAX_DEPTH],
            depth: 0,
            key: [0; KEY_MAX],
            key_len: 0,
            key_overflow: false,
            vnode: 0,
            field: NONE,
            hex: 0,
            hex_n: 0,
            hi_surrogate: 0,
            skip_depth: 0,
            skip_str: false,
            skip_esc: false,
        }
    }

    /// Ready for a new document (each server-sent event carries one).
    pub fn reset(&mut self) {
        self.st = St::Value;
        self.depth = 0;
        self.vnode = 0;
        self.field = NONE;
        self.hi_surrogate = 0;
    }

    pub fn done(&self) -> bool {
        self.st == St::Done
    }

    pub fn failed(&self) -> bool {
        self.st == St::Error
    }

    /// Call at the end of input: closes a top-level scalar that had no delimiter.
    pub fn finish(&mut self, sink: &mut dyn Sink) {
        if self.st == St::Scalar {
            self.end_field(sink);
            self.after_value_done();
        }
    }

    fn end_field(&mut self, sink: &mut dyn Sink) {
        if self.field != NONE {
            if self.hi_surrogate != 0 {
                sink.bytes("\u{FFFD}".as_bytes());
                self.hi_surrogate = 0;
            }
            sink.field_end(self.field);
            self.field = NONE;
        }
    }

    fn after_value_done(&mut self) {
        self.st = if self.depth == 0 { St::Done } else { St::AfterValue };
    }

    fn push_frame(&mut self, node: u8, array: bool, sink: &mut dyn Sink) -> bool {
        if self.depth >= MAX_DEPTH {
            return false;
        }
        self.stack[self.depth] = Frame { node, array };
        self.depth += 1;
        let rec = self.paths.nodes[node as usize].record;
        if rec != NONE {
            sink.record_start(rec);
        }
        true
    }

    fn pop_frame(&mut self, sink: &mut dyn Sink) {
        self.depth -= 1;
        let node = self.stack[self.depth].node;
        let rec = self.paths.nodes[node as usize].record;
        if rec != NONE {
            sink.record_end(rec);
        }
        self.after_value_done();
    }

    fn emit_cp(&mut self, cp: u32, sink: &mut dyn Sink) {
        if self.field == NONE {
            return;
        }
        let c = char::from_u32(cp).unwrap_or('\u{FFFD}');
        let mut buf = [0u8; 4];
        sink.bytes(c.encode_utf8(&mut buf).as_bytes());
    }

    pub fn feed(&mut self, data: &[u8], sink: &mut dyn Sink) {
        let mut i = 0usize;
        let n = data.len();
        while i < n {
            let c = data[i];
            match self.st {
                St::Done | St::Error => return,
                St::Value => {
                    if c.is_ascii_whitespace() {
                        i += 1;
                        continue;
                    }
                    let node = self.vnode;
                    let has_kids = node != DEAD && self.paths.nodes[node as usize].first_child != DEAD;
                    let field = if node == DEAD { NONE } else { self.paths.nodes[node as usize].field };
                    match c {
                        b'{' | b'[' => {
                            let array = c == b'[';
                            let rec = node != DEAD && self.paths.nodes[node as usize].record != NONE;
                            if (has_kids || rec) && self.push_frame(node, array, sink) {
                                self.st = if array { St::ArrValueOrEnd } else { St::ObjKeyOrEnd };
                            } else {
                                self.st = St::Skip;
                                self.skip_depth = 1;
                                self.skip_str = false;
                                self.skip_esc = false;
                            }
                            i += 1;
                        }
                        b'"' => {
                            if field != NONE {
                                self.field = field;
                                sink.field_start(field, true);
                            }
                            self.st = St::Str;
                            i += 1;
                        }
                        b'-' | b'0'..=b'9' | b't' | b'f' | b'n' => {
                            if field != NONE {
                                self.field = field;
                                sink.field_start(field, false);
                            }
                            self.st = St::Scalar;
                        }
                        _ => {
                            self.st = St::Error;
                        }
                    }
                }
                St::Scalar => {
                    let start = i;
                    while i < n && !matches!(data[i], b',' | b'}' | b']' | b' ' | b'\n' | b'\r' | b'\t') {
                        i += 1;
                    }
                    if self.field != NONE && i > start {
                        sink.bytes(&data[start..i]);
                    }
                    if i < n {
                        self.end_field(sink);
                        self.after_value_done();
                    }
                }
                St::Str => {
                    // emit the longest run without quote or backslash in one call
                    let start = i;
                    while i < n && data[i] != b'"' && data[i] != b'\\' {
                        i += 1;
                    }
                    if self.field != NONE && i > start {
                        if self.hi_surrogate != 0 {
                            self.hi_surrogate = 0;
                            sink.bytes("\u{FFFD}".as_bytes());
                        }
                        sink.bytes(&data[start..i]);
                    }
                    if i < n {
                        if data[i] == b'"' {
                            self.end_field(sink);
                            self.after_value_done();
                        } else {
                            self.st = St::StrEsc;
                        }
                        i += 1;
                    }
                }
                St::StrEsc => {
                    i += 1;
                    let out = match c {
                        b'n' => b'\n',
                        b't' => b'\t',
                        b'r' => b'\r',
                        b'b' => 0x08,
                        b'f' => 0x0c,
                        b'"' => b'"',
                        b'\\' => b'\\',
                        b'/' => b'/',
                        b'u' => {
                            self.st = St::StrHex;
                            self.hex = 0;
                            self.hex_n = 0;
                            continue;
                        }
                        _ => {
                            self.st = St::Error;
                            continue;
                        }
                    };
                    if self.hi_surrogate != 0 {
                        self.hi_surrogate = 0;
                        self.emit_cp(0xFFFD, sink);
                    }
                    if self.field != NONE {
                        sink.bytes(&[out]);
                    }
                    self.st = St::Str;
                }
                St::StrHex => {
                    let v = match c {
                        b'0'..=b'9' => c - b'0',
                        b'a'..=b'f' => c - b'a' + 10,
                        b'A'..=b'F' => c - b'A' + 10,
                        _ => {
                            self.st = St::Error;
                            continue;
                        }
                    };
                    i += 1;
                    self.hex = self.hex << 4 | v as u32;
                    self.hex_n += 1;
                    if self.hex_n == 4 {
                        let cp = self.hex;
                        if (0xD800..0xDC00).contains(&cp) {
                            if self.hi_surrogate != 0 {
                                self.emit_cp(0xFFFD, sink);
                            }
                            self.hi_surrogate = cp;
                        } else if (0xDC00..0xE000).contains(&cp) {
                            if self.hi_surrogate != 0 {
                                let full = 0x10000 + ((self.hi_surrogate - 0xD800) << 10) + (cp - 0xDC00);
                                self.hi_surrogate = 0;
                                self.emit_cp(full, sink);
                            } else {
                                self.emit_cp(0xFFFD, sink);
                            }
                        } else {
                            if self.hi_surrogate != 0 {
                                self.hi_surrogate = 0;
                                self.emit_cp(0xFFFD, sink);
                            }
                            self.emit_cp(cp, sink);
                        }
                        self.st = St::Str;
                    }
                }
                St::ObjKeyOrEnd => {
                    i += 1;
                    match c {
                        b'"' => {
                            self.key_len = 0;
                            self.key_overflow = false;
                            self.st = St::ObjKey;
                        }
                        b'}' => self.pop_frame(sink),
                        b' ' | b'\n' | b'\r' | b'\t' => {}
                        _ => self.st = St::Error,
                    }
                }
                St::ObjKey => {
                    let start = i;
                    while i < n && data[i] != b'"' && data[i] != b'\\' {
                        i += 1;
                    }
                    let piece = &data[start..i];
                    if self.key_len + piece.len() <= KEY_MAX {
                        self.key[self.key_len..self.key_len + piece.len()].copy_from_slice(piece);
                        self.key_len += piece.len();
                    } else {
                        self.key_overflow = true;
                    }
                    if i < n {
                        if data[i] == b'"' {
                            self.st = St::ObjColon;
                        } else {
                            self.st = St::ObjKeyEsc;
                        }
                        i += 1;
                    }
                }
                St::ObjKeyEsc => {
                    // escaped keys never match a declared path; keep counting bytes
                    self.key_overflow = true;
                    i += 1;
                    self.st = St::ObjKey;
                }
                St::ObjColon => {
                    i += 1;
                    match c {
                        b':' => {
                            let parent = self.stack[self.depth - 1].node;
                            self.vnode = if self.key_overflow {
                                DEAD
                            } else {
                                self.paths.child_key(parent, &self.key[..self.key_len])
                            };
                            self.st = St::Value;
                        }
                        b' ' | b'\n' | b'\r' | b'\t' => {}
                        _ => self.st = St::Error,
                    }
                }
                St::ArrValueOrEnd => {
                    if c.is_ascii_whitespace() {
                        i += 1;
                    } else if c == b']' {
                        i += 1;
                        self.pop_frame(sink);
                    } else {
                        self.vnode = self.paths.child_any(self.stack[self.depth - 1].node);
                        self.st = St::Value;
                    }
                }
                St::AfterValue => {
                    i += 1;
                    let top = self.stack[self.depth - 1];
                    match c {
                        b',' => {
                            if top.array {
                                self.vnode = self.paths.child_any(top.node);
                                self.st = St::Value;
                            } else {
                                self.st = St::ObjKeyOrEnd;
                            }
                        }
                        b'}' if !top.array => self.pop_frame(sink),
                        b']' if top.array => self.pop_frame(sink),
                        b' ' | b'\n' | b'\r' | b'\t' => {}
                        _ => self.st = St::Error,
                    }
                }
                St::Skip => {
                    // count brackets outside strings; never look inside values
                    while i < n {
                        let b = data[i];
                        i += 1;
                        if self.skip_str {
                            if self.skip_esc {
                                self.skip_esc = false;
                            } else if b == b'\\' {
                                self.skip_esc = true;
                            } else if b == b'"' {
                                self.skip_str = false;
                            }
                            continue;
                        }
                        match b {
                            b'"' => self.skip_str = true,
                            b'{' | b'[' => self.skip_depth += 1,
                            b'}' | b']' => {
                                self.skip_depth -= 1;
                                if self.skip_depth == 0 {
                                    self.after_value_done();
                                    break;
                                }
                            }
                            _ => {}
                        }
                    }
                }
            }
        }
    }
}

/// A sink that collects matches as (field, text) pairs and record markers. For tests,
/// fixtures and one-off small responses (auth tokens), never for feeds.
#[derive(Default, Debug)]
pub struct Collect {
    pub out: Vec<(u16, String)>,
    pub records: Vec<(bool, u16)>,
    cur: Vec<u8>,
}

impl Sink for Collect {
    fn record_start(&mut self, r: u16) {
        self.records.push((true, r));
    }
    fn record_end(&mut self, r: u16) {
        self.records.push((false, r));
    }
    fn field_start(&mut self, _f: u16, _s: bool) {
        self.cur.clear();
    }
    fn bytes(&mut self, b: &[u8]) {
        self.cur.extend_from_slice(b);
    }
    fn field_end(&mut self, f: u16) {
        self.out.push((f, String::from_utf8_lossy(&self.cur).into_owned()));
    }
}

impl Collect {
    pub fn get(&self, f: u16) -> Option<&str> {
        self.out.iter().find(|(id, _)| *id == f).map(|(_, s)| s.as_str())
    }
}

/// Scans a whole small document and returns what matched.
pub fn collect(specs: &[Spec], doc: &[u8]) -> Collect {
    let p = Paths::compile(specs);
    let mut s = Scanner::new(&p);
    let mut c = Collect::default();
    s.feed(doc, &mut c);
    s.finish(&mut c);
    c
}

/// Writes a json string literal (with quotes) for request bodies.
pub fn push_str(out: &mut Vec<u8>, s: &str) {
    out.push(b'"');
    for ch in s.chars() {
        match ch {
            '"' => out.extend_from_slice(b"\\\""),
            '\\' => out.extend_from_slice(b"\\\\"),
            '\n' => out.extend_from_slice(b"\\n"),
            '\r' => out.extend_from_slice(b"\\r"),
            '\t' => out.extend_from_slice(b"\\t"),
            c if (c as u32) < 0x20 => {
                let v = c as u32;
                const H: &[u8; 16] = b"0123456789abcdef";
                out.extend_from_slice(b"\\u00");
                out.push(H[(v >> 4) as usize]);
                out.push(H[(v & 15) as usize]);
            }
            c => {
                let mut b = [0u8; 4];
                out.extend_from_slice(c.encode_utf8(&mut b).as_bytes());
            }
        }
    }
    out.push(b'"');
}

#[cfg(test)]
mod tests {
    use super::*;

    json_paths!(T {
        "data[*]" => 1, Record;
        "data[*].id" => 10, Field;
        "data[*].text" => 11, Field;
        "data[*].m.likes" => 12, Field;
        "meta.next_token" => 20, Field;
    });

    const DOC: &str = r#"{"data":[{"id":"1","text":"hi \"there\"\né😀","skip":{"a":[1,{"b":"}]"}]},"m":{"likes":42}},
        {"text":"two","id":"2","m":{"likes":0}}],"includes":{"users":[{"id":"9"}]},"meta":{"result_count":2,"next_token":"abc"}}"#;

    fn run(chunks: &[&[u8]]) -> Collect {
        let p = Paths::compile(T);
        let mut s = Scanner::new(&p);
        let mut c = Collect::default();
        for ch in chunks {
            s.feed(ch, &mut c);
        }
        s.finish(&mut c);
        assert!(!s.failed());
        c
    }

    #[test]
    fn matches_paths_and_decodes() {
        let c = run(&[DOC.as_bytes()]);
        assert_eq!(
            c.out,
            vec![
                (10, "1".into()),
                (11, "hi \"there\"\né😀".into()),
                (12, "42".into()),
                (11, "two".into()),
                (10, "2".into()),
                (12, "0".into()),
                (20, "abc".into()),
            ]
        );
        assert_eq!(c.records, vec![(true, 1), (false, 1), (true, 1), (false, 1)]);
    }

    #[test]
    fn any_chunking_gives_the_same_result() {
        let whole = run(&[DOC.as_bytes()]).out;
        let b = DOC.as_bytes();
        for size in 1..12 {
            let chunks: Vec<&[u8]> = b.chunks(size).collect();
            assert_eq!(run(&chunks).out, whole, "chunk size {size}");
        }
    }

    #[test]
    fn lone_surrogates_become_replacement() {
        json_paths!(S { "t" => 1, Field; });
        let c = collect(S, br#"{"t":"a\ud83dz\udc00"}"#);
        assert_eq!(c.get(1), Some("a\u{FFFD}z\u{FFFD}"));
    }

    #[test]
    fn top_level_scalar_and_garbage() {
        json_paths!(S { "x" => 1, Field; });
        let c = collect(S, b"{\"x\":12}");
        assert_eq!(c.get(1), Some("12"));
        // garbage never panics
        let _ = collect(S, b"{\"x\":[}}}}\"\\");
        let _ = collect(S, b"]]]");
    }

    #[test]
    fn path_validation() {
        assert!(valid_path("data[*].text"));
        assert!(valid_path("[*].x"));
        assert!(valid_path("a.b.c"));
        assert!(!valid_path("a..b"));
        assert!(!valid_path("a[x]"));
        assert!(!valid_path("a."));
    }

    #[test]
    fn writes_json_strings() {
        let mut v = Vec::new();
        push_str(&mut v, "a\"b\\\n\u{1}é");
        assert_eq!(String::from_utf8(v).unwrap(), r#""a\"b\\\n\u0001é""#);
    }
}
