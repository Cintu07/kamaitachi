//! Boxes to positions. Block formatting contexts stack boxes and place floats; runs of
//! inline content become paragraphs that the text engine breaks into lines; flex, grid
//! (as equal columns) and tables have their own passes. Everything is recorded straight
//! into a display list; a box's background is reserved before its children and filled
//! in once its height is known, so paint order stays tree order.

use super::dom::{t, Dom, NodeId, NONE};
use super::style::{Display, GLine, GridDef, Rgba, Style, Styles, Track, Ts, L};
use super::{DisplayList, Field, Font, Item, Link, Obj, Para, Rect, Run, TextEngine};
use crate::url::Url;
use std::collections::HashMap;
use std::sync::OnceLock;

/// Images a page refers to, by url, with their natural sizes once known.
#[derive(Default, Clone)]
pub struct Images {
    pub urls: Vec<String>,
    pub sizes: Vec<Option<(f32, f32)>>,
    index: HashMap<String, u32>,
}

impl Images {
    pub fn id(&mut self, url: &str) -> u32 {
        if let Some(&i) = self.index.get(url) {
            return i;
        }
        let i = self.urls.len() as u32;
        self.urls.push(url.to_string());
        self.sizes.push(None);
        self.index.insert(url.to_string(), i);
        i
    }
    pub fn size(&self, id: u32) -> Option<(f32, f32)> {
        self.sizes.get(id as usize).copied().flatten()
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Child {
    Text(NodeId),
    Elem(NodeId),
    Before(NodeId),
    After(NodeId),
}

enum Class {
    Skip,
    Inline,
    Block(NodeId),
    Float(NodeId),
    Abs(NodeId),
}

#[derive(Default)]
struct Floats {
    /// (right edge, bottom) of left floats
    l: Vec<(f32, f32)>,
    /// (left edge, bottom) of right floats
    r: Vec<(f32, f32)>,
}

impl Floats {
    fn band(&self, y: f32, x0: f32, x1: f32) -> (f32, f32) {
        let l = self.l.iter().filter(|f| f.1 > y).map(|f| f.0).fold(x0, f32::max);
        let r = self.r.iter().filter(|f| f.1 > y).map(|f| f.0).fold(x1, f32::min);
        (l.min(x1), r.max(l.min(x1)))
    }
    fn clear(&self, which: u8) -> f32 {
        let mut y = f32::MIN;
        if which & 1 != 0 {
            y = self.l.iter().map(|f| f.1).fold(y, f32::max);
        }
        if which & 2 != 0 {
            y = self.r.iter().map(|f| f.1).fold(y, f32::max);
        }
        y
    }
    fn bottom(&self) -> f32 {
        self.clear(3)
    }
    fn next_bottom(&self, y: f32) -> Option<f32> {
        self.l.iter().chain(self.r.iter()).map(|f| f.1).filter(|b| *b > y).reduce(f32::min)
    }
}

#[derive(Clone, Copy, Default, Debug)]
struct Mark {
    items: usize,
    glyphs: usize,
    links: usize,
    clips: usize,
    boxes: usize,
    paras: usize,
}

#[derive(Clone, Copy, Default, Debug)]
struct BoxOut {
    /// border box
    x: f32,
    y: f32,
    w: f32,
    h: f32,
    mt: f32,
    mb: f32,
    ml: f32,
    mr: f32,
    deco: Deco,
    start: Mark,
    content: Mark,
    /// just past the box's last item
    end: Mark,
}

#[derive(Clone, Copy, Default, Debug)]
struct Deco {
    bg: Option<usize>,
    bg_img: Option<(usize, u32)>,
    ring: Option<usize>,
    /// first of four border rects: top, right, bottom, left
    sides: Option<usize>,
}

#[derive(Clone, Copy, Default)]
struct Opt {
    /// border-box width decided by the caller (flex item, table cell, float)
    width: Option<f32>,
    /// border-box height decided by the caller
    height: Option<f32>,
    /// containing block height when definite
    cbh: Option<f32>,
}

struct Pb {
    para: Para,
    objs: Vec<ObjKind>,
    last_space: bool,
    abs: Vec<NodeId>,
    visible: bool,
}

enum ObjKind {
    Img(u32, f32, f32),
    Boxed(DisplayList),
    Blank,
}

fn fallback_style() -> &'static Style {
    static S: OnceLock<Style> = OnceLock::new();
    S.get_or_init(super::style::initial)
}

pub struct Layout<'a> {
    /// keep every box in out.boxes
    record: bool,
    dom: &'a Dom,
    st: &'a Styles,
    text: &'a mut dyn TextEngine,
    pub out: DisplayList,
    img: &'a mut Images,
    base: &'a Url,
    vw: f32,
    vh: f32,
    /// abspos boxes waiting for their containing block: (node, static x, static y, static width)
    abs: Vec<Vec<(NodeId, f32, f32, f32)>>,
    intr: HashMap<NodeId, (f32, f32)>,
    blockish: HashMap<NodeId, bool>,
    canvas: NodeId,
    root: NodeId,
    depth: u32,
}

/// Lays out a document for a viewport `vw` css px wide.
pub fn layout(dom: &Dom, st: &Styles, text: &mut dyn TextEngine, img: &mut Images, base: &Url, vw: f32, vh: f32) -> DisplayList {
    run(dom, st, text, img, base, vw, vh, false)
}

/// The same, with every box it placed in `boxes` and every paragraph in `paras`, so a
/// point can be traced back to an element and words found on the page. Only the hiding
/// picker and find want that, and only while they are open.
pub fn layout_boxes(dom: &Dom, st: &Styles, text: &mut dyn TextEngine, img: &mut Images, base: &Url, vw: f32, vh: f32) -> DisplayList {
    run(dom, st, text, img, base, vw, vh, true)
}

#[allow(clippy::too_many_arguments)]
fn run(dom: &Dom, st: &Styles, text: &mut dyn TextEngine, img: &mut Images, base: &Url, vw: f32, vh: f32, record: bool) -> DisplayList {
    let html = dom.find(t::html).unwrap_or(0);
    let mut lx = Layout { record, dom, st, text, out: DisplayList::new(), img, base, vw, vh, abs: vec![Vec::new()], intr: HashMap::new(), blockish: HashMap::new(), canvas: NONE, root: html, depth: 0 };
    let body = dom.find(t::body).unwrap_or(NONE);
    let hs = lx.style(html);
    if hs.bg[3] > 0 {
        lx.out.bg = hs.bg;
        lx.canvas = html;
    } else if body != NONE && lx.style(body).bg[3] > 0 {
        lx.out.bg = lx.style(body).bg;
        lx.canvas = body;
    }
    if lx.out.bg[3] < 255 {
        // the canvas is opaque; blend over white
        let c = lx.out.bg;
        let a = c[3] as f32 / 255.0;
        let mix = |v: u8| (v as f32 * a + 255.0 * (1.0 - a)) as u8;
        lx.out.bg = [mix(c[0]), mix(c[1]), mix(c[2]), 255];
    }
    let mut floats = Floats::default();
    let b = lx.block(html, 0.0, 0.0, vw, Opt { cbh: Some(vh), ..Default::default() }, &mut floats);
    let mut height = (b.y + b.h + b.mb).max(floats.bottom());
    let root_abs = lx.abs.pop().unwrap_or_default();
    for (n, sx, sy, sw) in root_abs {
        let ob = lx.abspos(n, sx, sy, sw, Rect::new(0.0, 0.0, vw, vh));
        height = height.max(ob.y + ob.h);
    }
    let mut title = String::new();
    if let Some(tn) = dom.find(t::title) {
        dom.text_content(tn, &mut title);
    }
    lx.out.title = title.split_whitespace().collect::<Vec<_>>().join(" ");
    lx.out.height = height.max(0.0);
    lx.out.width = vw;
    lx.out
}

fn collapse(a: f32, b: f32) -> f32 {
    if a >= 0.0 && b >= 0.0 {
        a.max(b)
    } else if a < 0.0 && b < 0.0 {
        a.min(b)
    } else {
        a + b
    }
}

/// Starts the address of something a page answers itself, see `goes_to`.
pub const PRESS: &str = "press:";

/// The grey of a hint in an empty box.
pub const PLACEHOLDER: Rgba = [0x75, 0x75, 0x75, 255];

fn font_of(s: &Style) -> Font {
    Font { family: s.font_family.clone(), size: s.font_size, weight: s.font_weight, italic: s.italic }
}

/// How far down a box on a line meets the line's baseline, given where its own is.
fn hang(s: &Style, h: f32, own: f32, pb: &Pb) -> f32 {
    let size = pb.para.runs.last().map_or(s.font_size, |r| r.font.size);
    match s.valign {
        2 => h / 2.0 + size * 0.25,
        1 => (size * 0.9).min(h),
        3 => h - size * 0.22,
        4 => own - size * 0.2,
        5 => own + size * 0.33,
        _ => own,
    }
}

fn is_replaced(tag: u32) -> bool {
    matches!(tag, t::img | t::svg | t::input | t::select | t::textarea | t::canvas | t::video | t::iframe)
}

fn mul_alpha(c: Rgba, k: f32) -> Rgba {
    [c[0], c[1], c[2], (c[3] as f32 * k) as u8]
}

impl<'a> Layout<'a> {
    fn style(&self, n: NodeId) -> &'a Style {
        let st: &'a Styles = self.st;
        st.get(n).unwrap_or_else(|| fallback_style())
    }

    fn mark(&self) -> Mark {
        Mark { items: self.out.items.len(), glyphs: self.out.glyphs.len(), links: self.out.links.len(), clips: self.out.clips.len(), boxes: self.out.boxes.len(), paras: self.out.paras.len() }
    }

    /// Children in box order: ::before, the element's children (display: contents
    /// flattened), ::after.
    fn kids(&self, n: NodeId, out: &mut Vec<Child>) {
        if self.st.before.contains_key(&n) {
            out.push(Child::Before(n));
        }
        for c in self.dom.children(n) {
            if self.dom.text(c).is_some() {
                out.push(Child::Text(c));
            } else if self.dom.elem(c).is_some() {
                let s = self.style(c);
                if s.display == Display::Contents {
                    self.kids(c, out);
                } else {
                    out.push(Child::Elem(c));
                }
            }
        }
        if self.st.after.contains_key(&n) {
            out.push(Child::After(n));
        }
    }

    fn classify(&mut self, c: Child) -> Class {
        let Child::Elem(e) = c else { return Class::Inline };
        let s = self.style(e);
        if s.display == Display::None || s.display == Display::TableColumn || s.gone {
            return Class::Skip;
        }
        if matches!(s.position, 2 | 3) {
            return Class::Abs(e);
        }
        if s.float != 0 {
            return Class::Float(e);
        }
        if s.display.inline_level() {
            if s.display == Display::Inline && self.has_block(e) {
                return Class::Block(e);
            }
            return Class::Inline;
        }
        Class::Block(e)
    }

    /// An inline element with block-level children is laid out as a block.
    fn has_block(&mut self, n: NodeId) -> bool {
        if let Some(&b) = self.blockish.get(&n) {
            return b;
        }
        let mut kids = Vec::new();
        self.kids(n, &mut kids);
        let mut r = false;
        for c in kids {
            if let Child::Elem(e) = c {
                let s = self.style(e);
                if s.display == Display::None || matches!(s.position, 2 | 3) || s.float != 0 {
                    continue;
                }
                if !s.display.inline_level() || (s.display == Display::Inline && self.has_block(e)) {
                    r = true;
                    break;
                }
            }
        }
        self.blockish.insert(n, r);
        r
    }

    fn margins(&self, s: &Style, cw: f32) -> ([f32; 4], [bool; 4]) {
        let mut m = [0.0; 4];
        let mut auto = [false; 4];
        for k in 0..4 {
            match s.margin[k].get(cw) {
                Some(v) => m[k] = v,
                None => auto[k] = s.margin[k].is_auto(),
            }
        }
        (m, auto)
    }

    fn padding(&self, s: &Style, cw: f32) -> [f32; 4] {
        [0, 1, 2, 3].map(|k| s.padding[k].or0(cw).max(0.0))
    }

    /// Lays out a block-level box whose margin box starts at (x, y) inside a containing
    /// block `cw` wide.
    fn block(&mut self, n: NodeId, x: f32, y: f32, cw: f32, opt: Opt, floats: &mut Floats) -> BoxOut {
        self.depth += 1;
        let mut r = if self.depth > 300 { BoxOut { x, y, ..Default::default() } } else { self.block_inner(n, x, y, cw, opt, floats) };
        self.depth -= 1;
        r.end = self.mark();
        r
    }

    fn block_inner(&mut self, n: NodeId, x: f32, y: f32, cw: f32, opt: Opt, floats: &mut Floats) -> BoxOut {
        let s = self.style(n);
        let (mut m, auto) = self.margins(s, cw);
        let p = self.padding(s, cw);
        let b = s.bw;
        let pbh = p[1] + p[3] + b[1] + b[3];
        let pbv = p[0] + p[2] + b[0] + b[2];
        let tag = self.dom.tag(n);
        let replaced = is_replaced(tag);
        let shrink = matches!(s.display, Display::InlineBlock | Display::InlineFlex | Display::InlineGrid | Display::InlineTable | Display::Table)
            || s.float != 0
            || matches!(s.position, 2 | 3);
        let mut rsize = None;
        let mut w = match opt.width {
            Some(fw) => fw,
            None => {
                let spec = s.width.get(cw).map(|v| if s.border_box { v } else { v + pbh });
                match spec {
                    Some(v) => v,
                    None if replaced => {
                        let rs = self.replaced_size(n, s, cw, pbh, pbv);
                        rsize = Some(rs);
                        rs.0 + pbh
                    }
                    None => {
                        let avail = cw - m[1] - m[3];
                        if shrink {
                            let (mn, mx) = self.intrinsic(n);
                            mx.min(avail).max(mn)
                        } else {
                            avail
                        }
                    }
                }
            }
        };
        if opt.width.is_none() {
            if let Some(mx) = s.max_w.get(cw) {
                w = w.min(if s.border_box { mx } else { mx + pbh });
            }
            if let Some(mn) = s.min_w.get(cw) {
                w = w.max(if s.border_box { mn } else { mn + pbh });
            }
        }
        w = w.max(pbh);
        if opt.width.is_none() && s.float == 0 && !matches!(s.position, 2 | 3) {
            // auto margins center a block with a definite width
            let free = cw - w - m[1] - m[3];
            if auto[1] && auto[3] {
                m[1] = (free / 2.0).max(0.0);
                m[3] = (free / 2.0).max(0.0);
            } else if auto[3] {
                m[3] = free.max(0.0);
            }
        }
        let bx = x + m[3];
        let by = y + m[0];
        let cx = bx + b[3] + p[3];
        let cy = by + b[0] + p[0];
        let content_w = (w - pbh).max(0.0);
        let start = self.mark();
        let deco = self.reserve_deco(n, s);
        let content = self.mark();
        let positioned = s.position != 0;
        if positioned {
            self.abs.push(Vec::new());
        }
        let spec_h = opt.height.map(|h| (h - pbv).max(0.0)).or_else(|| {
            s.height.get(opt.cbh.unwrap_or(f32::NAN)).filter(|h| h.is_finite()).map(|h| if s.border_box { (h - pbv).max(0.0) } else { h })
        });
        let child_cbh = spec_h;
        let mut own = Floats::default();
        let bfc = s.overflow != 0 || shrink || matches!(s.display, Display::TableCell | Display::Flex | Display::Grid) || opt.width.is_some() || n == self.root;
        let fl: &mut Floats = if bfc { &mut own } else { floats };
        let mut ch = if replaced {
            let (rw, rh) = rsize.unwrap_or_else(|| {
                let (rw, rh) = self.replaced_size(n, s, cw, pbh, pbv);
                (content_w.min(rw.max(content_w)), rh)
            });
            let rw = if opt.width.is_some() || s.width.get(cw).is_some() { content_w } else { rw };
            let rh = spec_h.unwrap_or(rh);
            self.replaced_content(n, s, Rect::new(cx, cy, rw, rh));
            rh
        } else {
            match s.display {
                Display::Flex | Display::InlineFlex => self.flex(n, s, cx, cy, content_w, spec_h),
                Display::Grid | Display::InlineGrid => self.grid(n, s, cx, cy, content_w),
                Display::Table | Display::InlineTable => self.table(n, s, cx, cy, content_w),
                _ => self.flow(n, s, cx, cy, content_w, child_cbh, fl),
            }
        };
        if bfc {
            ch = ch.max(own.bottom() - cy);
        }
        let mut h = match spec_h {
            Some(hh) => hh,
            None => ch,
        };
        if let Some(mx) = s.max_h.get(opt.cbh.unwrap_or(f32::NAN)).filter(|v| v.is_finite()) {
            h = h.min(if s.border_box { (mx - pbv).max(0.0) } else { mx });
        }
        if let Some(mn) = s.min_h.get(opt.cbh.unwrap_or(f32::NAN)).filter(|v| v.is_finite()) {
            h = h.max(if s.border_box { (mn - pbv).max(0.0) } else { mn });
        }
        if let Some(a) = s.aspect
            && spec_h.is_none()
            && ch == 0.0
            && !replaced
        {
            h = content_w / a;
        }
        let bh = h + pbv;
        let mut bo = BoxOut { x: bx, y: by, w, h: bh, mt: m[0], mb: m[2], ml: m[3], mr: m[1], deco, start, content, end: start };
        self.fill_deco(n, s, &bo);
        if s.display == Display::ListItem && s.list_style != 0 {
            self.marker(n, s, cx, cy);
        }
        if let Some(u) = self.goes_to(n) {
            let i = self.out.hrefs.len() as u32;
            self.out.hrefs.push(u);
            self.out.links.push(Link { r: Rect::new(bx, by, w, bh), href: i });
        }
        if positioned {
            let pad = Rect::new(bx + b[3], by + b[0], w - b[1] - b[3], bh - b[0] - b[2]);
            let q = self.abs.pop().unwrap_or_default();
            for (an, sx, sy, sw) in q {
                self.abspos(an, sx, sy, sw, pad);
            }
        }
        if s.overflow != 0 {
            let clip = Rect::new(bx + b[3], by + b[0], (w - b[1] - b[3]).max(0.0), (bh - b[0] - b[2]).max(0.0));
            self.clip_range(content, clip);
        }
        if !s.visible {
            self.out.items.truncate(content.items);
            self.out.links.truncate(content.links);
            self.out.paras.truncate(content.paras);
        }
        if s.opacity < 1.0 {
            self.fade_range(start, s.opacity);
        }
        if s.position == 1 {
            let dx = s.inset[3].get(cw).or_else(|| s.inset[1].get(cw).map(|r| -r)).unwrap_or(0.0);
            let dy = s.inset[0].get(cw).or_else(|| s.inset[2].get(cw).map(|v| -v)).unwrap_or(0.0);
            if dx != 0.0 || dy != 0.0 {
                self.translate(start, dx, dy);
                bo.x += dx;
                bo.y += dy;
            }
        }
        if self.record && s.visible {
            self.out.boxes.push((n, Rect::new(bo.x, bo.y, bo.w, bo.h)));
        }
        bo
    }

    fn resolve(&self, href: &str) -> Option<String> {
        let h = href.trim();
        if h.starts_with("data:") {
            return Some(h.to_string());
        }
        self.base.join(h).map(|u| u.full())
    }

    /// What clicking an element does: a link's address, or `press:` and the node number
    /// for something the page itself answers. A page cannot write a link that becomes
    /// one of those: every address it can write starts with https or data.
    fn goes_to(&self, n: NodeId) -> Option<String> {
        match self.dom.tag(n) {
            t::a => self.resolve(self.dom.attr(n, t::href)?),
            t::button | t::label | t::summary => Some(format!("{PRESS}{n}")),
            _ => None,
        }
    }

    fn reserve_deco(&mut self, n: NodeId, s: &Style) -> Deco {
        let mut d = Deco::default();
        let z = Rect::default();
        if s.bg[3] > 0 && n != self.canvas && !s.masked {
            d.bg = Some(self.out.items.len());
            self.out.items.push(Item::Rect { r: z, color: s.bg, radius: 0.0, clip: 0 });
        }
        if let Some(u) = &s.bg_image
            && let Some(url) = self.resolve(u)
        {
            let id = self.img.id(&url);
            d.bg_img = Some((self.out.items.len(), id));
            self.out.items.push(Item::Image { r: z, img: id, alpha: 0, radius: 0.0, clip: 0 });
        }
        if s.bw.iter().any(|w| *w > 0.0) {
            let uniform = s.bw.iter().all(|w| *w == s.bw[0]) && s.bc.iter().all(|c| *c == s.bc[0]);
            if uniform && s.radius != L::ZERO {
                d.ring = Some(self.out.items.len());
                self.out.items.push(Item::Ring { r: z, width: 0.0, color: [0; 4], radius: 0.0, clip: 0 });
            } else {
                d.sides = Some(self.out.items.len());
                for _ in 0..4 {
                    self.out.items.push(Item::Rect { r: z, color: [0; 4], radius: 0.0, clip: 0 });
                }
            }
        }
        d
    }

    fn fill_deco(&mut self, n: NodeId, s: &Style, bo: &BoxOut) {
        let _ = n;
        let r = Rect::new(bo.x, bo.y, bo.w, bo.h);
        let radius = s.radius.or0(bo.w).min(bo.w / 2.0).min(bo.h / 2.0).max(0.0);
        if let Some(i) = bo.deco.bg {
            self.out.items[i] = Item::Rect { r, color: s.bg, radius, clip: 0 };
        }
        if let Some((i, id)) = bo.deco.bg_img {
            let ir;
            let mut clip = 0;
            if let Some((nw, nh)) = self.img.size(id)
                && nw > 0.0
                && nh > 0.0
            {
                if nw >= r.w && nh >= r.h {
                    // a photo behind a box: cover it
                    let k = (r.w / nw).max(r.h / nh);
                    ir = Rect::new(r.x + (r.w - nw * k) / 2.0, r.y + (r.h - nh * k) / 2.0, nw * k, nh * k);
                } else {
                    ir = Rect::new(r.x, r.y, nw, nh);
                }
                self.out.clips.push(r);
                clip = (self.out.clips.len() - 1) as u16;
            } else {
                ir = Rect::default();
            }
            self.out.items[i] = Item::Image { r: ir, img: id, alpha: 255, radius: 0.0, clip };
        }
        let col = |k: usize| s.bc[k].unwrap_or(s.color);
        if let Some(i) = bo.deco.ring {
            self.out.items[i] = Item::Ring { r, width: s.bw[0], color: col(0), radius, clip: 0 };
        }
        if let Some(i) = bo.deco.sides {
            let b = s.bw;
            let rects = [
                Rect::new(r.x, r.y, r.w, b[0]),
                Rect::new(r.x + r.w - b[1], r.y, b[1], r.h),
                Rect::new(r.x, r.y + r.h - b[2], r.w, b[2]),
                Rect::new(r.x, r.y, b[3], r.h),
            ];
            for k in 0..4 {
                self.out.items[i + k] = Item::Rect { r: rects[k], color: if b[k] > 0.0 { col(k) } else { [0; 4] }, radius: 0.0, clip: 0 };
            }
        }
    }

    fn resize(&mut self, n: NodeId, bo: &mut BoxOut, h: f32) {
        bo.h = h;
        let s = self.style(n);
        self.fill_deco(n, s, bo);
        // its own box was recorded at the old size, and a move of its contents took it along
        let r = Rect::new(bo.x, bo.y, bo.w, h);
        for b in self.out.boxes[bo.start.boxes..bo.end.boxes].iter_mut().filter(|b| b.0 == n) {
            b.1 = r;
        }
    }

    fn translate(&mut self, from: Mark, dx: f32, dy: f32) {
        let to = self.mark();
        self.shift(from, to, dx, dy);
    }

    /// Moves what was recorded between two marks.
    fn shift(&mut self, from: Mark, to: Mark, dx: f32, dy: f32) {
        for it in &mut self.out.items[from.items..to.items] {
            match it {
                Item::Rect { r, .. } | Item::Ring { r, .. } | Item::Image { r, .. } | Item::Field { r, .. } => {
                    r.x += dx;
                    r.y += dy;
                }
                Item::Text { .. } => {}
            }
        }
        for g in &mut self.out.glyphs[from.glyphs..to.glyphs] {
            g.x += dx;
            g.y += dy;
        }
        for l in &mut self.out.links[from.links..to.links] {
            l.r.x += dx;
            l.r.y += dy;
        }
        for c in &mut self.out.clips[from.clips.max(1)..to.clips.max(1)] {
            c.x += dx;
            c.y += dy;
        }
        for (_, r) in &mut self.out.boxes[from.boxes..to.boxes] {
            r.x += dx;
            r.y += dy;
        }
        for (_, r) in &mut self.out.paras[from.paras..to.paras] {
            r.x += dx;
            r.y += dy;
        }
    }

    fn clip_range(&mut self, from: Mark, clip: Rect) {
        let mut map: HashMap<u16, u16> = HashMap::new();
        for i in from.items..self.out.items.len() {
            let old = self.out.items[i].clip();
            let new = *map.entry(old).or_insert_with(|| {
                let r = self.out.clips[old as usize].intersect(&clip);
                self.out.clips.push(r);
                (self.out.clips.len() - 1).min(u16::MAX as usize) as u16
            });
            self.out.items[i].set_clip(new);
        }
        let links = &mut self.out.links;
        let mut k = from.links;
        while k < links.len() {
            let r = links[k].r.intersect(&clip);
            if r.w <= 0.0 || r.h <= 0.0 {
                links.remove(k);
            } else {
                links[k].r = r;
                k += 1;
            }
        }
    }

    fn fade_range(&mut self, from: Mark, k: f32) {
        for it in &mut self.out.items[from.items..] {
            match it {
                Item::Rect { color, .. } | Item::Ring { color, .. } | Item::Text { color, .. } => *color = mul_alpha(*color, k),
                Item::Image { alpha, .. } => *alpha = (*alpha as f32 * k) as u8,
                Item::Field { .. } => {}
            }
        }
    }

    /// Places a laid-out fragment (an inline-block drawn into its own list) at (dx, dy).
    fn merge(&mut self, frag: DisplayList, dx: f32, dy: f32) {
        let gbase = self.out.glyphs.len() as u32;
        let cbase = self.out.clips.len() as u16 - 1;
        let hbase = self.out.hrefs.len() as u32;
        self.out.glyphs.extend(frag.glyphs.iter().map(|g| super::Glyph { x: g.x + dx, y: g.y + dy, ..*g }));
        self.out.clips.extend(frag.clips.iter().skip(1).map(|c| Rect::new(c.x + dx, c.y + dy, c.w, c.h)));
        let fbase = self.out.fields.len() as u32;
        self.out.hrefs.extend(frag.hrefs);
        self.out.fields.extend(frag.fields);
        for mut it in frag.items {
            let c = it.clip();
            if c != 0 {
                it.set_clip(c + cbase);
            }
            match &mut it {
                Item::Rect { r, .. } | Item::Ring { r, .. } | Item::Image { r, .. } | Item::Field { r, .. } => {
                    r.x += dx;
                    r.y += dy;
                }
                Item::Text { start, .. } => *start += gbase,
            }
            if let Item::Field { field, .. } = &mut it {
                *field += fbase;
            }
            self.out.items.push(it);
        }
        self.out.boxes.extend(frag.boxes.iter().map(|(n, r)| (*n, Rect::new(r.x + dx, r.y + dy, r.w, r.h))));
        self.out.paras.extend(frag.paras.into_iter().map(|(p, r)| (p, Rect::new(r.x + dx, r.y + dy, r.w, r.h))));
        for l in frag.links {
            self.out.links.push(Link { r: Rect::new(l.r.x + dx, l.r.y + dy, l.r.w, l.r.h), href: l.href + hbase });
        }
    }

    // ---- block flow ----

    #[allow(clippy::too_many_arguments)]
    fn flow(&mut self, parent: NodeId, ps: &Style, x: f32, y: f32, w: f32, cbh: Option<f32>, floats: &mut Floats) -> f32 {
        let mut kids = Vec::new();
        self.kids(parent, &mut kids);
        let mut cursor = y;
        let mut prev_mb = 0.0f32;
        let mut run: Vec<Child> = Vec::new();
        for c in kids {
            match self.classify(c) {
                Class::Skip => {}
                Class::Inline => run.push(c),
                Class::Abs(e) => {
                    if run.is_empty() {
                        self.queue_abs(e, x, cursor + prev_mb, w);
                    } else {
                        run.push(c);
                    }
                }
                Class::Float(e) => {
                    self.flush(&mut run, ps, x, w, &mut cursor, &mut prev_mb, floats);
                    self.float(e, x, cursor + prev_mb, w, floats);
                }
                Class::Block(e) => {
                    self.flush(&mut run, ps, x, w, &mut cursor, &mut prev_mb, floats);
                    let s = self.style(e);
                    if s.clear != 0 {
                        let cy = floats.clear(s.clear);
                        if cy > cursor + prev_mb {
                            cursor = cy;
                            prev_mb = 0.0;
                        }
                    }
                    let (m, _) = self.margins(s, w);
                    let top = cursor + collapse(prev_mb, m[0]) - m[0];
                    let (bx0, bx1) = floats.band(top + m[0], x, x + w);
                    let narrowed = bx0 > x + 0.5 || bx1 < x + w - 0.5;
                    let bo = if narrowed {
                        let mut own = Floats::default();
                        self.block(e, bx0, top, bx1 - bx0, Opt { cbh, ..Default::default() }, &mut own)
                    } else {
                        self.block(e, x, top, w, Opt { cbh, ..Default::default() }, floats)
                    };
                    if bo.h <= 0.0 {
                        prev_mb = collapse(prev_mb, collapse(bo.mt, bo.mb));
                    } else {
                        cursor = bo.y + bo.h;
                        prev_mb = bo.mb;
                    }
                }
            }
        }
        self.flush(&mut run, ps, x, w, &mut cursor, &mut prev_mb, floats);
        cursor + prev_mb - y
    }

    #[allow(clippy::too_many_arguments)]
    fn flush(&mut self, run: &mut Vec<Child>, ps: &Style, x: f32, w: f32, cursor: &mut f32, prev_mb: &mut f32, floats: &mut Floats) {
        if run.is_empty() {
            return;
        }
        let items = std::mem::take(run);
        let top = *cursor + *prev_mb;
        let (bx0, bx1) = floats.band(top, x, x + w);
        let pb = self.build_para(&items, ps, Some(bx1 - bx0));
        for a in &pb.abs {
            self.queue_abs(*a, bx0, top, w);
        }
        if !pb.visible {
            return;
        }
        let h = self.place_para(pb, bx0, top, bx1 - bx0);
        *cursor = top + h;
        *prev_mb = 0.0;
    }

    /// Breaks a paragraph into lines at (x, y) and places its objects; returns its height.
    fn place_para(&mut self, pb: Pb, x: f32, y: f32, w: f32) -> f32 {
        let po = self.text.layout(&pb.para, w.max(1.0), x, y, Some(&mut self.out));
        if self.record && pb.visible {
            self.out.paras.push((pb.para.clone(), Rect::new(x, y, w.max(1.0), po.height)));
        }
        for (k, obj) in pb.objs.into_iter().enumerate() {
            let Some(&(ox, oy)) = po.objs.get(k) else { continue };
            let (ax, ay) = (x + ox, y + oy);
            match obj {
                ObjKind::Img(id, iw, ih) => {
                    self.out.items.push(Item::Image { r: Rect::new(ax, ay, iw, ih), img: id, alpha: 255, radius: 0.0, clip: 0 });
                }
                ObjKind::Boxed(dl) => self.merge(dl, ax, ay),
                ObjKind::Blank => {}
            }
        }
        po.height
    }

    fn queue_abs(&mut self, n: NodeId, x: f32, y: f32, w: f32) {
        let s = self.style(n);
        let q = if s.position == 3 { 0 } else { self.abs.len() - 1 };
        self.abs[q].push((n, x, y, w));
    }

    fn abspos(&mut self, n: NodeId, sx: f32, sy: f32, sw: f32, cb: Rect) -> BoxOut {
        let s = self.style(n);
        let cb = if s.position == 3 { Rect::new(0.0, 0.0, self.vw, self.vh) } else { cb };
        let ins = [s.inset[0].get(cb.h), s.inset[1].get(cb.w), s.inset[2].get(cb.h), s.inset[3].get(cb.w)];
        let (m, _) = self.margins(s, cb.w);
        let p = self.padding(s, cb.w);
        let pbh = p[1] + p[3] + s.bw[1] + s.bw[3];
        let w = match s.width.get(cb.w) {
            Some(v) => (if s.border_box { v } else { v + pbh }).max(0.0),
            None => match (ins[3], ins[1]) {
                (Some(l), Some(r)) => (cb.w - l - r - m[1] - m[3]).max(0.0),
                _ => {
                    let (mn, mx) = self.intrinsic(n);
                    let avail = (cb.w - ins[3].unwrap_or(0.0) - m[1] - m[3]).max(0.0);
                    mx.min(avail).max(mn)
                }
            },
        };
        let w = match s.max_w.get(cb.w) {
            Some(mx) => w.min(if s.border_box { mx } else { mx + pbh }),
            None => w,
        };
        let x = match (ins[3], ins[1]) {
            (Some(l), _) => cb.x + l,
            (None, Some(r)) => cb.x + cb.w - r - w - m[1] - m[3],
            _ => sx,
        };
        let top = ins[0].map(|t| cb.y + t);
        let y0 = top.unwrap_or(sy);
        let cbh = (cb.h > 0.0).then_some(cb.h);
        let height = match (ins[0], ins[2], s.height.is_auto()) {
            (Some(t), Some(b), true) if cbh.is_some() => Some((cb.h - t - b - m[0] - m[2]).max(0.0)),
            _ => None,
        };
        let _ = sw;
        let mut own = Floats::default();
        let mut bo = self.block(n, x, y0, cb.w, Opt { width: Some(w), height, cbh }, &mut own);
        if top.is_none()
            && let Some(b) = ins[2]
            && cb.h > 0.0
        {
            let want = cb.y + cb.h - b - bo.h - m[2];
            let dy = want - bo.y;
            self.translate(bo.start, 0.0, dy);
            bo.y += dy;
        }
        bo
    }

    fn float(&mut self, n: NodeId, x: f32, y: f32, w: f32, floats: &mut Floats) {
        let s = self.style(n);
        let (m, _) = self.margins(s, w);
        let p = self.padding(s, w);
        let pbh = p[1] + p[3] + s.bw[1] + s.bw[3];
        let bw = match s.width.get(w) {
            Some(v) => if s.border_box { v } else { v + pbh },
            None if is_replaced(self.dom.tag(n)) => self.replaced_size(n, s, w, pbh, p[0] + p[2] + s.bw[0] + s.bw[2]).0 + pbh,
            None => {
                let (mn, mx) = self.intrinsic(n);
                mx.min(w - m[1] - m[3]).max(mn)
            }
        };
        let bw = match s.max_w.get(w) {
            Some(mx) => bw.min(if s.border_box { mx } else { mx + pbh }),
            None => bw,
        };
        let outer = bw + m[1] + m[3];
        let mut yy = y.max(floats.clear(s.clear));
        let mut guard = 0;
        let (l, r) = loop {
            let (l, r) = floats.band(yy, x, x + w);
            if r - l >= outer - 0.5 || guard > 50 {
                break (l, r);
            }
            match floats.next_bottom(yy) {
                Some(b) => yy = b,
                None => break (l, r),
            }
            guard += 1;
        };
        let bx = if s.float == 1 { l } else { (r - outer).max(l) };
        let mut own = Floats::default();
        let bo = self.block(n, bx, yy, w, Opt { width: Some(bw), ..Default::default() }, &mut own);
        let bottom = bo.y + bo.h + bo.mb;
        if s.float == 1 {
            floats.l.push((bx + outer, bottom));
        } else {
            floats.r.push((bx, bottom));
        }
    }

    // ---- inline content ----

    fn build_para(&mut self, items: &[Child], ps: &Style, avail: Option<f32>) -> Pb {
        let mut pb = Pb {
            para: Para { align: ps.text_align, line_h: if ps.line_height == 0.0 { 0.0 } else { ps.line_px() }, nowrap: ps.white_space == 1 || ps.white_space == 2, ..Default::default() },
            objs: Vec::new(),
            last_space: true,
            abs: Vec::new(),
            visible: false,
        };
        for &c in items {
            self.walk_inline(c, ps, &mut pb, avail, u32::MAX);
        }
        // drop a trailing collapsible space
        if pb.para.text.last() == Some(&(b' ' as u16)) && ps.white_space != 2 && ps.white_space != 3 {
            pb.para.text.pop();
            if let Some(r) = pb.para.runs.last_mut()
                && r.start + r.len > pb.para.text.len() as u32
            {
                r.len -= 1;
            }
        }
        pb
    }

    fn walk_inline(&mut self, c: Child, ps: &Style, pb: &mut Pb, avail: Option<f32>, link: u32) {
        match c {
            Child::Text(n) => {
                let parent = self.dom.parent(n);
                let s = if self.st.get(parent).is_some() { self.style(parent) } else { ps };
                let txt = self.dom.text(n).unwrap_or("");
                self.push_text(pb, s, txt, link);
            }
            Child::Before(n) | Child::After(n) => {
                let s = if matches!(c, Child::Before(_)) { self.st.before.get(&n) } else { self.st.after.get(&n) };
                if let Some(s) = s
                    && let Some(txt) = &s.content
                {
                    let txt = txt.clone();
                    if txt.is_empty() {
                        // an empty pseudo-element with a size (an icon drawn by css) holds its space
                        if let Some(w) = s.width.get(0.0).filter(|w| *w > 0.0) {
                            pb.para.objs.push(Obj { pos: pb.para.text.len() as u32, w, h: s.height.get(0.0).unwrap_or(w), base: s.height.get(0.0).unwrap_or(w) });
                            pb.para.text.push(0xFFFC);
                            pb.objs.push(ObjKind::Blank);
                        }
                        return;
                    }
                    let s = s.clone();
                    self.push_text(pb, &s, &txt, link);
                }
            }
            Child::Elem(e) => {
                let s = self.style(e);
                if s.display == Display::None || s.gone {
                    return;
                }
                if matches!(s.position, 2 | 3) {
                    pb.abs.push(e);
                    return;
                }
                let tag = self.dom.tag(e);
                if tag == t::br {
                    pb.para.text.push(0x2028);
                    pb.last_space = true;
                    pb.visible = true;
                    return;
                }
                if tag == t::wbr {
                    pb.para.text.push(0x200B);
                    return;
                }
                let link = if let Some(u) = self.goes_to(e) {
                    self.out.hrefs.push(u);
                    (self.out.hrefs.len() - 1) as u32
                } else {
                    link
                };
                if is_replaced(tag) || s.display != Display::Inline || s.float != 0 {
                    self.inline_box(e, s, pb, avail, link);
                    return;
                }
                let start = pb.para.text.len() as u32;
                let mut kids = Vec::new();
                self.kids(e, &mut kids);
                // horizontal padding and borders of inline elements become a little space
                let lead = s.padding[3].or0(0.0) + s.bw[3] + s.margin[3].or0(0.0);
                if lead >= 3.0 && !kids.is_empty() {
                    self.push_gap(pb, s, lead, link);
                }
                for k in kids {
                    self.walk_inline(k, ps, pb, avail, link);
                }
                let trail = s.padding[1].or0(0.0) + s.bw[1] + s.margin[1].or0(0.0);
                if trail >= 3.0 {
                    self.push_gap(pb, s, trail, link);
                }
                let end = pb.para.text.len() as u32;
                if s.bg[3] > 0 && end > start {
                    pb.para.bgs.push((start, end - start, s.bg));
                }
            }
        }
    }

    /// Horizontal space inside a line, as an empty object.
    fn push_gap(&mut self, pb: &mut Pb, _s: &Style, w: f32, _link: u32) {
        pb.para.objs.push(Obj { pos: pb.para.text.len() as u32, w, h: 0.0, base: 0.0 });
        pb.para.text.push(0xFFFC);
        pb.objs.push(ObjKind::Blank);
    }

    fn push_text(&mut self, pb: &mut Pb, s: &Style, txt: &str, link: u32) {
        if txt.is_empty() {
            return;
        }
        let mut buf = String::with_capacity(txt.len());
        match s.white_space {
            0 | 1 => {
                for ch in txt.chars() {
                    if matches!(ch, ' ' | '\n' | '\t' | '\r' | '\x0c') {
                        if !pb.last_space {
                            buf.push(' ');
                            pb.last_space = true;
                        }
                    } else {
                        buf.push(ch);
                        pb.last_space = false;
                    }
                }
            }
            4 => {
                for ch in txt.chars() {
                    if ch == '\n' {
                        buf.push('\u{2028}');
                        pb.last_space = true;
                    } else if matches!(ch, ' ' | '\t' | '\r') {
                        if !pb.last_space {
                            buf.push(' ');
                            pb.last_space = true;
                        }
                    } else {
                        buf.push(ch);
                        pb.last_space = false;
                    }
                }
            }
            _ => {
                for ch in txt.chars() {
                    match ch {
                        '\r' => {}
                        '\n' => buf.push('\u{2028}'),
                        c => buf.push(c),
                    }
                }
                pb.last_space = false;
            }
        }
        if buf.is_empty() {
            return;
        }
        let buf = match s.text_transform {
            1 => buf.to_uppercase(),
            2 => buf.to_lowercase(),
            3 => {
                let mut out = String::with_capacity(buf.len());
                let mut prev_space = true;
                for ch in buf.chars() {
                    if prev_space && ch.is_alphabetic() {
                        out.extend(ch.to_uppercase());
                    } else {
                        out.push(ch);
                    }
                    prev_space = ch.is_whitespace();
                }
                out
            }
            _ => buf,
        };
        if buf.chars().any(|c| !c.is_whitespace()) {
            pb.visible = true;
        }
        let start = pb.para.text.len() as u32;
        pb.para.text.extend(buf.encode_utf16());
        let len = pb.para.text.len() as u32 - start;
        let font = font_of(s);
        let color = s.color;
        let deco = s.deco;
        if let Some(r) = pb.para.runs.last_mut()
            && r.start + r.len == start
            && r.font == font
            && r.color == color
            && r.deco == deco
            && r.link == link
        {
            r.len += len;
            return;
        }
        pb.para.runs.push(Run { start, len, font, color, deco, link });
    }

    /// An image, form control or inline-block sitting on a line.
    fn inline_box(&mut self, e: NodeId, s: &'a Style, pb: &mut Pb, avail: Option<f32>, link: u32) {
        let (m, _) = self.margins(s, avail.unwrap_or(0.0));
        let tag = self.dom.tag(e);
        let pos = pb.para.text.len() as u32;
        pb.visible = true;
        let Some(avail) = avail else {
            // measuring: only the width matters
            let (_, mx) = self.intrinsic(e);
            pb.para.objs.push(Obj { pos, w: mx + m[1] + m[3], h: 1.0, base: 1.0 });
            pb.para.text.push(0xFFFC);
            pb.objs.push(ObjKind::Blank);
            return;
        };
        if tag == t::img && s.bw.iter().all(|w| *w == 0.0) && s.padding.iter().all(|p| *p == L::ZERO) && s.bg[3] == 0 && s.radius == L::ZERO && s.opacity >= 1.0 {
            let (iw, ih) = self.replaced_size(e, s, avail, 0.0, 0.0);
            let id = self.img_id(e);
            if iw > 0.0 && ih > 0.0 {
                let h = ih + m[0] + m[2];
                pb.para.objs.push(Obj { pos, w: iw + m[1] + m[3], h, base: hang(s, h, h, pb) });
                pb.para.text.push(0xFFFC);
                pb.objs.push(match id {
                    Some(id) => ObjKind::Img(id, iw, ih),
                    None => ObjKind::Blank,
                });
                self.link_obj(pb, link);
            }
            return;
        }
        let saved = std::mem::replace(&mut self.out, DisplayList::new());
        self.abs.push(Vec::new());
        let mut own = Floats::default();
        let bo = self.block(e, 0.0, 0.0, avail, Opt::default(), &mut own);
        let q = self.abs.pop().unwrap_or_default();
        for (an, sx, sy, sw) in q {
            self.abspos(an, sx, sy, sw, Rect::new(bo.x, bo.y, bo.w, bo.h));
        }
        let mut frag = std::mem::replace(&mut self.out, saved);
        if link != u32::MAX && self.out.hrefs.len() > link as usize {
            // the whole box is part of the link
            let href = self.out.hrefs[link as usize].clone();
            frag.hrefs.push(href);
            frag.links.push(Link { r: Rect::new(bo.x, bo.y, bo.w, bo.h), href: (frag.hrefs.len() - 1) as u32 });
        }
        let h = bo.h + bo.mt + bo.mb;
        // a box with words in it lines its last line up with the words around it
        let words = frag.items.iter().rev().find_map(|it| match *it {
            Item::Text { start, len, .. } if len > 0 => Some(frag.glyphs[(start + len) as usize - 1].y - (bo.y - bo.mt)),
            _ => None,
        });
        let text_base = match tag {
            // an empty box to type in lines up as if something were in it
            t::input if words.is_none() && !self.ticked(e) => bo.mt + (bo.h + s.font_size * 0.7) / 2.0,
            t::input if self.ticked(e) => h - bo.mb,
            _ if s.overflow != 0 => h,
            _ => words.filter(|w| (0.0..=h).contains(w)).unwrap_or(h),
        };
        pb.para.objs.push(Obj { pos, w: bo.w + bo.ml + bo.mr, h, base: hang(s, h, text_base, pb) });
        pb.para.text.push(0xFFFC);
        pb.objs.push(ObjKind::Boxed(frag));
    }

    /// A checkbox or radio button, which has no text.
    fn ticked(&self, n: NodeId) -> bool {
        self.dom.attr(n, t::type_).is_some_and(|ty| ty.eq_ignore_ascii_case("checkbox") || ty.eq_ignore_ascii_case("radio"))
    }

    fn link_obj(&mut self, pb: &mut Pb, link: u32) {
        if link != u32::MAX {
            let pos = pb.para.text.len() as u32 - 1;
            let font = pb.para.runs.last().map(|r| r.font.clone()).unwrap_or(Font { family: "serif".into(), size: 16.0, weight: 400, italic: false });
            pb.para.runs.push(Run { start: pos, len: 1, font, color: [0; 4], deco: 0, link });
        }
    }

    fn img_id(&mut self, e: NodeId) -> Option<u32> {
        let src = self.img_src(e)?;
        let url = self.resolve(&src)?;
        Some(self.img.id(&url))
    }

    fn img_src(&self, e: NodeId) -> Option<String> {
        let src = self.dom.attr(e, t::src).unwrap_or("");
        let lazy = self.dom.attr(e, t::data_src);
        let srcset = self.dom.attr(e, t::srcset);
        if (src.is_empty() || src.starts_with("data:image/gif") || src.starts_with("data:image/svg")) && lazy.is_some() {
            return lazy.map(|s| s.to_string());
        }
        if src.is_empty() {
            // the first candidate of a srcset
            return srcset.and_then(|s| s.split(',').next()).and_then(|c| c.split_whitespace().next()).map(|s| s.to_string());
        }
        Some(src.to_string())
    }

    /// Content-box size of a replaced element.
    fn replaced_size(&mut self, n: NodeId, s: &Style, cw: f32, pbh: f32, pbv: f32) -> (f32, f32) {
        let tag = self.dom.tag(n);
        let natural = match tag {
            t::img => self.img_id(n).and_then(|id| self.img.size(id)),
            t::input | t::select | t::textarea => {
                let lh = self.control_line(s);
                let ty = self.dom.attr(n, t::type_).unwrap_or("text").to_ascii_lowercase();
                if matches!(ty.as_str(), "checkbox" | "radio") {
                    Some((13.0, 13.0))
                } else if matches!(ty.as_str(), "submit" | "button" | "reset") || tag == t::select {
                    let label = self.control_label(n);
                    let w = label.chars().count() as f32 * s.font_size * 0.55 + if tag == t::select { s.font_size * 1.5 } else { 0.0 };
                    Some((w, lh))
                } else if tag == t::textarea {
                    Some((300.0, lh * 2.0))
                } else {
                    // the size attribute counts letters, at about half an em each
                    let letters = self.dom.attr(n, t::size).and_then(|v| v.trim().parse::<f32>().ok()).filter(|v| *v >= 1.0);
                    Some((letters.map_or(150.0, |l| l * s.font_size * 0.52), lh))
                }
            }
            _ => None,
        };
        // a picture keeps its shape when one side changes; a control does not
        let shaped = !matches!(tag, t::input | t::select | t::textarea);
        let ratio = s.aspect.or_else(|| natural.filter(|(w, h)| shaped && *w > 0.0 && *h > 0.0).map(|(w, h)| w / h));
        let sw = s.width.get(cw).map(|v| if s.border_box { (v - pbh).max(0.0) } else { v });
        let sh = if s.height.pct() { None } else { s.height.get(0.0).map(|v| if s.border_box { (v - pbv).max(0.0) } else { v }) };
        let (mut w, mut h) = match (sw, sh) {
            (Some(w), Some(h)) => (w, h),
            (Some(w), None) => (w, ratio.map(|r| w / r).or(natural.map(|n| n.1)).unwrap_or(0.0)),
            (None, Some(h)) => (ratio.map(|r| h * r).or(natural.map(|n| n.0)).unwrap_or(0.0), h),
            (None, None) => natural.unwrap_or((0.0, 0.0)),
        };
        if let Some(mx) = s.max_w.get(cw) {
            let mx = if s.border_box { (mx - pbh).max(0.0) } else { mx };
            if w > mx {
                if let Some(r) = ratio
                    && sh.is_none()
                {
                    h = mx / r;
                }
                w = mx;
            }
        }
        if let Some(mx) = s.max_h.get(f32::NAN).filter(|v| v.is_finite())
            && h > mx
        {
            if let Some(r) = ratio
                && sw.is_none()
            {
                w = mx * r;
            }
            h = mx;
        }
        (w.max(0.0), h.max(0.0))
    }

    /// The height of a line in a form control. Without a line-height this is the font's
    /// own, or the bottoms of letters like g would be cut off.
    fn control_line(&mut self, s: &Style) -> f32 {
        if s.line_height != 0.0 {
            return s.line_px();
        }
        let p = Para { text: vec![b'x' as u16], runs: vec![Run { start: 0, len: 1, font: font_of(s), color: s.color, deco: 0, link: u32::MAX }], nowrap: true, ..Default::default() };
        self.text.layout(&p, 1.0e4, 0.0, 0.0, None).height.ceil()
    }

    fn control_label(&self, n: NodeId) -> String {
        let tag = self.dom.tag(n);
        if tag == t::select {
            let mut first = None;
            for c in self.dom.children(n).collect::<Vec<_>>() {
                let opts: Vec<NodeId> = if self.dom.tag(c) == t::optgroup { self.dom.children(c).collect() } else { vec![c] };
                for o in opts {
                    if self.dom.tag(o) == t::option {
                        let mut s = String::new();
                        self.dom.text_content(o, &mut s);
                        let s = s.split_whitespace().collect::<Vec<_>>().join(" ");
                        if self.dom.attr(o, t::selected).is_some() {
                            return s;
                        }
                        first.get_or_insert(s);
                    }
                }
            }
            return first.unwrap_or_default();
        }
        if tag == t::textarea {
            let mut s = String::new();
            self.dom.text_content(n, &mut s);
            return s;
        }
        let ty = self.dom.attr(n, t::type_).unwrap_or("text").to_ascii_lowercase();
        match ty.as_str() {
            "submit" => self.dom.attr(n, t::value).unwrap_or("Submit").to_string(),
            "reset" => self.dom.attr(n, t::value).unwrap_or("Reset").to_string(),
            _ => self.dom.attr(n, t::value).unwrap_or("").to_string(),
        }
    }

    /// Draws what a replaced element shows inside its content box.
    fn replaced_content(&mut self, n: NodeId, s: &Style, r: Rect) {
        let tag = self.dom.tag(n);
        match tag {
            t::img => {
                if let Some(id) = self.img_id(n)
                    && r.w > 0.0
                    && r.h > 0.0
                {
                    let radius = s.radius.or0(r.w).min(r.w / 2.0);
                    self.out.items.push(Item::Image { r, img: id, alpha: 255, radius, clip: 0 });
                }
            }
            t::input | t::select | t::textarea => {
                let ty = self.dom.attr(n, t::type_).unwrap_or("text").to_ascii_lowercase();
                if matches!(ty.as_str(), "checkbox" | "radio") {
                    self.out.items.push(Item::Field { r, field: self.out.fields.len() as u32, clip: 0 });
                    self.out.items.push(Item::Text { start: 0, len: 0, color: s.color, clip: 0 });
                    self.out.fields.push(Field { node: n, font: font_of(s), color: s.color, align: 1, line_h: 0.0, multiline: false, password: false });
                    if self.dom.attr(n, t::checked).is_some() {
                        let k = r.w * 0.25;
                        self.out.items.push(Item::Rect { r: Rect::new(r.x + k, r.y + k, r.w - 2.0 * k, r.h - 2.0 * k), color: [0x1a, 0x73, 0xe8, 255], radius: if ty == "radio" { r.w } else { 1.0 }, clip: 0 });
                    }
                    return;
                }
                let mut label = self.control_label(n);
                let mut color = s.color;
                if label.is_empty()
                    && let Some(ph) = self.dom.attr(n, t::placeholder)
                {
                    label = ph.to_string();
                    color = PLACEHOLDER;
                }
                if tag == t::select {
                    label.push_str("  \u{25be}");
                }
                let pressed = matches!(ty.as_str(), "submit" | "button" | "reset") && tag == t::input;
                let field = Field {
                    node: n,
                    font: font_of(s),
                    color: s.color,
                    align: match s.text_align {
                        _ if pressed => 3,
                        2 | 3 | 5 => s.text_align,
                        _ => 1,
                    },
                    line_h: if tag == t::textarea { s.line_px() } else { 0.0 },
                    multiline: tag == t::textarea,
                    password: ty == "password" && color != PLACEHOLDER,
                };
                let para = field.para(&field.shown(&label), color);
                let m = self.mark();
                let h = self.text.layout(&para, r.w.max(1.0), r.x, r.y, None).height;
                let mut own = DisplayList::new();
                self.text.layout(&para, r.w.max(1.0), r.x, field.top(r, h), Some(&mut own));
                self.out.items.push(Item::Field { r, field: self.out.fields.len() as u32, clip: 0 });
                self.out.items.push(Item::Text { start: self.out.glyphs.len() as u32, len: own.glyphs.len() as u32, color, clip: 0 });
                self.out.glyphs.extend(own.glyphs);
                self.out.fields.push(field);
                self.clip_range(m, r);
            }
            _ => {}
        }
    }

    fn marker(&mut self, n: NodeId, s: &Style, cx: f32, cy: f32) {
        let idx = {
            let mut k = 1i64;
            let parent = self.dom.parent(n);
            if parent != NONE
                && let Some(st) = self.dom.attr(parent, t::start)
            {
                k = st.trim().parse().unwrap_or(1);
            }
            let mut c = self.dom.prev_elem(n);
            while let Some(p) = c {
                if self.dom.tag(p) == t::li {
                    k += 1;
                }
                c = self.dom.prev_elem(p);
            }
            k
        };
        let text = match s.list_style {
            1 => "•".to_string(),
            2 => "◦".to_string(),
            3 => "▪".to_string(),
            4 => format!("{idx}."),
            5 => format!("{}.", alpha(idx, b'a')),
            6 => format!("{}.", alpha(idx, b'A')),
            7 => format!("{}.", roman(idx).to_lowercase()),
            8 => format!("{}.", roman(idx)),
            _ => return,
        };
        let txt: Vec<u16> = text.encode_utf16().collect();
        let len = txt.len() as u32;
        let para = Para {
            text: txt,
            runs: vec![Run { start: 0, len, font: font_of(s), color: s.color, deco: 0, link: u32::MAX }],
            align: 1,
            line_h: if s.line_height == 0.0 { 0.0 } else { s.line_px() },
            nowrap: true,
            ..Default::default()
        };
        let (_, w) = self.text.widths(&para);
        let gap = s.font_size * 0.45;
        self.text.layout(&para, w + 1.0, cx - w - gap, cy, Some(&mut self.out));
    }

    // ---- flex and grid ----

    /// Children that become flex or grid items; a run of text becomes one anonymous item.
    fn items(&mut self, n: NodeId, cx: f32, cy: f32, cw: f32) -> Vec<Vec<Child>> {
        let mut kids = Vec::new();
        self.kids(n, &mut kids);
        let mut out: Vec<Vec<Child>> = Vec::new();
        let mut text: Vec<Child> = Vec::new();
        for c in kids {
            match c {
                Child::Elem(e) => {
                    let s = self.style(e);
                    if s.display == Display::None || s.gone {
                        continue;
                    }
                    if matches!(s.position, 2 | 3) {
                        self.queue_abs(e, cx, cy, cw);
                        continue;
                    }
                    if !text.is_empty() {
                        out.push(std::mem::take(&mut text));
                    }
                    out.push(vec![c]);
                }
                Child::Text(tn) => {
                    if self.dom.text(tn).is_some_and(|s| s.chars().any(|c| !c.is_whitespace())) {
                        text.push(c);
                    }
                }
                _ => text.push(c),
            }
        }
        if !text.is_empty() {
            out.push(text);
        }
        // order
        out.sort_by_key(|it| match it.as_slice() {
            [Child::Elem(e)] => self.style(*e).order,
            _ => 0,
        });
        out
    }

    /// Lays out one item at (x, y) with a border-box width; returns the box.
    fn item(&mut self, it: &[Child], ps: &Style, x: f32, y: f32, w: f32, cw: f32, cbh: Option<f32>) -> (BoxOut, Option<NodeId>) {
        if let [Child::Elem(e)] = it {
            let mut own = Floats::default();
            let s = self.style(*e);
            let (m, _) = self.margins(s, cw);
            let bw = (w - m[1] - m[3]).max(0.0);
            (self.block(*e, x, y, cw, Opt { width: Some(bw), cbh, ..Default::default() }, &mut own), Some(*e))
        } else {
            let start = self.mark();
            let pb = self.build_para(it, ps, Some(w));
            let h = if pb.visible { self.place_para(pb, x, y, w) } else { 0.0 };
            let end = self.mark();
            (BoxOut { x, y, w, h, start, content: start, end, ..Default::default() }, None)
        }
    }

    fn item_widths(&mut self, it: &[Child], ps: &Style) -> (f32, f32) {
        if let [Child::Elem(e)] = it {
            let s = self.style(*e);
            let (m, _) = self.margins(s, 0.0);
            let (mn, mx) = self.intrinsic(*e);
            (mn + m[1] + m[3], mx + m[1] + m[3])
        } else {
            let pb = self.build_para(it, ps, None);
            self.text.widths(&pb.para)
        }
    }

    fn flex(&mut self, n: NodeId, s: &Style, cx: f32, cy: f32, cw: f32, ch: Option<f32>) -> f32 {
        let items = self.items(n, cx, cy, cw);
        if items.is_empty() {
            return 0.0;
        }
        let row = s.flex_dir < 2;
        let gap_main = if row { s.col_gap.or0(cw) } else { s.row_gap.or0(cw) };
        let gap_cross = if row { s.row_gap.or0(cw) } else { s.col_gap.or0(cw) };
        if !row {
            return self.flex_col(n, s, items, cx, cy, cw, ch, gap_main);
        }
        struct Fi {
            base: f32,
            min: f32,
            max: f32,
            grow: f32,
            shrink: f32,
            ml: f32,
            mr: f32,
            auto_l: bool,
            auto_r: bool,
            main: f32,
        }
        let mut fis: Vec<Fi> = Vec::with_capacity(items.len());
        for it in &items {
            if let [Child::Elem(e)] = it.as_slice() {
                let es = self.style(*e);
                let (m, auto) = self.margins(es, cw);
                let p = self.padding(es, cw);
                let pbh = p[1] + p[3] + es.bw[1] + es.bw[3];
                let to_bb = |v: f32| if es.border_box { v } else { v + pbh };
                let (imin, imax) = self.intrinsic(*e);
                let base = if !es.basis.is_auto() && es.basis.get(cw).is_some() {
                    to_bb(es.basis.or0(cw)).max(pbh)
                } else if let Some(wv) = es.width.get(cw) {
                    to_bb(wv)
                } else {
                    imax
                };
                let min = match es.min_w.get(cw) {
                    Some(v) if v > 0.0 => to_bb(v),
                    _ if es.overflow == 0 && es.width.is_auto() => imin.min(base),
                    _ => pbh,
                };
                let max = es.max_w.get(cw).map(to_bb).unwrap_or(f32::INFINITY);
                fis.push(Fi { base, min, max, grow: es.grow, shrink: es.shrink, ml: m[3], mr: m[1], auto_l: auto[3], auto_r: auto[1], main: base.clamp(min, max.max(min)) });
            } else {
                let (mn, mx) = self.item_widths(it, s);
                fis.push(Fi { base: mx, min: mn, max: f32::INFINITY, grow: 0.0, shrink: 1.0, ml: 0.0, mr: 0.0, auto_l: false, auto_r: false, main: mx });
            }
        }
        // break into lines
        let mut lines: Vec<std::ops::Range<usize>> = Vec::new();
        let mut st = 0;
        let mut used = 0.0;
        for (i, f) in fis.iter().enumerate() {
            let outer = f.main + f.ml + f.mr;
            if s.flex_wrap && i > st && used + gap_main + outer > cw + 0.5 {
                lines.push(st..i);
                st = i;
                used = outer;
            } else {
                used += if i > st { gap_main } else { 0.0 } + outer;
            }
        }
        lines.push(st..fis.len());
        let mut y = cy;
        let single = lines.len() == 1;
        for line in lines {
            let k = line.len();
            let sum: f32 = fis[line.clone()].iter().map(|f| f.main + f.ml + f.mr).sum::<f32>() + gap_main * (k.saturating_sub(1)) as f32;
            let free = cw - sum;
            if free > 0.0 {
                let g: f32 = fis[line.clone()].iter().map(|f| f.grow).sum();
                if g > 0.0 {
                    let share = free * if g < 1.0 { g } else { 1.0 };
                    for f in &mut fis[line.clone()] {
                        f.main = (f.main + share * f.grow / g).min(f.max.max(f.min));
                    }
                }
            } else if free < 0.0 {
                let ws: f32 = fis[line.clone()].iter().map(|f| f.shrink * f.base).sum();
                if ws > 0.0 {
                    for f in &mut fis[line.clone()] {
                        f.main = (f.main + free * f.shrink * f.base / ws).max(f.min);
                    }
                }
            }
            let sum: f32 = fis[line.clone()].iter().map(|f| f.main + f.ml + f.mr).sum::<f32>() + gap_main * (k.saturating_sub(1)) as f32;
            let mut free = (cw - sum).max(0.0);
            let autos = fis[line.clone()].iter().map(|f| f.auto_l as u32 + f.auto_r as u32).sum::<u32>();
            if autos > 0 && free > 0.0 {
                let each = free / autos as f32;
                for f in &mut fis[line.clone()] {
                    if f.auto_l {
                        f.ml += each;
                    }
                    if f.auto_r {
                        f.mr += each;
                    }
                }
                free = 0.0;
            }
            let (mut x, between) = match s.justify {
                1 => (cx + free, 0.0),
                2 => (cx + free / 2.0, 0.0),
                3 if k > 1 => (cx, free / (k - 1) as f32),
                4 => (cx + free / (2 * k) as f32, free / k as f32),
                5 => (cx + free / (k + 1) as f32, free / (k + 1) as f32),
                _ => (cx, 0.0),
            };
            let reverse = s.flex_dir == 1;
            let order: Vec<usize> = if reverse { line.clone().rev().collect() } else { line.clone().collect() };
            if reverse {
                x = cx + cw - (x - cx);
            }
            let mut placed: Vec<(BoxOut, Option<NodeId>)> = Vec::with_capacity(k);
            for i in order {
                let f = &fis[i];
                let outer = f.main + f.ml + f.mr;
                let ix = if reverse { x - outer } else { x };
                // item() wants the margin-box left and the width with the item's own margins
                let own_l = self.style_margin_l(&items[i], cw);
                let own_lr = self.style_margin_lr(&items[i], cw);
                let (mut bo, e) = self.item(&items[i], s, ix + f.ml - own_l, y, f.main + own_lr, cw, if single { ch } else { None });
                if e.is_none() {
                    bo.w = f.main;
                }
                placed.push((bo, e));
                if reverse {
                    x -= outer + gap_main + between;
                } else {
                    x += outer + gap_main + between;
                }
            }
            let mut line_h = placed.iter().map(|(b, _)| b.h + b.mt + b.mb).fold(0.0, f32::max);
            if single && let Some(h) = ch {
                line_h = line_h.max(h);
            }
            for (mut bo, e) in placed {
                let outer = bo.h + bo.mt + bo.mb;
                let Some(e) = e else { continue };
                let es = self.style(e);
                let a = if es.align_self == 255 { s.align_items } else { es.align_self };
                match a {
                    0 if es.height.is_auto() && outer < line_h => {
                        let h = line_h - bo.mt - bo.mb;
                        self.resize(e, &mut bo, h);
                    }
                    2 => self.shift(bo.start, bo.end, 0.0, line_h - outer),
                    3 => self.shift(bo.start, bo.end, 0.0, (line_h - outer) / 2.0),
                    _ => {}
                }
            }
            y += line_h + gap_cross;
        }
        (y - gap_cross - cy).max(0.0)
    }

    fn style_margin_l(&self, it: &[Child], cw: f32) -> f32 {
        if let [Child::Elem(e)] = it { self.margins(self.style(*e), cw).0[3] } else { 0.0 }
    }

    fn style_margin_lr(&self, it: &[Child], cw: f32) -> f32 {
        if let [Child::Elem(e)] = it {
            let m = self.margins(self.style(*e), cw).0;
            m[1] + m[3]
        } else {
            0.0
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn flex_col(&mut self, _n: NodeId, s: &Style, items: Vec<Vec<Child>>, cx: f32, cy: f32, cw: f32, ch: Option<f32>, gap: f32) -> f32 {
        let mut y = cy;
        let mut placed: Vec<(BoxOut, Option<NodeId>, f32)> = Vec::new();
        for it in &items {
            if let [Child::Elem(e)] = it.as_slice() {
                let es = self.style(*e);
                let (m, auto) = self.margins(es, cw);
                let a = if es.align_self == 255 { s.align_items } else { es.align_self };
                let stretch = a == 0 && es.width.is_auto() && !(auto[1] || auto[3]);
                let mut own = Floats::default();
                let w = if stretch {
                    None
                } else {
                    let p = self.padding(es, cw);
                    let pbh = p[1] + p[3] + es.bw[1] + es.bw[3];
                    Some(match es.width.get(cw) {
                        Some(v) => if es.border_box { v } else { v + pbh },
                        None => {
                            let (mn, mx) = self.intrinsic(*e);
                            mx.min(cw - m[1] - m[3]).max(mn)
                        }
                    })
                };
                let w = w.map(|w| match es.max_w.get(cw) {
                    Some(mx) => w.min(mx),
                    None => w,
                });
                let bo = self.block(*e, cx, y, cw, Opt { width: w, ..Default::default() }, &mut own);
                if let Some(w) = w {
                    let free = cw - w - m[1] - m[3];
                    let dx = if auto[1] && auto[3] {
                        free / 2.0
                    } else if auto[3] {
                        free
                    } else {
                        match a {
                            2 => free,
                            3 => free / 2.0,
                            _ => 0.0,
                        }
                    };
                    if dx > 0.0 {
                        self.shift(bo.start, bo.end, dx, 0.0);
                    }
                }
                y = bo.y + bo.h + bo.mb + gap;
                placed.push((bo, Some(*e), es.grow));
            } else {
                let (bo, _) = self.item(it, s, cx, y, cw, cw, None);
                y = bo.y + bo.h + gap;
                placed.push((bo, None, 0.0));
            }
        }
        let mut total = (y - gap - cy).max(0.0);
        if let Some(h) = ch
            && h > total
        {
            let free = h - total;
            let g: f32 = placed.iter().map(|p| p.2).sum();
            if g > 0.0 {
                let mut shift = 0.0;
                for (bo, e, grow) in placed.iter_mut() {
                    if shift > 0.0 {
                        self.shift(bo.start, bo.end, 0.0, shift);
                        bo.y += shift;
                    }
                    if *grow > 0.0
                        && let Some(e) = e
                    {
                        let add = free * *grow / g;
                        let nh = bo.h + add;
                        self.resize(*e, bo, nh);
                        shift += add;
                    }
                }
            } else {
                let off = match s.justify {
                    1 => free,
                    2 => free / 2.0,
                    _ => 0.0,
                };
                if off > 0.0
                    && let Some((first, _, _)) = placed.first()
                {
                    self.translate(first.start, 0.0, off);
                }
            }
            total = h;
        }
        total
    }

    fn track_px(&self, t: &Ts, cw: f32) -> Option<f32> {
        match t {
            Ts::Len(l) => l.get(cw),
            _ => None,
        }
    }

    /// The columns a grid has at this width (auto-fill repeated to fit).
    fn grid_cols(&self, def: &GridDef, cw: f32, gap: f32) -> Vec<Track> {
        let mut cols = def.cols.clone();
        if let Some(t) = &def.fill {
            let min = self.track_px(&t.min, cw).or_else(|| self.track_px(&t.max, cw)).unwrap_or(200.0).max(1.0);
            let fixed: f32 = cols.iter().filter_map(|c| self.track_px(&c.max, cw)).sum();
            let n = (((cw - fixed + gap) / (min + gap)).floor() as usize).max(1);
            cols.extend(std::iter::repeat_n(t.clone(), n));
        }
        cols
    }

    /// CSS grid: placement by area name, line numbers or auto-flow; columns sized from
    /// their tracks (lengths, fr, auto, min/max-content); rows from their content.
    fn grid(&mut self, n: NodeId, s: &Style, cx: f32, cy: f32, cw: f32) -> f32 {
        let items = self.items(n, cx, cy, cw);
        if items.is_empty() {
            return 0.0;
        }
        let gap_c = s.col_gap.or0(cw);
        let gap_r = s.row_gap.or0(cw);
        let def: GridDef = s.grid.as_deref().cloned().unwrap_or_default();
        let cols = self.grid_cols(&def, cw, gap_c);
        let area_w = def.areas.iter().map(|r| r.len()).max().unwrap_or(0);
        let mut ncols = cols.len().max(area_w).max(1);
        // named areas: their bounding cells
        let mut named: HashMap<u32, (usize, usize, usize, usize)> = HashMap::new();
        for (r, row) in def.areas.iter().enumerate() {
            for (c, &a) in row.iter().enumerate() {
                if a == NONE {
                    continue;
                }
                let e = named.entry(a).or_insert((c, c + 1, r, r + 1));
                e.0 = e.0.min(c);
                e.1 = e.1.max(c + 1);
                e.2 = e.2.min(r);
                e.3 = e.3.max(r + 1);
            }
        }
        let styles: Vec<Option<&Style>> = items.iter().map(|it| if let [Child::Elem(e)] = it.as_slice() { Some(self.style(*e)) } else { None }).collect();
        let resolve = |g: GLine, n: usize| -> (Option<usize>, usize) {
            let line = |v: i16| -> Option<usize> {
                if v > 0 {
                    Some(v as usize - 1)
                } else if v < 0 {
                    Some((n as i32 + 1 + v as i32).max(0) as usize)
                } else {
                    None
                }
            };
            let span = g.span.max(1) as usize;
            match (line(g.start), line(g.end)) {
                (Some(a), Some(b)) if b > a => (Some(a), b - a),
                (Some(a), Some(b)) if b < a => (Some(b), a - b),
                (Some(a), _) => (Some(a), span),
                (None, Some(b)) => (Some(b.saturating_sub(span)), span),
                _ => (None, span),
            }
        };
        // explicit columns can widen the grid
        for st in styles.iter().flatten() {
            if let (Some(c), sp) = resolve(st.g_col, ncols) {
                ncols = ncols.max(c + sp);
            }
            if st.g_area != NONE
                && let Some(a) = named.get(&st.g_area)
            {
                ncols = ncols.max(a.1);
            }
        }
        let mut taken: Vec<Vec<bool>> = Vec::new();
        let free = |taken: &mut Vec<Vec<bool>>, r: usize, c0: usize, c1: usize, r1: usize| -> bool {
            while taken.len() < r1 {
                taken.push(vec![false; ncols]);
            }
            (r..r1).all(|rr| (c0..c1).all(|cc| !taken[rr][cc]))
        };
        let mark = |taken: &mut Vec<Vec<bool>>, c0: usize, c1: usize, r0: usize, r1: usize| {
            while taken.len() < r1 {
                taken.push(vec![false; ncols]);
            }
            for row in taken.iter_mut().take(r1).skip(r0) {
                for cell in row.iter_mut().take(c1).skip(c0) {
                    *cell = true;
                }
            }
        };
        let mut place: Vec<Option<(usize, usize, usize, usize)>> = vec![None; items.len()];
        // definite placements first
        for (i, st) in styles.iter().enumerate() {
            let Some(st) = st else { continue };
            if st.g_area != NONE {
                if let Some(&a) = named.get(&st.g_area) {
                    mark(&mut taken, a.0, a.1, a.2, a.3);
                    place[i] = Some(a);
                }
                continue;
            }
            let (c, cs) = resolve(st.g_col, ncols);
            let (r, rs) = resolve(st.g_row, def.rows.len().max(def.areas.len()));
            if let (Some(c), Some(r)) = (c, r) {
                let c1 = (c + cs).min(ncols);
                mark(&mut taken, c, c1, r, r + rs);
                place[i] = Some((c, c1, r, r + rs));
            }
        }
        // the rest flow into free cells, row by row
        let (mut cr, mut cc) = (0usize, 0usize);
        for i in 0..items.len() {
            if place[i].is_some() {
                continue;
            }
            let (fixed_c, cs, rs) = match styles[i] {
                Some(st) => {
                    let (c, cs) = resolve(st.g_col, ncols);
                    let (_, rs) = resolve(st.g_row, ncols);
                    (c, cs.min(ncols), rs)
                }
                None => (None, 1, 1),
            };
            if let Some(c) = fixed_c {
                let c1 = (c + cs).min(ncols);
                let mut r = 0;
                while !free(&mut taken, r, c, c1, r + rs) {
                    r += 1;
                }
                mark(&mut taken, c, c1, r, r + rs);
                place[i] = Some((c, c1, r, r + rs));
                continue;
            }
            loop {
                if cc + cs > ncols {
                    cc = 0;
                    cr += 1;
                }
                if free(&mut taken, cr, cc, cc + cs, cr + rs) {
                    break;
                }
                cc += 1;
            }
            mark(&mut taken, cc, cc + cs, cr, cr + rs);
            place[i] = Some((cc, cc + cs, cr, cr + rs));
            cc += cs;
        }
        // column widths
        let mut cmin = vec![0.0f32; ncols];
        let mut cmax = vec![0.0f32; ncols];
        for (i, it) in items.iter().enumerate() {
            let Some((c0, c1, _, _)) = place[i] else { continue };
            if c1 - c0 != 1 {
                continue;
            }
            let t = cols.get(c0);
            let content_sized = t.is_none_or(|t| !matches!(t.max, Ts::Len(_)) || !matches!(t.min, Ts::Len(_)));
            if content_sized {
                let (a, b) = self.item_widths(it, s);
                cmin[c0] = cmin[c0].max(a);
                cmax[c0] = cmax[c0].max(b);
            }
        }
        let auto_t = Track { min: Ts::Auto, max: Ts::Auto };
        let mut w = vec![0.0f32; ncols];
        let mut fr = vec![0.0f32; ncols];
        let mut floor = vec![0.0f32; ncols];
        let mut auto_cols = Vec::new();
        for k in 0..ncols {
            let t = cols.get(k).unwrap_or(&auto_t);
            floor[k] = match &t.min {
                Ts::Len(l) => l.get(cw).unwrap_or(0.0),
                Ts::MinContent | Ts::Auto => cmin[k],
                Ts::MaxContent => cmax[k],
                Ts::Fr(_) => 0.0,
            };
            match &t.max {
                Ts::Len(l) => w[k] = l.get(cw).unwrap_or(0.0).max(floor[k]),
                Ts::Fr(f) => {
                    fr[k] = *f;
                    w[k] = floor[k];
                }
                Ts::MinContent => w[k] = cmin[k],
                Ts::MaxContent | Ts::Auto => {
                    w[k] = cmax[k].max(floor[k]);
                    if matches!(t.max, Ts::Auto) {
                        auto_cols.push(k);
                    }
                }
            }
        }
        let gaps = gap_c * (ncols - 1) as f32;
        let total_fr: f32 = fr.iter().sum();
        if total_fr > 0.0 {
            // fr tracks share what the others leave; a track below its floor keeps the floor
            let mut frozen = vec![false; ncols];
            for _ in 0..4 {
                let used: f32 = (0..ncols).filter(|&k| fr[k] == 0.0 || frozen[k]).map(|k| w[k]).sum();
                let flex: f32 = (0..ncols).filter(|&k| fr[k] > 0.0 && !frozen[k]).map(|k| fr[k]).sum();
                if flex <= 0.0 {
                    break;
                }
                let unit = ((cw - gaps - used) / flex.max(1.0)).max(0.0);
                let mut again = false;
                for k in 0..ncols {
                    if fr[k] > 0.0 && !frozen[k] {
                        let v = unit * fr[k];
                        if v < floor[k] {
                            w[k] = floor[k];
                            frozen[k] = true;
                            again = true;
                        } else {
                            w[k] = v;
                        }
                    }
                }
                if !again {
                    break;
                }
            }
        } else {
            let sum: f32 = w.iter().sum();
            let free = cw - gaps - sum;
            if free > 0.0 && !auto_cols.is_empty() {
                let each = free / auto_cols.len() as f32;
                for &k in &auto_cols {
                    w[k] += each;
                }
            } else if free < 0.0 {
                let room: f32 = auto_cols.iter().map(|&k| (w[k] - cmin[k].min(w[k])).max(0.0)).sum();
                if room > 0.0 {
                    let cut = (-free).min(room);
                    for &k in &auto_cols {
                        let r = (w[k] - cmin[k].min(w[k])).max(0.0);
                        w[k] -= cut * r / room;
                    }
                }
            }
        }
        let mut col_x = vec![cx; ncols + 1];
        for k in 0..ncols {
            col_x[k + 1] = col_x[k] + w[k] + gap_c;
        }
        // lay every item out at the top, then move it to its row
        let mut placed: Vec<(BoxOut, Option<NodeId>, (usize, usize, usize, usize))> = Vec::with_capacity(items.len());
        for (i, it) in items.iter().enumerate() {
            let Some(pl) = place[i] else { continue };
            let (c0, c1, _, _) = pl;
            let area_w = (col_x[c1] - col_x[c0] - gap_c).max(0.0);
            let outer = match styles[i] {
                Some(st) if !st.width.is_auto() => {
                    let p = self.padding(st, area_w);
                    let pbh = p[1] + p[3] + st.bw[1] + st.bw[3];
                    let (m, _) = self.margins(st, area_w);
                    st.width.get(area_w).map_or(area_w, |v| (if st.border_box { v } else { v + pbh }) + m[1] + m[3]).min(area_w.max(0.0))
                }
                _ => area_w,
            };
            let (bo, e) = self.item(it, s, col_x[c0], cy, outer, area_w, None);
            placed.push((bo, e, pl));
        }
        let nrows = placed.iter().map(|p| p.2 .3).max().unwrap_or(0);
        let mut rows = vec![0.0f32; nrows];
        let mut fixed_row = vec![false; nrows];
        for (k, r) in rows.iter_mut().enumerate() {
            if let Some(Ts::Len(l)) = def.rows.get(k).map(|t| &t.max)
                && let Some(v) = l.get(f32::NAN).filter(|v| v.is_finite())
            {
                *r = v;
                fixed_row[k] = true;
            }
        }
        for (bo, _, (_, _, r0, r1)) in &placed {
            if r1 - r0 == 1 && !fixed_row[*r0] {
                rows[*r0] = rows[*r0].max(bo.h + bo.mt + bo.mb);
            }
        }
        for (bo, _, (_, _, r0, r1)) in &placed {
            if r1 - r0 > 1 {
                let have: f32 = rows[*r0..*r1].iter().sum::<f32>() + gap_r * (r1 - r0 - 1) as f32;
                let need = bo.h + bo.mt + bo.mb - have;
                if need > 0.0 {
                    rows[r1 - 1] += need;
                }
            }
        }
        let mut row_y = vec![cy; nrows + 1];
        for k in 0..nrows {
            row_y[k + 1] = row_y[k] + rows[k] + gap_r;
        }
        for (mut bo, e, (_, _, r0, r1)) in placed {
            let dy = row_y[r0] - cy;
            if dy != 0.0 {
                self.shift(bo.start, bo.end, 0.0, dy);
                bo.y += dy;
            }
            let Some(e) = e else { continue };
            let es = self.style(e);
            let area_h = row_y[r1] - row_y[r0] - gap_r;
            let outer = bo.h + bo.mt + bo.mb;
            let a = if es.align_self == 255 { s.align_items } else { es.align_self };
            match a {
                0 if es.height.is_auto() && outer < area_h => {
                    let nh = area_h - bo.mt - bo.mb;
                    self.resize(e, &mut bo, nh);
                }
                3 => self.shift(bo.start, bo.end, 0.0, (area_h - outer) / 2.0),
                2 => self.shift(bo.start, bo.end, 0.0, area_h - outer),
                _ => {}
            }
        }
        (row_y[nrows] - gap_r - cy).max(0.0)
    }

    // ---- tables ----

    /// Rows of (cell, colspan), in order, plus the caption.
    fn table_rows(&mut self, n: NodeId) -> (Vec<(NodeId, Vec<(NodeId, usize)>)>, Option<NodeId>) {
        let mut rows = Vec::new();
        let mut caption = None;
        let mut kids = Vec::new();
        self.kids(n, &mut kids);
        let push_row = |me: &mut Self, r: NodeId, rows: &mut Vec<(NodeId, Vec<(NodeId, usize)>)>| {
            let mut cells = Vec::new();
            let mut ck = Vec::new();
            me.kids(r, &mut ck);
            for c in ck {
                if let Child::Elem(e) = c {
                    let s = me.style(e);
                    if s.display == Display::None {
                        continue;
                    }
                    let span = me.dom.attr(e, t::colspan).and_then(|v| v.trim().parse::<usize>().ok()).unwrap_or(1).clamp(1, 64);
                    cells.push((e, span));
                }
            }
            rows.push((r, cells));
        };
        for c in kids {
            let Child::Elem(e) = c else { continue };
            let s = self.style(e);
            match s.display {
                Display::None | Display::TableColumn => {}
                Display::TableCaption => caption = Some(e),
                Display::TableRowGroup => {
                    let mut rk = Vec::new();
                    self.kids(e, &mut rk);
                    for r in rk {
                        if let Child::Elem(re) = r
                            && self.style(re).display != Display::None
                        {
                            push_row(self, re, &mut rows);
                        }
                    }
                }
                Display::TableRow => push_row(self, e, &mut rows),
                _ => {
                    // a stray cell or block directly in the table: its own row
                    rows.push((e, vec![(e, 1)]));
                }
            }
        }
        (rows, caption)
    }

    fn table_cols(&mut self, rows: &[(NodeId, Vec<(NodeId, usize)>)]) -> (Vec<f32>, Vec<f32>) {
        let ncol = rows.iter().map(|(_, c)| c.iter().map(|x| x.1).sum::<usize>()).max().unwrap_or(0);
        let mut mins = vec![0.0f32; ncol];
        let mut maxs = vec![0.0f32; ncol];
        let mut spans = Vec::new();
        for (_, cells) in rows {
            let mut col = 0;
            for &(c, span) in cells {
                if col >= ncol {
                    break;
                }
                let (mut mn, mut mx) = self.intrinsic(c);
                let cs = self.style(c);
                if let Some(w) = cs.width.get(0.0).filter(|_| !cs.width.pct()) {
                    let p = self.padding(cs, 0.0);
                    let bb = if cs.border_box { w } else { w + p[1] + p[3] + cs.bw[1] + cs.bw[3] };
                    mx = mx.max(bb);
                    mn = mn.max(bb.min(mx));
                }
                if span == 1 {
                    mins[col] = mins[col].max(mn);
                    maxs[col] = maxs[col].max(mx);
                } else {
                    spans.push((col, span.min(ncol - col), mn, mx));
                }
                col += span;
            }
        }
        for (col, span, mn, mx) in spans {
            let have_min: f32 = mins[col..col + span].iter().sum();
            let have_max: f32 = maxs[col..col + span].iter().sum();
            if mn > have_min {
                let add = (mn - have_min) / span as f32;
                for v in &mut mins[col..col + span] {
                    *v += add;
                }
            }
            if mx > have_max {
                let add = (mx - have_max) / span as f32;
                for v in &mut maxs[col..col + span] {
                    *v += add;
                }
            }
        }
        for k in 0..ncol {
            maxs[k] = maxs[k].max(mins[k]);
        }
        (mins, maxs)
    }

    fn table(&mut self, n: NodeId, s: &Style, cx: f32, cy: f32, cw: f32) -> f32 {
        let (rows, caption) = self.table_rows(n);
        let mut y = cy;
        if let Some(c) = caption {
            let mut own = Floats::default();
            let bo = self.block(c, cx, y, cw, Opt::default(), &mut own);
            y = bo.y + bo.h + bo.mb;
        }
        if rows.is_empty() {
            return y - cy;
        }
        let (mins, maxs) = self.table_cols(&rows);
        let ncol = mins.len();
        if ncol == 0 {
            return y - cy;
        }
        let sp = if s.collapse { 0.0 } else { s.spacing };
        let avail = (cw - sp * (ncol + 1) as f32).max(0.0);
        let (smin, smax): (f32, f32) = (mins.iter().sum(), maxs.iter().sum());
        let explicit = !s.width.is_auto();
        let target = if explicit { avail } else { avail.min(smax).max(smin) };
        let widths: Vec<f32> = if target >= smax {
            if smax > 0.0 { maxs.iter().map(|m| m + (target - smax) * m / smax).collect() } else { vec![target / ncol as f32; ncol] }
        } else if target <= smin || smax <= smin {
            mins.clone()
        } else {
            let k = (target - smin) / (smax - smin);
            mins.iter().zip(&maxs).map(|(a, b)| a + (b - a) * k).collect()
        };
        y += sp;
        for (row, cells) in rows {
            let rs = self.style(row);
            let row_bg = if rs.bg[3] > 0 && row != cells.first().map_or(NONE, |c| c.0) {
                self.out.items.push(Item::Rect { r: Rect::default(), color: rs.bg, radius: 0.0, clip: 0 });
                Some(self.out.items.len() - 1)
            } else {
                None
            };
            let mut x = cx + sp;
            let mut col = 0;
            let mut placed: Vec<(BoxOut, NodeId)> = Vec::new();
            for (c, span) in cells {
                if col >= ncol {
                    break;
                }
                let span = span.min(ncol - col);
                let w = widths[col..col + span].iter().sum::<f32>() + sp * (span - 1) as f32;
                let mut own = Floats::default();
                let bo = self.block(c, x, y, w, Opt { width: Some(w), ..Default::default() }, &mut own);
                placed.push((bo, c));
                x += w + sp;
                col += span;
            }
            let mut h = placed.iter().map(|(b, _)| b.h).fold(0.0, f32::max);
            if let Some(rh) = rs.height.get(0.0).filter(|_| !rs.height.pct()) {
                h = h.max(rh);
            }
            for (mut bo, c) in placed {
                if bo.h < h {
                    let cs = self.style(c);
                    let dy = match cs.valign {
                        2 => (h - bo.h) / 2.0,
                        3 => h - bo.h,
                        _ => 0.0,
                    };
                    if dy > 0.0 {
                        self.shift(bo.content, bo.end, 0.0, dy);
                    }
                    self.resize(c, &mut bo, h);
                }
            }
            let row_r = Rect::new(cx + sp, y, x - cx - 2.0 * sp, h);
            if let Some(i) = row_bg {
                self.out.items[i] = Item::Rect { r: row_r, color: rs.bg, radius: 0.0, clip: 0 };
            }
            if self.record && self.dom.tag(row) == t::tr {
                self.out.boxes.push((row, row_r));
            }
            y += h + sp;
        }
        y - cy
    }

    // ---- intrinsic sizes ----

    /// (min-content, max-content) border-box widths.
    fn intrinsic(&mut self, n: NodeId) -> (f32, f32) {
        if let Some(&v) = self.intr.get(&n) {
            return v;
        }
        self.depth += 1;
        let v = if self.depth > 200 { (0.0, 0.0) } else { self.intrinsic_inner(n) };
        self.depth -= 1;
        self.intr.insert(n, v);
        v
    }

    fn intrinsic_inner(&mut self, n: NodeId) -> (f32, f32) {
        let s = self.style(n);
        if s.display == Display::None {
            return (0.0, 0.0);
        }
        let p = self.padding(s, 0.0);
        let pbh = p[1] + p[3] + s.bw[1] + s.bw[3];
        let clamp = |v: f32| -> f32 {
            let mut v = v;
            if let Some(mx) = s.max_w.get(0.0).filter(|_| !s.max_w.pct()) {
                v = v.min(if s.border_box { mx } else { mx + pbh });
            }
            if let Some(mn) = s.min_w.get(0.0).filter(|_| !s.min_w.pct()) {
                v = v.max(if s.border_box { mn } else { mn + pbh });
            }
            v
        };
        if let Some(w) = s.width.get(0.0).filter(|_| !s.width.pct()) {
            let v = clamp(if s.border_box { w } else { w + pbh });
            return (v, v);
        }
        let tag = self.dom.tag(n);
        if is_replaced(tag) {
            let pbv = p[0] + p[2] + s.bw[0] + s.bw[2];
            // a width in percent says nothing until the box around it has one, so that box is
            // measured as if there were none, and this one can shrink to nothing
            let w = if s.width.pct() {
                let s = Style { width: L::Auto, ..s.clone() };
                self.replaced_size(n, &s, self.vw, pbh, pbv).0
            } else {
                self.replaced_size(n, s, self.vw, pbh, pbv).0
            };
            let v = clamp(w + pbh);
            let mn = if s.max_w.pct() || s.width.pct() { 0.0 } else { v };
            return (mn, v);
        }
        let (mn, mx) = match s.display {
            Display::Table | Display::InlineTable => {
                let (rows, _) = self.table_rows(n);
                let (mins, maxs) = self.table_cols(&rows);
                let sp = if s.collapse { 0.0 } else { s.spacing * (mins.len() + 1) as f32 };
                (mins.iter().sum::<f32>() + sp, maxs.iter().sum::<f32>() + sp)
            }
            Display::Flex | Display::InlineFlex if s.flex_dir < 2 => {
                let items = self.items_measure(n);
                let gap = s.col_gap.or0(0.0) * items.len().saturating_sub(1) as f32;
                let mut mn: f32 = 0.0;
                let mut mx = 0.0;
                for it in &items {
                    let (a, b) = self.item_widths(it, s);
                    mn = if s.flex_wrap { mn.max(a) } else { mn + a };
                    mx += b;
                }
                (mn + if s.flex_wrap { 0.0 } else { gap }, mx + gap)
            }
            Display::Grid | Display::InlineGrid => {
                let items = self.items_measure(n);
                let def: GridDef = s.grid.as_deref().cloned().unwrap_or_default();
                let fixed: Option<f32> = if !def.cols.is_empty() && def.fill.is_none() {
                    def.cols.iter().map(|t| if let Ts::Len(l) = &t.max { l.get(0.0).filter(|_| !l.pct()) } else { None }).sum()
                } else {
                    None
                };
                if let Some(f) = fixed {
                    let g = s.col_gap.or0(0.0) * def.cols.len().saturating_sub(1) as f32;
                    return (clamp(f + g + pbh), clamp(f + g + pbh));
                }
                let cols = def.cols.len().max(def.areas.iter().map(|r| r.len()).max().unwrap_or(0)).max(1) as f32;
                let mut mn: f32 = 0.0;
                let mut mx: f32 = 0.0;
                for it in &items {
                    let (a, b) = self.item_widths(it, s);
                    mn = mn.max(a);
                    mx = mx.max(b);
                }
                (mn * cols, mx * cols + s.col_gap.or0(0.0) * (cols - 1.0))
            }
            _ => {
                let mut kids = Vec::new();
                self.kids(n, &mut kids);
                let mut mn: f32 = 0.0;
                let mut mx: f32 = 0.0;
                let mut run: Vec<Child> = Vec::new();
                let flush = |me: &mut Self, run: &mut Vec<Child>, mn: &mut f32, mx: &mut f32| {
                    if run.is_empty() {
                        return;
                    }
                    let pb = me.build_para(run, s, None);
                    run.clear();
                    if pb.visible {
                        let (a, b) = me.text.widths(&pb.para);
                        *mn = mn.max(a);
                        *mx = mx.max(b);
                    }
                };
                for c in kids {
                    match self.classify(c) {
                        Class::Skip | Class::Abs(_) => {}
                        Class::Inline => run.push(c),
                        Class::Block(e) | Class::Float(e) => {
                            flush(self, &mut run, &mut mn, &mut mx);
                            let cs = self.style(e);
                            let (m, _) = self.margins(cs, 0.0);
                            let (a, b) = self.intrinsic(e);
                            mn = mn.max(a + m[1] + m[3]);
                            mx = mx.max(b + m[1] + m[3]);
                        }
                    }
                }
                flush(self, &mut run, &mut mn, &mut mx);
                (mn, mx)
            }
        };
        (clamp(mn + pbh), clamp(mx + pbh))
    }

    fn items_measure(&mut self, n: NodeId) -> Vec<Vec<Child>> {
        let mut kids = Vec::new();
        self.kids(n, &mut kids);
        let mut out = Vec::new();
        let mut text = Vec::new();
        for c in kids {
            match c {
                Child::Elem(e) => {
                    let s = self.style(e);
                    if s.display == Display::None || matches!(s.position, 2 | 3) || s.gone {
                        continue;
                    }
                    if !text.is_empty() {
                        out.push(std::mem::take(&mut text));
                    }
                    out.push(vec![c]);
                }
                Child::Text(tn) => {
                    if self.dom.text(tn).is_some_and(|s| s.chars().any(|c| !c.is_whitespace())) {
                        text.push(c);
                    }
                }
                _ => text.push(c),
            }
        }
        if !text.is_empty() {
            out.push(text);
        }
        out
    }
}

fn alpha(mut n: i64, base: u8) -> String {
    let mut s = Vec::new();
    while n > 0 {
        n -= 1;
        s.push(base + (n % 26) as u8);
        n /= 26;
    }
    s.reverse();
    String::from_utf8(s).unwrap_or_default()
}

fn roman(mut n: i64) -> String {
    const R: [(i64, &str); 13] = [(1000, "M"), (900, "CM"), (500, "D"), (400, "CD"), (100, "C"), (90, "XC"), (50, "L"), (40, "XL"), (10, "X"), (9, "IX"), (5, "V"), (4, "IV"), (1, "I")];
    let mut s = String::new();
    for (v, r) in R {
        while n >= v {
            s.push_str(r);
            n -= v;
        }
    }
    s
}

#[cfg(test)]
pub mod tests {
    use super::*;
    use crate::web::css::parse_sheet;
    use crate::web::parse::parse;
    use crate::web::style::Cascade;
    use crate::web::ParaOut;

    /// Text engine for tests: every character is half the font size wide, lines are
    /// 1.2 em, breaks at spaces.
    pub struct Mono;
    impl TextEngine for Mono {
        fn layout(&mut self, p: &Para, width: f32, x: f32, y: f32, out: Option<&mut DisplayList>) -> ParaOut {
            let size = p.runs.first().map_or(16.0, |r| r.font.size);
            let cw = size * 0.5;
            let lh = if p.line_h > 0.0 { p.line_h } else { size * 1.2 };
            let mut lx = 0.0;
            let mut ly = 0.0;
            let mut maxw: f32 = 0.0;
            let mut objs = Vec::new();
            let mut glyphs = Vec::new();
            for (i, &c) in p.text.iter().enumerate() {
                let w = if c == 0xFFFC { p.objs.iter().find(|o| o.pos == i as u32).map_or(0.0, |o| o.w) } else { cw };
                if c == 0x2028 || (lx + w > width && lx > 0.0 && !p.nowrap) {
                    ly += lh;
                    lx = 0.0;
                    if c == 0x2028 {
                        continue;
                    }
                }
                if c == 0xFFFC {
                    objs.push((lx, ly));
                } else {
                    glyphs.push(crate::web::Glyph { x: x + lx, y: y + ly + size, id: c, face: 0, size });
                }
                lx += w;
                maxw = maxw.max(lx);
            }
            if let Some(out) = out {
                let start = out.glyphs.len() as u32;
                let n = glyphs.len() as u32;
                out.glyphs.extend(glyphs);
                out.items.push(Item::Text { start, len: n, color: p.runs.first().map_or([0; 4], |r| r.color), clip: 0 });
            }
            ParaOut { height: if p.text.is_empty() { 0.0 } else { ly + lh }, width: maxw, objs }
        }
        fn widths(&mut self, p: &Para) -> (f32, f32) {
            let size = p.runs.first().map_or(16.0, |r| r.font.size);
            let s = String::from_utf16_lossy(&p.text);
            let longest = s.split(' ').map(|w| w.chars().count()).max().unwrap_or(0) as f32;
            (longest * size * 0.5, s.chars().count() as f32 * size * 0.5)
        }
    }

    pub fn render(html: &str, css: &str, vw: f32) -> DisplayList {
        let dom = parse(html);
        let ua = parse_sheet(include_str!("ua.css"));
        let author = parse_sheet(css);
        let sheets = vec![std::sync::Arc::new(ua), std::sync::Arc::new(author)];
        let st = Cascade { dom: &dom, sheets: &sheets, vw, vh: 600.0 }.run();
        let base = Url::parse("https://example.com/").unwrap();
        let mut img = Images::default();
        layout(&dom, &st, &mut Mono, &mut img, &base, vw, 600.0)
    }

    fn rects(dl: &DisplayList, color: Rgba) -> Vec<Rect> {
        dl.items.iter().filter_map(|i| if let Item::Rect { r, color: c, .. } = i { (*c == color).then_some(*r) } else { None }).collect()
    }

    #[test]
    fn blocks_stack_and_margins_collapse() {
        let dl = render(
            "<div class=a>x</div><div class=b>y</div>",
            "body{margin:0} div{height:10px;margin:20px 0} .a{background:#f00} .b{background:#0f0}",
            400.0,
        );
        let a = rects(&dl, [255, 0, 0, 255])[0];
        let b = rects(&dl, [0, 255, 0, 255])[0];
        assert_eq!((a.y, a.h, a.w), (20.0, 10.0, 400.0));
        assert_eq!(b.y, 50.0, "20px margins collapse into one gap");
    }

    #[test]
    fn centered_with_auto_margins_and_max_width() {
        let dl = render("<main>x</main>", "body{margin:0} main{max-width:200px;margin:0 auto;background:#00f}", 600.0);
        let m = rects(&dl, [0, 0, 255, 255])[0];
        assert_eq!((m.x, m.w), (200.0, 200.0));
    }

    #[test]
    fn flex_row_grows_and_centers() {
        let dl = render(
            "<nav><a class=l>logo</a><span class=g></span><b class=r>menu</b></nav>",
            "body{margin:0} nav{display:flex;align-items:center;height:40px} .l{background:#f00;width:50px;height:20px} .g{flex:1;background:#0f0;height:10px} .r{width:60px;height:30px;background:#00f}",
            500.0,
        );
        let l = rects(&dl, [255, 0, 0, 255])[0];
        let g = rects(&dl, [0, 255, 0, 255])[0];
        let r = rects(&dl, [0, 0, 255, 255])[0];
        assert_eq!((l.x, l.w), (0.0, 50.0));
        assert_eq!((g.x, g.w), (50.0, 390.0));
        assert_eq!((r.x, r.w), (440.0, 60.0));
        assert_eq!(l.y, 10.0, "centered in the 40px line");
        assert_eq!(r.y, 5.0);
    }

    #[test]
    fn floats_narrow_following_text() {
        let dl = render(
            "<div class=f></div><p>words words words words</p>",
            "body{margin:0} .f{float:right;width:100px;height:50px;background:#f00} p{margin:0}",
            300.0,
        );
        let f = rects(&dl, [255, 0, 0, 255])[0];
        assert_eq!((f.x, f.y), (200.0, 0.0));
        let maxx = dl.glyphs.iter().map(|g| g.x).fold(0.0, f32::max);
        assert!(maxx < 200.0, "text stays left of the float: {maxx}");
    }

    #[test]
    fn table_columns_share_width() {
        let dl = render(
            "<table><tr><td class=a>aa</td><td class=b>bbbbbbbb</td></tr><tr><td colspan=2 class=c>c</td></tr></table>",
            "body{margin:0} table{border-spacing:0;width:400px} td{padding:0} .a{background:#f00} .b{background:#0f0} .c{background:#00f}",
            800.0,
        );
        let a = rects(&dl, [255, 0, 0, 255])[0];
        let b = rects(&dl, [0, 255, 0, 255])[0];
        let c = rects(&dl, [0, 0, 255, 255])[0];
        assert!((a.w + b.w - 400.0).abs() < 0.5);
        assert!(b.w > a.w * 3.0, "wider content gets the wider column: {a:?} {b:?}");
        assert_eq!(c.w, 400.0);
        assert_eq!(c.y, a.y + a.h);
    }

    #[test]
    fn grid_and_abspos() {
        let dl = render(
            "<div class=g><i></i><i></i><i></i><i></i></div><div class=rel><b class=abs></b></div>",
            "body{margin:0} .g{display:grid;grid-template-columns:repeat(2,1fr);gap:10px} i{display:block;height:20px;background:#f00} .rel{position:relative;height:100px;margin-top:10px} .abs{position:absolute;right:0;bottom:0;width:30px;height:30px;background:#00f}",
            210.0,
        );
        let cells = rects(&dl, [255, 0, 0, 255]);
        assert_eq!(cells.len(), 4);
        assert_eq!((cells[1].x, cells[1].w), (110.0, 100.0));
        assert_eq!(cells[2].y, 30.0);
        let a = rects(&dl, [0, 0, 255, 255])[0];
        assert_eq!((a.x, a.y), (180.0, 130.0));
    }

    /// A search box that fills its row: it must not push the row wider than the page, or
    /// grow taller with its width the way a photo does.
    #[test]
    fn a_full_width_box_stays_in_its_row() {
        let dl = render(
            "<div class=row><div class=grow><input class=q value=x></div><button>go</button></div>",
            ".row{display:flex;width:400px}.grow{flex:1 1 auto}.q{width:100%;box-sizing:border-box;border:1px solid red;background:#0f0}",
            800.0,
        );
        let q = dl.items.iter().find_map(|it| match it {
            Item::Rect { r, color: [0, 255, 0, 255], .. } => Some(*r),
            _ => None,
        });
        let q = q.expect("the box");
        assert!(q.x + q.w < 400.0, "{q:?}");
        assert!(q.h < 40.0, "{q:?}");
    }

    #[test]
    fn controls_can_be_found_and_pressed() {
        let dl = render("<p>a</p><form><label>name <input name=n value=abc></label><input type=hidden name=h><button>go</button></form>", "", 400.0);
        assert_eq!(dl.fields.len(), 1);
        let (r, at) = dl.items.iter().enumerate().find_map(|(i, it)| if let Item::Field { r, .. } = it { Some((*r, i)) } else { None }).expect("a field");
        // its text is always the item after it
        assert!(matches!(dl.items[at + 1], Item::Text { len: 3, .. }));
        assert_eq!(dl.field_at(r.x + 2.0, r.y + 2.0).map(|f| f.0), Some(0));
        assert!(dl.field_at(r.x - 8.0, r.y).is_none());
        let pressed: std::collections::HashSet<&str> = dl.links.iter().map(|l| dl.hrefs[l.href as usize].as_str()).filter(|h| h.starts_with(PRESS)).collect();
        assert_eq!(pressed.len(), 2, "the label and the button");
    }

    /// The hiding picker finds the innermost box under a point, so every box has to be
    /// where it was drawn, table cells and rows too.
    #[test]
    fn recorded_boxes_are_where_they_are_drawn() {
        let dom = parse("<table><tr><td>short</td><td>a much longer cell<br>on two lines</td></tr><tr id=b><td>x</td></tr></table>");
        let ua = crate::web::css::parse_sheet(include_str!("ua.css"));
        let sheets = vec![std::sync::Arc::new(ua)];
        let st = Cascade { dom: &dom, sheets: &sheets, vw: 600.0, vh: 600.0 }.run();
        let base = Url::parse("https://example.com/").unwrap();
        let dl = layout_boxes(&dom, &st, &mut Mono, &mut Images::default(), &base, 600.0, 600.0);
        let cells: Vec<Rect> = dl.boxes.iter().filter(|(n, _)| dom.tag(*n) == t::td).map(|b| b.1).collect();
        assert_eq!(cells.len(), 3);
        // the short cell is stretched to its row, and both start at the row's top
        assert_eq!((cells[0].y, cells[0].h), (cells[1].y, cells[1].h));
        let rows: Vec<Rect> = dl.boxes.iter().filter(|(n, _)| dom.tag(*n) == t::tr).map(|b| b.1).collect();
        assert_eq!(rows.len(), 2);
        assert!(rows[1].y >= cells[0].y + cells[0].h && rows[1].contains(rows[1].x + 1.0, rows[1].y + 1.0));
    }

    #[test]
    fn links_and_hidden_things() {
        let dl = render(
            "<p>go <a href=/next>there</a></p><div hidden>secret</div><span class=sr>skip</span><noscript>no js</noscript>",
            ".sr{position:absolute;width:1px;height:1px;overflow:hidden;clip:rect(0 0 0 0)}",
            400.0,
        );
        assert_eq!(dl.hrefs, vec!["https://example.com/next".to_string()]);
        let text: String = dl.glyphs.iter().map(|g| char::from_u32(g.id as u32).unwrap_or('?')).collect();
        assert!(text.contains("there") && text.contains("no js"), "{text}");
        assert!(!text.contains("secret") && !text.contains("skip"), "{text}");
    }
}
