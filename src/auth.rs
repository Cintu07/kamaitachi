//! Sign-in and token refresh. Secrets live in windows credential manager under
//! `hayate/<name>`; the config holds only names. Nothing here prints a secret.

use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;

use crate::net::conn::{simple, Host};
use crate::net::http::push_query;
use crate::sys::{self, cred_read, cred_write, Locked};

const B64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

pub fn base64(input: &[u8], url: bool) -> String {
    let mut out = String::with_capacity(input.len().div_ceil(3) * 4);
    for c in input.chunks(3) {
        let n = (c[0] as u32) << 16 | (*c.get(1).unwrap_or(&0) as u32) << 8 | *c.get(2).unwrap_or(&0) as u32;
        let take = c.len() + 1;
        for i in 0..4 {
            if i < take {
                let mut ch = B64[(n >> (18 - 6 * i) & 63) as usize];
                if url {
                    ch = match ch {
                        b'+' => b'-',
                        b'/' => b'_',
                        x => x,
                    };
                }
                out.push(ch as char);
            } else if !url {
                out.push('=');
            }
        }
    }
    out
}

fn form(pairs: &[(&str, &[u8])]) -> Vec<u8> {
    let mut b = Vec::new();
    for (i, (k, v)) in pairs.iter().enumerate() {
        if i > 0 {
            b.push(b'&');
        }
        b.extend_from_slice(k.as_bytes());
        b.push(b'=');
        push_query(&mut b, std::str::from_utf8(v).unwrap_or(""));
    }
    b
}

fn cred(name: &str) -> Option<Locked> {
    let mut l = Locked::new();
    if cred_read(name, &mut l) && !l.is_empty() { Some(l) } else { None }
}

crate::json_paths!(TOKEN {
    "access_token" => 1, Field;
    "refresh_token" => 2, Field;
    "expires_in" => 3, Field;
    "error_description" => 4, Field;
    "error" => 5, Field;
    "device_code" => 6, Field;
    "user_code" => 7, Field;
    "verification_url" => 8, Field;
    "interval" => 9, Field;
});

fn token_error(status: u16, body: &[u8]) -> String {
    let c = crate::json::collect(TOKEN, body);
    let msg = c.get(4).or(c.get(5)).unwrap_or("no detail");
    format!("token request failed ({status}): {msg}")
}

/// Uses the stored refresh token to get a new x access token. x rotates refresh tokens,
/// so the new one is stored too.
pub fn refresh_x(plain: Option<&str>) -> Result<(u64, String), String> {
    let id = cred("x-client-id").ok_or("not signed in. run `hayate auth x`")?;
    let refresh = cred("x-refresh").ok_or("not signed in. run `hayate auth x`")?;
    let secret = cred("x-client-secret");
    let body = form(&[("grant_type", b"refresh_token"), ("refresh_token", refresh.as_bytes()), ("client_id", id.as_bytes())]);
    let mut headers: Vec<(&str, Vec<u8>)> = vec![("Content-Type", b"application/x-www-form-urlencoded".to_vec())];
    if let Some(s) = &secret {
        let mut pair = id.as_bytes().to_vec();
        pair.push(b':');
        pair.extend_from_slice(s.as_bytes());
        headers.push(("Authorization", format!("Basic {}", base64(&pair, false)).into_bytes()));
    }
    let h: Vec<(&str, &[u8])> = headers.iter().map(|(k, v)| (*k, v.as_slice())).collect();
    let (status, resp) = simple(Host::X, plain, "POST", b"/2/oauth2/token", &h, &body).map_err(|e| e.to_string())?;
    if status != 200 {
        return Err(token_error(status, &resp));
    }
    store_x_tokens(&resp)
}

fn store_x_tokens(resp: &[u8]) -> Result<(u64, String), String> {
    let c = crate::json::collect(TOKEN, resp);
    let access = c.get(1).ok_or("no access token in the response")?.to_string();
    let exp = sys::now_unix() + c.get(3).and_then(|v| v.parse::<u64>().ok()).unwrap_or(7200);
    if let Some(r) = c.get(2) {
        cred_write("x-refresh", r.as_bytes()).map_err(|e| e.to_string())?;
    }
    cred_write("x-access", format!("{exp} {access}").as_bytes()).map_err(|e| e.to_string())?;
    Ok((exp, access))
}

/// Uses the stored google refresh token to get a youtube access token (one hour).
pub fn refresh_google(plain: Option<&str>) -> Result<(u64, String), String> {
    let id = cred("youtube-client-id").ok_or("run `hayate auth youtube`")?;
    let secret = cred("youtube-client-secret").ok_or("run `hayate auth youtube`")?;
    let refresh = cred("youtube-refresh").ok_or("run `hayate auth youtube`")?;
    let body = form(&[
        ("grant_type", b"refresh_token"),
        ("refresh_token", refresh.as_bytes()),
        ("client_id", id.as_bytes()),
        ("client_secret", secret.as_bytes()),
    ]);
    let h: [(&str, &[u8]); 1] = [("Content-Type", b"application/x-www-form-urlencoded")];
    let (status, resp) = simple(Host::GoogleAuth, plain, "POST", b"/token", &h, &body).map_err(|e| e.to_string())?;
    if status != 200 {
        return Err(token_error(status, &resp));
    }
    let c = crate::json::collect(TOKEN, &resp);
    let access = c.get(1).ok_or("no access token in the response")?.to_string();
    let exp = sys::now_unix() + c.get(3).and_then(|v| v.parse::<u64>().ok()).unwrap_or(3600);
    Ok((exp, access))
}

fn prompt(label: &str, name: &str, secret: bool) -> Result<(), String> {
    if cred(name).is_some() {
        return Ok(());
    }
    print!("{label}: ");
    std::io::stdout().flush().ok();
    let v = if secret {
        let v = sys::read_secret_line().map_err(|e| e.to_string())?;
        println!();
        v
    } else {
        let mut s = String::new();
        std::io::stdin().read_line(&mut s).map_err(|e| e.to_string())?;
        s.trim().to_string()
    };
    if v.is_empty() {
        return Err(format!("{label} is required"));
    }
    cred_write(name, v.as_bytes()).map_err(|e| e.to_string())
}

fn open_browser(url: &str) {
    let _ = std::process::Command::new("rundll32").args(["url.dll,FileProtocolHandler", url]).spawn();
}

/// `hayate auth youtube`: the google device flow. Needs an oauth client of type "tv and
/// limited input devices" from the google cloud console.
pub fn cli_youtube() -> Result<(), String> {
    println!("youtube sign-in. create an oauth client of type \"TVs and Limited Input devices\" in the google cloud console,");
    println!("with the youtube data api v3 enabled, then paste its id and secret.");
    prompt("client id", "youtube-client-id", false)?;
    prompt("client secret", "youtube-client-secret", true)?;
    let id = cred("youtube-client-id").unwrap();
    let secret = cred("youtube-client-secret").unwrap();
    let body = form(&[("client_id", id.as_bytes()), ("scope", b"https://www.googleapis.com/auth/youtube.readonly")]);
    let h: [(&str, &[u8]); 1] = [("Content-Type", b"application/x-www-form-urlencoded")];
    let (status, resp) = simple(Host::GoogleAuth, None, "POST", b"/device/code", &h, &body).map_err(|e| e.to_string())?;
    if status != 200 {
        return Err(token_error(status, &resp));
    }
    let c = crate::json::collect(TOKEN, &resp);
    let device = c.get(6).ok_or("no device code")?.to_string();
    let user_code = c.get(7).unwrap_or("?");
    let url = c.get(8).unwrap_or("https://www.google.com/device");
    let mut interval = c.get(9).and_then(|v| v.parse::<u64>().ok()).unwrap_or(5);
    println!("open {url} and enter the code: {user_code}");
    open_browser(url);
    loop {
        std::thread::sleep(std::time::Duration::from_secs(interval));
        let body = form(&[
            ("client_id", id.as_bytes()),
            ("client_secret", secret.as_bytes()),
            ("device_code", device.as_bytes()),
            ("grant_type", b"urn:ietf:params:oauth:grant-type:device_code"),
        ]);
        let (status, resp) = simple(Host::GoogleAuth, None, "POST", b"/token", &h, &body).map_err(|e| e.to_string())?;
        let c = crate::json::collect(TOKEN, &resp);
        if status == 200 {
            let refresh = c.get(2).ok_or("google returned no refresh token")?;
            cred_write("youtube-refresh", refresh.as_bytes()).map_err(|e| e.to_string())?;
            println!("signed in. hayate will read your subscriptions.");
            return Ok(());
        }
        match c.get(5) {
            Some("authorization_pending") => {}
            Some("slow_down") => interval += 5,
            _ => return Err(token_error(status, &resp)),
        }
    }
}

/// `hayate auth x`: oauth 2.0 with pkce and a loopback redirect. The app in the x
/// developer console needs http://127.0.0.1:8765/callback as a callback url.
pub fn cli_x() -> Result<(), String> {
    println!("x sign-in. in the x developer console, set the app's callback url to http://127.0.0.1:8765/callback");
    prompt("client id", "x-client-id", false)?;
    print!("client secret (leave empty for a public client): ");
    std::io::stdout().flush().ok();
    let s = sys::read_secret_line().map_err(|e| e.to_string())?;
    println!();
    if !s.is_empty() {
        cred_write("x-client-secret", s.as_bytes()).map_err(|e| e.to_string())?;
    }
    let id = cred("x-client-id").unwrap();
    let rng = ring::rand::SystemRandom::new();
    let mut raw = [0u8; 32];
    ring::rand::SecureRandom::fill(&rng, &mut raw).map_err(|_| "no randomness")?;
    let verifier = base64(&raw, true);
    let challenge = base64(ring::digest::digest(&ring::digest::SHA256, verifier.as_bytes()).as_ref(), true);
    ring::rand::SecureRandom::fill(&rng, &mut raw[..16]).map_err(|_| "no randomness")?;
    let state = base64(&raw[..16], true);
    let redirect = "http://127.0.0.1:8765/callback";
    let mut url = b"https://x.com/i/oauth2/authorize?response_type=code&client_id=".to_vec();
    push_query(&mut url, std::str::from_utf8(id.as_bytes()).unwrap_or(""));
    url.extend_from_slice(b"&redirect_uri=");
    push_query(&mut url, redirect);
    url.extend_from_slice(b"&scope=");
    push_query(&mut url, "tweet.read tweet.write users.read like.write offline.access");
    url.extend_from_slice(format!("&state={state}&code_challenge={challenge}&code_challenge_method=S256").as_bytes());
    let url = String::from_utf8(url).unwrap();
    let listener = TcpListener::bind("127.0.0.1:8765").map_err(|e| format!("port 8765: {e}"))?;
    println!("opening the browser. if it does not open, visit:\n{url}");
    open_browser(&url);
    let (mut stream, _) = listener.accept().map_err(|e| e.to_string())?;
    let mut line = String::new();
    BufReader::new(&stream).read_line(&mut line).map_err(|e| e.to_string())?;
    let _ = stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nConnection: close\r\n\r\nhayate is signed in. you can close this tab.");
    let query = line.split_whitespace().nth(1).and_then(|p| p.split_once('?')).map(|(_, q)| q).unwrap_or("");
    let get = |k: &str| query.split('&').find_map(|kv| kv.strip_prefix(k).and_then(|v| v.strip_prefix('='))).map(str::to_string);
    if get("state").as_deref() != Some(state.as_str()) {
        return Err("state mismatch: try again".into());
    }
    let code = get("code").ok_or("no code in the callback (did you deny access?)")?;
    let body = form(&[
        ("grant_type", b"authorization_code"),
        ("code", code.as_bytes()),
        ("redirect_uri", redirect.as_bytes()),
        ("client_id", id.as_bytes()),
        ("code_verifier", verifier.as_bytes()),
    ]);
    let mut headers: Vec<(&str, Vec<u8>)> = vec![("Content-Type", b"application/x-www-form-urlencoded".to_vec())];
    if let Some(sec) = cred("x-client-secret") {
        let mut pair = id.as_bytes().to_vec();
        pair.push(b':');
        pair.extend_from_slice(sec.as_bytes());
        headers.push(("Authorization", format!("Basic {}", base64(&pair, false)).into_bytes()));
    }
    let h: Vec<(&str, &[u8])> = headers.iter().map(|(k, v)| (*k, v.as_slice())).collect();
    let (status, resp) = simple(Host::X, None, "POST", b"/2/oauth2/token", &h, &body).map_err(|e| e.to_string())?;
    if status != 200 {
        return Err(token_error(status, &resp));
    }
    store_x_tokens(&resp)?;
    println!("signed in to x.");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_matches_rfc4648() {
        assert_eq!(base64(b"", false), "");
        assert_eq!(base64(b"f", false), "Zg==");
        assert_eq!(base64(b"foobar", false), "Zm9vYmFy");
        assert_eq!(base64(&[0xfb, 0xff], true), "-_8");
    }

    #[test]
    fn pkce_challenge_vector() {
        // rfc 7636 appendix b
        let v = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk";
        let c = base64(ring::digest::digest(&ring::digest::SHA256, v.as_bytes()).as_ref(), true);
        assert_eq!(c, "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM");
    }
}
