//! Golden frames: the renderer draws into an in-memory cell grid and the result is
//! compared with stored snapshots, including emoji, telugu and wide characters at line
//! ends. Run with HAYATE_BLESS=1 to rewrite the snapshots after a deliberate change.

mod common;

use common::*;
use hayate::msg::{Service, Source};
use hayate::store::{Role, F_ROOT};
use hayate::sys::Key;

fn check(name: &str, got: &str) {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/golden").join(format!("{name}.txt"));
    if std::env::var("HAYATE_BLESS").is_ok() || !path.exists() {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, got).unwrap();
        return;
    }
    let want = std::fs::read_to_string(&path).unwrap().replace("\r\n", "\n");
    if want != got {
        for (i, (a, b)) in want.lines().zip(got.lines()).enumerate() {
            if a != b {
                panic!("{name}: line {i} differs\nwant: {a:?}\n got: {b:?}");
            }
        }
        panic!("{name}: line count differs");
    }
}

#[test]
fn feed_with_wide_text_and_threads() {
    let (mut app, _rx) = app(60, 24);
    app.open(Source::XHome);
    app.focus(0);
    let a = author(&mut app, "Pavan", "pavan");
    let b = author(&mut app, "疾風 bot", "hayate");
    post(&mut app, 0, "3", a, 3 * 3600, "family 👨‍👩‍👧 and flag 🇮🇳 and heart ❤️ in one line", 0, 0);
    post(&mut app, 0, "2", b, 2 * 3600, "తెలుగు అక్షరాలు: క్ష త్ర శ్రీ stay in their cells, and so does 日本語のテキスト at the very end of a line", 0, 0);
    post(&mut app, 0, "1", a, 3600, "a link https://example.com/a?b=1 then more words to wrap across the box edge", 0, 0);
    done(&mut app, 0);
    check("feed", &screen(&mut app));
    // the cursor moves; only the selection bar changes
    app.key(Key::Char('j'));
    check("feed_cursor", &screen(&mut app));
}

#[test]
fn thread_is_indented() {
    let (mut app, _rx) = app(60, 16);
    app.open(Source::XThread { root: "1".into() });
    app.focus(0);
    let a = author(&mut app, "root author", "root");
    let b = author(&mut app, "replier", "re");
    post(&mut app, 0, "1", a, 7200, "the root post of a thread", F_ROOT, 0);
    post(&mut app, 0, "2", b, 3600, "a reply", 0, 1);
    post(&mut app, 0, "3", a, 1800, "a reply to the reply, wrapped because it is long enough to wrap", 0, 2);
    done(&mut app, 0);
    check("thread", &screen(&mut app));
}

#[test]
fn chat_with_code_and_streaming() {
    let (mut app, _rx) = app(60, 20);
    app.open(Source::Chat { service: Service::Claude, chat: 1 });
    app.focus(0);
    done(&mut app, 0);
    msg(&mut app, 0, Role::User, "", "show me a loop");
    msg(&mut app, 0, Role::Assistant, "claude-opus-5-5", "here:\n```\nfor i in 0..3 {\n    println!(\"{i}\");\n}\n```\nthat prints 0, 1, 2 😀");
    done(&mut app, 0);
    for c in "next question".chars() {
        app.key(Key::Char(c));
    }
    check("chat", &screen(&mut app));
}

#[test]
fn help_and_picker_overlays() {
    let (mut app, _rx) = app(80, 26);
    app.open(Source::XHome);
    app.focus(0);
    done(&mut app, 0);
    app.key(Key::Char('?'));
    check("help", &screen(&mut app));
    app.key(Key::Esc);
    app.key(Key::Char('t'));
    check("picker", &screen(&mut app));
}

#[test]
fn compose_counts_like_x() {
    let (mut app, _rx) = app(60, 12);
    app.open(Source::XHome);
    app.focus(0);
    done(&mut app, 0);
    app.key(Key::Char('n'));
    for c in "日本 and https://example.com/long/path".chars() {
        app.key(Key::Char(c));
    }
    check("compose", &screen(&mut app));
}

#[test]
fn closing_and_reopening_a_chat_keeps_its_id() {
    use hayate::msg::Reply;
    let (mut app, _rx) = app(60, 12);
    app.open(Source::Chat { service: Service::Claude, chat: 1 });
    app.focus(0);
    let id = app.tabs[0].id;
    app.apply(Reply::ChatId { tab: id, service: Service::Claude, chat: 777 });
    app.key(Key::Esc);
    app.key(Key::Char('q'));
    // closing the last tab leaves a start page; reopen from a feed tab
    app.open(Source::XHome);
    app.focus(1);
    app.key(Key::Char('u'));
    assert_eq!(app.tabs.last().unwrap().src, Source::Chat { service: Service::Claude, chat: 777 });
}

#[test]
fn wheel_scrolls_by_rows_not_entries() {
    let (mut app, _rx) = app(60, 12);
    app.open(Source::XHome);
    app.focus(0);
    let a = author(&mut app, "Pavan", "pavan");
    for i in 0..20u64 {
        post(&mut app, 0, &format!("{}", 100 + i), a, 60 * (i + 1), &format!("post number {i} with enough words to take two rows of this narrow screen"), 0, 0);
    }
    done(&mut app, 0);
    let before = screen(&mut app);
    app.wheel(1);
    let after = screen(&mut app);
    let b: Vec<&str> = before.lines().collect();
    let a2: Vec<&str> = after.lines().collect();
    // everything below the header moved up exactly one row (the selection bar aside)
    let clean = |l: &str| l.replace('▎', " ").trim().to_string();
    for r in 2..b.len() - 1 {
        assert_eq!(clean(b[r + 1]), clean(a2[r]), "row {r}");
    }
}
