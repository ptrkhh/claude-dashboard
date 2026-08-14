//! Spec assertions 7–12, plus assertion 5 with its exact `bearer,password` pair.
use cdash_agent::auth::boot::decide;
use cdash_agent::auth::config::{AuthConfig, GuardKind};
use cdash_agent::auth::login::PasswordState;
use cdash_agent::auth::password::hash_password;
use cdash_agent::auth::throttle::DEFAULT_PENDING_MAX;
use cdash_agent::http::serve::{serve, Config};
use std::path::PathBuf;
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

const PW: &str = "a good long password";

fn tempdir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("cdash-pwit-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("sessions")).unwrap();
    std::fs::write(dir.join("history.jsonl"), "").unwrap();
    dir
}

/// A loopback password config — the Termux posture, which needs no
/// `CDASH_PUBLIC_URL` and no insecure-cookie flag.
fn password_cfg(dir: PathBuf, guards: Vec<GuardKind>, token: Option<String>) -> Config {
    let policy = decide(
        Some(&hash_password(PW).unwrap()),
        "127.0.0.1".parse().unwrap(),
        None,
        false,
    )
    .expect("loopback + a valid hash must boot");
    Config {
        bind: "127.0.0.1".parse().unwrap(),
        port: 0,
        claude_dir: dir,
        disk_extra: None,
        // Integration tests run with the crate root as CWD, so a relative
        // "public" would not resolve and every asset would 404 once the guard
        // let it through.
        public_dir: PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../public"),
        auth: Arc::new(
            AuthConfig::build(guards, token, "X-Forwarded-Email".into(), vec![]).unwrap(),
        ),
        password: Some(PasswordState::new(policy, DEFAULT_PENDING_MAX)),
    }
}

struct Resp {
    status: u16,
    /// Lowercased, for case-insensitive attribute checks.
    headers: String,
    /// Verbatim — the sid is base64url and case-sensitive, so it must not be
    /// read out of the lowercased copy.
    raw_headers: String,
    body: String,
}

async fn req(
    addr: &str,
    method: &str,
    path: &str,
    headers: &[&str],
    content_type: Option<&str>,
    body: Option<&str>,
) -> Resp {
    let mut r = format!("{method} {path} HTTP/1.1\r\nHost: {addr}\r\nConnection: close\r\n");
    for h in headers {
        r.push_str(h);
        r.push_str("\r\n");
    }
    if let Some(b) = body {
        if let Some(ct) = content_type {
            r.push_str(&format!("Content-Type: {ct}\r\n"));
        }
        r.push_str(&format!("Content-Length: {}\r\n", b.len()));
    }
    r.push_str("\r\n");
    if let Some(b) = body {
        r.push_str(b);
    }
    let mut s = tokio::net::TcpStream::connect(addr).await.unwrap();
    s.write_all(r.as_bytes()).await.unwrap();
    let mut raw = Vec::new();
    s.read_to_end(&mut raw).await.unwrap();
    let text = String::from_utf8_lossy(&raw).into_owned();
    let status = text.split_whitespace().nth(1).and_then(|c| c.parse().ok()).unwrap_or(0);
    let (head, body) = text.split_once("\r\n\r\n").unwrap_or((&text, ""));
    Resp {
        status,
        headers: head.to_lowercase(),
        raw_headers: head.to_string(),
        body: body.to_string(),
    }
}

async fn login(addr: &str, password: &str) -> Resp {
    req(
        addr,
        "POST",
        "/api/login",
        &[],
        Some("application/json"),
        Some(&format!("{{\"password\":\"{password}\"}}")),
    )
    .await
}

fn cookie_of(r: &Resp) -> String {
    r.raw_headers
        .lines()
        .find(|l| l.to_lowercase().starts_with("set-cookie:"))
        .and_then(|l| l.split_once(": "))
        .map(|(_, v)| v.split(';').next().unwrap_or("").to_string())
        .expect("a successful login must set a cookie")
}

#[tokio::test]
async fn assertion_7_login_page_wrong_password_and_the_cookie_round_trip() {
    let b = serve(password_cfg(tempdir("seven"), vec![GuardKind::Password], None)).await.unwrap();
    let a = b.addr.to_string();

    // /login is reachable unauthenticated.
    let page = req(&a, "GET", "/login", &[], None, None).await;
    assert_eq!(page.status, 200);
    assert!(page.body.contains("Sign in"));

    // A wrong password sets no cookie.
    let bad = login(&a, "wrong password here").await;
    assert_eq!(bad.status, 401);
    assert!(!bad.headers.contains("set-cookie"), "a failed login must not set a cookie");

    // The right one does, with every attribute the __Host- prefix requires.
    let good = login(&a, PW).await;
    assert_eq!(good.status, 200);
    for attr in ["httponly", "secure", "samesite=lax", "__host-cdash_sid", "path=/"] {
        assert!(good.headers.contains(attr), "{attr} missing from {}", good.headers);
    }

    // Replay assertions 2 and 3 with the cookie.
    let c = cookie_of(&good);
    let ck = format!("Cookie: {c}");
    assert_eq!(req(&a, "GET", "/api/sessions", &[&ck], None, None).await.status, 200);
    assert_eq!(req(&a, "GET", "/", &[&ck], None, None).await.status, 200);
    assert_eq!(req(&a, "GET", "/sw.js", &[&ck], None, None).await.status, 200);

    // And without it a navigation lands on the login page.
    let nav = req(&a, "GET", "/", &[], None, None).await;
    assert_eq!(nav.status, 302);
    assert!(nav.headers.contains("location: /login"));
}

#[tokio::test]
async fn assertion_8_csrf_invariants_distinguish_415_400_and_422() {
    // The only mechanical enforcement of the primary CSRF control. If it is
    // ever dropped, the sibling-subdomain exposure becomes HIGH.
    let b = serve(password_cfg(tempdir("eight"), vec![GuardKind::Password], None)).await.unwrap();
    let a = b.addr.to_string();
    let ck = format!("Cookie: {}", cookie_of(&login(&a, PW).await));

    // A cross-site form can send exactly these three, and none may be parsed.
    for ct in ["text/plain", "application/x-www-form-urlencoded", "multipart/form-data"] {
        let r = req(&a, "POST", "/api/kill", &[&ck], Some(ct), Some("name=cdash-x")).await;
        assert_eq!(r.status, 415, "{ct} must be rejected before the handler runs");
    }
    // No content-type at all is the same case.
    let none = req(&a, "POST", "/api/kill", &[&ck], None, Some("name=cdash-x")).await;
    assert_eq!(none.status, 415, "an absent content-type must be rejected too");

    // The three outcomes are different codes and different proofs.
    let malformed = req(&a, "POST", "/api/kill", &[&ck], Some("application/json"), Some("{")).await;
    assert_eq!(malformed.status, 400, "malformed JSON under a correct content-type");
    let mistyped =
        req(&a, "POST", "/api/kill", &[&ck], Some("application/json"), Some("{\"name\":123}")).await;
    assert_eq!(mistyped.status, 422, "well-formed JSON that fails to deserialize");

    // A suffix type is accepted — not CORS-simple, so no form can send one.
    let suffix = req(
        &a,
        "POST",
        "/api/purge",
        &[&ck],
        Some("application/vnd.api+json"),
        Some("{\"sid\":\"2f8a1c94-3b7e-4d51-9a02-6c5f8e1b7d34\"}"),
    )
    .await;
    assert_eq!(suffix.status, 200, "*/*+json is admitted by the extractor");

    // And nothing loosens CORS server-side.
    let s = req(&a, "GET", "/api/sessions", &[&ck], None, None).await;
    assert!(!s.headers.contains("access-control-allow-origin"));
}

#[tokio::test]
async fn assertion_9_the_throttle_delays_and_never_denies() {
    let b = serve(password_cfg(tempdir("nine"), vec![GuardKind::Password], None)).await.unwrap();
    let a = b.addr.to_string();

    // Arm it past the free attempts with distinct credentials.
    for i in 0..6 {
        assert_eq!(login(&a, &format!("wrong guess number {i}")).await.status, 401);
    }

    // A correct login is delayed, then succeeds. Never 429.
    let started = std::time::Instant::now();
    let ok = login(&a, PW).await;
    assert_eq!(ok.status, 200, "a login is never rejected for throttle reasons");
    assert!(started.elapsed() >= std::time::Duration::from_secs(2), "it must have been delayed");

    // Success reset the counter, so the next login is immediate.
    let started = std::time::Instant::now();
    assert_eq!(login(&a, PW).await.status, 200);
    assert!(started.elapsed() < std::time::Duration::from_secs(2), "success resets the ladder");
}

#[tokio::test]
async fn assertion_9b_replaying_one_wrong_password_does_not_advance_the_ladder() {
    let b = serve(password_cfg(tempdir("nine-b"), vec![GuardKind::Password], None)).await.unwrap();
    let a = b.addr.to_string();

    // Rule B: a stale client repeats itself and stays at one distinct failure.
    for _ in 0..8 {
        assert_eq!(login(&a, "the same stale password").await.status, 401);
    }
    let started = std::time::Instant::now();
    assert_eq!(login(&a, PW).await.status, 200);
    assert!(
        started.elapsed() < std::time::Duration::from_secs(2),
        "8 replays of one credential must not have armed the ladder"
    );
}

#[tokio::test]
async fn assertion_10_a_duplicate_cookie_authenticates_as_the_last_one() {
    let b = serve(password_cfg(tempdir("ten"), vec![GuardKind::Password], None)).await.unwrap();
    let a = b.addr.to_string();
    let real = cookie_of(&login(&a, PW).await);

    let junk = "__Host-cdash_sid=notavalidsession";
    let last_wins = format!("Cookie: {junk}; {real}");
    assert_eq!(req(&a, "GET", "/api/sessions", &[&last_wins], None, None).await.status, 200);

    let last_loses = format!("Cookie: {real}; {junk}");
    assert_eq!(
        req(&a, "GET", "/api/sessions", &[&last_loses], None, None).await.status,
        401,
        "last-wins must be deterministic in both orderings"
    );
}

#[tokio::test]
async fn assertion_11_boot_refusals_and_the_insecure_escape_hatch() {
    let public: std::net::IpAddr = "0.0.0.0".parse().unwrap();
    let loopback: std::net::IpAddr = "127.0.0.1".parse().unwrap();
    let h = hash_password(PW).unwrap();

    assert!(decide(None, loopback, None, false).is_err(), "unset hash must refuse");
    assert!(decide(Some("garbage"), loopback, None, false).is_err(), "unparseable hash must refuse");
    assert!(decide(Some(&h), public, None, false).is_err(), "public bind without TLS must refuse");

    // With the escape hatch, Secure and the __Host- prefix are dropped together.
    let p = decide(Some(&h), public, None, true).unwrap();
    assert!(!p.secure_cookie);
    let st = PasswordState::new(p, DEFAULT_PENDING_MAX);
    let c = cdash_agent::auth::cookie::set_cookie(
        st.cookie_name(),
        "x",
        std::time::Duration::from_secs(1),
        st.policy.secure_cookie,
    );
    assert!(!c.contains("Secure"), "no Secure without TLS");
    assert!(!c.contains("__Host-"), "and the prefix goes with it");
}

#[tokio::test]
async fn assertion_12_loopback_boots_with_secure_intact() {
    // The Termux posture: the safe configuration is reachable without setting
    // the flag that would degrade it.
    let b = serve(password_cfg(tempdir("twelve"), vec![GuardKind::Password], None)).await.unwrap();
    let a = b.addr.to_string();
    let r = login(&a, PW).await;
    assert_eq!(r.status, 200);
    assert!(r.headers.contains("secure"));
    assert!(r.headers.contains("__host-cdash_sid"));
}

#[tokio::test]
async fn assertion_5_bearer_and_password_composed_needs_both() {
    // The spec's exact pair. 6a pinned this with bearer,trusted-proxy because
    // password did not exist yet; this replaces the substitute.
    let cfg = password_cfg(
        tempdir("compose"),
        vec![GuardKind::Bearer, GuardKind::Password],
        Some("s3cret".into()),
    );
    let b = serve(cfg).await.unwrap();
    let a = b.addr.to_string();

    let bearer = "Authorization: Bearer s3cret";
    // /api/login is unauthenticated, so a cookie is obtainable without the bearer.
    let ck = format!("Cookie: {}", cookie_of(&login(&a, PW).await));

    assert_eq!(
        req(&a, "GET", "/api/sessions", &[bearer], None, None).await.status,
        401,
        "a valid bearer alone must not pass"
    );
    assert_eq!(
        req(&a, "GET", "/api/sessions", &[&ck], None, None).await.status,
        401,
        "a valid session cookie alone must not pass"
    );
    assert_eq!(
        req(&a, "GET", "/api/sessions", &[bearer, &ck], None, None).await.status,
        200,
        "both together must pass"
    );
}
