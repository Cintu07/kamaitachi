//! x api v2, pay per use. Every post read is billed, so a post fetched once is stored in
//! the log and never fetched again; the timeline asks only for posts newer than the
//! newest one held, and scrolling back reads the log before it reads the api.

use std::sync::OnceLock;

use super::*;
use crate::json::Paths;
use crate::net::Host;

crate::json_paths!(FEED {
    "data[*]" => R_POST, Record;
    "data[*].id" => SID, Field;
    "data[*].text" => TEXT, Field;
    "data[*].note_tweet.text" => TEXT_LONG, Field;
    "data[*].author_id" => AKEY, Field;
    "data[*].created_at" => TIME, Field;
    "data[*].public_metrics.like_count" => METRIC, Field;
    "data[*].referenced_tweets[*].id" => REF_ID, Field;
    "data[*].referenced_tweets[*].type" => REF_TYPE, Field;
    "data[*].attachments.media_keys[*]" => MEDIA, Field;
    "includes.users[*]" => R_USER, Record;
    "includes.users[*].id" => UKEY, Field;
    "includes.users[*].name" => UNAME, Field;
    "includes.users[*].username" => UHANDLE, Field;
    "meta.next_token" => NEXT, Field;
    "meta.newest_id" => NEWEST, Field;
    "meta.oldest_id" => OLDEST, Field;
    "errors[*].detail" => ERR, Field;
});

fn paths() -> &'static Paths {
    static P: OnceLock<Paths> = OnceLock::new();
    P.get_or_init(|| Paths::compile(FEED))
}

const FIELDS: &str = "tweet.fields=created_at,author_id,conversation_id,public_metrics,referenced_tweets,note_tweet,attachments&expansions=author_id&user.fields=username,name";

/// The bearer header, refreshing the oauth token first if it has expired.
fn bearer(sh: &mut Shared, tab: Option<TabId>) -> Option<Vec<u8>> {
    let now = sys::now_unix();
    if sh.secrets.x_token.is_empty() || sh.secrets.x_exp <= now + 60 {
        match crate::auth::refresh_x(sh.cfg.replay_addr.as_deref()) {
            Ok((exp, tok)) => {
                sh.secrets.x_exp = exp;
                sh.secrets.x_token.set(tok.as_bytes());
            }
            Err(e) => {
                sh.status(tab, Level::Error, &format!("x: {e}"));
                if let Some(t) = tab {
                    done(sh, t, false);
                }
                return None;
            }
        }
    }
    let mut v = b"Bearer ".to_vec();
    v.extend_from_slice(sh.secrets.x_token.as_bytes());
    Some(v)
}

fn req(sh: &mut Shared, tab: TabId, target: String, kind: FeedKind, max: u32, what: String) -> Option<Req> {
    let est = (max as f64 * sh.cfg.x_price_read * 1e6) as u64;
    if let Err(e) = sh.allow(Service::X, est, 0) {
        sh.status(Some(tab), Level::Warn, &e);
        done(sh, tab, false);
        return None;
    }
    let auth = bearer(sh, Some(tab))?;
    let mut job = FeedJob::new(paths(), tab, kind, Service::X, true);
    if matches!(kind, FeedKind::XThreadRoot | FeedKind::XThreadReplies) {
        job.st.collect = Some(Vec::new());
    }
    if kind == FeedKind::XThreadRoot {
        job.st.extra_flags = F_ROOT;
    }
    Some(Req {
        tab: Some(tab),
        service: Service::X,
        host: Host::X,
        method: "GET",
        target: target.into_bytes(),
        headers: vec![("Authorization", auth)],
        body: Vec::new(),
        what,
        job: Job::Feed(job),
        est_micros: est,
        units: 0,
        idle_timeout: 30,
    })
}

fn me(sh: &mut Shared) -> Option<String> {
    sh.ad.meta.get("x.me.id").cloned()
}

fn ask_me(sh: &mut Shared, tab: TabId) {
    let Some(auth) = bearer(sh, Some(tab)) else { return };
    sh.followups.push(Req {
        tab: Some(tab),
        service: Service::X,
        host: Host::X,
        method: "GET",
        target: b"/2/users/me".to_vec(),
        headers: vec![("Authorization", auth)],
        body: Vec::new(),
        what: "users/me".into(),
        job: Job::Small(Small::XMe { tab }, Vec::new()),
        est_micros: 0,
        units: 0,
        idle_timeout: 30,
    });
}

pub fn open(sh: &mut Shared, tab: TabId) {
    let src = sh.tabs[&tab].src.clone();
    match src {
        Source::XThread { root } => thread(sh, tab, &root),
        _ => {
            let n = restore(sh, tab, 100, None);
            if n > 0 {
                sh.status(Some(tab), Level::Info, &format!("{n} posts from the local cache"));
            }
            refresh(sh, tab);
        }
    }
}

fn timeline_target(sh: &Shared, src: &Source, me: &str, since: &str, until: &str, token: &str) -> (String, u32, String) {
    let page = sh.cfg.x_page;
    let mut t = match src {
        Source::XHome => format!("/2/users/{me}/timelines/reverse_chronological?max_results={page}&{FIELDS}"),
        Source::XMentions => format!("/2/users/{me}/mentions?max_results={page}&{FIELDS}"),
        Source::XSearch { q } => {
            let mut v = Vec::new();
            crate::net::http::push_query(&mut v, q);
            format!("/2/tweets/search/recent?query={}&max_results={}&{FIELDS}", String::from_utf8_lossy(&v), page.max(10))
        }
        _ => unreachable!(),
    };
    // older pages ask for posts before the oldest held (until_id); a pagination token is
    // only used when nothing is held yet
    if !until.is_empty() {
        t.push_str("&until_id=");
        t.push_str(until);
    } else if !token.is_empty() {
        t.push_str("&pagination_token=");
        t.push_str(token);
    } else if !since.is_empty() {
        t.push_str("&since_id=");
        t.push_str(since);
    }
    let what = match src {
        Source::XHome => "home timeline",
        Source::XMentions => "mentions",
        _ => "search",
    };
    (t, page, what.to_string())
}

pub fn refresh(sh: &mut Shared, tab: TabId) {
    let src = sh.tabs[&tab].src.clone();
    match &src {
        Source::XThread { root } => {
            let root = root.to_string();
            replies(sh, tab, &root);
        }
        Source::XHome | Source::XMentions | Source::XSearch { .. } => {
            let me = if matches!(src, Source::XSearch { .. }) { String::new() } else {
                match me(sh) {
                    Some(m) => m,
                    None => return ask_me(sh, tab),
                }
            };
            let since = sh.tabs[&tab].newest_sid.clone();
            let (target, page, what) = timeline_target(sh, &src, &me, &since, "", "");
            if let Some(r) = req(sh, tab, target, FeedKind::XTimeline, page, format!("{what} (newer)")) {
                sh.followups.push(r);
            }
        }
        _ => {}
    }
}

pub fn more(sh: &mut Shared, tab: TabId) {
    let t = &sh.tabs[&tab];
    let (src, oldest_ts, oldest, token, exhausted) = (t.src.clone(), t.oldest_ts, t.oldest_sid.clone(), t.next_token.clone(), t.log_exhausted);
    if matches!(src, Source::XThread { .. }) {
        return done(sh, tab, false);
    }
    // the log first: older posts already paid for
    if !exhausted && oldest_ts > 0 {
        let n = restore(sh, tab, 50, Some(oldest_ts));
        if n > 0 {
            return done(sh, tab, true);
        }
        sh.tabs.get_mut(&tab).unwrap().log_exhausted = true;
    }
    let me = if matches!(src, Source::XSearch { .. }) { String::new() } else {
        match me(sh) {
            Some(m) => m,
            None => return ask_me(sh, tab),
        }
    };
    let (target, page, what) = timeline_target(sh, &src, &me, "", &oldest, &token);
    if let Some(r) = req(sh, tab, target, FeedKind::XTimeline, page, format!("{what} (older)")) {
        sh.followups.push(r);
    }
}

fn thread(sh: &mut Shared, tab: TabId, root: &str) {
    sh.tabs.get_mut(&tab).unwrap().thread.clear();
    if let Some(rec) = find_logged(sh, Service::X, root.as_bytes()) {
        if let Some(lp) = read_post(&rec) {
            emit_logged(sh, tab, &lp, F_ROOT, true);
            return replies(sh, tab, root);
        }
    }
    let target = format!("/2/tweets?ids={root}&{FIELDS}");
    if let Some(r) = req(sh, tab, target, FeedKind::XThreadRoot, 1, "thread root".into()) {
        sh.followups.push(r);
    }
}

fn replies(sh: &mut Shared, tab: TabId, root: &str) {
    let target = format!("/2/tweets/search/recent?query=conversation_id:{root}&max_results=100&{FIELDS}");
    if let Some(r) = req(sh, tab, target, FeedKind::XThreadReplies, 100, "thread replies".into()) {
        sh.followups.push(r);
    }
}

/// Orders a thread as a tree: root first, then replies depth-first, oldest first.
fn emit_thread(sh: &mut Shared, tab: TabId) {
    let Some(t) = sh.tabs.get_mut(&tab) else { return };
    let mut posts = std::mem::take(&mut t.thread);
    let arena = t.arena.clone();
    posts.sort_by_key(|(p, _)| (p.flags & F_ROOT == 0, p.ts));
    let ids: Vec<u32> = posts.iter().map(|(p, _)| sh.interner.intern(arena.peek(p.sid))).collect();
    let root = 0usize;
    let mut kids: Vec<Vec<usize>> = vec![Vec::new(); posts.len()];
    for i in 1..posts.len() {
        let parent = posts[i].1;
        let pi = ids.iter().position(|&h| h == parent).filter(|&p| p != i).unwrap_or(root);
        kids[pi].push(i);
    }
    let mut order = Vec::with_capacity(posts.len());
    let mut stack = vec![(root, 0u8)];
    let mut seen = vec![false; posts.len()];
    while let Some((i, d)) = stack.pop() {
        if posts.is_empty() || seen[i] {
            continue;
        }
        seen[i] = true;
        order.push((i, d));
        for &k in kids[i].iter().rev() {
            stack.push((k, (d + 1).min(8)));
        }
    }
    for (i, d) in order {
        let mut p = posts[i].0;
        p.depth = d;
        sh.out.push(Reply::Post { tab, post: p });
    }
}

pub fn feed_done(sh: &mut Shared, st: FeedState) -> u32 {
    let micros = (st.raw as f64 * sh.cfg.x_price_read * 1e6) as u64;
    sh.charge(Service::X, micros, 0);
    let tab = st.tab;
    if !st.err.is_empty() && st.count == 0 {
        sh.status(Some(tab), Level::Warn, &format!("x: {}", st.err));
    }
    if !sh.tabs.contains_key(&tab) {
        return micros as u32;
    }
    match st.kind {
        FeedKind::XThreadRoot => {
            if let Some(v) = st.collect {
                sh.tabs.get_mut(&tab).unwrap().thread.extend(v);
            }
            let root = match &sh.tabs[&tab].src {
                Source::XThread { root } => root.to_string(),
                _ => String::new(),
            };
            replies(sh, tab, &root);
        }
        FeedKind::XThreadReplies => {
            if let Some(v) = st.collect {
                sh.tabs.get_mut(&tab).unwrap().thread.extend(v);
            }
            emit_thread(sh, tab);
            done(sh, tab, false);
        }
        _ => {
            let t = sh.tabs.get_mut(&tab).unwrap();
            t.next_token = st.next.clone();
            if st.count > 0 {
                sh.status(Some(tab), Level::Info, &format!("{} new posts · ${:.3}", st.count, micros as f64 / 1e6));
            }
            done(sh, tab, true);
        }
    }
    micros as u32
}

pub fn me_done(sh: &mut Shared, tab: TabId, body: &[u8]) -> u32 {
    crate::json_paths!(ME { "data.id" => 1, Field; "data.username" => 2, Field; });
    let c = crate::json::collect(ME, body);
    match c.get(1) {
        Some(id) => {
            let (id, user) = (id.to_string(), c.get(2).unwrap_or("").to_string());
            set_meta(sh, "x.me.id", &id);
            set_meta(sh, "x.me.username", &user);
            if sh.tabs.contains_key(&tab) {
                refresh(sh, tab);
            }
        }
        None => {
            sh.status(Some(tab), Level::Error, "x: could not read your user id");
            done(sh, tab, false);
        }
    }
    0
}

fn write_req(sh: &mut Shared, tab: TabId, target: String, body: Vec<u8>, what: &str, job: Small) {
    let est = (sh.cfg.x_price_write * 1e6) as u64;
    if let Err(e) = sh.allow(Service::X, est, 0) {
        return sh.status(Some(tab), Level::Warn, &e);
    }
    let Some(auth) = bearer(sh, Some(tab)) else { return };
    sh.followups.push(Req {
        tab: None,
        service: Service::X,
        host: Host::X,
        method: "POST",
        target: target.into_bytes(),
        headers: vec![("Authorization", auth), ("Content-Type", b"application/json".to_vec())],
        body,
        what: what.into(),
        job: Job::Small(job, Vec::new()),
        est_micros: est,
        units: 0,
        idle_timeout: 30,
    });
}

pub fn post(sh: &mut Shared, tab: TabId, text: &str, reply_to: Option<&str>) {
    let mut b = b"{\"text\":".to_vec();
    crate::json::push_str(&mut b, text);
    if let Some(r) = reply_to {
        b.extend_from_slice(b",\"reply\":{\"in_reply_to_tweet_id\":");
        crate::json::push_str(&mut b, r);
        b.push(b'}');
    }
    b.push(b'}');
    let what = if reply_to.is_some() { "reply" } else { "post" };
    write_req(sh, tab, "/2/tweets".into(), b, what, Small::XPosted { tab });
}

pub fn like(sh: &mut Shared, tab: TabId, sid: &str) {
    let Some(me) = me(sh) else {
        return sh.status(Some(tab), Level::Warn, "x: open your home timeline once first so hayate knows your user id");
    };
    let mut b = b"{\"tweet_id\":".to_vec();
    crate::json::push_str(&mut b, sid);
    b.push(b'}');
    write_req(sh, tab, format!("/2/users/{me}/likes"), b, "like", Small::XLiked { tab, sid: sid.to_string() });
}

pub fn posted(sh: &mut Shared, tab: TabId, body: &[u8]) -> u32 {
    crate::json_paths!(P { "data.id" => 1, Field; });
    let micros = (sh.cfg.x_price_write * 1e6) as u64;
    sh.charge(Service::X, micros, 0);
    match crate::json::collect(P, body).get(1) {
        Some(id) => sh.status(Some(tab), Level::Info, &format!("posted · https://x.com/i/status/{id}")),
        None => sh.status(Some(tab), Level::Warn, "x: the post may not have gone out"),
    }
    micros as u32
}

pub fn liked(sh: &mut Shared, tab: TabId, sid: &str) -> u32 {
    let micros = (sh.cfg.x_price_write * 1e6) as u64;
    sh.charge(Service::X, micros, 0);
    sh.out.push(Reply::Liked { tab, sid_hash: key_hash(sid) });
    sh.status(Some(tab), Level::Info, "liked");
    micros as u32
}
