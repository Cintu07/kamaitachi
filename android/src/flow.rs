//! Line breaking and glyph placement on Android, the job DirectWrite does on
//! Windows. Each run is cut wherever the font has to change (a Telugu word inside
//! English, say), each piece is shaped with swash, and the clusters are packed into
//! lines greedily. A line may break after a space or a hyphen, between CJK
//! characters, and anywhere at all when one word is wider than the line.
//!
//! Left to right only for now; Arabic and Hebrew come out in logical order.

use crate::fonts;
use hayate::web::{DisplayList, Font, Glyph, Item, Link, Para, ParaOut, Rect, Run, TextEngine};
use std::collections::HashMap;
use std::ops::Range;
use swash::shape::{Direction, ShapeContext};
use swash::text::{Codepoint, Script};

const OBJECT: char = '\u{FFFC}';

/// One shaped glyph, its offsets relative to the pen.
struct G {
    id: u16,
    face: u16,
    size: f32,
    dx: f32,
    dy: f32,
}

/// The smallest thing a line can hold: one cluster of glyphs (or an inline box).
struct Cluster {
    /// utf-16 range in the paragraph
    at: u32,
    end: u32,
    glyphs: Range<usize>,
    adv: f32,
    run: usize,
    ch: char,
    space: bool,
    /// a line may end after this one
    soft: bool,
    /// a line must end after this one
    hard: bool,
    obj: Option<usize>,
}

pub struct Flow {
    shaper: ShapeContext,
    /// (ascent, descent, leading) per em, by face
    metrics: HashMap<u16, (f32, f32, f32)>,
}

fn cjk(c: char) -> bool {
    matches!(c as u32, 0x2E80..=0x9FFF | 0xAC00..=0xD7AF | 0xF900..=0xFAFF | 0xFF00..=0xFFEF | 0x20000..=0x2FFFF)
}

/// The whole text as (start, end, run) pieces. Text no run covers (an inline box
/// that is not a link, say) takes the formatting of the run before it, the way
/// DirectWrite gives such text its base format.
fn spans(p: &Para) -> Vec<(u32, u32, usize)> {
    let n = p.text.len() as u32;
    let mut out = Vec::new();
    if p.runs.is_empty() {
        // a line of nothing but inline boxes has no runs at all; it still needs laying out
        if n > 0 {
            out.push((0, n, usize::MAX));
        }
        return out;
    }
    let (mut pos, mut prev) = (0u32, 0usize);
    for (ri, r) in p.runs.iter().enumerate() {
        let s = r.start.max(pos).min(n);
        if s > pos {
            out.push((pos, s, prev));
        }
        let e = (r.start + r.len).min(n);
        if e > s {
            out.push((s, e, ri));
            pos = e;
        }
        prev = ri;
    }
    if pos < n {
        out.push((pos, n, prev));
    }
    out
}

impl Flow {
    pub fn new() -> Flow {
        Flow { shaper: ShapeContext::new(), metrics: HashMap::new() }
    }

    fn em(&mut self, face: u16) -> (f32, f32, f32) {
        if let Some(&m) = self.metrics.get(&face) {
            return m;
        }
        let m = fonts::get(face)
            .map(|f| {
                let font = f.font();
                let coords: Vec<i16> = font.variations().normalized_coords(f.settings()).collect();
                let m = font.metrics(&coords);
                let u = m.units_per_em.max(1) as f32;
                (m.ascent / u, m.descent / u, m.leading / u)
            })
            .unwrap_or((0.9, 0.22, 0.0));
        self.metrics.insert(face, m);
        m
    }

    /// Shapes the whole paragraph into clusters, in text order.
    fn shape(&mut self, p: &Para) -> (Vec<Cluster>, Vec<G>) {
        let mut clusters = Vec::new();
        let mut glyphs = Vec::new();
        let plain = Run { start: 0, len: 0, font: Font { family: "sans-serif".into(), size: 16.0, weight: 400, italic: false }, color: [0, 0, 0, 255], deco: 0, link: u32::MAX };
        for (from, end, ri) in spans(p) {
            let r = p.runs.get(ri).unwrap_or(&plain);
            let primary = fonts::pick(&r.font.family, r.font.weight, r.font.italic);
            // one piece of text in one face, waiting to be shaped
            let mut seg = String::new();
            let mut seg_at: Vec<u32> = Vec::new();
            let mut seg_face = primary;
            let units = &p.text[from as usize..end as usize];
            let mut i = from;
            for ch in char::decode_utf16(units.iter().copied()) {
                let ch = ch.unwrap_or('\u{FFFD}');
                let n = ch.len_utf16() as u32;
                if ch == OBJECT || ch == '\n' || ch == '\u{2028}' {
                    self.flush(&mut seg, &mut seg_at, seg_face, r.font.size, ri, &mut clusters, &mut glyphs);
                    let obj = (ch == OBJECT).then(|| p.objs.iter().position(|o| o.pos == i)).flatten();
                    let adv = obj.map_or(0.0, |o| p.objs[o].w);
                    clusters.push(Cluster { at: i, end: i + n, glyphs: glyphs.len()..glyphs.len(), adv, run: ri, ch, space: false, soft: true, hard: ch != OBJECT, obj });
                } else {
                    let face = fonts::cover(primary, ch);
                    if face != seg_face && !seg.is_empty() {
                        self.flush(&mut seg, &mut seg_at, seg_face, r.font.size, ri, &mut clusters, &mut glyphs);
                    }
                    seg_face = face;
                    for _ in 0..ch.len_utf8() {
                        seg_at.push(i);
                    }
                    seg.push(ch);
                }
                i += n;
            }
            seg_at.push(i);
            self.flush(&mut seg, &mut seg_at, seg_face, r.font.size, ri, &mut clusters, &mut glyphs);
        }
        // a line may also break before a CJK character
        for k in 1..clusters.len() {
            if cjk(clusters[k].ch) {
                clusters[k - 1].soft = true;
            }
        }
        (clusters, glyphs)
    }

    /// Shapes one piece of text in one face. `at` maps each byte of it (and one past
    /// the end) to its utf-16 index in the paragraph.
    #[allow(clippy::too_many_arguments)]
    fn flush(&mut self, text: &mut String, at: &mut Vec<u32>, face: u16, size: f32, run: usize, clusters: &mut Vec<Cluster>, glyphs: &mut Vec<G>) {
        if text.is_empty() {
            at.clear();
            return;
        }
        if at.len() == text.len() {
            // the index one past the end, so the last cluster knows where it stops
            let last = at.last().copied().unwrap_or(0) + text.chars().last().map_or(1, |c| c.len_utf16() as u32);
            at.push(last);
        }
        let Some(f) = fonts::get(face) else {
            text.clear();
            at.clear();
            return;
        };
        let script = text.chars().map(|c| c.script()).find(|s| !matches!(s, Script::Common | Script::Inherited | Script::Unknown)).unwrap_or(Script::Latin);
        let settings = f.settings();
        let mut shaper = self.shaper.builder(f.font()).size(size).script(script).direction(Direction::LeftToRight).variations(settings.iter().copied()).build();
        shaper.add_str(text);
        shaper.shape_with(|c| {
            let s = c.source.start as usize;
            let e = (c.source.end as usize).min(at.len() - 1);
            let ch = text[s..].chars().next().unwrap_or(' ');
            let start = glyphs.len();
            let mut adv = 0.0;
            for g in c.glyphs {
                glyphs.push(G { id: g.id, face, size, dx: g.x, dy: g.y });
                adv += g.advance;
            }
            let space = ch == ' ' || ch == '\t';
            let soft = space || ch == '-' || ch == '\u{2010}' || ch == '/' || cjk(ch);
            clusters.push(Cluster { at: at[s], end: at[e], glyphs: start..glyphs.len(), adv, run, ch, space, soft, hard: false, obj: None });
        });
        text.clear();
        at.clear();
    }

    /// Greedy line breaking: [start, end) cluster ranges.
    fn lines(cl: &[Cluster], width: f32, nowrap: bool) -> Vec<Range<usize>> {
        let mut out = Vec::new();
        let (mut a, mut w, mut last) = (0usize, 0.0f32, None::<usize>);
        let mut i = 0;
        while i < cl.len() {
            let c = &cl[i];
            if c.hard {
                out.push(a..i + 1);
                (a, w, last) = (i + 1, 0.0, None);
                i += 1;
                continue;
            }
            if !nowrap && i > a && !c.space && w + c.adv > width + 0.01 {
                match last {
                    Some(b) => {
                        out.push(a..b + 1);
                        a = b + 1;
                        w = cl[a..i].iter().map(|c| c.adv).sum();
                    }
                    None => {
                        // one word wider than the line: break inside it
                        out.push(a..i);
                        (a, w) = (i, 0.0);
                    }
                }
                last = None;
                continue;
            }
            w += c.adv;
            if c.soft {
                last = Some(i);
            }
            i += 1;
        }
        if a < cl.len() || out.is_empty() {
            out.push(a..cl.len());
        }
        out
    }
}

impl Default for Flow {
    fn default() -> Self {
        Self::new()
    }
}

impl TextEngine for Flow {
    fn layout(&mut self, p: &Para, width: f32, x: f32, y: f32, out: Option<&mut DisplayList>) -> ParaOut {
        let mut po = ParaOut { height: 0.0, width: 0.0, objs: vec![(0.0, 0.0); p.objs.len()] };
        if p.text.is_empty() {
            return po;
        }
        let (cl, gl) = self.shape(p);
        let lines = Flow::lines(&cl, width, p.nowrap);
        let base = p.runs.first().map(|r| (fonts::pick(&r.font.family, r.font.weight, r.font.italic), r.font.size)).unwrap_or((0, 16.0));
        let tall = p.objs.iter().any(|o| o.h > p.line_h);
        let mut out = out;
        let mut top = 0.0f32;
        for line in &lines {
            let items = &cl[line.clone()];
            // trailing spaces hang past the edge and do not count
            let trail = items.iter().rev().take_while(|c| c.space || c.hard).count();
            let lw: f32 = items[..items.len() - trail].iter().map(|c| c.adv).sum();
            let (mut asc, mut desc, mut lead) = (0.0f32, 0.0f32, 0.0f32);
            for c in items {
                if let Some(o) = c.obj {
                    asc = asc.max(p.objs[o].h);
                    continue;
                }
                let (face, size) = match gl.get(c.glyphs.start) {
                    Some(g) => (g.face, g.size),
                    None => (base.0, p.runs.get(c.run).map_or(base.1, |r| r.font.size)),
                };
                let (a, d, l) = self.em(face);
                asc = asc.max(a * size);
                desc = desc.max(d * size);
                lead = lead.max(l * size);
            }
            if asc == 0.0 && desc == 0.0 {
                let (a, d, _) = self.em(base.0);
                (asc, desc) = (a * base.1, d * base.1);
            }
            let (lh, baseline) = if p.line_h > 0.0 && !tall {
                let (a, d, _) = self.em(base.0);
                (p.line_h, (p.line_h - (a + d) * base.1) / 2.0 + a * base.1)
            } else {
                (asc + desc + lead, asc + lead / 2.0)
            };
            let off = match p.align {
                2 | 5 => (width - lw).max(0.0),
                3 => ((width - lw) / 2.0).max(0.0),
                _ => 0.0,
            };
            // where each cluster of this line starts
            let mut xs = Vec::with_capacity(items.len());
            let mut pen = x + off;
            for c in items {
                xs.push(pen);
                if let Some(o) = c.obj {
                    po.objs[o] = (pen - x, top + baseline - p.objs[o].h);
                }
                pen += c.adv;
            }
            if let Some(out) = out.as_deref_mut() {
                let ly = y + top;
                let span = |from: u32, to: u32| -> Option<(f32, f32)> {
                    let mut r: Option<(f32, f32)> = None;
                    for (k, c) in items.iter().enumerate() {
                        if c.at < to && c.end > from && !c.hard {
                            let (x0, x1) = (xs[k], xs[k] + c.adv);
                            r = Some(r.map_or((x0, x1), |(a, b)| (a.min(x0), b.max(x1))));
                        }
                    }
                    r
                };
                // inline backgrounds go under the glyphs
                for &(s, l, col) in &p.bgs {
                    if let Some((x0, x1)) = span(s, s + l) {
                        out.items.push(Item::Rect { r: Rect::new(x0 - 1.0, ly, x1 - x0 + 2.0, lh), color: col, radius: 3.0, clip: 0 });
                    }
                }
                // glyphs, one Text item per stretch of the same run
                let mut k = 0;
                while k < items.len() {
                    let run = items[k].run;
                    let first = out.glyphs.len() as u32;
                    while k < items.len() && items[k].run == run {
                        for g in &gl[items[k].glyphs.clone()] {
                            out.glyphs.push(Glyph { x: xs[k] + g.dx, y: ly + baseline - g.dy, id: g.id, face: g.face, size: g.size });
                        }
                        k += 1;
                    }
                    let n = out.glyphs.len() as u32 - first;
                    if n > 0 {
                        out.items.push(Item::Text { start: first, len: n, color: p.runs.get(run).map_or([0, 0, 0, 255], |r| r.color), clip: 0 });
                    }
                }
                for r in &p.runs {
                    if r.deco == 0 || r.color[3] == 0 {
                        continue;
                    }
                    let Some((x0, x1)) = span(r.start, r.start + r.len) else { continue };
                    let th = (r.font.size / 14.0).max(1.0);
                    let bl = ly + baseline;
                    if r.deco & 1 != 0 {
                        out.items.push(Item::Rect { r: Rect::new(x0, bl + r.font.size * 0.11, x1 - x0, th), color: r.color, radius: 0.0, clip: 0 });
                    }
                    if r.deco & 2 != 0 {
                        out.items.push(Item::Rect { r: Rect::new(x0, bl - r.font.size * 0.3, x1 - x0, th), color: r.color, radius: 0.0, clip: 0 });
                    }
                }
                // link areas, one per link per line
                let mut seen: Vec<u32> = Vec::new();
                for r in &p.runs {
                    if r.link == u32::MAX || seen.contains(&r.link) {
                        continue;
                    }
                    seen.push(r.link);
                    let (mut from, mut to) = (u32::MAX, 0);
                    for rr in p.runs.iter().filter(|rr| rr.link == r.link) {
                        from = from.min(rr.start);
                        to = to.max(rr.start + rr.len);
                    }
                    if let Some((x0, x1)) = span(from, to) {
                        out.links.push(Link { r: Rect::new(x0, ly, x1 - x0, lh), href: r.link });
                    }
                }
            }
            po.width = po.width.max(lw);
            top += lh;
        }
        po.height = top;
        po
    }

    fn widths(&mut self, p: &Para) -> (f32, f32) {
        if p.text.is_empty() {
            return (0.0, 0.0);
        }
        let (cl, _) = self.shape(p);
        let (mut min, mut max, mut word, mut line) = (0.0f32, 0.0f32, 0.0f32, 0.0f32);
        for c in &cl {
            if !c.space {
                word += c.adv;
            }
            line += c.adv;
            if c.soft || c.hard {
                min = min.max(word);
                word = 0.0;
            }
            if c.hard {
                max = max.max(line);
                line = 0.0;
            }
        }
        (min.max(word).ceil(), max.max(line).ceil())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    fn para(parts: &[(&str, u16)]) -> Para {
        let mut p = Para::default();
        for (t, weight) in parts {
            let start = p.text.len() as u32;
            p.text.extend(t.encode_utf16());
            let len = p.text.len() as u32 - start;
            p.runs.push(Run { start, len, font: Font { family: Arc::from("sans-serif"), size: 16.0, weight: *weight, italic: false }, color: [0, 0, 0, 255], deco: 0, link: u32::MAX });
        }
        p
    }

    #[test]
    fn a_space_between_runs_keeps_its_width() {
        let mut f = Flow::new();
        let p = para(&[("Hacker News", 700), (" ", 400), ("new", 400)]);
        let (cl, _) = f.shape(&p);
        let space = cl.iter().find(|c| c.at == 11).expect("the space has a cluster");
        assert!(space.space && space.soft && space.adv > 0.0);
    }

    #[test]
    fn a_box_after_text_sits_after_it() {
        let mut f = Flow::new();
        let mut p = para(&[("Article", 400)]);
        p.text.push(0xFFFC);
        p.objs.push(hayate::web::Obj { pos: 7, w: 40.0, h: 10.0 });
        let out = f.layout(&p, 500.0, 0.0, 0.0, None);
        assert!(out.objs[0].0 > 40.0, "{:?}", out.objs);
    }

    #[test]
    fn a_row_of_boxes_with_no_text_is_laid_out() {
        // like Wikipedia's tabs: inline boxes and not one run
        let mut f = Flow::new();
        let mut row = Para::default();
        for i in 0..3u32 {
            row.text.push(0xFFFC);
            row.objs.push(hayate::web::Obj { pos: i, w: 50.0, h: 20.0 });
        }
        let out = f.layout(&row, 500.0, 0.0, 0.0, None);
        assert_eq!(out.objs.iter().map(|o| o.0).collect::<Vec<_>>(), vec![0.0, 50.0, 100.0]);
    }

    #[test]
    fn words_wrap_and_the_longest_sets_the_minimum() {
        let mut f = Flow::new();
        let p = para(&[("aaa bbb ccc ddd eee fff", 400)]);
        let (min, max) = f.widths(&p);
        assert!(min < max / 4.0, "{min} {max}");
        let one = f.layout(&p, 1000.0, 0.0, 0.0, None).height;
        let narrow = f.layout(&p, min + 1.0, 0.0, 0.0, None).height;
        assert!(narrow > one * 5.0, "one line {one}, narrow {narrow}");
    }
}
