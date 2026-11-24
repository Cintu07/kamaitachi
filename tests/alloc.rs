//! The steady-state frame loop makes zero heap allocations: a counting allocator watches
//! 1,000 frames of scrolling, a streamed message growing, and tab switches.

mod common;

use common::*;
use hayate::arena::thread_allocs;
use hayate::msg::{Reply, Service, Source};
use hayate::store::Role;
use hayate::sys::Key;

#[test]
fn frame_loop_never_allocates() {
    let (mut app, _rx) = app(200, 60);
    app.open(Source::XHome);
    app.open(Source::Chat { service: Service::Claude, chat: 1 });
    app.focus(0);
    let a = author(&mut app, "author 👨‍👩‍👧", "a");
    for i in 0..300 {
        let text = format!("post {i}: తెలుగు 日本語 😀 https://example.com/{i} and some words to wrap around the edge of the screen more than once, more than once, more than once");
        post(&mut app, 0, &format!("{}", 1000 + i), a, 60 * i as u64, &text, 0, (i % 3) as u8);
    }
    done(&mut app, 0);
    msg(&mut app, 1, Role::User, "", "hi");
    msg(&mut app, 1, Role::Assistant, "claude-opus-5-5", "streaming answer ".repeat(200).as_str());
    done(&mut app, 1);
    // warm up: the first frames size the buffers
    for _ in 0..3 {
        let _ = app.frame().len();
    }

    let before = thread_allocs();
    for k in 0..1000 {
        let key = match k % 10 {
            0..=5 => Key::Char('j'),
            6..=8 => Key::Char('k'),
            _ => Key::PageDown,
        };
        app.key(key);
        let _ = app.frame().len();
    }
    // a streamed answer grows: only its last rows change
    app.focus(1);
    let _ = app.frame().len();
    let before_chat = thread_allocs();
    let id = app.tabs[1].id;
    let full = match &app.tabs[1].store {
        hayate::ui::Store::Chat(c) => c.msgs.last().unwrap().text.len,
        _ => 0,
    };
    for len in (0..full).step_by(17) {
        app.apply(Reply::Grow { tab: id, len });
        let _ = app.frame().len();
    }
    let after = thread_allocs();
    assert_eq!(before_chat - before, 0, "scrolling allocated");
    assert_eq!(after - before_chat, 0, "streaming allocated");
}

#[test]
fn reading_a_page_never_allocates() {
    use hayate::store::{PageBlock, PageLink, Span};
    let (mut app, _rx) = app(160, 50);
    app.open(Source::Web { url: "https://example.com/".into() });
    app.focus(0);
    let id = app.tabs[0].id;
    let arena = app.tabs[0].arena.clone();
    let mut nlinks = 0u32;
    for i in 0..400u32 {
        let kind = [0u8, 2, 7, 7, 8, 9, 11][i as usize % 7];
        let text = format!("block {i}: a paragraph with a link inside it and 日本語 and emoji 😀 and enough words to wrap twice in the column");
        let span = arena.append(text.as_bytes()).unwrap();
        let link = PageLink { start: span.off + 30, len: 4, href: arena.append(b"https://example.com/next").unwrap() };
        arena.publish();
        app.apply(Reply::Link { tab: id, link });
        app.apply(Reply::Block { tab: id, block: PageBlock { kind, depth: 1, text: span, first_link: nlinks, links: 1 } });
        nlinks += 1;
    }
    let _ = Span::EMPTY;
    app.apply(Reply::Done { tab: id, more: false });
    for _ in 0..3 {
        let _ = app.frame().len();
    }
    let before = thread_allocs();
    for k in 0..600 {
        let key = match k % 12 {
            0..=6 => Key::Char('j'),
            7..=8 => Key::Tab,
            9 => Key::Char('k'),
            _ => Key::PageDown,
        };
        app.key(key);
        app.wheel(if k % 2 == 0 { 3 } else { -1 });
        let _ = app.frame().len();
    }
    assert_eq!(thread_allocs() - before, 0, "reading a page allocated");
}
