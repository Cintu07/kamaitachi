//! hayate (疾風): a terminal client for x, youtube, claude and chatgpt.

use std::sync::Arc;
use std::time::Instant;

use hayate::arena::{ring, MB};
use hayate::config::Config;
use hayate::msg::{Command, Source};
use hayate::net::{self, NetInit};
use hayate::store::TextArena;
use hayate::sys::{self, Console, Event, WinEvent};
use hayate::ui::App;

const USAGE: &str = "hayate (疾風) 0.1: x, youtube, claude and chatgpt in a terminal

usage:
  hayate                      open the client
  hayate key set <name>       store a secret in windows credential manager
  hayate key list             show which secrets are stored
  hayate key delete <name>    remove a secret
  hayate auth youtube         sign in to youtube (your subscriptions)
  hayate auth x               sign in to x (timeline, posting)
  hayate bench                run the budget tests against a local replay server
  hayate demo                 the client on made-up data from a local replay server
  hayate config               print where the config and data live

secret names: anthropic, openai, youtube-key";

const SECRET_NAMES: [&str; 11] = [
    "anthropic",
    "openai",
    "youtube-key",
    "youtube-client-id",
    "youtube-client-secret",
    "youtube-refresh",
    "x-client-id",
    "x-client-secret",
    "x-refresh",
    "x-access",
    "youtube-access",
];

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let r = match args.get(1).map(String::as_str) {
        None => run(),
        Some("key") => key(&args[2..]),
        Some("auth") => match args.get(2).map(String::as_str) {
            Some("youtube") => hayate::auth::cli_youtube(),
            Some("x") => hayate::auth::cli_x(),
            _ => Err("usage: hayate auth youtube | hayate auth x".into()),
        },
        Some("bench") => hayate::bench::run(&args[2..]),
        Some("demo") => demo(),
        Some("filters") => {
            println!("{}", hayate::web::block::report());
            Ok(())
        }
        Some("web") => hayate::gui::run(args[2..].to_vec()),
        Some("shot") => hayate::bench::shots(&args[2..]),
        Some("replay-server") => hayate::bench::serve_stdout(),
        Some("probe") => hayate::bench::probe(&args[2..]),
        Some("config") => Config::load().map(|c| {
            println!("config: {}", c.config_path.display());
            println!("data:   {}", c.data_dir.display());
        }),
        Some("--version" | "-V" | "version") => {
            println!("hayate 0.1.0");
            Ok(())
        }
        Some("help" | "--help" | "-h") => {
            println!("{USAGE}");
            Ok(())
        }
        Some(other) => Err(format!("unknown command `{other}`\n\n{USAGE}")),
    };
    if let Err(e) = r {
        sys::restore_console();
        eprintln!("hayate: {e}");
        std::process::exit(1);
    }
}

fn key(args: &[String]) -> Result<(), String> {
    match (args.first().map(String::as_str), args.get(1)) {
        (Some("set"), Some(name)) => {
            if !SECRET_NAMES.contains(&name.as_str()) {
                return Err(format!("unknown secret `{name}`. names: {}", SECRET_NAMES[..3].join(", ")));
            }
            eprint!("{name}: ");
            let v = sys::read_secret_line().map_err(|e| e.to_string())?;
            eprintln!();
            if v.is_empty() {
                return Err("empty value, nothing stored".into());
            }
            sys::cred_write(name, v.as_bytes()).map_err(|e| e.to_string())?;
            println!("stored hayate/{name} in windows credential manager");
            Ok(())
        }
        (Some("list"), _) => {
            for n in SECRET_NAMES {
                let mut l = sys::Locked::new();
                let set = sys::cred_read(n, &mut l);
                println!("{:<22} {}", n, if set { "set" } else { "-" });
            }
            Ok(())
        }
        (Some("delete"), Some(name)) => {
            if sys::cred_delete(name) {
                println!("removed hayate/{name}");
                Ok(())
            } else {
                Err(format!("hayate/{name} was not stored"))
            }
        }
        _ => Err("usage: hayate key set|list|delete <name>".into()),
    }
}

fn run() -> Result<(), String> {
    run_with(Arc::new(Config::load()?), false)
}

/// The real client against the local replay server: made-up posts, videos and answers,
/// nothing leaves the machine, and the real config and history are not touched.
fn demo() -> Result<(), String> {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let mut server = std::process::Command::new(exe)
        .arg("replay-server")
        .env("HAYATE_BENCH_TOKENS", "700")
        .env("HAYATE_REPLAY_PACE_MS", "12")
        .stdout(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| e.to_string())?;
    let mut addr = String::new();
    std::io::BufRead::read_line(&mut std::io::BufReader::new(server.stdout.take().unwrap()), &mut addr).map_err(|e| e.to_string())?;
    // SAFETY: set before the net thread starts
    unsafe { std::env::set_var("HAYATE_TEST_KEYS", "1") };
    let dir = std::env::temp_dir().join("hayate-demo");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let mut cfg = Config::defaults_for_tests(&dir);
    cfg.replay_addr = Some(addr.trim().to_string());
    cfg.yt_per_channel = 3;
    let r = run_with(Arc::new(cfg), true);
    let _ = server.kill();
    r
}

fn run_with(cfg: Arc<Config>, demo: bool) -> Result<(), String> {
    std::panic::set_hook(Box::new(|info| {
        sys::restore_console();
        eprintln!("hayate crashed: {info}");
    }));
    let console = Console::init().map_err(|e| e.to_string())?;

    let (cmd_tx, cmd_rx) = ring::<Command>(1024);
    let (rep_tx, rep_rx) = ring(4096);
    let cmd_event = Arc::new(WinEvent::new());
    let ui_event = Arc::new(WinEvent::new());
    let authors = Arc::new(TextArena::with_budget(2 * MB));
    let net = net::spawn(NetInit {
        cfg: cfg.clone(),
        cmds: cmd_rx,
        replies: rep_tx,
        cmd_event: cmd_event.clone(),
        ui_event: ui_event.clone(),
        authors: authors.clone(),
    });

    let (w, h) = console.size();
    let mut app = App::new(cfg.clone(), w, h, cmd_tx, cmd_event.clone(), authors);
    for name in &cfg.tabs {
        match Source::from_config(name) {
            // services that are not connected yet stay out of the way; the start page
            // links to them and says how to connect
            Some(src) if demo || hayate::adapters::has_credentials(src.service()) => {
                app.open(src);
            }
            Some(_) => {}
            None => app.say(hayate::msg::Level::Warn, &format!("config: unknown tab `{name}`")),
        }
    }
    if app.tabs.is_empty() {
        app.open(Source::Start);
    }
    app.focus(0);
    if demo {
        app.say(hayate::msg::Level::Info, "demo: made-up data from a local replay server. ? for keys, ctrl+c quits");
    }
    let out = app.frame();
    console.write(out);
    app.startup_us = sys::since_process_start_us();
    hayate::mpv::update_ytdlp_daily(&cfg);
    if let Ok(path) = std::env::var("HAYATE_SCROLLTEST") {
        return scroll_test(&mut app, &console, &rep_rx, &ui_event, &path, net);
    }

    let mut recs = sys::zeroed_records::<128>();
    let mut events = [Event::Resize; 128];
    let mut hi = 0u16;
    loop {
        // one blocking wait: the console, the net thread, and mpv while a video plays
        let mut handles = [console.hin, ui_event.0, console.hin];
        let mut n = 2;
        if let Some(p) = &app.mpv {
            handles[2] = p.handle();
            n = 3;
        }
        let which = sys::wait_any(&handles[..n], sys::INFINITE_WAIT);
        let t_in = Instant::now();

        let k = console.read(&mut recs, &mut events, &mut hi);
        for e in &events[..k] {
            match *e {
                Event::Key(key) => app.key(key),
                Event::Wheel(d) => app.wheel(d),
                Event::Click { x, y } => app.click(x, y),
                Event::Resize => {
                    let (w, h) = console.size();
                    app.resize(w, h);
                }
            }
        }
        while let Some(r) = rep_rx.pop() {
            app.apply(r);
        }
        if which == Some(2) {
            if let Some(p) = app.mpv.take() {
                let (pos, err) = p.finish();
                app.played(pos, err);
            }
        }
        if app.quit {
            break;
        }
        if app.dirty {
            let f0 = Instant::now();
            let out = app.frame();
            console.write(out);
            app.hist.frame(f0.elapsed().as_micros() as u32);
            if k > 0 {
                app.hist.input(t_in.elapsed().as_micros() as u32);
            }
        }
    }
    app.send(Command::Shutdown);
    let _ = net.join();
    sys::restore_console();
    Ok(())
}

/// Scripted scrolling in a real console: times each frame's build and its console write,
/// and counts the bytes written. Results go to a file because the screen is in use.
fn scroll_test(
    app: &mut App,
    console: &Console,
    rep: &hayate::arena::Consumer<hayate::msg::Reply>,
    ui_event: &WinEvent,
    path: &str,
    net: std::thread::JoinHandle<()>,
) -> Result<(), String> {
    use hayate::sys::Key;
    let t0 = Instant::now();
    while t0.elapsed().as_secs() < 20 {
        sys::wait_any(&[ui_event.0], 100);
        while let Some(r) = rep.pop() {
            app.apply(r);
        }
        let out = app.frame();
        console.write(out);
        if app.tabs.iter().all(|t| !t.loading) {
            break;
        }
    }
    let mut report = String::new();
    for (name, key, n) in [("j (next entry)", Key::Char('j'), 300usize), ("wheel down", Key::PageDown, 0), ("k (previous entry)", Key::Char('k'), 300)] {
        let mut build = Vec::new();
        let mut write = Vec::new();
        let mut bytes = Vec::new();
        let steps = if n == 0 { 300 } else { n };
        for _ in 0..steps {
            while let Some(r) = rep.pop() {
                app.apply(r);
            }
            if n == 0 {
                app.wheel(3);
            } else {
                app.key(key);
            }
            let b0 = Instant::now();
            let out = app.frame();
            let b1 = Instant::now();
            console.write(out);
            write.push(b1.elapsed().as_micros() as u64);
            build.push((b1 - b0).as_micros() as u64);
            bytes.push(out.len() as u64);
        }
        let p = |v: &mut Vec<u64>, q: usize| {
            v.sort_unstable();
            v[(v.len() * q / 100).min(v.len() - 1)]
        };
        let avg_b = bytes.iter().sum::<u64>() / bytes.len() as u64;
        report.push_str(&format!(
            "{name:<20} build p50 {:>5} us p99 {:>5} us | console write p50 {:>6} us p99 {:>6} us | bytes/frame avg {:>6} max {:>6}
",
            p(&mut build, 50), p(&mut build, 99), p(&mut write, 50), p(&mut write, 99), avg_b, p(&mut bytes, 100)
        ));
    }
    let _ = std::fs::write(path, report);
    app.send(Command::Shutdown);
    let _ = net.join();
    sys::restore_console();
    Ok(())
}
