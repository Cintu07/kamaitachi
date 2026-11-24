//! Video is mpv's job. hayate starts mpv (mpv uses yt-dlp, and yt-dlp uses the deno
//! runtime to get youtube's stream urls) with hardware decoding on the gpu. mpv owns
//! every video pixel and its memory. While it plays, hayate waits on the process handle:
//! no frames, no timers.
//!
//! The resume position is read from mpv's own watch-later file when it exits, so there is
//! no stream of position events to wake hayate while the video plays.

use std::fs;
use std::os::windows::process::CommandExt;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};

use crate::config::Config;
use crate::sys;

const CREATE_NO_WINDOW: u32 = 0x0800_0000;

pub struct Playing {
    child: Child,
    dir: PathBuf,
}

impl Playing {
    pub fn handle(&self) -> sys::Handle {
        sys::child_handle(&self.child)
    }

    /// mpv exited: the position it saved (None when the video played to the end) and the
    /// last error line if it failed.
    pub fn finish(mut self) -> (Option<u32>, Option<String>) {
        let status = self.child.wait().ok();
        let mut pos = None;
        if let Ok(rd) = fs::read_dir(&self.dir) {
            for e in rd.flatten() {
                if e.file_name() == "stderr.txt" {
                    continue;
                }
                if let Ok(text) = fs::read_to_string(e.path()) {
                    for line in text.lines() {
                        if let Some(v) = line.strip_prefix("start=") {
                            pos = v.trim().parse::<f64>().ok().map(|s| s as u32);
                        }
                    }
                }
            }
        }
        let mut err = None;
        if status.is_some_and(|s| !s.success()) {
            let log = fs::read_to_string(self.dir.join("stderr.txt")).unwrap_or_default();
            let last = log.lines().rev().find(|l| !l.trim().is_empty()).unwrap_or("mpv failed").trim();
            err = Some(format!("mpv: {last}"));
        }
        let _ = fs::remove_dir_all(&self.dir);
        (pos, err)
    }
}

fn valid_video_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= 64 && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

pub fn play(cfg: &Config, video: &str, start: u32) -> Result<Playing, String> {
    if !valid_video_id(video) {
        return Err("not a youtube video id".into());
    }
    let dir = cfg.data_dir.join("mpv").join(video);
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let stderr = fs::File::create(dir.join("stderr.txt")).map_err(|e| e.to_string())?;
    let child = Command::new("mpv")
        .arg("--force-window=immediate")
        .arg("--hwdec=auto-safe")
        .arg("--save-position-on-quit")
        .arg(format!("--watch-later-dir={}", dir.display()))
        .arg("--ytdl-format=bestvideo[height<=?1080]+bestaudio/best")
        .arg(format!("--start={start}"))
        .arg("--msg-level=all=error")
        .arg(format!("https://www.youtube.com/watch?v={video}"))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::from(stderr))
        .creation_flags(CREATE_NO_WINDOW)
        .spawn()
        .map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                "mpv is not installed: winget install mpv yt-dlp.yt-dlp denoland.deno".to_string()
            } else {
                format!("mpv: {e}")
            }
        })?;
    Ok(Playing { child, dir })
}

/// yt-dlp is updated on start when a new release exists, at most once a day.
pub fn update_ytdlp_daily(cfg: &Config) {
    let stamp = cfg.data_dir.join("ytdlp.checked");
    let fresh = fs::metadata(&stamp).and_then(|m| m.modified()).ok().and_then(|t| t.elapsed().ok()).is_some_and(|d| d.as_secs() < 86_400);
    if fresh {
        return;
    }
    let _ = fs::write(&stamp, b"");
    let _ = Command::new("yt-dlp")
        .arg("-U")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .creation_flags(CREATE_NO_WINDOW)
        .spawn();
}

#[cfg(test)]
mod tests {
    #[test]
    fn video_ids() {
        assert!(super::valid_video_id("dQw4w9WgXcQ"));
        assert!(!super::valid_video_id("a b"));
        assert!(!super::valid_video_id("--exec=calc"));
    }
}
