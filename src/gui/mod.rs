//! The window browser: tabs, an address bar, and pages drawn by hayate's own engine on
//! the gpu. Pages load on worker threads (fetch, cascade, layout, images) and arrive as
//! display lists; the window thread only draws, scrolls and handles input, and sleeps
//! when nothing moves.

pub mod full;
pub mod gpu;
pub mod image;
pub mod paint;
pub mod soft;
pub mod text;

use crate::url::Url;
use crate::web::layout::{self, Images};
use crate::web::load;
use crate::web::style::{media_key, Cascade, Styles};
use crate::web::dom::t as tg;
use crate::web::{DisplayList, Font, Item, Para, Rect, Rgba, Run, TextEngine};
use webview2_com::Microsoft::Web::WebView2::Win32::{ICoreWebView2, ICoreWebView2Controller, ICoreWebView2Environment};
use gpu::Clip;
use paint::Painter;
use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;
use text::Dw;
use windows::core::{w, BOOL, PCWSTR};
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Dwm::*;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::System::DataExchange::*;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Com::{CoInitializeEx, COINIT_APARTMENTTHREADED};
use windows::Win32::System::Memory::*;
use windows::Win32::UI::HiDpi::*;
use windows::Win32::UI::Input::KeyboardAndMouse::*;
use windows::Win32::UI::WindowsAndMessaging::*;

const TAB_H: f32 = 40.0;
const BAR_H: f32 = 46.0;
const TOP: f32 = TAB_H + BAR_H;
const START: &str = "hayate:start";

// late-90s hardware, quietly: ink, a warm panel, amber phosphor, one orange, one cyan
const BG: Rgba = [0x08, 0x08, 0x0b, 255];
const PANEL: Rgba = [0x11, 0x11, 0x15, 255];
const HOVER: Rgba = [0x22, 0x21, 0x26, 255];
const TEXT: Rgba = [0xec, 0xeb, 0xe6, 255];
const MUTED: Rgba = [0x8b, 0x8a, 0x85, 255];
const FAINT: Rgba = [0x4f, 0x4e, 0x4b, 255];
const ACCENT: Rgba = [0xff, 0x6b, 0x1a, 255];
const PHOS: Rgba = [0xff, 0xc8, 0x8f, 255];
const PHOS_DIM: Rgba = [0x8f, 0x72, 0x56, 255];
const CYAN: Rgba = [0x6f, 0xe3, 0xff, 255];
const MONO: &str = "Cascadia Mono, Consolas, monospace";
/// the browser's name: kamaitachi, the sickle-weasel that rides whirlwinds
const BRAND: &str = "kamaitachi";
const UI_FONT: &str = "Segoe UI Variable Text, Segoe UI, sans-serif";
const ICON_FONT: &str = "Segoe Fluent Icons, Segoe MDL2 Assets";

const START_HTML: &str = include_str!("start.html");

pub struct Page {
    doc: load::Doc,
    styles: Styles,
    images: Images,
}

enum Msg {
    Preview { tab: u64, seq: u64, dl: DisplayList, url: String },
    Done { tab: u64, seq: u64, page: Box<Page>, dl: DisplayList, pixels: Vec<(u32, u32, u32, Vec<u8>)>, blocked: u32 },
    Fail { tab: u64, seq: u64, err: String, url: String },
    /// the page is a javascript app: open it in the full engine
    NeedsJs { tab: u64, seq: u64, url: String },
    /// the page is on a malware or phishing list
    Danger { tab: u64, seq: u64, url: String },
}

struct FullView {
    ctl: ICoreWebView2Controller,
    wv: ICoreWebView2,
}

struct Shared {
    inbox: Mutex<Vec<Msg>>,
    hwnd: isize,
}

impl Shared {
    fn post(&self, m: Msg) {
        self.inbox.lock().unwrap_or_else(|e| e.into_inner()).push(m);
        unsafe {
            let _ = PostMessageW(Some(HWND(self.hwnd as *mut _)), WM_APP, WPARAM(0), LPARAM(0));
        }
    }
}

struct Tab {
    id: u64,
    url: String,
    title: String,
    page: Option<Box<Page>>,
    dl: Option<DisplayList>,
    scroll: f32,
    target: f32,
    back: Vec<(String, f32)>,
    fwd: Vec<(String, f32)>,
    loading: bool,
    /// a newer navigation makes older results stale
    seq: u64,
    tex: HashMap<u32, paint::Image>,
    restore: Option<f32>,
    vw: f32,
    /// this tab runs in the full engine
    full_mode: bool,
    full: Option<FullView>,
    /// where the full view goes once it exists
    pending: Option<String>,
    /// a view was asked for and has not arrived
    requested: bool,
    /// the view was closed to save memory; it comes back when the tab is shown
    parked: bool,
    /// the last picture of the full view (jpeg), and its decoded copy while on screen
    picture: Option<Vec<u8>>,
    picture_img: Option<paint::Image>,
    shot_at: Option<Instant>,
    /// when the tab was last on screen
    used: Instant,
    /// ads and trackers stopped on this page
    blocked: u32,
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum Hit {
    None,
    Tab(usize),
    Close(usize),
    NewTab,
    Back,
    Fwd,
    Reload,
    Addr,
    Engine,
}

#[derive(Default)]
struct Edit {
    text: Vec<char>,
    caret: usize,
    focused: bool,
    all: bool,
}

struct App {
    hwnd: HWND,
    gpu: Painter,
    dw: Dw,
    shared: Arc<Shared>,
    tabs: Vec<Tab>,
    cur: usize,
    next_id: u64,
    scale: f32,
    zoom: f32,
    w: u32,
    h: u32,
    mouse: (f32, f32),
    hover: Hit,
    hover_link: Option<String>,
    hits: Vec<(Rect, Hit)>,
    edit: Edit,
    relayout_due: bool,
    shot: Option<(String, u32)>,
    /// the window size asked for, device px
    want: (i32, i32),
    started: Instant,
    env: Option<ICoreWebView2Environment>,
    /// 0 not started, 1 starting, 2 ready, 3 failed
    env_state: u8,
    waiting: Vec<u64>,
    /// per-site engine choices: true for the full engine
    engines: HashMap<String, bool>,
    full_mb: f32,
    /// the new tab's wallpaper, decoded at the window's size while the new tab is shown
    wall: Option<(u32, u32, paint::Image)>,
    wall_checked: bool,
}

fn wall_dir() -> std::path::PathBuf {
    let base = std::env::var("APPDATA").unwrap_or_else(|_| ".".into());
    std::path::Path::new(&base).join("hayate")
}

const WALL_EXT: [&str; 6] = ["jpg", "jpeg", "png", "webp", "bmp", "gif"];

fn wall_file() -> Option<std::path::PathBuf> {
    WALL_EXT.iter().map(|e| wall_dir().join(format!("wallpaper.{e}"))).find(|p| p.exists())
}

fn engines_path() -> std::path::PathBuf {
    let base = std::env::var("APPDATA").unwrap_or_else(|_| ".".into());
    std::path::Path::new(&base).join("hayate").join("engines.txt")
}

fn load_engines() -> HashMap<String, bool> {
    std::fs::read_to_string(engines_path())
        .unwrap_or_default()
        .lines()
        .filter_map(|l| {
            let (h, e) = l.split_once(' ')?;
            Some((h.to_string(), e.trim() == "full"))
        })
        .collect()
}

fn host_of(url: &str) -> String {
    let r = url.split("://").nth(1).unwrap_or(url);
    r.split(['/', '?', '#']).next().unwrap_or("").to_ascii_lowercase()
}

static mut APP: *mut App = std::ptr::null_mut();

/// Opens the window browser on these addresses, one tab each (or the start page).
pub fn run(urls: Vec<String>) -> Result<(), String> {
    let url = urls.first().cloned();
    unsafe {
        // the full engine needs a single-threaded apartment on this thread
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        crate::web::block::start();
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        let inst = GetModuleHandleW(None).map_err(|e| e.to_string())?;
        let class = w!("hayate.window");
        let wc = WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            lpfnWndProc: Some(wndproc),
            hInstance: inst.into(),
            hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
            lpszClassName: class,
            hbrBackground: HBRUSH::default(),
            ..Default::default()
        };
        RegisterClassExW(&wc);
        let (ww, wh) = std::env::var("HAYATE_SIZE")
            .ok()
            .and_then(|s| s.split_once('x').and_then(|(a, b)| Some((a.parse::<i32>().ok()?, b.parse::<i32>().ok()?))))
            .unwrap_or_else(|| {
                // most of the screen, in css px, leaving room for the taskbar and frame
                let dpi = GetDpiForSystem() as f32 / 96.0;
                let sw = GetSystemMetrics(SM_CXSCREEN) as f32 / dpi;
                let sh = GetSystemMetrics(SM_CYSCREEN) as f32 / dpi;
                ((sw * 0.86).min(1440.0) as i32, (sh * 0.84).min(980.0) as i32)
            });
        // the gpu path presents with a flip swap chain and needs no gdi surface
        let ex = if paint::wanted() == "cpu" { WINDOW_EX_STYLE(0) } else { WS_EX_NOREDIRECTIONBITMAP };
        let hwnd = CreateWindowExW(ex, class, w!("kamaitachi"), WS_OVERLAPPEDWINDOW | WS_CLIPCHILDREN, CW_USEDEFAULT, CW_USEDEFAULT, 100, 100, None, None, Some(inst.into()), None).map_err(|e| e.to_string())?;
        let dark: BOOL = TRUE;
        let _ = DwmSetWindowAttribute(hwnd, DWMWA_USE_IMMERSIVE_DARK_MODE, &dark as *const BOOL as *const _, 4);
        let cap = COLORREF((BG[0] as u32) | (BG[1] as u32) << 8 | (BG[2] as u32) << 16);
        let _ = DwmSetWindowAttribute(hwnd, DWMWA_CAPTION_COLOR, &cap as *const COLORREF as *const _, 4);
        let memlog = std::env::var("HAYATE_MEMLOG").is_ok();
        if memlog {
            eprintln!("mem: window {:.1}", private_mb());
        }
        let scale = GetDpiForWindow(hwnd) as f32 / 96.0;
        let (px, py) = if std::env::var("HAYATE_SHOT").is_ok() { (-32000, -32000) } else { (60, 40) };
        let _ = SetWindowPos(hwnd, None, px, py, (ww as f32 * scale) as i32, (wh as f32 * scale) as i32, SWP_NOZORDER | SWP_NOACTIVATE);
        let gpu = Painter::new(hwnd).map_err(|e| format!("direct3d: {e}"))?;
        if memlog {
            eprintln!("mem: painter ({}) {:.1}", gpu.name(), private_mb());
        }
        let _dw_probe = Dw::new();
        if memlog {
            eprintln!("mem: directwrite {:.1}", private_mb());
        }
        let shared = Arc::new(Shared { inbox: Mutex::new(Vec::new()), hwnd: hwnd.0 as isize });
        let shot = std::env::var("HAYATE_SHOT").ok().map(|p| (p, 0));
        let app = Box::new(App {
            hwnd,
            gpu,
            dw: Dw::new(),
            shared,
            tabs: Vec::new(),
            cur: 0,
            next_id: 1,
            scale,
            zoom: 1.0,
            w: 0,
            h: 0,
            mouse: (0.0, 0.0),
            hover: Hit::None,
            hover_link: None,
            hits: Vec::new(),
            edit: Edit::default(),
            relayout_due: false,
            shot,
            want: ((ww as f32 * scale) as i32, (wh as f32 * scale) as i32),
            started: Instant::now(),
            env: None,
            env_state: 0,
            waiting: Vec::new(),
            engines: load_engines(),
            full_mb: 0.0,
            wall: None,
            wall_checked: false,
        });
        APP = Box::into_raw(app);
        let app = &mut *APP;
        app.sync_size();
        app.new_tab(url.as_deref().unwrap_or(START));
        for u in urls.iter().skip(1) {
            app.navigate(u, true);
        }
        if url.is_none() {
            app.focus_addr();
        }
        // test shots render into a window that is never shown, off every screen
        if app.shot.is_none() {
            let _ = ShowWindow(hwnd, SW_SHOW);
        }
        windows::Win32::UI::Shell::DragAcceptFiles(hwnd, true);

        let mut msg = MSG::default();
        loop {
            let app = &mut *APP;
            if app.animating() {
                while PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool() {
                    if msg.message == WM_QUIT {
                        return Ok(());
                    }
                    let _ = TranslateMessage(&msg);
                    DispatchMessageW(&msg);
                }
                (*APP).render();
            } else {
                if !GetMessageW(&mut msg, None, 0, 0).as_bool() {
                    return Ok(());
                }
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }
    }
}

fn lo(l: LPARAM) -> f32 {
    (l.0 & 0xFFFF) as i16 as f32
}

fn hi(l: LPARAM) -> f32 {
    ((l.0 >> 16) & 0xFFFF) as i16 as f32
}

fn key_down(vk: VIRTUAL_KEY) -> bool {
    unsafe { GetKeyState(vk.0 as i32) < 0 }
}

extern "system" fn wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    unsafe {
        if APP.is_null() {
            return DefWindowProcW(hwnd, msg, wp, lp);
        }
        let app = &mut *APP;
        match msg {
            WM_DESTROY => {
                PostQuitMessage(0);
                LRESULT(0)
            }
            WM_ERASEBKGND => LRESULT(1),
            WM_PAINT => {
                let mut ps = PAINTSTRUCT::default();
                BeginPaint(hwnd, &mut ps);
                let _ = EndPaint(hwnd, &ps);
                app.render();
                LRESULT(0)
            }
            WM_SIZE => {
                app.sync_size();
                app.update_views();

                app.relayout_due = true;
                SetTimer(Some(hwnd), 1, 120, None);
                app.render();
                LRESULT(0)
            }
            WM_TIMER => {
                if wp.0 == 2 {
                    app.measure_full();
                    return LRESULT(0);
                }
                let _ = KillTimer(Some(hwnd), wp.0);
                if wp.0 == 1 {
                    app.relayout();
                }
                app.render();
                LRESULT(0)
            }
            WM_DROPFILES => {
                let drop = windows::Win32::UI::Shell::HDROP(wp.0 as *mut _);
                let mut buf = [0u16; 1024];
                let n = windows::Win32::UI::Shell::DragQueryFileW(drop, 0, Some(&mut buf)) as usize;
                windows::Win32::UI::Shell::DragFinish(drop);
                let path = String::from_utf16_lossy(&buf[..n.min(buf.len())]);
                app.set_wallpaper(&path);
                LRESULT(0)
            }
            full::WM_FULL => {
                app.full_events();
                app.render();
                LRESULT(0)
            }
            WM_DPICHANGED => {
                if std::env::var("HAYATE_MEMLOG").is_ok() {
                    eprintln!("dpi changed: {}", wp.0 & 0xFFFF);
                }
                app.scale = ((wp.0 & 0xFFFF) as f32 / 96.0).max(0.5);
                let r = &*(lp.0 as *const RECT);
                let _ = SetWindowPos(hwnd, None, r.left, r.top, r.right - r.left, r.bottom - r.top, SWP_NOZORDER | SWP_NOACTIVATE);
                app.relayout_due = true;
                app.relayout();
                LRESULT(0)
            }
            WM_APP => {
                app.inbox();
                app.render();
                LRESULT(0)
            }
            WM_MOUSEMOVE => {
                app.mouse_move(lo(lp), hi(lp));
                LRESULT(0)
            }
            WM_SETCURSOR => {
                if (lp.0 & 0xFFFF) as u32 == HTCLIENT {
                    let c = if app.hover_link.is_some() || matches!(app.hover, Hit::Tab(_) | Hit::Close(_) | Hit::NewTab | Hit::Back | Hit::Fwd | Hit::Reload | Hit::Engine) {
                        IDC_HAND
                    } else if app.hover == Hit::Addr {
                        IDC_IBEAM
                    } else {
                        IDC_ARROW
                    };
                    SetCursor(LoadCursorW(None, c).ok());
                    return LRESULT(1);
                }
                DefWindowProcW(hwnd, msg, wp, lp)
            }
            WM_LBUTTONDOWN => {
                app.click(lo(lp), hi(lp), key_down(VK_CONTROL));
                LRESULT(0)
            }
            WM_MBUTTONDOWN => {
                app.mouse_move(lo(lp), hi(lp));
                if let Some(l) = app.hover_link.clone() {
                    app.open_background(&l);
                } else if let Hit::Tab(i) = app.hover {
                    app.close_tab(i);
                }
                app.render();
                LRESULT(0)
            }
            WM_XBUTTONDOWN => {
                if (wp.0 >> 16) & 0xFFFF == 1 {
                    app.go_back();
                } else {
                    app.go_fwd();
                }
                LRESULT(1)
            }
            WM_MOUSEWHEEL => {
                let d = ((wp.0 >> 16) & 0xFFFF) as i16 as f32 / 120.0;
                if key_down(VK_CONTROL) {
                    app.set_zoom(app.zoom * if d > 0.0 { 1.1 } else { 1.0 / 1.1 });
                } else {
                    app.scroll_by(-d * 120.0);
                }
                LRESULT(0)
            }
            WM_KEYDOWN | WM_SYSKEYDOWN => {
                if app.key(VIRTUAL_KEY(wp.0 as u16), msg == WM_SYSKEYDOWN) {
                    app.render();
                    return LRESULT(0);
                }
                DefWindowProcW(hwnd, msg, wp, lp)
            }
            WM_CHAR => {
                if app.edit.focused {
                    if let Some(c) = char::from_u32(wp.0 as u32)
                        && !c.is_control()
                    {
                        app.insert(&c.to_string());
                        app.render();
                    }
                }
                LRESULT(0)
            }
            _ => DefWindowProcW(hwnd, msg, wp, lp),
        }
    }
}

fn private_mb() -> f32 {
    crate::sys::process_memory().private_bytes as f32 / (1024.0 * 1024.0)
}

/// The full pipeline for one page, on a worker thread.
fn load_page(url: String, vw: f32, vh: f32, dpr: f32, tab: u64, seq: u64, shared: Arc<Shared>) {
    let r: Result<(), String> = (|| {
        if url != START && crate::web::block::is_danger(&url) {
            shared.post(Msg::Danger { tab, seq, url: url.clone() });
            return Ok(());
        }
        let doc = if url == START {
            load::from_html(START_HTML, Url { host: "hayate".into(), path: "/start".into() }, 1)
        } else {
            let u = Url::parse(&url).ok_or_else(|| format!("not an address: {url}"))?;
            load::load(&u)?
        };
        let mut dw = Dw::new();
        let styles = Cascade { dom: &doc.dom, sheets: &doc.sheets, vw, vh }.run();
        let mut images = Images::default();
        let dl = layout::layout(&doc.dom, &styles, &mut dw, &mut images, &doc.base, vw, vh);
        let final_url = if url == START { START.to_string() } else { doc.url.full() };
        if url != START && needs_js(&doc, &dl) {
            shared.post(Msg::NeedsJs { tab, seq, url: final_url });
            return Ok(());
        }
        shared.post(Msg::Preview { tab, seq, dl: dl.clone(), url: final_url });
        // images: fetch in parallel, read their sizes, lay out again
        let urls: Vec<String> = images.urls.iter().take(400).cloned().collect();
        let mut blocked = doc.blocked;
        let urls: Vec<String> = match crate::web::block::get() {
            Some(b) => urls
                .into_iter()
                .map(|u| {
                    if b.blocks(&u, &doc.url.host, crate::web::block::Kind::Image) {
                        blocked += 1;
                        String::new()
                    } else {
                        u
                    }
                })
                .collect(),
            None => urls,
        };
        let bytes = fetch_images(&urls);
        let mut any = false;
        for (i, b) in bytes.iter().enumerate() {
            if let Some(b) = b
                && let Some((w, h)) = image::size(b)
            {
                images.sizes[i] = Some((w as f32, h as f32));
                any = true;
            }
        }
        let dl = if any { layout::layout(&doc.dom, &styles, &mut dw, &mut images, &doc.base, vw, vh) } else { dl };
        // decode each image once, at the largest size it is shown
        let mut want: HashMap<u32, (f32, f32)> = HashMap::new();
        for it in &dl.items {
            if let Item::Image { r, img, .. } = it {
                let e = want.entry(*img).or_insert((0.0, 0.0));
                e.0 = e.0.max(r.w);
                e.1 = e.1.max(r.h);
            }
        }
        let jobs: Vec<(u32, (f32, f32))> = want.into_iter().filter(|(id, _)| bytes.get(*id as usize).is_some_and(|b| b.is_some())).collect();
        let pixels = decode_images(&jobs, &bytes, dpr);
        let page = Box::new(Page { doc, styles, images });
        drop(bytes);
        shared.post(Msg::Done { tab, seq, page, dl, pixels, blocked });
        Ok(())
    })();
    if let Err(e) = r {
        shared.post(Msg::Fail { tab, seq, err: e, url });
    }
}

/// Whether a page is a javascript application that shows nothing useful without it.
fn needs_js(doc: &load::Doc, dl: &DisplayList) -> bool {
    let dom = &doc.dom;
    let scripts = (0..dom.nodes.len() as u32).filter(|&n| dom.tag(n) == tg::script).count();
    let mut nos = String::new();
    for n in 0..dom.nodes.len() as u32 {
        if dom.tag(n) == tg::noscript {
            dom.text_content(n, &mut nos);
        }
    }
    let nos = nos.to_lowercase();
    let warns = nos.contains("javascript") && ["enable", "required", "disabled", "turn on", "needs", "requires", "please"].iter().any(|w| nos.contains(w));
    let challenge = ["Just a moment", "Attention Required", "Verifying you are human", "Checking your browser"].iter().any(|t| dl.title.contains(t));
    let glyphs = dl.glyphs.len();
    (scripts > 0 && glyphs < 250) || (warns && glyphs < 4000) || challenge
}

fn fetch_images(urls: &[String]) -> Vec<Option<Vec<u8>>> {
    let next = AtomicUsize::new(0);
    let out: Mutex<Vec<Option<Vec<u8>>>> = Mutex::new(vec![None; urls.len()]);
    std::thread::scope(|sc| {
        for _ in 0..8.min(urls.len()) {
            sc.spawn(|| {
                loop {
                    let i = next.fetch_add(1, Ordering::Relaxed);
                    let Some(u) = urls.get(i) else { break };
                    if u.is_empty() || u.ends_with(".svg") || u.contains(".svg?") {
                        continue;
                    }
                    if let Ok(b) = load::fetch_bytes(u, "image/avif,image/webp,image/png,image/jpeg,image/*;q=0.8")
                        && b.len() < 40 << 20
                    {
                        out.lock().unwrap_or_else(|e| e.into_inner())[i] = Some(b);
                    }
                }
            });
        }
    });
    out.into_inner().unwrap_or_default()
}

fn decode_images(jobs: &[(u32, (f32, f32))], bytes: &[Option<Vec<u8>>], dpr: f32) -> Vec<(u32, u32, u32, Vec<u8>)> {
    let next = AtomicUsize::new(0);
    let out = Mutex::new(Vec::new());
    std::thread::scope(|sc| {
        for _ in 0..4.min(jobs.len()) {
            sc.spawn(|| {
                loop {
                    let i = next.fetch_add(1, Ordering::Relaxed);
                    let Some(&(id, (w, h))) = jobs.get(i) else { break };
                    let Some(Some(b)) = bytes.get(id as usize) else { continue };
                    if let Some((pw, ph, px)) = image::decode(b, (w * dpr).ceil() as u32, (h * dpr).ceil() as u32) {
                        out.lock().unwrap_or_else(|e| e.into_inner()).push((id, pw, ph, px));
                    }
                }
            });
        }
    });
    out.into_inner().unwrap_or_default()
}

fn danger_html(url: &str) -> String {
    let esc = |s: &str| s.replace('&', "&amp;").replace('<', "&lt;").replace('"', "&quot;");
    let host = url.split("://").nth(1).unwrap_or(url).split('/').next().unwrap_or("").to_string();
    let enc: String = url.bytes().map(|b| if b.is_ascii_alphanumeric() || b"-._~".contains(&b) { (b as char).to_string() } else { format!("%{b:02X}") }).collect();
    format!(
        r#"<html><head><title>Stopped: dangerous site</title><style>
html {{ background: #0c0607; color: #ecebe6; font-family: "Segoe UI", sans-serif; }}
body {{ margin: 0; }}
main {{ max-width: 640px; margin: 0 auto; padding: 120px 28px; }}
.tag {{ font-family: "Cascadia Mono", Consolas, monospace; font-size: 11px; color: #ff6b5a; }}
h1 {{ font-size: 30px; font-weight: 600; margin: 16px 0 14px; line-height: 1.25; }}
.host {{ font-family: "Cascadia Mono", Consolas, monospace; color: #ffc88f; font-size: 15px; }}
p {{ color: #a09d97; line-height: 1.6; font-size: 15px; margin: 18px 0 0; }}
.row {{ display: flex; gap: 10px; margin-top: 34px; }}
a {{ display: block; padding: 10px 16px; border-radius: 6px; text-decoration: none; font-size: 14px; font-weight: 600; }}
a.safe {{ background: #ff6b1a; color: #08080b; }}
a.risk {{ color: #8b8a85; border: 1px solid #2a2a30; }}
</style></head><body><main>
<div class="tag">// KAMAITACHI STOPPED THIS PAGE</div>
<h1>This site is on a malware or phishing list.</h1>
<div class="host">{}</div>
<p>It matches the badware, URLhaus malware or phishing lists that every page is checked against. Sites like this steal passwords or push malicious downloads. Nothing from it has loaded.</p>
<div class="row"><a class="safe" href="https://kamaitachi.local/back">Go back</a><a class="risk" href="https://kamaitachi.local/proceed?u={}">Open anyway</a></div>
</main></body></html>"#,
        esc(&host),
        enc
    )
}

fn error_html(url: &str, err: &str) -> String {
    let esc = |s: &str| s.replace('&', "&amp;").replace('<', "&lt;");
    format!(
        "<html><head><title>Can't open this page</title><style>html{{background:#0b0b0d;color:#ededee;font-family:'Segoe UI',sans-serif}}body{{margin:0}}main{{max-width:620px;margin:0 auto;padding:140px 24px}}h1{{font-size:28px;font-weight:600;margin:0 0 12px}}p{{color:#8a8a93;font-size:15px;line-height:1.6;margin:0 0 8px}}code{{font-family:'Cascadia Mono',monospace;color:#ff6b1a;font-size:13px}}</style></head><body><main><h1>Can't open this page</h1><p><code>{}</code></p><p>{}</p></main></body></html>",
        esc(url),
        esc(err)
    )
}

impl App {
    fn tab(&self) -> &Tab {
        &self.tabs[self.cur]
    }
    fn tab_mut(&mut self) -> &mut Tab {
        let c = self.cur;
        &mut self.tabs[c]
    }

    /// Css px per device px for pages.
    fn k(&self) -> f32 {
        self.scale * self.zoom
    }

    fn view_w(&self) -> f32 {
        self.w as f32 / self.k()
    }

    fn view_h(&self) -> f32 {
        (self.h as f32 - TOP * self.scale).max(1.0) / self.k()
    }

    fn sync_size(&mut self) {
        let mut r = RECT::default();
        unsafe {
            let _ = GetClientRect(self.hwnd, &mut r);
        }
        self.w = (r.right - r.left).max(1) as u32;
        self.h = (r.bottom - r.top).max(1) as u32;
        self.gpu.resize(self.w, self.h);
    }

    fn animating(&self) -> bool {
        self.tabs.get(self.cur).is_some_and(|t| (t.target - t.scroll).abs() > 0.5)
    }

    fn max_scroll(&self) -> f32 {
        let t = self.tab();
        t.dl.as_ref().map_or(0.0, |d| (d.height - self.view_h()).max(0.0))
    }

    fn scroll_by(&mut self, d: f32) {
        let max = self.max_scroll();
        let t = self.tab_mut();
        t.target = (t.target + d).clamp(0.0, max);
        self.render();
    }

    fn scroll_to(&mut self, y: f32) {
        let max = self.max_scroll();
        let t = self.tab_mut();
        t.target = y.clamp(0.0, max);
        self.render();
    }

    fn new_tab(&mut self, url: &str) {
        let id = self.next_id;
        self.next_id += 1;
        self.tabs.push(Tab { id, url: String::new(), title: "New tab".into(), page: None, dl: None, scroll: 0.0, target: 0.0, back: Vec::new(), fwd: Vec::new(), loading: false, seq: 0, tex: HashMap::new(), restore: None, vw: 0.0, full_mode: false, full: None, pending: None, requested: false, parked: false, picture: None, picture_img: None, shot_at: None, used: Instant::now(), blocked: 0 });
        self.cur = self.tabs.len() - 1;
        self.load(self.cur, url.to_string(), false);
    }

    fn open_background(&mut self, url: &str) {
        let keep = self.cur;
        self.new_tab(url);
        self.cur = keep;
    }

    fn close_tab(&mut self, i: usize) {
        if i >= self.tabs.len() {
            return;
        }
        if let Some(f) = self.tabs[i].full.take() {
            unsafe {
                let _ = f.ctl.Close();
            }
        }
        self.tabs.remove(i);
        if self.tabs.is_empty() {
            self.new_tab(START);
            self.focus_addr();
            return;
        }
        if self.cur >= self.tabs.len() || self.cur > i {
            self.cur = self.cur.saturating_sub(1).min(self.tabs.len() - 1);
        }
        self.update_title();
        self.update_views();
    }

    /// Puts a warning page in tab `i` in place of a flagged site.
    fn show_danger(&mut self, i: usize, url: String, push: bool) {
        if let Some(f) = self.tabs[i].full.take() {
            unsafe {
                let _ = f.ctl.Close();
            }
        }
        let t = &mut self.tabs[i];
        if push && !t.url.is_empty() && t.url != url {
            t.back.push((t.url.clone(), t.scroll));
            t.fwd.clear();
        }
        t.full_mode = false;
        t.pending = None;
        t.seq += 1;
        t.url = url.clone();
        self.show_html(i, &danger_html(&url), "Stopped: dangerous site");
        self.update_views();
    }

    /// Lays out a page made here (warnings, errors) into tab `i`.
    fn show_html(&mut self, i: usize, html: &str, title: &str) {
        let doc = load::from_html(html, Url { host: "kamaitachi.local".into(), path: "/".into() }, 1);
        let (vw, vh) = (self.view_w(), self.view_h());
        let styles = Cascade { dom: &doc.dom, sheets: &doc.sheets, vw, vh }.run();
        let mut images = Images::default();
        let dl = layout::layout(&doc.dom, &styles, &mut self.dw, &mut images, &doc.base, vw, vh);
        let t = &mut self.tabs[i];
        t.title = title.into();
        t.dl = Some(dl);
        t.page = None;
        t.loading = false;
        t.scroll = 0.0;
        t.target = 0.0;
        self.update_title();
    }

    fn wants_full(&self, url: &str) -> bool {
        if url == START || url.is_empty() {
            return false;
        }
        let h = host_of(url);
        match self.engines.get(&h) {
            Some(&f) => f,
            None => full::is_app(&h),
        }
    }

    fn save_engines(&self) {
        let mut v: Vec<_> = self.engines.iter().map(|(h, f)| format!("{h} {}\n", if *f { "full" } else { "lite" })).collect();
        v.sort();
        let p = engines_path();
        if let Some(d) = p.parent() {
            let _ = std::fs::create_dir_all(d);
        }
        let _ = std::fs::write(p, v.concat());
    }

    /// Shows each tab's full view only while its tab is active; hidden ones sleep.
    fn update_views(&mut self) {
        let r = RECT { left: 0, top: (TOP * self.scale) as i32, right: self.w as i32, bottom: self.h as i32 };
        let cur = self.cur;
        let mut wake = None;
        for (i, t) in self.tabs.iter_mut().enumerate() {
            if i == cur {
                t.used = Instant::now();
            }
            if let Some(f) = &t.full {
                full::place(&f.ctl, r, i == cur && t.full_mode);
            } else if i == cur && t.full_mode && t.parked && !t.requested {
                wake = Some(i);
            }
            if i != cur {
                t.picture_img = None;
            }
        }
        if let Some(i) = wake {
            self.wake(i);
        }
    }

    /// Brings a parked tab back: its picture shows until the page is live again.
    fn wake(&mut self, i: usize) {
        if let Some(jpeg) = self.tabs[i].picture.clone()
            && let Some((w, h, px)) = image::decode(&jpeg, self.w, self.h)
            && let Ok(img) = self.gpu.image(w, h, &px)
        {
            self.tabs[i].picture_img = Some(img);
        }
        let t = &mut self.tabs[i];
        t.pending = Some(t.url.clone());
        t.requested = true;
        t.loading = true;
        let id = t.id;
        self.request_view(id);
    }

    /// Closes a hidden tab's full view; its chromium process goes away with it.
    fn park(&mut self, i: usize) {
        let t = &mut self.tabs[i];
        if let Some(f) = t.full.take() {
            unsafe {
                let _ = f.ctl.Close();
            }
            t.parked = true;
            t.requested = false;
            t.loading = false;
        }
    }

    /// Keeps hayate and chromium under the memory budget, and parks tabs nobody looked
    /// at for ten minutes.
    fn govern(&mut self) {
        let budget: f32 = std::env::var("HAYATE_BUDGET_MB").ok().and_then(|v| v.parse().ok()).unwrap_or(500.0);
        let total = private_mb() + self.full_mb;
        let cur = self.cur;
        let mut idle: Vec<(usize, Instant)> = self.tabs.iter().enumerate().filter(|(i, t)| *i != cur && t.full.is_some()).map(|(i, t)| (i, t.used)).collect();
        idle.sort_by_key(|x| x.1);
        if total > budget * 0.9
            && let Some(&(i, _)) = idle.first()
        {
            self.park(i);
            return;
        }
        for (i, used) in idle {
            if used.elapsed().as_secs() > 600 {
                self.park(i);
            }
        }
    }

    /// Opens `url` in tab `i` with the full engine.
    fn load_full(&mut self, i: usize, url: String, push: bool) {
        let t = &mut self.tabs[i];
        if push && !t.url.is_empty() {
            t.back.push((t.url.clone(), t.scroll));
            t.fwd.clear();
        }
        t.full_mode = true;
        t.seq += 1;
        t.blocked = 0;
        t.url = url.clone();
        t.title = host_of(&url).trim_start_matches("www.").to_string();
        t.dl = None;
        t.page = None;
        t.tex.clear();
        t.loading = true;
        let id = t.id;
        t.parked = false;
        if let Some(f) = &t.full {
            full::navigate(&f.wv, &url);
        } else {
            t.pending = Some(url);
            if !t.requested {
                t.requested = true;
                self.request_view(id);
            }
        }
        self.update_title();
        self.update_views();
    }

    fn request_view(&mut self, id: u64) {
        match self.env_state {
            0 => {
                self.waiting.push(id);
                self.env_state = 1;
                if let Err(e) = full::start(self.hwnd) {
                    self.env_state = 3;
                    self.full_failed(&e);
                }
            }
            1 => self.waiting.push(id),
            2 => {
                if let Some(env) = &self.env
                    && let Err(e) = full::create(env, self.hwnd, id)
                {
                    self.full_failed(&e);
                }
            }
            _ => self.full_failed("the full engine (WebView2) is not available on this computer"),
        }
    }

    fn full_failed(&mut self, err: &str) {
        let ids: Vec<u64> = self.tabs.iter().filter(|t| t.full_mode && t.full.is_none()).map(|t| t.id).collect();
        for id in ids {
            if let Some(i) = self.tabs.iter().position(|t| t.id == id) {
                let url = self.tabs[i].pending.take().unwrap_or_default();
                self.tabs[i].full_mode = false;
                let seq = self.tabs[i].seq;
                self.shared.inbox.lock().unwrap_or_else(|e| e.into_inner()).push(Msg::Fail { tab: id, seq, err: err.to_string(), url });
            }
        }
        self.inbox();
    }

    /// Events from the full engine, handled outside its callbacks.
    fn full_events(&mut self) {
        for ev in full::take() {
            match ev {
                full::Ev::Env(Ok(env)) => {
                    self.env = Some(env);
                    self.env_state = 2;
                    for id in std::mem::take(&mut self.waiting) {
                        self.request_view(id);
                    }
                    unsafe {
                        SetTimer(Some(self.hwnd), 2, 2000, None);
                    }
                }
                full::Ev::Env(Err(e)) => {
                    self.env_state = 3;
                    self.full_failed(&e);
                }
                full::Ev::Ready { tab, ctl } => {
                    let Some(i) = self.tabs.iter().position(|t| t.id == tab) else {
                        unsafe {
                            let _ = ctl.Close();
                        }
                        continue;
                    };
                    let env = self.env.clone();
                    match full::wire(&ctl, tab, env) {
                        Ok(wv) => {
                            if let Some(u) = self.tabs[i].pending.take() {
                                full::navigate(&wv, &u);
                            }
                            self.tabs[i].full = Some(FullView { ctl, wv });
                            self.tabs[i].requested = false;
                            self.tabs[i].parked = false;
                            self.tabs[i].picture_img = None;
                            self.update_views();
                            if i == self.cur && !self.edit.focused
                                && let Some(f) = &self.tabs[i].full
                            {
                                full::focus(&f.ctl);
                            }
                            self.measure_full();
                        }
                        Err(e) => self.full_failed(&e.message()),
                    }
                }
                full::Ev::Failed { err, .. } => self.full_failed(&err),
                full::Ev::Url { tab, url } => {
                    if let Some(t) = self.tabs.iter_mut().find(|t| t.id == tab && t.full_mode)
                        && !url.is_empty()
                        && url != "about:blank"
                    {
                        t.url = url;
                    }
                }
                full::Ev::Title { tab, title } => {
                    if let Some(t) = self.tabs.iter_mut().find(|t| t.id == tab && t.full_mode)
                        && !title.is_empty()
                    {
                        t.title = title;
                    }
                    self.update_title();
                }
                full::Ev::Loading { tab, on } => {
                    if let Some(t) = self.tabs.iter_mut().find(|t| t.id == tab && t.full_mode) {
                        t.loading = on;
                    }
                }
                full::Ev::NewWindow { url } => {
                    self.new_tab(&url);
                }
                full::Ev::Key { vk, alt } => {
                    self.key(VIRTUAL_KEY(vk as u16), alt);
                }
                full::Ev::Danger { tab, url } => {
                    if let Some(i) = self.tabs.iter().position(|t| t.id == tab) {
                        self.show_danger(i, url, true);
                    }
                }
                full::Ev::Blocked { tab } => {
                    if let Some(t) = self.tabs.iter_mut().find(|t| t.id == tab) {
                        t.blocked += 1;
                    }
                }
                full::Ev::Picture { tab, jpeg } => {
                    if let Some(t) = self.tabs.iter_mut().find(|t| t.id == tab) {
                        t.picture = Some(jpeg);
                    }
                }
            }
        }
    }

    /// Memory of the full engine's processes, refreshed every two seconds.
    fn measure_full(&mut self) {
        let c = self.cur;
        if let Some(t) = self.tabs.get_mut(c)
            && let Some(f) = &t.full
            && !t.loading
            && t.shot_at.is_none_or(|s| s.elapsed().as_secs() >= 6)
        {
            full::capture(&f.wv, t.id);
            t.shot_at = Some(Instant::now());
        }
        let pid = self.tabs.iter().find_map(|t| t.full.as_ref()).map_or(0, |f| full::browser_pid(&f.wv));
        let mb = if pid == 0 { 0.0 } else { full::tree_mb(pid) };
        self.full_mb = mb;
        self.govern();
        if (mb - self.full_mb).abs() > 0.5 {
            self.full_mb = mb;
            self.render();
        }
    }

    /// Switches the current site between the light and the full engine, and remembers it.
    fn toggle_engine(&mut self) {
        let c = self.cur;
        let url = self.tabs[c].url.clone();
        if url == START || url.is_empty() {
            return;
        }
        let want_full = !self.tabs[c].full_mode;
        self.engines.insert(host_of(&url), want_full);
        self.save_engines();
        self.load(c, url, false);
    }

    /// Starts loading `url` into tab `i`; `push` records the old page in history.
    fn load(&mut self, i: usize, url: String, push: bool) {
        if let Some(rest) = url.strip_prefix("https://kamaitachi.local/") {
            if let Some(q) = rest.strip_prefix("proceed?u=") {
                let target = crate::url::percent_decode(q);
                crate::web::block::allow(&host_of(&target));
                return self.load(i, target, false);
            }
            self.cur = i;
            return self.go_back();
        }
        if url != START && crate::web::block::is_danger(&url) {
            return self.show_danger(i, url, push);
        }
        if self.wants_full(&url) {
            return self.load_full(i, url, push);
        }
        if self.tabs[i].full_mode {
            // back to the light engine: the full view goes away with its memory
            if let Some(f) = self.tabs[i].full.take() {
                unsafe {
                    let _ = f.ctl.Close();
                }
            }
            self.tabs[i].full_mode = false;
            self.tabs[i].pending = None;
        }
        let (vw, vh, dpr) = (self.view_w(), self.view_h(), self.k());
        let shared = self.shared.clone();
        let t = &mut self.tabs[i];
        if push && !t.url.is_empty() {
            t.back.push((t.url.clone(), t.scroll));
            t.fwd.clear();
        }
        t.url = url.clone();
        t.seq += 1;
        t.loading = true;
        t.vw = vw;
        t.blocked = 0;
        if url != START {
            t.title = url.trim_start_matches("https://").to_string();
        }
        let (id, seq) = (t.id, t.seq);
        let _ = std::thread::Builder::new().stack_size(64 << 20).name("page".into()).spawn(move || load_page(url, vw, vh, dpr, id, seq, shared));
        self.update_title();
    }

    fn navigate(&mut self, input: &str, new_tab: bool) {
        let s = input.trim();
        if s.is_empty() {
            return;
        }
        let url = if s.starts_with("hayate:") || s == "about:blank" { START.to_string() } else { crate::url::from_input(s).full() };
        if new_tab {
            self.new_tab(&url);
        } else {
            let c = self.cur;
            self.load(c, url, true);
        }
        self.edit.focused = false;
    }

    fn go_back(&mut self) {
        let c = self.cur;
        if let Some(f) = &self.tabs[c].full
            && self.tabs[c].full_mode
            && full::can(&f.wv).0
        {
            unsafe {
                let _ = f.wv.GoBack();
            }
            return;
        }
        let t = &mut self.tabs[c];
        if let Some((u, y)) = t.back.pop() {
            t.fwd.push((t.url.clone(), t.scroll));
            t.restore = Some(y);
            self.load(c, u, false);
        }
    }

    fn go_fwd(&mut self) {
        let c = self.cur;
        if let Some(f) = &self.tabs[c].full
            && self.tabs[c].full_mode
            && full::can(&f.wv).1
        {
            unsafe {
                let _ = f.wv.GoForward();
            }
            return;
        }
        let t = &mut self.tabs[c];
        if let Some((u, y)) = t.fwd.pop() {
            t.back.push((t.url.clone(), t.scroll));
            t.restore = Some(y);
            self.load(c, u, false);
        }
    }

    fn reload(&mut self) {
        let c = self.cur;
        if let Some(f) = &self.tabs[c].full
            && self.tabs[c].full_mode
        {
            unsafe {
                let _ = f.wv.Reload();
            }
            return;
        }
        let u = self.tabs[c].url.clone();
        self.tabs[c].restore = Some(self.tabs[c].scroll);
        self.load(c, u, false);
    }

    fn set_zoom(&mut self, z: f32) {
        self.zoom = z.clamp(0.5, 3.0);
        self.relayout_due = true;
        self.relayout();
        self.render();
    }

    fn update_title(&self) {
        if let Some(t) = self.tabs.get(self.cur) {
            let s: Vec<u16> = format!("{} - {BRAND}", t.title).encode_utf16().chain(Some(0)).collect();
            unsafe {
                let _ = SetWindowTextW(self.hwnd, PCWSTR(s.as_ptr()));
            }
        }
    }

    fn inbox(&mut self) {
        let msgs = std::mem::take(&mut *self.shared.inbox.lock().unwrap_or_else(|e| e.into_inner()));
        for m in msgs {
            match m {
                Msg::Preview { tab, seq, dl, url } => {
                    let Some(t) = self.tabs.iter_mut().find(|t| t.id == tab && t.seq == seq) else { continue };
                    t.title = if dl.title.is_empty() { url.trim_start_matches("https://").to_string() } else { dl.title.clone() };
                    t.url = url;
                    t.dl = Some(dl);
                    t.tex.clear();
                    t.page = None;
                    let y = t.restore.take().unwrap_or(0.0);
                    t.scroll = y;
                    t.target = y;
                }
                Msg::Done { tab, seq, page, dl, pixels, blocked } => {
                    let Some(i) = self.tabs.iter().position(|t| t.id == tab && t.seq == seq) else { continue };
                    let mut tex = HashMap::new();
                    for (id, w, h, px) in pixels {
                        if let Ok(img) = self.gpu.image(w, h, &px) {
                            tex.insert(id, img);
                        }
                    }
                    if std::env::var("HAYATE_MEMLOG").is_ok() {
                        let st = &page.styles;
                        let dom_n = page.doc.dom.nodes.len();
                        let text: usize = page.doc.dom.nodes.iter().map(|n| match &n.kind { crate::web::dom::Kind::Text(s) => s.capacity(), crate::web::dom::Kind::Elem(e) => e.attrs.iter().map(|a| a.1.capacity() + 16).sum::<usize>() + 64, _ => 0 }).sum();
                        let rules: usize = page.doc.sheets.iter().map(|s| s.rules.len()).sum();
                        let (img, gl) = self.gpu.held();
                        eprintln!(
                            "mem: dom {} nodes ~{:.1} mb | styles {} x {} b = {:.1} mb | rules {} | list {:.1} mb ({} glyphs) | images {:.1} mb | glyph cache {:.1} mb | private {:.1} mb",
                            dom_n,
                            (dom_n * std::mem::size_of::<crate::web::dom::Node>() + text) as f32 / 1048576.0,
                            st.list.len(),
                            std::mem::size_of::<crate::web::style::Style>(),
                            (st.list.len() * std::mem::size_of::<crate::web::style::Style>()) as f32 / 1048576.0,
                            rules,
                            dl.bytes() as f32 / 1048576.0,
                            dl.glyphs.len(),
                            img as f32 / 1048576.0,
                            gl as f32 / 1048576.0,
                            private_mb()
                        );
                    }
                    let t = &mut self.tabs[i];
                    t.blocked = blocked;
                    t.tex = tex;
                    t.dl = Some(dl);
                    t.page = Some(page);
                    t.loading = false;
                    // the window may have changed size while this page loaded
                    if i == self.cur && (t.vw - self.view_w()).abs() > 0.5 {
                        self.relayout_due = true;
                        self.relayout();
                    }
                    if self.shot.is_some() {
                        // a window manager may have snapped the window; shots use the size asked for
                        unsafe {
                            let _ = SetWindowPos(self.hwnd, None, 0, 0, self.want.0, self.want.1, SWP_NOZORDER | SWP_NOMOVE | SWP_NOACTIVATE);
                        }
                        self.sync_size();
                        self.relayout_due = true;
                        self.relayout();
                        if let Some((_, n)) = &mut self.shot {
                            *n = 1;
                        }
                    }
                }
                Msg::Danger { tab, seq, url } => {
                    let Some(i) = self.tabs.iter().position(|t| t.id == tab && t.seq == seq) else { continue };
                    self.show_danger(i, url, false);
                }
                Msg::NeedsJs { tab, seq, url } => {
                    let Some(i) = self.tabs.iter().position(|t| t.id == tab && t.seq == seq) else { continue };
                    // learn it, so the next visit goes straight to the full engine
                    self.engines.insert(host_of(&url), true);
                    self.save_engines();
                    self.load_full(i, url, false);
                }
                Msg::Fail { tab, seq, err, url } => {
                    let Some(i) = self.tabs.iter().position(|t| t.id == tab && t.seq == seq) else { continue };
                    let doc = load::from_html(&error_html(&url, &err), Url { host: "hayate".into(), path: "/error".into() }, 1);
                    let (vw, vh) = (self.view_w(), self.view_h());
                    let styles = Cascade { dom: &doc.dom, sheets: &doc.sheets, vw, vh }.run();
                    let mut images = Images::default();
                    let dl = layout::layout(&doc.dom, &styles, &mut self.dw, &mut images, &doc.base, vw, vh);
                    let t = &mut self.tabs[i];
                    t.title = "Can't open this page".into();
                    t.dl = Some(dl);
                    t.page = None;
                    t.loading = false;
                }
            }
        }
        self.update_title();
    }

    /// Lays the current page out again for a new width or zoom.
    fn relayout(&mut self) {
        if !self.relayout_due {
            return;
        }
        let (vw, vh) = (self.view_w(), self.view_h());
        let c = self.cur;
        let t = &mut self.tabs[c];
        if (t.vw - vw).abs() < 0.5 {
            self.relayout_due = false;
            return;
        }
        // a page still loading is laid out again when it arrives
        let Some(page) = t.page.as_mut() else { return };
        self.relayout_due = false;
        let mk = media_key(&page.doc.sheets, vw, vh);
        if mk != page.styles.media_key {
            page.styles = Cascade { dom: &page.doc.dom, sheets: &page.doc.sheets, vw, vh }.run();
        }
        let frac = t.dl.as_ref().map_or(0.0, |d| if d.height > 0.0 { t.scroll / d.height } else { 0.0 });
        let dl = layout::layout(&page.doc.dom, &page.styles, &mut self.dw, &mut page.images, &page.doc.base, vw, vh);
        t.scroll = (frac * dl.height).max(0.0);
        t.target = t.scroll;
        t.dl = Some(dl);
        t.vw = vw;
    }

    // ---- input ----

    fn focus_addr(&mut self) {
        unsafe {
            let _ = windows::Win32::UI::Input::KeyboardAndMouse::SetFocus(Some(self.hwnd));
        }
        let url = self.tab().url.clone();
        let shown = if url == START { String::new() } else { url };
        self.edit = Edit { caret: shown.chars().count(), text: shown.chars().collect(), focused: true, all: true };
    }

    fn insert(&mut self, s: &str) {
        let e = &mut self.edit;
        if e.all {
            e.text.clear();
            e.caret = 0;
            e.all = false;
        }
        for c in s.chars().filter(|c| !c.is_control()) {
            e.text.insert(e.caret, c);
            e.caret += 1;
        }
    }

    fn paste(&mut self) {
        let s = unsafe {
            if OpenClipboard(Some(self.hwnd)).is_err() {
                return;
            }
            let mut out = String::new();
            if let Ok(h) = GetClipboardData(13) {
                let p = GlobalLock(HGLOBAL(h.0)) as *const u16;
                if !p.is_null() {
                    let mut n = 0;
                    while *p.add(n) != 0 && n < 1 << 20 {
                        n += 1;
                    }
                    out = String::from_utf16_lossy(std::slice::from_raw_parts(p, n));
                    let _ = GlobalUnlock(HGLOBAL(h.0));
                }
            }
            let _ = CloseClipboard();
            out
        };
        self.insert(&s.replace(['\r', '\n'], " "));
    }

    fn copy(&self, s: &str) {
        unsafe {
            if OpenClipboard(Some(self.hwnd)).is_err() {
                return;
            }
            let _ = EmptyClipboard();
            let w: Vec<u16> = s.encode_utf16().chain(Some(0)).collect();
            if let Ok(h) = GlobalAlloc(GMEM_MOVEABLE, w.len() * 2) {
                let p = GlobalLock(h) as *mut u16;
                if !p.is_null() {
                    std::ptr::copy_nonoverlapping(w.as_ptr(), p, w.len());
                    let _ = GlobalUnlock(h);
                    let _ = SetClipboardData(13, Some(HANDLE(h.0)));
                }
            }
            let _ = CloseClipboard();
        }
    }

    /// Returns true when the key was used.
    fn key(&mut self, vk: VIRTUAL_KEY, alt: bool) -> bool {
        let ctrl = key_down(VK_CONTROL);
        let shift = key_down(VK_SHIFT);
        if alt {
            match vk {
                VK_LEFT => self.go_back(),
                VK_RIGHT => self.go_fwd(),
                VK_D => self.focus_addr(),
                _ => return false,
            }
            return true;
        }
        if ctrl {
            match vk {
                VK_L => self.focus_addr(),
                VK_E if shift => self.toggle_engine(),
                VK_T => {
                    self.new_tab(START);
                    self.focus_addr();
                }
                VK_W => {
                    let c = self.cur;
                    self.close_tab(c);
                }
                VK_R => self.reload(),
                VK_TAB => {
                    let n = self.tabs.len();
                    self.cur = if shift { (self.cur + n - 1) % n } else { (self.cur + 1) % n };
                    self.update_title();
                    self.update_views();
                }
                VK_OEM_PLUS | VK_ADD => self.set_zoom(self.zoom * 1.1),
                VK_OEM_MINUS | VK_SUBTRACT => self.set_zoom(self.zoom / 1.1),
                VK_0 | VK_NUMPAD0 => self.set_zoom(1.0),
                VK_A if self.edit.focused => self.edit.all = true,
                VK_V if self.edit.focused => self.paste(),
                VK_C if self.edit.focused => {
                    let s: String = self.edit.text.iter().collect();
                    self.copy(&s);
                }
                VK_C => {
                    let u = self.tab().url.clone();
                    self.copy(&u);
                }
                k if (VK_1.0..=VK_9.0).contains(&k.0) => {
                    let i = (k.0 - VK_1.0) as usize;
                    self.cur = if k == VK_9 { self.tabs.len() - 1 } else { i.min(self.tabs.len() - 1) };
                    self.update_title();
                    self.update_views();
                }
                _ => return false,
            }
            return true;
        }
        if vk == VK_F6 {
            self.focus_addr();
            return true;
        }
        if vk == VK_F5 {
            self.reload();
            return true;
        }
        if self.edit.focused {
            let e = &mut self.edit;
            match vk {
                VK_RETURN => {
                    let s: String = e.text.iter().collect();
                    self.navigate(&s, false);
                }
                VK_ESCAPE => e.focused = false,
                VK_BACK => {
                    if e.all {
                        e.text.clear();
                        e.caret = 0;
                        e.all = false;
                    } else if e.caret > 0 {
                        e.caret -= 1;
                        e.text.remove(e.caret);
                    }
                }
                VK_DELETE => {
                    if e.all {
                        e.text.clear();
                        e.caret = 0;
                        e.all = false;
                    } else if e.caret < e.text.len() {
                        e.text.remove(e.caret);
                    }
                }
                VK_LEFT => {
                    e.all = false;
                    e.caret = e.caret.saturating_sub(1);
                }
                VK_RIGHT => {
                    e.all = false;
                    e.caret = (e.caret + 1).min(e.text.len());
                }
                VK_HOME => {
                    e.all = false;
                    e.caret = 0;
                }
                VK_END => {
                    e.all = false;
                    e.caret = e.text.len();
                }
                _ => return false,
            }
            return true;
        }
        let page = self.view_h();
        match vk {
            VK_DOWN => self.scroll_by(60.0),
            VK_UP => self.scroll_by(-60.0),
            VK_NEXT => self.scroll_by(page - 60.0),
            VK_PRIOR => self.scroll_by(-(page - 60.0)),
            VK_SPACE => self.scroll_by(if shift { -(page - 60.0) } else { page - 60.0 }),
            VK_HOME => self.scroll_to(0.0),
            VK_END => self.scroll_to(f32::MAX),
            VK_BACK => self.go_back(),
            VK_ESCAPE => {
                if let Some(f) = &self.tab().full {
                    unsafe {
                        let _ = f.wv.Stop();
                    }
                }
                let t = self.tab_mut();
                if t.loading {
                    t.seq += 1;
                    t.loading = false;
                }
            }
            _ => return false,
        }
        true
    }

    fn hit(&self, x: f32, y: f32) -> Hit {
        let (cx, cy) = (x / self.scale, y / self.scale);
        self.hits.iter().rev().find(|(r, _)| r.contains(cx, cy)).map_or(Hit::None, |(_, h)| *h)
    }

    fn mouse_move(&mut self, x: f32, y: f32) {
        self.mouse = (x, y);
        let h = self.hit(x, y);
        let link = if y > TOP * self.scale {
            let k = self.k();
            let t = self.tab();
            t.dl.as_ref().and_then(|d| d.link_at(x / k, (y - TOP * self.scale) / k + t.scroll)).map(|s| s.to_string())
        } else {
            None
        };
        if h != self.hover || link != self.hover_link {
            self.hover = h;
            self.hover_link = link;
            self.render();
        }
    }

    fn click(&mut self, x: f32, y: f32, ctrl: bool) {
        self.mouse_move(x, y);
        match self.hit(x, y) {
            Hit::Tab(i) => {
                self.cur = i;
                self.edit.focused = false;
                self.update_title();
                self.update_views();
            }
            Hit::Engine => self.toggle_engine(),
            Hit::Close(i) => self.close_tab(i),
            Hit::NewTab => {
                self.new_tab(START);
                self.focus_addr();
            }
            Hit::Back => self.go_back(),
            Hit::Fwd => self.go_fwd(),
            Hit::Reload => self.reload(),
            Hit::Addr => {
                if !self.edit.focused {
                    self.focus_addr();
                } else {
                    self.edit.all = false;
                }
            }
            Hit::None => {
                self.edit.focused = false;
                if let Some(l) = self.hover_link.clone() {
                    if ctrl {
                        self.open_background(&l);
                    } else {
                        let c = self.cur;
                        self.load(c, l, true);
                    }
                } else if let Some(f) = &self.tab().full {
                    full::focus(&f.ctl);
                }
            }
        }
        self.render();
    }

    // ---- drawing ----

    /// Lays out one line of interface text; returns its glyphs (relative) and width.
    fn ui_text(&mut self, s: &str, size: f32, weight: u16, family: &str, color: Rgba) -> (DisplayList, f32) {
        let txt: Vec<u16> = s.encode_utf16().collect();
        let len = txt.len() as u32;
        let p = Para { text: txt, runs: vec![Run { start: 0, len, font: Font { family: Arc::from(family), size, weight, italic: false }, color, deco: 0, link: u32::MAX }], nowrap: true, ..Default::default() };
        let mut dl = DisplayList::new();
        let po = self.dw.layout(&p, 1.0e5, 0.0, 0.0, Some(&mut dl));
        (dl, po.width)
    }

    /// Draws interface text with its top-left at (x, y) css px, clipped to `max` width.
    #[allow(clippy::too_many_arguments)]
    fn label(&mut self, s: &str, x: f32, y: f32, size: f32, weight: u16, color: Rgba, max: f32, family: &str) -> f32 {
        let (dl, w) = self.ui_text(s, size, weight, family, color);
        let k = self.scale;
        let clip = [x * k, -1.0e9, (x + max) * k, 1.0e9];
        for it in &dl.items {
            if let Item::Text { start, len, color, .. } = *it {
                let c = gpu::rgba(color, 1.0);
                for g in &dl.glyphs[start as usize..(start + len) as usize] {
                    self.gpu.glyph(g.face, g.size * k, g.id, (x + g.x) * k, (y + g.y) * k, c, clip);
                }
            }
        }
        w
    }

    fn icon(&mut self, cp: char, cx: f32, cy: f32, size: f32, color: Rgba) {
        let (dl, w) = self.ui_text(&cp.to_string(), size, 400, ICON_FONT, color);
        let k = self.scale;
        for it in &dl.items {
            if let Item::Text { start, len, color, .. } = *it {
                let c = gpu::rgba(color, 1.0);
                for g in &dl.glyphs[start as usize..(start + len) as usize] {
                    self.gpu.glyph(g.face, g.size * k, g.id, (cx - w / 2.0 + g.x) * k, (cy - size / 2.0 + g.y) * k, c, gpu::NO_CLIP);
                }
            }
        }
    }

    fn rect(&mut self, r: Rect, color: Rgba, radius: f32) {
        let k = self.scale;
        self.gpu.rect(r.x * k, r.y * k, r.w * k, r.h * k, color, radius * k, gpu::NO_CLIP);
    }

    fn render(&mut self) {
        if self.tabs.is_empty() || self.w == 0 {
            return;
        }
        // scrolling eases toward its target, one step per frame
        {
            let t = self.tab_mut();
            let d = t.target - t.scroll;
            t.scroll = if d.abs() < 0.5 { t.target } else { t.scroll + d * 0.28 };
        }
        let bg = if self.tab().url == START {
            BG
        } else if self.tab().full_mode { [0xff, 0xff, 0xff, 255] } else { self.tab().dl.as_ref().map_or(BG, |d| d.bg) };
        self.gpu.begin(bg);
        self.draw_page();
        self.draw_chrome();
        let shoot = matches!(self.shot, Some((_, n)) if n >= 1);
        if shoot {
            if let Some((_, n)) = &mut self.shot {
                *n += 1;
            }
            // a couple of frames so late glyphs are in the atlas
            if let Some((p, n)) = &self.shot
                && *n >= 3
            {
                let p = p.clone();
                let _ = self.gpu.draw();
                if let Ok((w, h, px)) = self.gpu.capture() {
                    let _ = write_bmp(&p, w, h, &px);
                }
                eprintln!("shot: {} ({} ms, {:.1} mb private, gpu {})", p, self.started.elapsed().as_millis(), private_mb(), self.gpu.name());
                unsafe { PostQuitMessage(0) };
                return;
            }
            let _ = self.gpu.end();
            // a hidden window gets no paint messages; ask for the next frame directly
            unsafe {
                let _ = PostMessageW(Some(self.hwnd), WM_APP, WPARAM(0), LPARAM(0));
            }
            return;
        }
        let _ = self.gpu.end();
    }

    fn draw_page(&mut self) {
        if self.tab().url == START {
            self.draw_scene();
        } else if self.wall.is_some() || self.wall_checked {
            // off the new tab the wallpaper's pixels go away
            self.wall = None;
            self.wall_checked = false;
        }
        let k = self.k();
        let top = TOP * self.scale;
        let (w, h) = (self.w as f32, self.h as f32);
        let view = [0.0, top, w, h];
        let c = self.cur;
        let t = &self.tabs[c];
        if t.full_mode {
            if t.full.is_none()
                && let Some(img) = &t.picture_img
            {
                let iw = img_size(img).0 as f32;
                let ih = img_size(img).1 as f32;
                let k = ((w / iw).min((h - top) / ih)).min(1.0);
                self.gpu.draw_image(img, 0.0, top, iw * k, ih * k, 1.0, 0.0, view);
                return;
            }
            if t.full.is_none() {
                let msg = if self.env_state == 1 || self.env_state == 0 { "Starting the full engine" } else { "Opening" };
                let k = self.scale;
                let (cw, ch) = (self.w as f32 / k, self.h as f32 / k);
                self.label(msg, cw / 2.0 - 70.0, TOP + (ch - TOP) / 2.0 - 10.0, 14.0, 400, MUTED, 300.0, UI_FONT);
            }
            return;
        }
        let Some(dl) = &t.dl else { return };
        let scroll = t.scroll;
        let oy = top - scroll * k;
        let vh = self.view_h();
        let (y0, y1) = (scroll - 50.0, scroll + vh + 50.0);
        let clipf = |ci: u16| -> Clip {
            if ci == 0 {
                return view;
            }
            let r = dl.clips[ci as usize];
            [(r.x * k).max(view[0]), (r.y * k + oy).max(view[1]), ((r.x + r.w) * k).min(view[2]), ((r.y + r.h) * k + oy).min(view[3])]
        };
        let gpu = &mut self.gpu;
        for it in &dl.items {
            match *it {
                Item::Rect { r, color, radius, clip } => {
                    if r.y + r.h < y0 || r.y > y1 {
                        continue;
                    }
                    gpu.rect(r.x * k, r.y * k + oy, r.w * k, r.h * k, color, radius * k, clipf(clip));
                }
                Item::Ring { r, width, color, radius, clip } => {
                    if r.y + r.h < y0 || r.y > y1 {
                        continue;
                    }
                    gpu.ring(r.x * k, r.y * k + oy, r.w * k, r.h * k, width * k, color, radius * k, clipf(clip));
                }
                Item::Text { start, len, color, clip } => {
                    if len == 0 {
                        continue;
                    }
                    let gs = &dl.glyphs[start as usize..(start + len) as usize];
                    if gs[gs.len() - 1].y < y0 || gs[0].y > y1 + 40.0 {
                        continue;
                    }
                    let cc = gpu::rgba(color, 1.0);
                    let cl = clipf(clip);
                    for g in gs {
                        gpu.glyph(g.face, g.size * k, g.id, g.x * k, g.y * k + oy, cc, cl);
                    }
                }
                Item::Image { r, img, alpha, radius, clip } => {
                    if r.y + r.h < y0 || r.y > y1 || r.w <= 0.0 {
                        continue;
                    }
                    if let Some(tex) = t.tex.get(&img) {
                        gpu.draw_image(tex, r.x * k, r.y * k + oy, r.w * k, r.h * k, alpha as f32 / 255.0, radius * k, clipf(clip));
                    } else if t.loading {
                        gpu.rect(r.x * k, r.y * k + oy, r.w * k, r.h * k, [128, 128, 128, 24], radius * k, clipf(clip));
                    }
                }
            }
        }
        // a thin scrollbar that only shows when the page is taller than the window
        if dl.height > vh + 1.0 {
            let track = h - top;
            let th = (track * vh / dl.height).max(32.0 * self.scale);
            let ty = top + (track - th) * (scroll / (dl.height - vh)).clamp(0.0, 1.0);
            let dark = (dl.bg[0] as u32 + dl.bg[1] as u32 + dl.bg[2] as u32) < 384;
            let col = if dark { [255, 255, 255, 60] } else { [0, 0, 0, 70] };
            gpu.rect(w - 9.0 * self.scale, ty + 2.0 * self.scale, 5.0 * self.scale, th - 4.0 * self.scale, col, 3.0 * self.scale, view);
        }
    }

    /// A dropped image becomes the new tab's wallpaper.
    fn set_wallpaper(&mut self, path: &str) {
        let ext = std::path::Path::new(path).extension().map(|e| e.to_string_lossy().to_ascii_lowercase()).unwrap_or_default();
        if !WALL_EXT.contains(&ext.as_str()) {
            return;
        }
        let _ = std::fs::create_dir_all(wall_dir());
        for e in WALL_EXT {
            let _ = std::fs::remove_file(wall_dir().join(format!("wallpaper.{e}")));
        }
        if std::fs::copy(path, wall_dir().join(format!("wallpaper.{ext}"))).is_ok() {
            self.wall = None;
            self.wall_checked = false;
            if self.tab().url != START {
                self.new_tab(START);
            }
            self.render();
        }
    }

    /// The wallpaper at this window's size: decoded once to cover the page area.
    fn wallpaper(&mut self) -> Option<paint::Image> {
        let (cw, ch) = (self.w, (self.h as f32 - TOP * self.scale).max(1.0) as u32);
        if let Some((w, h, img)) = &self.wall
            && *w == cw
            && *h == ch
        {
            return Some(img.clone());
        }
        if self.wall_checked && self.wall.is_none() {
            return None;
        }
        self.wall_checked = true;
        let bytes = std::fs::read(wall_file()?).ok()?;
        let (nw, nh) = image::size(&bytes)?;
        let k = (cw as f32 / nw as f32).max(ch as f32 / nh as f32);
        let (tw, th) = ((nw as f32 * k).ceil() as u32, (nh as f32 * k).ceil() as u32);
        let (pw, ph, px) = image::decode(&bytes, tw, th)?;
        drop(bytes);
        let img = self.gpu.image(pw, ph, &px).ok()?;
        self.wall = Some((cw, ch, img.clone()));
        Some(img)
    }

    /// The new-tab scene, drawn from shapes every frame and kept nowhere: a night sky,
    /// a striped vermilion sun, a snow-capped peak, hills, and seigaiha waves.
    fn draw_scene(&mut self) {
        if let Some(img) = self.wallpaper() {
            let k = self.scale;
            let top = TOP * k;
            let (w, h) = (self.w as f32, self.h as f32);
            let view: Clip = [0.0, top, w, h];
            let (iw, ih) = img_size(&img);
            let (x, y) = ((w - iw as f32) / 2.0, top + (h - top - ih as f32) / 2.0);
            self.gpu.draw_image(&img, x.round(), y.round(), iw as f32, ih as f32, 1.0, 0.0, view);
            // quiet it down a little, and darken toward the tiles so they read
            self.gpu.rect(0.0, top, w, h - top, [0, 0, 0, 38], 0.0, view);
            // bands on whole pixels that meet without overlapping, so no seams show
            let from = (top + (h - top) * 0.5).floor();
            let n = ((h - from) / 2.0).ceil() as i32;
            for i in 0..n {
                let y0 = from + (i * 2) as f32;
                let a = ((i as f32 + 1.0) / n as f32).powf(1.6) * 195.0;
                self.gpu.rect(0.0, y0, w, 2.0, [8, 8, 11, a as u8], 0.0, view);
            }
            return;
        }
        let k = self.scale;
        let top = TOP * k;
        let (w, h) = (self.w as f32, self.h as f32);
        let hh = h - top;
        let view: Clip = [0.0, top, w, h];
        let lerp = |a: Rgba, b: Rgba, t: f32| -> Rgba { [0, 1, 2, 3].map(|i| (a[i] as f32 + (b[i] as f32 - a[i] as f32) * t) as u8) };
        let sky = |t: f32| -> Rgba {
            let stops: [(f32, Rgba); 4] = [(0.0, [0x06, 0x06, 0x0c, 255]), (0.5, [0x12, 0x0c, 0x1c, 255]), (0.82, [0x2a, 0x13, 0x22, 255]), (1.0, [0x4a, 0x1d, 0x22, 255])];
            let mut c = stops[0].1;
            for p in stops.windows(2) {
                if t >= p[0].0 && t <= p[1].0 {
                    c = lerp(p[0].1, p[1].1, (t - p[0].0) / (p[1].0 - p[0].0));
                }
            }
            if t > 1.0 { stops[3].1 } else { c }
        };
        let horizon = top + hh * 0.70;
        let g = &mut self.gpu;
        // sky in bands
        let bands = 56;
        let band_h = (horizon - top) / bands as f32;
        for i in 0..bands {
            let y = top + i as f32 * band_h;
            g.rect(0.0, y.floor(), w, band_h.ceil() + 1.0, sky(i as f32 / (bands - 1) as f32), 0.0, view);
        }
        // stars, placed by a fixed hash so they never move
        let mut seed = 0x9e37_79b9u32;
        let mut rnd = || {
            seed ^= seed << 13;
            seed ^= seed >> 17;
            seed ^= seed << 5;
            (seed % 10_000) as f32 / 10_000.0
        };
        for _ in 0..140 {
            let (sx, sy, a) = (rnd() * w, top + rnd() * (horizon - top) * 0.62, rnd());
            let s = if a > 0.93 { 2.0 * k } else { k.max(1.0) };
            g.rect(sx.floor(), sy.floor(), s, s, [255, 240, 225, (40.0 + a * 150.0) as u8], 0.0, view);
        }
        // the sun: a vermilion disc with a soft glow, striped below its middle
        let (sx, sy, r) = (w * 0.70, top + hh * 0.42, hh * 0.19);
        for (grow, a) in [(2.1f32, 5u8), (1.85, 6), (1.65, 8), (1.48, 10), (1.34, 13), (1.22, 17), (1.12, 22), (1.05, 30)] {
            let rr = r * grow;
            g.rect(sx - rr, sy - rr, rr * 2.0, rr * 2.0, [0xff, 0x55, 0x30, a], rr, view);
        }
        g.rect(sx - r, sy - r, r * 2.0, r * 2.0, [0xee, 0x4b, 0x2b, 255], r, view);
        for s in 0..6 {
            let yy = sy + r * (0.12 + s as f32 * 0.155);
            let th = (1.0 + s as f32 * 1.2) * k;
            let t = ((yy - top) / (horizon - top)).clamp(0.0, 1.0);
            g.rect(sx - r - 2.0, yy, r * 2.0 + 4.0, th, sky(t), 0.0, [sx - r - 2.0, top, sx + r + 2.0, h]);
        }
        // the peak, with concave slopes and a ragged snow cap
        let (px, py) = (w * 0.34, top + hh * 0.26);
        let half = hh * 0.95;
        let step = (2.0 * k).max(2.0);
        let mut x = (px - half).max(0.0).floor();
        while x < (px + half).min(w) {
            // a flattened crater top, then long concave slopes
            let d = (((x - px).abs() / half - 0.035).max(0.0) / 0.965).min(1.0);
            let ytop = py + (horizon + hh * 0.04 - py) * d.powf(0.7);
            let shade = if x > px { [0x15, 0x12, 0x20, 255] } else { [0x1c, 0x18, 0x2a, 255] };
            g.rect(x, ytop, step, h - ytop, shade, 0.0, view);
            let jag = (x * 0.045 / k).sin() * 7.0 * k + (x * 0.11 / k).sin() * 4.0 * k;
            let snow = py + (horizon - py) * 0.24 + jag;
            if snow > ytop {
                let c = if x > px { [0xb8, 0xb0, 0xb6, 235] } else { [0xe2, 0xdc, 0xd6, 240] };
                g.rect(x, ytop, step, snow - ytop, c, 0.0, view);
            }
            x += step;
        }
        // two layers of hills
        for (i, (base, amp, col)) in [(0.66f32, 0.05f32, [0x10, 0x0e, 0x18, 255]), (0.74, 0.035, [0x0a, 0x09, 0x10, 255])].into_iter().enumerate() {
            let mut x = 0.0;
            while x < w {
                let t = x / k;
                let y = top + hh * base + hh * amp * ((t * 0.006 + i as f32 * 2.1).sin() * 0.7 + (t * 0.017 + i as f32).sin() * 0.3);
                g.rect(x, y, step, h - y, col, 0.0, view);
                x += step;
            }
        }
        // seigaiha: rows of overlapping scallops, each row covering the lower half of the last
        let band = top + hh * 0.80;
        g.rect(0.0, band, w, h - band, [0x0b, 0x0f, 0x21, 255], 0.0, view);
        let r = 26.0 * k;
        let mut row = 0;
        let mut cy = band;
        while cy - r < h {
            let off = if row % 2 == 1 { r } else { 0.0 };
            let mut cx = -r + off;
            while cx - r < w {
                let clip: Clip = [0.0_f32.max(cx - r), band.max(cy - r), w.min(cx + r), h.min(cy + 1.0)];
                g.rect(cx - r, cy - r, r * 2.0, r * 2.0, [0x0b, 0x0f, 0x21, 255], r, clip);
                for (j, f) in [0.92f32, 0.66, 0.40].into_iter().enumerate() {
                    let rr = r * f;
                    let a = [150u8, 110, 80][j];
                    g.ring(cx - rr, cy - rr, rr * 2.0, rr * 2.0, 1.6 * k, [0x3d, 0x56, 0x9a, a], rr, clip);
                }
                cx += r * 2.0;
            }
            cy += r * 0.5;
            row += 1;
        }
        // where the waves meet the land
        g.rect(0.0, band - 1.0 * k, w, 1.0 * k, [0xff, 0x6b, 0x1a, 60], 0.0, view);
    }

    /// A 1px bevel: light on top and dark below when raised, the other way when inset.
    fn bevel(&mut self, r: Rect, raised: bool) {
        let (hi, lo): (Rgba, Rgba) = ([255, 255, 255, 18], [0, 0, 0, 170]);
        let (top, bot) = if raised { (hi, lo) } else { (lo, hi) };
        self.rect(Rect::new(r.x + 3.0, r.y, r.w - 6.0, 1.0), top, 0.0);
        self.rect(Rect::new(r.x + 3.0, r.y + r.h - 1.0, r.w - 6.0, 1.0), bot, 0.0);
    }

    /// A status light; lit ones glow.
    fn led(&mut self, cx: f32, cy: f32, c: Rgba, on: bool) {
        if on {
            for (k, a) in [(7.0f32, 16u8), (5.0, 34), (3.6, 80)] {
                self.rect(Rect::new(cx - k, cy - k, 2.0 * k, 2.0 * k), [c[0], c[1], c[2], a], k);
            }
        }
        let core = if on { c } else { [0x34, 0x33, 0x36, 255] };
        self.rect(Rect::new(cx - 2.5, cy - 2.5, 5.0, 5.0), core, 2.5);
        if on {
            self.rect(Rect::new(cx - 1.0, cy - 1.8, 1.6, 1.2), [255, 255, 255, 150], 0.6);
        }
    }

    /// A square key, raised; lit when hovered.
    fn key_cap(&mut self, r: Rect, hover: bool) {
        self.rect(r, if hover { [0x22, 0x21, 0x26, 255] } else { [0x17, 0x17, 0x1c, 255] }, 5.0);
        self.bevel(r, true);
    }

    fn draw_chrome(&mut self) {
        let k = self.scale;
        let w = self.w as f32 / k;
        let h = self.h as f32 / k;
        self.hits.clear();
        // the panel, with faint scanlines across it
        self.rect(Rect::new(0.0, 0.0, w, TAB_H), BG, 0.0);
        self.rect(Rect::new(0.0, TAB_H, w, BAR_H), PANEL, 0.0);
        let mut y = 1.0;
        while y < TOP - 2.0 {
            self.rect(Rect::new(0.0, y, w, 1.0), [255, 255, 255, 4], 0.0);
            y += 3.0;
        }
        self.rect(Rect::new(0.0, TOP - 2.0, w, 1.0), [255, 255, 255, 8], 0.0);
        self.rect(Rect::new(0.0, TOP - 1.0, w, 1.0), [0, 0, 0, 255], 0.0);
        // the badge
        let badge = Rect::new(10.0, 8.0, 26.0, 26.0);
        self.rect(badge, ACCENT, 5.0);
        self.bevel(badge, true);
        // a blade cut, in pixels
        for i in 0..8 {
            let f = i as f32;
            self.rect(Rect::new(badge.x + 6.0 + f * 2.0, badge.y + 19.0 - f * 2.0, 3.0, 3.0), BG, 0.0);
        }
        // tabs, numbered like slots
        let n = self.tabs.len();
        let x0 = 44.0;
        let tw = ((w - x0 - 44.0) / n as f32).clamp(70.0, 236.0);
        for i in 0..n {
            let x = x0 + i as f32 * tw;
            let r = Rect::new(x, 7.0, tw - 6.0, TAB_H - 7.0);
            let active = i == self.cur;
            let hov = self.hover == Hit::Tab(i) || self.hover == Hit::Close(i);
            if active {
                self.rect(Rect::new(r.x, r.y, r.w, r.h + 8.0), PANEL, 6.0);
                self.rect(Rect::new(r.x + 5.0, r.y, r.w - 10.0, 1.0), [255, 255, 255, 26], 0.0);
            } else if hov {
                self.rect(Rect::new(r.x, r.y + 2.0, r.w, r.h - 6.0), [0x13, 0x13, 0x17, 255], 6.0);
            }
            self.hits.push((r, Hit::Tab(i)));
            let (title, loading, parked, full) = {
                let t = &self.tabs[i];
                (t.title.clone(), t.loading, t.parked, t.full_mode)
            };
            let cy = r.y + r.h / 2.0 - 1.0;
            let (lc, on) = if loading {
                (ACCENT, true)
            } else if parked {
                (FAINT, false)
            } else if full {
                (CYAN, active)
            } else {
                (PHOS, active)
            };
            self.led(r.x + 13.0, cy, lc, on);
            let num = format!("{:02}", i + 1);
            self.label(&num, r.x + 23.0, cy - 7.5, 10.5, 500, if active { PHOS_DIM } else { FAINT }, 20.0, MONO);
            let show_close = active || hov;
            let tmax = r.w - 52.0 - if show_close { 22.0 } else { 0.0 };
            self.label(&title, r.x + 44.0, cy - 9.0, 12.5, if active { 500 } else { 400 }, if active { TEXT } else { MUTED }, tmax.max(0.0), UI_FONT);
            if show_close && tw > 96.0 {
                let cr = Rect::new(r.x + r.w - 27.0, cy - 10.0, 20.0, 20.0);
                if self.hover == Hit::Close(i) {
                    self.rect(cr, HOVER, 5.0);
                }
                self.icon('\u{E8BB}', cr.x + 10.0, cr.y + 10.0, 8.5, MUTED);
                self.hits.push((cr, Hit::Close(i)));
            }
        }
        let nr = Rect::new(x0 + n as f32 * tw + 2.0, 10.0, 24.0, 24.0);
        let nh = self.hover == Hit::NewTab;
        self.key_cap(nr, nh);
        self.icon('\u{E710}', nr.x + 12.0, nr.y + 12.0, 11.0, if nh { TEXT } else { MUTED });
        self.hits.push((nr, Hit::NewTab));
        // navigation keys
        let (can_back, can_fwd, loading, full_mode) = {
            let t = self.tab();
            (!t.back.is_empty() || t.full.as_ref().is_some_and(|f| full::can(&f.wv).0), !t.fwd.is_empty() || t.full.as_ref().is_some_and(|f| full::can(&f.wv).1), t.loading, t.full_mode)
        };
        let buttons = [(Hit::Back, '\u{E72B}', can_back), (Hit::Fwd, '\u{E72A}', can_fwd), (Hit::Reload, if loading { '\u{E711}' } else { '\u{E72C}' }, true)];
        for (i, (hit, glyph, on)) in buttons.into_iter().enumerate() {
            let r = Rect::new(10.0 + i as f32 * 34.0, TAB_H + 8.0, 30.0, 30.0);
            let hv = self.hover == hit && on;
            if hv {
                self.key_cap(r, true);
            }
            self.icon(glyph, r.x + 15.0, r.y + 15.0, 12.5, if !on { FAINT } else if hv { TEXT } else { MUTED });
            self.hits.push((r, hit));
        }
        // right side, laid out from the edge: memory meter, then the engine switch
        let budget: f32 = std::env::var("HAYATE_BUDGET_MB").ok().and_then(|v| v.parse().ok()).unwrap_or(500.0);
        let mine = private_mb();
        let total = mine + self.full_mb;
        let segs = 12;
        let seg_w = 4.0;
        let meter_w = segs as f32 * (seg_w + 2.0);
        let mut rx = w - 14.0 - meter_w;
        let lit = ((total / budget) * segs as f32).ceil().clamp(1.0, segs as f32) as usize;
        for s in 0..segs {
            let c = if s < lit {
                if s >= segs - 2 { [0xff, 0x45, 0x3a, 255] } else if s >= segs - 4 { [0xff, 0xb0, 0x3a, 255] } else { PHOS }
            } else {
                [0x24, 0x23, 0x28, 255]
            };
            self.rect(Rect::new(rx + s as f32 * (seg_w + 2.0), TAB_H + 15.0, seg_w, 16.0), c, 1.0);
        }
        let val = if self.full_mb > 0.5 { format!("{:.0}+{:.0}", mine, self.full_mb) } else { format!("{mine:.1}") };
        let (_, vw) = self.ui_text(&val, 13.0, 500, MONO, PHOS);
        let (_, mbw) = self.ui_text("MB", 9.5, 500, MONO, FAINT);
        rx -= mbw + 6.0;
        self.label("MB", rx, TAB_H + 19.5, 9.5, 500, PHOS_DIM, mbw + 2.0, MONO);
        rx -= vw + 4.0;
        self.label(&val, rx, TAB_H + 15.0, 13.0, 500, PHOS, vw + 2.0, MONO);
        let (_, memw) = self.ui_text("MEM", 9.5, 500, MONO, FAINT);
        rx -= memw + 8.0;
        self.label("MEM", rx, TAB_H + 19.5, 9.5, 500, FAINT, memw + 2.0, MONO);
        let on_page = !matches!(self.tab().url.as_str(), START | "");
        // what the blocker stopped here
        let blocked = self.tab().blocked;
        if on_page && blocked > 0 {
            let s = format!("{blocked}");
            let (_, bw) = self.ui_text(&s, 12.0, 600, MONO, PHOS);
            rx -= bw + 4.0;
            self.label(&s, rx, TAB_H + 16.0, 12.0, 600, [0xff, 0x8a, 0x80, 255], bw + 2.0, MONO);
            let (_, lw) = self.ui_text("BLOCKED", 9.5, 500, MONO, FAINT);
            rx -= lw + 6.0;
            self.label("BLOCKED", rx, TAB_H + 19.5, 9.5, 500, FAINT, lw + 2.0, MONO);
            self.led(rx - 9.0, TAB_H + 23.0, [0xff, 0x45, 0x3a, 255], true);
            rx -= 22.0;
        }
        let sw_w = 104.0;
        rx -= sw_w + 16.0;
        if on_page {
            let sr = Rect::new(rx, TAB_H + 11.0, sw_w, 24.0);
            self.rect(sr, [0x07, 0x07, 0x09, 255], 6.0);
            self.bevel(sr, false);
            let half = sw_w / 2.0;
            let seg = if full_mode { Rect::new(sr.x + half, sr.y + 2.0, half - 2.0, 20.0) } else { Rect::new(sr.x + 2.0, sr.y + 2.0, half - 2.0, 20.0) };
            self.rect(seg, if self.hover == Hit::Engine { [0x26, 0x25, 0x2b, 255] } else { [0x1b, 0x1b, 0x21, 255] }, 4.5);
            self.bevel(seg, true);
            self.led(sr.x + 11.0, sr.y + 12.0, PHOS, !full_mode);
            self.label("LITE", sr.x + 19.0, sr.y + 5.0, 10.5, 600, if full_mode { FAINT } else { TEXT }, 34.0, MONO);
            self.led(sr.x + half + 10.0, sr.y + 12.0, CYAN, full_mode);
            self.label("FULL", sr.x + half + 18.0, sr.y + 5.0, 10.5, 600, if full_mode { TEXT } else { FAINT }, 34.0, MONO);
            self.hits.push((sr, Hit::Engine));
        }
        // the address display: an inset lcd
        let ax = 120.0;
        let aw = (rx - ax - 12.0).max(80.0);
        let ar = Rect::new(ax, TAB_H + 8.0, aw, 30.0);
        let focused = self.edit.focused;
        self.rect(ar, [0x06, 0x06, 0x08, 255], 6.0);
        self.bevel(ar, false);
        if focused {
            for (g, a) in [(3.0f32, 26u8), (1.5, 60)] {
                let kk = self.scale;
                self.gpu.ring((ar.x - g) * kk, (ar.y - g) * kk, (ar.w + 2.0 * g) * kk, (ar.h + 2.0 * g) * kk, 1.0 * kk, [0xff, 0x9a, 0x4a, a], (6.0 + g) * kk, gpu::NO_CLIP);
            }
            let kk = self.scale;
            self.gpu.ring(ar.x * kk, ar.y * kk, ar.w * kk, ar.h * kk, 1.0 * kk, [0xff, 0x9a, 0x4a, 200], 6.0 * kk, gpu::NO_CLIP);
        } else if self.hover == Hit::Addr {
            let kk = self.scale;
            self.gpu.ring(ar.x * kk, ar.y * kk, ar.w * kk, ar.h * kk, 1.0 * kk, [255, 255, 255, 22], 6.0 * kk, gpu::NO_CLIP);
        }
        self.hits.push((ar, Hit::Addr));
        let ty = ar.y + 7.5;
        if focused {
            let s: String = self.edit.text.iter().collect();
            let before: String = self.edit.text[..self.edit.caret].iter().collect();
            let (_, bw) = self.ui_text(&before, 13.0, 400, MONO, TEXT);
            let (_, all_w) = self.ui_text(&s, 13.0, 400, MONO, TEXT);
            if self.edit.all && !s.is_empty() {
                self.rect(Rect::new(ar.x + 13.0, ar.y + 6.0, all_w + 2.0, 18.0), [0xff, 0x8a, 0x3a, 64], 2.0);
            }
            if s.is_empty() {
                self.label("SEARCH OR TYPE AN ADDRESS", ar.x + 14.0, ty + 1.0, 11.5, 500, FAINT, aw - 28.0, MONO);
            } else {
                self.label(&s, ar.x + 14.0, ty, 13.0, 400, TEXT, aw - 28.0, MONO);
            }
            if !self.edit.all {
                self.rect(Rect::new(ar.x + 14.0 + bw, ar.y + 6.0, 7.5, 18.0), [0xff, 0x9a, 0x4a, 150], 1.0);
            }
        } else {
            let url = self.tab().url.clone();
            if url == START || url.is_empty() {
                self.label("SEARCH OR TYPE AN ADDRESS", ar.x + 14.0, ty + 1.0, 11.5, 500, FAINT, aw - 28.0, MONO);
            } else {
                let rest = url.trim_start_matches("https://");
                let (host, path) = match rest.find('/') {
                    Some(p) => (&rest[..p], &rest[p..]),
                    None => (rest, ""),
                };
                let host = host.trim_start_matches("www.").to_string();
                let path = if path == "/" { String::new() } else { path.to_string() };
                self.icon('\u{E72E}', ar.x + 16.0, ar.y + 15.0, 10.5, PHOS_DIM);
                let hw = self.label(&host, ar.x + 30.0, ty, 13.0, 500, PHOS, aw - 44.0, MONO);
                self.label(&path, ar.x + 30.0 + hw, ty, 13.0, 400, PHOS_DIM, (aw - 44.0 - hw).max(0.0), MONO);
            }
        }
        // loading: a row of lit segments under the panel
        if loading {
            let prog = if self.tab().dl.is_some() || self.tab().full.is_some() { 0.72 } else { 0.28 };
            let mut x = 0.0;
            while x < w * prog {
                self.rect(Rect::new(x, TOP - 2.0, 9.0, 2.0), ACCENT, 0.0);
                x += 12.0;
            }
        }
        // where a link goes, on a small display
        if let Some(l) = self.hover_link.clone() {
            let s = l.trim_start_matches("https://").to_string();
            let (_, lw) = self.ui_text(&s, 11.5, 400, MONO, PHOS);
            let bw = (lw + 22.0).min(w * 0.6);
            let r = Rect::new(8.0, h - 32.0, bw, 24.0);
            self.rect(r, [0x08, 0x08, 0x0a, 245], 5.0);
            self.bevel(r, true);
            self.label(&s, r.x + 11.0, r.y + 4.5, 11.5, 400, PHOS, bw - 22.0, MONO);
        }
    }
}

fn img_size(i: &paint::Image) -> (u32, u32) {
    match i {
        paint::Image::Gpu(g) => (g.w, g.h),
        paint::Image::Soft(s) => (s.w, s.h),
    }
}

fn write_bmp(path: &str, w: u32, h: u32, bgra: &[u8]) -> std::io::Result<()> {
    let mut f = Vec::with_capacity(54 + bgra.len());
    let size = 54 + bgra.len() as u32;
    f.extend_from_slice(b"BM");
    f.extend_from_slice(&size.to_le_bytes());
    f.extend_from_slice(&0u32.to_le_bytes());
    f.extend_from_slice(&54u32.to_le_bytes());
    f.extend_from_slice(&40u32.to_le_bytes());
    f.extend_from_slice(&(w as i32).to_le_bytes());
    f.extend_from_slice(&(-(h as i32)).to_le_bytes());
    f.extend_from_slice(&1u16.to_le_bytes());
    f.extend_from_slice(&32u16.to_le_bytes());
    f.extend_from_slice(&[0u8; 24]);
    f.extend_from_slice(bgra);
    std::fs::write(path, f)
}
