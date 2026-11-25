//! kamaitachi's native half on Android. The Java side owns the window, the address
//! bar, the keyboard and the System WebView; it calls in here to load a page with the
//! same engine the Windows browser uses, and to draw it into a Bitmap it hands over.
//!
//! A page lives on the Rust heap and Java holds it as an opaque number. Pages render
//! straight into the Bitmap's memory, so a page is never held twice.
//!
//! # Safety
//!
//! Every `Java_*` function is called only by the JVM, with a valid `JNIEnv` and Java
//! references, and with page numbers that `load` returned and `free` has not yet
//! taken back. Native.java is the only caller and keeps to that.

// the contract above covers every exported function, rather than ten copies of it
#![allow(clippy::missing_safety_doc)]

mod flow;
mod fonts;

use hayate::frame::{text_lut, Clip, Frame, Mask};
use hayate::url::Url;
use hayate::web::layout::{self, Images};
use hayate::web::style::Cascade;
use hayate::web::{block, load, DisplayList, Item, Rgba};
use jni_sys::{jboolean, jclass, jfloat, jint, jlong, jstring, JNIEnv};
#[cfg(target_os = "android")]
use jni_sys::jobject;
use std::collections::HashMap;
use std::ffi::{CStr, CString};
#[cfg(target_os = "android")]
use std::ffi::c_void;
use std::sync::{Mutex, OnceLock};
use swash::scale::{Render, ScaleContext, Source};
use swash::zeno::{Format, Vector};

/// A loaded page, laid out for one width.
struct Page {
    dl: DisplayList,
    url: String,
    needs_js: bool,
    dpr: f32,
}

/// Glyph masks, kept between draws. Capped like the Windows cache.
struct Glyphs {
    ctx: ScaleContext,
    cache: HashMap<(u16, u32, u16, u8), Mask>,
    bytes: usize,
    lut: [u8; 256],
}

fn glyphs() -> &'static Mutex<Glyphs> {
    static G: OnceLock<Mutex<Glyphs>> = OnceLock::new();
    G.get_or_init(|| Mutex::new(Glyphs { ctx: ScaleContext::new(), cache: HashMap::new(), bytes: 0, lut: text_lut() }))
}

impl Glyphs {
    fn mask(&mut self, face: u16, size: f32, id: u16, sub: u8) -> &Mask {
        let key = (face, (size * 16.0).round() as u32, id, sub);
        if !self.cache.contains_key(&key) {
            if self.bytes > 8 << 20 {
                self.cache.clear();
                self.bytes = 0;
            }
            let m = fonts::get(face)
                .and_then(|f| {
                    let settings = f.settings();
                    let mut scaler = self.ctx.builder(f.font()).size(size).hint(false).variations(settings.iter().copied()).build();
                    let img = Render::new(&[Source::Outline]).format(Format::Alpha).offset(Vector::new(sub as f32 / 4.0, 0.0)).render(&mut scaler, id)?;
                    let p = img.placement;
                    (p.width > 0 && p.height > 0 && p.width < 600 && p.height < 600)
                        .then(|| Mask { w: p.width as u16, h: p.height as u16, left: p.left as i16, top: -p.top as i16, a: img.data.into_boxed_slice() })
                })
                .unwrap_or_else(Mask::empty);
            self.bytes += m.a.len() + 32;
            self.cache.insert(key, m);
        }
        &self.cache[&key]
    }
}

/// A plain page for when a load fails or a site is on a malware list.
fn notice(title: &str, body: &str) -> String {
    format!("<!doctype html><title>{title}</title><body style=\"font-family:serif;background:#f6f2e7;color:#23211d;padding:24px\"><h2>{title}</h2><p>{body}</p></body>")
}

fn open(url: &str, width_px: i32, dpr: f32) -> Page {
    let (vw, vh) = (width_px as f32 / dpr, 800.0);
    let (doc, needs) = match Url::parse(url) {
        Some(u) if block::is_danger(url) => (load::from_html(&notice("Stopped", "This site is on a malware or phishing list, so kamaitachi did not open it."), u, 1), false),
        Some(u) => match load::load(&load::Request::get(u.clone())) {
            Ok(doc) => (doc, true),
            Err(e) => (load::from_html(&notice("Could not open this page", &e), u, 1), false),
        },
        None => (load::from_html(&notice("Not an address", url), Url { host: "kamaitachi".into(), path: "/".into() }, 1), false),
    };
    let styles = Cascade { dom: &doc.dom, sheets: &doc.sheets, vw, vh }.run();
    let mut text = flow::Flow::new();
    let mut images = Images::default();
    let dl = layout::layout(&doc.dom, &styles, &mut text, &mut images, &doc.base, vw, vh);
    let needs_js = needs && (load::needs_js(&doc, &dl) || load::bot_check(&doc, &dl));
    Page { url: doc.url.full(), dl, needs_js, dpr }
}

/// A Bitmap is RGBA in memory where a frame expects BGRA, so red and blue trade places.
fn swap(c: Rgba) -> Rgba {
    [c[2], c[1], c[0], c[3]]
}

fn draw(page: &Page, buf: &mut [u32], w: u32, h: u32, scroll: f32) {
    let mut f = Frame::over(buf, w, h);
    let dl = &page.dl;
    let k = page.dpr;
    f.fill(swap(dl.bg));
    let oy = -scroll;
    let view: Clip = [0.0, 0.0, w as f32, h as f32];
    let (y0, y1) = (scroll / k - 50.0, (scroll + h as f32) / k + 50.0);
    let clipf = |ci: u16| -> Clip {
        if ci == 0 {
            return view;
        }
        let r = dl.clips[ci as usize];
        [(r.x * k).max(0.0), (r.y * k + oy).max(0.0), ((r.x + r.w) * k).min(w as f32), ((r.y + r.h) * k + oy).min(h as f32)]
    };
    let mut g = glyphs().lock().unwrap_or_else(|e| e.into_inner());
    for it in &dl.items {
        match *it {
            Item::Rect { r, color, radius, clip } if r.y + r.h >= y0 && r.y <= y1 => f.rect(r.x * k, r.y * k + oy, r.w * k, r.h * k, swap(color), radius * k, clipf(clip)),
            Item::Ring { r, width, color, radius, clip } if r.y + r.h >= y0 && r.y <= y1 => f.ring(r.x * k, r.y * k + oy, r.w * k, r.h * k, width * k, swap(color), radius * k, clipf(clip)),
            // images are not decoded yet; a quiet box shows where they go
            Item::Image { r, radius, clip, .. } if r.y + r.h >= y0 && r.y <= y1 => f.rect(r.x * k, r.y * k + oy, r.w * k, r.h * k, [0xe4, 0xde, 0xd2, 255], radius * k, clipf(clip)),
            Item::Text { start, len, color, clip } if len > 0 => {
                let gs = &dl.glyphs[start as usize..(start + len) as usize];
                if gs[gs.len() - 1].y < y0 || gs[0].y > y1 + 40.0 {
                    continue;
                }
                let c = swap(color);
                let cf = [c[0] as f32 / 255.0, c[1] as f32 / 255.0, c[2] as f32 / 255.0, c[3] as f32 / 255.0];
                let cl = clipf(clip);
                for gl in gs {
                    let x = gl.x * k;
                    let xf = x.floor();
                    let sub = ((x - xf) * 4.0) as u8 & 3;
                    let lut = g.lut;
                    let m = g.mask(gl.face, gl.size * k, gl.id, sub);
                    f.mask(m, xf as i64, (gl.y * k + oy).round() as i64, cf, cl, &lut);
                }
            }
            _ => {}
        }
    }
}

// ---- JNI, called from dev.cintu.kamaitachi.Native

unsafe fn string(env: *mut JNIEnv, s: jstring) -> String {
    unsafe {
        let f = (**env).v1_1.GetStringUTFChars;
        let p = f(env, s, std::ptr::null_mut());
        if p.is_null() {
            return String::new();
        }
        let out = CStr::from_ptr(p).to_string_lossy().into_owned();
        ((**env).v1_1.ReleaseStringUTFChars)(env, s, p);
        out
    }
}

unsafe fn jstr(env: *mut JNIEnv, s: &str) -> jstring {
    let c = CString::new(s.replace('\0', "")).unwrap_or_default();
    unsafe { ((**env).v1_1.NewStringUTF)(env, c.as_ptr()) }
}

fn page<'a>(p: jlong) -> Option<&'a Page> {
    (p != 0).then(|| unsafe { &*(p as *const Page) })
}

/// Starts the ad blocker. It keeps its filter lists under `dir`, the app's files folder.
#[unsafe(no_mangle)]
pub unsafe extern "system" fn Java_dev_cintu_kamaitachi_Native_start(env: *mut JNIEnv, _: jclass, dir: jstring) {
    let dir = unsafe { string(env, dir) };
    // the activity can be created more than once in one process
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        block::set_dir(std::path::Path::new(&dir).join("filters"));
        block::start();
    });
}

/// Loads and lays out a page. Blocks; Java calls it off the main thread.
#[unsafe(no_mangle)]
pub unsafe extern "system" fn Java_dev_cintu_kamaitachi_Native_load(env: *mut JNIEnv, _: jclass, url: jstring, width: jint, density: jfloat) -> jlong {
    let url = unsafe { string(env, url) };
    Box::into_raw(Box::new(open(&url, width, density.max(1.0)))) as jlong
}

#[unsafe(no_mangle)]
pub unsafe extern "system" fn Java_dev_cintu_kamaitachi_Native_free(_: *mut JNIEnv, _: jclass, p: jlong) {
    if p != 0 {
        drop(unsafe { Box::from_raw(p as *mut Page) });
    }
}

/// The page's height in device px.
#[unsafe(no_mangle)]
pub unsafe extern "system" fn Java_dev_cintu_kamaitachi_Native_height(_: *mut JNIEnv, _: jclass, p: jlong) -> jint {
    page(p).map_or(0, |p| (p.dl.height * p.dpr).ceil() as jint)
}

#[unsafe(no_mangle)]
pub unsafe extern "system" fn Java_dev_cintu_kamaitachi_Native_title(env: *mut JNIEnv, _: jclass, p: jlong) -> jstring {
    unsafe { jstr(env, page(p).map_or("", |p| p.dl.title.as_str())) }
}

/// Where the page ended up, after redirects.
#[unsafe(no_mangle)]
pub unsafe extern "system" fn Java_dev_cintu_kamaitachi_Native_url(env: *mut JNIEnv, _: jclass, p: jlong) -> jstring {
    unsafe { jstr(env, page(p).map_or("", |p| p.url.as_str())) }
}

/// Whether this page is an app that needs the WebView.
#[unsafe(no_mangle)]
pub unsafe extern "system" fn Java_dev_cintu_kamaitachi_Native_needsJs(_: *mut JNIEnv, _: jclass, p: jlong) -> jboolean {
    page(p).is_some_and(|p| p.needs_js) as jboolean
}

/// The link under a point (device px, in page coordinates), or an empty string.
#[unsafe(no_mangle)]
pub unsafe extern "system" fn Java_dev_cintu_kamaitachi_Native_linkAt(env: *mut JNIEnv, _: jclass, p: jlong, x: jfloat, y: jfloat) -> jstring {
    let href = page(p).and_then(|p| p.dl.link_at(x / p.dpr, y / p.dpr).map(|h| h.to_string())).unwrap_or_default();
    unsafe { jstr(env, &href) }
}

/// What the app holds right now, in MB, for the meter in the toolbar.
#[unsafe(no_mangle)]
pub unsafe extern "system" fn Java_dev_cintu_kamaitachi_Native_memoryMb(_: *mut JNIEnv, _: jclass) -> jfloat {
    // resident pages of the whole process, from the kernel
    std::fs::read_to_string("/proc/self/statm").ok().and_then(|s| s.split_whitespace().nth(1).and_then(|v| v.parse::<f32>().ok())).map_or(0.0, |pages| pages * 4096.0 / 1048576.0)
}

/// Renders a page the way the app would, into a w by h frame scrolled to the top.
/// For checking the engine on a computer (examples/shot.rs); the app never calls it.
pub fn shot(url: &str, w: u32, h: u32, dpr: f32) -> Vec<u32> {
    let page = open(url, w as i32, dpr);
    let mut px = vec![0u32; (w * h) as usize];
    draw(&page, &mut px, w, h, 0.0);
    px
}

#[cfg(target_os = "android")]
#[repr(C)]
struct BitmapInfo {
    width: u32,
    height: u32,
    stride: u32,
    format: i32,
    flags: u32,
}

#[cfg(target_os = "android")]
#[link(name = "jnigraphics")]
unsafe extern "C" {
    fn AndroidBitmap_getInfo(env: *mut JNIEnv, bitmap: jobject, info: *mut BitmapInfo) -> i32;
    fn AndroidBitmap_lockPixels(env: *mut JNIEnv, bitmap: jobject, addr: *mut *mut c_void) -> i32;
    fn AndroidBitmap_unlockPixels(env: *mut JNIEnv, bitmap: jobject) -> i32;
}

/// Draws the page into an ARGB_8888 Bitmap, scrolled down by `scroll` device px.
#[cfg(target_os = "android")]
#[unsafe(no_mangle)]
pub unsafe extern "system" fn Java_dev_cintu_kamaitachi_Native_draw(env: *mut JNIEnv, _: jclass, p: jlong, bitmap: jobject, scroll: jfloat) {
    let Some(page) = page(p) else { return };
    unsafe {
        let mut info = BitmapInfo { width: 0, height: 0, stride: 0, format: 0, flags: 0 };
        // format 1 is RGBA_8888
        if AndroidBitmap_getInfo(env, bitmap, &mut info) != 0 || info.format != 1 {
            return;
        }
        let mut addr = std::ptr::null_mut();
        if AndroidBitmap_lockPixels(env, bitmap, &mut addr) != 0 || addr.is_null() {
            return;
        }
        let w = info.stride / 4;
        let px = std::slice::from_raw_parts_mut(addr as *mut u32, (w * info.height) as usize);
        draw(page, px, w, info.height, scroll);
        AndroidBitmap_unlockPixels(env, bitmap);
    }
}
