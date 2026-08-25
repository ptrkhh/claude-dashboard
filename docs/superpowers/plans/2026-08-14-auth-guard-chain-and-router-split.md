# Auth: Guard Chain and Router Split Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Put every route except three behind a guard layer, with `CDASH_AUTH` composing guards by AND, and prove no route can reach the origin unauthenticated.

**Architecture:** A new `crates/agent/src/auth/` module. The router is built in two halves — an unauthenticated one carrying exactly three routes, and a guarded one carrying everything else including the static file service — with the guard applied as a layer over the second before the two are merged. This makes "unauthenticated" a countable list of three rather than a line-ordering property of one file.

**Tech Stack:** Rust (edition 2021), `axum` 0.8.9, `subtle` (constant-time compare).

**Spec:** `docs/superpowers/specs/2026-07-30-tauri-multi-host-design.md` (§Sequencing step 6, §`src/auth/` — guard chain, §Guard placement, §Health endpoints)

**Previous plans:** parsers/host (steps 1–2), collect (step 3), HTTP layer + parity gate + Node retirement (steps 4–5) — all complete.

## Why step 6 is three plans

Step 6 names five guards, a session/cookie/throttle system, a JWT verifier, a CLI subcommand, boot refusals, and twelve integration assertions. Those are three independent subsystems, and the scope rule says each plan should produce working, testable software on its own. This is the first of three:

- **6a (this plan)** — `CDASH_AUTH` parsing, AND composition, `none`, `bearer`, `trusted-proxy`, the two-half router, `/api/hostinfo`, and integration assertions 1–5.
- **6b** — the `password` guard: scrypt, the session store, the cookie splitter, `/login`, `/api/login`, `/api/logout`, throttle rules A–C, the boot refusals and the loopback exemption, `set-password`, assertions 7–12.
- **6c** — `cf-access`: RS256 verification against cached JWKS, the `aud`-array and `iss` checks, the `service_token_status` discriminator, and the `common_name` rejection.

## Global Constraints

Copied verbatim from the spec. Every task's requirements implicitly include this section.

- **`sysinfo` is pinned to `0.38.4`.** `0.39.x` requires Rust 1.95.
- **`-D clippy::disallowed_types` is a REQUIRED build gate.** No new `Command` site.
- **Rust version floor: 1.94.1.**
- **Rejection body.** A rejected request returns `{ "error": "unauthorized" }` and **nothing else** — no guard name, no chain composition, no hint about which leg failed. Which guard rejected goes to the log buffer, which sits behind the guard. **The no-leak rule applies to all unauthenticated responses, not only `/api/health`.**
- **All configured guards must pass.** `CDASH_AUTH` is comma-composable and the composition is AND.
- **Do not hand-roll verification of an attacker-supplied signature.** (Binds on 6c; nothing in 6a verifies a signature.)

## One deviation, stated plainly

Spec assertion 2 says the guarded-route list is "derived by enumerating the router at test time, not hand-written". **Axum exposes no public API to enumerate a `Router`'s routes** — verified against 0.8.9 before writing this plan. The purpose of that requirement is that *a route added later without a guard fails the test on the day it is added*, and it is preserved differently: every guarded route is declared once in a `guarded_routes!` macro invocation that emits **both** the router and the path list the test walks. A route cannot be added to the router through that macro without appearing in the list.

What this does not catch is a route added to the *unauthenticated* half by hand. Assertion 1's companion check closes that: the unauthenticated router's own path list is asserted to be exactly the three exceptions, so a fourth fails the test.

---

### Task 1: `CDASH_AUTH` parsing and the guard set

The step upstream of the composer. A parse that silently drops a leg leaves every other assertion passing, because they all run a single guard and a dropped leg is indistinguishable from a working one-guard chain.

**Files:**
- Create: `crates/agent/src/auth/mod.rs`
- Create: `crates/agent/src/auth/config.rs`
- Modify: `crates/agent/src/lib.rs`

**Interfaces:**
- Consumes: nothing.
- Produces:
  - `pub enum GuardKind { None, Bearer, CfAccess, TrustedProxy, Password }`
  - `pub fn parse_auth(spec: &str) -> Result<Vec<GuardKind>, String>`
  - `pub struct AuthConfig { pub guards: Vec<GuardKind>, pub token: Option<String>, pub proxy_header: String, pub proxy_allow: Vec<IpAddr> }`
  - `pub fn config_from_env() -> Result<AuthConfig, String>`

- [x] **Step 1: Write the failing tests**

`crates/agent/src/auth/config.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_empty_or_absent_setting_is_the_local_default() {
        assert_eq!(parse_auth("").unwrap(), vec![GuardKind::None]);
        assert_eq!(parse_auth("none").unwrap(), vec![GuardKind::None]);
    }

    #[test]
    fn every_named_leg_is_kept() {
        // The defect this guards: a parse that drops a leg is invisible to
        // every other test, because one working guard looks like a chain.
        assert_eq!(
            parse_auth("bearer,password").unwrap(),
            vec![GuardKind::Bearer, GuardKind::Password]
        );
        assert_eq!(
            parse_auth("password,cf-access").unwrap(),
            vec![GuardKind::Password, GuardKind::CfAccess]
        );
        assert_eq!(parse_auth("bearer, trusted-proxy ").unwrap().len(), 2, "whitespace is trimmed");
    }

    #[test]
    fn an_unknown_leg_is_an_error_not_a_silent_drop() {
        // Falling back to `none` on a typo would turn a guarded origin into an
        // open one, on an origin where every caller gets RCE.
        let e = parse_auth("bearer,paswrod").unwrap_err();
        assert!(e.contains("paswrod"));
        assert!(parse_auth("nonsense").is_err());
    }

    #[test]
    fn none_composed_with_a_real_guard_is_rejected() {
        // `none,bearer` reads as "no auth AND bearer", which is either a typo
        // or a misunderstanding. Neither should silently become one of them.
        assert!(parse_auth("none,bearer").is_err());
    }

    #[test]
    fn bearer_without_a_token_is_a_boot_error() {
        let cfg = AuthConfig::build(vec![GuardKind::Bearer], None, "X-Forwarded-Email".into(), vec![]);
        assert!(cfg.is_err());
    }

    #[test]
    fn trusted_proxy_without_an_allowlist_is_a_boot_error() {
        // Accepting an identity header from anywhere is the whole vulnerability
        // the allowlist exists to prevent.
        let cfg = AuthConfig::build(
            vec![GuardKind::TrustedProxy],
            None,
            "X-Forwarded-Email".into(),
            vec![],
        );
        assert!(cfg.is_err());
    }
}
```

- [x] **Step 2: Run tests to verify they fail**

`crates/agent/src/auth/mod.rs`:

```rust
pub mod config;
```

Add to `crates/agent/src/lib.rs`:

```rust
pub mod auth;
```

Run: `cargo test -p cdash-agent auth::config`
Expected: FAIL — `cannot find function 'parse_auth' in this scope`.

- [x] **Step 3: Write the implementation**

Prepend to `crates/agent/src/auth/config.rs`:

```rust
use std::net::IpAddr;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GuardKind {
    None,
    Bearer,
    CfAccess,
    TrustedProxy,
    Password,
}

/// Parse the comma-composable `CDASH_AUTH`. An unrecognised leg is an error,
/// never a silent fall back to `none`: this origin runs every session with
/// `--dangerously-skip-permissions`.
pub fn parse_auth(spec: &str) -> Result<Vec<GuardKind>, String> {
    let legs: Vec<&str> = spec.split(',').map(|s| s.trim()).filter(|s| !s.is_empty()).collect();
    if legs.is_empty() {
        return Ok(vec![GuardKind::None]);
    }
    let mut out = Vec::new();
    for leg in &legs {
        out.push(match *leg {
            "none" => GuardKind::None,
            "bearer" => GuardKind::Bearer,
            "cf-access" => GuardKind::CfAccess,
            "trusted-proxy" => GuardKind::TrustedProxy,
            "password" => GuardKind::Password,
            other => return Err(format!("unknown CDASH_AUTH value: {other}")),
        });
    }
    if out.len() > 1 && out.contains(&GuardKind::None) {
        return Err("CDASH_AUTH: 'none' cannot be composed with another guard".to_string());
    }
    Ok(out)
}

#[derive(Debug, Clone)]
pub struct AuthConfig {
    pub guards: Vec<GuardKind>,
    pub token: Option<String>,
    pub proxy_header: String,
    pub proxy_allow: Vec<IpAddr>,
}

impl AuthConfig {
    /// Every guard's required inputs are checked here, at boot, rather than on
    /// the first request that needs them.
    pub fn build(
        guards: Vec<GuardKind>,
        token: Option<String>,
        proxy_header: String,
        proxy_allow: Vec<IpAddr>,
    ) -> Result<Self, String> {
        if guards.contains(&GuardKind::Bearer) && token.as_deref().unwrap_or("").is_empty() {
            return Err("CDASH_AUTH includes 'bearer' but CDASH_TOKEN is unset".to_string());
        }
        if guards.contains(&GuardKind::TrustedProxy) && proxy_allow.is_empty() {
            return Err(
                "CDASH_AUTH includes 'trusted-proxy' but CDASH_PROXY_ALLOW names no upstream IP"
                    .to_string(),
            );
        }
        Ok(Self { guards, token, proxy_header, proxy_allow })
    }

    pub fn is_open(&self) -> bool {
        self.guards == [GuardKind::None]
    }
}

pub fn config_from_env() -> Result<AuthConfig, String> {
    let guards = parse_auth(&std::env::var("CDASH_AUTH").unwrap_or_default())?;
    let proxy_allow = std::env::var("CDASH_PROXY_ALLOW")
        .unwrap_or_default()
        .split(',')
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .map(|s| s.parse::<IpAddr>().map_err(|_| format!("CDASH_PROXY_ALLOW: bad IP: {s}")))
        .collect::<Result<Vec<_>, _>>()?;
    AuthConfig::build(
        guards,
        std::env::var("CDASH_TOKEN").ok(),
        std::env::var("CDASH_PROXY_HEADER").unwrap_or_else(|_| "X-Forwarded-Email".to_string()),
        proxy_allow,
    )
}
```

- [x] **Step 4: Run tests to verify they pass**

Run: `cargo test -p cdash-agent auth::config`
Expected: PASS, 6 tests.

- [x] **Step 5: Commit**

```bash
git add crates/agent/src/auth/ crates/agent/src/lib.rs
git commit -m "feat: CDASH_AUTH parsing that refuses an unknown leg rather than opening the origin"
```

---

### Task 2: The `bearer` and `trusted-proxy` verifiers

Two pure functions over request-derived values, so both are testable without a server.

**Files:**
- Create: `crates/agent/src/auth/guards.rs`
- Modify: `crates/agent/src/auth/mod.rs`
- Modify: `crates/agent/Cargo.toml`

**Interfaces:**
- Consumes: `AuthConfig` (Task 1).
- Produces:
  - `pub fn check_bearer(header: Option<&str>, token: &str) -> bool`
  - `pub fn check_trusted_proxy(peer: Option<IpAddr>, identity: Option<&str>, allow: &[IpAddr]) -> bool`

- [x] **Step 1: Add the constant-time compare dependency**

In `crates/agent/Cargo.toml`:

```toml
subtle = "2"
```

- [x] **Step 2: Write the failing tests**

`crates/agent/src/auth/guards.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bearer_accepts_only_the_exact_token_with_the_scheme() {
        assert!(check_bearer(Some("Bearer s3cret"), "s3cret"));
        assert!(!check_bearer(Some("Bearer wrong"), "s3cret"));
        assert!(!check_bearer(Some("s3cret"), "s3cret"), "the scheme is required");
        assert!(!check_bearer(None, "s3cret"));
        assert!(!check_bearer(Some("Bearer "), "s3cret"));
    }

    #[test]
    fn bearer_is_length_safe() {
        // `subtle` is length-safe by construction; a prefix must not pass.
        assert!(!check_bearer(Some("Bearer s3"), "s3cret"));
        assert!(!check_bearer(Some("Bearer s3cretlonger"), "s3cret"));
    }

    #[test]
    fn bearer_never_accepts_an_empty_configured_token() {
        // Belt and braces: `AuthConfig::build` already refuses this at boot.
        assert!(!check_bearer(Some("Bearer "), ""));
        assert!(!check_bearer(Some("Bearer x"), ""));
    }

    #[test]
    fn trusted_proxy_requires_both_an_allowed_peer_and_an_identity() {
        let allow: Vec<IpAddr> = vec!["10.0.0.5".parse().unwrap()];
        assert!(check_trusted_proxy(Some("10.0.0.5".parse().unwrap()), Some("u@x"), &allow));
        // The whole vulnerability this guard must not have: an identity header
        // accepted from an unlisted peer.
        assert!(!check_trusted_proxy(Some("10.0.0.6".parse().unwrap()), Some("u@x"), &allow));
        assert!(!check_trusted_proxy(None, Some("u@x"), &allow));
        assert!(!check_trusted_proxy(Some("10.0.0.5".parse().unwrap()), None, &allow));
        assert!(!check_trusted_proxy(Some("10.0.0.5".parse().unwrap()), Some(""), &allow));
    }

    #[test]
    fn trusted_proxy_with_an_empty_allowlist_accepts_nobody() {
        assert!(!check_trusted_proxy(Some("10.0.0.5".parse().unwrap()), Some("u@x"), &[]));
    }
}
```

- [x] **Step 3: Run tests to verify they fail**

Add to `crates/agent/src/auth/mod.rs`:

```rust
pub mod guards;
```

Run: `cargo test -p cdash-agent auth::guards`
Expected: FAIL — `cannot find function 'check_bearer' in this scope`.

- [x] **Step 4: Write the implementation**

Prepend to `crates/agent/src/auth/guards.rs`:

```rust
use std::net::IpAddr;
use subtle::ConstantTimeEq;

/// Constant-time compare against `CDASH_TOKEN`. `subtle` is length-safe by
/// construction rather than by a wrapper, so unequal lengths are handled
/// without an early return that would leak length by timing.
pub fn check_bearer(header: Option<&str>, token: &str) -> bool {
    if token.is_empty() {
        return false;
    }
    let Some(presented) = header.and_then(|h| h.strip_prefix("Bearer ")) else {
        return false;
    };
    presented.as_bytes().ct_eq(token.as_bytes()).into()
}

/// Accept an identity header, but only from a configured upstream. Off by
/// default and unsafe unless the origin is unreachable except through the
/// proxy — anyone who can reach the origin directly can set the header.
pub fn check_trusted_proxy(
    peer: Option<IpAddr>,
    identity: Option<&str>,
    allow: &[IpAddr],
) -> bool {
    let Some(peer) = peer else { return false };
    if !allow.contains(&peer) {
        return false;
    }
    identity.is_some_and(|i| !i.is_empty())
}
```

- [x] **Step 5: Run tests to verify they pass**

Run: `cargo test -p cdash-agent auth::guards`
Expected: PASS, 5 tests.

- [x] **Step 6: Commit**

```bash
git add crates/agent/src/auth/ crates/agent/Cargo.toml Cargo.lock
git commit -m "feat: bearer and trusted-proxy verifiers as pure, testable functions"
```

---

### Task 3: The guard layer and the two-half router

The structural change. Everything except three routes goes behind a layer, so a route added to the guarded half **cannot** escape the guard regardless of where it is written.

**Files:**
- Create: `crates/agent/src/auth/layer.rs`
- Modify: `crates/agent/src/auth/mod.rs`
- Modify: `crates/agent/src/http/serve.rs`

**Interfaces:**
- Consumes: `AuthConfig`, `check_bearer`, `check_trusted_proxy`.
- Produces:
  - `pub async fn guard_mw(State<GuardState>, ConnectInfo<SocketAddr>, Request, Next) -> Response`
  - `pub struct GuardState { pub auth: Arc<AuthConfig>, pub log: Arc<LogBuffer> }`
  - `guarded_routes!` — the macro emitting both the guarded router and `GUARDED_PATHS`
  - `pub const UNAUTH_PATHS: &[&str]` — exactly the three exceptions

- [x] **Step 1: Write the failing tests**

`crates/agent/src/auth/layer.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exactly_three_routes_are_unauthenticated() {
        // Assertion 1's companion: a fourth exception fails here on the day it
        // is added. `/login` and `/api/login` land in plan 6b; until then the
        // list is shorter, and this test says so rather than passing blindly.
        assert!(UNAUTH_PATHS.contains(&"/api/health"));
        assert!(UNAUTH_PATHS.len() <= 3, "no more than the three enumerated exceptions");
    }

    #[test]
    fn every_guarded_route_is_listed_for_the_bypass_test() {
        // The list and the router come from one macro invocation, so this
        // cannot drift. Spot-check the routes that matter most.
        for p in ["/api/sessions", "/api/logs", "/api/kill", "/api/launch", "/api/hostinfo"] {
            assert!(GUARDED_PATHS.iter().any(|(_, path)| *path == p), "{p} must be guarded");
        }
        assert!(!GUARDED_PATHS.iter().any(|(_, p)| *p == "/api/health"));
    }
}
```

- [x] **Step 2: Run tests to verify they fail**

Add to `crates/agent/src/auth/mod.rs`:

```rust
pub mod layer;
```

Run: `cargo test -p cdash-agent auth::layer`
Expected: FAIL — `cannot find value 'UNAUTH_PATHS' in this scope`.

- [x] **Step 3: Write the guard layer**

Prepend to `crates/agent/src/auth/layer.rs`:

```rust
use super::config::{AuthConfig, GuardKind};
use super::guards::{check_bearer, check_trusted_proxy};
use crate::host::log::LogBuffer;
use axum::extract::{ConnectInfo, Request, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use std::net::SocketAddr;
use std::sync::Arc;

/// The three unauthenticated exceptions, enumerated rather than implied.
/// `/login` and `POST /api/login` join this list in plan 6b.
pub const UNAUTH_PATHS: &[&str] = &["/api/health"];

#[derive(Clone)]
pub struct GuardState {
    pub auth: Arc<AuthConfig>,
    pub log: Arc<LogBuffer>,
}

/// A rejected request says only this. Which leg failed goes to the log buffer,
/// which sits behind the guard.
fn unauthorized() -> Response {
    (StatusCode::UNAUTHORIZED, Json(serde_json::json!({ "error": "unauthorized" }))).into_response()
}

/// All configured guards must pass.
pub async fn guard_mw(
    State(st): State<GuardState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    req: Request,
    next: axum::middleware::Next,
) -> Response {
    if st.auth.is_open() {
        return next.run(req).await;
    }

    let bearer = req
        .headers()
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string());
    let proxy_identity = req
        .headers()
        .get(&st.auth.proxy_header)
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string());

    for g in &st.auth.guards {
        let ok = match g {
            GuardKind::None => true,
            GuardKind::Bearer => {
                check_bearer(bearer.as_deref(), st.auth.token.as_deref().unwrap_or(""))
            }
            GuardKind::TrustedProxy => check_trusted_proxy(
                Some(peer.ip()),
                proxy_identity.as_deref(),
                &st.auth.proxy_allow,
            ),
            // Not implemented until plans 6b and 6c. Refusing here is the safe
            // direction: a configured-but-unimplemented guard must never pass.
            GuardKind::Password | GuardKind::CfAccess => false,
        };
        if !ok {
            st.log.push(format!("auth: rejected by {g:?}"));
            return unauthorized();
        }
    }
    next.run(req).await
}
```

`GuardKind` needs `Debug` for the log line — it already derives it.

- [x] **Step 4: Write the route macro and split the router**

Add to `crates/agent/src/auth/layer.rs`:

```rust
/// Declares the guarded routes once. The macro emits both the router and the
/// path list the bypass test walks, so a route cannot be registered without
/// appearing in the list. Axum exposes no API to enumerate a built `Router`,
/// which is why the single source of truth lives here instead.
#[macro_export]
macro_rules! guarded_routes {
    ($($method:ident $path:literal => $handler:path),* $(,)?) => {
        pub fn guarded_router() -> axum::Router<std::sync::Arc<$crate::collect::ctx::Ctx>> {
            axum::Router::new()
                $(.route($path, axum::routing::$method($handler)))*
        }
        pub const GUARDED_PATHS: &[(&str, &str)] = &[$((stringify!($method), $path)),*];
    };
}
```

In `crates/agent/src/http/serve.rs`, replace the body of `router` with the two-half build:

```rust
use crate::auth::config::AuthConfig;
use crate::auth::layer::{guard_mw, GuardState};

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
}

pub fn router(ctx: Arc<Ctx>, public_dir: &Path, auth: Arc<AuthConfig>) -> Router {
    let st = GuardState { auth, log: Arc::clone(&ctx.host.log) };

    // Everything else, static assets included, behind one layer.
    let guarded = guarded_router()
        .fallback_service(tower_http::services::ServeDir::new(public_dir))
        .layer(axum::middleware::from_fn_with_state(st, guard_mw))
        .with_state(Arc::clone(&ctx));

    Router::new()
        .route("/api/health", get(|| async { Json(serde_json::json!({ "ok": true })) }))
        .merge(guarded)
}
```

`serve` builds the `AuthConfig` and passes it through; add to `Config`:

```rust
pub auth: Arc<AuthConfig>,
```

set in `from_env` from `crate::auth::config::config_from_env()`, and in `serve`:

```rust
    let app = router(Arc::clone(&ctx), &cfg.public_dir, Arc::clone(&cfg.auth));
```

`ConnectInfo` requires the peer address to be supplied by the server, so `serve`'s axum call becomes:

```rust
    tokio::spawn(async move {
        let _ = axum::serve(
            listener,
            app.into_make_service_with_connect_info::<SocketAddr>(),
        )
        .await;
    });
```

- [x] **Step 5: Update every existing test's `cfg_for`**

`Config` gained a field, so the helper in `crates/agent/src/http/serve.rs` needs:

```rust
            auth: Arc::new(
                AuthConfig::build(vec![GuardKind::None], None, "X-Forwarded-Email".into(), vec![])
                    .expect("none is always buildable"),
            ),
```

- [x] **Step 6: Run the whole suite**

Run: `cargo test -p cdash-agent -- --test-threads=1`
Expected: PASS. Every existing HTTP test runs under `CDASH_AUTH=none`, which is a pass-through, so none of them should change behaviour. **If any existing test now 401s, the pass-through is wrong** — fix that before continuing.

- [x] **Step 7: Commit**

```bash
git add crates/agent/src/auth/ crates/agent/src/http/
git commit -m "feat: guard layer and the two-half router, making unauthenticated a countable list"
```

---

### Task 4: `/api/hostinfo`

Authenticated. Delivers the macOS setup story: when `tmux` is missing the UI shows a setup screen with the install command and a re-check button rather than failing every launch with an opaque error.

**Files:**
- Modify: `crates/agent/src/http/routes.rs`

**Interfaces:**
- Consumes: `Host::missing` (step-2 plan, Task 13).
- Produces: `pub async fn get_hostinfo(State<Arc<Ctx>>) -> Response`

- [x] **Step 1: Write the failing test**

Add to the `tests` module in `crates/agent/src/http/routes.rs`:

```rust
    #[tokio::test]
    async fn hostinfo_reports_platform_version_and_missing_binaries() {
        let b = serve(cfg_for(tempdir("hostinfo"))).await.unwrap();
        let body = reqwest_get(&format!("http://{}/api/hostinfo", b.addr)).await;
        let v: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(v["platform"], std::env::consts::OS);
        assert_eq!(v["version"], env!("CARGO_PKG_VERSION"));
        assert!(v["missing"].is_array(), "the setup screen reads this array");
    }

    #[tokio::test]
    async fn hostinfo_missing_is_recomputed_not_a_boot_time_cache() {
        // UX-5: a user who installs tmux while the agent runs and presses
        // re-check must get the new answer.
        let b = serve(cfg_for(tempdir("hostinfo-recheck"))).await.unwrap();
        let url = format!("http://{}/api/hostinfo", b.addr);
        assert_eq!(reqwest_get(&url).await, reqwest_get(&url).await);
    }
```

- [x] **Step 2: Run tests to verify they fail**

Run: `cargo test -p cdash-agent hostinfo`
Expected: FAIL — 404, because the route does not exist yet.

- [x] **Step 3: Write the implementation**

Add to `crates/agent/src/http/routes.rs`:

```rust
/// Authenticated: it names the host's platform and which binaries are absent.
/// `/api/health` is the unauthenticated one and says only `{ok:true}`.
pub async fn get_hostinfo(State(ctx): State<Arc<Ctx>>) -> Response {
    Json(serde_json::json!({
        "platform": std::env::consts::OS,
        "version": env!("CARGO_PKG_VERSION"),
        // Re-probed per request, never a boot-time cache: the setup screen's
        // re-check button is worthless against a stale answer.
        "missing": ctx.host.missing(),
    }))
    .into_response()
}
```

The route is already declared in the `guarded_routes!` invocation from Task 3.

- [x] **Step 4: Run tests to verify they pass**

Run: `cargo test -p cdash-agent hostinfo`
Expected: PASS, 2 tests.

- [x] **Step 5: Commit**

```bash
git add crates/agent/src/http/routes.rs
git commit -m "feat: /api/hostinfo behind the guard, re-probing binaries per request"
```

---

### Task 5: The auth integration suite

Spec assertions 1–5. The spec calls the bypass test the highest-value test in the suite: it is what catches an accidental auth bypass.

**Files:**
- Create: `crates/agent/tests/auth_integration.rs`

**Interfaces:**
- Consumes: `serve`, `Config`, `AuthConfig`, `GUARDED_PATHS`, `UNAUTH_PATHS`.
- Produces: the suite.

- [x] **Step 1: Write the failing tests**

`crates/agent/tests/auth_integration.rs`:

```rust
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
async fn req(addr: &str, method: &str, path: &str, headers: &[&str], body: Option<&str>) -> (u16, String) {
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
    let (status, _) = req(&a, "GET", "/api/sessions", &["Authorization: Bearer s3cret"], None).await;
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

    let (only_proxy, _) =
        req(&a, "GET", "/api/sessions", &["X-Forwarded-Email: u@x"], None).await;
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
```

- [x] **Step 2: Export what the suite imports**

An integration test links the crate as a library, so `GUARDED_PATHS` must be public from `http::serve`, and `Config`'s fields must be public (they already are).

Run: `cargo test -p cdash-agent --test auth_integration`
Expected: FAIL to compile — unresolved imports — then FAIL on assertions until Task 3's layer is in place.

- [x] **Step 3: Make the suite pass**

No new production code should be required: Tasks 1–4 provide everything. If an assertion fails, the guard is wrong — fix the guard, not the assertion. Two failures to expect and what they mean:

- **Assertion 3 returns 200 for `/index.html`** — the static service was attached outside the guarded half.
- **Assertion 2 returns 415 rather than 401 for a POST** — the guard layer runs after the body extractor. The layer must reject before extraction; `from_fn_with_state` applied with `.layer()` on the router runs before handler extractors, so check the layer is on the guarded router rather than on an individual route.

- [x] **Step 4: Run the suite**

Run: `cargo test -p cdash-agent --test auth_integration -- --test-threads=1`
Expected: PASS, 6 tests.

- [x] **Step 5: Verify the bypass test actually fails on a bypass**

A gate nobody has seen fail is not known to work. Temporarily add an unguarded route to the **unauthenticated** half in `crates/agent/src/http/serve.rs`:

```rust
        .route("/api/sessions-oops", get(routes::get_sessions))
```

and add `get "/api/sessions-oops" => routes::get_sessions,` to the `guarded_routes!` invocation so the list knows about it.

Run: `cargo test -p cdash-agent --test auth_integration`
Expected: **FAIL** — `GET /api/sessions-oops was reachable unauthenticated`.

Remove both lines and re-run; expected: PASS.

- [x] **Step 6: Run the full gate**

Run: `cargo test --all --locked -- --test-threads=1`
Run: `cargo clippy --all-targets --locked -- -D warnings -D clippy::disallowed_types`
Expected: PASS and exit 0.

- [x] **Step 7: Commit**

```bash
git add crates/agent/tests/auth_integration.rs
git commit -m "test: auth integration suite, assertions 1-5, with a verified bypass check"
```

---

## What this plan deliberately does not cover

- **The `password` guard** and everything it implies — sessions, cookies, `/login`, the throttle, boot refusals, `set-password`. Plan 6b. Until then a configured `password` leg **rejects every request**, which is the safe direction for an unimplemented guard.
- **`cf-access`.** Plan 6c, same rejection rule.
- **The UI.** Spec step 7.

## Next plan starts here

Plan 6b: the `password` guard. Its first task is the scrypt hash parse and constant-time verify, because every other piece — the session store, the login route, the throttle — depends on knowing whether a password was correct.
