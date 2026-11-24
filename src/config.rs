//! One small text file: keys, colors, caps, prices, default models. No gui, no sync.
//! Lives at %APPDATA%\hayate\config.txt and is written with defaults on first run.

use std::path::{Path, PathBuf};

pub const DEFAULT: &str = r#"# hayate config. lines are `key = value`. restart hayate after editing.

# tabs opened at start: start x:home x:mentions yt:subs claude chatgpt hn web:<address>
# a service tab only opens once that service is connected
tabs = start hn x:home yt:subs claude chatgpt

# --- claude (anthropic messages api)
claude.model = claude-opus-5-5
claude.max_tokens = 16000
# low, medium, high, xhigh, max
claude.effort = medium
claude.cap_usd = 20

# --- chatgpt (openai responses api)
chatgpt.model = gpt-5
chatgpt.max_tokens = 16000
chatgpt.cap_usd = 20

# --- prices, usd per million tokens: input output. prices live here, not in code.
price.claude-opus-5-5 = 4 20
price.claude-sonnet-5-5 = 2 10
price.claude-haiku-5-5 = 0.1 0.5
price.claude-fable-5-2 = 8 40
price.gpt-5 = 1.25 10
price.gpt-5-mini = 0.25 2

# --- x api v2, pay per use. set these from your x developer console:
# usd per post read and per write (post, reply, like). the defaults are placeholders.
x.price_read = 0.005
x.price_write = 0.01
x.cap_usd = 10
x.page = 50

# --- youtube data api v3
youtube.daily_quota = 10000
# channel ids to follow when you have not signed in with `hayate auth youtube`
youtube.channels =
youtube.per_channel = 4

# --- system prompt presets for chats. switch with :system <name>
system.default = You are talking to an engineer in a terminal. Be direct and brief. Plain text, no markdown tables.
system.code = You are a senior engineer. Answer with code first, short explanations after.

# --- colors. the theme is built in; override any slot with color.<name> = #rrggbb
# names: bg fg muted accent author link ok warn err bar_bg bar_fg sel_bg user bot border
# code body faint surface x youtube claude chatgpt web accent_bg code_bg focus_bg history
"#;


#[derive(Clone, Debug)]
pub struct Price {
    pub model: String,
    pub input: f64,
    pub output: f64,
}

#[derive(Clone, Debug)]
pub struct Config {
    pub tabs: Vec<String>,
    pub claude_model: String,
    pub claude_max_tokens: u32,
    pub claude_effort: String,
    pub openai_model: String,
    pub openai_max_tokens: u32,
    pub cap_usd: [f64; 3], // claude, chatgpt, x
    pub prices: Vec<Price>,
    pub x_price_read: f64,
    pub x_price_write: f64,
    pub x_page: u32,
    pub yt_daily_quota: u32,
    pub yt_channels: Vec<String>,
    pub yt_per_channel: u32,
    pub systems: Vec<(String, String)>,
    pub palette: [[u8; 3]; crate::render::SLOTS],
    pub data_dir: PathBuf,
    pub config_path: PathBuf,
    /// when set, every connection goes in plain tcp to this address (record/replay tests)
    pub replay_addr: Option<String>,
}

impl Config {
    pub fn price(&self, model: &str) -> Option<&Price> {
        // exact match first, then the longest prefix (dated or suffixed variants)
        self.prices.iter().find(|p| p.model == model).or_else(|| {
            self.prices.iter().filter(|p| model.starts_with(&p.model)).max_by_key(|p| p.model.len())
        })
    }

    pub fn system(&self, name: &str) -> Option<&str> {
        self.systems.iter().find(|(n, _)| n == name).map(|(_, v)| v.as_str())
    }

    pub fn parse(text: &str, data_dir: PathBuf, config_path: PathBuf) -> Result<Config, String> {
        let mut c = Config {
            tabs: vec![],
            claude_model: String::new(),
            claude_max_tokens: 16000,
            claude_effort: "medium".into(),
            openai_model: String::new(),
            openai_max_tokens: 16000,
            cap_usd: [20.0, 20.0, 10.0],
            prices: vec![],
            x_price_read: 0.005,
            x_price_write: 0.01,
            x_page: 50,
            yt_daily_quota: 10_000,
            yt_channels: vec![],
            yt_per_channel: 4,
            systems: vec![],
            palette: crate::render::default_palette(),
            data_dir,
            config_path,
            replay_addr: None,
        };
        for (n, line) in text.lines().enumerate() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let Some((k, v)) = line.split_once('=') else {
                return Err(format!("config line {}: expected `key = value`", n + 1));
            };
            let (k, v) = (k.trim(), v.trim());
            let bad = |what: &str| format!("config line {}: {k}: {what}", n + 1);
            let num = |v: &str| v.parse::<f64>().map_err(|_| bad("not a number"));
            match k {
                "tabs" => c.tabs = v.split_whitespace().map(String::from).collect(),
                "claude.model" => c.claude_model = v.into(),
                "claude.max_tokens" => c.claude_max_tokens = num(v)? as u32,
                "claude.effort" => c.claude_effort = v.into(),
                "claude.cap_usd" => c.cap_usd[0] = num(v)?,
                "chatgpt.model" => c.openai_model = v.into(),
                "chatgpt.max_tokens" => c.openai_max_tokens = num(v)? as u32,
                "chatgpt.cap_usd" => c.cap_usd[1] = num(v)?,
                "x.price_read" => c.x_price_read = num(v)?,
                "x.price_write" => c.x_price_write = num(v)?,
                "x.cap_usd" => c.cap_usd[2] = num(v)?,
                "x.page" => c.x_page = (num(v)? as u32).clamp(5, 100),
                "youtube.daily_quota" => c.yt_daily_quota = num(v)? as u32,
                "youtube.channels" => c.yt_channels = v.split([' ', ',']).filter(|s| !s.is_empty()).map(String::from).collect(),
                "youtube.per_channel" => c.yt_per_channel = (num(v)? as u32).clamp(1, 50),
                _ if k.starts_with("price.") => {
                    let mut it = v.split_whitespace();
                    let (Some(i), Some(o)) = (it.next(), it.next()) else { return Err(bad("expected `input output`")) };
                    c.prices.push(Price { model: k[6..].into(), input: num(i)?, output: num(o)? });
                }
                _ if k.starts_with("system.") => c.systems.push((k[7..].into(), v.into())),
                _ if k.starts_with("color.") => {
                    let name = &k[6..];
                    let Some(slot) = crate::render::THEME.iter().position(|(n, _)| *n == name) else { return Err(bad("unknown color")) };
                    c.palette[slot] = parse_hex(v).ok_or_else(|| bad("expected #rrggbb"))?;
                }
                _ => return Err(bad("unknown key")),
            }
        }
        Ok(c)
    }

    /// Loads the config, writing the defaults first if there is none.
    pub fn load() -> Result<Config, String> {
        let appdata = std::env::var_os("APPDATA").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("."));
        let local = std::env::var_os("LOCALAPPDATA").map(PathBuf::from).unwrap_or_else(|| appdata.clone());
        let config_path = std::env::var_os("HAYATE_CONFIG").map(PathBuf::from).unwrap_or_else(|| appdata.join("hayate").join("config.txt"));
        let data_dir = std::env::var_os("HAYATE_DATA").map(PathBuf::from).unwrap_or_else(|| local.join("hayate"));
        if !config_path.exists() {
            if let Some(p) = config_path.parent() {
                std::fs::create_dir_all(p).map_err(|e| e.to_string())?;
            }
            std::fs::write(&config_path, DEFAULT).map_err(|e| format!("{}: {e}", config_path.display()))?;
        }
        std::fs::create_dir_all(&data_dir).map_err(|e| format!("{}: {e}", data_dir.display()))?;
        let text = std::fs::read_to_string(&config_path).map_err(|e| format!("{}: {e}", config_path.display()))?;
        let mut c = Config::parse(&text, data_dir, config_path)?;
        c.fill_defaults();
        Ok(c)
    }

    /// Defaults for anything the file left out, so an old config keeps working.
    pub fn fill_defaults(&mut self) {
        let d = Config::parse(DEFAULT, PathBuf::new(), PathBuf::new()).expect("default config parses");
        if self.claude_model.is_empty() {
            self.claude_model = d.claude_model;
        }
        if self.openai_model.is_empty() {
            self.openai_model = d.openai_model;
        }
        if self.tabs.is_empty() {
            self.tabs = d.tabs;
        }
        for p in d.prices {
            if !self.prices.iter().any(|q| q.model == p.model) {
                self.prices.push(p);
            }
        }
        for s in d.systems {
            if !self.systems.iter().any(|(n, _)| *n == s.0) {
                self.systems.push(s);
            }
        }
    }

    pub fn defaults_for_tests(dir: &Path) -> Config {
        let mut c = Config::parse(DEFAULT, dir.to_path_buf(), dir.join("config.txt")).unwrap();
        c.fill_defaults();
        c
    }
}

fn parse_hex(v: &str) -> Option<[u8; 3]> {
    let v = v.strip_prefix('#')?;
    if v.len() != 6 {
        return None;
    }
    let n = u32::from_str_radix(v, 16).ok()?;
    Some([(n >> 16) as u8, (n >> 8) as u8, n as u8])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config_parses() {
        let c = Config::defaults_for_tests(Path::new("."));
        assert_eq!(c.claude_model, "claude-opus-5-5");
        assert_eq!(c.price("claude-opus-5-5").unwrap().output, 20.0);
        assert_eq!(c.price("gpt-5-2026-01-01").unwrap().model, "gpt-5");
        assert_eq!(c.palette[0], [0x0a, 0x0a, 0x0b]);
        assert!(c.system("default").is_some());
    }

    #[test]
    fn errors_name_the_line() {
        let e = Config::parse("tabs = x\nnope", PathBuf::new(), PathBuf::new()).unwrap_err();
        assert!(e.contains("line 2"));
    }
}
