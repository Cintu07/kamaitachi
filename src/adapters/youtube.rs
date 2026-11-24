//! youtube data api v3. The adapter knows each call's quota cost and spends the day's
//! units on what you open: the subscription feed reads each channel's uploads playlist
//! (1 unit) instead of search (100 units).

use std::sync::OnceLock;

use super::*;
use crate::json::Paths;
use crate::net::Host;

pub const UNITS_LIST: u32 = 1;
pub const UNITS_SEARCH: u32 = 100;

crate::json_paths!(PLAYLIST {
    "items[*]" => R_POST, Record;
    "items[*].snippet.resourceId.videoId" => SID, Field;
    "items[*].snippet.title" => TEXT, Field;
    "items[*].snippet.publishedAt" => TIME, Field;
    "items[*].snippet.videoOwnerChannelTitle" => ANAME, Field;
    "items[*].snippet.videoOwnerChannelId" => AKEY, Field;
    "nextPageToken" => NEXT, Field;
    "error.message" => ERR, Field;
});

crate::json_paths!(SEARCH {
    "items[*]" => R_POST, Record;
    "items[*].id.videoId" => SID, Field;
    "items[*].snippet.title" => TEXT, Field;
    "items[*].snippet.channelTitle" => ANAME, Field;
    "items[*].snippet.channelId" => AKEY, Field;
    "items[*].snippet.publishedAt" => TIME, Field;
    "nextPageToken" => NEXT, Field;
});

crate::json_paths!(VIDEO {
    "items[*]" => R_POST, Record;
    "items[*].id" => SID, Field;
    "items[*].snippet.title" => TEXT, Field;
    "items[*].snippet.description" => EXTRA, Field;
    "items[*].snippet.channelTitle" => ANAME, Field;
    "items[*].snippet.channelId" => AKEY, Field;
    "items[*].snippet.publishedAt" => TIME, Field;
    "items[*].statistics.viewCount" => METRIC, Field;
});

crate::json_paths!(COMMENTS {
    "items[*]" => R_POST, Record;
    "items[*].snippet.topLevelComment.id" => SID, Field;
    "items[*].snippet.topLevelComment.snippet.textDisplay" => TEXT, Field;
    "items[*].snippet.topLevelComment.snippet.authorDisplayName" => ANAME, Field;
    "items[*].snippet.topLevelComment.snippet.authorChannelId.value" => AKEY, Field;
    "items[*].snippet.topLevelComment.snippet.publishedAt" => TIME, Field;
    "items[*].snippet.topLevelComment.snippet.likeCount" => METRIC, Field;
    "items[*].replies.comments[*]" => R_REPLY, Record;
    "items[*].replies.comments[*].id" => SID, Field;
    "items[*].replies.comments[*].snippet.textDisplay" => TEXT, Field;
    "items[*].replies.comments[*].snippet.authorDisplayName" => ANAME, Field;
    "items[*].replies.comments[*].snippet.authorChannelId.value" => AKEY, Field;
    "items[*].replies.comments[*].snippet.publishedAt" => TIME, Field;
    "items[*].replies.comments[*].snippet.likeCount" => METRIC, Field;
    "nextPageToken" => NEXT, Field;
});

crate::json_paths!(SUBS {
    "items[*].snippet.resourceId.channelId" => 1, Field;
    "nextPageToken" => 2, Field;
});

fn compiled(which: FeedKind) -> &'static Paths {
    static P: OnceLock<[Paths; 4]> = OnceLock::new();
    let all = P.get_or_init(|| [Paths::compile(PLAYLIST), Paths::compile(SEARCH), Paths::compile(VIDEO), Paths::compile(COMMENTS)]);
    match which {
        FeedKind::YtPlaylist => &all[0],
        FeedKind::YtSearch => &all[1],
        FeedKind::YtVideo => &all[2],
        _ => &all[3],
    }
}

enum Auth {
    Key(Vec<u8>),
    Bearer(Vec<u8>),
}

fn has_oauth() -> bool {
    if std::env::var("HAYATE_TEST_KEYS").is_ok_and(|v| v == "1") {
        return true;
    }
    let mut l = Locked::new();
    sys::cred_read("youtube-refresh", &mut l)
}

fn auth(sh: &mut Shared, need_oauth: bool) -> Result<Auth, String> {
    if has_oauth() {
        let now = sys::now_unix();
        if sh.secrets.yt_token.is_empty() || sh.secrets.yt_exp <= now + 60 {
            let (exp, tok) = crate::auth::refresh_google(sh.cfg.replay_addr.as_deref())?;
            sh.secrets.yt_exp = exp;
            sh.secrets.yt_token.set(tok.as_bytes());
        }
        let mut v = b"Bearer ".to_vec();
        v.extend_from_slice(sh.secrets.yt_token.as_bytes());
        return Ok(Auth::Bearer(v));
    }
    if need_oauth {
        return Err("sign in for your subscriptions: run `hayate auth youtube`, or list channels in youtube.channels".into());
    }
    if sh.secrets.yt_key.is_empty() {
        return Err("no api key: run `hayate key set youtube-key`".into());
    }
    Ok(Auth::Key(sh.secrets.yt_key.as_bytes().to_vec()))
}

fn build(sh: &mut Shared, tab: TabId, mut target: String, units: u32, what: String, job: Job, need_oauth: bool) -> Option<Req> {
    if let Err(e) = sh.allow(Service::YouTube, 0, units) {
        sh.status(Some(tab), Level::Warn, &e);
        return None;
    }
    let mut headers = Vec::new();
    match auth(sh, need_oauth) {
        Ok(Auth::Key(k)) => {
            target.push_str("&key=");
            target.push_str(&String::from_utf8_lossy(&k));
        }
        Ok(Auth::Bearer(b)) => headers.push(("Authorization", b)),
        Err(e) => {
            sh.status(Some(tab), Level::Error, &format!("youtube: {e}"));
            return None;
        }
    }
    Some(Req {
        tab: Some(tab),
        service: Service::YouTube,
        host: Host::Google,
        method: "GET",
        target: target.into_bytes(),
        headers,
        body: Vec::new(),
        what,
        job,
        est_micros: 0,
        units,
        idle_timeout: 30,
    })
}

fn feed(sh: &mut Shared, tab: TabId, kind: FeedKind, target: String, units: u32, what: String) -> bool {
    let html = matches!(kind, FeedKind::YtSearch | FeedKind::YtPlaylist);
    let mut fj = FeedJob::new(compiled(kind), tab, kind, Service::YouTube, html);
    if kind == FeedKind::YtVideo {
        fj.st.extra_flags = F_ROOT;
    }
    let job = Job::Feed(fj);
    match build(sh, tab, target, units, what, job, false) {
        Some(r) => {
            sh.followups.push(r);
            sh.tabs.get_mut(&tab).unwrap().pending += 1;
            true
        }
        None => false,
    }
}

pub fn open(sh: &mut Shared, tab: TabId) {
    let src = sh.tabs[&tab].src.clone();
    match &src {
        Source::YtSubs | Source::YtSearch { .. } => {
            let n = restore(sh, tab, 100, None);
            if n > 0 {
                sh.status(Some(tab), Level::Info, &format!("{n} videos from the local cache"));
            }
            // search results already held are shown without spending 100 units again
            if matches!(src, Source::YtSubs) || n == 0 {
                refresh(sh, tab);
            } else {
                done(sh, tab, true);
            }
        }
        Source::YtComments { .. } => refresh(sh, tab),
        _ => {}
    }
}

pub fn refresh(sh: &mut Shared, tab: TabId) {
    let src = sh.tabs[&tab].src.clone();
    match &src {
        Source::YtSubs => {
            let fresh = sh.ad.meta.get("yt.channels.at").and_then(|v| v.parse::<u64>().ok()).is_some_and(|t| sys::now_unix() < t + 86400);
            if has_oauth() && !fresh {
                return subs_page(sh, tab, String::new(), Vec::new());
            }
            let channels: Vec<String> = if has_oauth() {
                sh.ad.meta.get("yt.channels").map(|v| v.split(' ').filter(|s| !s.is_empty()).map(String::from).collect()).unwrap_or_default()
            } else {
                sh.cfg.yt_channels.clone()
            };
            fetch_uploads(sh, tab, &channels);
        }
        Source::YtSearch { q } => {
            let q = q.to_string();
            search(sh, tab, &q, "");
        }
        Source::YtComments { video } => {
            let video = video.to_string();
            sh.tabs.get_mut(&tab).unwrap().seen.clear();
            let t = format!("/youtube/v3/videos?part=snippet,statistics&id={video}");
            feed(sh, tab, FeedKind::YtVideo, t, UNITS_LIST, "video details".into());
            comments(sh, tab, &video, "");
        }
        _ => {}
    }
    if sh.tabs.get(&tab).is_some_and(|t| t.pending == 0) {
        done(sh, tab, false);
    }
}

fn search(sh: &mut Shared, tab: TabId, q: &str, token: &str) {
    let mut v = Vec::new();
    crate::net::http::push_query(&mut v, q);
    let mut t = format!("/youtube/v3/search?part=snippet&type=video&maxResults=25&q={}", String::from_utf8_lossy(&v));
    if !token.is_empty() {
        t.push_str("&pageToken=");
        t.push_str(token);
    }
    feed(sh, tab, FeedKind::YtSearch, t, UNITS_SEARCH, "search (100 units)".into());
}

fn comments(sh: &mut Shared, tab: TabId, video: &str, token: &str) {
    let mut t = format!("/youtube/v3/commentThreads?part=snippet,replies&videoId={video}&maxResults=50&order=relevance&textFormat=plainText");
    if !token.is_empty() {
        t.push_str("&pageToken=");
        t.push_str(token);
    }
    feed(sh, tab, FeedKind::YtComments, t, UNITS_LIST, "comments".into());
}

fn fetch_uploads(sh: &mut Shared, tab: TabId, channels: &[String]) {
    if channels.is_empty() {
        sh.status(Some(tab), Level::Warn, "youtube: no channels. run `hayate auth youtube` or set youtube.channels in the config");
        return;
    }
    let per = sh.cfg.yt_per_channel;
    for ch in channels {
        // a channel's uploads playlist is its id with UC replaced by UU
        let Some(rest) = ch.strip_prefix("UC") else { continue };
        let t = format!("/youtube/v3/playlistItems?part=snippet&maxResults={per}&playlistId=UU{rest}");
        if !feed(sh, tab, FeedKind::YtPlaylist, t, UNITS_LIST, format!("uploads {ch}")) {
            break;
        }
    }
}

fn subs_page(sh: &mut Shared, tab: TabId, token: String, acc: Vec<String>) {
    let mut t = "/youtube/v3/subscriptions?part=snippet&mine=true&maxResults=50".to_string();
    if !token.is_empty() {
        t.push_str("&pageToken=");
        t.push_str(&token);
    }
    if let Some(r) = build(sh, tab, t, UNITS_LIST, "subscriptions".into(), Job::Small(Small::YtSubs { tab, acc }, Vec::new()), true) {
        sh.followups.push(r);
        sh.tabs.get_mut(&tab).unwrap().pending += 1;
    } else {
        done(sh, tab, false);
    }
}

pub fn subs_done(sh: &mut Shared, tab: TabId, body: &[u8], mut acc: Vec<String>) -> u32 {
    sh.charge(Service::YouTube, 0, UNITS_LIST);
    let Some(t) = sh.tabs.get_mut(&tab) else { return UNITS_LIST };
    t.pending = t.pending.saturating_sub(1);
    let c = crate::json::collect(SUBS, body);
    acc.extend(c.out.iter().filter(|(f, _)| *f == 1).map(|(_, v)| v.clone()));
    match c.get(2) {
        Some(next) if !next.is_empty() && acc.len() < 1000 => subs_page(sh, tab, next.to_string(), acc),
        _ => {
            set_meta(sh, "yt.channels", &acc.join(" "));
            set_meta(sh, "yt.channels.at", &sys::now_unix().to_string());
            fetch_uploads(sh, tab, &acc);
            if sh.tabs.get(&tab).is_some_and(|t| t.pending == 0) {
                done(sh, tab, false);
            }
        }
    }
    UNITS_LIST
}

pub fn more(sh: &mut Shared, tab: TabId) {
    let t = &sh.tabs[&tab];
    let (src, token, oldest_ts, exhausted) = (t.src.clone(), t.next_token.clone(), t.oldest_ts, t.log_exhausted);
    match &src {
        Source::YtSubs => {
            if !exhausted && oldest_ts > 0 && restore(sh, tab, 50, Some(oldest_ts)) > 0 {
                return done(sh, tab, true);
            }
            sh.tabs.get_mut(&tab).unwrap().log_exhausted = true;
            done(sh, tab, false);
        }
        Source::YtSearch { q } if !token.is_empty() => {
            let q = q.to_string();
            search(sh, tab, &q, &token);
        }
        Source::YtComments { video } if !token.is_empty() => {
            let v = video.to_string();
            comments(sh, tab, &v, &token);
        }
        _ => done(sh, tab, false),
    }
}

pub fn feed_done(sh: &mut Shared, st: FeedState, units: u32) -> u32 {
    sh.charge(Service::YouTube, 0, units);
    let tab = st.tab;
    if !st.err.is_empty() {
        sh.status(Some(tab), Level::Warn, &format!("youtube: {}", st.err));
    }
    let Some(t) = sh.tabs.get_mut(&tab) else { return units };
    t.pending = t.pending.saturating_sub(1);
    if matches!(st.kind, FeedKind::YtComments | FeedKind::YtSearch) {
        t.next_token = st.next.clone();
    }
    if t.pending == 0 {
        let more = !t.next_token.is_empty() || st.kind == FeedKind::YtPlaylist;
        done(sh, tab, more);
    }
    units
}
