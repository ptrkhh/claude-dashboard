use super::routes;
use crate::auth::config::AuthConfig;
use crate::auth::layer::{guard_mw, GuardState};
use crate::collect::ctx::Ctx;
use crate::host;
use axum::routing::get;
use axum::{Json, Router};
use std::net::{IpAddr, SocketAddr};
use std::path::{Path, PathBuf};
use std::sync::Arc;

pub struct Config {
    pub bind: IpAddr,
    pub port: u16,
    pub claude_dir: PathBuf,
    pub disk_extra: Option<String>,
    pub public_dir: PathBuf,
    pub auth: Arc<AuthConfig>,
    /// Present exactly when `CDASH_AUTH` includes `password`. Built at boot so
    /// a misconfiguration is refused before anything listens.
    pub password: Option<crate::auth::login::PasswordState>,
}

crate::guarded_routes! {
    get "/api/sessions" => routes::get_sessions,
    get "/api/logs" => routes::get_logs,
    get "/api/places" => routes::get_places,
    get "/api/browse" => routes::get_browse,
    get "/api/hostinfo" => routes::get_hostinfo,
    post "/api/favorites" => routes::post_favorites,
    post "/api/launch" => routes::post_launch,
    post "/api/resume" => routes::post_resume,
    post "/api/kill" => routes::post_kill,
    post "/api/purge" => routes::post_purge,
    post "/api/logout" => routes::post_logout,
}

impl Config {
    /// The bind default is `127.0.0.1` — a breaking change from the Node agent,
    /// which bound every interface. Exposing the dangerous topology now takes a
    /// deliberate `CDASH_BIND=0.0.0.0`.
    /// A bad `CDASH_AUTH` is a boot error, surfaced by the caller. Returning it
    /// here rather than defaulting is the point: a typo must not open the
    /// origin.
    pub fn from_env() -> Result<Self, String> {
        let auth = crate::auth::config::config_from_env()?;
        let home = std::env::var("HOME").unwrap_or_else(|_| "/".to_string());
        let bind: IpAddr = std::env::var("CDASH_BIND")
            .ok()
            .and_then(|b| b.parse().ok())
            .unwrap_or_else(|| "127.0.0.1".parse().expect("literal is a valid IP"));

        let password = if auth.guards.contains(&crate::auth::config::GuardKind::Password) {
            let policy = crate::auth::boot::decide(
                std::env::var("CDASH_PASSWORD_HASH").ok().as_deref(),
                bind,
                std::env::var("CDASH_PUBLIC_URL").ok().as_deref(),
                std::env::var("CDASH_ALLOW_INSECURE_COOKIE").as_deref() == Ok("1"),
            )?;
            let pending_max = std::env::var("CDASH_LOGIN_PENDING_MAX")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(crate::auth::throttle::DEFAULT_PENDING_MAX);
            Some(crate::auth::login::PasswordState::new(policy, pending_max))
        } else {
            None
        };

        Ok(Self {
            password,
            bind,
            port: std::env::var("PORT").ok().and_then(|p| p.parse().ok()).unwrap_or(8080),
            claude_dir: std::env::var("CLAUDE_DIR")
                .map(PathBuf::from)
                .unwrap_or_else(|_| PathBuf::from(&home).join(".claude")),
            disk_extra: std::env::var("DISK_EXTRA").ok().filter(|s| !s.is_empty()),
            // Not in the spec: the Node agent resolved `public/` relative to
            // `__dirname`. A Rust binary has no equivalent, so the location is
            // configurable and defaults to the working directory.
            public_dir: std::env::var("CDASH_PUBLIC")
                .map(PathBuf::from)
                .unwrap_or_else(|_| PathBuf::from("public")),
            auth: Arc::new(auth),
        })
    }
}

/// What `serve` hands back. The address is the readiness signal — an in-process
/// caller awaits this future instead of polling `/api/health`.
pub struct Bound {
    pub addr: SocketAddr,
    pub ctx: Arc<Ctx>,
}

/// Built in two halves: an unauthenticated router carrying exactly the
/// enumerated exceptions, and a guarded one carrying everything else — the
/// static file service included — with the guard applied as a layer over the
/// second before the two are merged. A route added to the guarded half cannot
/// escape the layer regardless of where it is written.
pub fn router(
    ctx: Arc<Ctx>,
    public_dir: &Path,
    auth: Arc<AuthConfig>,
    password: Option<crate::auth::login::PasswordState>,
) -> Router {
    let st = GuardState { auth, log: Arc::clone(&ctx.host.log), password: password.clone() };

    let guarded = guarded_router()
        .fallback_service(tower_http::services::ServeDir::new(public_dir))
        .layer(axum::middleware::from_fn_with_state(st, guard_mw))
        .with_state(ctx);

    let mut unauth = Router::new()
        .route("/api/health", get(|| async { Json(serde_json::json!({ "ok": true })) }));
    if let Some(pw) = password {
        // Exceptions 2 and 3: static HTML with no host data, and a throttled
        // login that carries none either.
        unauth = unauth.merge(
            Router::new()
                .route("/login", get(crate::auth::login::get_login))
                .route("/api/login", axum::routing::post(crate::auth::login::post_login))
                .with_state(pw),
        );
    }

    unauth.merge(guarded)
}

/// Bind, start serving on a background task, and return the bound address.
/// Errors rather than logging-and-aborting because the in-process caller has no
/// stderr to scrape and no child to have exited.
pub async fn serve(cfg: Config) -> std::io::Result<Bound> {
    let listener = tokio::net::TcpListener::bind((cfg.bind, cfg.port)).await?;
    let addr = listener.local_addr()?;

    let h = host::init::init().await;
    let ctx = Arc::new(Ctx::new(h, cfg.claude_dir, cfg.disk_extra));
    if let Some(pw) = cfg.password.clone() {
        let _ = ctx.password.set(pw);
    }
    let app = router(Arc::clone(&ctx), &cfg.public_dir, Arc::clone(&cfg.auth), cfg.password.clone());

    tokio::spawn(async move {
        // `ConnectInfo` needs the peer address, which the trusted-proxy guard
        // checks against its allowlist.
        let _ = axum::serve(listener, app.into_make_service_with_connect_info::<SocketAddr>())
            .await;
    });

    Ok(Bound { addr, ctx })
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    fn tempdir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("cdash-http-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("sessions")).unwrap();
        dir
    }

    pub(crate) fn cfg_for(dir: PathBuf) -> Config {
        Config {
            bind: "127.0.0.1".parse().unwrap(),
            port: 0, // let the OS choose, so tests never collide
            claude_dir: dir,
            disk_extra: None,
            public_dir: PathBuf::from("public"),
            auth: Arc::new(
                AuthConfig::build(
                    vec![crate::auth::config::GuardKind::None],
                    None,
                    "X-Forwarded-Email".into(),
                    vec![],
                )
                .expect("none is always buildable"),
            ),
            password: None,
        }
    }

    #[tokio::test]
    async fn serve_returns_the_bound_address_rather_than_logging_it() {
        // This is what `server.js` could not do: it discarded the returned
        // Server and logged the `port` variable, so no test could find it.
        let b = serve(cfg_for(tempdir("bound"))).await.unwrap();
        assert_ne!(b.addr.port(), 0, "port 0 must resolve to a real port");
        assert!(b.addr.ip().is_loopback());
    }

    #[tokio::test]
    async fn health_answers_without_leaking_host_details() {
        let b = serve(cfg_for(tempdir("health"))).await.unwrap();
        let body = reqwest_get(&format!("http://{}/api/health", b.addr)).await;
        assert_eq!(body, "{\"ok\":true}");
    }

    #[tokio::test]
    async fn a_held_port_is_an_error_rather_than_a_panic() {
        let held = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = held.local_addr().unwrap().port();
        let mut cfg = cfg_for(tempdir("held"));
        cfg.port = port;
        let Err(e) = serve(cfg).await else {
            panic!("binding a held port must fail");
        };
        assert_eq!(e.kind(), std::io::ErrorKind::AddrInUse);
    }

    /// Minimal HTTP GET. `reqwest` is a dependency this crate does not need in
    /// production, and the responses under test are small and well-formed.
    pub(crate) async fn reqwest_get(url: &str) -> String {
        let (status, body) = http_get(url).await;
        assert_eq!(status, 200, "GET {url} returned {status}: {body}");
        body
    }

    /// Returns (status, body).
    pub(crate) async fn http_get(url: &str) -> (u16, String) {
        raw_request("GET", url, None).await
    }

    pub(crate) async fn http_post(url: &str, json: &str) -> (u16, String) {
        raw_request("POST", url, Some(json)).await
    }

    async fn raw_request(method: &str, url: &str, json: Option<&str>) -> (u16, String) {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let rest = url.strip_prefix("http://").expect("http:// url");
        let (host, path) = rest.split_once('/').expect("url has a path");
        let mut req =
            format!("{method} /{path} HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\n");
        if let Some(b) = json {
            req.push_str(&format!(
                "Content-Type: application/json\r\nContent-Length: {}\r\n",
                b.len()
            ));
        }
        req.push_str("\r\n");
        if let Some(b) = json {
            req.push_str(b);
        }

        let mut s = tokio::net::TcpStream::connect(host).await.unwrap();
        s.write_all(req.as_bytes()).await.unwrap();
        let mut raw = Vec::new();
        s.read_to_end(&mut raw).await.unwrap();
        let text = String::from_utf8_lossy(&raw).into_owned();
        let status: u16 = text
            .split_whitespace()
            .nth(1)
            .and_then(|c| c.parse().ok())
            .unwrap_or(0);
        let body = text.split("\r\n\r\n").nth(1).unwrap_or("").to_string();
        (status, body)
    }
}
