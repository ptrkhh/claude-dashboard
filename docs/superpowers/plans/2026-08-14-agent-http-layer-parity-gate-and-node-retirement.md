# HTTP Layer, Parity Gate, and Node Retirement Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Serve the existing ten routes from the Rust agent, prove field-by-field that it answers identically to the Node agent, and delete the Node tree.

**Architecture:** A new `crates/agent/src/http/` module over the `collect/` layer. Every handler body is already a tested function, so the routes are wiring plus status codes. `serve(cfg)` binds and returns the bound address, which is what finally makes an integration test possible — `server.js` exported nothing and discarded its `Server`. The parity gate then runs both agents against one synthetic `~/.claude` and compares. It is the step allowed to declare the port finished.

**Tech Stack:** Rust (edition 2021), `axum` 0.8.9, `tower-http` (fs), `tokio`, `serde_json`.

**Spec:** `docs/superpowers/specs/2026-07-30-tauri-multi-host-design.md` (§Sequencing steps 4–5)

**Previous plans:** `2026-07-30-agent-port-parsers-and-host-layer.md` (steps 1–2), `2026-08-14-agent-port-collect-and-orchestration.md` (step 3) — both complete.

## Global Constraints

Copied verbatim from the spec. Every task's requirements implicitly include this section.

- **`sysinfo` is pinned to `0.38.4`.** Version `0.39.x` requires Rust 1.95; do not upgrade.
- **`-D clippy::disallowed_types` is a REQUIRED build gate, not advisory.** A green build with this lint disabled is not a valid build.
- **No direct use of `std::process::Command` or `tokio::process::Command`** except the two sanctioned sites in `host/cmd.rs` and `host/path.rs`. **This plan adds no new site.**
- **Default subprocess time-box is 5 seconds.** The PATH probe uses 2000 ms; the git-status refresh uses 20 000 ms.
- **Field semantics must match Node exactly** for every value that reaches `/api/sessions`. Task 6 is the gate that checks this. Do not "improve" a field's shape, name, or rounding.
- **Rust version floor: 1.94.1.**

## Verified before planning

`axum = "0.8.9"` and `tower-http` (feature `fs`) resolve and build on the pinned 1.94.1 toolchain in this container, and a `Router` with a `get` route plus `ServeDir` as `fallback_service` compiles and binds. The spec's axum findings (§Rust-stack assumptions) were measured on this same version.

## Route inventory

Ported from `server.js:17-68`. Each names the tested function that already does the work.

| Method | Path | Body / query | Calls | Success |
|---|---|---|---|---|
| GET | `/api/health` | — | — | `{"ok":true}` |
| GET | `/api/sessions` | — | `collect_sessions` | the response object |
| GET | `/api/logs` | — | `LogBuffer::lines` | `{"lines":[…]}` |
| GET | `/api/browse` | `?path=&hidden=1` | `list_dirs` | the listing |
| GET | `/api/places` | — | `read_places` | `{recents,favorites}` |
| POST | `/api/favorites` | `{path}` | `assert_path` → `toggle_favorite` | `{recents,favorites}` |
| POST | `/api/launch` | `{dir,model?,effort?}` | `launch_session` (+ fire-and-forget `add_recent`) | `{"name":…}` |
| POST | `/api/resume` | `{sid}` | `resume_session` | `{"name":…}` |
| POST | `/api/kill` | `{name}` | `kill_session` | `{"ok":true}` |
| POST | `/api/purge` | `{sid}` | `purge_session` | `{"ok":true}` |

**Defaults that must carry:** `model` defaults to `sonnet` and `effort` to `medium` when absent (`collect.js:159`); `/api/browse` with no `path` uses the home directory (`server.js:45`).

**Error mapping** (`server.js:39-42`): `BadRequest` and `BrowseError` render as **400** with `{"error": <message>}`; anything else is **500** with the same shape. `/api/sessions` is the exception — it logs and returns **500** with the message (`server.js:34`), never a 400.

## Two divergences this plan accepts

Neither reaches `/api/sessions`, so neither is visible to the parity gate. Both are recorded rather than hidden.

1. **A wrong content-type on a POST returns 415, not 400.** Node's `express.json()` left `req.body` empty and the handler's own guard produced a 400. Axum's `Json<T>` rejects before the handler runs — measured at 415 in the spec's §Rust-stack assumptions, and treated there as a CSRF control worth having. Keep it.
2. **Static files no longer shadow API routes.** Express mounted `express.static` *before* the routes (`server.js:15`), so a file at `public/api/health` would have won. Axum serves static as `fallback_service`, so routes win. `public/` contains no such path; the change is strictly safer.

---

### Task 1: `Config`, `router`, `serve`, and the health route

The skeleton, plus the one route that has no dependencies. `serve` returning the bound address is the whole reason an integration test is possible now.

**Files:**
- Create: `crates/agent/src/http/mod.rs`
- Create: `crates/agent/src/http/serve.rs`
- Modify: `crates/agent/src/lib.rs`
- Modify: `crates/agent/Cargo.toml`

**Interfaces:**
- Consumes: `Ctx`, `Ctx::new` (step-3 plan, Task 5); `host::init` (step-2 plan, Task 13).
- Produces:
  - `pub struct Config { pub bind: IpAddr, pub port: u16, pub claude_dir: PathBuf, pub disk_extra: Option<String>, pub public_dir: PathBuf }` with `Config::from_env() -> Config`
  - `pub struct Bound { pub addr: SocketAddr, pub ctx: Arc<Ctx> }`
  - `pub fn router(ctx: Arc<Ctx>, public_dir: &Path) -> Router`
  - `pub async fn serve(cfg: Config) -> std::io::Result<Bound>`

- [x] **Step 1: Add the dependencies**

In `crates/agent/Cargo.toml`, under `[dependencies]`:

```toml
axum = "0.8.9"
tower-http = { version = "0.7", features = ["fs"] }
```

`Cargo.lock` is committed and CI builds `--locked`, so the exact resolved versions are pinned there rather than by a `=` requirement.

- [x] **Step 2: Write the failing tests**

`crates/agent/src/http/serve.rs`:

```rust
#[cfg(test)]
mod tests {
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
        let e = serve(cfg).await.unwrap_err();
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
        let mut req = format!(
            "{method} /{path} HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\n"
        );
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
```

- [x] **Step 3: Run tests to verify they fail**

Create `crates/agent/src/http/mod.rs`:

```rust
pub mod serve;
```

Add to `crates/agent/src/lib.rs`:

```rust
pub mod http;
```

Run: `cargo test -p cdash-agent http::`
Expected: FAIL — `cannot find type 'Config' in this scope`.

- [x] **Step 4: Write the implementation**

Prepend to `crates/agent/src/http/serve.rs`:

```rust
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
```

`Router::with_state(ctx)` types the router as `Router<()>` once every handler takes `State<Arc<Ctx>>`; the health handler takes no state, which is allowed.

- [x] **Step 5: Run tests to verify they pass**

Run: `cargo test -p cdash-agent http:: -- --test-threads=1`
Expected: PASS, 3 tests.

- [x] **Step 6: Commit**

```bash
git add crates/agent/src/http/ crates/agent/src/lib.rs crates/agent/Cargo.toml Cargo.lock
git commit -m "feat: axum router, Config, and serve() returning the bound address"
```

---

### Task 2: The read routes — `/api/sessions`, `/api/logs`, `/api/places`, `/api/browse`

Four GETs. `/api/browse` is the only one with input, and its guard already lives with `list_dirs`.

**Files:**
- Create: `crates/agent/src/http/routes.rs`
- Modify: `crates/agent/src/http/mod.rs`
- Modify: `crates/agent/src/http/serve.rs` (register the routes)

**Interfaces:**
- Consumes: `collect_sessions`, `list_dirs`/`BrowseError`, `read_places`, `LogBuffer::lines`.
- Produces:
  - `pub struct ApiError { pub status: StatusCode, pub message: String }` implementing `IntoResponse`
  - `pub async fn get_sessions`, `get_logs`, `get_places`, `get_browse` — axum handlers

- [x] **Step 1: Write the failing tests**

`crates/agent/src/http/routes.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::http::serve::tests::{cfg_for, http_get, reqwest_get};
    use crate::http::serve::serve;

    fn tempdir(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("cdash-routes-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("sessions")).unwrap();
        std::fs::write(dir.join("history.jsonl"), "").unwrap();
        dir
    }

    #[tokio::test]
    async fn sessions_returns_the_three_top_level_keys() {
        let b = serve(cfg_for(tempdir("sessions"))).await.unwrap();
        let body = reqwest_get(&format!("http://{}/api/sessions", b.addr)).await;
        let v: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert!(v["running"].is_array());
        assert!(v["resumable"].is_array());
        assert!(v["stats"]["ramTotalKb"].is_number());
    }

    #[tokio::test]
    async fn logs_returns_a_lines_array() {
        let b = serve(cfg_for(tempdir("logs"))).await.unwrap();
        let body = reqwest_get(&format!("http://{}/api/logs", b.addr)).await;
        let v: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert!(v["lines"].is_array());
    }

    #[tokio::test]
    async fn places_returns_the_empty_shape_when_no_file_exists() {
        let b = serve(cfg_for(tempdir("places"))).await.unwrap();
        let body = reqwest_get(&format!("http://{}/api/places", b.addr)).await;
        assert_eq!(body, "{\"recents\":[],\"favorites\":[]}");
    }

    #[tokio::test]
    async fn browse_lists_a_directory_and_defaults_to_home() {
        let d = tempdir("browse");
        std::fs::create_dir_all(d.join("child")).unwrap();
        let b = serve(cfg_for(d.clone())).await.unwrap();

        let body = reqwest_get(&format!(
            "http://{}/api/browse?path={}",
            b.addr,
            d.to_str().unwrap()
        ))
        .await;
        let v: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert!(v["entries"].as_array().unwrap().iter().any(|e| e["name"] == "child"));

        // No `path` at all must still answer — Node defaulted to the home dir.
        let (status, _) = http_get(&format!("http://{}/api/browse", b.addr)).await;
        assert_eq!(status, 200);
    }

    #[tokio::test]
    async fn browse_hidden_is_opt_in_via_the_query_string() {
        let d = tempdir("browse-hidden");
        std::fs::create_dir_all(d.join(".secret")).unwrap();
        let b = serve(cfg_for(d.clone())).await.unwrap();
        let p = d.to_str().unwrap();

        let plain = reqwest_get(&format!("http://{}/api/browse?path={p}", b.addr)).await;
        assert!(!plain.contains(".secret"));
        let shown = reqwest_get(&format!("http://{}/api/browse?path={p}&hidden=1", b.addr)).await;
        assert!(shown.contains(".secret"));
    }

    #[tokio::test]
    async fn a_browse_failure_is_a_400_naming_the_problem_not_a_500() {
        let b = serve(cfg_for(tempdir("browse-fail"))).await.unwrap();
        let (status, body) =
            http_get(&format!("http://{}/api/browse?path=/no/such/cdash-dir", b.addr)).await;
        assert_eq!(status, 400);
        assert_eq!(body, "{\"error\":\"No such folder\"}");
    }
}
```

- [x] **Step 2: Run tests to verify they fail**

Add to `crates/agent/src/http/mod.rs`:

```rust
pub mod routes;
```

Run: `cargo test -p cdash-agent http::routes`
Expected: FAIL — unresolved import `crate::http::serve::tests`.

Make the `serve.rs` test helpers visible to the sibling module by marking the module `pub(crate)`:

```rust
#[cfg(test)]
pub(crate) mod tests {
```

- [x] **Step 3: Write the implementation**

Prepend to `crates/agent/src/http/routes.rs`:

```rust
use crate::collect::browse::{list_dirs, BrowseError};
use crate::collect::ctx::Ctx;
use crate::collect::places::read_places;
use crate::collect::sessions::collect_sessions;
use crate::collect::validate::BadRequest;
use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use std::collections::HashMap;
use std::sync::Arc;

/// Mirrors the error shape of `server.js:41`: a status and `{ error: message }`.
pub struct ApiError {
    pub status: StatusCode,
    pub message: String,
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.status, Json(serde_json::json!({ "error": self.message }))).into_response()
    }
}

impl From<BadRequest> for ApiError {
    fn from(e: BadRequest) -> Self {
        Self { status: StatusCode::BAD_REQUEST, message: e.0 }
    }
}

impl From<BrowseError> for ApiError {
    fn from(e: BrowseError) -> Self {
        Self {
            status: StatusCode::from_u16(e.status()).unwrap_or(StatusCode::BAD_REQUEST),
            message: e.message,
        }
    }
}

/// `/api/sessions` is the one route that answers 500 rather than 400 on
/// failure, and logs first (`server.js:34`).
pub async fn get_sessions(State(ctx): State<Arc<Ctx>>) -> Response {
    Json(collect_sessions(&ctx).await).into_response()
}

pub async fn get_logs(State(ctx): State<Arc<Ctx>>) -> Response {
    Json(serde_json::json!({ "lines": ctx.host.log.lines() })).into_response()
}

pub async fn get_places(State(ctx): State<Arc<Ctx>>) -> Response {
    Json(read_places(&ctx.places_file).await).into_response()
}

pub async fn get_browse(
    Query(q): Query<HashMap<String, String>>,
) -> Result<Response, ApiError> {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/".to_string());
    let target = q.get("path").filter(|p| !p.is_empty()).cloned().unwrap_or(home);
    let hidden = q.get("hidden").map(|h| h == "1").unwrap_or(false);
    Ok(Json(list_dirs(&target, hidden).await?).into_response())
}
```

`collect_sessions` cannot fail — every fallible step inside it already degrades to a default — so `get_sessions` has no error arm. That is a real difference from Node, where any throw became a 500; it is a consequence of the port's error handling, not of this route.

- [x] **Step 4: Register the routes**

In `crates/agent/src/http/serve.rs`, extend `router`:

```rust
use super::routes;
```

```rust
pub fn router(ctx: Arc<Ctx>, public_dir: &Path) -> Router {
    Router::new()
        .route("/api/health", get(|| async { Json(serde_json::json!({ "ok": true })) }))
        .route("/api/sessions", get(routes::get_sessions))
        .route("/api/logs", get(routes::get_logs))
        .route("/api/places", get(routes::get_places))
        .route("/api/browse", get(routes::get_browse))
        .fallback_service(tower_http::services::ServeDir::new(public_dir))
        .with_state(ctx)
}
```

- [x] **Step 5: Run tests to verify they pass**

Run: `cargo test -p cdash-agent http:: -- --test-threads=1`
Expected: PASS, 9 tests.

- [x] **Step 6: Commit**

```bash
git add crates/agent/src/http/
git commit -m "feat: the four read routes with Node's error shape"
```

---

### Task 3: The write routes — `/api/favorites`, `/api/launch`, `/api/resume`, `/api/kill`, `/api/purge`

Five POSTs. Every guard already exists and is tested as a function; this task proves each is actually *called*, which is the failure mode a validator module cannot rule out on its own.

**Files:**
- Modify: `crates/agent/src/http/routes.rs`
- Modify: `crates/agent/src/http/serve.rs`

**Interfaces:**
- Consumes: `assert_path`, `toggle_favorite`, `add_recent`, `launch_session`, `resume_session`, `kill_session`, `purge_session`.
- Produces: `pub async fn post_favorites`, `post_launch`, `post_resume`, `post_kill`, `post_purge`.

- [x] **Step 1: Write the failing tests**

Add to the `tests` module in `crates/agent/src/http/routes.rs`:

```rust
    use crate::http::serve::tests::http_post;

    #[tokio::test]
    async fn favorites_rejects_a_relative_path_before_writing_anything() {
        // A1 reaching the route. The validator has its own test; this proves
        // the route calls it.
        let d = tempdir("fav-guard");
        let b = serve(cfg_for(d.clone())).await.unwrap();
        let (status, body) = http_post(
            &format!("http://{}/api/favorites", b.addr),
            "{\"path\":\"relative/x\"}",
        )
        .await;
        assert_eq!(status, 400);
        assert!(body.contains("bad path"));
        assert!(!d.join("cdash-places.json").exists(), "nothing may be written");
    }

    #[tokio::test]
    async fn favorites_toggles_and_persists() {
        let d = tempdir("fav-ok");
        let b = serve(cfg_for(d.clone())).await.unwrap();
        let url = format!("http://{}/api/favorites", b.addr);
        let (status, body) = http_post(&url, "{\"path\":\"/home/x/proj\"}").await;
        assert_eq!(status, 200);
        assert!(body.contains("/home/x/proj"));

        let (_, body) = http_post(&url, "{\"path\":\"/home/x/proj\"}").await;
        assert_eq!(body, "{\"recents\":[],\"favorites\":[]}", "second call toggles off");
    }

    #[tokio::test]
    async fn launch_rejects_a_model_outside_the_allowlist() {
        let b = serve(cfg_for(tempdir("launch-guard"))).await.unwrap();
        let (status, body) = http_post(
            &format!("http://{}/api/launch", b.addr),
            "{\"dir\":\"/tmp\",\"model\":\"gpt-4\"}",
        )
        .await;
        assert_eq!(status, 400);
        assert!(body.contains("bad model"));
    }

    #[tokio::test]
    async fn launch_rejects_a_directory_that_is_not_one() {
        let b = serve(cfg_for(tempdir("launch-dir"))).await.unwrap();
        let (status, body) = http_post(
            &format!("http://{}/api/launch", b.addr),
            "{\"dir\":\"/no/such/cdash-dir\"}",
        )
        .await;
        assert_eq!(status, 400);
        assert!(body.contains("not a directory"));
    }

    #[tokio::test]
    async fn resume_rejects_a_sid_that_is_not_a_uuid() {
        let b = serve(cfg_for(tempdir("resume-guard"))).await.unwrap();
        let (status, body) = http_post(
            &format!("http://{}/api/resume", b.addr),
            "{\"sid\":\"not-a-uuid; rm -rf /\"}",
        )
        .await;
        assert_eq!(status, 400);
        assert!(body.contains("bad sid"));
    }

    #[tokio::test]
    async fn kill_rejects_a_name_that_is_not_a_cdash_session() {
        let b = serve(cfg_for(tempdir("kill-guard"))).await.unwrap();
        let (status, body) = http_post(
            &format!("http://{}/api/kill", b.addr),
            "{\"name\":\"other; rm -rf /\"}",
        )
        .await;
        assert_eq!(status, 400);
        assert!(body.contains("bad name"));
    }

    #[tokio::test]
    async fn purge_guards_the_sid_and_then_hides_it() {
        let b = serve(cfg_for(tempdir("purge"))).await.unwrap();
        let url = format!("http://{}/api/purge", b.addr);

        let (status, _) = http_post(&url, "{\"sid\":\"../../etc/passwd\"}").await;
        assert_eq!(status, 400);

        let sid = "2f8a1c94-3b7e-4d51-9a02-6c5f8e1b7d34";
        let (status, body) = http_post(&url, &format!("{{\"sid\":\"{sid}\"}}")).await;
        assert_eq!(status, 200);
        assert_eq!(body, "{\"ok\":true}");
        assert!(b.ctx.purged.lock().unwrap().contains(sid));
    }

    #[tokio::test]
    async fn a_missing_body_field_is_rejected_rather_than_defaulted_into_a_subprocess() {
        let b = serve(cfg_for(tempdir("missing"))).await.unwrap();
        let (status, _) = http_post(&format!("http://{}/api/kill", b.addr), "{}").await;
        assert_eq!(status, 400, "an absent name must not become an empty tmux target");
    }
```

- [x] **Step 2: Run tests to verify they fail**

Run: `cargo test -p cdash-agent http::routes`
Expected: FAIL — `cannot find function 'post_favorites' in this scope` (after the routes are registered in Step 4; before that, the POSTs 404).

- [x] **Step 3: Write the implementation**

Add to `crates/agent/src/http/routes.rs`:

```rust
use crate::collect::places::{add_recent, toggle_favorite};
use crate::collect::spawn::{kill_session, launch_session, purge_session, resume_session};
use crate::collect::validate::assert_path;
use serde::Deserialize;

#[derive(Deserialize)]
pub struct PathBody {
    #[serde(default)]
    pub path: String,
}

#[derive(Deserialize)]
pub struct LaunchBody {
    #[serde(default)]
    pub dir: String,
    #[serde(default = "default_model")]
    pub model: String,
    #[serde(default = "default_effort")]
    pub effort: String,
}

// Node destructured `{ dir, model = 'sonnet', effort = 'medium' }`
// (`lib/collect.js:159`); an absent field must not become a rejected request.
fn default_model() -> String {
    "sonnet".to_string()
}
fn default_effort() -> String {
    "medium".to_string()
}

#[derive(Deserialize)]
pub struct SidBody {
    #[serde(default)]
    pub sid: String,
}

#[derive(Deserialize)]
pub struct NameBody {
    #[serde(default)]
    pub name: String,
}

pub async fn post_favorites(
    State(ctx): State<Arc<Ctx>>,
    Json(body): Json<PathBody>,
) -> Result<Response, ApiError> {
    assert_path(&body.path)?;
    let places = toggle_favorite(&ctx.places_file, &body.path)
        .await
        .map_err(|e| ApiError {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            message: e.to_string(),
        })?;
    Ok(Json(places).into_response())
}

pub async fn post_launch(
    State(ctx): State<Arc<Ctx>>,
    Json(body): Json<LaunchBody>,
) -> Result<Response, ApiError> {
    let name = launch_session(&ctx, &body.dir, &body.model, &body.effort).await?;

    // Fire-and-forget, exactly as `server.js:56`: a failed recents write logs
    // and does not fail the launch. The route resolves the directory before
    // recording it; `launch_session` received the raw value.
    let places_file = ctx.places_file.clone();
    let resolved = std::path::absolute(&body.dir)
        .unwrap_or_else(|_| std::path::PathBuf::from(&body.dir))
        .to_string_lossy()
        .into_owned();
    let log = Arc::clone(&ctx.host.log);
    tokio::spawn(async move {
        if let Err(e) = add_recent(&places_file, &resolved).await {
            log.push(format!("recent write failed: {e}"));
        }
    });

    Ok(Json(serde_json::json!({ "name": name })).into_response())
}

pub async fn post_resume(
    State(ctx): State<Arc<Ctx>>,
    Json(body): Json<SidBody>,
) -> Result<Response, ApiError> {
    let name = resume_session(&ctx, &body.sid).await?;
    Ok(Json(serde_json::json!({ "name": name })).into_response())
}

pub async fn post_kill(
    State(ctx): State<Arc<Ctx>>,
    Json(body): Json<NameBody>,
) -> Result<Response, ApiError> {
    kill_session(&ctx, &body.name).await?;
    Ok(Json(serde_json::json!({ "ok": true })).into_response())
}

pub async fn post_purge(
    State(ctx): State<Arc<Ctx>>,
    Json(body): Json<SidBody>,
) -> Result<Response, ApiError> {
    purge_session(&ctx, &body.sid)?;
    Ok(Json(serde_json::json!({ "ok": true })).into_response())
}
```

- [x] **Step 4: Register the routes**

In `crates/agent/src/http/serve.rs`, add `use axum::routing::post;` and extend `router`:

```rust
        .route("/api/favorites", post(routes::post_favorites))
        .route("/api/launch", post(routes::post_launch))
        .route("/api/resume", post(routes::post_resume))
        .route("/api/kill", post(routes::post_kill))
        .route("/api/purge", post(routes::post_purge))
```

- [x] **Step 5: Run tests to verify they pass**

Run: `cargo test -p cdash-agent http:: -- --test-threads=1`
Expected: PASS, 17 tests.

- [x] **Step 6: Commit**

```bash
git add crates/agent/src/http/
git commit -m "feat: the five write routes, each proving its guard is called"
```

---

### Task 4: Static serving and the binary

The last wiring: `public/` served, `main.rs` reading the environment, and the held-port diagnosis the spec specifies.

**Files:**
- Modify: `crates/agent/src/main.rs`
- Modify: `crates/agent/src/http/routes.rs` (test only)
- Modify: `README.md`

**Interfaces:**
- Consumes: `Config::from_env`, `serve`.
- Produces: a runnable `cdash-agent` binary.

- [x] **Step 1: Write the failing test**

Add to the `tests` module in `crates/agent/src/http/routes.rs`:

```rust
    #[tokio::test]
    async fn static_files_are_served_from_the_configured_directory() {
        let d = tempdir("static");
        let pubdir = d.join("pub");
        std::fs::create_dir_all(&pubdir).unwrap();
        std::fs::write(pubdir.join("index.html"), "<h1>cdash</h1>").unwrap();

        let mut cfg = cfg_for(d.clone());
        cfg.public_dir = pubdir;
        let b = serve(cfg).await.unwrap();

        let body = reqwest_get(&format!("http://{}/index.html", b.addr)).await;
        assert_eq!(body, "<h1>cdash</h1>");
    }

    #[tokio::test]
    async fn an_api_route_is_not_shadowed_by_a_static_file_of_the_same_name() {
        // Express mounted static FIRST, so this file would have won there. The
        // route winning is the safer order; asserted so the change is deliberate.
        let d = tempdir("shadow");
        let pubdir = d.join("pub");
        std::fs::create_dir_all(pubdir.join("api")).unwrap();
        std::fs::write(pubdir.join("api/health"), "STATIC").unwrap();

        let mut cfg = cfg_for(d.clone());
        cfg.public_dir = pubdir;
        let b = serve(cfg).await.unwrap();

        let body = reqwest_get(&format!("http://{}/api/health", b.addr)).await;
        assert_eq!(body, "{\"ok\":true}");
    }
```

- [x] **Step 2: Run tests to verify they fail**

Run: `cargo test -p cdash-agent static_files`
Expected: FAIL — the response is a 404, because `cfg_for` hard-codes `public_dir` and nothing writes the file. (If `ServeDir` already resolves it, the second test still pins the ordering.)

- [x] **Step 3: Write the binary**

`crates/agent/src/main.rs`:

```rust
use cdash_agent::http::serve::{serve, Config};

#[tokio::main]
async fn main() {
    let cfg = Config::from_env();
    let (bind, port) = (cfg.bind, cfg.port);

    match serve(cfg).await {
        Ok(b) => {
            println!("cdash-agent {} on http://{}", env!("CARGO_PKG_VERSION"), b.addr);
            let missing = b.ctx.host.missing();
            if !missing.is_empty() {
                println!("missing: {}", missing.join(", "));
            }
            // The task inside `serve` owns the accept loop; park here.
            std::future::pending::<()>().await;
        }
        Err(e) if e.kind() == std::io::ErrorKind::AddrInUse => {
            // The spec's diagnosed condition: stderr, exit 3, no pidfile.
            eprintln!("port {port} already in use");
            std::process::exit(3);
        }
        Err(e) => {
            eprintln!("cannot bind {bind}:{port}: {e}");
            std::process::exit(1);
        }
    }
}
```

- [x] **Step 4: Run tests to verify they pass**

Run: `cargo test -p cdash-agent http:: -- --test-threads=1`
Expected: PASS, 19 tests.

- [x] **Step 5: Run the binary against the real environment**

```bash
CDASH_PUBLIC=public PORT=0 cargo run -p cdash-agent &
sleep 2
```

Expected: a line naming a real port. Then, in another shell, `curl -s http://127.0.0.1:<port>/api/health` returns `{"ok":true}`. Kill it afterwards.

Verify the held-port path too:

```bash
PORT=8080 cargo run -q -p cdash-agent &   # first instance holds it
sleep 2
PORT=8080 cargo run -q -p cdash-agent; echo "exit=$?"
```

Expected: `port 8080 already in use` on stderr and `exit=3`. Kill the first instance.

- [x] **Step 6: Document the breaking change**

The spec requires the README to document it. Add to `README.md`:

```markdown
## Configuration

| Variable | Default | Meaning |
|---|---|---|
| `PORT` | `8080` | Port to listen on. `0` picks any free port. |
| `CDASH_BIND` | `127.0.0.1` | Address to bind. **Breaking change:** the Node agent bound every interface. LAN access now requires `CDASH_BIND=0.0.0.0` explicitly. |
| `CLAUDE_DIR` | `~/.claude` | Where sessions, projects and history are read from. |
| `DISK_EXTRA` | — | A second mount to report alongside `/`, e.g. `/mnt/d`. |
| `CDASH_PUBLIC` | `public` | Directory served as static files. |
```

- [x] **Step 7: Commit**

```bash
git add crates/agent/src/main.rs crates/agent/src/http/routes.rs README.md
git commit -m "feat: static serving, the agent binary, and the held-port diagnosis"
```

---

### Task 5: The parity gate

Spec step 5. Runs both agents against one synthetic `~/.claude` and compares `/api/sessions` field-by-field over a closed exemption list, then checks `/api/logs` by invariant. **Nothing after this may begin while it fails.**

The fixture is synthetic rather than the developer's real `~/.claude` so the comparison is reproducible. It deliberately contains:

- **a real tmux session** named `cdash-parity-…` running `sleep`, because the pane branch is the main path through `collectSessions` and it is the one whose tmux format string changed in the port — Node reads `#{session_name}|#{pane_pid}|#{pane_current_path}|#{session_created}` and Rust reads `PANE_FORMAT` with the path last. Both must arrive at the same `dir`, `pid` and `uptimeSec`. Without this the format change is never compared.
- **two live external sessions** (real `sleep` processes with session files) in **two distinct non-repository directories**, which exercise the process walk, the git-status cache, and the two-sided dedupe property.
- **an `sdk-cli` session** both agents must exclude.

If `tmux` is unavailable the gate reports that it could not compare the pane path rather than passing quietly — a gate that silently skips its main path is worse than one that fails.

**Files:**
- Create: `scripts/parity-gate.mjs`

**Interfaces:**
- Consumes: both agents.
- Produces: a pass/fail report; exit 0 only when every comparison holds.

- [x] **Step 1: Write the gate**

`scripts/parity-gate.mjs`:

```javascript
#!/usr/bin/env node
// Spec step 5. Runs the Node and Rust agents against one synthetic ~/.claude
// and compares /api/sessions field-by-field over the closed exemption list in
// docs/superpowers/specs/2026-07-30-tauri-multi-host-design.md.
//
// This script is deleted with the Node tree: it cannot run without server.js.
import { spawn } from 'node:child_process';
import fs from 'node:fs/promises';
import os from 'node:os';
import path from 'node:path';

const fail = [];
const check = (ok, msg) => { if (!ok) fail.push(msg); };
const sleep = ms => new Promise(r => setTimeout(r, ms));

// ---------- fixture ----------
const root = await fs.mkdtemp(path.join(os.tmpdir(), 'cdash-parity-'));
const claudeDir = path.join(root, '.claude');
await fs.mkdir(path.join(claudeDir, 'sessions'), { recursive: true });

const munge = p => p.replace(/[^a-zA-Z0-9]/g, '-');
const transcript = turns => Array.from({ length: turns }, (_, i) =>
  JSON.stringify({ type: 'assistant', message: { content: [{ type: 'text', text: `turn ${i}` }] } })
).join('\n') + '\n';

async function writeTranscript(cwd, sid, turns, extra = '') {
  const dir = path.join(claudeDir, 'projects', munge(cwd));
  await fs.mkdir(dir, { recursive: true });
  await fs.writeFile(path.join(dir, `${sid}.jsonl`), extra + transcript(turns));
}

// Two live external sessions in two distinct non-repository directories.
const dirs = [path.join(root, 'proj-a'), path.join(root, 'proj-b')];
const kids = [];
const external = [];
for (const [i, cwd] of dirs.entries()) {
  await fs.mkdir(cwd, { recursive: true });
  const kid = spawn('sleep', ['300'], { stdio: 'ignore' });
  kids.push(kid);
  const sid = `1111111${i}-2222-4333-8444-555555555555`;
  external.push({ sid, cwd, pid: kid.pid });
  await fs.writeFile(
    path.join(claudeDir, 'sessions', `${kid.pid}.json`),
    JSON.stringify({
      sessionId: sid, cwd, name: `proj-${i}`, entrypoint: 'cli',
      startedAt: Date.now() - 60_000, bridgeSessionId: `session_ext_${i}`,
    })
  );
  await writeTranscript(cwd, sid, 4, JSON.stringify({ type: 'user', gitBranch: 'main' }) + '\n');
}

// A real tmux session, so the pane branch — the main path, and the one whose
// format string changed — is actually compared. Its name must satisfy the kill
// guard's `^cdash-[\w-]+$`, because that is the shape both agents filter on.
const paneDir = path.join(root, 'pane-proj');
await fs.mkdir(paneDir, { recursive: true });
const TMUX_SESSION = 'cdash-parity-1200-abc';
let tmuxOk = false;
try {
  await new Promise((res, rej) => {
    const p = spawn('tmux', ['new-session', '-d', '-s', TMUX_SESSION, '-c', paneDir, 'sleep', '300'],
      { stdio: 'ignore' });
    p.on('exit', c => (c === 0 ? res() : rej(new Error(`tmux exited ${c}`))));
    p.on('error', rej);
  });
  tmuxOk = true;
} catch (e) {
  check(false, `could not create a tmux session, so the PANE PATH WAS NOT COMPARED: ${e.message}`);
}

// An sdk-cli session that BOTH agents must exclude.
const observer = spawn('sleep', ['300'], { stdio: 'ignore' });
kids.push(observer);
await fs.writeFile(
  path.join(claudeDir, 'sessions', `${observer.pid}.json`),
  JSON.stringify({ sessionId: 'aaaaaaaa-0000-4000-8000-000000000000', cwd: dirs[0], entrypoint: 'sdk-cli' })
);

// Resumable history: one with enough turns, one without, one purged-shaped.
const hist = [];
for (const [i, turns] of [[0, 5], [1, 2]]) {
  const sid = `9999999${i}-2222-4333-8444-555555555555`;
  const cwd = dirs[i % dirs.length];
  await writeTranscript(cwd, sid, turns, JSON.stringify({ type: 'ai-title', aiTitle: `Title ${i}` }) + '\n');
  hist.push(JSON.stringify({ sessionId: sid, project: cwd, timestamp: 1_700_000_000 + i, display: `prompt ${i}` }));
}
await fs.writeFile(path.join(claudeDir, 'history.jsonl'), hist.join('\n') + '\n');
await fs.writeFile(path.join(root, '.claude.json'), JSON.stringify({ projects: {} }));

// ---------- servers ----------
const env = { ...process.env, CLAUDE_DIR: claudeDir, HOME: root };
const nodeSrv = spawn('node', ['server.js'], { env: { ...env, PORT: '8791' }, stdio: 'ignore' });
const rustSrv = spawn('./target/debug/cdash-agent', [], {
  env: { ...env, PORT: '8792', CDASH_BIND: '127.0.0.1', CDASH_PUBLIC: 'public' },
  stdio: 'ignore',
});

const get = async (port, route) => {
  const r = await fetch(`http://127.0.0.1:${port}${route}`);
  return { status: r.status, body: await r.json() };
};

async function waitFor(port) {
  for (let i = 0; i < 100; i++) {
    try { if ((await get(port, '/api/health')).status === 200) return; } catch { /* not up */ }
    await sleep(100);
  }
  throw new Error(`agent on ${port} never became healthy`);
}

try {
  await waitFor(8791);
  await waitFor(8792);

  // Warm both: the git cache returns null cold, and the CPU sampler needs two
  // refreshes 200ms apart. The exemption list assumes a warm comparison.
  for (const p of [8791, 8792]) await get(p, '/api/sessions');
  await sleep(1500);
  for (const p of [8791, 8792]) await get(p, '/api/sessions');
  await sleep(500);

  const [n, r] = [(await get(8791, '/api/sessions')).body, (await get(8792, '/api/sessions')).body];

  // ---------- /api/sessions ----------
  const key = s => s.sid ?? s.name;
  const byKey = list => Object.fromEntries(list.map(s => [key(s), s]));
  const [nr, rr] = [byKey(n.running), byKey(r.running)];

  check(
    JSON.stringify(Object.keys(nr).sort()) === JSON.stringify(Object.keys(rr).sort()),
    `running sets differ:\n  node=${Object.keys(nr).sort()}\n  rust=${Object.keys(rr).sort()}`
  );

  if (tmuxOk) {
    const pane = k => Object.values(k).find(s => s.name === TMUX_SESSION);
    const [np, rp] = [pane(nr), pane(rr)];
    check(np !== undefined, 'node did not report the tmux pane');
    check(rp !== undefined, 'rust did not report the tmux pane');
    if (np && rp) {
      // The format-string change lives or dies here: Node read the path third
      // of four, Rust reads it last.
      check(np.dir === rp.dir, `pane dir: node=${np.dir} rust=${rp.dir}`);
      check(np.pid === rp.pid, `pane pid: node=${np.pid} rust=${rp.pid}`);
      check(Math.abs(np.uptimeSec - rp.uptimeSec) <= 5, 'pane uptimeSec drifted by more than 5s');
    }
  }

  // Exempt by name, per the spec's closed list (plus the two sampled `stats`
  // fields this port's derivation found missing from it).
  const EXACT = ['name', 'dir', 'pid', 'model', 'effort', 'rcLink', 'sid', 'lastMessage', 'external'];

  for (const k of Object.keys(nr)) {
    const a = nr[k], b = rr[k];
    if (!a || !b) continue;
    for (const f of EXACT) {
      check(
        JSON.stringify(a[f] ?? null) === JSON.stringify(b[f] ?? null),
        `running[${k}].${f}: node=${JSON.stringify(a[f])} rust=${JSON.stringify(b[f])}`
      );
    }
    check(Math.abs(a.uptimeSec - b.uptimeSec) <= 5, `running[${k}].uptimeSec drifted by more than 5s`);
    check(a.uptimeSec >= 0 && b.uptimeSec >= 0, `running[${k}].uptimeSec negative`);
    check(JSON.stringify(a.git) === JSON.stringify(b.git), `running[${k}].git: ${JSON.stringify(a.git)} vs ${JSON.stringify(b.git)}`);
    check(a.working === b.working, `running[${k}].working differs`);
    check(b.cpu === null || typeof b.cpu === 'number', `running[${k}].cpu is neither null nor a number`);
    check(a.rssKb > 0 && b.rssKb > 0, `running[${k}].rssKb must be positive`);
    check(Math.abs(a.rssKb - b.rssKb) / Math.max(a.rssKb, 1) < 0.10, `running[${k}].rssKb differs by more than 10%`);
    check(a.cpuSampleAgeMs === undefined, `node must not have cpuSampleAgeMs`);
    check(b.cpuSampleAgeMs !== undefined, `rust must have cpuSampleAgeMs`);
  }

  check(
    JSON.stringify(n.resumable) === JSON.stringify(r.resumable),
    `resumable differs:\n  node=${JSON.stringify(n.resumable)}\n  rust=${JSON.stringify(r.resumable)}`
  );

  check(n.stats.ramTotalKb === r.stats.ramTotalKb, `stats.ramTotalKb: ${n.stats.ramTotalKb} vs ${r.stats.ramTotalKb}`);
  check(
    JSON.stringify(n.stats.disks.map(d => [d.mount, d.totalKb])) ===
    JSON.stringify(r.stats.disks.map(d => [d.mount, d.totalKb])),
    `stats.disks mounts/totals differ`
  );
  // cpuPct and ramUsedKb are sampled per request and cannot be compared for
  // equality — the exemption this port's derivation added to the spec's list.
  check(typeof r.stats.cpuPct === 'number' && r.stats.cpuPct <= 100, 'stats.cpuPct out of range');
  check(r.stats.ramUsedKb > 0, 'stats.ramUsedKb must be positive');

  // ---------- /api/logs: invariants, not equality ----------
  const logs = (await get(8792, '/api/logs')).body;
  check(Array.isArray(logs.lines), '/api/logs must return a lines array');
  check(logs.lines.every(l => /^\d\d:\d\d:\d\d /.test(l)), 'every log line needs an HH:MM:SS prefix');

  // The two-sided dedupe property. Both project dirs are non-repositories, so
  // `git status` failed in each; the keys are per-directory.
  const gitLines = logs.lines.filter(l => l.includes('sh failed: git '));
  const gitDirs = new Set(gitLines.map(l => l.split('sh failed: git ')[1].split(':')[0]));
  check(gitDirs.size >= 2, `two different failing directories must log separately, got ${gitDirs.size}`);
  for (const d of gitDirs) {
    const n = gitLines.filter(l => l.includes(`sh failed: git ${d}:`)).length;
    check(n === 1, `one directory failing repeatedly must log once, got ${n} for ${d}`);
  }
} finally {
  for (const k of [nodeSrv, rustSrv, ...kids]) k.kill('SIGKILL');
  if (tmuxOk) spawn('tmux', ['kill-session', '-t', TMUX_SESSION], { stdio: 'ignore' });
  await fs.rm(root, { recursive: true, force: true });
}

if (fail.length) {
  console.error(`PARITY GATE FAILED (${fail.length}):`);
  for (const f of fail) console.error(`  - ${f}`);
  process.exit(1);
}
console.log('PARITY GATE PASSED');
```

- [x] **Step 2: Build the Rust agent the gate expects**

Run: `cargo build -p cdash-agent`
Expected: `target/debug/cdash-agent` exists.

- [x] **Step 3: Run the gate**

Run: `node scripts/parity-gate.mjs`
Expected: `PARITY GATE PASSED`.

**If it fails, the port is not finished.** Read each reported line: it names the field, the Node value, and the Rust value. Fix the Rust side to match Node unless the field is a sampled machine quantity — in which case adding an exemption is a design decision, not a test fix, and belongs in the spec first.

- [x] **Step 4: Record the result**

Paste the gate's output into the commit message. A gate whose passing run nobody recorded is a gate nobody can show passed.

- [x] **Step 5: Commit**

```bash
git add scripts/parity-gate.mjs
git commit -m "test: parity gate comparing the Node and Rust agents field-by-field"
```

---

### Task 6: Retire the Node tree

Only after Task 5 passes. `server.js`, `lib/`, and `test/` are replaced by `crates/agent/`; `public/` is carried over.

**Files:**
- Delete: `server.js`, `lib/`, `test/`, `scripts/parity-gate.mjs`
- Modify: `package.json`, `README.md`, `.github/workflows/ci.yml`

**Interfaces:**
- Consumes: a passing parity gate.
- Produces: a repository whose only agent is the Rust one.

- [x] **Step 1: Confirm the gate passed on the current tree**

Run: `node scripts/parity-gate.mjs`
Expected: `PARITY GATE PASSED`. **Do not proceed on a failure or a skip.**

- [x] **Step 2: Delete the Node agent**

```bash
git rm -r server.js lib test scripts/parity-gate.mjs
```

The gate goes with it: it spawns `node server.js` and cannot run once that file is gone. It stays in git history alongside the tree it validated.

- [x] **Step 3: Reduce `package.json` to what `public/` still needs**

`public/` is plain JavaScript with no build step, and nothing in it imports a dependency. `express` was the agent's only dependency.

`package.json`:

```json
{
  "name": "claude-dashboard",
  "private": true,
  "type": "module"
}
```

Then remove the stale lockfile:

```bash
git rm package-lock.json
```

- [x] **Step 4: Point CI at the only suite that remains**

`.github/workflows/ci.yml` already runs `cargo test --all --locked` and the clippy gate, and never ran `npm test`. Confirm no change is needed:

Run: `grep -n "npm" .github/workflows/ci.yml`
Expected: no output.

- [x] **Step 5: Update the README's run instructions**

Replace any `npm start` / `node server.js` instruction in `README.md` with:

```markdown
## Running

```bash
cargo run -p cdash-agent          # http://127.0.0.1:8080
```

Configuration is environment-driven — see the table above.
```

- [x] **Step 6: Verify the tree still builds and tests green**

Run: `cargo test --all --locked -- --test-threads=1`
Expected: PASS.

Run: `cargo clippy --all-targets --locked -- -D warnings -D clippy::disallowed_types`
Expected: exit 0.

Run: `ls lib server.js test 2>&1`
Expected: "No such file or directory" for each.

Run the agent once against the real environment and confirm the UI loads:

```bash
PORT=8099 cargo run -q -p cdash-agent &
sleep 3
curl -s http://127.0.0.1:8099/api/health
curl -s -o /dev/null -w '%{http_code}\n' http://127.0.0.1:8099/index.html
```

Expected: `{"ok":true}` and `200`. Kill it afterwards.

- [x] **Step 7: Commit**

```bash
git add -A
git commit -m "refactor: retire the Node agent, replaced by crates/agent"
```

---

## What this plan deliberately does not cover

- **Auth.** Spec step 6 — the guard chain, `/login`, `/api/hostinfo`, the throttle, and the `CDASH_BIND=0.0.0.0` + `CDASH_AUTH=none` warning, which cannot be written before `CDASH_AUTH` exists. Absent from the Node tree entirely, so nothing is lost by deleting it first.
- **The UI work.** Spec step 7 — `backoff.js`, the status-propagating `api()`, the service-worker changes. Independent of this plan.
- **The Tauri clients.** Spec steps 8–11.

## Next plan starts here

Spec step 6: auth. It is the first step whose design has no Node implementation to port, so it is written from the spec rather than derived from existing code — which also means it needs no parity gate.
