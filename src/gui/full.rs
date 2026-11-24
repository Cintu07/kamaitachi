//! Full-engine tabs: the WebView2 runtime (Chromium, shipped with windows) hosted in the
//! content area, for sites that need javascript, video calls or a camera. It starts on
//! first use, so reading with the light engine never pays for it. Callbacks only queue
//! events; the window thread handles them on its next message, never inside a call.

use std::cell::{Cell, RefCell};
use webview2_com::Microsoft::Web::WebView2::Win32::*;
use webview2_com::*;
use windows::core::{Interface, BOOL, HSTRING, PCWSTR, PWSTR};
use windows::Win32::Foundation::{HWND, LPARAM, RECT, WPARAM};
use windows::Win32::System::LibraryLoader::LoadLibraryW;
use windows::Win32::UI::Input::KeyboardAndMouse::{GetKeyState, VK_CONTROL, VK_SHIFT};
use windows::Win32::UI::WindowsAndMessaging::{PostMessageW, WM_APP};

pub const WM_FULL: u32 = WM_APP + 1;

/// Flags for the full engine's chromium, each measured on a youtube video page (631 mb with
/// none): no gpu process (the intel driver alone is 300 mb; video decodes on the cpu), no
/// spare renderer, the network, audio and storage services inside the browser process, and
/// v8's memory-saving mode (renderer 301 -> 190 mb, slower heavy javascript). Together: 298 mb.
/// HAYATE_CHROMIUM_ARGS overrides them.
pub const CHROMIUM_ARGS: &str = "--disable-gpu --disable-features=SpareRendererForSitePerProcess,BackForwardCache,Translate,OptimizationHints,MediaRouter,AudioServiceOutOfProcess,StorageServiceOutOfProcess,msSmartScreenProtection,msEdgeTranslate --enable-features=NetworkServiceInProcess2 --disable-background-networking --disable-component-update --renderer-process-limit=2 --js-flags=--optimize-for-size,--lite-mode";

pub enum Ev {
    Env(std::result::Result<ICoreWebView2Environment, String>),
    Ready { tab: u64, ctl: ICoreWebView2Controller },
    Failed { tab: u64, err: String },
    Url { tab: u64, url: String },
    Title { tab: u64, title: String },
    Loading { tab: u64, on: bool },
    NewWindow { url: String },
    Key { vk: u32, alt: bool },
    /// a jpeg of what the tab shows, kept for when it is parked
    Picture { tab: u64, jpeg: Vec<u8> },
    /// the blocker stopped a request
    Blocked { tab: u64 },
    /// a navigation to a malware or phishing site was stopped
    Danger { tab: u64, url: String },
}

thread_local! {
    static EVENTS: RefCell<Vec<Ev>> = const { RefCell::new(Vec::new()) };
    static MAIN: Cell<isize> = const { Cell::new(0) };
}

fn push(e: Ev) {
    EVENTS.with(|q| q.borrow_mut().push(e));
    let h = MAIN.with(|m| m.get());
    unsafe {
        let _ = PostMessageW(Some(HWND(h as *mut _)), WM_FULL, WPARAM(0), LPARAM(0));
    }
}

pub fn take() -> Vec<Ev> {
    EVENTS.with(|q| std::mem::take(&mut *q.borrow_mut()))
}

const LOADER: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/WebView2Loader.dll"));

fn data_dir() -> std::path::PathBuf {
    let base = std::env::var("LOCALAPPDATA").unwrap_or_else(|_| ".".into());
    std::path::Path::new(&base).join("hayate")
}

/// Puts the small WebView2 loader where windows finds it, once.
fn ensure_loader() -> std::result::Result<(), String> {
    let dir = data_dir();
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let p = dir.join("WebView2Loader.dll");
    if !LOADER.is_empty() && std::fs::metadata(&p).ok().map(|m| m.len() as usize) != Some(LOADER.len()) {
        std::fs::write(&p, LOADER).map_err(|e| format!("writing the webview2 loader: {e}"))?;
    }
    let target = if LOADER.is_empty() { std::path::PathBuf::from("WebView2Loader.dll") } else { p };
    unsafe { LoadLibraryW(&HSTRING::from(target.as_os_str())).map_err(|e| format!("webview2 loader: {e}"))? };
    Ok(())
}

/// Starts the shared environment (one browser process for every full tab).
pub fn start(hwnd: HWND) -> std::result::Result<(), String> {
    MAIN.with(|m| m.set(hwnd.0 as isize));
    ensure_loader()?;
    let data = HSTRING::from(data_dir().join("webview2").as_os_str());
    let opts = CoreWebView2EnvironmentOptions::default();
    let args = std::env::var("HAYATE_CHROMIUM_ARGS").unwrap_or_else(|_| CHROMIUM_ARGS.to_string());
    unsafe {
        opts.set_additional_browser_arguments(args);
        opts.set_are_browser_extensions_enabled(std::env::var("HAYATE_EXTENSIONS").is_ok());
    }
    let options: ICoreWebView2EnvironmentOptions = opts.into();
    let handler = CreateCoreWebView2EnvironmentCompletedHandler::create(Box::new(|r, env| {
        push(Ev::Env(match (r, env) {
            (Ok(()), Some(e)) => Ok(e),
            (Err(e), _) => Err(e.message().to_string()),
            _ => Err("no environment".into()),
        }));
        Ok(())
    }));
    unsafe { CreateCoreWebView2EnvironmentWithOptions(PCWSTR::null(), PCWSTR(data.as_ptr()), &options, &handler) }.map_err(|e| format!("webview2: {}", e.message()))
}

pub fn create(env: &ICoreWebView2Environment, hwnd: HWND, tab: u64) -> std::result::Result<(), String> {
    let handler = CreateCoreWebView2ControllerCompletedHandler::create(Box::new(move |r, ctl| {
        match (r, ctl) {
            (Ok(()), Some(c)) => push(Ev::Ready { tab, ctl: c }),
            (Err(e), _) => push(Ev::Failed { tab, err: e.message().to_string() }),
            _ => push(Ev::Failed { tab, err: "no controller".into() }),
        }
        Ok(())
    }));
    unsafe { env.CreateCoreWebView2Controller(hwnd, &handler) }.map_err(|e| e.message().to_string())
}

fn source(wv: &ICoreWebView2) -> String {
    let mut p = PWSTR::null();
    unsafe {
        if wv.Source(&mut p).is_ok() {
            return take_pwstr(p);
        }
    }
    String::new()
}

/// Hooks a new view's events to its tab.
/// Strips the ad data out of youtube's player responses before the player reads them
/// (what uBlock Origin's json-prune scriptlet does).
const YT_SCRIPT: &str = r#"(()=>{if(!/(^|\.)youtube\.com$/.test(location.hostname))return;const K=['adPlacements','playerAds','adSlots','adBreakHeartbeatParams'];const P=o=>{try{if(o&&typeof o==='object'){for(const k of K)if(k in o)delete o[k];if(o.playerResponse)P(o.playerResponse);}}catch(e){}return o};const jp=JSON.parse;JSON.parse=function(){return P(jp.apply(this,arguments))};const rj=Response.prototype.json;Response.prototype.json=function(){return rj.apply(this,arguments).then(P)};let v;try{Object.defineProperty(window,'ytInitialPlayerResponse',{configurable:true,get(){return v},set(x){v=P(x)}})}catch(e){}})();"#;

fn host_of(url: &str) -> String {
    let r = url.split("://").nth(1).unwrap_or(url);
    r.split(['/', '?', '#', ':']).next().unwrap_or("").to_ascii_lowercase()
}

fn kind_of(c: COREWEBVIEW2_WEB_RESOURCE_CONTEXT) -> crate::web::block::Kind {
    use crate::web::block::Kind;
    match c {
        COREWEBVIEW2_WEB_RESOURCE_CONTEXT_DOCUMENT => Kind::Document,
        COREWEBVIEW2_WEB_RESOURCE_CONTEXT_STYLESHEET => Kind::Stylesheet,
        COREWEBVIEW2_WEB_RESOURCE_CONTEXT_IMAGE => Kind::Image,
        COREWEBVIEW2_WEB_RESOURCE_CONTEXT_MEDIA => Kind::Media,
        COREWEBVIEW2_WEB_RESOURCE_CONTEXT_FONT => Kind::Font,
        COREWEBVIEW2_WEB_RESOURCE_CONTEXT_SCRIPT => Kind::Script,
        COREWEBVIEW2_WEB_RESOURCE_CONTEXT_XML_HTTP_REQUEST | COREWEBVIEW2_WEB_RESOURCE_CONTEXT_FETCH | COREWEBVIEW2_WEB_RESOURCE_CONTEXT_EVENT_SOURCE => Kind::Xhr,
        COREWEBVIEW2_WEB_RESOURCE_CONTEXT_WEBSOCKET => Kind::Websocket,
        COREWEBVIEW2_WEB_RESOURCE_CONTEXT_PING | COREWEBVIEW2_WEB_RESOURCE_CONTEXT_CSP_VIOLATION_REPORT => Kind::Ping,
        _ => Kind::Other,
    }
}

pub fn wire(ctl: &ICoreWebView2Controller, tab: u64, env: Option<ICoreWebView2Environment>) -> windows::core::Result<ICoreWebView2> {
    let wv = unsafe { ctl.CoreWebView2()? };
    let mut tok = 0i64;
    unsafe {
        // the blocker sees every request before it leaves
        wv.AddWebResourceRequestedFilter(windows::core::w!("*"), COREWEBVIEW2_WEB_RESOURCE_CONTEXT_ALL)?;
        wv.add_WebResourceRequested(
            &WebResourceRequestedEventHandler::create(Box::new(move |w, args| {
                let (Some(w), Some(a), Some(env)) = (w, args, env.as_ref()) else { return Ok(()) };
                let Some(b) = crate::web::block::get() else { return Ok(()) };
                let mut ctx = COREWEBVIEW2_WEB_RESOURCE_CONTEXT::default();
                a.ResourceContext(&mut ctx)?;
                if ctx == COREWEBVIEW2_WEB_RESOURCE_CONTEXT_DOCUMENT {
                    return Ok(());
                }
                let mut p = PWSTR::null();
                a.Request()?.Uri(&mut p)?;
                let url = take_pwstr(p);
                let page = host_of(&source(&w));
                if b.blocks(&url, &page, kind_of(ctx)) {
                    let resp = env.CreateWebResourceResponse(None, 403, windows::core::w!("Blocked"), windows::core::w!(""))?;
                    a.SetResponse(&resp)?;
                    push(Ev::Blocked { tab });
                }
                Ok(())
            })),
            &mut tok,
        )?;
        // each page's own hiding rules go in as its content starts to load
        wv.add_ContentLoading(
            &ContentLoadingEventHandler::create(Box::new(move |w, _| {
                let Some(w) = w else { return Ok(()) };
                let Some(b) = crate::web::block::get() else { return Ok(()) };
                let css = b.site_hiding(&host_of(&source(&w)));
                if !css.is_empty() {
                    let js = format!("(()=>{{const s=document.createElement('style');s.textContent={};(document.head||document.documentElement).appendChild(s)}})()", json_str(&css));
                    let _ = w.ExecuteScript(&HSTRING::from(js), &ExecuteScriptCompletedHandler::create(Box::new(|_, _| Ok(()))));
                }
                Ok(())
            })),
            &mut tok,
        )?;
        if crate::web::block::get().is_some() {
            let _ = wv.AddScriptToExecuteOnDocumentCreated(&HSTRING::from(YT_SCRIPT), &AddScriptToExecuteOnDocumentCreatedCompletedHandler::create(Box::new(|_, _| Ok(()))));
        }
        wv.add_SourceChanged(
            &SourceChangedEventHandler::create(Box::new(move |w, _| {
                if let Some(w) = w {
                    push(Ev::Url { tab, url: source(&w) });
                }
                Ok(())
            })),
            &mut tok,
        )?;
        wv.add_DocumentTitleChanged(
            &DocumentTitleChangedEventHandler::create(Box::new(move |w, _| {
                if let Some(w) = w {
                    let mut p = PWSTR::null();
                    if w.DocumentTitle(&mut p).is_ok() {
                        push(Ev::Title { tab, title: take_pwstr(p) });
                    }
                }
                Ok(())
            })),
            &mut tok,
        )?;
        wv.add_NavigationStarting(
            &NavigationStartingEventHandler::create(Box::new(move |_, args| {
                if let Some(a) = args {
                    let mut p = PWSTR::null();
                    if a.Uri(&mut p).is_ok() {
                        let url = take_pwstr(p);
                        if crate::web::block::is_danger(&url) {
                            a.SetCancel(true)?;
                            push(Ev::Danger { tab, url });
                            return Ok(());
                        }
                    }
                }
                push(Ev::Loading { tab, on: true });
                Ok(())
            })),
            &mut tok,
        )?;
        wv.add_NavigationCompleted(
            &NavigationCompletedEventHandler::create(Box::new(move |_, _| {
                push(Ev::Loading { tab, on: false });
                Ok(())
            })),
            &mut tok,
        )?;
        wv.add_NewWindowRequested(
            &NewWindowRequestedEventHandler::create(Box::new(move |_, args| {
                if let Some(a) = args {
                    let mut p = PWSTR::null();
                    if a.Uri(&mut p).is_ok() {
                        let url = take_pwstr(p);
                        let _ = a.SetHandled(true);
                        push(Ev::NewWindow { url });
                    }
                }
                Ok(())
            })),
            &mut tok,
        )?;
        // the browser's own shortcuts still work while the page has the keyboard
        ctl.add_AcceleratorKeyPressed(
            &AcceleratorKeyPressedEventHandler::create(Box::new(move |_, args| {
                let Some(a) = args else { return Ok(()) };
                let mut kind = COREWEBVIEW2_KEY_EVENT_KIND::default();
                let mut vk = 0u32;
                a.KeyEventKind(&mut kind)?;
                a.VirtualKey(&mut vk)?;
                let alt = kind == COREWEBVIEW2_KEY_EVENT_KIND_SYSTEM_KEY_DOWN;
                if kind != COREWEBVIEW2_KEY_EVENT_KIND_KEY_DOWN && !alt {
                    return Ok(());
                }
                let ctrl = GetKeyState(VK_CONTROL.0 as i32) < 0;
                let shift = GetKeyState(VK_SHIFT.0 as i32) < 0;
                let ours = if alt {
                    matches!(vk, 0x25 | 0x27 | 0x44)
                } else if ctrl {
                    matches!(vk, 0x4C | 0x54 | 0x57 | 0x09 | 0x31..=0x39) || (shift && vk == 0x45)
                } else {
                    matches!(vk, 0x74 | 0x75)
                };
                if ours {
                    a.SetHandled(true)?;
                    push(Ev::Key { vk, alt });
                }
                Ok(())
            })),
            &mut tok,
        )?;
    }
    Ok(wv)
}

fn json_str(s: &str) -> String {
    let mut o = String::with_capacity(s.len() + 2);
    o.push('"');
    for c in s.chars() {
        match c {
            '"' => o.push_str("\\\""),
            '\\' => o.push_str("\\\\"),
            '\n' => o.push_str("\\n"),
            '\r' => {}
            c if (c as u32) < 0x20 => {}
            '<' => o.push_str("\\u003c"),
            c => o.push(c),
        }
    }
    o.push('"');
    o
}

pub fn place(ctl: &ICoreWebView2Controller, r: RECT, visible: bool) {
    unsafe {
        let _ = ctl.SetBounds(r);
        let _ = ctl.SetIsVisible(visible);
        if let Ok(wv) = ctl.CoreWebView2() {
            if let Ok(w19) = wv.cast::<ICoreWebView2_19>() {
                let _ = w19.SetMemoryUsageTargetLevel(if visible { COREWEBVIEW2_MEMORY_USAGE_TARGET_LEVEL_NORMAL } else { COREWEBVIEW2_MEMORY_USAGE_TARGET_LEVEL_LOW });
            }
            if !visible && let Ok(w3) = wv.cast::<ICoreWebView2_3>() {
                // a hidden tab stops running scripts and timers until shown again
                let _ = w3.TrySuspend(&TrySuspendCompletedHandler::create(Box::new(|_, _| Ok(()))));
            }
        }
    }
}

/// Asks for a jpeg of the view; it arrives as Ev::Picture.
pub fn capture(wv: &ICoreWebView2, tab: u64) {
    use windows::Win32::System::Com::StructuredStorage::CreateStreamOnHGlobal;
    use windows::Win32::System::Com::{IStream, STREAM_SEEK_CUR, STREAM_SEEK_SET};
    unsafe {
        let Ok(stream) = CreateStreamOnHGlobal(windows::Win32::Foundation::HGLOBAL::default(), true) else { return };
        let s2: IStream = stream.clone();
        let h = CapturePreviewCompletedHandler::create(Box::new(move |r| {
            if r.is_ok() {
                let mut len = 0u64;
                if s2.Seek(0, STREAM_SEEK_CUR, Some(&mut len)).is_ok() && len > 0 && len < 16 << 20 {
                    let _ = s2.Seek(0, STREAM_SEEK_SET, None);
                    let mut buf = vec![0u8; len as usize];
                    let mut got = 0u32;
                    if s2.Read(buf.as_mut_ptr() as *mut _, len as u32, Some(&mut got)).is_ok() {
                        buf.truncate(got as usize);
                        push(Ev::Picture { tab, jpeg: buf });
                    }
                }
            }
            Ok(())
        }));
        let _ = wv.CapturePreview(COREWEBVIEW2_CAPTURE_PREVIEW_IMAGE_FORMAT_JPEG, &stream, &h);
    }
}

pub fn navigate(wv: &ICoreWebView2, url: &str) {
    unsafe {
        let _ = wv.Navigate(&HSTRING::from(url));
    }
}

pub fn can(wv: &ICoreWebView2) -> (bool, bool) {
    let (mut b, mut f) = (BOOL(0), BOOL(0));
    unsafe {
        let _ = wv.CanGoBack(&mut b);
        let _ = wv.CanGoForward(&mut f);
    }
    (b.as_bool(), f.as_bool())
}

pub fn focus(ctl: &ICoreWebView2Controller) {
    unsafe {
        let _ = ctl.MoveFocus(COREWEBVIEW2_MOVE_FOCUS_REASON_PROGRAMMATIC);
    }
}

pub fn browser_pid(wv: &ICoreWebView2) -> u32 {
    let mut pid = 0u32;
    unsafe {
        let _ = wv.BrowserProcessId(&mut pid);
    }
    pid
}

/// Private bytes of a process and everything it started, in mb.
pub fn tree_mb(root: u32) -> f32 {
    use windows::Win32::System::Diagnostics::ToolHelp::*;
    use windows::Win32::System::ProcessStatus::*;
    use windows::Win32::System::Threading::*;
    if root == 0 {
        return 0.0;
    }
    let mut pairs = Vec::new();
    unsafe {
        let Ok(snap) = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) else { return 0.0 };
        let mut e = PROCESSENTRY32W { dwSize: std::mem::size_of::<PROCESSENTRY32W>() as u32, ..Default::default() };
        if Process32FirstW(snap, &mut e).is_ok() {
            loop {
                pairs.push((e.th32ProcessID, e.th32ParentProcessID));
                if Process32NextW(snap, &mut e).is_err() {
                    break;
                }
            }
        }
        let _ = windows::Win32::Foundation::CloseHandle(snap);
    }
    let mut set = vec![root];
    let mut k = 0;
    while k < set.len() {
        let p = set[k];
        for &(c, par) in &pairs {
            if par == p && !set.contains(&c) {
                set.push(c);
            }
        }
        k += 1;
    }
    let mut total = 0usize;
    for pid in set {
        unsafe {
            if let Ok(h) = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) {
                let mut c = PROCESS_MEMORY_COUNTERS_EX { cb: std::mem::size_of::<PROCESS_MEMORY_COUNTERS_EX>() as u32, ..Default::default() };
                if K32GetProcessMemoryInfo(h, &mut c as *mut _ as *mut PROCESS_MEMORY_COUNTERS, c.cb).as_bool() {
                    total += c.PrivateUsage;
                }
                let _ = windows::Win32::Foundation::CloseHandle(h);
            }
        }
    }
    total as f32 / (1024.0 * 1024.0)
}

/// Sites that are applications: they get the full engine from the start.
const APPS: &[&str] = &[
    "x.com", "twitter.com", "youtube.com", "youtu.be", "zoom.us", "meet.google.com", "teams.microsoft.com", "teams.live.com", "docs.google.com",
    "drive.google.com", "mail.google.com", "calendar.google.com", "web.whatsapp.com", "discord.com", "app.slack.com", "notion.so", "figma.com",
    "chatgpt.com", "claude.ai", "gemini.google.com", "hackerrank.com", "codesignal.com", "leetcode.com", "instagram.com", "facebook.com",
    "linkedin.com", "www.reddit.com", "netflix.com", "open.spotify.com", "twitch.tv", "outlook.live.com", "outlook.office.com", "replit.com",
    "codepen.io", "maps.google.com", "photos.google.com", "web.telegram.org", "excalidraw.com", "canva.com",
];

pub fn is_app(host: &str) -> bool {
    let h = host.trim_start_matches("www.");
    APPS.iter().any(|a| {
        let a = a.trim_start_matches("www.");
        h == a || h.ends_with(&format!(".{a}"))
    }) || (host == "www.google.com" || host == "google.com")
}
