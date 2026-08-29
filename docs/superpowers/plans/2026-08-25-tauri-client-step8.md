# Tauri Client Step 8 Implementation Plan — in-process agent, profile store, api_request

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Ship spec step 8 — a Tauri v2 desktop client that links the agent as a library, serves it in-process on a tokio task, exposes the narrow command set to the webview, and drives the existing UI over `api_request`.

**Architecture:** One new workspace member, `crates/tauri-app/`, depending on `cdash-agent` as a library. The HTTP boundary is kept even in-process: `serve(Config)` runs as a tokio task, the caller holds the returned bound address directly (readiness is a resolved future, nothing polls), and the webview calls it over HTTP through a single `api_request` Rust command that attaches the active profile's credentials. Non-secret profile fields live in `tauri-plugin-store`. Secrets (keyring, password variant) are **step 10** and deliberately absent here.

**Tech Stack:** Tauri v2 (`tauri = "2"`, `tauri-plugin-store = "2"`), `reqwest` (already in the tree via the agent), tokio multi-thread runtime (Tauri brings its own).

**Spec:** `docs/superpowers/specs/2026-07-30-tauri-multi-host-design.md` — sections [Tauri client — `src-tauri/`](#3-tauri-client--src_tauri), [The agent as a crate](#the-agent-as-a-crate), [Commands exposed to the webview](#commands-exposed-to-the-webview), [Where the server runs, per platform], [UI — `public/`] (the transport branch).

## Global Constraints

- The HTTP boundary is never bypassed: the webview speaks HTTP to loopback, even when the agent is in-process. No Tauri command duplicates an API route.
- The webview receives **no filesystem, shell, or network capability** beyond the command list below.
- Command surface exactly: `api_request`, `profiles_list`, `profile_save`, `profile_delete`, `profile_activate`, `server_start`, `server_stop`, `server_state`, `host_platform`.
- Managed (in-process) servers boot with `CDASH_BIND=127.0.0.1` and `CDASH_AUTH=none` on a free port (`PORT=0`) — same trust shape as the standalone loopback default.
- `api_request(method, path, body) -> { status, body }` is the entire data path; CORS therefore does not apply.
- The Tauri detection predicate in the UI is `'__TAURI_INTERNALS__' in window || '__TAURI__' in window`; step 8 must confirm which global the configured runtime actually injects and record it (this plan sets `app.withGlobalTauri: true`, which injects `window.__TAURI__`).
- Version skew cannot arise in-process (same build); no banner logic ships in this step.
- macOS verification needs a Mac and is out of scope for this container; everything here must compile and run on Linux.

---

### Task 1: Scaffold `crates/tauri-app` — it compiles and boots a webview

**Files:**
- Create: `crates/tauri-app/Cargo.toml`
- Create: `crates/tauri-app/build.rs`
- Create: `crates/tauri-app/tauri.conf.json`
- Create: `crates/tauri-app/src/main.rs`
- Modify: `/mnt/d/git/claude-dashboard/Cargo.toml` (workspace members)

**Interfaces:**
- Consumes: nothing from other tasks.
- Produces: a Tauri app whose builder compiles; later tasks register commands on it.

- [x] **Step 1: Workspace member**

In the root `Cargo.toml`, add `"crates/tauri-app"` to `[workspace] members`.

`crates/tauri-app/Cargo.toml`:

```toml
[package]
name = "cdash-tauri"
version = "0.1.0"
edition = "2021"

[build-dependencies]
tauri-build = { version = "2", features = [] }

[dependencies]
tauri = { version = "2", features = [] }
tauri-plugin-store = "2"
serde = { version = "1", features = ["derive"] }
serde_json = "1"
tokio = { version = "1", features = ["full"] }
```

`crates/tauri-app/build.rs`:

```rust
fn main() { tauri_build::build() }
```

- [x] **Step 2: Configuration**

`crates/tauri-app/tauri.conf.json`:

```json
{
  "$schema": "https://schema.tauri.app/config/2",
  "productName": "claude-dashboard",
  "version": "0.1.0",
  "identifier": "dev.cdash.app",
  "build": {
    "frontendDist": "../../public"
  },
  "app": {
    "withGlobalTauri": true,
    "windows": [
      { "title": "claude-dashboard", "width": 1100, "height": 800 }
    ],
    "security": {
      "csp": null
    }
  },
  "bundle": {
    "active": false
  }
}
```

Notes for the reviewer: `withGlobalTauri: true` is load-bearing twice — it is the predicate confirmation for `public/app.js`'s transport branch (the runtime injects `window.__TAURI__`; `__TAURI_INTERNALS__` is present regardless), and it keeps the no-build-step property (classic scripts call `window.__TAURI__` directly, no bundler). `frontendDist` points at the shared `public/` — one copy of the UI across all delivery modes.

- [x] **Step 3: Minimal main**

`crates/tauri-app/src/main.rs`:

```rust
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    tauri::Builder::default()
        .plugin(tauri_plugin_store::Builder::new().build())
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
```

- [x] **Step 4: Compile gate**

Run: `cargo check --locked -p cdash-tauri`
Expected: PASS. First run pulls the Tauri tree; webkit2gtk-4.1 must be present (`pkg-config --exists webkit2gtk-4.1`).

- [x] **Step 5: Commit**

```bash
git add Cargo.toml crates/tauri-app
git commit -m "feat: tauri client skeleton embedding the shared public/ UI"
```

---

### Task 2: The agent in-process — `server_start` / `server_stop` / `server_state`

**Files:**
- Modify: `crates/tauri-app/src/main.rs` (or split `src/server.rs`; prefer keeping one file until >300 lines)
- Modify: `crates/agent/Cargo.toml` if a lib feature gate is needed (none expected — `serve` and `Config` are already public)

**Interfaces:**
- Consumes: `cdash_agent::http::serve::serve(cfg) -> std::io::Result<Bound>` — **verified**: it binds, spawns axum on an internal background task, and resolves immediately with `Bound { addr, ctx }`. The caller holds no future.
- Produces:
  - `server_start() -> Result<String, String>` — builds `Config { port: 0, bind: "127.0.0.1".into(), auth: none, ..Default }`, calls `serve`, stores bound address + shutdown trigger in `tauri::State<Mutex<Option<Running>>>`, returns `http://127.0.0.1:<port>`.
  - `server_stop()` — fires the shutdown trigger, clears state.
  - `server_state() -> Option<String>` — the bound address if running.

**Required agent change (small, this task):** `serve` currently has no way to stop its internal axum task. Switch the internal `axum::serve(...)` to `.with_graceful_shutdown(...)` driven by a `tokio::sync::watch` (or `oneshot`) created inside `serve`, and add the trigger (or a `stop()` method) to `pub struct Bound`. This keeps the standalone binary unchanged (it never stops) and gives the in-process caller real teardown. Test in the agent crate: start `serve`, call `bound.stop()`, await completion, assert the port is free again (rebind succeeds).

```rust
#[derive(Default)]
struct ServerState(std::sync::Mutex<Option<Running>>);

struct Running {
    addr: String,
    stop: tokio::sync::watch::Sender<bool>, // shape per the Bound change above
}

#[tauri::command]
fn server_start(state: tauri::State<ServerState>) -> Result<String, String> {
    let mut guard = state.0.lock().unwrap();
    if let Some(r) = guard.as_ref() { return Ok(r.addr.clone()); } // idempotent
    let cfg = cdash_agent::http::serve::Config {
        port: 0, bind: "127.0.0.1".into(),
        auth: Default::default(),          // CDASH_AUTH=none — the in-process trust shape
        ..Default::default()
    };
    // serve() resolves as soon as the listener is bound; call it from the
    // Tauri async runtime (e.g. tauri::async_runtime::block_on) and keep
    // Bound's stop trigger for server_stop.
    ...
}
```

The contract, regardless of exact plumbing: the caller learns the bound address synchronously after `serve` resolves; readiness is that resolution (nothing polls); a bind error propagates as `Err(String)` naming the reason — the spec's "held port is a diagnosed condition".

- [x] **Step 1: Read the real signature, then implement**

Run `grep -n 'pub async fn serve\|pub struct Bound\|pub fn router' crates/agent/src/http/*.rs` and adapt.

- [x] **Step 2: A test proves the lifecycle**

Add `crates/tauri-app/src/server_test.rs` (`#[cfg(test)]`) that calls the same internal start function the command wraps — not through Tauri — asserting: returns an `Ok(addr)` where addr starts `http://127.0.0.1:`; a second start returns the same address (idempotent); stop clears it. Run: `cargo test -p cdash-tauri`.

- [x] **Step 3: Commit**

```bash
git add crates/tauri-app
git commit -m "feat: in-process agent served on a tokio task with start/stop/state"
```

---

### Task 3: `api_request` — the entire data path

**Files:**
- Modify: `crates/tauri-app/src/main.rs`

**Interfaces:**
- Consumes: the running server's address (Task 2 state).
- Produces: `api_request(method: String, path: String, body: Option<serde_json::Value>) -> Result<{status: u16, body: serde_json::Value}, String>`.

- [x] **Step 1: Implement**

One `reqwest::Client` built once (no cookie jar — explicit per spec), stored in Tauri state. Attach `Authorization: Bearer …` only if the active profile carries a token (profiles arrive in Task 4; structure the code so credential attachment is a single function `attach_auth(&Profile, &mut RequestParts)` that is currently a no-op passthrough).

Reject non-loopback destinations: parse `path` against the bound address — requests go to `{bound}{path}`, never to an arbitrary URL from JS. Path must start with `/`.

```rust
#[derive(serde::Serialize)]
struct ApiResponse { status: u16, body: serde_json::Value }

#[tauri::command]
async fn api_request(
    state: tauri::State<'_, ServerState>,
    http: tauri::State<'_, ReqwestState>,
    method: String,
    path: String,
    body: Option<serde_json::Value>,
) -> Result<ApiResponse, String> {
    let addr = state.0.lock().unwrap().as_ref()
        .map(|r| r.addr.clone())
        .ok_or("server not running")?;
    if !path.starts_with('/') { return Err("path must be absolute".into()); }
    let url = format!("{addr}{path}");
    let m = match method.as_str() {
        "GET" => reqwest::Method::GET, "POST" => reqwest::Method::POST,
        m => reqwest::Method::from_bytes(m.as_bytes()).map_err(|e| e.to_string())?,
    };
    let mut req = http.0.request(m, &url);
    if let Some(b) = body { req = req.json(&b); }
    let res = req.send().await.map_err(|e| e.to_string())?;
    let status = res.status().as_u16();
    let body = res.json::<serde_json::Value>().await.unwrap_or(serde_json::Value::Null);
    Ok(ApiResponse { status, body })
}
```

- [x] **Step 2: Test**

Extend the test module: boot the in-process server, call the internal request function against `/api/health`, assert `status == 200` and `body.ok == true`; assert a relative path (`"api/x"`) is rejected.

- [x] **Step 3: Commit**

```bash
git add crates/tauri-app
git commit -m "feat: api_request command, loopback-bound, no cookie jar"
```

---

### Task 4: Profiles — non-secret fields in tauri-plugin-store

**Files:**
- Modify: `crates/tauri-app/src/main.rs`

**Interfaces:**
- Consumes: the plugin from Task 1.
- Produces: `profiles_list() -> Vec<ProfileRecord>`, `profile_save(profile: ProfileInput)`, `profile_delete(name: String)`, `profile_activate(name: String)`, `host_platform() -> String`.

`ProfileRecord { name, base_url, managed: "in-process" | "external", auth: "none", has_secret: false }` — `has_secret` is hardcoded false until step 10 wires the keyring; `auth` accepts only `"none"` in this step and rejects anything else with a message naming step 10 (fail closed rather than silently ignoring a secret-bearing profile).

Storage: one JSON document under the store key `"profiles"`. `profile_activate` records the chosen name under key `"active"`. All commands operate through the store plugin's Rust API; nothing here touches disk outside the app-config dir.

- [x] **Step 1: Implement the four commands + `host_platform`** (`std::env::consts::OS`)
- [x] **Step 2: Register every command on the builder; compile**
- [x] **Step 3: Round-trip test** for save/list/delete/activate against a temp store dir
- [x] **Step 4: Commit**

```bash
git add crates/tauri-app
git commit -m "feat: profile store and the narrowed command surface"
```

---

### Task 5: Predicate confirmation recorded + wiring notes

**Files:**
- Modify: `public/app.js` (comment only)
- Modify: `README.md`

- [x] **Step 1: Record the confirmed predicate**

Update the `ponytail:` comment at `public/app.js:111` and the index.html registration gate comment: with `withGlobalTauri: true` (set in Task 1's conf), the runtime injects `window.__TAURI__` (and `__TAURI_INTERNALS__` unconditionally); the existing OR-predicate therefore fires correctly in the Tauri webview and never on the web. Remove "unconfirmed" wording.

Note: `invokeTauri`'s provisional `invoke('api', { path, body })` must now name the real command: `api_request`, invoked as
`window.__TAURI__.core.invoke('api_request', { method, path, body })` (Tauri v2 global API shape — verify against https://tauri.app/develop/calling-rust/ at execution time and adapt; args are passed camelCase-mapped). Update `api()`'s Tauri branch accordingly, mapping GET/POST and JSON body per Task 3's signature.

- [x] **Step 2: README section**

Short "Desktop client (Linux)" paragraph: `cargo run -p cdash-tauri`, in-process agent, profiles stored in app config, secrets arrive in step 10.

- [x] **Step 3: Full gate**

Run: `npm test && cargo clippy --all-targets --locked -- -D warnings -D clippy::disallowed_types && cargo test --all --locked`
Expected: PASS everywhere.

Ran green only after the follow-up review: `cargo test --all` builds `cdash-tauri`,
which needs webkit2gtk and libappindicator — absent on `ubuntu-latest`, so CI had
been failing in a build script before a single test ran, masking a racing
cf-access test fixture underneath. Both fixed; the gate is now honest.

- [x] **Step 4: Commit**

```bash
git add public/app.js public/index.html README.md
git commit -m "docs: confirm the Tauri detection predicate and name api_request"
```

---

## Found after the fact

Every task above shipped, but nothing ever called `server_start` — not the
builder, not `public/app.js` — so `api_request` answered `server not running`
for every request and the desktop client was inert on launch. The client *is*
the server on Linux and macOS, so it now starts the agent in Tauri's `setup`
hook. The plan never named an owner for that call; a future step that adds the
external/VPS profile has to decide when *not* to start one.

## What this plan does not cover

- **Secrets** — keyring, password variant, login flow, Rule A/B: step 10.
- **WSL relay** — spawn, pidfile, copy-in cache: step 9.
- **macOS execution** — needs a Mac; everything here gates on Linux compile + tests only.
- **Bundling/icons** — `bundle.active: false`; installers are a later concern.
- **GUI run verification** — this container has no display; the gate is compile + unit tests. A human runs `cargo run -p cdash-tauri` on a desktop to see the window.
