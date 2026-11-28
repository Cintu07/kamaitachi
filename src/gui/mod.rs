//! The window browser: tabs, an address bar, and pages drawn by hayate's own engine on
//! the gpu. Pages load on worker threads (fetch, cascade, layout, images) and arrive as
//! display lists; the window thread only draws, scrolls and handles input, and sleeps
//! when nothing moves.

pub mod brush;
mod drive;
mod fields;
mod find;
mod menu;
mod pick;
mod places;
mod prefs;
mod settings;
mod shell;
mod tabs;
pub mod full;
pub mod gpu;
pub mod image;
pub mod paint;
pub mod skin;
pub mod soft;
pub mod text;

use crate::edit::Edit;
use crate::net::cookies;
use crate::url::Url;
use crate::web::layout::{self, Images};
use crate::web::load;
use crate::web::style::{media_key, Cascade, Styles};
use crate::web::{DisplayList, Font, Item, Para, Rect, Rgba, Run, TextEngine};
use webview2_com::Microsoft::Web::WebView2::Win32::{ICoreWebView2, ICoreWebView2Controller, ICoreWebView2Environment};
use fields::{Focus, Menu, Mods};
use gpu::Clip;
use paint::Painter;
use skin::sk;
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

const START: &str = "hayate:start";
const SETTINGS: &str = "https://kamaitachi.local/settings";

const MONO: &str = "Cascadia Mono, Consolas, monospace";
/// the browser's name: kamaitachi, the sickle-weasel that rides whirlwinds
const BRAND: &str = "kamaitachi";
const UI_FONT: &str = "Instrument Sans, Segoe UI Variable Text, Segoe UI, sans-serif";
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
    NeedsJs { tab: u64, seq: u64, url: String, learn: bool },
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
    /// the page was made here (settings, a warning), so its links may change settings
    own: bool,
    /// back from last session and not loaded yet; it loads when it is shown
    asleep: bool,
    muted: bool,
    /// showing the reading view of `url`
    reading: bool,
    /// kept at the front of the row and drawn as a square at the top of the column
    pinned: bool,
    /// a card over another tab, opened with Shift+click; not in the column
    peek: bool,
}

impl Tab {
    fn new(id: u64) -> Tab {
        Tab {
            id,
            url: String::new(),
            title: "New tab".into(),
            page: None,
            dl: None,
            scroll: 0.0,
            target: 0.0,
            back: Vec::new(),
            fwd: Vec::new(),
            loading: false,
            seq: 0,
            tex: HashMap::new(),
            restore: None,
            vw: 0.0,
            full_mode: false,
            full: None,
            pending: None,
            requested: false,
            parked: false,
            picture: None,
            picture_img: None,
            shot_at: None,
            used: Instant::now(),
            blocked: 0,
            own: false,
            asleep: false,
            muted: false,
            reading: false,
            pinned: false,
            peek: false,
        }
    }
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
    Settings,
    Mute(usize),
    /// a line under the address field
    Suggest(usize),
    /// the address field's panel, which a click inside keeps open
    Panel,
    /// the star in the address: bookmark the page
    Star,
    /// one of the browser's own pages, from the foot of the column
    Page(&'static str),
    PeekClose,
    PeekKeep,
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
    /// the address being typed
    edit: Edit,
    addr_focused: bool,
    /// the control of a page being used, and the list it has open
    focus: Option<Focus>,
    menu: Option<Menu>,
    /// tabs closed, the last one last
    closed: Vec<tabs::Closed>,
    picking: Option<pick::Pick>,
    find: Option<find::Find>,
    /// a right-click menu
    tab_menu: Option<menu::Menu>,
    /// the tab a peek is over
    peek_under: Option<u64>,
    /// the column is out over the page, brought by the mouse at the edge
    col_peek: bool,
    /// the mouse is over a box to type in, or something to press
    over: Option<bool>,
    /// the first half of a character that arrives as two messages, an emoji say
    half: u16,
    last_click: (Instant, f32, f32),
    relayout_due: bool,
    shot: Option<(String, u32)>,
    /// what a test render does to the page first
    steps: Vec<String>,
    driving: bool,
    drove: bool,
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
    /// a short note at the foot of the page, for a few seconds
    toast: Option<(String, Instant)>,
    /// the column of tabs is put away (Ctrl+S)
    column_hidden: bool,
    /// the first tab the column shows, when there are more than fit
    col_scroll: usize,
    /// Ctrl is down, so the tabs show their numbers
    ctrl_held: bool,
    /// the line picked under the address field
    addr_pick: usize,
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
        if std::env::var_os("HAYATE_SHOT").is_some() {
            test_profile();
        }
        prefs::load();
        crate::web::block::start();
        load_cookies();
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        let inst = GetModuleHandleW(None).map_err(|e| e.to_string())?;
        let class = w!("hayate.window");
        let wc = WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            lpfnWndProc: Some(wndproc),
            hInstance: inst.into(),
            hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
            // icon 1 is the weasel seal build.rs puts in kamaitachi.exe
            hIcon: LoadIconW(Some(inst.into()), PCWSTR(1 as *const u16)).unwrap_or_default(),
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
            addr_focused: false,
            focus: None,
            menu: None,
            closed: Vec::new(),
            picking: None,
            find: None,
            tab_menu: None,
            peek_under: None,
            col_peek: false,
            over: None,
            half: 0,
            last_click: (Instant::now(), -100.0, -100.0),
            relayout_due: false,
            shot,
            steps: drive::steps(),
            driving: false,
            drove: false,
            want: ((ww as f32 * scale) as i32, (wh as f32 * scale) as i32),
            started: Instant::now(),
            env: None,
            env_state: 0,
            waiting: Vec::new(),
            engines: load_engines(),
            full_mb: 0.0,
            wall: None,
            wall_checked: false,
            toast: None,
            column_hidden: false,
            col_scroll: 0,
            ctrl_held: false,
            addr_pick: 0,
        });
        APP = Box::into_raw(app);
        let app = &mut *APP;
        skin::glass_possible(matches!(app.gpu, Painter::Soft(_)));
        app.zoom = prefs::get().zoom as f32 / 100.0;
        app.apply_prefs();
        app.sync_size();
        // a test render starts from nothing; otherwise last session's tabs come back
        let restored = (app.shot.is_none() || std::env::var_os("HAYATE_RESTORE").is_some()) && prefs::get().restore && app.restore_session();
        if let Some(u) = &url {
            app.new_tab(u);
            for u in urls.iter().skip(1) {
                app.navigate(u, true);
            }
        } else if !restored {
            app.new_tab(START);
            app.focus_addr();
        }
        app.update_views();
        SetTimer(Some(hwnd), 6, 60_000, None);
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
                app.save_session();
                save_cookies();
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
                if wp.0 == 4 {
                    if app.toast.as_ref().is_none_or(|t| t.1.elapsed().as_secs() >= 4) {
                        app.toast = None;
                        let _ = KillTimer(Some(hwnd), 4);
                    }
                    app.render();
                    return LRESULT(0);
                }
                if wp.0 == 2 {
                    app.measure_full();
                    return LRESULT(0);
                }
                if wp.0 == 6 {
                    app.govern();
                    return LRESULT(0);
                }
                if wp.0 == 5 {
                    let _ = KillTimer(Some(hwnd), 5);
                    app.save_session();
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
                    let c = if app.over == Some(true) {
                        IDC_IBEAM
                    } else if app.over == Some(false) || app.hover_link.is_some() || matches!(app.hover, Hit::Tab(_) | Hit::Close(_) | Hit::NewTab | Hit::Back | Hit::Fwd | Hit::Reload | Hit::Engine | Hit::Settings | Hit::Mute(_) | Hit::Suggest(_) | Hit::Star | Hit::Page(_)) {
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
                app.click(lo(lp), hi(lp), Mods { ctrl: key_down(VK_CONTROL), shift: key_down(VK_SHIFT), alt: false });
                LRESULT(0)
            }
            WM_RBUTTONDOWN => {
                if app.context(lo(lp), hi(lp)) {
                    return LRESULT(0);
                }
                DefWindowProcW(hwnd, msg, wp, lp)
            }
            WM_LBUTTONUP => {
                app.drop_field();
                LRESULT(0)
            }
            WM_MBUTTONDOWN => {
                app.mouse_move(lo(lp), hi(lp));
                if let Some(l) = app.hover_link.clone().filter(|l| !l.starts_with(layout::PRESS)) {
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
                let (mx, my) = app.mouse;
                if app.pick_wheel(d > 0.0) {
                } else if app.column_box().contains(mx / app.scale, my / app.scale) {
                    app.col_scroll = if d > 0.0 { app.col_scroll.saturating_sub(2) } else { app.col_scroll + 2 };
                    app.render();
                } else if app.scroll_menu(-d as i32 * 3) {
                    app.render();
                } else if key_down(VK_CONTROL) {
                    app.set_zoom(app.zoom * if d > 0.0 { 1.1 } else { 1.0 / 1.1 });
                } else {
                    app.scroll_by(-d * 120.0);
                }
                LRESULT(0)
            }
            WM_KEYUP if VIRTUAL_KEY(wp.0 as u16) == VK_CONTROL => {
                app.ctrl_held = false;
                app.render();
                DefWindowProcW(hwnd, msg, wp, lp)
            }
            WM_SETTINGCHANGE => {
                // Windows went light or dark
                app.apply_prefs();
                app.render();
                DefWindowProcW(hwnd, msg, wp, lp)
            }
            WM_KEYDOWN | WM_SYSKEYDOWN => {
                if VIRTUAL_KEY(wp.0 as u16) == VK_CONTROL && !app.ctrl_held {
                    app.ctrl_held = true;
                    app.render();
                }
                let m = Mods { ctrl: key_down(VK_CONTROL), shift: key_down(VK_SHIFT), alt: msg == WM_SYSKEYDOWN };
                if app.key(VIRTUAL_KEY(wp.0 as u16), m) {
                    app.render();
                    return LRESULT(0);
                }
                DefWindowProcW(hwnd, msg, wp, lp)
            }
            WM_CHAR => {
                let unit = wp.0 as u16;
                let pair = [std::mem::take(&mut app.half), unit];
                if (0xD800..0xDC00).contains(&unit) {
                    app.half = unit;
                    return LRESULT(0);
                }
                let units = if pair[0] != 0 { &pair[..] } else { &pair[1..] };
                for c in char::decode_utf16(units.iter().copied()).flatten().filter(|c| !c.is_control()) {
                    app.type_char(c);
                }
                LRESULT(0)
            }
            _ => DefWindowProcW(hwnd, msg, wp, lp),
        }
    }
}

/// Points a test render at a profile of its own in %TEMP%, so nothing it does (cookies,
/// sites it learns need Chromium, hidden boxes, counts) lands in the real one. It keeps
/// reading the real filter lists, which are slow to download. This runs before any
/// other thread exists, and Windows guards its environment block with a lock anyway.
fn test_profile() {
    crate::web::block::set_dir(wall_dir().join("filters"));
    let test = std::env::temp_dir().join("kamaitachi-test");
    let _ = std::fs::create_dir_all(test.join("hayate"));
    unsafe { std::env::set_var("APPDATA", &test) };
}

/// Where sign-ins are kept between runs, readable only by this Windows user.
fn cookie_file() -> std::path::PathBuf {
    wall_dir().join("cookies")
}

fn load_cookies() {
    if let Some(b) = std::fs::read(cookie_file()).ok().and_then(|b| crate::sys::unseal(&b)) {
        cookies::restore(&String::from_utf8_lossy(&b));
    }
}

fn save_cookies() {
    if let Some(text) = cookies::unsaved()
        && let Some(b) = crate::sys::seal(text.as_bytes())
    {
        let _ = std::fs::write(cookie_file(), b);
    }
}

fn private_mb() -> f32 {
    crate::sys::process_memory().private_bytes as f32 / (1024.0 * 1024.0)
}

/// The full pipeline for one page, on a worker thread.
fn load_page(req: load::Request, vw: f32, vh: f32, dpr: f32, tab: u64, seq: u64, shared: Arc<Shared>) {
    let url = if req.url.host == "hayate" { START.to_string() } else { req.url.full() };
    let r: Result<(), String> = (|| {
        if url != START && crate::web::block::is_danger(&url) {
            shared.post(Msg::Danger { tab, seq, url: url.clone() });
            return Ok(());
        }
        let doc = if url == START {
            load::from_html(START_HTML, Url { host: "hayate".into(), path: "/start".into() }, 1)
        } else {
            load::load(&req)?
        };
        // a link that looked fine can redirect to one that is not
        if url != START && crate::web::block::is_danger(&doc.url.full()) {
            shared.post(Msg::Danger { tab, seq, url: doc.url.full() });
            return Ok(());
        }
        let mut dw = Dw::new();
        let styles = Cascade { dom: &doc.dom, sheets: &doc.sheets, vw, vh }.run();
        let mut images = Images::default();
        let dl = layout::layout(&doc.dom, &styles, &mut dw, &mut images, &doc.base, vw, vh);
        let final_url = if url == START { START.to_string() } else { doc.url.full() };
        let check = url != START && load::bot_check(&doc, &dl);
        // a page you hid most of is not a page that needs JavaScript
        let trimmed = crate::web::hide::count(&doc.url.host) > 0;
        if check || (url != START && !trimmed && load::needs_js(&doc, &dl)) {
            shared.post(Msg::NeedsJs { tab, seq, url: final_url, learn: !check });
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
        let bytes = fetch_images(&urls, &doc.url.host);
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

fn fetch_images(urls: &[String], page_host: &str) -> Vec<Option<Vec<u8>>> {
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
                    if let Ok(b) = load::fetch_bytes(u, "image/avif,image/webp,image/png,image/jpeg,image/*;q=0.8", page_host)
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

fn hex(c: Rgba) -> String {
    format!("#{:02x}{:02x}{:02x}", c[0], c[1], c[2])
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
        self.page_box().w * self.scale / self.k()
    }

    fn view_h(&self) -> f32 {
        (self.page_box().h * self.scale).max(1.0) / self.k()
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
        self.tabs.push(Tab::new(id));
        self.cur = self.tabs.len() - 1;
        self.load(self.cur, url.to_string(), false);
    }

    fn open_background(&mut self, url: &str) {
        let keep = self.cur;
        self.new_tab(url);
        if prefs::get().beside && self.cur != keep + 1 {
            let t = self.tabs.pop().expect("the tab just made");
            self.tabs.insert(keep + 1, t);
            // the page loading for it knows the tab by its id, so moving it is safe
        }
        self.cur = keep;
        self.session_changed();
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
        self.remember_closed(i);
        self.tabs.remove(i);
        if self.tabs.is_empty() {
            self.new_tab(START);
            self.focus_addr();
            return;
        }
        if self.cur == i && prefs::get().close_goes_back {
            self.cur = (0..self.tabs.len()).min_by_key(|&j| self.tabs[j].used.elapsed()).unwrap_or(0);
        } else if self.cur >= self.tabs.len() || self.cur > i {
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
        t.own = true;
        // kept, so its boxes and lists work and it is laid out again when the window changes
        t.page = Some(Box::new(Page { doc, styles, images }));
        t.vw = vw;
        t.loading = false;
        t.scroll = 0.0;
        t.target = 0.0;
        self.update_title();
        if let Some((_, n)) = &mut self.shot {
            *n = 1;
            unsafe {
                let _ = PostMessageW(Some(self.hwnd), WM_APP, WPARAM(0), LPARAM(0));
            }
        }
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
        let b = self.page_box();
        let k = self.scale;
        let r = RECT { left: (b.x * k) as i32, top: (b.y * k) as i32, right: ((b.x + b.w) * k) as i32, bottom: ((b.y + b.h) * k) as i32 };
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
        self.wake_asleep();
        self.session_changed();
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
        let p = prefs::get();
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
        let rest = p.sleep_after as u64 * 60;
        if rest > 0 {
            for (i, used) in idle {
                if used.elapsed().as_secs() > rest {
                    self.park(i);
                }
            }
            for i in 0..self.tabs.len() {
                if i != cur && self.tabs[i].used.elapsed().as_secs() > rest {
                    self.sleep_lite(i);
                }
            }
        }
        let gone = p.close_after as u64 * 3600;
        if gone > 0 {
            let old: Vec<u64> = self.tabs.iter().enumerate().filter(|(i, t)| *i != cur && !t.pinned && !t.peek && t.used.elapsed().as_secs() > gone).map(|(_, t)| t.id).collect();
            for id in old {
                if let Some(i) = self.tabs.iter().position(|t| t.id == id) {
                    self.close_tab(i);
                }
            }
        }
    }

    /// Lets a light-engine tab's page go: its document, layout and pictures. It loads
    /// again, at the same place, when you go back to it.
    fn sleep_lite(&mut self, i: usize) {
        let t = &mut self.tabs[i];
        if t.full_mode || t.asleep || t.loading || t.page.is_none() || t.url == START || t.url.starts_with("https://kamaitachi.local/") {
            return;
        }
        t.restore = Some(t.scroll);
        t.page = None;
        t.dl = None;
        t.tex = HashMap::new();
        t.asleep = true;
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
        let t = &mut self.tabs[i];
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
                            if self.tabs[i].muted {
                                full::mute(&wv, true);
                            }
                            self.tabs[i].full = Some(FullView { ctl, wv });
                            self.tabs[i].requested = false;
                            self.tabs[i].parked = false;
                            self.tabs[i].picture_img = None;
                            self.update_views();
                            if i == self.cur && !self.addr_focused
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
                        places::visit(&t.url, &title);
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
                    self.key(VIRTUAL_KEY(vk as u16), Mods { ctrl: key_down(VK_CONTROL), shift: key_down(VK_SHIFT), alt });
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
        self.load_from(i, url, push, String::new());
    }

    /// The same, for a link on a page of the site `from`.
    fn load_from(&mut self, i: usize, url: String, push: bool, from: String) {
        let parsed = if url == START { Some(Url { host: "hayate".into(), path: "/start".into() }) } else { Url::parse(&url) };
        match parsed {
            Some(u) => self.open(i, load::Request { url: u, body: None, from }, push),
            None => self.say("that is not an address"),
        }
    }

    /// Opens a page in tab `i`: an address, a link, or the answer to a form.
    fn open(&mut self, i: usize, req: load::Request, push: bool) {
        if i == self.cur {
            self.focus = None;
            self.menu = None;
            self.picking = None;
            self.find = None;
        }
        self.tabs[i].reading = false;
        self.tabs[i].asleep = false;
        self.session_changed();
        let mut req = req;
        if prefs::get().clean_links && req.url.host != "kamaitachi.local" {
            req.url = req.url.without_trackers();
        }
        let url = if req.url.host == "hayate" { START.to_string() } else { req.url.full() };
        if let Some(rest) = url.strip_prefix("https://kamaitachi.local/") {
            // another site's page may not change settings, with a link or a form
            if !req.from.is_empty() && req.from != "kamaitachi.local" {
                return;
            }
            if let Some(q) = rest.strip_prefix("settings") {
                let find = q.strip_prefix("?q=").map(crate::url::percent_decode).unwrap_or_default();
                let section = q.strip_prefix("?s=").unwrap_or("look").to_string();
                return self.show_settings(i, &find, &section, push);
            }
            if let Some(q) = rest.strip_prefix("set?") {
                let section = self.take_setting(q);
                return self.show_settings(i, "", &section, false);
            }
            for name in ["history", "bookmarks"] {
                if let Some(q) = rest.strip_prefix(name) {
                    let find = q.strip_prefix("?q=").map(crate::url::percent_decode).unwrap_or_default();
                    return self.show_place(i, name, &find, push);
                }
            }
            if let Some(u) = rest.strip_prefix("unmark?u=") {
                places::toggle_mark(&crate::url::percent_decode(u), "");
                return self.show_place(i, "bookmarks", "", false);
            }
            if rest == "clearhistory" {
                places::clear_history();
                self.say("history cleared");
                return self.show_place(i, "history", "", false);
            }
            if rest == "forget" {
                cookies::forget();
                save_cookies();
                self.say("signed out of every site");
                return self.show_settings(i, "", "privacy", false);
            }
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
        // a form's answer is opened here: the full engine has no way to be handed one
        if req.body.is_none() && self.wants_full(&url) {
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
        let _ = std::thread::Builder::new().stack_size(64 << 20).name("page".into()).spawn(move || load_page(req, vw, vh, dpr, id, seq, shared));
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
        self.addr_focused = false;
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
                    t.own = false;
                    t.tex.clear();
                    t.page = None;
                    let y = t.restore.take().unwrap_or(0.0);
                    t.scroll = y;
                    t.target = y;
                }
                Msg::Done { tab, seq, page, dl, pixels, blocked } => {
                    if let Some(t) = self.tabs.iter().find(|t| t.id == tab && t.seq == seq) {
                        places::visit(&t.url, &dl.title);
                    }
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
                    save_cookies();
                    if i == self.cur {
                        self.autofocus();
                    }
                    let t = &mut self.tabs[i];
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
                Msg::NeedsJs { tab, seq, url, learn } => {
                    let Some(i) = self.tabs.iter().position(|t| t.id == tab && t.seq == seq) else { continue };
                    // learn it, so the next visit goes straight to the full engine
                    if learn {
                        self.engines.insert(host_of(&url), true);
                        self.save_engines();
                    }
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
        if !self.relayout_due || self.tabs.is_empty() {
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
        // the matches moved with the text
        if self.find.is_some() {
            self.open_find();
        }
    }

    // ---- input ----

    fn focus_addr(&mut self) {
        unsafe {
            let _ = windows::Win32::UI::Input::KeyboardAndMouse::SetFocus(Some(self.hwnd));
        }
        let url = self.tab().url.clone();
        let shown = if url == START { String::new() } else { url };
        self.blur();
        self.edit = Edit::new(&shown);
        self.edit.select_all();
        self.addr_focused = true;
    }

    /// A character from the keyboard.
    fn type_char(&mut self, c: char) {
        if self.menu_char(c) || (!self.addr_focused && self.find_char(c)) {
        } else if self.addr_focused {
            self.edit.insert(&c.to_string());
            self.addr_pick = 0;
        } else if self.typing() {
            self.type_text(&c.to_string());
        } else {
            return;
        }
        self.render();
    }

    /// The text being edited, in the address bar or on the page.
    fn editing(&mut self) -> Option<&mut Edit> {
        if self.addr_focused { Some(&mut self.edit) } else { self.focused_edit() }
    }

    fn paste(&mut self) {
        let s = self.clipboard_text();
        if self.addr_focused {
            self.edit.insert(s.trim());
        } else if !self.find_paste(&s) {
            self.type_text(&s);
        }
    }

    /// The text on the clipboard, or nothing.
    fn clipboard_text(&self) -> String {
        unsafe {
            if OpenClipboard(Some(self.hwnd)).is_err() {
                return String::new();
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
        }
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

    /// Copy, cut and paste in whatever is being edited.
    fn clipboard_key(&mut self, vk: VIRTUAL_KEY) -> bool {
        // a password can be pasted in but not copied out
        if vk == VK_V && !self.addr_focused && self.find.is_some() {
            self.paste();
            return true;
        }
        let hidden = !self.addr_focused && self.password();
        let Some(e) = self.editing() else { return false };
        let picked = e.selected();
        match vk {
            VK_V => self.paste(),
            VK_C | VK_X if picked.is_empty() || hidden => {}
            VK_C => self.copy(&picked),
            VK_X => {
                e.insert("");
                self.copy(&picked);
                self.keep_value();
            }
            _ => return false,
        }
        true
    }

    /// Returns true when the key was used.
    fn key(&mut self, vk: VIRTUAL_KEY, m: Mods) -> bool {
        let Mods { ctrl, shift, alt } = m;
        if vk == VK_ESCAPE && (self.menu_escape() || (self.find.is_none() && !self.addr_focused && self.close_peek())) {
            return true;
        }
        if self.pick_key(vk) || (self.addr_focused && self.address_key(vk)) || (!self.addr_focused && self.find_key(vk, m)) {
            return true;
        }
        if alt {
            match vk {
                VK_LEFT => self.go_back(),
                VK_RIGHT => self.go_fwd(),
                VK_D => self.focus_addr(),
                _ => return false,
            }
            return true;
        }
        if ctrl && self.clipboard_key(vk) {
            return true;
        }
        if self.addr_focused && fields::edit_key(&mut self.edit, vk, m) {
            return true;
        }
        if !self.addr_focused && self.field_key(vk, m) {
            return true;
        }
        if ctrl {
            match vk {
                VK_L => self.focus_addr(),
                VK_E if shift => self.toggle_engine(),
                VK_T if shift => self.reopen_closed(),
                VK_H if shift => self.start_pick(),
                VK_R if shift => self.toggle_reading(),
                VK_K if !shift => self.focus_addr(),
                VK_F if !shift => self.open_find(),
                VK_C if shift => {
                    let u = self.tab().url.clone();
                    self.copy(&u);
                    self.say("address copied");
                }
                VK_V if shift => {
                    let text = self.clipboard_text();
                    if !text.trim().is_empty() {
                        self.navigate(text.trim(), false);
                    }
                }
                VK_Z if !shift && self.editing().is_none() => self.reopen_closed(),
                VK_PRIOR | VK_NEXT => {
                    let n = self.tabs.iter().filter(|t| !t.peek).count();
                    if n > 0 {
                        self.cur = if vk == VK_PRIOR { (self.cur + n - 1) % n } else { (self.cur + 1) % n };
                        self.update_title();
                        self.update_views();
                    }
                }
                VK_D if !shift => self.bookmark(),
                VK_H if !shift => {
                    let c = self.cur;
                    self.show_place(c, "history", "", true);
                }
                VK_O if shift => {
                    let c = self.cur;
                    self.show_place(c, "bookmarks", "", true);
                }
                VK_M => self.toggle_mute(),
                VK_S => {
                    self.column_hidden = !self.column_hidden;
                    self.sync_size();
                    self.relayout_due = true;
                    self.relayout();
                    self.update_views();
                }
                VK_OEM_COMMA => {
                    let c = self.cur;
                    self.show_settings(c, "", "look", true);
                }
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
        if self.addr_focused {
            match vk {
                VK_RETURN => {
                    let s = self.edit.text();
                    self.navigate(&s, false);
                }
                VK_ESCAPE => self.addr_focused = false,
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
            VK_BACK if prefs::get().backspace_back => self.go_back(),
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
        if self.edge_reveal(x) {
            self.render();
        }
        if self.menu_hover(x, y) || (self.in_page(x, y) && self.pick_hover(x, y)) || self.hover_menu(x, y) || self.drag_field(x, y) {
            return self.render();
        }
        self.over = self.control_at(x, y).map(|(_, c, _)| c.typed());
        let h = self.hit(x, y);
        let link = if self.in_page(x, y) && !self.addr_focused && h == Hit::None {
            let (px, py) = self.to_page(x, y);
            self.tab().dl.as_ref().and_then(|d| d.link_at(px, py)).map(|s| s.to_string())
        } else {
            None
        };
        if h != self.hover || link != self.hover_link {
            self.hover = h;
            self.hover_link = link;
            self.render();
        }
    }

    fn click(&mut self, x: f32, y: f32, m: Mods) {
        self.mouse_move(x, y);
        let (t, lx, ly) = self.last_click;
        let double = t.elapsed().as_millis() < unsafe { GetDoubleClickTime() } as u128 && (x - lx).abs() + (y - ly).abs() < 8.0;
        if self.menu_click(x, y) || (self.in_page(x, y) && self.pick_click()) {
            return self.render();
        }
        // a click beside a peek puts it away
        if self.peeking() && !self.in_page(x, y) && self.hit(x, y) == Hit::None {
            self.close_peek();
            return self.render();
        }
        self.picking = None;
        self.last_click = (if double { t - std::time::Duration::from_secs(9).min(t.elapsed()) } else { Instant::now() }, x, y);
        match self.hit(x, y) {
            Hit::Tab(i) => {
                self.blur();
                self.cur = i;
                self.addr_focused = false;
                self.update_title();
                self.update_views();
            }
            Hit::Engine => self.toggle_engine(),
            Hit::Settings => {
                let c = self.cur;
                if self.tab().url == START {
                    self.show_settings(c, "", "look", true);
                } else {
                    self.new_tab(START);
                    let c = self.cur;
                    self.show_settings(c, "", "look", false);
                }
            }
            Hit::Mute(i) => {
                let keep = self.cur;
                self.cur = i;
                self.toggle_mute();
                self.cur = keep;
            }
            Hit::Suggest(i) => self.take_suggestion(i),
            Hit::Star => self.bookmark(),
            Hit::PeekClose => {
                self.close_peek();
            }
            Hit::PeekKeep => self.keep_peek(),
            Hit::Page(name) => {
                if self.tab().url != START {
                    self.new_tab(START);
                }
                let c = self.cur;
                self.show_place(c, name, "", false);
            }
            Hit::Panel => {}
            Hit::Close(i) => self.close_tab(i),
            Hit::NewTab => {
                self.new_tab(START);
                self.focus_addr();
            }
            Hit::Back => self.go_back(),
            Hit::Fwd => self.go_fwd(),
            Hit::Reload => self.reload(),
            Hit::Addr => {
                if !self.addr_focused {
                    self.focus_addr();
                } else {
                    let at = self.addr_char(x);
                    if double {
                        self.edit.select_word(at);
                    } else {
                        self.edit.place(at, m.shift);
                    }
                }
            }
            Hit::None if self.addr_focused => self.addr_focused = false,
            Hit::None => {
                if self.click_field(x, y, m, double) {
                } else if let Some(l) = self.hover_link.clone() {
                    if l.starts_with(layout::PRESS) {
                        self.press(&l);
                    } else if host_of(&l) == "kamaitachi.local" && !self.tab().own {
                        // only pages made here may change settings
                    } else if m.shift && !self.peeking() {
                        self.peek(&l);
                    } else if m.ctrl {
                        self.open_background(&l);
                    } else {
                        let c = self.cur;
                        let from = host_of(&self.tab().url);
                        self.load_from(c, l, true, from);
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
        // scrolling eases toward its target, one step per frame, unless it should jump
        {
            let smooth = prefs::get().smooth;
            let t = self.tab_mut();
            let d = t.target - t.scroll;
            t.scroll = if d.abs() < 0.5 || !smooth { t.target } else { t.scroll + d * 0.28 };
        }
        let card = if self.tab().url == START || self.tab().full_mode { sk().field } else { self.tab().dl.as_ref().map_or(sk().field, |d| d.bg) };
        let card = if !sk().light && prefs::get().darken && luma(card) > 150 && self.tab().url != START && !self.tab().full_mode { night(card) } else { card };
        self.hits.clear();
        let t0 = Instant::now();
        self.gpu.begin(sk().frame);
        if self.peeking() {
            self.draw_peek_under();
        }
        self.draw_card(card);
        self.draw_page();
        self.draw_pick();
        self.draw_find();
        let t1 = t0.elapsed();
        self.draw_card_edges();
        self.draw_peek_bar();
        self.draw_loading();
        self.draw_column();
        self.draw_notes();
        self.draw_address();
        self.draw_menu();
        self.draw_tab_menu();
        if std::env::var_os("HAYATE_FRAMES").is_some() {
            eprintln!("frame {:.2} ms (page {:.2})", t0.elapsed().as_secs_f64() * 1000.0, t1.as_secs_f64() * 1000.0);
        }
        let shoot = matches!(self.shot, Some((_, n)) if n >= 1);
        if shoot {
            let busy = self.drive();
            if let Some((_, n)) = &mut self.shot {
                *n = if busy { 1 } else { *n + 1 };
            }
            // a couple of frames so late glyphs are in the atlas
            let wait = std::env::var("HAYATE_SHOT_WAIT_MS").ok().and_then(|v| v.parse::<u128>().ok()).unwrap_or(0);
            if let Some((p, n)) = &self.shot
                && *n >= 3
                && self.started.elapsed().as_millis() >= wait
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
            self.draw_start();
        }
        if self.tab().url != START && (self.wall.is_some() || self.wall_checked) {
            // off the new tab the wallpaper's pixels go away
            self.wall = None;
            self.wall_checked = false;
        }
        let k = self.k();
        let (ox, top) = self.page_origin();
        let b = self.page_box();
        let (w, h) = ((b.x + b.w) * self.scale, (b.y + b.h) * self.scale);
        let view = [ox, top, w, h];
        let live = self.live();
        let c = self.cur;
        let t = &self.tabs[c];
        if t.full_mode {
            if t.full.is_none()
                && let Some(img) = &t.picture_img
            {
                let iw = img_size(img).0 as f32;
                let ih = img_size(img).1 as f32;
                let k = (((w - ox) / iw).min((h - top) / ih)).min(1.0);
                self.gpu.draw_image(img, ox, top, iw * k, ih * k, 1.0, 0.0, view);
                return;
            }
            if t.full.is_none() {
                let msg = if self.env_state == 1 || self.env_state == 0 { "Starting the full engine" } else { "Opening" };
                self.label(msg, b.x + b.w / 2.0 - 70.0, b.y + b.h / 2.0 - 10.0, 14.0, 400, sk().muted, 300.0, UI_FONT);
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
            [(ox + r.x * k).max(view[0]), (r.y * k + oy).max(view[1]), (ox + (r.x + r.w) * k).min(view[2]), ((r.y + r.h) * k + oy).min(view[3])]
        };
        // in Night, a page that is only light is turned dark: each colour's lightness is
        // turned over and its hue kept, so white is near black and blue links stay blue
        let dark = !sk().light && prefs::get().darken && luma(dl.bg) > 150;
        let tone = |c: Rgba| if dark { night(c) } else { c };
        let gpu = &mut self.gpu;
        let mut skip_text = false;
        for it in &dl.items {
            match *it {
                Item::Field { field, clip, .. } => {
                    let Some(l) = live.as_ref().filter(|l| l.field == field) else { continue };
                    let cl = {
                        let (a, b) = (clipf(clip), [ox + l.clip.x * k, l.clip.y * k + oy, ox + (l.clip.x + l.clip.w) * k, (l.clip.y + l.clip.h) * k + oy]);
                        [a[0].max(b[0]), a[1].max(b[1]), a[2].min(b[2]), a[3].min(b[3])]
                    };
                    let mark = sk().accent;
                    for m in &l.marked {
                        gpu.rect(ox + m.x * k, m.y * k + oy, m.w * k, m.h * k, [mark[0], mark[1], mark[2], 90], 0.0, cl);
                    }
                    for it in &l.list.items {
                        if let Item::Text { start, len, color, .. } = *it {
                            let cc = gpu::rgba(color, 1.0);
                            for g in &l.list.glyphs[start as usize..(start + len) as usize] {
                                gpu.glyph(g.face, g.size * k, g.id, ox + g.x * k, g.y * k + oy, cc, cl);
                            }
                        }
                    }
                    if let Some(c) = l.caret {
                        gpu.rect(ox + c.x * k, c.y * k + oy, (c.w * k).max(1.0), c.h * k, dl.fields[field as usize].color, 0.0, cl);
                    }
                    skip_text = true;
                }
                Item::Text { .. } if std::mem::take(&mut skip_text) => {}
                Item::Rect { r, color, radius, clip } => {
                    if r.y + r.h < y0 || r.y > y1 {
                        continue;
                    }
                    gpu.rect(ox + r.x * k, r.y * k + oy, r.w * k, r.h * k, tone(color), radius * k, clipf(clip));
                }
                Item::Ring { r, width, color, radius, clip } => {
                    if r.y + r.h < y0 || r.y > y1 {
                        continue;
                    }
                    gpu.ring(ox + r.x * k, r.y * k + oy, r.w * k, r.h * k, width * k, tone(color), radius * k, clipf(clip));
                }
                Item::Text { start, len, color, clip } => {
                    if len == 0 {
                        continue;
                    }
                    let gs = &dl.glyphs[start as usize..(start + len) as usize];
                    if gs[gs.len() - 1].y < y0 || gs[0].y > y1 + 40.0 {
                        continue;
                    }
                    let cc = gpu::rgba(tone(color), 1.0);
                    let cl = clipf(clip);
                    for g in gs {
                        gpu.glyph(g.face, g.size * k, g.id, ox + g.x * k, g.y * k + oy, cc, cl);
                    }
                }
                Item::Image { r, img, alpha, radius, clip } => {
                    if r.y + r.h < y0 || r.y > y1 || r.w <= 0.0 {
                        continue;
                    }
                    if let Some(tex) = t.tex.get(&img) {
                        gpu.draw_image(tex, ox + r.x * k, r.y * k + oy, r.w * k, r.h * k, alpha as f32 / 255.0 * if dark { 0.85 } else { 1.0 }, radius * k, clipf(clip));
                    } else if t.loading {
                        gpu.rect(ox + r.x * k, r.y * k + oy, r.w * k, r.h * k, [128, 128, 128, 24], radius * k, clipf(clip));
                    }
                }
            }
        }
        if let Some(l) = &live {
            let r = l.clip;
            gpu.ring(ox + (r.x - 4.0) * k, (r.y - 3.0) * k + oy, (r.w + 8.0) * k, (r.h + 6.0) * k, 2.0 * k, sk().accent, 3.0 * k, view);
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
        let b = self.page_box();
        let (cw, ch) = ((b.w * self.scale).max(1.0) as u32, (b.h * self.scale).max(1.0) as u32);
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
}

/// How light a colour looks, 0 to 255.
fn luma(c: Rgba) -> u32 {
    (c[0] as u32 * 299 + c[1] as u32 * 587 + c[2] as u32 * 114) / 1000
}

/// A colour with its lightness turned over and its hue kept: each channel moves by the
/// same amount, so the brightest and darkest channels trade places around the middle.
fn night(c: Rgba) -> Rgba {
    let (hi, lo) = (c[0].max(c[1]).max(c[2]) as i32, c[0].min(c[1]).min(c[2]) as i32);
    let shift = 255 - hi - lo;
    let f = |v: u8| (v as i32 + shift).clamp(0, 255) as u8;
    // pure black would be too hard against the night frame
    let lift = |v: u8| v.max(0x16);
    [lift(f(c[0])), lift(f(c[1])), lift(f(c[2])), c[3]]
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
