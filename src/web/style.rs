//! The cascade: which declarations win for each element, with custom properties
//! resolved per element, then the winners turned into computed values. Rules are
//! bucketed by the rightmost compound's id, class or tag, so an element only tests the
//! selectors that could match it (Ladybird does the same).

use super::css::{self, media_matches, split_top, Decl, Sheet};
use super::dom::{atom, lookup, t, Dom, NodeId, NONE};
use std::collections::HashMap;
use std::sync::Arc;

pub type Rgba = [u8; 4];

/// A length: a fixed part plus a percentage of the containing block, or a min/max of those.
#[derive(Clone, Debug, PartialEq)]
pub enum L {
    Auto,
    /// "none" for max-width/max-height
    Nil,
    V(f32, f32),
    Min(Arc<Vec<L>>),
    Max(Arc<Vec<L>>),
}

impl L {
    pub const ZERO: L = L::V(0.0, 0.0);
    pub fn px(v: f32) -> L {
        L::V(v, 0.0)
    }
    pub fn is_auto(&self) -> bool {
        matches!(self, L::Auto)
    }
    /// The used value against a containing block size; None for auto and none.
    pub fn get(&self, cb: f32) -> Option<f32> {
        match self {
            L::Auto | L::Nil => None,
            L::V(px, pct) => Some(if *pct == 0.0 { *px } else { px + pct * cb / 100.0 }),
            L::Min(v) => v.iter().filter_map(|x| x.get(cb)).reduce(f32::min),
            L::Max(v) => v.iter().filter_map(|x| x.get(cb)).reduce(f32::max),
        }
    }
    pub fn or0(&self, cb: f32) -> f32 {
        self.get(cb).unwrap_or(0.0)
    }
    /// Whether the value depends on the containing block.
    pub fn pct(&self) -> bool {
        match self {
            L::V(_, p) => *p != 0.0,
            L::Min(v) | L::Max(v) => v.iter().any(|x| x.pct()),
            _ => false,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Display {
    None,
    Inline,
    Block,
    InlineBlock,
    ListItem,
    Flex,
    InlineFlex,
    Grid,
    InlineGrid,
    Table,
    InlineTable,
    TableRow,
    TableCell,
    TableRowGroup,
    TableCaption,
    TableColumn,
    Contents,
}

impl Display {
    pub fn inline_level(self) -> bool {
        matches!(self, Display::Inline | Display::InlineBlock | Display::InlineFlex | Display::InlineGrid | Display::InlineTable)
    }
}

/// One side of a grid track's size.
#[derive(Clone, Debug, PartialEq)]
pub enum Ts {
    Len(L),
    Fr(f32),
    Auto,
    MinContent,
    MaxContent,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Track {
    pub min: Ts,
    pub max: Ts,
}

/// A grid line placement: start and end lines (1-based, negative from the end, 0 auto)
/// and a span.
#[derive(Clone, Copy, Debug, PartialEq, Default)]
pub struct GLine {
    pub start: i16,
    pub end: i16,
    pub span: u16,
}

#[derive(Clone, Debug, PartialEq, Default)]
pub struct GridDef {
    pub cols: Vec<Track>,
    pub rows: Vec<Track>,
    /// repeat(auto-fill/auto-fit, <track>) columns: the track, repeated to fit
    pub fill: Option<Track>,
    /// grid-template-areas: rows of names (NONE for '.')
    pub areas: Vec<Vec<u32>>,
}

pub struct CNode {
    pub vars: Vec<(u32, Arc<str>)>,
    pub parent: Option<Arc<CNode>>,
}

fn custom_get(c: &Option<Arc<CNode>>, name: u32) -> Option<Arc<str>> {
    let mut cur = c.as_ref();
    while let Some(n) = cur {
        if let Some((_, v)) = n.vars.iter().rev().find(|(k, _)| *k == name) {
            return Some(v.clone());
        }
        cur = n.parent.as_ref();
    }
    None
}

#[derive(Clone)]
pub struct Style {
    pub display: Display,
    /// 0 static, 1 relative, 2 absolute, 3 fixed, 4 sticky
    pub position: u8,
    /// 0 none, 1 left, 2 right
    pub float: u8,
    /// bit 1 left, bit 2 right
    pub clear: u8,
    pub width: L,
    pub height: L,
    pub min_w: L,
    pub max_w: L,
    pub min_h: L,
    pub max_h: L,
    pub margin: [L; 4],
    pub padding: [L; 4],
    pub bw: [f32; 4],
    /// 0 none, 1 solid, 2 dashed, 3 dotted, 4 double, 5 other visible
    pub bs: [u8; 4],
    /// None means currentColor
    pub bc: [Option<Rgba>; 4],
    pub radius: L,
    /// top right bottom left
    pub inset: [L; 4],
    pub color: Rgba,
    pub bg: Rgba,
    pub bg_image: Option<Arc<str>>,
    pub font_family: Arc<str>,
    pub font_size: f32,
    pub font_weight: u16,
    pub italic: bool,
    /// < 0: a multiple of the font size (-1.2 means 1.2), 0: normal, > 0: px
    pub line_height: f32,
    /// 0 start, 1 left, 2 right, 3 center, 4 justify, 5 end
    pub text_align: u8,
    /// 0 normal, 1 nowrap, 2 pre, 3 pre-wrap, 4 pre-line
    pub white_space: u8,
    /// 0 none, 1 upper, 2 lower, 3 capitalize
    pub text_transform: u8,
    /// bit 1 underline, bit 2 line-through (in effect: own plus ancestors')
    pub deco: u8,
    /// 0 none, 1 disc, 2 circle, 3 square, 4 decimal, 5 lower-alpha, 6 upper-alpha, 7 lower-roman, 8 upper-roman
    pub list_style: u8,
    pub visible: bool,
    /// 0 visible, 1 clips (hidden, auto, scroll, clip)
    pub overflow: u8,
    pub opacity: f32,
    pub z: i32,
    pub border_box: bool,
    /// 0 baseline, 1 top, 2 middle, 3 bottom, 4 sub, 5 super
    pub valign: u8,
    /// 0 row, 1 row-reverse, 2 column, 3 column-reverse
    pub flex_dir: u8,
    pub flex_wrap: bool,
    /// 0 start, 1 end, 2 center, 3 space-between, 4 space-around, 5 space-evenly
    pub justify: u8,
    /// 0 stretch, 1 start, 2 end, 3 center, 4 baseline
    pub align_items: u8,
    /// 255 auto
    pub align_self: u8,
    pub grow: f32,
    pub shrink: f32,
    pub basis: L,
    pub row_gap: L,
    pub col_gap: L,
    pub order: i32,
    pub grid: Option<Arc<GridDef>>,
    /// grid-area by name
    pub g_area: u32,
    pub g_col: GLine,
    pub g_row: GLine,
    pub collapse: bool,
    pub spacing: f32,
    pub content: Option<Arc<str>>,
    /// clipped to nothing (the "visually hidden" pattern) or moved off screen
    pub gone: bool,
    pub aspect: Option<f32>,
    /// drawn through a css mask (an icon): its background is the icon's ink, not a box
    pub masked: bool,
    pub customs: Option<Arc<CNode>>,
}

pub fn initial() -> Style {
    Style {
        display: Display::Inline,
        position: 0,
        float: 0,
        clear: 0,
        width: L::Auto,
        height: L::Auto,
        min_w: L::ZERO,
        max_w: L::Nil,
        min_h: L::ZERO,
        max_h: L::Nil,
        margin: [L::ZERO, L::ZERO, L::ZERO, L::ZERO],
        padding: [L::ZERO, L::ZERO, L::ZERO, L::ZERO],
        bw: [3.0; 4],
        bs: [0; 4],
        bc: [None; 4],
        radius: L::ZERO,
        inset: [L::Auto, L::Auto, L::Auto, L::Auto],
        color: [0, 0, 0, 255],
        bg: [0, 0, 0, 0],
        bg_image: None,
        font_family: Arc::from("serif"),
        font_size: 16.0,
        font_weight: 400,
        italic: false,
        line_height: 0.0,
        text_align: 0,
        white_space: 0,
        text_transform: 0,
        deco: 0,
        list_style: 1,
        visible: true,
        overflow: 0,
        opacity: 1.0,
        z: 0,
        border_box: false,
        valign: 0,
        flex_dir: 0,
        flex_wrap: false,
        justify: 0,
        align_items: 0,
        align_self: 255,
        grow: 0.0,
        shrink: 1.0,
        basis: L::Auto,
        row_gap: L::ZERO,
        col_gap: L::ZERO,
        order: 0,
        grid: None,
        g_area: NONE,
        g_col: GLine { start: 0, end: 0, span: 1 },
        g_row: GLine { start: 0, end: 0, span: 1 },
        collapse: false,
        spacing: 0.0,
        content: None,
        gone: false,
        aspect: None,
        masked: false,
        customs: None,
    }
}

impl Style {
    /// The starting point for a child: inherited properties copied, the rest initial.
    pub fn child(&self) -> Style {
        let mut s = initial();
        s.color = self.color;
        s.font_family = self.font_family.clone();
        s.font_size = self.font_size;
        s.font_weight = self.font_weight;
        s.italic = self.italic;
        s.line_height = self.line_height;
        s.text_align = self.text_align;
        s.white_space = self.white_space;
        s.text_transform = self.text_transform;
        s.deco = self.deco;
        s.list_style = self.list_style;
        s.visible = self.visible;
        s.collapse = self.collapse;
        s.spacing = self.spacing;
        s.customs = self.customs.clone();
        s
    }

    /// Line height in px.
    pub fn line_px(&self) -> f32 {
        if self.line_height > 0.0 {
            self.line_height
        } else if self.line_height < 0.0 {
            -self.line_height * self.font_size
        } else {
            (self.font_size * 1.2).round()
        }
    }
}

/// Longhand properties the engine uses.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(u8)]
enum P {
    Display,
    Position,
    Float,
    Clear,
    Width,
    Height,
    MinW,
    MaxW,
    MinH,
    MaxH,
    MT,
    MR,
    MB,
    ML,
    PT,
    PR,
    PB,
    PL,
    BwT,
    BwR,
    BwB,
    BwL,
    BsT,
    BsR,
    BsB,
    BsL,
    BcT,
    BcR,
    BcB,
    BcL,
    Radius,
    Top,
    Right,
    Bottom,
    Left,
    Color,
    Bg,
    BgImage,
    Family,
    Size,
    Weight,
    FStyle,
    LineH,
    Align,
    Ws,
    Transform,
    Deco,
    List,
    Visibility,
    Overflow,
    Opacity,
    Z,
    BoxSizing,
    VAlign,
    FlexDir,
    FlexWrap,
    Justify,
    AlignItems,
    AlignSelf,
    Grow,
    Shrink,
    Basis,
    RowGap,
    ColGap,
    Order,
    GridCols,
    GridRows,
    GridAreas,
    GridArea,
    GridColumn,
    GridRow,
    Collapse,
    Spacing,
    Content,
    Clip,
    ClipPath,
    XForm,
    Aspect,
    Mask,
    Count,
}

const NP: usize = P::Count as usize;

fn longhand(n: &str) -> Option<P> {
    Some(match n {
        "display" => P::Display,
        "position" => P::Position,
        "float" => P::Float,
        "clear" => P::Clear,
        "width" => P::Width,
        "height" => P::Height,
        "min-width" => P::MinW,
        "max-width" => P::MaxW,
        "min-height" => P::MinH,
        "max-height" => P::MaxH,
        "margin-top" | "margin-block-start" => P::MT,
        "margin-right" | "margin-inline-end" => P::MR,
        "margin-bottom" | "margin-block-end" => P::MB,
        "margin-left" | "margin-inline-start" => P::ML,
        "padding-top" | "padding-block-start" => P::PT,
        "padding-right" | "padding-inline-end" => P::PR,
        "padding-bottom" | "padding-block-end" => P::PB,
        "padding-left" | "padding-inline-start" => P::PL,
        "border-top-width" => P::BwT,
        "border-right-width" => P::BwR,
        "border-bottom-width" => P::BwB,
        "border-left-width" => P::BwL,
        "border-top-style" => P::BsT,
        "border-right-style" => P::BsR,
        "border-bottom-style" => P::BsB,
        "border-left-style" => P::BsL,
        "border-top-color" => P::BcT,
        "border-right-color" => P::BcR,
        "border-bottom-color" => P::BcB,
        "border-left-color" => P::BcL,
        "border-top-left-radius" | "border-top-right-radius" | "border-bottom-left-radius" | "border-bottom-right-radius" => P::Radius,
        "top" | "inset-block-start" => P::Top,
        "right" | "inset-inline-end" => P::Right,
        "bottom" | "inset-block-end" => P::Bottom,
        "left" | "inset-inline-start" => P::Left,
        "color" => P::Color,
        "background-color" => P::Bg,
        "background-image" => P::BgImage,
        "font-family" => P::Family,
        "font-size" => P::Size,
        "font-weight" => P::Weight,
        "font-style" => P::FStyle,
        "line-height" => P::LineH,
        "text-align" => P::Align,
        "white-space" | "white-space-collapse" => P::Ws,
        "text-transform" => P::Transform,
        "text-decoration-line" => P::Deco,
        "list-style-type" => P::List,
        "visibility" => P::Visibility,
        "overflow-x" | "overflow-y" => P::Overflow,
        "opacity" => P::Opacity,
        "z-index" => P::Z,
        "box-sizing" => P::BoxSizing,
        "vertical-align" => P::VAlign,
        "flex-direction" => P::FlexDir,
        "flex-wrap" => P::FlexWrap,
        "justify-content" => P::Justify,
        "align-items" => P::AlignItems,
        "align-self" => P::AlignSelf,
        "flex-grow" => P::Grow,
        "flex-shrink" => P::Shrink,
        "flex-basis" => P::Basis,
        "row-gap" => P::RowGap,
        "column-gap" => P::ColGap,
        "order" => P::Order,
        "grid-template-columns" => P::GridCols,
        "grid-template-rows" => P::GridRows,
        "grid-template-areas" => P::GridAreas,
        "grid-column" => P::GridColumn,
        "grid-row" => P::GridRow,
        "border-collapse" => P::Collapse,
        "border-spacing" => P::Spacing,
        "content" => P::Content,
        "clip" => P::Clip,
        "clip-path" => P::ClipPath,
        "transform" | "translate" => P::XForm,
        "aspect-ratio" => P::Aspect,
        "mask-image" | "-webkit-mask-image" | "mask" | "-webkit-mask" => P::Mask,
        _ => return None,
    })
}

fn inherited(p: P) -> bool {
    matches!(p, P::Color | P::Family | P::Size | P::Weight | P::FStyle | P::LineH | P::Align | P::Ws | P::Transform | P::List | P::Visibility | P::Collapse | P::Spacing)
}

/// Splits values at whitespace outside parentheses.
fn words(v: &str) -> Vec<&str> {
    let b = v.as_bytes();
    let mut out = Vec::new();
    let mut depth = 0;
    let mut st = None;
    for (i, &c) in b.iter().enumerate() {
        match c {
            b'(' => depth += 1,
            b')' => depth -= 1,
            _ => {}
        }
        if c.is_ascii_whitespace() && depth == 0 {
            if let Some(s) = st.take() {
                out.push(&v[s..i]);
            }
        } else if st.is_none() {
            st = Some(i);
        }
    }
    if let Some(s) = st {
        out.push(&v[s..]);
    }
    out
}

fn four<'a>(v: &'a str) -> [&'a str; 4] {
    let w = words(v);
    match w.len() {
        0 => ["", "", "", ""],
        1 => [w[0]; 4],
        2 => [w[0], w[1], w[0], w[1]],
        3 => [w[0], w[1], w[2], w[1]],
        _ => [w[0], w[1], w[2], w[3]],
    }
}

fn is_style_kw(w: &str) -> bool {
    matches!(w, "none" | "hidden" | "solid" | "dashed" | "dotted" | "double" | "groove" | "ridge" | "inset" | "outset")
}

/// A shorthand to longhands; longhands map to themselves.
fn expand(name: &str, v: &str, out: &mut Vec<(P, String)>) {
    let sides = |out: &mut Vec<(P, String)>, ps: [P; 4], v: &str| {
        let f = four(v);
        for k in 0..4 {
            out.push((ps[k], f[k].to_string()));
        }
    };
    let global = matches!(v, "inherit" | "initial" | "unset" | "revert" | "revert-layer");
    match name {
        "margin" => sides(out, [P::MT, P::MR, P::MB, P::ML], v),
        "padding" => sides(out, [P::PT, P::PR, P::PB, P::PL], v),
        "border-width" => sides(out, [P::BwT, P::BwR, P::BwB, P::BwL], v),
        "border-style" => sides(out, [P::BsT, P::BsR, P::BsB, P::BsL], v),
        "border-color" => sides(out, [P::BcT, P::BcR, P::BcB, P::BcL], v),
        "inset" => sides(out, [P::Top, P::Right, P::Bottom, P::Left], v),
        "margin-inline" | "margin-block" | "padding-inline" | "padding-block" | "inset-inline" | "inset-block" => {
            let w = words(v);
            let (a, b) = (w.first().copied().unwrap_or(""), w.get(1).or(w.first()).copied().unwrap_or(""));
            let ps = match name {
                "margin-inline" => [P::ML, P::MR],
                "margin-block" => [P::MT, P::MB],
                "padding-inline" => [P::PL, P::PR],
                "padding-block" => [P::PT, P::PB],
                "inset-inline" => [P::Left, P::Right],
                _ => [P::Top, P::Bottom],
            };
            out.push((ps[0], a.to_string()));
            out.push((ps[1], b.to_string()));
        }
        "border" | "border-top" | "border-right" | "border-bottom" | "border-left" | "border-block" | "border-inline" | "border-block-start" | "border-block-end"
        | "border-inline-start" | "border-inline-end" => {
            let idx: &[usize] = match name {
                "border-top" | "border-block-start" => &[0],
                "border-right" | "border-inline-end" => &[1],
                "border-bottom" | "border-block-end" => &[2],
                "border-left" | "border-inline-start" => &[3],
                "border-block" => &[0, 2],
                "border-inline" => &[1, 3],
                _ => &[0, 1, 2, 3],
            };
            let (mut w, mut s, mut c) = ("medium".to_string(), "none".to_string(), "currentcolor".to_string());
            if global {
                (w, s, c) = (v.to_string(), v.to_string(), v.to_string());
            } else {
                for t in words(v) {
                    let lt = t.to_ascii_lowercase();
                    if is_style_kw(&lt) {
                        s = lt;
                    } else if lt.starts_with(|c: char| c.is_ascii_digit() || c == '.') || matches!(lt.as_str(), "thin" | "medium" | "thick") || lt.starts_with("calc(") {
                        w = lt;
                    } else {
                        c = t.to_string();
                    }
                }
            }
            for &k in idx {
                out.push(([P::BwT, P::BwR, P::BwB, P::BwL][k], w.clone()));
                out.push(([P::BsT, P::BsR, P::BsB, P::BsL][k], s.clone()));
                out.push(([P::BcT, P::BcR, P::BcB, P::BcL][k], c.clone()));
            }
        }
        "border-radius" => out.push((P::Radius, words(v.split('/').next().unwrap_or("")).first().copied().unwrap_or("0").to_string())),
        "background" => {
            if global {
                out.push((P::Bg, v.to_string()));
                out.push((P::BgImage, v.to_string()));
                return;
            }
            let mut color = "transparent".to_string();
            let mut image = "none".to_string();
            for layer in split_top(v, b',') {
                for w in words(layer) {
                    let lw = w.to_ascii_lowercase();
                    if lw.starts_with("url(") {
                        image = w.to_string();
                    } else if parse_color(w, [0, 0, 0, 255]).is_some() {
                        color = w.to_string();
                    }
                }
            }
            out.push((P::Bg, color));
            out.push((P::BgImage, image));
        }
        "font" => {
            if global {
                for p in [P::FStyle, P::Weight, P::Size, P::LineH, P::Family] {
                    out.push((p, v.to_string()));
                }
                return;
            }
            let w = words(v);
            let mut k = 0;
            let (mut style, mut weight) = ("normal", "normal");
            while k < w.len() {
                let x = w[k];
                match x {
                    "italic" | "oblique" => style = x,
                    "bold" | "bolder" | "lighter" => weight = x,
                    "normal" | "small-caps" => {}
                    _ if x.parse::<f32>().is_ok() && !x.contains('.') && x.len() == 3 => weight = x,
                    _ => break,
                }
                k += 1;
            }
            let Some(size) = w.get(k) else { return };
            let (size, lh) = match size.split_once('/') {
                Some((a, b)) => (a, Some(b)),
                None => (*size, None),
            };
            let mut rest = w[k + 1..].join(" ");
            let lh = match lh {
                Some(l) if !l.is_empty() => l.to_string(),
                _ => {
                    if let Some(r) = rest.strip_prefix('/') {
                        let mut it = r.trim().splitn(2, ' ');
                        let l = it.next().unwrap_or("normal").to_string();
                        rest = it.next().unwrap_or("").to_string();
                        l
                    } else {
                        "normal".to_string()
                    }
                }
            };
            out.push((P::FStyle, style.to_string()));
            out.push((P::Weight, weight.to_string()));
            out.push((P::Size, size.to_string()));
            out.push((P::LineH, lh));
            if !rest.is_empty() {
                out.push((P::Family, rest));
            }
        }
        "flex" => {
            if global {
                for p in [P::Grow, P::Shrink, P::Basis] {
                    out.push((p, v.to_string()));
                }
                return;
            }
            let w = words(v);
            let (g, s, b) = match (w.len(), w.first().copied().unwrap_or("")) {
                (1, "none") => ("0", "0", "auto"),
                (1, "auto") => ("1", "1", "auto"),
                (1, x) if x.parse::<f32>().is_ok() => (x, "1", "0%"),
                (1, x) => ("1", "1", x),
                (2, x) if w[1].parse::<f32>().is_ok() => (x, w[1], "0%"),
                (2, x) => (x, "1", w[1]),
                _ => (w[0], w[1], w[2]),
            };
            out.push((P::Grow, g.to_string()));
            out.push((P::Shrink, s.to_string()));
            out.push((P::Basis, b.to_string()));
        }
        "flex-flow" => {
            for w in words(v) {
                if w.contains("wrap") {
                    out.push((P::FlexWrap, w.to_string()));
                } else {
                    out.push((P::FlexDir, w.to_string()));
                }
            }
        }
        "gap" | "grid-gap" => {
            let w = words(v);
            out.push((P::RowGap, w.first().copied().unwrap_or("0").to_string()));
            out.push((P::ColGap, w.get(1).or(w.first()).copied().unwrap_or("0").to_string()));
        }
        "grid-row-gap" => out.push((P::RowGap, v.to_string())),
        "grid-column-gap" => out.push((P::ColGap, v.to_string())),
        "overflow" => out.push((P::Overflow, words(v).into_iter().max_by_key(|w| (*w != "visible") as u8).unwrap_or("visible").to_string())),
        "list-style" => {
            for w in words(v) {
                if !w.starts_with("url(") && !matches!(w, "inside" | "outside") {
                    out.push((P::List, w.to_string()));
                }
            }
        }
        "text-decoration" => out.push((P::Deco, v.to_string())),
        "place-items" => out.push((P::AlignItems, words(v).first().copied().unwrap_or("normal").to_string())),
        "grid-template" => {
            if global || v == "none" {
                for p in [P::GridCols, P::GridRows, P::GridAreas] {
                    out.push((p, v.to_string()));
                }
                return;
            }
            let (rows_part, cols) = match split_top(v, b'/').as_slice() {
                [r, c] => (r.to_string(), c.trim().to_string()),
                _ => (v.to_string(), "none".to_string()),
            };
            if rows_part.contains('"') || rows_part.contains('\'') {
                // 'a b' 40px 'c d' 1fr: strings are areas, what follows each is its row size
                let mut areas = String::new();
                let mut rows = String::new();
                let b = rows_part.as_bytes();
                let mut i = 0;
                while i < b.len() {
                    if b[i] == b'"' || b[i] == b'\'' {
                        let q = b[i];
                        let e = rows_part[i + 1..].find(q as char).map_or(b.len(), |e| i + 1 + e);
                        areas.push_str(&rows_part[i..(e + 1).min(b.len())]);
                        areas.push(' ');
                        i = e + 1;
                        let st = i.min(b.len());
                        while i < b.len() && b[i] != b'"' && b[i] != b'\'' {
                            i += 1;
                        }
                        let size = rows_part[st..i.min(b.len())].trim();
                        rows.push_str(if size.is_empty() { "auto" } else { size });
                        rows.push(' ');
                    } else {
                        i += 1;
                    }
                }
                out.push((P::GridAreas, areas));
                out.push((P::GridRows, rows));
            } else {
                out.push((P::GridRows, rows_part.trim().to_string()));
                out.push((P::GridAreas, "none".to_string()));
            }
            out.push((P::GridCols, cols));
        }
        "grid" => {
            if let Some((rows, cols)) = v.split_once('/')
                && !rows.contains("auto-flow")
                && !cols.contains("auto-flow")
            {
                out.push((P::GridRows, rows.trim().to_string()));
                out.push((P::GridCols, cols.trim().to_string()));
            }
        }
        "grid-area" => out.push((P::GridArea, v.to_string())),
        "grid-column-start" | "grid-row-start" => out.push((if name.contains("column") { P::GridColumn } else { P::GridRow }, v.to_string())),
        "grid-column-end" | "grid-row-end" => out.push((if name.contains("column") { P::GridColumn } else { P::GridRow }, format!("auto / {v}"))),
        _ => {
            if let Some(p) = longhand(name) {
                out.push((p, v.to_string()));
            }
        }
    }
}

#[derive(Clone, Copy)]
pub struct Ctx {
    pub em: f32,
    pub rem: f32,
    pub vw: f32,
    pub vh: f32,
}

#[derive(Clone, Copy, Default, Debug)]
struct CV {
    px: f32,
    pct: f32,
    num: f32,
    is_num: bool,
}

/// calc()/min()/max()/clamp() and plain lengths.
pub fn parse_len(v: &str, c: &Ctx) -> Option<L> {
    let v = v.trim();
    let lv = v.to_ascii_lowercase();
    match lv.as_str() {
        "auto" | "fit-content" | "max-content" | "min-content" | "stretch" | "-webkit-fill-available" | "-moz-available" | "-webkit-fit-content" => return Some(L::Auto),
        "none" => return Some(L::Nil),
        "0" => return Some(L::ZERO),
        _ => {}
    }
    let mut p = LenParser { s: lv.as_bytes(), i: 0, c };
    let r = p.expr()?;
    p.ws();
    if p.i != p.s.len() {
        return None;
    }
    match r {
        LE::V(cv) if cv.is_num && cv.num == 0.0 => Some(L::ZERO),
        LE::V(cv) if cv.is_num => None,
        LE::V(cv) => Some(L::V(cv.px, cv.pct)),
        LE::L(l) => Some(l),
    }
}

enum LE {
    V(CV),
    L(L),
}

struct LenParser<'a> {
    s: &'a [u8],
    i: usize,
    c: &'a Ctx,
}

impl LenParser<'_> {
    fn ws(&mut self) {
        while self.i < self.s.len() && self.s[self.i].is_ascii_whitespace() {
            self.i += 1;
        }
    }
    fn expr(&mut self) -> Option<LE> {
        let mut a = self.term()?;
        loop {
            self.ws();
            let Some(&op) = self.s.get(self.i) else { break };
            if op != b'+' && op != b'-' {
                break;
            }
            self.i += 1;
            let b = self.term()?;
            a = match (a, b) {
                (LE::V(x), LE::V(y)) => {
                    let s = if op == b'+' { 1.0 } else { -1.0 };
                    LE::V(CV { px: x.px + s * y.px, pct: x.pct + s * y.pct, num: x.num + s * y.num, is_num: x.is_num && y.is_num })
                }
                (LE::L(l), _) | (_, LE::L(l)) => LE::L(l),
            };
        }
        Some(a)
    }
    fn term(&mut self) -> Option<LE> {
        let mut a = self.unit()?;
        loop {
            self.ws();
            let Some(&op) = self.s.get(self.i) else { break };
            if op != b'*' && op != b'/' {
                break;
            }
            self.i += 1;
            let b = self.unit()?;
            a = match (a, b) {
                (LE::V(x), LE::V(y)) => {
                    if op == b'*' {
                        if y.is_num {
                            LE::V(CV { px: x.px * y.num, pct: x.pct * y.num, num: x.num * y.num, is_num: x.is_num })
                        } else if x.is_num {
                            LE::V(CV { px: y.px * x.num, pct: y.pct * x.num, num: 0.0, is_num: false })
                        } else {
                            return None;
                        }
                    } else if y.is_num && y.num != 0.0 {
                        LE::V(CV { px: x.px / y.num, pct: x.pct / y.num, num: x.num / y.num, is_num: x.is_num })
                    } else {
                        return None;
                    }
                }
                (LE::L(l), _) => LE::L(l),
                (_, LE::L(l)) => LE::L(l),
            };
        }
        Some(a)
    }
    fn unit(&mut self) -> Option<LE> {
        self.ws();
        let s = self.s;
        if self.i >= s.len() {
            return None;
        }
        if s[self.i] == b'(' {
            self.i += 1;
            let r = self.expr()?;
            self.ws();
            if s.get(self.i) == Some(&b')') {
                self.i += 1;
            }
            return Some(r);
        }
        if s[self.i].is_ascii_alphabetic() || s[self.i] == b'-' && s.get(self.i + 1).is_some_and(|c| c.is_ascii_alphabetic()) {
            let st = self.i;
            while self.i < s.len() && (s[self.i].is_ascii_alphanumeric() || s[self.i] == b'-') {
                self.i += 1;
            }
            let name = std::str::from_utf8(&s[st..self.i]).ok()?.to_string();
            if s.get(self.i) != Some(&b'(') {
                return None;
            }
            self.i += 1;
            let mut args = Vec::new();
            loop {
                args.push(self.expr()?);
                self.ws();
                match s.get(self.i) {
                    Some(b',') => self.i += 1,
                    Some(b')') => {
                        self.i += 1;
                        break;
                    }
                    _ => return None,
                }
            }
            let to_l = |e: LE| -> L {
                match e {
                    LE::V(cv) => L::V(cv.px, cv.pct),
                    LE::L(l) => l,
                }
            };
            return Some(match name.as_str() {
                "calc" | "-webkit-calc" => args.into_iter().next()?,
                "min" | "max" => {
                    let all_fixed = args.iter().all(|a| matches!(a, LE::V(cv) if cv.pct == 0.0));
                    if all_fixed {
                        let vals: Vec<CV> = args.iter().map(|a| if let LE::V(cv) = a { *cv } else { CV::default() }).collect();
                        let pick = |a: &CV| if a.is_num { a.num } else { a.px };
                        let best = if name == "min" {
                            vals.iter().copied().reduce(|a, b| if pick(&b) < pick(&a) { b } else { a })
                        } else {
                            vals.iter().copied().reduce(|a, b| if pick(&b) > pick(&a) { b } else { a })
                        };
                        LE::V(best?)
                    } else {
                        let ls: Vec<L> = args.into_iter().map(to_l).collect();
                        LE::L(if name == "min" { L::Min(Arc::new(ls)) } else { L::Max(Arc::new(ls)) })
                    }
                }
                "clamp" if args.len() == 3 => {
                    let mut it = args.into_iter();
                    let (lo, mid, hi) = (to_l(it.next()?), to_l(it.next()?), to_l(it.next()?));
                    LE::L(L::Max(Arc::new(vec![lo, L::Min(Arc::new(vec![mid, hi]))])))
                }
                _ => return None,
            });
        }
        // a number with an optional unit
        let st = self.i;
        if matches!(s[self.i], b'+' | b'-') {
            self.i += 1;
        }
        while self.i < s.len() && (s[self.i].is_ascii_digit() || s[self.i] == b'.') {
            self.i += 1;
        }
        if self.i < s.len() && (s[self.i] == b'e' || s[self.i] == b'E') && s.get(self.i + 1).is_some_and(|c| c.is_ascii_digit() || *c == b'-' || *c == b'+') {
            self.i += 2;
            while self.i < s.len() && s[self.i].is_ascii_digit() {
                self.i += 1;
            }
        }
        let n: f32 = std::str::from_utf8(&s[st..self.i]).ok()?.parse().ok()?;
        let ust = self.i;
        while self.i < s.len() && (s[self.i].is_ascii_alphabetic() || s[self.i] == b'%') {
            self.i += 1;
        }
        let c = self.c;
        let px = match std::str::from_utf8(&s[ust..self.i]).ok()? {
            "" => return Some(LE::V(CV { num: n, is_num: true, ..Default::default() })),
            "%" => return Some(LE::V(CV { pct: n, ..Default::default() })),
            "px" => n,
            "em" => n * c.em,
            "rem" => n * c.rem,
            "ex" | "ch" | "cap" => n * c.em * 0.5,
            "lh" | "rlh" => n * c.em * 1.2,
            "vw" | "dvw" | "svw" | "lvw" | "vi" | "cqw" | "cqi" => n * c.vw / 100.0,
            "vh" | "dvh" | "svh" | "lvh" | "vb" | "cqh" | "cqb" => n * c.vh / 100.0,
            "vmin" | "dvmin" | "svmin" => n * c.vw.min(c.vh) / 100.0,
            "vmax" | "dvmax" | "svmax" => n * c.vw.max(c.vh) / 100.0,
            "pt" => n * 96.0 / 72.0,
            "pc" => n * 16.0,
            "in" => n * 96.0,
            "cm" => n * 96.0 / 2.54,
            "mm" => n * 96.0 / 25.4,
            "q" => n * 96.0 / 101.6,
            _ => return None,
        };
        Some(LE::V(CV { px, ..Default::default() }))
    }
}

fn hexval(h: &str) -> Option<Rgba> {
    let d: Vec<u8> = h.bytes().map(|c| (c as char).to_digit(16).map(|x| x as u8)).collect::<Option<_>>()?;
    Some(match d.len() {
        3 => [d[0] * 17, d[1] * 17, d[2] * 17, 255],
        4 => [d[0] * 17, d[1] * 17, d[2] * 17, d[3] * 17],
        6 => [d[0] * 16 + d[1], d[2] * 16 + d[3], d[4] * 16 + d[5], 255],
        8 => [d[0] * 16 + d[1], d[2] * 16 + d[3], d[4] * 16 + d[5], d[6] * 16 + d[7]],
        _ => return None,
    })
}

fn num_or_pct(s: &str, scale: f32) -> Option<f32> {
    let s = s.trim();
    if s == "none" {
        return Some(0.0);
    }
    if let Some(p) = s.strip_suffix('%') {
        return Some(p.trim().parse::<f32>().ok()? / 100.0 * scale);
    }
    let s = s.trim_end_matches("deg");
    s.parse().ok()
}

fn srgb_from_linear(x: f32) -> u8 {
    let x = x.clamp(0.0, 1.0);
    let v = if x <= 0.003_130_8 { 12.92 * x } else { 1.055 * x.powf(1.0 / 2.4) - 0.055 };
    (v * 255.0).round() as u8
}

fn oklab_rgb(l: f32, a: f32, b: f32) -> [u8; 3] {
    let l_ = l + 0.396_337_78 * a + 0.215_803_76 * b;
    let m_ = l - 0.105_561_346 * a - 0.063_854_17 * b;
    let s_ = l - 0.089_484_18 * a - 1.291_485_5 * b;
    let (l3, m3, s3) = (l_ * l_ * l_, m_ * m_ * m_, s_ * s_ * s_);
    let r = 4.076_741_7 * l3 - 3.307_711_6 * m3 + 0.230_969_94 * s3;
    let g = -1.268_438 * l3 + 2.609_757_4 * m3 - 0.341_319_38 * s3;
    let bl = -0.004_196_086_3 * l3 - 0.703_418_6 * m3 + 1.707_614_7 * s3;
    [srgb_from_linear(r), srgb_from_linear(g), srgb_from_linear(bl)]
}

fn hsl_rgb(h: f32, s: f32, l: f32) -> [u8; 3] {
    let h = h.rem_euclid(360.0) / 360.0;
    let f = |n: f32| {
        let k = (n + h * 12.0) % 12.0;
        let a = s * l.min(1.0 - l);
        l - a * (k - 3.0).min(9.0 - k).clamp(-1.0, 1.0)
    };
    [(f(0.0) * 255.0).round() as u8, (f(8.0) * 255.0).round() as u8, (f(4.0) * 255.0).round() as u8]
}

/// A css color; `cur` stands for currentColor.
pub fn parse_color(v: &str, cur: Rgba) -> Option<Rgba> {
    let v = v.trim();
    let lv = v.to_ascii_lowercase();
    if let Some(h) = lv.strip_prefix('#') {
        return hexval(h);
    }
    if let Some(p) = lv.find('(') {
        let name = &lv[..p];
        let inner = lv[p + 1..].trim_end_matches(')');
        if name == "light-dark" {
            return parse_color(split_top(inner, b',').first()?, cur);
        }
        if name == "color-mix" {
            // the first color stands in for the mix
            let parts = split_top(inner, b',');
            let c = parts.get(1)?.trim();
            let c = words(c).into_iter().find(|w| !w.ends_with('%'))?;
            return parse_color(c, cur);
        }
        let (body, alpha) = match inner.split_once('/') {
            Some((a, b)) => (a.to_string(), Some(b.trim().to_string())),
            None => (inner.to_string(), None),
        };
        let mut parts: Vec<String> = if body.contains(',') { body.split(',').map(|x| x.trim().to_string()).collect() } else { body.split_whitespace().map(|x| x.to_string()).collect() };
        let alpha = alpha.or_else(|| (parts.len() == 4).then(|| parts.pop().unwrap_or_default()));
        let a = alpha.and_then(|a| num_or_pct(&a, 1.0)).unwrap_or(1.0);
        if parts.len() < 3 {
            return None;
        }
        let rgb = match name {
            "rgb" | "rgba" => [num_or_pct(&parts[0], 255.0)?, num_or_pct(&parts[1], 255.0)?, num_or_pct(&parts[2], 255.0)?].map(|x| x.round().clamp(0.0, 255.0) as u8),
            "hsl" | "hsla" => hsl_rgb(num_or_pct(&parts[0], 360.0)?, num_or_pct(&parts[1], 1.0)?.min(1.0), num_or_pct(&parts[2], 1.0)?.min(1.0)),
            "oklch" => {
                let l = num_or_pct(&parts[0], 1.0)?;
                let c = num_or_pct(&parts[1], 0.4)?;
                let h = num_or_pct(&parts[2], 360.0)?.to_radians();
                oklab_rgb(l, c * h.cos(), c * h.sin())
            }
            "oklab" => oklab_rgb(num_or_pct(&parts[0], 1.0)?, num_or_pct(&parts[1], 0.4)?, num_or_pct(&parts[2], 0.4)?),
            _ => return None,
        };
        return Some([rgb[0], rgb[1], rgb[2], (a.clamp(0.0, 1.0) * 255.0).round() as u8]);
    }
    match lv.as_str() {
        "transparent" => return Some([0, 0, 0, 0]),
        "currentcolor" => return Some(cur),
        _ => {}
    }
    named_color(&lv).map(|c| [(c >> 16) as u8, (c >> 8) as u8, c as u8, 255])
}

fn named_color(n: &str) -> Option<u32> {
    const C: &[(&str, u32)] = &[
        ("aliceblue", 0xf0f8ff), ("antiquewhite", 0xfaebd7), ("aqua", 0x00ffff), ("aquamarine", 0x7fffd4), ("azure", 0xf0ffff), ("beige", 0xf5f5dc), ("bisque", 0xffe4c4),
        ("black", 0x000000), ("blanchedalmond", 0xffebcd), ("blue", 0x0000ff), ("blueviolet", 0x8a2be2), ("brown", 0xa52a2a), ("burlywood", 0xdeb887), ("cadetblue", 0x5f9ea0),
        ("chartreuse", 0x7fff00), ("chocolate", 0xd2691e), ("coral", 0xff7f50), ("cornflowerblue", 0x6495ed), ("cornsilk", 0xfff8dc), ("crimson", 0xdc143c), ("cyan", 0x00ffff),
        ("darkblue", 0x00008b), ("darkcyan", 0x008b8b), ("darkgoldenrod", 0xb8860b), ("darkgray", 0xa9a9a9), ("darkgreen", 0x006400), ("darkgrey", 0xa9a9a9), ("darkkhaki", 0xbdb76b),
        ("darkmagenta", 0x8b008b), ("darkolivegreen", 0x556b2f), ("darkorange", 0xff8c00), ("darkorchid", 0x9932cc), ("darkred", 0x8b0000), ("darksalmon", 0xe9967a), ("darkseagreen", 0x8fbc8f),
        ("darkslateblue", 0x483d8b), ("darkslategray", 0x2f4f4f), ("darkslategrey", 0x2f4f4f), ("darkturquoise", 0x00ced1), ("darkviolet", 0x9400d3), ("deeppink", 0xff1493),
        ("deepskyblue", 0x00bfff), ("dimgray", 0x696969), ("dimgrey", 0x696969), ("dodgerblue", 0x1e90ff), ("firebrick", 0xb22222), ("floralwhite", 0xfffaf0), ("forestgreen", 0x228b22),
        ("fuchsia", 0xff00ff), ("gainsboro", 0xdcdcdc), ("ghostwhite", 0xf8f8ff), ("gold", 0xffd700), ("goldenrod", 0xdaa520), ("gray", 0x808080), ("green", 0x008000),
        ("greenyellow", 0xadff2f), ("grey", 0x808080), ("honeydew", 0xf0fff0), ("hotpink", 0xff69b4), ("indianred", 0xcd5c5c), ("indigo", 0x4b0082), ("ivory", 0xfffff0), ("khaki", 0xf0e68c),
        ("lavender", 0xe6e6fa), ("lavenderblush", 0xfff0f5), ("lawngreen", 0x7cfc00), ("lemonchiffon", 0xfffacd), ("lightblue", 0xadd8e6), ("lightcoral", 0xf08080), ("lightcyan", 0xe0ffff),
        ("lightgoldenrodyellow", 0xfafad2), ("lightgray", 0xd3d3d3), ("lightgreen", 0x90ee90), ("lightgrey", 0xd3d3d3), ("lightpink", 0xffb6c1), ("lightsalmon", 0xffa07a),
        ("lightseagreen", 0x20b2aa), ("lightskyblue", 0x87cefa), ("lightslategray", 0x778899), ("lightslategrey", 0x778899), ("lightsteelblue", 0xb0c4de), ("lightyellow", 0xffffe0),
        ("lime", 0x00ff00), ("limegreen", 0x32cd32), ("linen", 0xfaf0e6), ("magenta", 0xff00ff), ("maroon", 0x800000), ("mediumaquamarine", 0x66cdaa), ("mediumblue", 0x0000cd),
        ("mediumorchid", 0xba55d3), ("mediumpurple", 0x9370db), ("mediumseagreen", 0x3cb371), ("mediumslateblue", 0x7b68ee), ("mediumspringgreen", 0x00fa9a), ("mediumturquoise", 0x48d1cc),
        ("mediumvioletred", 0xc71585), ("midnightblue", 0x191970), ("mintcream", 0xf5fffa), ("mistyrose", 0xffe4e1), ("moccasin", 0xffe4b5), ("navajowhite", 0xffdead), ("navy", 0x000080),
        ("oldlace", 0xfdf5e6), ("olive", 0x808000), ("olivedrab", 0x6b8e23), ("orange", 0xffa500), ("orangered", 0xff4500), ("orchid", 0xda70d6), ("palegoldenrod", 0xeee8aa),
        ("palegreen", 0x98fb98), ("paleturquoise", 0xafeeee), ("palevioletred", 0xdb7093), ("papayawhip", 0xffefd5), ("peachpuff", 0xffdab9), ("peru", 0xcd853f), ("pink", 0xffc0cb),
        ("plum", 0xdda0dd), ("powderblue", 0xb0e0e6), ("purple", 0x800080), ("rebeccapurple", 0x663399), ("red", 0xff0000), ("rosybrown", 0xbc8f8f), ("royalblue", 0x4169e1),
        ("saddlebrown", 0x8b4513), ("salmon", 0xfa8072), ("sandybrown", 0xf4a460), ("seagreen", 0x2e8b57), ("seashell", 0xfff5ee), ("sienna", 0xa0522d), ("silver", 0xc0c0c0),
        ("skyblue", 0x87ceeb), ("slateblue", 0x6a5acd), ("slategray", 0x708090), ("slategrey", 0x708090), ("snow", 0xfffafa), ("springgreen", 0x00ff7f), ("steelblue", 0x4682b4),
        ("tan", 0xd2b48c), ("teal", 0x008080), ("thistle", 0xd8bfd8), ("tomato", 0xff6347), ("turquoise", 0x40e0d0), ("violet", 0xee82ee), ("wheat", 0xf5deb3), ("white", 0xffffff),
        ("whitesmoke", 0xf5f5f5), ("yellow", 0xffff00), ("yellowgreen", 0x9acd32),
        // system colors
        ("canvas", 0xffffff), ("canvastext", 0x000000), ("linktext", 0x0000ee), ("buttonface", 0xefefef), ("buttontext", 0x000000), ("buttonborder", 0x767676),
        ("field", 0xffffff), ("fieldtext", 0x000000), ("graytext", 0x6d6d6d), ("highlight", 0x3390ff), ("highlighttext", 0xffffff), ("threedface", 0xc0c0c0),
    ];
    C.iter().find(|(k, _)| *k == n).map(|(_, v)| *v)
}

/// Every element's computed style.
pub struct Styles {
    /// node -> index into list (NONE for non-elements)
    pub of: Vec<u32>,
    pub list: Vec<Style>,
    pub before: HashMap<NodeId, Style>,
    pub after: HashMap<NodeId, Style>,
    /// media results this cascade used, to know when a resize needs a new cascade
    pub media_key: Vec<bool>,
}

impl Styles {
    pub fn get(&self, n: NodeId) -> Option<&Style> {
        let i = *self.of.get(n as usize)?;
        self.list.get(i as usize)
    }
}

struct Index {
    by_id: HashMap<u32, Vec<(u32, u16)>>,
    by_class: HashMap<u32, Vec<(u32, u16)>>,
    by_tag: HashMap<u32, Vec<(u32, u16)>>,
    any: Vec<(u32, u16)>,
}

/// Rule references pack the sheet in the top 12 bits and the rule in the rest, which
/// also orders them as the cascade wants.
fn rref(sheet: usize, rule: usize) -> u32 {
    ((sheet as u32) << 20) | rule as u32
}

fn build_index(sheets: &[Arc<Sheet>]) -> Index {
    let mut ix = Index { by_id: HashMap::new(), by_class: HashMap::new(), by_tag: HashMap::new(), any: Vec::new() };
    for (si, sh) in sheets.iter().enumerate() {
        for (ri, r) in sh.rules.iter().enumerate() {
            for (k, s) in r.sels.iter().enumerate() {
                let c = &s.parts[0];
                if c.never {
                    continue;
                }
                let e = (rref(si, ri), k as u16);
                if c.id != NONE {
                    ix.by_id.entry(c.id).or_default().push(e);
                } else if let Some(&cl) = c.classes.first() {
                    ix.by_class.entry(cl).or_default().push(e);
                } else if c.tag != NONE {
                    ix.by_tag.entry(c.tag).or_default().push(e);
                } else {
                    ix.any.push(e);
                }
            }
        }
    }
    ix
}

pub struct Cascade<'a> {
    pub dom: &'a Dom,
    /// sheet 0 is the user agent's
    pub sheets: &'a [Arc<Sheet>],
    pub vw: f32,
    pub vh: f32,
}

/// Keys the cascade orders declarations by.
#[derive(PartialEq, Eq, PartialOrd, Ord)]
struct Key {
    rank: u8,
    spec: u32,
    order: u32,
    idx: u16,
}

pub fn media_key(sheets: &[Arc<Sheet>], vw: f32, vh: f32) -> Vec<bool> {
    sheets.iter().flat_map(|s| s.media.iter().map(move |m| media_matches(m, vw, vh))).collect()
}

impl Cascade<'_> {
    pub fn run(&self) -> Styles {
        let ix = build_index(self.sheets);
        let mk = media_key(self.sheets, self.vw, self.vh);
        let mut media_ok: Vec<Vec<bool>> = Vec::new();
        let mut k = 0;
        for s in self.sheets {
            media_ok.push(mk[k..k + s.media.len()].to_vec());
            k += s.media.len();
        }
        let mut st = Styles { of: vec![NONE; self.dom.nodes.len()], list: Vec::new(), before: HashMap::new(), after: HashMap::new(), media_key: mk };
        let root = initial();
        let mut rem = 16.0;
        // depth-first with an explicit stack: (node, parent style index)
        let mut work: Vec<(NodeId, u32)> = Vec::new();
        for c in self.dom.children(0).collect::<Vec<_>>().into_iter().rev() {
            work.push((c, NONE));
        }
        let mut pseudo: [Vec<(Key, &Decl)>; 2] = [Vec::new(), Vec::new()];
        while let Some((n, pidx)) = work.pop() {
            let Some(e) = self.dom.elem(n) else { continue };
            let mut extra: Vec<Decl> = Vec::new();
            let mut matched: Vec<(Key, &Decl)> = Vec::new();
            pseudo[0].clear();
            pseudo[1].clear();
            let mut cands: Vec<&(u32, u16)> = Vec::new();
            if e.id != NONE
                && let Some(v) = ix.by_id.get(&e.id)
            {
                cands.extend(v);
            }
            for c in &e.classes {
                if let Some(v) = ix.by_class.get(c) {
                    cands.extend(v);
                }
            }
            if let Some(v) = ix.by_tag.get(&e.tag) {
                cands.extend(v);
            }
            cands.extend(&ix.any);
            for &&(rr, si) in &cands {
                let (sh, ri) = ((rr >> 20) as usize, (rr & 0xFFFFF) as usize);
                let rule = &self.sheets[sh].rules[ri];
                if !media_ok[sh][rule.media as usize] {
                    continue;
                }
                let sel = &rule.sels[si as usize];
                if !css::matches(self.dom, sel, n) {
                    continue;
                }
                for (di, d) in rule.decls.iter().enumerate() {
                    let rank = match (sh == 0, d.important) {
                        (true, false) => 0,
                        (false, false) => 2,
                        (false, true) => 4,
                        (true, true) => 6,
                    };
                    let key = Key { rank, spec: sel.spec, order: rr, idx: di as u16 };
                    if sel.pseudo == 0 {
                        matched.push((key, d));
                    } else {
                        pseudo[sel.pseudo as usize - 1].push((key, d));
                    }
                }
            }
            // presentational attributes, then the style attribute
            hints(self.dom, n, &mut extra);
            let hint_n = extra.len();
            if let Some(sa) = self.dom.attr(n, t::style) {
                extra.extend(css::parse_decls(sa));
            }
            for (i, d) in extra.iter().enumerate() {
                let rank = if i < hint_n { 1 } else if d.important { 5 } else { 3 };
                matched.push((Key { rank, spec: 0, order: u32::MAX, idx: i as u16 }, d));
            }
            matched.sort_by(|a, b| a.0.cmp(&b.0));
            let parent = if pidx == NONE { &root } else { &st.list[pidx as usize] };
            let s = compute(&matched, parent, rem, self.vw, self.vh, self.dom, n);
            if e.tag == t::html {
                rem = s.font_size;
            }
            for (k, list) in pseudo.iter_mut().enumerate() {
                if list.is_empty() {
                    continue;
                }
                list.sort_by(|a, b| a.0.cmp(&b.0));
                let ps = compute(list, &s, rem, self.vw, self.vh, self.dom, n);
                if ps.content.is_some() && ps.display != Display::None {
                    if k == 0 {
                        st.before.insert(n, ps);
                    } else {
                        st.after.insert(n, ps);
                    }
                }
            }
            let idx = st.list.len() as u32;
            st.of[n as usize] = idx;
            let skip_children = s.display == Display::None;
            st.list.push(s);
            if !skip_children {
                let kids: Vec<NodeId> = self.dom.children(n).filter(|&c| self.dom.elem(c).is_some()).collect();
                for c in kids.into_iter().rev() {
                    work.push((c, idx));
                }
            }
        }
        st
    }
}

/// Declarations implied by old presentational attributes (bgcolor, width, align...).
fn hints(dom: &Dom, n: NodeId, out: &mut Vec<Decl>) {
    let Some(e) = dom.elem(n) else { return };
    let mut add = |name: &str, value: String| out.push(Decl { name: name.into(), value: value.into(), important: false });
    let dim = |v: &str| -> Option<String> {
        let v = v.trim();
        let num: String = v.chars().take_while(|c| c.is_ascii_digit() || *c == '.').collect();
        if num.is_empty() {
            return None;
        }
        Some(if v[num.len()..].starts_with('%') { format!("{num}%") } else { format!("{num}px") })
    };
    for (k, v) in &e.attrs {
        match *k {
            t::bgcolor => add("background-color", v.clone()),
            t::width if matches!(e.tag, t::table | t::td | t::th | t::img | t::col | t::hr | t::iframe | t::video | t::canvas | t::input | t::svg) => {
                if let Some(d) = dim(v) {
                    add("width", d);
                }
            }
            t::height if matches!(e.tag, t::table | t::td | t::th | t::tr | t::img | t::iframe | t::video | t::canvas | t::svg) => {
                if let Some(d) = dim(v) {
                    add("height", d);
                }
            }
            t::align => {
                let a = v.to_ascii_lowercase();
                if e.tag == t::table || e.tag == t::img {
                    match a.as_str() {
                        "center" => {
                            add("margin-left", "auto".into());
                            add("margin-right", "auto".into());
                        }
                        "left" | "right" => add("float", a),
                        _ => {}
                    }
                } else if matches!(a.as_str(), "left" | "right" | "center" | "justify") {
                    add("text-align", a);
                }
            }
            t::valign => add("vertical-align", v.to_ascii_lowercase()),
            t::nowrap if matches!(e.tag, t::td | t::th) => add("white-space", "nowrap".into()),
            t::color if e.tag == t::font => add("color", v.clone()),
            t::face if e.tag == t::font => add("font-family", v.clone()),
            t::size if e.tag == t::font => {
                let sz = match v.trim() {
                    "1" | "-2" => "x-small",
                    "2" | "-1" => "small",
                    "3" => "medium",
                    "4" | "+1" => "large",
                    "5" | "+2" => "x-large",
                    "6" | "+3" => "xx-large",
                    "7" | "+4" => "xxx-large",
                    _ => continue,
                };
                add("font-size", sz.into());
            }
            t::border if e.tag == t::table => {
                let w = v.trim().parse::<f32>().unwrap_or(1.0);
                if w > 0.0 {
                    add("border", format!("{w}px outset gray"));
                }
            }
            t::cellspacing if e.tag == t::table => {
                if let Some(d) = dim(v) {
                    add("border-spacing", d);
                }
            }
            t::hidden => add("display", "none".into()),
            _ => {}
        }
    }
    if matches!(e.tag, t::td | t::th) {
        // cellpadding and border live on the table
        let mut p = dom.parent_elem(n);
        while let Some(x) = p {
            if dom.tag(x) == t::table {
                if let Some(cp) = dom.attr(x, t::cellpadding).and_then(dim) {
                    add("padding", cp);
                }
                if dom.attr(x, t::border).and_then(|b| b.trim().parse::<f32>().ok()).is_some_and(|b| b > 0.0) {
                    add("border", "1px inset gray".into());
                }
                break;
            }
            p = dom.parent_elem(x);
        }
    }
    if e.tag == t::body {
        if let Some(c) = dom.attr(n, atom("text")) {
            add("color", c.to_string());
        }
    }
}

/// Replaces var() references with custom property values.
fn substitute(v: &str, customs: &Option<Arc<CNode>>, depth: u8) -> Option<String> {
    if depth > 8 {
        return None;
    }
    let mut out = String::with_capacity(v.len());
    let mut rest = v;
    while let Some(p) = rest.find("var(") {
        out.push_str(&rest[..p]);
        let after = &rest[p + 4..];
        // find the matching ')'
        let b = after.as_bytes();
        let mut depthp = 1;
        let mut e = 0;
        while e < b.len() {
            match b[e] {
                b'(' => depthp += 1,
                b')' => {
                    depthp -= 1;
                    if depthp == 0 {
                        break;
                    }
                }
                _ => {}
            }
            e += 1;
        }
        let inner = &after[..e.min(after.len())];
        rest = &after[(e + 1).min(after.len())..];
        let (name, fallback) = match inner.find(',') {
            Some(c) => (inner[..c].trim(), Some(inner[c + 1..].trim())),
            None => (inner.trim(), None),
        };
        let val = lookup(name).and_then(|a| custom_get(customs, a));
        match (val, fallback) {
            (Some(v), _) if !v.trim().is_empty() || fallback.is_none() => {
                let v = if v.contains("var(") { substitute(&v, customs, depth + 1)? } else { v.to_string() };
                out.push_str(&v);
            }
            (_, Some(f)) => {
                let f = if f.contains("var(") { substitute(f, customs, depth + 1)? } else { f.to_string() };
                out.push_str(&f);
            }
            _ => return None,
        }
    }
    out.push_str(rest);
    Some(out)
}

fn compute(decls: &[(Key, &Decl)], parent: &Style, rem: f32, vw: f32, vh: f32, dom: &Dom, n: NodeId) -> Style {
    let mut s = parent.child();
    // custom properties first, since every other value may refer to them
    let mut own: Vec<(u32, Arc<str>)> = Vec::new();
    for (_, d) in decls {
        if d.name.starts_with("--") {
            let a = atom(&d.name);
            let v: Arc<str> = Arc::from(d.value.trim());
            if custom_get(&parent.customs, a).as_deref() != Some(&*v) || own.iter().any(|(k, _)| *k == a) {
                own.push((a, v));
            }
        }
    }
    if !own.is_empty() {
        s.customs = Some(Arc::new(CNode { vars: own, parent: parent.customs.clone() }));
    }
    let mut win: [Option<String>; NP] = std::array::from_fn(|_| None);
    let mut buf = Vec::new();
    for (_, d) in decls {
        if d.name.starts_with("--") {
            continue;
        }
        let v = if d.value.contains("var(") {
            match substitute(&d.value, &s.customs, 0) {
                Some(v) => v,
                None => "unset".to_string(),
            }
        } else {
            d.value.to_string()
        };
        buf.clear();
        expand(&d.name, v.trim(), &mut buf);
        for (p, v) in buf.drain(..) {
            win[p as usize] = Some(v);
        }
    }
    // font size first: em units elsewhere depend on it
    if let Some(v) = win[P::Size as usize].take() {
        apply(P::Size, &v, &mut s, parent, rem, vw, vh, dom, n);
    }
    for (i, v) in win.iter().enumerate() {
        if let Some(v) = v {
            // Safety of the cast: i < NP and P is repr(u8) with contiguous values
            let p: P = unsafe { std::mem::transmute::<u8, P>(i as u8) };
            apply(p, v, &mut s, parent, rem, vw, vh, dom, n);
        }
    }
    // a border with style none has no width
    for k in 0..4 {
        if s.bs[k] == 0 {
            s.bw[k] = 0.0;
        }
    }
    if matches!(s.position, 2 | 3) || s.float != 0 {
        s.display = match s.display {
            Display::Inline | Display::InlineBlock | Display::TableRow | Display::TableCell | Display::TableRowGroup | Display::TableCaption => Display::Block,
            Display::InlineFlex => Display::Flex,
            Display::InlineGrid => Display::Grid,
            Display::InlineTable => Display::Table,
            d => d,
        };
    }
    s
}

#[allow(clippy::too_many_arguments)]
fn apply(p: P, v: &str, s: &mut Style, parent: &Style, rem: f32, vw: f32, vh: f32, dom: &Dom, n: NodeId) {
    let lv = v.trim().to_ascii_lowercase();
    if matches!(lv.as_str(), "inherit" | "initial" | "unset" | "revert" | "revert-layer") {
        let from = if lv == "inherit" || ((lv == "unset" || lv.starts_with("revert")) && inherited(p)) { parent.clone() } else { initial() };
        copy(p, s, &from);
        return;
    }
    let c = Ctx { em: s.font_size, rem, vw, vh };
    let len = |v: &str| parse_len(v, &c);
    let len_nn = |v: &str| parse_len(v, &c).filter(|l| !matches!(l, L::V(a, b) if *a < 0.0 || *b < 0.0));
    let side = |p: P, base: P| p as usize - base as usize;
    match p {
        P::Display => {
            let first = lv.split_whitespace().collect::<Vec<_>>();
            let d = match first.as_slice() {
                ["none"] => Display::None,
                ["inline"] | ["inline", "flow"] => Display::Inline,
                ["block"] | ["block", "flow"] | ["flow-root"] | ["block", "flow-root"] | ["run-in"] => Display::Block,
                ["inline-block"] | ["inline", "flow-root"] => Display::InlineBlock,
                ["list-item"] | ["block", "list-item"] | ["list-item", "block"] => Display::ListItem,
                ["flex"] | ["block", "flex"] | ["-webkit-box"] | ["-webkit-flex"] | ["-ms-flexbox"] => Display::Flex,
                ["inline-flex"] | ["inline", "flex"] | ["-webkit-inline-flex"] | ["-webkit-inline-box"] => Display::InlineFlex,
                ["grid"] | ["block", "grid"] => Display::Grid,
                ["inline-grid"] | ["inline", "grid"] => Display::InlineGrid,
                ["table"] => Display::Table,
                ["inline-table"] => Display::InlineTable,
                ["table-row"] => Display::TableRow,
                ["table-cell"] => Display::TableCell,
                ["table-row-group"] | ["table-header-group"] | ["table-footer-group"] => Display::TableRowGroup,
                ["table-caption"] => Display::TableCaption,
                ["table-column"] | ["table-column-group"] => Display::TableColumn,
                ["contents"] => Display::Contents,
                _ => return,
            };
            s.display = d;
        }
        P::Position => {
            s.position = match lv.as_str() {
                "static" => 0,
                "relative" => 1,
                "absolute" => 2,
                "fixed" => 3,
                "sticky" | "-webkit-sticky" => 4,
                _ => return,
            }
        }
        P::Float => {
            s.float = match lv.as_str() {
                "left" | "inline-start" => 1,
                "right" | "inline-end" => 2,
                _ => 0,
            }
        }
        P::Clear => {
            s.clear = match lv.as_str() {
                "left" | "inline-start" => 1,
                "right" | "inline-end" => 2,
                "both" => 3,
                _ => 0,
            }
        }
        P::Width => {
            if let Some(l) = len_nn(&lv) {
                s.width = l;
            }
        }
        P::Height => {
            if let Some(l) = len_nn(&lv) {
                s.height = l;
            }
        }
        P::MinW => {
            if let Some(l) = len_nn(&lv) {
                s.min_w = if l.is_auto() { L::ZERO } else { l };
            }
        }
        P::MaxW => {
            if let Some(l) = len_nn(&lv) {
                s.max_w = if l.is_auto() { L::Nil } else { l };
            }
        }
        P::MinH => {
            if let Some(l) = len_nn(&lv) {
                s.min_h = if l.is_auto() { L::ZERO } else { l };
            }
        }
        P::MaxH => {
            if let Some(l) = len_nn(&lv) {
                s.max_h = if l.is_auto() { L::Nil } else { l };
            }
        }
        P::MT | P::MR | P::MB | P::ML => {
            if let Some(l) = len(&lv) {
                s.margin[side(p, P::MT)] = l;
            }
        }
        P::PT | P::PR | P::PB | P::PL => {
            if let Some(l) = len_nn(&lv) {
                s.padding[side(p, P::PT)] = if l.is_auto() { L::ZERO } else { l };
            }
        }
        P::BwT | P::BwR | P::BwB | P::BwL => {
            let w = match lv.as_str() {
                "thin" => Some(1.0),
                "medium" => Some(3.0),
                "thick" => Some(5.0),
                _ => len_nn(&lv).and_then(|l| l.get(0.0)),
            };
            if let Some(w) = w {
                s.bw[side(p, P::BwT)] = if w > 0.0 && w < 1.0 { 1.0 } else { w.floor() };
            }
        }
        P::BsT | P::BsR | P::BsB | P::BsL => {
            s.bs[side(p, P::BsT)] = match lv.as_str() {
                "none" | "hidden" => 0,
                "solid" => 1,
                "dashed" => 2,
                "dotted" => 3,
                "double" => 4,
                _ => 5,
            }
        }
        P::BcT | P::BcR | P::BcB | P::BcL => {
            let k = side(p, P::BcT);
            if lv == "currentcolor" {
                s.bc[k] = None;
            } else if let Some(c) = parse_color(&lv, s.color) {
                s.bc[k] = Some(c);
            }
        }
        P::Radius => {
            if let Some(l) = len_nn(words(&lv).first().copied().unwrap_or("0")) {
                s.radius = l;
            }
        }
        P::Top | P::Right | P::Bottom | P::Left => {
            if let Some(l) = len(&lv) {
                s.inset[side(p, P::Top)] = l;
            }
        }
        P::Color => {
            if let Some(c) = parse_color(&lv, parent.color) {
                s.color = c;
            }
        }
        P::Bg => {
            if let Some(c) = parse_color(&lv, s.color) {
                s.bg = c;
            }
        }
        P::BgImage => {
            s.bg_image = None;
            if let Some(st) = v.find("url(") {
                let rest = &v[st + 4..];
                let e = rest.find(')').unwrap_or(rest.len());
                let u = rest[..e].trim().trim_matches(|c| c == '"' || c == '\'');
                if !u.is_empty() {
                    s.bg_image = Some(Arc::from(u));
                }
            }
        }
        P::Family => s.font_family = Arc::from(v.trim()),
        P::Size => {
            let pf = parent.font_size;
            let px = match lv.as_str() {
                "xx-small" => 9.0,
                "x-small" => 10.0,
                "small" => 13.0,
                "medium" => 16.0,
                "large" => 18.0,
                "x-large" => 24.0,
                "xx-large" => 32.0,
                "xxx-large" => 48.0,
                "smaller" => pf / 1.2,
                "larger" => pf * 1.2,
                _ => match parse_len(&lv, &Ctx { em: pf, rem, vw, vh }) {
                    Some(l) => l.get(pf * 100.0 / 100.0).map(|x| x.max(0.0)).unwrap_or(pf),
                    None => return,
                },
            };
            s.font_size = px;
        }
        P::Weight => {
            s.font_weight = match lv.as_str() {
                "normal" => 400,
                "bold" => 700,
                "bolder" => (parent.font_weight + 300).min(900).max(700),
                "lighter" => {
                    if parent.font_weight > 500 {
                        400
                    } else {
                        100
                    }
                }
                x => match x.parse::<f32>() {
                    Ok(w) => w.clamp(1.0, 1000.0) as u16,
                    Err(_) => return,
                },
            }
        }
        P::FStyle => s.italic = lv.starts_with("italic") || lv.starts_with("oblique"),
        P::LineH => {
            if lv == "normal" {
                s.line_height = 0.0;
            } else if let Ok(x) = lv.parse::<f32>() {
                s.line_height = -x;
            } else if let Some(l) = len_nn(&lv) {
                s.line_height = l.get(s.font_size).unwrap_or(0.0);
            }
        }
        P::Align => {
            s.text_align = match lv.as_str() {
                "left" | "-webkit-left" => 1,
                "right" | "-webkit-right" => 2,
                "center" | "-webkit-center" | "-moz-center" => 3,
                "justify" => 4,
                "end" => 5,
                _ => 0,
            }
        }
        P::Ws => {
            s.white_space = match lv.as_str() {
                "nowrap" => 1,
                "pre" | "preserve nowrap" => 2,
                "pre-wrap" | "break-spaces" | "preserve" => 3,
                "pre-line" | "preserve-breaks" => 4,
                _ => 0,
            }
        }
        P::Transform => {
            s.text_transform = match lv.as_str() {
                "uppercase" => 1,
                "lowercase" => 2,
                "capitalize" => 3,
                _ => 0,
            }
        }
        P::Deco => {
            let mut d = parent.deco;
            if lv.contains("none") {
                d = parent.deco;
            }
            if lv.contains("underline") {
                d |= 1;
            }
            if lv.contains("line-through") {
                d |= 2;
            }
            s.deco = d;
        }
        P::List => {
            s.list_style = match lv.as_str() {
                "none" => 0,
                "disc" => 1,
                "circle" => 2,
                "square" => 3,
                "decimal" | "decimal-leading-zero" => 4,
                "lower-alpha" | "lower-latin" => 5,
                "upper-alpha" | "upper-latin" => 6,
                "lower-roman" => 7,
                "upper-roman" => 8,
                _ => return,
            }
        }
        P::Visibility => s.visible = !matches!(lv.as_str(), "hidden" | "collapse"),
        P::Overflow => s.overflow = (!matches!(lv.as_str(), "visible")) as u8,
        P::Opacity => {
            if let Some(o) = num_or_pct(&lv, 1.0) {
                s.opacity = o.clamp(0.0, 1.0);
            }
        }
        P::Z => s.z = lv.parse().unwrap_or(0),
        P::BoxSizing => s.border_box = lv == "border-box",
        P::VAlign => {
            s.valign = match lv.as_str() {
                "top" | "text-top" => 1,
                "middle" => 2,
                "bottom" | "text-bottom" => 3,
                "sub" => 4,
                "super" => 5,
                _ => 0,
            }
        }
        P::FlexDir => {
            s.flex_dir = match lv.as_str() {
                "row-reverse" => 1,
                "column" => 2,
                "column-reverse" => 3,
                _ => 0,
            }
        }
        P::FlexWrap => s.flex_wrap = lv.starts_with("wrap"),
        P::Justify => {
            s.justify = match lv.trim_start_matches("safe ").trim_start_matches("unsafe ") {
                "flex-end" | "end" | "right" => 1,
                "center" => 2,
                "space-between" => 3,
                "space-around" => 4,
                "space-evenly" => 5,
                _ => 0,
            }
        }
        P::AlignItems | P::AlignSelf => {
            let a = match lv.trim_start_matches("safe ").trim_start_matches("unsafe ") {
                "auto" => 255,
                "flex-start" | "start" | "self-start" => 1,
                "flex-end" | "end" | "self-end" => 2,
                "center" => 3,
                "baseline" | "first baseline" | "last baseline" => 4,
                _ => 0,
            };
            if p == P::AlignItems {
                s.align_items = if a == 255 { 0 } else { a };
            } else {
                s.align_self = a;
            }
        }
        P::Grow => s.grow = lv.parse().unwrap_or(0.0f32).max(0.0),
        P::Shrink => s.shrink = lv.parse().unwrap_or(1.0f32).max(0.0),
        P::Basis => {
            if lv == "content" {
                s.basis = L::Auto;
            } else if let Some(l) = len_nn(&lv) {
                s.basis = l;
            }
        }
        P::RowGap | P::ColGap => {
            let l = if lv == "normal" { Some(L::ZERO) } else { len_nn(&lv) };
            if let Some(l) = l {
                if p == P::RowGap {
                    s.row_gap = l;
                } else {
                    s.col_gap = l;
                }
            }
        }
        P::Order => s.order = lv.parse().unwrap_or(0),
        P::GridCols | P::GridRows | P::GridAreas => {
            let mut g = s.grid.as_deref().cloned().unwrap_or_default();
            match p {
                P::GridCols => {
                    let (t, fill) = tracks(&lv, &c);
                    g.cols = t;
                    g.fill = fill;
                }
                P::GridRows => g.rows = tracks(&lv, &c).0,
                _ => g.areas = areas(v),
            }
            s.grid = Some(Arc::new(g));
        }
        P::GridArea => {
            let parts = split_top(&lv, b'/');
            let first = parts[0].trim();
            if parts.len() == 1 && first.starts_with(|ch: char| ch.is_ascii_alphabetic() || ch == '-' || ch == '_') && first != "auto" && !first.starts_with("span") {
                s.g_area = atom(v.trim());
            } else {
                s.g_area = NONE;
                let line = |k: usize| parts.get(k).map(|x| x.trim()).unwrap_or("auto");
                s.g_row = gline(&format!("{} / {}", line(0), line(2)));
                s.g_col = gline(&format!("{} / {}", line(1), line(3)));
            }
        }
        P::GridColumn => s.g_col = gline(&lv),
        P::GridRow => s.g_row = gline(&lv),
        P::Collapse => s.collapse = lv == "collapse",
        P::Spacing => {
            if let Some(l) = len_nn(words(&lv).first().copied().unwrap_or("0")) {
                s.spacing = l.or0(0.0);
            }
        }
        P::Content => {
            s.content = content(v, dom, n);
        }
        P::Clip => {
            if lv.starts_with("rect(") {
                let nums: Vec<f32> = lv[5..].trim_end_matches(')').split([',', ' ']).filter(|x| !x.is_empty()).filter_map(|x| x.trim_end_matches("px").parse().ok()).collect();
                if nums.len() == 4 && (nums[2] - nums[0]).abs() <= 1.0 {
                    s.gone = true;
                }
            }
        }
        P::ClipPath => {
            if lv.starts_with("inset(50%") || lv.starts_with("circle(0") || lv == "inset(100%)" || lv.starts_with("polygon(0 0, 0 0") {
                s.gone = true;
            }
        }
        P::XForm => {
            // off-canvas panels a script would slide in
            if (lv.contains("translatex(-100%)") || lv.contains("translatex(100%)") || lv.contains("translatey(-100%)") || lv.contains("translate(-100%") || lv.contains("scale(0)")) && matches!(s.position, 2 | 3) {
                s.gone = true;
            }
        }
        P::Aspect => {
            let w = lv.replace(' ', "");
            let r = match w.split_once('/') {
                Some((a, b)) => a.parse::<f32>().ok().zip(b.parse::<f32>().ok()).map(|(a, b)| a / b.max(0.001)),
                None => w.parse::<f32>().ok(),
            };
            s.aspect = r.filter(|r| *r > 0.0);
        }
        P::Mask => s.masked = lv != "none",
        P::Count => {}
    }
}

fn ts(s: &str, c: &Ctx) -> Ts {
    let s = s.trim();
    match s {
        "auto" => Ts::Auto,
        "min-content" => Ts::MinContent,
        "max-content" => Ts::MaxContent,
        _ => {
            if let Some(f) = s.strip_suffix("fr").and_then(|n| n.trim().parse::<f32>().ok()) {
                Ts::Fr(f.max(0.0))
            } else {
                parse_len(s, c).filter(|l| !l.is_auto()).map_or(Ts::Auto, Ts::Len)
            }
        }
    }
}

fn track(s: &str, c: &Ctx) -> Track {
    let s = s.trim();
    if let Some(inner) = s.strip_prefix("minmax(").and_then(|x| x.strip_suffix(')')) {
        let parts = split_top(inner, b',');
        let a = ts(parts.first().copied().unwrap_or("auto"), c);
        let b = ts(parts.get(1).copied().unwrap_or("auto"), c);
        return Track { min: if matches!(a, Ts::Fr(_)) { Ts::Auto } else { a }, max: b };
    }
    if let Some(inner) = s.strip_prefix("fit-content(").and_then(|x| x.strip_suffix(')')) {
        return Track { min: Ts::Auto, max: ts(inner, c) };
    }
    let t = ts(s, c);
    match t {
        Ts::Fr(_) => Track { min: Ts::Auto, max: t },
        _ => Track { min: t.clone(), max: t },
    }
}

/// A track list; the second value is an auto-fill/auto-fit track.
fn tracks(v: &str, c: &Ctx) -> (Vec<Track>, Option<Track>) {
    let mut out = Vec::new();
    let mut fill = None;
    if v == "none" || v == "subgrid" || v.starts_with("masonry") {
        return (out, fill);
    }
    for w in words(v) {
        if w.starts_with('[') {
            continue;
        }
        if let Some(inner) = w.strip_prefix("repeat(").and_then(|x| x.strip_suffix(')')) {
            let (n, list) = inner.split_once(',').unwrap_or((inner, "1fr"));
            let list: Vec<Track> = words(list).into_iter().filter(|x| !x.starts_with('[')).map(|x| track(x, c)).collect();
            match n.trim() {
                "auto-fill" | "auto-fit" => fill = list.into_iter().next(),
                n => {
                    for _ in 0..n.parse::<usize>().unwrap_or(1).min(100) {
                        out.extend(list.iter().cloned());
                    }
                }
            }
        } else {
            out.push(track(w, c));
        }
    }
    (out, fill)
}

fn areas(v: &str) -> Vec<Vec<u32>> {
    let mut out = Vec::new();
    let mut rest = v;
    while let Some(st) = rest.find(['"', '\'']) {
        let q = rest.as_bytes()[st] as char;
        let Some(e) = rest[st + 1..].find(q) else { break };
        let row = &rest[st + 1..st + 1 + e];
        out.push(row.split_whitespace().map(|n| if n.chars().all(|c| c == '.') { NONE } else { atom(n) }).collect());
        rest = &rest[st + 2 + e..];
    }
    out
}

fn gline(v: &str) -> GLine {
    let parts = split_top(v, b'/');
    let mut g = GLine { start: 0, end: 0, span: 1 };
    let one = |x: &str| -> (i16, u16) {
        let x = x.trim();
        if let Some(n) = x.strip_prefix("span") {
            return (0, n.trim().parse::<u16>().unwrap_or(1).max(1));
        }
        (x.parse::<i16>().unwrap_or(0), 0)
    };
    let (a, sa) = one(parts.first().copied().unwrap_or("auto"));
    g.start = a;
    if sa > 0 {
        g.span = sa;
    }
    if let Some(b) = parts.get(1) {
        let (e, sb) = one(b);
        g.end = e;
        if sb > 0 {
            g.span = sb;
        }
    }
    g
}

fn content(v: &str, dom: &Dom, n: NodeId) -> Option<Arc<str>> {
    let v = v.trim();
    if v == "none" || v == "normal" || v.is_empty() {
        return None;
    }
    let mut out = String::new();
    let b = v.as_bytes();
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'"' | b'\'' => {
                let q = b[i];
                i += 1;
                while i < b.len() && b[i] != q {
                    if b[i] == b'\\' && i + 1 < b.len() {
                        // css escape: hex code point or a literal character
                        let st = i + 1;
                        let mut e = st;
                        while e < b.len() && e - st < 6 && b[e].is_ascii_hexdigit() {
                            e += 1;
                        }
                        if e > st {
                            if let Some(ch) = u32::from_str_radix(&v[st..e], 16).ok().and_then(char::from_u32) {
                                out.push(ch);
                            }
                            i = if e < b.len() && b[e] == b' ' { e + 1 } else { e };
                        } else {
                            i += 1;
                        }
                        continue;
                    }
                    let ch = v[i..].chars().next().unwrap_or(' ');
                    out.push(ch);
                    i += ch.len_utf8();
                }
                i += 1;
            }
            _ if v[i..].starts_with("attr(") => {
                let e = v[i..].find(')').map_or(b.len(), |e| i + e);
                let name = v[i + 5..e].trim();
                if let Some(a) = lookup(name)
                    && let Some(val) = dom.attr(n, a)
                {
                    out.push_str(val);
                }
                i = e + 1;
            }
            _ if v[i..].starts_with("open-quote") => {
                out.push('“');
                i += 10;
            }
            _ if v[i..].starts_with("close-quote") => {
                out.push('”');
                i += 11;
            }
            _ => i += 1,
        }
    }
    // image-only and counter content draws nothing we can show
    Some(Arc::from(out))
}

fn copy(p: P, s: &mut Style, f: &Style) {
    match p {
        P::Display => s.display = f.display,
        P::Position => s.position = f.position,
        P::Float => s.float = f.float,
        P::Clear => s.clear = f.clear,
        P::Width => s.width = f.width.clone(),
        P::Height => s.height = f.height.clone(),
        P::MinW => s.min_w = f.min_w.clone(),
        P::MaxW => s.max_w = f.max_w.clone(),
        P::MinH => s.min_h = f.min_h.clone(),
        P::MaxH => s.max_h = f.max_h.clone(),
        P::MT | P::MR | P::MB | P::ML => {
            let k = p as usize - P::MT as usize;
            s.margin[k] = f.margin[k].clone();
        }
        P::PT | P::PR | P::PB | P::PL => {
            let k = p as usize - P::PT as usize;
            s.padding[k] = f.padding[k].clone();
        }
        P::BwT | P::BwR | P::BwB | P::BwL => {
            let k = p as usize - P::BwT as usize;
            s.bw[k] = f.bw[k];
        }
        P::BsT | P::BsR | P::BsB | P::BsL => {
            let k = p as usize - P::BsT as usize;
            s.bs[k] = f.bs[k];
        }
        P::BcT | P::BcR | P::BcB | P::BcL => {
            let k = p as usize - P::BcT as usize;
            s.bc[k] = f.bc[k];
        }
        P::Radius => s.radius = f.radius.clone(),
        P::Top | P::Right | P::Bottom | P::Left => {
            let k = p as usize - P::Top as usize;
            s.inset[k] = f.inset[k].clone();
        }
        P::Color => s.color = f.color,
        P::Bg => s.bg = f.bg,
        P::BgImage => s.bg_image = f.bg_image.clone(),
        P::Family => s.font_family = f.font_family.clone(),
        P::Size => s.font_size = f.font_size,
        P::Weight => s.font_weight = f.font_weight,
        P::FStyle => s.italic = f.italic,
        P::LineH => s.line_height = f.line_height,
        P::Align => s.text_align = f.text_align,
        P::Ws => s.white_space = f.white_space,
        P::Transform => s.text_transform = f.text_transform,
        P::Deco => s.deco = f.deco,
        P::List => s.list_style = f.list_style,
        P::Visibility => s.visible = f.visible,
        P::Overflow => s.overflow = f.overflow,
        P::Opacity => s.opacity = f.opacity,
        P::Z => s.z = f.z,
        P::BoxSizing => s.border_box = f.border_box,
        P::VAlign => s.valign = f.valign,
        P::FlexDir => s.flex_dir = f.flex_dir,
        P::FlexWrap => s.flex_wrap = f.flex_wrap,
        P::Justify => s.justify = f.justify,
        P::AlignItems => s.align_items = f.align_items,
        P::AlignSelf => s.align_self = f.align_self,
        P::Grow => s.grow = f.grow,
        P::Shrink => s.shrink = f.shrink,
        P::Basis => s.basis = f.basis.clone(),
        P::RowGap => s.row_gap = f.row_gap.clone(),
        P::ColGap => s.col_gap = f.col_gap.clone(),
        P::Order => s.order = f.order,
        P::GridCols | P::GridRows | P::GridAreas => s.grid = f.grid.clone(),
        P::GridArea => {
            s.g_area = f.g_area;
            s.g_col = f.g_col;
            s.g_row = f.g_row;
        }
        P::GridColumn => s.g_col = f.g_col,
        P::GridRow => s.g_row = f.g_row,
        P::Collapse => s.collapse = f.collapse,
        P::Spacing => s.spacing = f.spacing,
        P::Content => s.content = f.content.clone(),
        P::Clip | P::ClipPath | P::XForm => s.gone = f.gone,
        P::Aspect => s.aspect = f.aspect,
        P::Mask => s.masked = f.masked,
        P::Count => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::web::css::parse_sheet;
    use crate::web::parse::parse;

    fn ctx() -> Ctx {
        Ctx { em: 10.0, rem: 16.0, vw: 1000.0, vh: 800.0 }
    }

    #[test]
    fn lengths() {
        let c = ctx();
        assert_eq!(parse_len("2em", &c), Some(L::V(20.0, 0.0)));
        assert_eq!(parse_len("calc(100% - 2rem)", &c), Some(L::V(-32.0, 100.0)));
        assert_eq!(parse_len("calc((100vw - 200px) / 2)", &c), Some(L::V(400.0, 0.0)));
        assert_eq!(parse_len("min(100%, 600px)", &c).unwrap().get(1000.0), Some(600.0));
        assert_eq!(parse_len("clamp(1rem, 2.5vw, 2rem)", &c).unwrap().get(0.0), Some(25.0));
        assert_eq!(parse_len("max(10px, 2em)", &c), Some(L::V(20.0, 0.0)));
        assert_eq!(parse_len("bogus", &c), None);
    }

    #[test]
    fn colors() {
        assert_eq!(parse_color("#fff", [0; 4]), Some([255, 255, 255, 255]));
        assert_eq!(parse_color("rgb(10 20 30 / 50%)", [0; 4]), Some([10, 20, 30, 128]));
        assert_eq!(parse_color("rgba(10,20,30,.5)", [0; 4]), Some([10, 20, 30, 128]));
        assert_eq!(parse_color("hsl(0, 100%, 50%)", [0; 4]), Some([255, 0, 0, 255]));
        assert_eq!(parse_color("rebeccapurple", [0; 4]), Some([0x66, 0x33, 0x99, 255]));
        let w = parse_color("oklch(1 0 0)", [0; 4]).unwrap();
        assert!(w[0] >= 254 && w[1] >= 254 && w[2] >= 254, "{w:?}");
    }

    #[test]
    fn cascade_with_vars_and_specificity() {
        let dom = parse(r#"<html><body><div id="a" class="b"><p class="c" style="margin-top: 3px">x</p></div></body></html>"#);
        let ua = parse_sheet("div, p { display: block } p { margin: 1em 0 }");
        let author = parse_sheet(
            ":root { --accent: #ff6b1a; --gap: 12px } #a p { color: blue } .b .c { color: red } p { color: var(--accent) !important; padding: var(--gap) var(--missing, 4px); font: italic bold 20px/1.5 Inter, sans-serif }",
        );
        let sheets = vec![Arc::new(ua), Arc::new(author)];
        let st = Cascade { dom: &dom, sheets: &sheets, vw: 1000.0, vh: 800.0 }.run();
        let p = dom.find(t::p).unwrap();
        let s = st.get(p).unwrap();
        assert_eq!(s.color, [0xff, 0x6b, 0x1a, 255]);
        assert_eq!(s.padding[0], L::px(12.0));
        assert_eq!(s.padding[1], L::px(4.0));
        assert_eq!(s.margin[0], L::px(3.0));
        assert_eq!(s.font_size, 20.0);
        assert_eq!(s.font_weight, 700);
        assert!(s.italic);
        assert_eq!(s.line_px(), 30.0);
        assert_eq!(&*s.font_family, "Inter, sans-serif");
        assert_eq!(s.display, Display::Block);
    }
}
