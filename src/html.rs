//! Html to readable blocks, streamed. No dom, no css, no javascript: the page's text,
//! its headings, lists, code and quotes, and its links. Script, style, svg and the head
//! are skipped. Feed it bytes in any chunk sizes.

pub const K_P: u8 = 0;
// 1..=6 are headings h1..h6
pub const K_LI: u8 = 7;
pub const K_PRE: u8 = 8;
pub const K_QUOTE: u8 = 9;
pub const K_HR: u8 = 10;
pub const K_ROW: u8 = 11;
/// not a block: everything sent so far was page chrome; the article starts now
pub const K_RESET: u8 = 12;

pub trait PageSink {
    fn block_start(&mut self, kind: u8, depth: u8);
    /// decoded utf-8 text of the open block
    fn text(&mut self, b: &[u8]);
    fn block_end(&mut self);
    /// a link starts at the next text written
    fn link_start(&mut self, href: &[u8]);
    fn link_end(&mut self);
    fn title(&mut self, b: &[u8]);
    /// drop the blocks sent so far (they were menus and headers before the article)
    fn reset(&mut self);
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum St {
    Text,
    Ent,
    TagOpen,
    TagName,
    BeforeAttr,
    AttrName,
    AfterAttrName,
    BeforeValue,
    ValueQ,
    ValueU,
    Bang,
    Comment,
    Raw,
}

const NAME: usize = 16;
const HREF: usize = 2048;

pub struct Html {
    st: St,
    tag: [u8; NAME],
    tag_len: usize,
    end_tag: bool,
    attr: [u8; NAME],
    attr_len: usize,
    quote: u8,
    // the value of href or alt for the current tag
    val: Box<[u8; HREF]>,
    val_len: usize,
    capture: u8, // 0 none, 1 href, 2 alt, 3 class, 4 role, 5 aria-hidden
    href: Box<[u8; HREF]>,
    href_len: usize,
    alt: [u8; 256],
    alt_len: usize,
    class: [u8; 256],
    class_len: usize,
    role: [u8; 24],
    role_len: usize,
    hidden: bool,
    ent: [u8; 12],
    ent_len: usize,
    dashes: u8,
    // raw text element being skipped or captured: script, style, title, textarea
    raw: [u8; NAME],
    raw_len: usize,
    raw_match: usize,
    raw_title: bool,
    // layout state
    open: bool,
    has_text: bool,
    space: bool,
    // a skipped element: its name and how deep inside it we are
    skip_tag: [u8; NAME],
    skip_len: usize,
    skip: u32,
    // inside <main> or <article>, and whether one has been seen
    content: u32,
    seen_content: bool,
    blocks: u32,
    cell_sep: bool,
    in_head: bool,
    heading: u8,
    pre: u32,
    quote_depth: u32,
    list: u32,
    in_li: bool,
    in_row: bool,
    cell_text: bool,
    in_link: bool,
    out: [u8; 512],
    out_len: usize,
}

impl Default for Html {
    fn default() -> Self {
        Self::new()
    }
}

fn is(tag: &[u8], names: &[&str]) -> bool {
    names.iter().any(|n| n.as_bytes() == tag)
}

const BLOCK_TAGS: &[&str] = &[
    "p", "div", "section", "article", "main", "header", "footer", "aside", "nav", "ul", "ol", "li", "dl", "dt", "dd", "pre", "blockquote", "h1", "h2", "h3",
    "h4", "h5", "h6", "table", "tr", "tbody", "thead", "figure", "figcaption", "form", "center", "hr", "address", "details", "summary", "fieldset",
];
const VOID_TAGS: &[&str] = &["area", "base", "br", "col", "embed", "hr", "img", "input", "link", "meta", "source", "track", "wbr"];
/// Classes and ids of page chrome on common sites (wikipedia's menus and edit links,
/// cookie banners, share buttons).
const SKIP_CLASSES: &[&str] = &[
    "mw-editsection", "navbox", "noprint", "mw-jump", "vector-menu", "vector-toc", "vector-page-toolbar", "vector-dropdown", "vector-pinnable",
    "vector-appearance", "vector-sticky", "vector-header", "vector-column", "mw-indicators", "catlinks", "printfooter", "sidebar", "cookie", "share-",
    "skip-link", "visually-hidden", "sr-only", "screen-reader", "mw-portlet", "site-nav", "breadcrumb",
];
const SKIP_TAGS: &[&str] = &["svg", "noscript", "template", "iframe", "select", "canvas", "math", "button", "object", "video", "audio", "picture"];

impl Html {
    pub fn new() -> Html {
        Html {
            st: St::Text,
            tag: [0; NAME],
            tag_len: 0,
            end_tag: false,
            attr: [0; NAME],
            attr_len: 0,
            quote: 0,
            val: Box::new([0; HREF]),
            val_len: 0,
            capture: 0,
            href: Box::new([0; HREF]),
            href_len: 0,
            alt: [0; 256],
            alt_len: 0,
            class: [0; 256],
            class_len: 0,
            role: [0; 24],
            role_len: 0,
            hidden: false,
            ent: [0; 12],
            ent_len: 0,
            dashes: 0,
            raw: [0; NAME],
            raw_len: 0,
            raw_match: 0,
            raw_title: false,
            open: false,
            has_text: false,
            space: false,
            skip_tag: [0; NAME],
            skip_len: 0,
            skip: 0,
            content: 0,
            seen_content: false,
            blocks: 0,
            cell_sep: false,
            in_head: false,
            heading: 0,
            pre: 0,
            quote_depth: 0,
            list: 0,
            in_li: false,
            in_row: false,
            cell_text: false,
            in_link: false,
            out: [0; 512],
            out_len: 0,
        }
    }

    fn kind(&self) -> (u8, u8) {
        if self.heading > 0 {
            (self.heading, 0)
        } else if self.pre > 0 {
            (K_PRE, 0)
        } else if self.in_li {
            (K_LI, self.list.min(6) as u8)
        } else if self.quote_depth > 0 {
            (K_QUOTE, 0)
        } else if self.in_row {
            (K_ROW, 0)
        } else {
            (K_P, 0)
        }
    }

    fn flush(&mut self, sink: &mut dyn PageSink) {
        if self.out_len > 0 {
            sink.text(&self.out[..self.out_len]);
            self.out_len = 0;
        }
    }

    fn emit(&mut self, b: &[u8], sink: &mut dyn PageSink) {
        if self.out_len + b.len() > self.out.len() {
            self.flush(sink);
        }
        if b.len() > self.out.len() {
            sink.text(b);
            return;
        }
        self.out[self.out_len..self.out_len + b.len()].copy_from_slice(b);
        self.out_len += b.len();
    }

    /// One visible character (already decoded) of page text.
    fn put(&mut self, b: &[u8], sink: &mut dyn PageSink) {
        if self.skip > 0 || self.in_head || (self.seen_content && self.content == 0) {
            return;
        }
        let ws = b.len() == 1 && matches!(b[0], b' ' | b'\n' | b'\r' | b'\t' | 0x0c);
        let pre = self.pre > 0;
        if ws && !pre {
            if self.has_text {
                self.space = true;
            }
            return;
        }
        if !self.open {
            let (k, d) = self.kind();
            sink.block_start(k, d);
            self.open = true;
            self.has_text = false;
        }
        if self.cell_sep && self.has_text {
            self.emit(b"   ", sink);
        } else if self.space && self.has_text {
            self.emit(b" ", sink);
        }
        self.cell_sep = false;
        self.space = false;
        if pre && b == b"\r" {
            return;
        }
        self.emit(b, sink);
        self.has_text = true;
        self.cell_text = true;
    }

    fn end_block(&mut self, sink: &mut dyn PageSink) {
        if self.in_link {
            self.flush(sink);
            sink.link_end();
            self.in_link = false;
        }
        if self.open {
            self.flush(sink);
            sink.block_end();
            self.blocks += 1;
        }
        self.open = false;
        self.has_text = false;
        self.space = false;
        self.cell_text = false;
    }

    fn on_tag(&mut self, sink: &mut dyn PageSink) {
        let mut name = [0u8; NAME];
        let n = self.tag_len;
        for i in 0..n {
            name[i] = self.tag[i].to_ascii_lowercase();
        }
        let t = &name[..n];
        let end = self.end_tag;
        if !end && is(t, &["script", "style", "title", "textarea"]) {
            self.raw[..n].copy_from_slice(t);
            self.raw_len = n;
            self.raw_match = 0;
            self.raw_title = t == b"title";
            self.st = St::Raw;
            return;
        }
        if t == b"head" {
            self.in_head = !end;
            return;
        }
        if t == b"body" {
            self.in_head = false;
            return;
        }
        // inside a skipped element only its own nesting matters
        if self.skip > 0 {
            if t == &self.skip_tag[..self.skip_len] {
                if end {
                    self.skip -= 1;
                } else {
                    self.skip += 1;
                }
            }
            return;
        }
        if self.in_head {
            return;
        }
        if !end && !is(t, VOID_TAGS) && self.chrome(t) {
            if is(t, BLOCK_TAGS) || is(t, &["nav", "footer", "aside"]) {
                self.end_block(sink);
            }
            self.skip_tag[..n].copy_from_slice(t);
            self.skip_len = n;
            self.skip = 1;
            return;
        }
        let main = is(t, &["main", "article"]) || (!end && &self.role[..self.role_len] == b"main");
        if main {
            if end {
                self.content = self.content.saturating_sub(1);
                if self.content == 0 {
                    self.end_block(sink);
                }
            } else {
                if !self.seen_content && self.blocks > 0 {
                    // what came before the article was menus and headers
                    self.end_block(sink);
                    sink.reset();
                    self.blocks = 0;
                }
                self.content += 1;
                self.seen_content = true;
            }
            if is(t, &["main", "article"]) {
                return;
            }
        }
        match t {
            b"br" => {
                if self.open && self.has_text {
                    self.emit(b"\n", sink);
                    self.space = false;
                }
                return;
            }
            b"a" => {
                if end {
                    if self.in_link {
                        self.flush(sink);
                        sink.link_end();
                        self.in_link = false;
                    }
                } else if self.href_len > 0 {
                    if self.in_link {
                        self.flush(sink);
                        sink.link_end();
                    }
                    // a pending space belongs before the link, not inside it
                    if self.has_text && self.open && (self.space || self.cell_sep) {
                        self.emit(if self.cell_sep { b"   " } else { b" " }, sink);
                        self.space = false;
                        self.cell_sep = false;
                    }
                    self.flush(sink);
                    sink.link_start(&self.href[..self.href_len]);
                    self.in_link = true;
                }
                return;
            }
            b"img" => {
                if self.alt_len > 0 && self.alt_len < 120 {
                    let alt = self.alt;
                    let n = self.alt_len;
                    self.put(b"[", sink);
                    for ch in crate::text::utf8_prefix(&alt[..n]).chars() {
                        let mut buf = [0u8; 4];
                        self.put(ch.encode_utf8(&mut buf).as_bytes(), sink);
                    }
                    self.put(b"]", sink);
                }
                return;
            }
            b"td" | b"th" => {
                if !end && self.cell_text {
                    self.cell_sep = true;
                    self.cell_text = false;
                }
                return;
            }
            _ => {}
        }
        if !is(t, BLOCK_TAGS) {
            return;
        }
        // in a table row, boxes inside cells do not break the row
        if self.in_row && is(t, &["div", "center", "p", "section", "figure", "figcaption", "header", "footer", "form", "details", "summary"]) {
            if self.has_text {
                self.space = true;
            }
            return;
        }
        self.end_block(sink);
        match t {
            b"h1" | b"h2" | b"h3" | b"h4" | b"h5" | b"h6" => self.heading = if end { 0 } else { t[1] - b'0' },
            b"pre" => self.pre = if end { self.pre.saturating_sub(1) } else { self.pre + 1 },
            b"blockquote" => self.quote_depth = if end { self.quote_depth.saturating_sub(1) } else { self.quote_depth + 1 },
            b"ul" | b"ol" => {
                if end {
                    self.list = self.list.saturating_sub(1);
                    self.in_li = self.list > 0;
                } else {
                    self.list += 1;
                    self.in_li = false;
                }
            }
            b"li" => self.in_li = !end || self.list > 1,
            b"tr" => self.in_row = !end,
            b"table" => self.in_row = false,
            b"hr" if !end => {
                sink.block_start(K_HR, 0);
                sink.block_end();
            }
            _ => {}
        }
    }

    fn decode_entity(&mut self, sink: &mut dyn PageSink) {
        let n = self.ent_len;
        let e = self.ent;
        let name = &e[..n];
        let ch: Option<char> = match name {
            b"amp" => Some('&'),
            b"lt" => Some('<'),
            b"gt" => Some('>'),
            b"quot" => Some('"'),
            b"apos" => Some('\''),
            b"nbsp" => Some(' '),
            b"mdash" => Some('—'),
            b"ndash" => Some('–'),
            b"hellip" => Some('…'),
            b"rsquo" => Some('’'),
            b"lsquo" => Some('‘'),
            b"rdquo" => Some('”'),
            b"ldquo" => Some('“'),
            b"copy" => Some('©'),
            b"reg" => Some('®'),
            b"trade" => Some('™'),
            b"middot" => Some('·'),
            b"bull" => Some('•'),
            b"times" => Some('×'),
            b"rarr" => Some('→'),
            b"larr" => Some('←'),
            b"deg" => Some('°'),
            b"euro" => Some('€'),
            b"pound" => Some('£'),
            b"laquo" => Some('«'),
            b"eacute" => Some('é'),
            b"egrave" => Some('è'),
            b"aacute" => Some('á'),
            b"agrave" => Some('à'),
            b"iacute" => Some('í'),
            b"oacute" => Some('ó'),
            b"uacute" => Some('ú'),
            b"auml" => Some('ä'),
            b"ouml" => Some('ö'),
            b"uuml" => Some('ü'),
            b"szlig" => Some('ß'),
            b"ntilde" => Some('ñ'),
            b"ccedil" => Some('ç'),
            b"raquo" => Some('»'),
            b"shy" | b"zwj" | b"zwnj" => Some('\u{200B}'),
            [b'#', b'x' | b'X', h @ ..] => std::str::from_utf8(h).ok().and_then(|h| u32::from_str_radix(h, 16).ok()).and_then(char::from_u32),
            [b'#', d @ ..] => std::str::from_utf8(d).ok().and_then(|d| d.parse().ok()).and_then(char::from_u32),
            _ => None,
        };
        match ch {
            Some('\u{200B}') => {}
            Some(c) => {
                let c = if c == '\u{a0}' { ' ' } else { c };
                let mut b = [0u8; 4];
                self.put(c.encode_utf8(&mut b).as_bytes(), sink);
            }
            None => {
                self.put(b"&", sink);
                for i in 0..n {
                    self.put(&[e[i]], sink);
                }
                self.put(b";", sink);
            }
        }
    }

    fn put_text_bytes(&mut self, b: &[u8], sink: &mut dyn PageSink) {
        // whitespace one byte at a time, everything else as utf-8 runs
        let mut i = 0;
        while i < b.len() {
            let c = b[i];
            if matches!(c, b' ' | b'\n' | b'\r' | b'\t' | 0x0c) {
                self.put(&b[i..i + 1], sink);
                i += 1;
                continue;
            }
            let start = i;
            while i < b.len() && !matches!(b[i], b' ' | b'\n' | b'\r' | b'\t' | 0x0c) {
                i += 1;
            }
            self.put(&b[start..i], sink);
        }
    }

    pub fn feed(&mut self, data: &[u8], sink: &mut dyn PageSink) {
        let mut i = 0;
        while i < data.len() {
            let c = data[i];
            match self.st {
                St::Text => {
                    let start = i;
                    while i < data.len() && data[i] != b'<' && data[i] != b'&' {
                        i += 1;
                    }
                    if i > start {
                        self.put_text_bytes(&data[start..i], sink);
                    }
                    if i < data.len() {
                        if data[i] == b'<' {
                            self.st = St::TagOpen;
                        } else {
                            self.st = St::Ent;
                            self.ent_len = 0;
                        }
                        i += 1;
                    }
                }
                St::Ent => {
                    if c == b';' {
                        i += 1;
                        self.st = St::Text;
                        self.decode_entity(sink);
                    } else if (c.is_ascii_alphanumeric() || c == b'#') && self.ent_len < self.ent.len() {
                        self.ent[self.ent_len] = c;
                        self.ent_len += 1;
                        i += 1;
                    } else {
                        // not an entity: the '&' and what followed are text
                        self.st = St::Text;
                        self.put(b"&", sink);
                        let e = self.ent;
                        let n = self.ent_len;
                        self.put_text_bytes(&e[..n], sink);
                    }
                }
                St::TagOpen => {
                    self.tag_len = 0;
                    self.href_len = 0;
                    self.alt_len = 0;
                    self.class_len = 0;
                    self.role_len = 0;
                    self.hidden = false;
                    if c == b'/' {
                        self.end_tag = true;
                        self.st = St::TagName;
                        i += 1;
                    } else if c == b'!' {
                        self.st = St::Bang;
                        self.dashes = 0;
                        i += 1;
                    } else if c.is_ascii_alphabetic() {
                        self.end_tag = false;
                        self.st = St::TagName;
                    } else {
                        self.st = St::Text;
                        self.put(b"<", sink);
                    }
                }
                St::TagName => {
                    i += 1;
                    if c.is_ascii_alphanumeric() {
                        if self.tag_len < NAME {
                            self.tag[self.tag_len] = c;
                            self.tag_len += 1;
                        }
                    } else if c == b'>' {
                        self.st = St::Text;
                        self.on_tag(sink);
                    } else {
                        self.st = St::BeforeAttr;
                    }
                }
                St::BeforeAttr => {
                    i += 1;
                    if c == b'>' {
                        self.st = St::Text;
                        self.on_tag(sink);
                    } else if !c.is_ascii_whitespace() && c != b'/' {
                        self.attr[0] = c.to_ascii_lowercase();
                        self.attr_len = 1;
                        self.st = St::AttrName;
                    }
                }
                St::AttrName => {
                    i += 1;
                    if c == b'=' {
                        self.start_value();
                        self.st = St::BeforeValue;
                    } else if c == b'>' {
                        self.bare_attr();
                        self.st = St::Text;
                        self.on_tag(sink);
                    } else if c.is_ascii_whitespace() {
                        self.st = St::AfterAttrName;
                    } else if self.attr_len < NAME {
                        self.attr[self.attr_len] = c.to_ascii_lowercase();
                        self.attr_len += 1;
                    }
                }
                St::AfterAttrName => {
                    if c == b'=' {
                        i += 1;
                        self.start_value();
                        self.st = St::BeforeValue;
                    } else if c.is_ascii_whitespace() {
                        i += 1;
                    } else {
                        self.bare_attr();
                        self.st = St::BeforeAttr;
                    }
                }
                St::BeforeValue => {
                    i += 1;
                    if c == b'"' || c == b'\'' {
                        self.quote = c;
                        self.st = St::ValueQ;
                    } else if c == b'>' {
                        self.st = St::Text;
                        self.on_tag(sink);
                    } else if !c.is_ascii_whitespace() {
                        self.val_byte(c);
                        self.st = St::ValueU;
                    }
                }
                St::ValueQ => {
                    i += 1;
                    if c == self.quote {
                        self.end_value();
                        self.st = St::BeforeAttr;
                    } else {
                        self.val_byte(c);
                    }
                }
                St::ValueU => {
                    i += 1;
                    if c.is_ascii_whitespace() {
                        self.end_value();
                        self.st = St::BeforeAttr;
                    } else if c == b'>' {
                        self.end_value();
                        self.st = St::Text;
                        self.on_tag(sink);
                    } else {
                        self.val_byte(c);
                    }
                }
                St::Bang => {
                    i += 1;
                    if c == b'-' {
                        self.dashes += 1;
                        if self.dashes == 2 {
                            self.st = St::Comment;
                            self.dashes = 0;
                        }
                    } else if c == b'>' {
                        self.st = St::Text;
                    } else {
                        self.dashes = 3; // a doctype or cdata: wait for '>'
                    }
                }
                St::Comment => {
                    i += 1;
                    if c == b'-' {
                        self.dashes = (self.dashes + 1).min(2);
                    } else if c == b'>' && self.dashes == 2 {
                        self.st = St::Text;
                    } else {
                        self.dashes = 0;
                    }
                }
                St::Raw => {
                    // look for "</name" (case-insensitive); title text is kept
                    i += 1;
                    let want: &[u8] = match self.raw_match {
                        0 => b"<",
                        1 => b"/",
                        k => &self.raw[k - 2..k - 1],
                    };
                    if c.to_ascii_lowercase() == want[0] {
                        self.raw_match += 1;
                        if self.raw_match == self.raw_len + 2 {
                            self.raw_match = 0;
                            self.raw_title = false;
                            self.st = St::Bang; // skip to the closing '>'
                            self.dashes = 3;
                        }
                    } else {
                        if self.raw_title {
                            let k = self.raw_match;
                            if k > 0 {
                                let mut pre = [0u8; NAME + 2];
                                pre[0] = b'<';
                                if k > 1 {
                                    pre[1] = b'/';
                                    pre[2..k].copy_from_slice(&self.raw[..k - 2]);
                                }
                                sink.title(&pre[..k]);
                            }
                            sink.title(&[c]);
                        }
                        self.raw_match = if c == b'<' { 1 } else { 0 };
                    }
                }
            }
        }
        self.flush(sink);
    }

    pub fn finish(&mut self, sink: &mut dyn PageSink) {
        self.end_block(sink);
    }

    fn start_value(&mut self) {
        let a = &self.attr[..self.attr_len];
        self.capture = match a {
            b"href" => 1,
            b"alt" => 2,
            b"class" | b"id" => 3,
            b"role" => 4,
            b"aria-hidden" => 5,
            _ => 0,
        };
        self.val_len = 0;
    }

    /// An attribute with no value, like `hidden`.
    fn bare_attr(&mut self) {
        if &self.attr[..self.attr_len] == b"hidden" {
            self.hidden = true;
        }
    }

    /// Page chrome a reader does not want: menus, footers, edit links, hidden things.
    fn chrome(&self, t: &[u8]) -> bool {
        if is(t, SKIP_TAGS) || is(t, &["nav", "footer", "aside"]) || self.hidden {
            return true;
        }
        let role = &self.role[..self.role_len];
        if matches!(role, b"navigation" | b"banner" | b"contentinfo" | b"search" | b"complementary" | b"dialog") {
            return true;
        }
        let class = &self.class[..self.class_len];
        SKIP_CLASSES.iter().any(|c| class.windows(c.len()).any(|w| w.eq_ignore_ascii_case(c.as_bytes())))
    }

    fn val_byte(&mut self, c: u8) {
        if self.capture != 0 && self.val_len < HREF {
            self.val[self.val_len] = c;
            self.val_len += 1;
        }
    }

    fn end_value(&mut self) {
        match self.capture {
            1 => {
                self.href[..self.val_len].copy_from_slice(&self.val[..self.val_len]);
                self.href_len = self.val_len;
                // &amp; in an href is an ampersand
                let mut w = 0;
                let mut r = 0;
                while r < self.href_len {
                    if self.href[r..self.href_len].starts_with(b"&amp;") {
                        self.href[w] = b'&';
                        r += 5;
                    } else {
                        self.href[w] = self.href[r];
                        r += 1;
                    }
                    w += 1;
                }
                self.href_len = w;
            }
            2 => {
                let n = self.val_len.min(self.alt.len());
                self.alt[..n].copy_from_slice(&self.val[..n]);
                self.alt_len = n;
            }
            3 => {
                // class and id are matched together
                let room = self.class.len() - self.class_len;
                let n = (self.val_len + 1).min(room);
                if n > 0 {
                    self.class[self.class_len] = b' ';
                    self.class[self.class_len + 1..self.class_len + n].copy_from_slice(&self.val[..n - 1]);
                    self.class_len += n;
                }
            }
            4 => {
                let n = self.val_len.min(self.role.len());
                for k in 0..n {
                    self.role[k] = self.val[k].to_ascii_lowercase();
                }
                self.role_len = n;
            }
            5 => {
                if self.val[..self.val_len].eq_ignore_ascii_case(b"true") {
                    self.hidden = true;
                }
            }
            _ => {}
        }
        self.capture = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Default)]
    struct Doc {
        blocks: Vec<(u8, u8, String)>,
        links: Vec<(String, String)>,
        title: String,
        cur: String,
        link: Option<(usize, String)>,
    }

    impl PageSink for Doc {
        fn block_start(&mut self, k: u8, d: u8) {
            self.blocks.push((k, d, String::new()));
            self.cur.clear();
        }
        fn text(&mut self, b: &[u8]) {
            self.cur.push_str(std::str::from_utf8(b).unwrap());
        }
        fn block_end(&mut self) {
            self.blocks.last_mut().unwrap().2 = self.cur.clone();
        }
        fn link_start(&mut self, href: &[u8]) {
            self.link = Some((self.cur.len(), String::from_utf8_lossy(href).into()));
        }
        fn link_end(&mut self) {
            if let Some((s, h)) = self.link.take() {
                self.links.push((self.cur[s..].to_string(), h));
            }
        }
        fn title(&mut self, b: &[u8]) {
            self.title.push_str(&String::from_utf8_lossy(b));
        }
        fn reset(&mut self) {
            self.blocks.clear();
            self.links.clear();
        }
    }

    fn page(html: &str) -> Doc {
        let mut h = Html::new();
        let mut d = Doc::default();
        h.feed(html.as_bytes(), &mut d);
        h.finish(&mut d);
        d
    }

    #[test]
    fn reader_mode_keeps_the_article() {
        let d = page(r##"<body><a class="mw-jump-link" href="#c">Jump to content</a><nav><ul><li>Main page</li></ul></nav>
<header><p>site header</p></header><main><h1>Title</h1><p>Body <span class="mw-editsection">[edit]</span>text.</p>
<div role="navigation">menu</div><p hidden>secret</p><p aria-hidden="true">icon</p></main><footer>foot</footer><p>after</p></body>"##);
        let texts: Vec<&str> = d.blocks.iter().map(|b| b.2.as_str()).collect();
        assert_eq!(texts, vec!["Title", "Body text."]);
    }

    #[test]
    fn table_rows_stay_on_one_line() {
        let d = page(r#"<table><tr><td class="title"><span class="rank">1.</span></td><td><center><a id="up"><div class="votearrow"></div></a></center></td>
<td class="title"><span class="titleline"><a href="https://a.b">A story</a> (<a href="from?site=a.b">a.b</a>)</span></td></tr>
<tr><td colspan=2></td><td class="subtext">12 points by <a href="user?id=x">x</a></td></tr></table>"#);
        let texts: Vec<&str> = d.blocks.iter().map(|b| b.2.as_str()).collect();
        assert_eq!(texts, vec!["1.   A story (a.b)", "12 points by x"]);
    }

    const PAGE: &str = r#"<!DOCTYPE html><html><head><title>A &amp; B</title><style>p{color:red}</style>
<script>if (a < b) { document.write("<p>no</p>") }</script></head>
<body><!-- a comment > with -- dashes --><h1>Hello   <b>world</b></h1>
<p>Some <a href="/x?a=1&amp;b=2">linked text</a>, then more.<br>Next line &mdash; caf&eacute; &#233; &#x1F600;</p>
<ul><li>one</li><li>two <ul><li>nested</li></ul></li></ul>
<pre>fn main() {
    println!("hi");
}</pre><blockquote>quoted</blockquote><svg><text>hidden</text></svg>
<table><tr><td>cell a</td><td>cell b</td></tr></table><img src=x alt="a cat"><hr></body></html>"#;

    fn run(chunks: usize) -> Doc {
        let mut h = Html::new();
        let mut d = Doc::default();
        for c in PAGE.as_bytes().chunks(chunks) {
            h.feed(c, &mut d);
        }
        h.finish(&mut d);
        d
    }

    #[test]
    fn extracts_readable_blocks() {
        let d = run(4096);
        assert_eq!(d.title, "A &amp; B");
        let texts: Vec<(u8, &str)> = d.blocks.iter().map(|(k, _, t)| (*k, t.as_str())).collect();
        assert_eq!(
            texts,
            vec![
                (1, "Hello world"),
                (K_P, "Some linked text, then more.\nNext line — café é 😀"),
                (K_LI, "one"),
                (K_LI, "two"),
                (K_LI, "nested"),
                (K_PRE, "fn main() {\n    println!(\"hi\");\n}"),
                (K_QUOTE, "quoted"),
                (K_ROW, "cell a   cell b"),
                (K_P, "[a cat]"),
                (K_HR, ""),
            ]
        );
        assert_eq!(d.blocks[4].1, 2, "nested list depth");
        assert_eq!(d.links, vec![("linked text".to_string(), "/x?a=1&b=2".to_string())]);
    }

    #[test]
    fn any_chunking_gives_the_same_page() {
        let whole = run(4096);
        for n in 1..9 {
            let d = run(n);
            assert_eq!(d.blocks, whole.blocks, "chunk {n}");
            assert_eq!(d.links, whole.links, "chunk {n}");
            assert_eq!(d.title, whole.title, "chunk {n}");
        }
    }

    #[test]
    fn garbage_does_not_panic() {
        let mut h = Html::new();
        let mut d = Doc::default();
        h.feed(b"<<<>>><a href='x'<<&&&;;#<!--<!--><script></scrip></script><p", &mut d);
        h.finish(&mut d);
    }
}
