use super::routes;
use crate::auth::config::{AuthConfig, GuardKind};
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
    post "/api/keys" => routes::post_keys,
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
        // Same principle as CDASH_AUTH below: a typo is refused, not defaulted.
        // `CDASH_BIND=0.0.0..1` silently becoming loopback also flips the
        // cookie policy `boot::decide` derives from this value.
        let bind: IpAddr = match std::env::var("CDASH_BIND") {
            Err(_) => "127.0.0.1".parse().expect("literal is a valid IP"),
            Ok(b) => b.parse().map_err(|_| format!("CDASH_BIND: not an IP address: {b:?}"))?,
        };

        let password = if auth.guards.contains(&crate::auth::config::GuardKind::Password) {
            let policy = crate::auth::boot::decide(
                std::env::var("CDASH_PASSWORD_HASH").ok().as_deref(),
                bind,
                std::env::var("CDASH_PUBLIC_URL").ok().as_deref(),
                std::env::var("CDASH_ALLOW_INSECURE_COOKIE").as_deref() == Ok("1"),
            )?;
            // A bound of 0 admits nothing: every login would 503 forever
            // while the page advises trying again shortly.
            let pending_max = match std::env::var("CDASH_LOGIN_PENDING_MAX") {
                Err(_) => crate::auth::throttle::DEFAULT_PENDING_MAX,
                Ok(v) => match v.parse::<usize>() {
                    Ok(n) if n > 0 => n,
                    _ => return Err(format!("CDASH_LOGIN_PENDING_MAX: not a positive integer: {v:?}")),
                },
            };
            Some(crate::auth::login::PasswordState::new(policy, pending_max))
        } else {
            None
        };

        Ok(Self {
            password,
            bind,
            port: match std::env::var("PORT") {
                Err(_) => 8080,
                Ok(p) => p.parse().map_err(|_| format!("PORT: not a port number: {p:?}"))?,
            },
            claude_dir: std::env::var("CLAUDE_DIR")
                .map(PathBuf::from)
                .unwrap_or_else(|_| PathBuf::from(&home).join(".claude")),
            disk_extra: std::env::var("DISK_EXTRA").ok().filter(|s| !s.is_empty()),
            public_dir: std::env::var("CDASH_PUBLIC")
                .map(PathBuf::from)
                .unwrap_or_else(|_| default_public_dir()),
            auth: Arc::new(auth),
        })
    }
}

/// `CDASH_PUBLIC` unset: `public/` under the working directory when it exists
/// (`cargo run` from the repo root), else beside the binary. Without the
/// fallback, a systemd unit with no `WorkingDirectory=` serves 404s for the
/// whole UI.
fn default_public_dir() -> PathBuf {
    let cwd = PathBuf::from("public");
    if cwd.is_dir() {
        return cwd;
    }
    std::env::current_exe()
        .ok()
        .and_then(|exe| Some(exe.parent()?.join("public")))
        .unwrap_or(cwd)
}

/// What `serve` hands back. The address is the readiness signal — an in-process
/// caller awaits this future instead of polling `/api/health`.
pub struct Bound {
    pub addr: SocketAddr,
    pub ctx: Arc<Ctx>,
    stop: tokio::sync::watch::Sender<bool>,
    task: tokio::task::JoinHandle<()>,
}

impl Bound {
    /// Fire the graceful-shutdown trigger and wait for the serving task to
    /// drain and exit, after which the port is released. The standalone binary
    /// never calls this; it exists so an in-process embedder can tear the
    /// server down.
    pub async fn stop(self) {
        let _ = self.stop.send(true);
        let _ = self.task.await;
    }

    /// Whether the serving task has ended. An embedder that caches `addr` needs
    /// this: an accept loop that panicked leaves the address string valid-looking
    /// while every request to it is refused.
    pub fn is_finished(&self) -> bool {
        self.task.is_finished()
    }
}

/// The unauthenticated exceptions, enumerated rather than implied — and the
/// list the router below is built from, so the two cannot disagree. A fourth
/// entry here without a matching arm registers nothing; a fourth route without
/// an entry here is unreachable.
pub const UNAUTH_PATHS: &[&str] = &["/api/health", "/login", "/api/login"];

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
    cf: Option<Arc<crate::auth::cfaccess::CfState>>,
) -> Router {
    let st = GuardState { auth, log: Arc::clone(&ctx.host.log), password: password.clone(), cf };

    let guarded = guarded_router()
        .fallback_service(tower_http::services::ServeDir::new(public_dir))
        .layer(axum::middleware::from_fn_with_state(st, guard_mw))
        .with_state(ctx);

    // Built by walking UNAUTH_PATHS rather than beside it: the list and the
    // router are one statement, so an exception cannot be added to the router
    // without appearing in the list the bypass test reads.
    let mut unauth = Router::new();
    for p in UNAUTH_PATHS {
        unauth = match (*p, password.clone()) {
            ("/api/health", _) => unauth
                .route(p, get(|| async { Json(serde_json::json!({ "ok": true })) })),
            // Exceptions 2 and 3 exist only under `password`: static HTML with
            // no host data, and a throttled login that carries none either.
            ("/login", Some(pw)) => {
                unauth.merge(Router::new().route(p, get(crate::auth::login::get_login)).with_state(pw))
            }
            ("/api/login", Some(pw)) => unauth.merge(
                Router::new()
                    .route(p, axum::routing::post(crate::auth::login::post_login))
                    .with_state(pw),
            ),
            _ => unauth,
        };
    }

    unauth.merge(guarded)
}

/// Bind, start serving on a background task, and return the bound address.
/// Errors rather than logging-and-aborting because the in-process caller has no
/// stderr to scrape and no child to have exited.
pub async fn serve(cfg: Config) -> std::io::Result<Bound> {
    // Fetched before binding: a cf-access origin that cannot verify anything
    // must fail as a service that did not start, with a named reason, rather
    // than as a successful SSO followed by 401 on every request.
    // Gated on the guard chain, not on the config being present. Leftover
    // CDASH_CF_* variables in a unit file must not couple boot to Cloudflare's
    // availability for a guard that is not in the chain — that turned a switch
    // to `CDASH_AUTH=none` behind a tunnel into an agent that exits 2 whenever
    // Cloudflare is unreachable.
    let cf = match cfg.auth.cf.clone() {
        Some(cfcfg) if cfg.auth.guards.contains(&GuardKind::CfAccess) => {
            let url = crate::auth::cfaccess::certs_url(&cfcfg.team_domain);
            let jwks = crate::auth::cfaccess::fetch_jwks(&url).await.map_err(|e| {
                std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    format!("cf-access: could not fetch JWKS: {e}"),
                )
            })?;
            let cache = crate::auth::cfaccess::JwksCache::new();
            cache.install(jwks);
            Some(Arc::new(crate::auth::cfaccess::CfState { cfg: cfcfg, jwks: cache }))
        }
        _ => None,
    };

    let listener = tokio::net::TcpListener::bind((cfg.bind, cfg.port)).await?;
    let addr = listener.local_addr()?;

    let h = host::init::init().await;
    let ctx = Arc::new(Ctx::new(h, cfg.claude_dir, cfg.disk_extra));
    if let Some(pw) = cfg.password.clone() {
        let _ = ctx.password.set(pw);
    }
    if let Some(cf) = cf.clone() {
        crate::auth::cfaccess::spawn_refresh(cf, Arc::clone(&ctx.host.log));
    }
    let app =
        router(Arc::clone(&ctx), &cfg.public_dir, Arc::clone(&cfg.auth), cfg.password.clone(), cf);

    let (stop_tx, mut stop_rx) = tokio::sync::watch::channel(false);
    let task = tokio::spawn(async move {
        // `ConnectInfo` needs the peer address, which the trusted-proxy guard
        // checks against its allowlist.
        let _ = axum::serve(listener, app.into_make_service_with_connect_info::<SocketAddr>())
            .with_graceful_shutdown(async move {
                let _ = stop_rx.changed().await;
            })
            .await;
    });

    Ok(Bound { addr, ctx, stop: stop_tx, task })
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

    /// An unreachable team domain: if the fetch were attempted, `serve` would
    /// return InvalidData and this would fail. Booting proves it was skipped.
    #[tokio::test]
    async fn cf_config_without_the_cf_access_guard_does_not_fetch_jwks() {
        let mut cfg = cfg_for(tempdir("cf-unused"));
        cfg.auth = Arc::new(
            AuthConfig::build_with_cf(
                vec![GuardKind::None],
                None,
                "X-Forwarded-Email".into(),
                vec![],
                Some(crate::auth::cfaccess::CfConfig {
                    team_domain: "https://team.cloudflareaccess.invalid".into(),
                    aud: "tag".into(),
                }),
            )
            .expect("none is always buildable"),
        );
        let b = serve(cfg).await.expect("a guard chain without cf-access must not need Cloudflare");
        b.stop().await;
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

    #[test]
    fn default_public_dir_falls_back_beside_the_binary() {
        // Test cwd is `crates/agent`, which has no `public/`, so the cwd
        // branch must not win: a bare relative path here is the systemd-unit
        // 404 bug this function exists to prevent.
        let d = super::default_public_dir();
        assert!(d.is_absolute(), "expected exe-relative fallback, got {d:?}");
        assert!(d.ends_with("public"), "got {d:?}");
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

    #[tokio::test]
    async fn stop_releases_the_port_for_rebinding() {
        let b = serve(cfg_for(tempdir("stop"))).await.unwrap();
        let addr = b.addr;
        let mut cfg = cfg_for(tempdir("rebind"));
        cfg.port = addr.port();
        b.stop().await;
        // The old listener must be gone, not just the accept loop parked.
        tokio::net::TcpListener::bind(addr)
            .await
            .expect("port must be rebindable after stop");
    }

    /// `reqwest` used to be absent from this crate, which is why these helpers
    /// spoke HTTP down a raw `TcpStream`. cf-access's JWKS fetch made it a
    /// production dependency; the hand-rolled parser outlived its reason.
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
        // No redirects: the guard answers navigations with 302 -> /login, and
        // following it would report 200 for a request that was refused.
        let c = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .expect("client builds");
        let m = reqwest::Method::from_bytes(method.as_bytes()).expect("test method is a token");
        let mut req = c.request(m, url);
        if let Some(b) = json {
            req = req.header("content-type", "application/json").body(b.to_string());
        }
        let res = req.send().await.expect("request reaches the test server");
        (res.status().as_u16(), res.text().await.unwrap_or_default())
    }
}
