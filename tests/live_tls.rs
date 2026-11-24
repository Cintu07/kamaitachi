//! Talks to the real servers without credentials: proves the tls and http/1.1 path works
//! against production endpoints. Ignored by default; run with `cargo test -- --ignored`.

use hayate::net::conn::{simple, Host};

#[test]
#[ignore]
fn real_servers_answer_over_tls() {
    let (status, body) = simple(Host::Anthropic, None, "GET", b"/v1/models", &[("anthropic-version", b"2026-10-21")], b"").unwrap();
    assert_eq!(status, 401, "{}", String::from_utf8_lossy(&body));
    assert!(!hayate::adapters::error_text(&body).is_empty());

    let (status, _) = simple(Host::Google, None, "GET", b"/youtube/v3/videos?part=id&id=x", &[], b"").unwrap();
    assert!(status == 403 || status == 400, "youtube said {status}");

    let (status, _) = simple(Host::X, None, "GET", b"/2/users/me", &[], b"").unwrap();
    assert_eq!(status, 401);

    let (status, _) = simple(Host::OpenAi, None, "GET", b"/v1/models", &[], b"").unwrap();
    assert_eq!(status, 401);
}
