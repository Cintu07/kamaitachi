//! Raw windows: console, events, memory counters, credential manager, mapped files,
//! reserved memory. Everything here is a thin wrapper over windows-sys.

#![allow(clippy::missing_safety_doc)]

use core::ffi::c_void;
use core::ptr::{null, null_mut};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

use windows_sys::Win32::Foundation::*;
use windows_sys::Win32::Security::Credentials::*;
use windows_sys::Win32::Storage::FileSystem::WriteFile;
use windows_sys::Win32::System::Console::*;
use windows_sys::Win32::System::Memory::*;
use windows_sys::Win32::System::ProcessStatus::*;
use windows_sys::Win32::System::Threading::*;

pub type Handle = HANDLE;

pub fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(Some(0)).collect()
}

// ---------------------------------------------------------------- console

static SAVED: AtomicBool = AtomicBool::new(false);
static OLD_IN: AtomicU32 = AtomicU32::new(0);
static OLD_OUT: AtomicU32 = AtomicU32::new(0);
static OLD_CP: AtomicU32 = AtomicU32::new(0);
static OLD_INCP: AtomicU32 = AtomicU32::new(0);

pub struct Console {
    pub hin: HANDLE,
    pub hout: HANDLE,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Key {
    Char(char),
    Ctrl(char),
    Alt(char),
    Enter,
    ShiftEnter,
    Esc,
    Backspace,
    Tab,
    BackTab,
    CtrlTab,
    CtrlShiftTab,
    Up,
    Down,
    Left,
    Right,
    PageUp,
    PageDown,
    Home,
    End,
    Delete,
    CtrlBackspace,
    CtrlLeft,
    CtrlRight,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Event {
    Key(Key),
    Wheel(i16),
    Click { x: u16, y: u16 },
    Resize,
}

impl Console {
    /// Raw input, virtual terminal output, utf-8, alternate screen. Restored by
    /// `restore_console`, which the panic hook also calls.
    pub fn init() -> std::io::Result<Console> {
        unsafe {
            let hin = GetStdHandle(STD_INPUT_HANDLE);
            let hout = GetStdHandle(STD_OUTPUT_HANDLE);
            let mut im = 0u32;
            let mut om = 0u32;
            if GetConsoleMode(hin, &mut im) == 0 || GetConsoleMode(hout, &mut om) == 0 {
                return Err(std::io::Error::other("hayate needs a console (run it in windows terminal)"));
            }
            OLD_IN.store(im, Ordering::Relaxed);
            OLD_OUT.store(om, Ordering::Relaxed);
            OLD_CP.store(GetConsoleOutputCP(), Ordering::Relaxed);
            OLD_INCP.store(GetConsoleCP(), Ordering::Relaxed);
            SAVED.store(true, Ordering::Release);

            // no line input, no echo, no processed ctrl+c; window and mouse events on;
            // extended flags without quick edit so the mouse reaches us
            let new_in = ENABLE_WINDOW_INPUT | ENABLE_MOUSE_INPUT | ENABLE_EXTENDED_FLAGS;
            SetConsoleMode(hin, new_in);
            let new_out = ENABLE_PROCESSED_OUTPUT | ENABLE_VIRTUAL_TERMINAL_PROCESSING | DISABLE_NEWLINE_AUTO_RETURN;
            if SetConsoleMode(hout, new_out) == 0 {
                restore_console();
                return Err(std::io::Error::other("this console has no virtual terminal support"));
            }
            SetConsoleOutputCP(65001);
            SetConsoleCP(65001);
            let c = Console { hin, hout };
            // alternate screen, hide cursor, clear
            c.write(b"\x1b[?1049h\x1b[?25l\x1b[2J");
            Ok(c)
        }
    }

    pub fn write(&self, b: &[u8]) {
        let mut off = 0usize;
        while off < b.len() {
            let mut n = 0u32;
            let chunk = (b.len() - off).min(u32::MAX as usize) as u32;
            let ok = unsafe { WriteFile(self.hout, b[off..].as_ptr(), chunk, &mut n, null_mut()) };
            if ok == 0 || n == 0 {
                return;
            }
            off += n as usize;
        }
    }

    pub fn size(&self) -> (u16, u16) {
        unsafe {
            let mut info: CONSOLE_SCREEN_BUFFER_INFO = core::mem::zeroed();
            if GetConsoleScreenBufferInfo(self.hout, &mut info) == 0 {
                return (80, 24);
            }
            let w = (info.srWindow.Right - info.srWindow.Left + 1).max(20) as u16;
            let h = (info.srWindow.Bottom - info.srWindow.Top + 1).max(5) as u16;
            (w, h)
        }
    }

    /// Reads whatever input is waiting and decodes it. Call after the console handle
    /// is signaled. Returns the number of events written into `out`.
    pub fn read(&self, recs: &mut [INPUT_RECORD], out: &mut [Event], pending_hi: &mut u16) -> usize {
        let mut avail = 0u32;
        unsafe {
            if GetNumberOfConsoleInputEvents(self.hin, &mut avail) == 0 || avail == 0 {
                return 0;
            }
            let mut n = 0u32;
            let want = (avail as usize).min(recs.len()) as u32;
            if ReadConsoleInputW(self.hin, recs.as_mut_ptr(), want, &mut n) == 0 {
                return 0;
            }
            let mut k = 0usize;
            for r in &recs[..n as usize] {
                if k >= out.len() {
                    break;
                }
                match r.EventType as u32 {
                    x if x == KEY_EVENT as u32 => {
                        let ke = r.Event.KeyEvent;
                        if ke.bKeyDown == 0 {
                            continue;
                        }
                        let ch = ke.uChar.UnicodeChar;
                        let key = decode_key(ke.wVirtualKeyCode, ch, ke.dwControlKeyState, pending_hi);
                        if let Some(key) = key {
                            for _ in 0..ke.wRepeatCount.max(1) {
                                if k < out.len() {
                                    out[k] = Event::Key(key);
                                    k += 1;
                                }
                            }
                        }
                    }
                    x if x == MOUSE_EVENT as u32 => {
                        let me = r.Event.MouseEvent;
                        if me.dwEventFlags & MOUSE_WHEELED != 0 {
                            let delta = (me.dwButtonState >> 16) as i16;
                            out[k] = Event::Wheel(if delta > 0 { -3 } else { 3 });
                            k += 1;
                        } else if me.dwEventFlags == 0 && me.dwButtonState & FROM_LEFT_1ST_BUTTON_PRESSED != 0 {
                            out[k] = Event::Click { x: me.dwMousePosition.X.max(0) as u16, y: me.dwMousePosition.Y.max(0) as u16 };
                            k += 1;
                        }
                    }
                    x if x == WINDOW_BUFFER_SIZE_EVENT as u32 => {
                        out[k] = Event::Resize;
                        k += 1;
                    }
                    _ => {}
                }
            }
            k
        }
    }
}

fn decode_key(vk: u16, ch: u16, state: u32, pending_hi: &mut u16) -> Option<Key> {
    let ctrl = state & (LEFT_CTRL_PRESSED | RIGHT_CTRL_PRESSED) != 0;
    let alt = state & (LEFT_ALT_PRESSED | RIGHT_ALT_PRESSED) != 0;
    let shift = state & SHIFT_PRESSED != 0;
    Some(match vk {
        0x0D => {
            if shift {
                Key::ShiftEnter
            } else {
                Key::Enter
            }
        }
        0x1B => Key::Esc,
        0x08 => {
            if ctrl {
                Key::CtrlBackspace
            } else {
                Key::Backspace
            }
        }
        0x09 => match (ctrl, shift) {
            (true, false) => Key::CtrlTab,
            (true, true) => Key::CtrlShiftTab,
            (false, true) => Key::BackTab,
            _ => Key::Tab,
        },
        0x26 => Key::Up,
        0x28 => Key::Down,
        0x25 => {
            if ctrl {
                Key::CtrlLeft
            } else {
                Key::Left
            }
        }
        0x27 => {
            if ctrl {
                Key::CtrlRight
            } else {
                Key::Right
            }
        }
        0x21 => Key::PageUp,
        0x22 => Key::PageDown,
        0x24 => Key::Home,
        0x23 => Key::End,
        0x2E => Key::Delete,
        _ => {
            if ch == 0 {
                return None;
            }
            // surrogate pairs arrive as two key events
            if (0xD800..0xDC00).contains(&ch) {
                *pending_hi = ch;
                return None;
            }
            let c = if (0xDC00..0xE000).contains(&ch) && *pending_hi != 0 {
                let hi = core::mem::take(pending_hi);
                char::decode_utf16([hi, ch]).next()?.ok()?
            } else {
                char::from_u32(ch as u32)?
            };
            if ctrl && !alt && (c as u32) < 0x20 {
                // ctrl+letter arrives as a control code
                Key::Ctrl((b'a' + (c as u8) - 1) as char)
            } else if alt && !ctrl {
                Key::Alt(c)
            } else {
                Key::Char(c)
            }
        }
    })
}

/// Puts the console back the way it was. Safe to call twice and from the panic hook.
pub fn restore_console() {
    if !SAVED.swap(false, Ordering::AcqRel) {
        return;
    }
    unsafe {
        let hin = GetStdHandle(STD_INPUT_HANDLE);
        let hout = GetStdHandle(STD_OUTPUT_HANDLE);
        let seq = b"\x1b[0m\x1b[?25h\x1b[?1049l";
        let mut n = 0u32;
        WriteFile(hout, seq.as_ptr(), seq.len() as u32, &mut n, null_mut());
        SetConsoleMode(hin, OLD_IN.load(Ordering::Relaxed));
        SetConsoleMode(hout, OLD_OUT.load(Ordering::Relaxed));
        SetConsoleOutputCP(OLD_CP.load(Ordering::Relaxed));
        SetConsoleCP(OLD_INCP.load(Ordering::Relaxed));
    }
}

pub type InputRecord = INPUT_RECORD;

pub fn zeroed_records<const N: usize>() -> [INPUT_RECORD; N] {
    unsafe { core::mem::zeroed() }
}

// ---------------------------------------------------------------- events and waits

/// An auto-reset win32 event, used to wake a thread blocked in one wait.
pub struct WinEvent(pub HANDLE);
unsafe impl Send for WinEvent {}
unsafe impl Sync for WinEvent {}

impl WinEvent {
    pub fn new() -> WinEvent {
        WinEvent(unsafe { CreateEventW(null(), 0, 0, null()) })
    }
    pub fn set(&self) {
        unsafe {
            SetEvent(self.0);
        }
    }
}

impl Drop for WinEvent {
    fn drop(&mut self) {
        unsafe {
            CloseHandle(self.0);
        }
    }
}

pub const INFINITE_WAIT: u32 = INFINITE;

/// Blocks until one handle is signaled. Returns its index, or None on timeout.
pub fn wait_any(handles: &[HANDLE], timeout_ms: u32) -> Option<usize> {
    let r = unsafe { WaitForMultipleObjects(handles.len() as u32, handles.as_ptr(), 0, timeout_ms) };
    if r < WAIT_OBJECT_0 + handles.len() as u32 {
        Some((r - WAIT_OBJECT_0) as usize)
    } else {
        None
    }
}

// ---------------------------------------------------------------- memory

#[derive(Clone, Copy, Default, Debug)]
pub struct ProcMem {
    pub working_set: usize,
    pub peak_working_set: usize,
    pub private_bytes: usize,
}

pub fn process_memory() -> ProcMem {
    unsafe {
        let mut c: PROCESS_MEMORY_COUNTERS_EX = core::mem::zeroed();
        c.cb = core::mem::size_of::<PROCESS_MEMORY_COUNTERS_EX>() as u32;
        if K32GetProcessMemoryInfo(GetCurrentProcess(), &mut c as *mut _ as *mut PROCESS_MEMORY_COUNTERS, c.cb) == 0 {
            return ProcMem::default();
        }
        ProcMem { working_set: c.WorkingSetSize, peak_working_set: c.PeakWorkingSetSize, private_bytes: c.PrivateUsage }
    }
}

/// A region of address space reserved up front and committed in 64 kb steps, so memory
/// counted by the os is memory actually written.
pub struct Reserved {
    base: *mut u8,
    cap: usize,
    committed: usize,
}

unsafe impl Send for Reserved {}
unsafe impl Sync for Reserved {}

pub const COMMIT_STEP: usize = 64 * 1024;

impl Reserved {
    pub fn new(cap: usize) -> Reserved {
        let base = unsafe { VirtualAlloc(null(), cap, MEM_RESERVE, PAGE_READWRITE) } as *mut u8;
        assert!(!base.is_null(), "address space reservation failed");
        Reserved { base, cap, committed: 0 }
    }
    pub fn base(&self) -> *mut u8 {
        self.base
    }
    pub fn cap(&self) -> usize {
        self.cap
    }
    pub fn committed(&self) -> usize {
        self.committed
    }
    /// Makes sure [0, upto) is committed. Returns false past the reservation.
    pub fn ensure(&mut self, upto: usize) -> bool {
        if upto <= self.committed {
            return true;
        }
        if upto > self.cap {
            return false;
        }
        let target = upto.div_ceil(COMMIT_STEP) * COMMIT_STEP;
        let target = target.min(self.cap);
        let p = unsafe {
            VirtualAlloc(self.base.add(self.committed) as *const c_void, target - self.committed, MEM_COMMIT, PAGE_READWRITE)
        };
        if p.is_null() {
            return false;
        }
        self.committed = target;
        true
    }
    /// Returns every committed page to the os.
    pub fn decommit(&mut self) {
        if self.committed > 0 {
            unsafe {
                VirtualFree(self.base as *mut c_void, self.committed, MEM_DECOMMIT);
            }
            self.committed = 0;
        }
    }
}

impl Drop for Reserved {
    fn drop(&mut self) {
        unsafe {
            VirtualFree(self.base as *mut c_void, 0, MEM_RELEASE);
        }
    }
}

// ---------------------------------------------------------------- secrets

/// A buffer locked into ram and zeroed on drop. Secrets are read into one of these and
/// never copied into ordinary heap memory except inside rustls/http request buffers.
pub struct Locked {
    buf: Box<[u8; 4096]>,
    len: usize,
}

impl Locked {
    pub fn new() -> Locked {
        let buf = Box::new([0u8; 4096]);
        unsafe {
            VirtualLock(buf.as_ptr() as *const c_void, 4096);
        }
        Locked { buf, len: 0 }
    }
    pub fn as_bytes(&self) -> &[u8] {
        &self.buf[..self.len]
    }
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }
    pub fn set(&mut self, b: &[u8]) {
        let n = b.len().min(self.buf.len());
        self.wipe();
        self.buf[..n].copy_from_slice(&b[..n]);
        self.len = n;
    }
    fn wipe(&mut self) {
        for b in self.buf.iter_mut() {
            unsafe { core::ptr::write_volatile(b, 0) };
        }
        self.len = 0;
    }
}

impl Drop for Locked {
    fn drop(&mut self) {
        self.wipe();
        unsafe {
            VirtualUnlock(self.buf.as_ptr() as *const c_void, 4096);
        }
    }
}

pub const CRED_PREFIX: &str = "hayate/";

/// Reads a generic credential from windows credential manager into a locked buffer.
pub fn cred_read(name: &str, out: &mut Locked) -> bool {
    let target = wide(&format!("{CRED_PREFIX}{name}"));
    unsafe {
        let mut p: *mut CREDENTIALW = null_mut();
        if CredReadW(target.as_ptr(), CRED_TYPE_GENERIC, 0, &mut p) == 0 {
            return false;
        }
        let c = &*p;
        let blob = core::slice::from_raw_parts(c.CredentialBlob, c.CredentialBlobSize as usize);
        out.set(blob);
        // zero the copy windows gave us before freeing it
        core::ptr::write_bytes(c.CredentialBlob, 0, c.CredentialBlobSize as usize);
        CredFree(p as *const c_void);
        true
    }
}

pub fn cred_write(name: &str, secret: &[u8]) -> std::io::Result<()> {
    let target = wide(&format!("{CRED_PREFIX}{name}"));
    let user = wide("hayate");
    unsafe {
        let mut c: CREDENTIALW = core::mem::zeroed();
        c.Type = CRED_TYPE_GENERIC;
        c.TargetName = target.as_ptr() as *mut u16;
        c.CredentialBlobSize = secret.len() as u32;
        c.CredentialBlob = secret.as_ptr() as *mut u8;
        c.Persist = CRED_PERSIST_LOCAL_MACHINE;
        c.UserName = user.as_ptr() as *mut u16;
        if CredWriteW(&c, 0) == 0 {
            return Err(std::io::Error::last_os_error());
        }
    }
    Ok(())
}

pub fn cred_delete(name: &str) -> bool {
    let target = wide(&format!("{CRED_PREFIX}{name}"));
    unsafe { CredDeleteW(target.as_ptr(), CRED_TYPE_GENERIC, 0) != 0 }
}

/// Reads a line from the console with echo off, for `hayate key set`.
pub fn read_secret_line() -> std::io::Result<String> {
    unsafe {
        let hin = GetStdHandle(STD_INPUT_HANDLE);
        let mut mode = 0u32;
        let is_console = GetConsoleMode(hin, &mut mode) != 0;
        if is_console {
            SetConsoleMode(hin, (mode & !ENABLE_ECHO_INPUT) | ENABLE_LINE_INPUT | ENABLE_PROCESSED_INPUT);
        }
        let mut s = String::new();
        let r = std::io::stdin().read_line(&mut s);
        if is_console {
            SetConsoleMode(hin, mode);
        }
        r?;
        Ok(s.trim_end_matches(['\r', '\n']).to_string())
    }
}

// ---------------------------------------------------------------- mapped files

/// A read-only memory-mapped view of a file. Reading old posts costs page cache, not heap.
pub struct Mapped {
    view: MEMORY_MAPPED_VIEW_ADDRESS,
    mapping: HANDLE,
    len: usize,
}

unsafe impl Send for Mapped {}

impl Mapped {
    pub fn open(file: &std::fs::File) -> std::io::Result<Option<Mapped>> {
        use std::os::windows::io::AsRawHandle;
        let len = file.metadata()?.len() as usize;
        if len == 0 {
            return Ok(None);
        }
        unsafe {
            let mapping = CreateFileMappingW(file.as_raw_handle() as HANDLE, null(), PAGE_READONLY, 0, 0, null());
            if mapping.is_null() {
                return Err(std::io::Error::last_os_error());
            }
            let view = MapViewOfFile(mapping, FILE_MAP_READ, 0, 0, len);
            if view.Value.is_null() {
                CloseHandle(mapping);
                return Err(std::io::Error::last_os_error());
            }
            Ok(Some(Mapped { view, mapping, len }))
        }
    }
    pub fn bytes(&self) -> &[u8] {
        unsafe { core::slice::from_raw_parts(self.view.Value as *const u8, self.len) }
    }
}

impl Drop for Mapped {
    fn drop(&mut self) {
        unsafe {
            UnmapViewOfFile(self.view);
            CloseHandle(self.mapping);
        }
    }
}

// ---------------------------------------------------------------- processes

/// Raw handle of a child process, for adding it to the ui thread's wait set.
pub fn child_handle(c: &std::process::Child) -> HANDLE {
    use std::os::windows::io::AsRawHandle;
    c.as_raw_handle() as HANDLE
}

/// Microseconds since this process was created (for the cold-start budget).
pub fn since_process_start_us() -> u64 {
    use windows_sys::Win32::System::SystemInformation::GetSystemTimePreciseAsFileTime;
    unsafe {
        let (mut c, mut e, mut k, mut u, mut now): (FILETIME, FILETIME, FILETIME, FILETIME, FILETIME) = core::mem::zeroed();
        GetProcessTimes(GetCurrentProcess(), &mut c, &mut e, &mut k, &mut u);
        GetSystemTimePreciseAsFileTime(&mut now);
        let f = |t: FILETIME| (t.dwHighDateTime as u64) << 32 | t.dwLowDateTime as u64;
        f(now).saturating_sub(f(c)) / 10
    }
}

/// User plus kernel cpu time of this process so far, microseconds.
pub fn cpu_time_us() -> u64 {
    unsafe {
        let (mut c, mut e, mut k, mut u): (FILETIME, FILETIME, FILETIME, FILETIME) = core::mem::zeroed();
        GetProcessTimes(GetCurrentProcess(), &mut c, &mut e, &mut k, &mut u);
        let f = |t: FILETIME| (t.dwHighDateTime as u64) << 32 | t.dwLowDateTime as u64;
        (f(k) + f(u)) / 10
    }
}

pub fn now_unix() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}
