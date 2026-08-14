use super::routes;
use crate::collect::ctx::Ctx;
use crate::host;
use axum::routing::{get, post};
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
}

impl Config {
    /// The bind default is `127.0.0.1` — a breaking change from the Node agent,
    /// which bound every interface. Exposing the dangerous topology now takes a
    /// deliberate `CDASH_BIND=0.0.0.0`.
    pub fn from_env() -> Self {
        let home = std::env::var("HOME").unwrap_or_else(|_| "/".to_string());
        Self {
            bind: std::env::var("CDASH_BIND")
                .ok()
                .and_then(|b| b.parse().ok())
                .unwrap_or_else(|| "127.0.0.1".parse().expect("literal is a valid IP")),
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
        }
    }
}

/// What `serve` hands back. The address is the readiness signal — an in-process
/// caller awaits this future instead of polling `/api/health`.
pub struct Bound {
    pub addr: SocketAddr,
    pub ctx: Arc<Ctx>,
}

pub fn router(ctx: Arc<Ctx>, public_dir: &Path) -> Router {
    Router::new()
        .route("/api/health", get(|| async { Json(serde_json::json!({ "ok": true })) }))
        .route("/api/sessions", get(routes::get_sessions))
        .route("/api/logs", get(routes::get_logs))
        .route("/api/places", get(routes::get_places))
        .route("/api/browse", get(routes::get_browse))
        .route("/api/favorites", post(routes::post_favorites))
        .route("/api/launch", post(routes::post_launch))
        .route("/api/resume", post(routes::post_resume))
        .route("/api/kill", post(routes::post_kill))
        .route("/api/purge", post(routes::post_purge))
        .fallback_service(tower_http::services::ServeDir::new(public_dir))
        .with_state(ctx)
}

/// Bind, start serving on a background task, and return the bound address.
/// Errors rather than logging-and-aborting because the in-process caller has no
/// stderr to scrape and no child to have exited.
pub async fn serve(cfg: Config) -> std::io::Result<Bound> {
    let listener = tokio::net::TcpListener::bind((cfg.bind, cfg.port)).await?;
    let addr = listener.local_addr()?;

    let h = host::init::init().await;
    let ctx = Arc::new(Ctx::new(h, cfg.claude_dir, cfg.disk_extra));
    let app = router(Arc::clone(&ctx), &cfg.public_dir);

    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
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
