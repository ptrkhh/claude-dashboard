//! Spec assertions 1–5. Assertions 7–12 arrive with the password guard (6b).
use cdash_agent::auth::config::{AuthConfig, GuardKind};
use cdash_agent::http::serve::{serve, Config, GUARDED_PATHS};
use std::path::PathBuf;
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

fn tempdir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("cdash-authit-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("sessions")).unwrap();
    std::fs::write(dir.join("history.jsonl"), "").unwrap();
    dir
}

fn cfg(dir: PathBuf, auth: AuthConfig) -> Config {
    Config {
        bind: "127.0.0.1".parse().unwrap(),
        port: 0,
        claude_dir: dir,
        disk_extra: None,
        public_dir: PathBuf::from("public"),
        auth: Arc::new(auth),
        password: None,
    }
}

fn bearer_cfg() -> AuthConfig {
    AuthConfig::build(
        vec![GuardKind::Bearer],
        Some("s3cret".into()),
        "X-Forwarded-Email".into(),
        vec![],
    )
    .unwrap()
}

/// (status, body). Extra headers are raw lines, e.g. "Authorization: Bearer x".
async fn req(
    addr: &str,
    method: &str,
    path: &str,
    headers: &[&str],
    body: Option<&str>,
) -> (u16, String) {
    let mut r = format!("{method} {path} HTTP/1.1\r\nHost: {addr}\r\nConnection: close\r\n");
    for h in headers {
        r.push_str(h);
        r.push_str("\r\n");
    }
    if let Some(b) = body {
        r.push_str(&format!("Content-Type: application/json\r\nContent-Length: {}\r\n", b.len()));
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
    (status, text.split("\r\n\r\n").nth(1).unwrap_or("").to_string())
}

#[tokio::test]
async fn assertion_1_health_is_reachable_unauthenticated() {
    let b = serve(cfg(tempdir("health"), bearer_cfg())).await.unwrap();
    let a = b.addr.to_string();
    let (status, body) = req(&a, "GET", "/api/health", &[], None).await;
    assert_eq!(status, 200);
    assert_eq!(body, "{\"ok\":true}");
}

#[tokio::test]
async fn assertion_2_every_guarded_route_401s_unauthenticated() {
    // The highest-value test in the suite. The path list comes from the same
    // macro invocation that builds the router, so a route added later without
    // a guard fails here on the day it is added.
    let b = serve(cfg(tempdir("bypass"), bearer_cfg())).await.unwrap();
    let a = b.addr.to_string();
    assert!(!GUARDED_PATHS.is_empty(), "an empty list would pass vacuously");

    for (method, path) in GUARDED_PATHS {
        let m = method.to_uppercase();
        let body = if m == "POST" { Some("{}") } else { None };
        let (status, resp) = req(&a, &m, path, &[], body).await;
        assert_eq!(status, 401, "{m} {path} was reachable unauthenticated: {resp}");
        assert_eq!(resp, "{\"error\":\"unauthorized\"}", "{m} {path} leaked detail");
    }
}

#[tokio::test]
async fn assertion_3_static_assets_are_behind_the_guard() {
    // Route enumeration alone cannot pin this: the static service and the
    // guard are layers, not routes.
    let b = serve(cfg(tempdir("static"), bearer_cfg())).await.unwrap();
    let a = b.addr.to_string();
    for path in ["/", "/sw.js", "/index.html", "/app.js"] {
        let (status, _) = req(&a, "GET", path, &[], None).await;
        assert_eq!(status, 401, "{path} was served unauthenticated");
    }
}

#[tokio::test]
async fn assertion_4_a_valid_credential_reaches_the_route() {
    let b = serve(cfg(tempdir("valid"), bearer_cfg())).await.unwrap();
    let a = b.addr.to_string();
    let (status, _) =
        req(&a, "GET", "/api/sessions", &["Authorization: Bearer s3cret"], None).await;
    assert_eq!(status, 200);
    let (status, _) = req(&a, "GET", "/api/sessions", &["Authorization: Bearer wrong"], None).await;
    assert_eq!(status, 401);
}

#[tokio::test]
async fn assertion_5_a_composed_chain_mounts_every_leg_it_names() {
    // The spec's pair is `bearer,password`; password lands in 6b, so the same
    // property is pinned here with `bearer,trusted-proxy` and 6b re-runs it
    // with the spec's exact pair. What this covers is the CDASH_AUTH parse: a
    // silently dropped leg is indistinguishable from a working one-guard chain
    // in every other assertion here.
    let auth = AuthConfig::build(
        vec![GuardKind::Bearer, GuardKind::TrustedProxy],
        Some("s3cret".into()),
        "X-Forwarded-Email".into(),
        vec!["127.0.0.1".parse().unwrap()],
    )
    .unwrap();
    let b = serve(cfg(tempdir("compose"), auth)).await.unwrap();
    let a = b.addr.to_string();

    let (only_bearer, _) =
        req(&a, "GET", "/api/sessions", &["Authorization: Bearer s3cret"], None).await;
    assert_eq!(only_bearer, 401, "a valid bearer alone must not pass an AND chain");

    let (only_proxy, _) = req(&a, "GET", "/api/sessions", &["X-Forwarded-Email: u@x"], None).await;
    assert_eq!(only_proxy, 401, "a valid proxy identity alone must not pass an AND chain");

    let (both, _) = req(
        &a,
        "GET",
        "/api/sessions",
        &["Authorization: Bearer s3cret", "X-Forwarded-Email: u@x"],
        None,
    )
    .await;
    assert_eq!(both, 200, "both legs together must pass");
}

#[tokio::test]
async fn cf_access_refuses_to_boot_when_the_keys_cannot_be_fetched() {
    // The UX this prevents: authenticating successfully with Cloudflare and
    // then hitting 401 on every request, with the reason only in the server's
    // stderr. A startup that cannot verify anything must not listen at all.
    let auth = AuthConfig::build_with_cf(
        vec![GuardKind::CfAccess],
        None,
        "X-Forwarded-Email".into(),
        vec![],
        Some(cdash_agent::auth::cfaccess::CfConfig {
            // Reserved by RFC 6761 to never resolve, so this is a local
            // failure rather than a test that depends on the network.
            team_domain: "https://cdash-test.invalid".into(),
            aud: "some-aud".into(),
        }),
    )
    .unwrap();

    let mut c = cfg(tempdir("cf-boot"), auth);
    c.port = 0;
    let Err(e) = serve(c).await else {
        panic!("cf-access with unreachable JWKS must refuse to start");
    };
    assert_eq!(e.kind(), std::io::ErrorKind::InvalidData);
    assert!(e.to_string().contains("cf-access"), "the message must name the guard: {e}");
    assert!(e.to_string().contains("JWKS"), "and what it could not obtain: {e}");
}

#[tokio::test]
async fn cf_access_without_its_config_is_a_boot_error_not_a_silent_lockout() {
    let e = AuthConfig::build(vec![GuardKind::CfAccess], None, "X-Forwarded-Email".into(), vec![])
        .unwrap_err();
    assert!(e.contains("CDASH_CF_TEAM_DOMAIN"), "the message must name what to set: {e}");
}

#[tokio::test]
async fn under_none_no_route_is_rejected() {
    // The invariant is that the guard refactor did not start rejecting the
    // local default — not that every handler succeeds on an empty body.
    let auth =
        AuthConfig::build(vec![GuardKind::None], None, "X-Forwarded-Email".into(), vec![]).unwrap();
    let b = serve(cfg(tempdir("open"), auth)).await.unwrap();
    let a = b.addr.to_string();

    for (method, path) in GUARDED_PATHS {
        let m = method.to_uppercase();
        // No test drives /api/launch, /api/resume or /api/kill to success: that
        // spawns or kills real tmux sessions as a side effect of `cargo test`.
        let body = if m == "POST" { Some("{}") } else { None };
        let (status, _) = req(&a, &m, path, &[], body).await;
        assert!(status != 401 && status != 403, "{m} {path} returned {status} under CDASH_AUTH=none");
    }
}
