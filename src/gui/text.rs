//! Text through DirectWrite: it shapes (every script, ligatures, fallback fonts) and
//! breaks lines; a recorder catches each glyph run so the GPU renderer can draw the
//! glyphs itself. Font faces get small ids so a display list can name them.

use crate::web::{DisplayList, Font, Item, Link, Para, ParaOut, Rect, Rgba, TextEngine};
use std::collections::HashMap;
use std::ffi::c_void;
use std::sync::{Arc, Mutex, OnceLock};
use windows::core::{implement, Interface, Ref, Result, BOOL, PCWSTR};
use windows::Win32::Foundation::{FALSE, TRUE};

/// HRESULT_FROM_WIN32(ERROR_INSUFFICIENT_BUFFER)
const E_NOT_SUFFICIENT_BUFFER: windows::core::HRESULT = windows::core::HRESULT(0x8007_007Au32 as i32);
use windows::Win32::Graphics::DirectWrite::*;

pub struct Face(pub IDWriteFontFace);
// DirectWrite objects from a shared factory are free-threaded.
unsafe impl Send for Face {}
unsafe impl Sync for Face {}

struct Faces {
    list: Vec<Arc<Face>>,
    by_ptr: HashMap<usize, u16>,
}

fn faces() -> &'static Mutex<Faces> {
    static F: OnceLock<Mutex<Faces>> = OnceLock::new();
    F.get_or_init(|| Mutex::new(Faces { list: Vec::new(), by_ptr: HashMap::new() }))
}

fn face_id(f: &IDWriteFontFace) -> u16 {
    let mut fs = faces().lock().unwrap_or_else(|e| e.into_inner());
    let key = f.as_raw() as usize;
    if let Some(&id) = fs.by_ptr.get(&key) {
        return id;
    }
    let id = fs.list.len().min(u16::MAX as usize) as u16;
    fs.list.push(Arc::new(Face(f.clone())));
    fs.by_ptr.insert(key, id);
    id
}

pub fn face(id: u16) -> Option<Arc<Face>> {
    faces().lock().unwrap_or_else(|e| e.into_inner()).list.get(id as usize).cloned()
}

struct SendFactory(IDWriteFactory2);
unsafe impl Send for SendFactory {}
unsafe impl Sync for SendFactory {}

pub fn factory() -> IDWriteFactory2 {
    static F: OnceLock<SendFactory> = OnceLock::new();
    F.get_or_init(|| SendFactory(unsafe { DWriteCreateFactory::<IDWriteFactory2>(DWRITE_FACTORY_TYPE_SHARED).expect("directwrite") })).0.clone()
}

/// The typewriter and the serif the chrome is set in. Both are Computer Modern
/// Unicode, the TeX fonts, under the SIL Open Font License (assets/fonts/OFL.txt).
pub const TYPEWRITER: &str = "CMU Typewriter Text";
pub const SERIF: &str = "CMU Serif";

/// They ship inside the exe as woff2, 92 KB for all three. DirectWrite unpacks each
/// one into memory once, which comes to a few hundred KB.
const OWN_FONTS: [&[u8]; 3] = [
    include_bytes!("../../assets/fonts/cmuntt.woff2"),
    include_bytes!("../../assets/fonts/cmuntb.woff2"),
    include_bytes!("../../assets/fonts/cmunti.woff2"),
];

struct SendCollection(IDWriteFontCollection);
unsafe impl Send for SendCollection {}
unsafe impl Sync for SendCollection {}

/// The fonts kamaitachi brings, as a collection of their own. None on a Windows too
/// old to load fonts from memory (before 10 1703); the chrome then falls back.
fn own_fonts() -> Option<IDWriteFontCollection> {
    static C: OnceLock<Option<SendCollection>> = OnceLock::new();
    C.get_or_init(|| unsafe { load_own_fonts() }.map(SendCollection)).as_ref().map(|c| c.0.clone())
}

unsafe fn load_own_fonts() -> Option<IDWriteFontCollection> {
    unsafe {
        let f: IDWriteFactory5 = factory().cast().ok()?;
        let loader = f.CreateInMemoryFontFileLoader().ok()?;
        f.RegisterFontFileLoader(&loader).ok()?;
        let set = f.CreateFontSetBuilder().ok()?;
        for data in OWN_FONTS {
            let stream = f.UnpackFontFile(DWRITE_CONTAINER_TYPE_WOFF2, data.as_ptr().cast(), data.len() as u32).ok()?;
            let size = stream.GetFileSize().ok()?;
            let mut bytes = std::ptr::null_mut();
            let mut ctx = std::ptr::null_mut();
            stream.ReadFileFragment(&mut bytes, 0, size, &mut ctx).ok()?;
            // with no owner object the loader keeps its own copy, so the stream can go
            let file = loader.CreateInMemoryFontFileReference(&f, bytes, size as u32, None);
            stream.ReleaseFileFragment(ctx);
            set.AddFontFile(&file.ok()?).ok()?;
        }
        f.CreateFontCollectionFromFontSet(&set.CreateFontSet().ok()?).ok()?.cast().ok()
    }
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(Some(0)).collect()
}

/// What the recorder needs while DirectWrite calls it back.
struct Rec {
    out: *mut DisplayList,
    para: *const Para,
    objs: Vec<usize>,
    obj_pos: Vec<(f32, f32)>,
    x: f32,
    y: f32,
}

#[implement(IDWriteTextRenderer)]
struct Recorder;

impl IDWritePixelSnapping_Impl for Recorder_Impl {
    fn IsPixelSnappingDisabled(&self, _: *const c_void) -> Result<BOOL> {
        Ok(TRUE)
    }
    fn GetCurrentTransform(&self, _: *const c_void, t: *mut DWRITE_MATRIX) -> Result<()> {
        unsafe { *t = DWRITE_MATRIX { m11: 1.0, m12: 0.0, m21: 0.0, m22: 1.0, dx: 0.0, dy: 0.0 } };
        Ok(())
    }
    fn GetPixelsPerDip(&self, _: *const c_void) -> Result<f32> {
        Ok(1.0)
    }
}

impl IDWriteTextRenderer_Impl for Recorder_Impl {
    fn DrawGlyphRun(&self, ctx: *const c_void, bx: f32, by: f32, _m: DWRITE_MEASURING_MODE, run: *const DWRITE_GLYPH_RUN, desc: *const DWRITE_GLYPH_RUN_DESCRIPTION, _e: Ref<windows::core::IUnknown>) -> Result<()> {
        unsafe {
            let st = &mut *(ctx as *mut Rec);
            let out = &mut *st.out;
            let para = &*st.para;
            let run = &*run;
            let desc = &*desc;
            let Some(ff) = run.fontFace.as_ref() else { return Ok(()) };
            let fid = face_id(ff);
            let n = run.glyphCount as usize;
            if n == 0 {
                return Ok(());
            }
            let ids = std::slice::from_raw_parts(run.glyphIndices, n);
            let adv = std::slice::from_raw_parts(run.glyphAdvances, n);
            let offs = if run.glyphOffsets.is_null() { None } else { Some(std::slice::from_raw_parts(run.glyphOffsets, n)) };
            let rtl = run.bidiLevel & 1 == 1;
            // which character each glyph came from, for its color
            let mut gchar = vec![desc.textPosition; n];
            if !desc.clusterMap.is_null() && desc.stringLength > 0 {
                let cm = std::slice::from_raw_parts(desc.clusterMap, desc.stringLength as usize);
                for (c, &g) in cm.iter().enumerate() {
                    let g = g as usize;
                    if g < n {
                        gchar[g] = desc.textPosition + c as u32;
                    }
                }
                for g in 1..n {
                    if gchar[g] < gchar[g - 1] && !rtl {
                        gchar[g] = gchar[g - 1];
                    }
                }
            }
            let color_at = |pos: u32| -> Rgba {
                para.runs.iter().find(|r| pos >= r.start && pos < r.start + r.len).map_or([0, 0, 0, 255], |r| r.color)
            };
            let mut x = bx;
            let mut cur: Option<(Rgba, u32)> = None;
            for i in 0..n {
                let a = adv[i];
                if rtl {
                    x -= a;
                }
                let (dx, dy) = offs.map_or((0.0, 0.0), |o| (o[i].advanceOffset, o[i].ascenderOffset));
                let gx = if rtl { x - dx } else { x + dx };
                let color = color_at(gchar[i]);
                let idx = out.glyphs.len() as u32;
                out.glyphs.push(crate::web::Glyph { x: gx, y: by - dy, id: ids[i], face: fid, size: run.fontEmSize });
                match cur {
                    Some((c, _)) if c == color => {}
                    _ => {
                        if let Some((c, s)) = cur {
                            out.items.push(Item::Text { start: s, len: idx - s, color: c, clip: 0 });
                        }
                        cur = Some((color, idx));
                    }
                }
                if !rtl {
                    x += a;
                }
            }
            if let Some((c, s)) = cur {
                out.items.push(Item::Text { start: s, len: out.glyphs.len() as u32 - s, color: c, clip: 0 });
            }
            let _ = st.x;
            let _ = st.y;
        }
        Ok(())
    }
    fn DrawUnderline(&self, _: *const c_void, _: f32, _: f32, _: *const DWRITE_UNDERLINE, _: Ref<windows::core::IUnknown>) -> Result<()> {
        Ok(())
    }
    fn DrawStrikethrough(&self, _: *const c_void, _: f32, _: f32, _: *const DWRITE_STRIKETHROUGH, _: Ref<windows::core::IUnknown>) -> Result<()> {
        Ok(())
    }
    fn DrawInlineObject(&self, ctx: *const c_void, ox: f32, oy: f32, obj: Ref<IDWriteInlineObject>, _: BOOL, _: BOOL, _: Ref<windows::core::IUnknown>) -> Result<()> {
        unsafe {
            let st = &mut *(ctx as *mut Rec);
            if let Some(o) = obj.as_ref() {
                let key = o.as_raw() as usize;
                if let Some(k) = st.objs.iter().position(|p| *p == key) {
                    st.obj_pos[k] = (ox - st.x, oy - st.y);
                }
            }
        }
        Ok(())
    }
}

#[implement(IDWriteInlineObject)]
struct InlineBox {
    w: f32,
    h: f32,
}

impl IDWriteInlineObject_Impl for InlineBox_Impl {
    fn Draw(&self, _: *const c_void, _: Ref<IDWriteTextRenderer>, _: f32, _: f32, _: BOOL, _: BOOL, _: Ref<windows::core::IUnknown>) -> Result<()> {
        Ok(())
    }
    fn GetMetrics(&self) -> Result<DWRITE_INLINE_OBJECT_METRICS> {
        // sits on the baseline, like an image
        Ok(DWRITE_INLINE_OBJECT_METRICS { width: self.w, height: self.h, baseline: self.h, supportsSideways: FALSE })
    }
    fn GetOverhangMetrics(&self) -> Result<DWRITE_OVERHANG_METRICS> {
        Ok(DWRITE_OVERHANG_METRICS::default())
    }
    fn GetBreakConditions(&self, before: *mut DWRITE_BREAK_CONDITION, after: *mut DWRITE_BREAK_CONDITION) -> Result<()> {
        unsafe {
            *before = DWRITE_BREAK_CONDITION_NEUTRAL;
            *after = DWRITE_BREAK_CONDITION_NEUTRAL;
        }
        Ok(())
    }
}

#[derive(Hash, PartialEq, Eq, Clone)]
struct FKey {
    family: Arc<str>,
    size: u32,
    weight: u16,
    italic: bool,
}

/// DirectWrite as the engine's text engine. One per thread; they share the factory.
pub struct Dw {
    f: IDWriteFactory2,
    sys: IDWriteFontCollection,
    /// the fonts that came with kamaitachi, if this Windows could load them
    own: Option<IDWriteFontCollection>,
    renderer: IDWriteTextRenderer,
    families: HashMap<Arc<str>, Arc<[u16]>>,
    formats: HashMap<FKey, IDWriteTextFormat>,
    /// (ascent, descent) per em for a resolved family
    metrics: HashMap<(Arc<[u16]>, u16, bool), (f32, f32)>,
}

impl Default for Dw {
    fn default() -> Self {
        Self::new()
    }
}

impl Dw {
    pub fn new() -> Dw {
        let f = factory();
        let mut sys = None;
        unsafe { f.GetSystemFontCollection(&mut sys, false).expect("system fonts") };
        let renderer: IDWriteTextRenderer = Recorder.into();
        Dw { f, sys: sys.expect("system fonts"), own: own_fonts(), renderer, families: HashMap::new(), formats: HashMap::new(), metrics: HashMap::new() }
    }

    fn installed(&self, name: &str) -> Option<u32> {
        let w = wide(name);
        let mut idx = 0u32;
        let mut ok = FALSE;
        unsafe { self.sys.FindFamilyName(PCWSTR(w.as_ptr()), &mut idx, &mut ok).ok()? };
        ok.as_bool().then_some(idx)
    }

    /// The collection a resolved family lives in: ours if we brought it, else the system's.
    fn collection(&self, fam: &[u16]) -> &IDWriteFontCollection {
        if let Some(own) = &self.own {
            let mut idx = 0u32;
            let mut ok = FALSE;
            if unsafe { own.FindFamilyName(PCWSTR(fam.as_ptr()), &mut idx, &mut ok) }.is_ok() && ok.as_bool() {
                return own;
            }
        }
        &self.sys
    }

    /// The first family in a css font-family list that this machine has.
    fn family(&mut self, list: &Arc<str>) -> Arc<[u16]> {
        if let Some(f) = self.families.get(list) {
            return f.clone();
        }
        let mut pick: Option<String> = None;
        for raw in list.split(',') {
            let name = raw.trim().trim_matches(|c| c == '"' || c == '\'').trim();
            let lname = name.to_ascii_lowercase();
            let cands: &[&str] = match lname.as_str() {
                "serif" | "ui-serif" => &["Georgia", "Times New Roman"],
                "sans-serif" | "system-ui" | "-apple-system" | "blinkmacsystemfont" | "ui-sans-serif" | "helvetica neue" | "helvetica" | "inter" | "roboto" | "sf pro text" | "sf pro display" => {
                    &["Segoe UI", "Arial"]
                }
                "monospace" | "ui-monospace" | "sfmono-regular" | "sf mono" | "menlo" | "monaco" | "courier" => &["Cascadia Mono", "Consolas", "Courier New"],
                "cursive" => &["Comic Sans MS"],
                "fantasy" => &["Impact"],
                _ => &[],
            };
            if cands.is_empty() {
                if (name == TYPEWRITER || name == SERIF) && self.own.is_some() {
                    pick = Some(name.to_string());
                    break;
                }
                if !name.is_empty() && self.installed(name).is_some() {
                    pick = Some(name.to_string());
                    break;
                }
            } else if let Some(c) = cands.iter().find(|c| self.installed(c).is_some()) {
                pick = Some(c.to_string());
                break;
            }
        }
        let name = pick.unwrap_or_else(|| "Georgia".to_string());
        let w: Arc<[u16]> = Arc::from(wide(&name));
        self.families.insert(list.clone(), w.clone());
        w
    }

    fn format(&mut self, font: &Font) -> Option<IDWriteTextFormat> {
        let key = FKey { family: font.family.clone(), size: (font.size * 64.0) as u32, weight: font.weight, italic: font.italic };
        if let Some(f) = self.formats.get(&key) {
            return Some(f.clone());
        }
        let fam = self.family(&font.family);
        let style = if font.italic { DWRITE_FONT_STYLE_ITALIC } else { DWRITE_FONT_STYLE_NORMAL };
        let fmt = unsafe {
            self.f.CreateTextFormat(PCWSTR(fam.as_ptr()), self.collection(&fam), DWRITE_FONT_WEIGHT(font.weight as i32), style, DWRITE_FONT_STRETCH_NORMAL, font.size.max(0.5), windows::core::w!("en-us")).ok()?
        };
        self.formats.insert(key, fmt.clone());
        Some(fmt)
    }

    /// Ascent and descent of a font, per em.
    fn ascent(&mut self, font: &Font) -> (f32, f32) {
        let fam = self.family(&font.family);
        let key = (fam.clone(), font.weight, font.italic);
        if let Some(&m) = self.metrics.get(&key) {
            return m;
        }
        let m = (|| -> Option<(f32, f32)> {
            let mut idx = 0u32;
            let mut ok = FALSE;
            let coll = self.collection(&fam);
            unsafe {
                coll.FindFamilyName(PCWSTR(fam.as_ptr()), &mut idx, &mut ok).ok()?;
                if !ok.as_bool() {
                    return None;
                }
                let family = coll.GetFontFamily(idx).ok()?;
                let style = if font.italic { DWRITE_FONT_STYLE_ITALIC } else { DWRITE_FONT_STYLE_NORMAL };
                let f = family.GetFirstMatchingFont(DWRITE_FONT_WEIGHT(font.weight as i32), DWRITE_FONT_STRETCH_NORMAL, style).ok()?;
                let mut fm = DWRITE_FONT_METRICS::default();
                f.GetMetrics(&mut fm);
                let u = fm.designUnitsPerEm.max(1) as f32;
                Some((fm.ascent as f32 / u, fm.descent as f32 / u))
            }
        })()
        .unwrap_or((0.9, 0.22));
        self.metrics.insert(key, m);
        m
    }

    /// A text layout for a paragraph, with its inline objects.
    fn build(&mut self, p: &Para, width: f32) -> Option<(IDWriteTextLayout, Vec<IDWriteInlineObject>)> {
        let base = p.runs.first().map(|r| r.font.clone()).unwrap_or(Font { family: "serif".into(), size: 16.0, weight: 400, italic: false });
        let fmt = self.format(&base)?;
        let layout = unsafe { self.f.CreateTextLayout(&p.text, &fmt, width.max(0.0), 1.0e7).ok()? };
        unsafe {
            let _ = layout.SetWordWrapping(if p.nowrap { DWRITE_WORD_WRAPPING_NO_WRAP } else { DWRITE_WORD_WRAPPING_EMERGENCY_BREAK });
            let al = match p.align {
                2 | 5 => DWRITE_TEXT_ALIGNMENT_TRAILING,
                3 => DWRITE_TEXT_ALIGNMENT_CENTER,
                4 => DWRITE_TEXT_ALIGNMENT_JUSTIFIED,
                _ => DWRITE_TEXT_ALIGNMENT_LEADING,
            };
            let _ = layout.SetTextAlignment(al);
            for r in &p.runs {
                if r.len == 0 {
                    continue;
                }
                let range = DWRITE_TEXT_RANGE { startPosition: r.start, length: r.len };
                if r.font.family != base.family {
                    let fam = self.family(&r.font.family);
                    let _ = layout.SetFontFamilyName(PCWSTR(fam.as_ptr()), range);
                    let _ = layout.SetFontCollection(self.collection(&fam), range);
                }
                if r.font.size != base.size {
                    let _ = layout.SetFontSize(r.font.size.max(0.5), range);
                }
                if r.font.weight != base.weight {
                    let _ = layout.SetFontWeight(DWRITE_FONT_WEIGHT(r.font.weight as i32), range);
                }
                if r.font.italic != base.italic {
                    let _ = layout.SetFontStyle(if r.font.italic { DWRITE_FONT_STYLE_ITALIC } else { DWRITE_FONT_STYLE_NORMAL }, range);
                }
            }
            let mut objs = Vec::with_capacity(p.objs.len());
            for o in &p.objs {
                let obj: IDWriteInlineObject = InlineBox { w: o.w, h: o.h }.into();
                let _ = layout.SetInlineObject(&obj, DWRITE_TEXT_RANGE { startPosition: o.pos, length: 1 });
                objs.push(obj);
            }
            let tall = p.objs.iter().any(|o| o.h > p.line_h);
            if p.line_h > 0.0 && !tall {
                let (a, d) = self.ascent(&base);
                let content = (a + d) * base.size;
                let baseline = (p.line_h - content) / 2.0 + a * base.size;
                let _ = layout.SetLineSpacing(DWRITE_LINE_SPACING_METHOD_UNIFORM, p.line_h, baseline);
            }
            Some((layout, objs))
        }
    }
}

fn hit_rects(layout: &IDWriteTextLayout, start: u32, len: u32, x: f32, y: f32) -> Vec<DWRITE_HIT_TEST_METRICS> {
    let mut buf = vec![DWRITE_HIT_TEST_METRICS::default(); 8];
    let mut n = 0u32;
    unsafe {
        match layout.HitTestTextRange(start, len, x, y, Some(&mut buf), &mut n) {
            Ok(()) => {}
            Err(e) if e.code() == E_NOT_SUFFICIENT_BUFFER => {
                buf = vec![DWRITE_HIT_TEST_METRICS::default(); n as usize];
                if layout.HitTestTextRange(start, len, x, y, Some(&mut buf), &mut n).is_err() {
                    return Vec::new();
                }
            }
            Err(_) => return Vec::new(),
        }
    }
    buf.truncate(n as usize);
    buf
}

impl TextEngine for Dw {
    fn layout(&mut self, p: &Para, width: f32, x: f32, y: f32, out: Option<&mut DisplayList>) -> ParaOut {
        let empty = ParaOut { height: 0.0, width: 0.0, objs: vec![(0.0, 0.0); p.objs.len()] };
        if p.text.is_empty() {
            return empty;
        }
        let Some((layout, objs)) = self.build(p, width) else { return empty };
        let mut tm = DWRITE_TEXT_METRICS::default();
        unsafe {
            let _ = layout.GetMetrics(&mut tm);
        }
        let mut po = ParaOut { height: tm.height, width: tm.width, objs: vec![(0.0, 0.0); p.objs.len()] };
        let Some(out) = out else { return po };
        // line baselines, for underlines
        let mut lines = vec![DWRITE_LINE_METRICS::default(); tm.lineCount.max(1) as usize];
        let mut nl = 0u32;
        unsafe {
            let _ = layout.GetLineMetrics(Some(&mut lines), &mut nl);
        }
        lines.truncate(nl as usize);
        let baseline_at = |top: f32| -> f32 {
            let mut ly = y;
            for l in &lines {
                if top < ly + l.height - 0.5 {
                    return ly + l.baseline;
                }
                ly += l.height;
            }
            lines.last().map_or(top, |l| ly - l.height + l.baseline)
        };
        // inline backgrounds go under the glyphs
        for &(s, l, c) in &p.bgs {
            for r in hit_rects(&layout, s, l, x, y) {
                out.items.push(Item::Rect { r: Rect::new(r.left - 1.0, r.top, r.width + 2.0, r.height), color: c, radius: 3.0, clip: 0 });
            }
        }
        let mut rec = Rec { out: out as *mut DisplayList, para: p as *const Para, objs: objs.iter().map(|o| o.as_raw() as usize).collect(), obj_pos: vec![(0.0, 0.0); objs.len()], x, y };
        unsafe {
            let _ = layout.Draw(Some(&mut rec as *mut Rec as *const c_void), &self.renderer, x, y);
        }
        po.objs = rec.obj_pos;
        // underlines and strike-throughs, then link areas
        for r in &p.runs {
            if r.deco != 0 && r.len > 0 && r.color[3] > 0 {
                let th = (r.font.size / 14.0).max(1.0);
                for h in hit_rects(&layout, r.start, r.len, x, y) {
                    if !h.isText.as_bool() {
                        continue;
                    }
                    let bl = baseline_at(h.top);
                    if r.deco & 1 != 0 {
                        out.items.push(Item::Rect { r: Rect::new(h.left, bl + r.font.size * 0.11, h.width, th), color: r.color, radius: 0.0, clip: 0 });
                    }
                    if r.deco & 2 != 0 {
                        out.items.push(Item::Rect { r: Rect::new(h.left, bl - r.font.size * 0.3, h.width, th), color: r.color, radius: 0.0, clip: 0 });
                    }
                }
            }
        }
        let mut k = 0;
        while k < p.runs.len() {
            let link = p.runs[k].link;
            if link == u32::MAX {
                k += 1;
                continue;
            }
            let start = p.runs[k].start;
            let mut end = start + p.runs[k].len;
            while k + 1 < p.runs.len() && p.runs[k + 1].link == link && p.runs[k + 1].start <= end + 1 {
                k += 1;
                end = p.runs[k].start + p.runs[k].len;
            }
            for h in hit_rects(&layout, start, end - start, x, y) {
                out.links.push(Link { r: Rect::new(h.left, h.top, h.width, h.height), href: link });
            }
            k += 1;
        }
        po
    }

    fn widths(&mut self, p: &Para) -> (f32, f32) {
        if p.text.is_empty() {
            return (0.0, 0.0);
        }
        let Some((layout, _objs)) = self.build(p, 1.0e6) else { return (0.0, 0.0) };
        let mut tm = DWRITE_TEXT_METRICS::default();
        let min = unsafe {
            let _ = layout.GetMetrics(&mut tm);
            layout.DetermineMinWidth().unwrap_or(tm.width)
        };
        (min.min(tm.width).ceil(), tm.width.ceil())
    }
}
