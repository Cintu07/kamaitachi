//! Builds a ui with no net thread. The test plays the net thread's part: it writes text
//! into the arenas and applies replies.

#![allow(dead_code)]

use std::sync::Arc;

use hayate::arena::{ring, Consumer, MB};
use hayate::config::Config;
use hayate::msg::{Command, Reply, TabId};
use hayate::store::{Msg, Post, Role, Span, TextArena};
use hayate::sys::WinEvent;
use hayate::ui::App;

pub fn app(w: u16, h: u16) -> (App, Consumer<Command>) {
    let dir = std::env::temp_dir().join(format!("hayate-test-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let cfg = Arc::new(Config::defaults_for_tests(&dir));
    let (tx, rx) = ring(4096);
    let authors = Arc::new(TextArena::with_budget(2 * MB));
    (App::new(cfg, w, h, tx, Arc::new(WinEvent::new()), authors), rx)
}

pub fn author(app: &mut App, name: &str, handle: &str) -> u32 {
    let idx = app.authors.len() as u32;
    let n = app.authors_arena.append(name.as_bytes()).unwrap();
    let h = app.authors_arena.append(handle.as_bytes()).unwrap();
    app.authors_arena.publish();
    app.apply(Reply::Author { idx, name: n, handle: h });
    idx
}

fn put(app: &App, tab: usize, s: &str) -> Span {
    let a = &app.tabs[tab].arena;
    let span = a.append(s.as_bytes()).unwrap();
    a.publish();
    span
}

pub fn post(app: &mut App, tab: usize, sid: &str, author: u32, ago: u64, text: &str, flags: u16, depth: u8) {
    let now = hayate::sys::now_unix();
    let p = Post {
        sid: put(app, tab, sid),
        author,
        ts: (now - ago) as u32,
        text: put(app, tab, text),
        extra: Span::EMPTY,
        metric: 0,
        position: 0,
        flags,
        depth,
    };
    let id: TabId = app.tabs[tab].id;
    app.apply(Reply::Post { tab: id, post: p });
}

pub fn msg(app: &mut App, tab: usize, role: Role, model: &str, text: &str) {
    let m = put(app, tab, model);
    let t = put(app, tab, text);
    let id = app.tabs[tab].id;
    app.apply(Reply::Msg { tab: id, msg: Msg { role, provider: 2, text: t, model: m, cost_micros: 0 } });
}

pub fn done(app: &mut App, tab: usize) {
    let id = app.tabs[tab].id;
    app.apply(Reply::Done { tab: id, more: false });
}

/// The screen as text without the status line (it shows live memory numbers).
pub fn screen(app: &mut App) -> String {
    let _ = app.frame().len();
    let all = app.screen.front_text();
    let lines: Vec<&str> = all.lines().collect();
    lines[..lines.len() - 1].join("\n") + "\n"
}
