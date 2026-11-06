# Browser View Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A **Browser** button on dashboard session cards that streams the session's Playwright-driven Chromium tab to the phone and forwards taps and typing, so the user can log in remotely. Support any Chromium-based browser exposing the required CDP methods; Edge is optional.

**Architecture:** cdash-agent reads a hand-edited folder-to-port map, probes the browser's loopback CDP port for the session's folder, and exposes `GET /api/browser` (WebSocket). The bridge speaks CDP (`Page.startScreencast`, `Input.*`, `Target.*`) and a narrow JSON protocol to the phone. The phone overlay reuses the Terminal overlay's shell. Existing Playwright launchers select the Chromium executable/channel and matching dedicated profile; they gain a debugging port.

**Tech Stack:** Rust (axum 0.8 `ws`, tokio, tokio-tungstenite 0.29, reqwest 0.13, serde_json), vanilla JS, `node --test`.

**Spec:** `docs/superpowers/specs/2026-10-21-browser-view-design.md`. Read it first; this plan argues from it.

**Review baseline:** `origin/master` `60cbf75`, reviewed 2026-11-06. Original PR head: `8814cef`. See the spec's review ledger for issues, evidence and residual risks. This PR remains documentation only; every task below is pending.

## Global Constraints

- No new crates or npm packages. CDP client uses `tokio-tungstenite`; HTTP probes use `reqwest`; the test fake uses `axum` with `ws`.
- Port map file: `$CLAUDE_DIR/cdash-browsers.json`, JSON object of absolute WSL dir to port. Ports outside `1024..=65535` ignored.
- cdash-agent only connects to `127.0.0.1:<port>` from the map. Never another host.
- All browser-discovery HTTP clients use `.no_proxy()` and `.redirect(reqwest::redirect::Policy::none())`. A redirect is failure, not a request to follow. Rebuild the WebSocket authority from the mapped host/port; validate and take only its `/devtools/browser/...` path, never its advertised authority, query or fragment.
- Never send an `Origin` header on the CDP WebSocket. Do not add `--remote-allow-origins=*` or disable a sandbox.
- No product-name allowlist, `Edg/` check or hard-coded `msedge.exe` path. Chromium channels/custom executables are launcher configuration. Keep a dedicated profile belonging to that browser; Chrome 136+ ignores debugging switches for its default personal profile.
- CDP methods cdash-agent may call: `Target.getTargets`, `Target.setDiscoverTargets`, `Target.attachToTarget`, `Target.detachFromTarget`, `Page.enable`, `Page.startScreencast`, `Page.stopScreencast`, `Page.screencastFrameAck`, `Page.getNavigationHistory`, `Page.navigateToHistoryEntry`, `Page.reload`, `Input.dispatchMouseEvent`, `Input.dispatchKeyEvent`. Nothing else, specifically never `Network.*`, `Storage.*`, `Runtime.*`, `Target.activateTarget`, `Browser.setWindowBounds`, `Input.insertText`.
- Log lines, exact: `browser open <name>`, `browser close <name>`, `browser refused: <category>`, `browser failed <name>: <category>`. Categories: `origin`, `name`, `session`, `mapping`, `connect`, `unsupported`, `protocol`, `timeout`, `send`. Only validated names; never raw errors, headers, URLs, titles, input or message bodies.
- Text cap: `MAX_TEXT = 4096` bytes per `text` message.
- Keys and Windows virtual key codes: Enter 13, Tab 9, Backspace 8, Escape 27, ArrowLeft 37, ArrowUp 38, ArrowRight 39, ArrowDown 40.
- Close codes on `/api/browser`: `4002` reason `Browser not running`, `4003` reason `Browser closed`, `4004` reason `Browser view unsupported` for a missing required CDP method (`-32601`), `1011` reason `Browser connection failed` otherwise. Discard raw error text before logging or replying.
- Screencast: JPEG, `quality: 60`, `maxWidth: 1280`. Ack only after the phone send completes.
- Ports: JOBS `9301`, INVEST `9302`.
- The view never changes the viewed tab by itself; only a phone `tab` message does (initial pick excepted).
- Forward frame `deviceWidth`, `deviceHeight`, `offsetTop`, `pageScaleFactor`; require positive finite dimensions. Input uses the metadata paired with the displayed JPEG, measured relative to the transformed image, not the stage. Disable input on tab switch/reconnect until that pair is displayed.
- Origins are serialized tuple origins or JSON `null`; no raw URL fallback. Use DOM text/property assignment for tab strings, never HTML.

## Review Focus

- Browser not running when Browser is tapped (button left over from an earlier poll): the overlay must show "Browser not running", not hang. Test: Task 5, `browser_not_running_closes_4002`.
- Browser closes while viewing (agent's `browser_close`, crash): the overlay shows "Browser closed" and cdash-agent stops cleanly. Test: Task 5, `browser_going_away_closes_4003`.
- A sibling folder sharing a prefix (`/mnt/d/p/Downloads/JOBS2` vs key `/mnt/d/p/Downloads/JOBS`) must not get JOBS's browser. Test: Task 1, `prefix_match_is_by_path_component`.
- Input with no frame yet, or a tap outside the frame: dropped or clamped, never sent with garbage coordinates. Test: Task 6, `input_before_first_frame_is_dropped` and Task 3, `clamp_keeps_points_inside_the_viewport`.
- Non-ASCII and astral text (`é`, `😀`): one key press per Unicode scalar, `text` equal to that character, no virtual key code. Test: Task 3, `chars_become_one_key_press_each`.
- Hostile discovery responses cannot redirect HTTP or WebSocket traffic off mapped loopback. Tests: Tasks 2/4, `discovery_never_follows_redirects`, `discovery_pins_the_websocket_authority`.
- Error text can contain secrets: logs/close reasons retain only fixed categories. Test: Task 5, `errors_never_expose_canaries`.
- IME commit/cancel and duplicate final input: no partial/doubled text, clear on close. Test: Task 8, `composition_is_forwarded_once_on_commit`.
- Opaque origins and page-controlled titles: no full URL or injected HTML in tab labels. Tests: Tasks 5/8, `opaque_origins_are_null`, `tab_labels_are_text`.
- Frame decode can finish after a tab switch/reconnect: stale frames never restore input. Tests: Tasks 7/8, `frame_pairing_survives_switch_and_reconnect`.

## Browser API notes (incorporated in the spec)

- **Tab picker requires a user action.** `HTMLSelectElement.showPicker()` needs transient user activation. When `active` is null, select the `Pick a tab` placeholder and show `Tab closed. Pick a tab.`; the user opens the native picker. No custom picker.
- **Frame coordinates start at the image.** Forward all four metadata fields. A point measured from the image's top needs no extra `offsetTop`; it is a strip above the image in DevTools' canvas. Do not divide dispatched mouse coordinates by `pageScaleFactor` (see the spec's DevTools source). Test image scaling/pan and non-default desktop zoom/DPI on the selected browser.

---

### Task 1: Port map and folder matching

**Files:**
- Create: `crates/agent/src/collect/browser.rs`
- Modify: `crates/agent/src/collect/mod.rs` (add `pub mod browser;`)

**Interfaces:**
- Produces: `pub const MAP_FILE: &str = "cdash-browsers.json";`, `pub async fn read_port_map(claude_dir: &Path) -> Vec<(String, u16)>`, `pub fn port_for(map: &[(String, u16)], dir: &str) -> Option<u16>`.

- [ ] **Step 1: Write the failing tests** (in `browser.rs` `#[cfg(test)] mod tests`)

```rust
#[test]
fn prefix_match_is_by_path_component() {
    let map = vec![("/mnt/d/p/Downloads/JOBS".to_string(), 9301), ("/mnt/d/p".to_string(), 9400)];
    assert_eq!(port_for(&map, "/mnt/d/p/Downloads/JOBS"), Some(9301));
    assert_eq!(port_for(&map, "/mnt/d/p/Downloads/JOBS/sub"), Some(9301));
    assert_eq!(port_for(&map, "/mnt/d/p/Downloads/JOBS2"), Some(9400), "sibling falls to the parent key");
    assert_eq!(port_for(&map, "/mnt/d/q"), None);
}

#[tokio::test]
async fn map_file_rules() {
    let dir = std::env::temp_dir().join(format!("cdash-bmap-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    assert!(read_port_map(&dir).await.is_empty(), "missing file");
    std::fs::write(dir.join(MAP_FILE), "{not json").unwrap();
    assert!(read_port_map(&dir).await.is_empty(), "malformed file");
    std::fs::write(dir.join(MAP_FILE), r#"{"/a/":9301,"rel":9302,"/b":80,"/c":70000,"/d":"9303","/e":9304}"#).unwrap();
    let mut m = read_port_map(&dir).await;
    m.sort();
    assert_eq!(m, vec![("/a".to_string(), 9301), ("/e".to_string(), 9304)]);
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p cdash-agent browser::tests`
Expected: compile error, `read_port_map` / `port_for` not found.

- [ ] **Step 3: Implement `read_port_map` and `port_for`**

Read with `super::fsio::read_if`, parse as `serde_json::Map`. Keep entries whose key starts with `/` (trailing `/` trimmed, except a bare `/`) and whose value is an integer in `1024..=65535`. `port_for`: a key matches when `dir == key` or `dir.starts_with(&format!("{key}/"))` (key `/` matches everything); longest matching key wins.

- [ ] **Step 4: Run to verify pass**

Run: `cargo test -p cdash-agent browser::tests`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/agent/src/collect/browser.rs crates/agent/src/collect/mod.rs
git commit -m "feat(agent): read the browser port map"
```

---

### Task 2: `browser` flag on `/api/sessions`

**Files:**
- Modify: `crates/agent/src/collect/browser.rs`
- Modify: `crates/agent/src/collect/external.rs` (`Session` field + `browser: None` in its constructor; remove stale parity-gate comments)
- Modify: `crates/agent/src/collect/sessions.rs` (`browser: None` in every `Session` literal: Claude, Codex, OpenCode, bare terminal and the serialization test; set flags after the pane loop)

**Interfaces:**
- Consumes: `read_port_map`, `port_for` (Task 1).
- Produces: `pub async fn browser_flags(claude_dir: &Path, dirs: &[&str]) -> Vec<bool>`; `Session.browser: Option<bool>` serialized as `"browser"`, `#[serde(skip_serializing_if = "Option::is_none")]`.
- Produces: `pub async fn browser_ws_url(port: u16) -> Result<String, String>` in `browser.rs`: discover `/json/version` with a 5 s deadline and the global HTTP policy; parse JSON, validate its WebSocket URL/path, then return `ws://127.0.0.1:{port}` plus that path. Reject missing/invalid paths, queries and fragments. Error strings are fixed categories (`connect`, `timeout`, `protocol`), never library errors. Task 4 reuses this function.

- [ ] **Step 1: Write the failing test**

```rust
#[tokio::test]
async fn flags_are_true_only_for_a_mapped_live_port() {
    let live = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let live_port = live.local_addr().unwrap().port();
    let app = axum::Router::new().route("/json/version", axum::routing::get(move || async move {
        axum::Json(serde_json::json!({"webSocketDebuggerUrl": format!("ws://localhost:{live_port}/devtools/browser/fake")}))
    }));
    tokio::spawn(async move { axum::serve(live, app).await });
    let dead_port = { let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap(); l.local_addr().unwrap().port() };
    let dir = std::env::temp_dir().join(format!("cdash-bflags-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join(MAP_FILE), format!(r#"{{"/live":{live_port},"/dead":{dead_port}}}"#)).unwrap();
    assert_eq!(browser_flags(&dir, &["/live/x", "/dead", "/other"]).await, vec![true, false, false]);
}
```

Add `empty_json_is_not_a_browser`: a mapped HTTP server returning 200 `{}`
must yield `false`. Add `discovery_never_follows_redirects`: discovery returns
302 to a second loopback listener on another port; `browser_flags` is false,
`browser_ws_url` errors, and the trap listener receives zero requests. Exercise
301/302/307/308; do not make external network requests in tests.
Add `discovery_pins_the_websocket_authority`: an advertised
`ws://elsewhere.invalid:1/devtools/browser/test` is reconstructed as
`ws://127.0.0.1:<mapped>/devtools/browser/test`; invalid paths, query/fragment
and non-WebSocket schemes fail. No `Browser` product string is required.

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p cdash-agent browser::tests::flags_are_true_only_for_a_mapped_live_port`
Expected: compile error, `browser_flags` not found.

- [ ] **Step 3: Implement `browser_flags`**

Implement `browser_ws_url` first with `.no_proxy()`, `.redirect(reqwest::redirect::Policy::none())` and a 5 s deadline covering request plus body parsing. Read the map once. For each mapped dir, wrap `browser_ws_url(port)` in `tokio::time::timeout(Duration::from_millis(300), ...)`; true only on a valid discovery response. Probes run concurrently (`futures_util::future::join_all`). Unmapped dirs are `false` without a request.

- [ ] **Step 4: Add the field and wire it**

Add `browser: Option<bool>` to `Session` next to `native_terminal`, `browser: None` in all six current literals (four tmux branches, external constructor, serialization test). In `collect_sessions`, after the pane loop and before external rows are appended, call `browser_flags(&ctx.claude_dir, &dirs)` with each pane's `p.path`, the `pane_current_path` also used by `/api/browser`. Codex/OpenCode rows can display a saved `dir` instead. Each branch pushes one row per pane; zip flags with those rows and set `browser = Some(true)` only for live ports. External rows are never probed. Add an integration assertion with a provider row whose saved dir differs from its pane path; button visibility and route lookup must use the same pane mapping. Verify `browser: None` is omitted by the existing serialization test.

- [ ] **Step 5: Run the agent suite**

Run: `cargo test -p cdash-agent && cargo clippy -p cdash-agent --all-targets -- -D warnings`
Expected: PASS, no warnings.

- [ ] **Step 6: Commit**

```bash
git add crates/agent/src/collect/browser.rs crates/agent/src/collect/external.rs crates/agent/src/collect/sessions.rs
git commit -m "feat(agent): flag sessions with a live browser CDP port"
```

---

### Task 3: Phone protocol and key translation

**Files:**
- Modify: `crates/agent/src/collect/browser.rs`

**Interfaces:**
- Produces:
  - `pub const MAX_TEXT: usize = 4096;`
  - `pub enum PhoneMsg { Tap(f64, f64), Scroll { x: f64, y: f64, dx: f64, dy: f64 }, Text(String), Key(&'static str), Tab(String), Back, Reload }` (derive `Debug, PartialEq` for assertions; never format input-bearing values in production diagnostics)
  - `pub fn parse_phone(text: &str) -> Option<PhoneMsg>`: the JSON shapes in the spec's message table (`{"tap":[x,y]}`, `{"scroll":[x,y,dx,dy]}`, `{"text":"…"}`, `{"key":"Enter"}`, `{"tab":"id"}`, `{"nav":"back"|"reload"}`). `None` for unknown shapes, non-finite numbers, empty or over-`MAX_TEXT` text, unknown key or nav names, an empty tab id.
  - `pub fn key_events(key: &str) -> Option<Vec<serde_json::Value>>`: `Input.dispatchKeyEvent` params in order.
  - `pub fn char_events(c: char) -> [serde_json::Value; 2]`
  - `pub struct Viewport { pub width: f64, pub height: f64 }` (positive finite dimensions only) and `pub fn clamp_point(x: f64, y: f64, vp: &Viewport) -> (f64, f64)`

- [ ] **Step 1: Write the failing tests**

```rust
#[test]
fn phone_messages_are_validated() {
    assert_eq!(parse_phone(r#"{"tap":[10.5,20]}"#), Some(PhoneMsg::Tap(10.5, 20.0)));
    assert_eq!(parse_phone(r#"{"key":"Backspace"}"#), Some(PhoneMsg::Key("Backspace")));
    assert_eq!(parse_phone(r#"{"nav":"back"}"#), Some(PhoneMsg::Back));
    for bad in [r#"{"tap":[1]}"#, r#"{"tap":["a",1]}"#, r#"{"key":"F5"}"#, r#"{"nav":"forward"}"#,
                r#"{"text":""}"#, r#"{"tab":""}"#, r#"{"eval":"x"}"#, "nope"] {
        assert_eq!(parse_phone(bad), None, "{bad}");
    }
    let long = format!(r#"{{"text":"{}"}}"#, "a".repeat(MAX_TEXT + 1));
    assert_eq!(parse_phone(&long), None);
}

#[test]
fn keys_carry_virtual_key_codes() {
    let enter = key_events("Enter").unwrap();
    assert_eq!(enter.len(), 3);
    assert_eq!(enter[0]["type"], "rawKeyDown");
    assert_eq!(enter[0]["windowsVirtualKeyCode"], 13);
    assert_eq!(enter[1]["type"], "char");
    assert_eq!(enter[1]["text"], "\r");
    assert_eq!(enter[2]["type"], "keyUp");
    for (k, vk) in [("Tab", 9), ("Backspace", 8), ("Escape", 27), ("ArrowLeft", 37), ("ArrowUp", 38), ("ArrowRight", 39), ("ArrowDown", 40)] {
        let ev = key_events(k).unwrap();
        assert_eq!(ev.len(), 2, "{k}");
        assert_eq!(ev[0]["type"], "rawKeyDown");
        assert_eq!(ev[0]["windowsVirtualKeyCode"], vk, "{k}");
        assert_eq!(ev[1]["type"], "keyUp");
    }
    assert!(key_events("F5").is_none());
}

#[test]
fn chars_become_one_key_press_each() {
    let [down, up] = char_events('a');
    assert_eq!((down["type"].as_str(), down["text"].as_str(), down["windowsVirtualKeyCode"].as_u64()), (Some("keyDown"), Some("a"), Some(65)));
    assert_eq!((up["type"].as_str(), up["windowsVirtualKeyCode"].as_u64()), (Some("keyUp"), Some(65)));
    assert_eq!(char_events('7')[0]["windowsVirtualKeyCode"], 55);
    for c in ['$', 'é', '😀', ' '] {
        let [down, _] = char_events(c);
        assert_eq!(down["text"], c.to_string());
        assert!(down.get("windowsVirtualKeyCode").is_none(), "{c:?} has no VK");
    }
}

#[test]
fn clamp_keeps_points_inside_the_viewport() {
    let vp = Viewport { width: 1000.0, height: 600.0 };
    assert_eq!(clamp_point(-5.0, 700.0, &vp), (0.0, 600.0));
    assert_eq!(clamp_point(10.0, 20.0, &vp), (10.0, 20.0));
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p cdash-agent browser::tests`
Expected: compile errors for the new names.

- [ ] **Step 3: Implement**

`key_events`: `key` and `code` both set to the key name. `char_events`: `key` and `text` set to the character; `windowsVirtualKeyCode` = `c.to_ascii_uppercase() as u32` only for ASCII letters and digits. Space is deliberately left without a VK (spike 3 typed `Pa$$ w0rd!é` this way).

- [ ] **Step 4: Run to verify pass**

Run: `cargo test -p cdash-agent browser::tests`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/agent/src/collect/browser.rs
git commit -m "feat(agent): parse browser view messages and build key events"
```

---

### Task 4: Minimal CDP client and fake Chromium browser

**Files:**
- Create: `crates/agent/src/collect/cdp.rs` (+ `pub mod cdp;` in `collect/mod.rs`)

**Interfaces:**
- Produces:
  - `pub struct Event { pub method: String, pub params: serde_json::Value, pub session: Option<String> }`
  - `pub struct Cdp` with `pub async fn connect(ws_url: &str) -> Result<(Cdp, tokio::sync::mpsc::UnboundedReceiver<Event>), String>` and `pub async fn call(&self, method: &str, params: serde_json::Value, session: Option<&str>) -> Result<serde_json::Value, String>` (5 s timeout per call; CDP `error.code == -32601` becomes `Err("unsupported")`, other CDP errors `Err("protocol")`, deadline expiry `Err("timeout")`; never retain or propagate `error.message`). Dropping `Cdp` closes the socket and ends reader/writer tasks. The event receiver yields `None` once the socket closes; pending calls then fail.
  - Consumes `browser::browser_ws_url` (Task 2); do not duplicate discovery/URL validation in `cdp.rs`.
  - `pub async fn page_order(port: u16) -> Vec<String>`: ids of `type == "page"` entries from `/json/list`, in returned order, not assumed activity order; empty on any error. Same no-proxy/no-redirect policy, 5 s deadline including body parsing.
  - Test-only, `#[cfg(test)] pub(crate) mod fake`: `pub(crate) async fn fake_edge(pages: &[(&str, &str, &str)]) -> FakeEdge` (id, url, title) where `FakeEdge { pub port: u16, pub calls: mpsc::UnboundedReceiver<(String, Value, Option<String>)>, pub events: mpsc::UnboundedSender<Value>, pub drop_socket: Arc<tokio::sync::Notify>, pub replies: Arc<std::sync::Mutex<std::collections::HashMap<String, Option<Value>>>> }`. Serves `/json/version`, `/json/list` and one CDP WebSocket at `/devtools/browser/fake`. Replies: `Target.getTargets` lists `pages` as `targetInfos` (`targetId`, `type: "page"`, `url`, `title`); `/json/list` returns them as `[{id, type: "page", url, title}]`; `Target.attachToTarget` returns `{"sessionId":"S-<targetId>"}`; `Page.getNavigationHistory` returns `{"currentIndex":1,"entries":[{"id":10},{"id":11}]}`; anything else `{}`. `replies` overrides a method with `Some({"result":...})` or `Some({"error":...})`, adding the incoming id; `None` withholds its reply. Every received call is pushed to `calls`. A value sent on `events` is written to the socket as-is. `drop_socket.notify_one()` closes the CDP socket.

- [ ] **Step 1: Write the failing test**

```rust
#[tokio::test]
async fn calls_events_and_close() {
    let mut edge = fake::fake_edge(&[("P1", "https://a.example/", "A")]).await;
    let url = super::super::browser::browser_ws_url(edge.port).await.unwrap();
    let (cdp, mut ev) = Cdp::connect(&url).await.unwrap();
    let r = cdp.call("Target.attachToTarget", serde_json::json!({"targetId":"P1","flatten":true}), None).await.unwrap();
    assert_eq!(r["sessionId"], "S-P1");
    let (m, p, s) = edge.calls.recv().await.unwrap();
    assert_eq!((m.as_str(), p["targetId"].as_str(), s), ("Target.attachToTarget", Some("P1"), None));
    edge.events.send(serde_json::json!({"method":"Page.screencastFrame","params":{"sessionId":7},"sessionId":"S-P1"})).unwrap();
    let e = ev.recv().await.unwrap();
    assert_eq!((e.method.as_str(), e.session.as_deref()), ("Page.screencastFrame", Some("S-P1")));
    assert_eq!(page_order(edge.port).await, vec!["P1".to_string()]);
    edge.drop_socket.notify_one();
    assert!(ev.recv().await.is_none(), "event stream ends when Edge goes away");
    assert!(cdp.call("Page.reload", serde_json::json!({}), Some("S-P1")).await.is_err());
}
```

The fake's existing name `fake_edge` is only a fixture name; its discovery
responses and assertions must not require `Edg/` or an Edge executable.
Add `list_discovery_never_follows_redirects`: redirect `/json/list` to a trap
listener, assert empty ordering and zero trap requests. Reuse Task 2's
discovery policy rather than allow redirects here.
Add `cdp_errors_are_fixed_categories`: fake errors containing URL/title/text
canaries produce only `unsupported` or `protocol`; a hung call produces
`timeout`. Add `dropping_cdp_closes_socket_and_tasks`: drop the client while
the fake remains running; assert peer close and event-stream termination
within a bounded deadline. Separately close the fake while a call is pending
and assert that call fails; do not add production test-only accessors.

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p cdash-agent cdp::tests`
Expected: compile error.

- [ ] **Step 3: Implement `Cdp` and the fake**

One reader task routes replies by `id` to `oneshot` senders in a `Mutex<HashMap>` and forwards messages that have `method` to the event channel. One writer task drains an `mpsc` of outgoing frames. Connect with `tokio_tungstenite::connect_async(ws_url)` (no Origin header) inside a 5 s `tokio::time::timeout`, so a hung browser closes `4002` instead of leaving the overlay on "Connecting…". Keep task handles/cancellation so dropping the owner cannot leave half a split socket alive. Remove pending calls on reply, timeout and disconnect; discard raw library/CDP error strings. HTTP uses the global no-proxy/no-redirect policy.

- [ ] **Step 4: Run to verify pass**

Run: `cargo test -p cdash-agent cdp::tests && cargo clippy -p cdash-agent --all-targets -- -D warnings`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/agent/src/collect/cdp.rs crates/agent/src/collect/mod.rs
git commit -m "feat(agent): add a minimal CDP client"
```

---

### Task 5: `GET /api/browser`, screencast relay and close codes

**Files:**
- Create: `crates/agent/src/collect/browser_bridge.rs` (+ `pub mod browser_bridge;`)
- Modify: `crates/agent/src/http/routes.rs` (add `get_browser`), `crates/agent/src/http/serve.rs` (`get "/api/browser" => routes::get_browser,` in `guarded_routes!`)
- Modify: `crates/agent/src/collect/term.rs`: make `PING_EVERY` and `send_or_drop` `pub(crate)`; reuse these existing pieces without exporting raw-error handling. `mod tests` becomes `pub(crate) mod tests` with `Session`, `have_tmux`, `tempdir`, `status_of`, `close_code` `pub(crate)`, plus `Session::in_dir(tag: &str, dir: &Path) -> Session` (`tmux new-session -d -s <name> -c <dir> ... sh`). Fixed browser close reasons already fit the 123-byte limit; no need to export `close_reason`.

**Interfaces:**
- Consumes: `read_port_map`, `port_for` (Task 1); `browser::browser_ws_url` (Task 2); `Cdp`, `page_order`, `fake::fake_edge` (Task 4); `origin_allowed`, `assert_kill_name`.
- Produces: `pub async fn bridge(ctx: Arc<Ctx>, name: String, port: u16, ws: WebSocket)`. Phone-bound text frames `{"frame":{"deviceWidth":f64,"deviceHeight":f64,"offsetTop":f64,"pageScaleFactor":f64}}` copied from CDP metadata immediately before each binary JPEG frame, and `{"tabs":[{"id":"...","title":"...","origin":"https://example.com"}],"active":"<id>"}` (either `origin` or `active` can be JSON `null`).

Handler order (all before upgrade): `origin_allowed` (403), `assert_kill_name` (400), session dir from `tmux list-panes -a -F PANE_FORMAT` (404 `no such session`), `port_for` (404 `no browser for this session`). Each refusal logs its fixed category (`origin`, `name`, `session`, `mapping`); never copy the Terminal route's raw rejected-Origin logging.

Bridge: log `browser open <name>`; discovery/connect failure closes `4002 Browser not running`. Discover targets before taking the initial list so tab changes cannot fall between snapshot and subscription. Initial tab: first `page_order(port)` id listed as a page by `Target.getTargets`, else the first page; with none, send `active:null` and wait. Attach (`flatten:true`), `Page.enable`, then `Page.startScreencast {format:"jpeg",quality:60,maxWidth:1280}`. Send the initial tab list before processing queued frames. On `Page.screencastFrame` for the viewed session: validate metadata, send it as text, then binary (base64-decoded `data`), then `Page.screencastFrameAck {sessionId: params.sessionId}`. Ping like `term.rs`. Missing required CDP methods close `4004 Browser view unsupported`; other failures use fixed category logging and `1011 Browser connection failed`. Event channel ending closes `4003 Browser closed`. Phone close: bounded best-effort `Page.stopScreencast` and `Target.detachFromTarget`, then drop `Cdp` on every exit path, even when a cleanup call fails. Log `browser close <name>`. `origin` is the parsed URL's tuple-origin ASCII serialization, or JSON `null` for opaque/invalid origins. Never fall back to a raw URL.

- [ ] **Step 1: Write the failing tests** (in `browser_bridge.rs`; tests skip when `have_tmux()` is false)

- `refusals_happen_before_upgrade`: origin `https://evil.example` gives 403; `name=other` 400; `name=cdash-missing` 404; a tmux session in an unmapped dir 404.
- `frame_is_relayed_then_acked`: fake browser with page `P1`; map the session dir to `edge.port`; connect; push `{"method":"Page.screencastFrame","sessionId":"S-P1","params":{"data":"SlBFRw==","sessionId":7,"metadata":{"deviceWidth":1000,"deviceHeight":600,"offsetTop":0,"pageScaleFactor":1}}}`. Assert the phone receives all four metadata fields, then binary `b"JPEG"`, and `calls` then contains `Page.screencastFrameAck` with `sessionId == 7`. Assert the first phone text frame is a tab list with `"active":"P1"` and `origin` `https://a.example`.
- `browser_not_running_closes_4002`: map to a closed port; `close_code` is 4002.
- `browser_going_away_closes_4003`: `drop_socket.notify_one()`; `close_code` is 4003.
- `log_never_contains_urls`: after open and close, the log has `browser open cdash-…` and `browser close cdash-…`, and no line contains `a.example`.
- `errors_never_expose_canaries`: put URL/title/text canaries in discovery/CDP failures and rejected Origin/name/message input; logs and close frames contain none of them. Assert fixed categories and exact fixed close reasons, including failures during input and cleanup.
- `missing_cdp_method_closes_4004`: fake `Page.startScreencast` returns code `-32601` with canary text; close reason is exactly `Browser view unsupported`.
- `opaque_origins_are_null`: data/file/invalid URLs give JSON null; an HTTPS URL with userinfo/path/query/fragment gives only its origin, with all canaries absent from `origin`.
- `phone_close_detaches_without_closing_browser`: disconnect the phone while fake CDP stays open; assert stop/detach, peer close and no surviving connection, including failed cleanup calls. No `Browser.close`/target activation/window/viewport calls.
- `no_pages_waits_for_user_selection`: initial empty list gives `active:null`; a later created page appears in the picker but does not become active until `tab` arrives.

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p cdash-agent browser_bridge::tests`
Expected: compile error / 404 from the router.

- [ ] **Step 3: Implement the handler and bridge**

- [ ] **Step 4: Run to verify pass**

Run: `cargo test -p cdash-agent && cargo clippy -p cdash-agent --all-targets -- -D warnings`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/agent/src
git commit -m "feat(agent): stream a session's Chromium tab over /api/browser"
```

---

### Task 6: Input, tab switching and navigation

**Files:**
- Modify: `crates/agent/src/collect/browser_bridge.rs`

**Interfaces:**
- Consumes: `parse_phone`, `key_events`, `char_events`, `clamp_point`, `Viewport` (Task 3); bridge state from Task 5.

Behaviour (spec section 4): the bridge keeps the last valid frame's `Viewport { width: deviceWidth, height: deviceHeight }`. `Tap`: clamp, then `Input.dispatchMouseEvent` `mouseMoved`, `mousePressed`, `mouseReleased` (`button:"left"`, `clickCount:1`). `Scroll`: clamp `x,y`, `mouseWheel` with `deltaX:dx, deltaY:dy`. `Text`: `char_events` for each `char`, in order. `Key`: `key_events`. All input is dropped when there is no viewport yet or `active` is null. `Tab(id)`: ignored unless it is a listed page; clear viewport before switching, stop/detach the old session if present, attach + `Page.enable` + start the new one, send the tab list. Ignore frames from any old CDP session. `Back`: `Page.getNavigationHistory`, then `Page.navigateToHistoryEntry {entryId}` with `entries[currentIndex-1].id`; nothing at index 0. `Reload`: `Page.reload`. Navigation is ignored when `active` is null. `Target.targetCreated` / `targetInfoChanged` for pages: update the stored list and resend it, active unchanged. `Target.targetDestroyed` of the viewed tab: active becomes null, viewport cleared, list resent; do not call a destroyed session on the next selection. Use the fixed failure categories/close reasons on every CDP error, including input.

- [ ] **Step 1: Write the failing tests**

- `input_before_first_frame_is_dropped`: send `{"tap":[5,5]}` before any frame; then a frame (deviceWidth 1000, deviceHeight 600); then `{"tap":[2000,-1]}`. The only `Input.dispatchMouseEvent` calls are three with `x == 1000.0, y == 0.0`.
- `text_becomes_key_presses`: after a frame, `{"text":"a$"}` yields `Input.dispatchKeyEvent` calls in order: keyDown `a` VK 65, keyUp, keyDown `$` without VK, keyUp. No `Input.insertText` call ever appears.
- `backspace_has_vk_8`: `{"key":"Backspace"}` yields rawKeyDown and keyUp with `windowsVirtualKeyCode == 8`.
- `tab_switch_and_close`: pages `P1`, `P2`. `{"tab":"nope"}` makes no calls. `{"tab":"P2"}` makes `Target.attachToTarget` with `targetId == "P2"`, and the next tab list has `"active":"P2"`. A `Target.targetCreated` for `P3` resends the list with `"active":"P2"`. A `Target.targetDestroyed` for `P2` sends `"active":null`, and a following `{"text":"x"}` makes no call.
- `back_uses_the_previous_entry_id`: `{"nav":"back"}` calls `Page.navigateToHistoryEntry` with `entryId == 10`.
- `old_frames_cannot_enable_input_after_tab_switch`: after switching P1 → P2, a queued P1 frame and input make no calls on P1 or P2; only a valid P2 frame establishes the new viewport. Zero/non-finite dimensions never enable input.
- `closed_tab_can_be_replaced`: close P2, select P1; attach without trying to stop/detach the destroyed P2 session. Back/Reload with no active tab make no calls.

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p cdash-agent browser_bridge::tests`
Expected: the five new tests FAIL.

- [ ] **Step 3: Implement**

- [ ] **Step 4: Run to verify pass**

Run: `cargo test -p cdash-agent && cargo clippy -p cdash-agent --all-targets -- -D warnings`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/agent/src/collect/browser_bridge.rs
git commit -m "feat(agent): forward browser input, tabs and navigation"
```

---

### Task 7: DOM-free view helpers

**Files:**
- Create: `public/browser/view.js` (IIFE on `globalThis`, same shape as `public/term/keys.js`)
- Modify: `crates/agent/src/http/serve.rs` (embed `"browser/view.js"`), `crates/agent/src/http/routes.rs` (`terminal_assets_are_embedded` gains `("/browser/view.js", "cdashBrowser")`), `public/index.html` (`<script src="browser/view.js"></script>` after `term/keys.js`)
- Test: `test/browser-view.test.mjs`

**Interfaces:**
- Produces `globalThis.cdashBrowser`:
  - `KEYBAR`: `{ tab: 'Tab', bksp: 'Backspace', left: 'ArrowLeft', right: 'ArrowRight', esc: 'Escape', enter: 'Enter' }`
  - `toPage(x, y, shown, meta) -> [number, number]`: `x`,`y` relative to the transformed image's top-left; `shown` `{ w, h }` its actual displayed size; `meta` the metadata paired with the displayed JPEG. Returns `[x / shown.w * meta.deviceWidth, y / shown.h * meta.deviceHeight]` (no extra `offsetTop` or division by `pageScaleFactor`; see Browser API notes). Caller drops input until image/dimensions are valid.
  - `fromInput(inputType, data) -> object[]`: `insertText`, `insertFromPaste`, `insertReplacementText` with non-empty `data` give `[{ text: data }]`; `deleteContentBackward` gives `[{ key: 'Backspace' }]`; `insertLineBreak` gives `[{ key: 'Enter' }]`; anything else `[]`.

- [ ] **Step 1: Write the failing test**

```js
import { test } from 'node:test';
import assert from 'node:assert/strict';
import '../public/browser/view.js';
const B = globalThis.cdashBrowser;

test('taps map from displayed image pixels to page CSS pixels', () => {
  const meta = { deviceWidth: 985, deviceHeight: 640, offsetTop: 0, pageScaleFactor: 1 };
  assert.deepEqual(B.toPage(0, 0, { w: 394, h: 256 }, meta), [0, 0]);
  assert.deepEqual(B.toPage(197, 128, { w: 394, h: 256 }, meta), [492.5, 320]);
  assert.deepEqual(B.toPage(394, 256, { w: 788, h: 512 }, meta), [492.5, 320], '2x image zoom');
  assert.deepEqual(B.toPage(197, 128, { w: 394, h: 256 }, { ...meta, offsetTop: 20, pageScaleFactor: 1.5 }), [492.5, 320], 'coordinates already start at the image');
});

test('keyboard input becomes text or keys', () => {
  assert.deepEqual(B.fromInput('insertText', 'é'), [{ text: 'é' }]);
  assert.deepEqual(B.fromInput('insertFromPaste', '123456'), [{ text: '123456' }]);
  assert.deepEqual(B.fromInput('deleteContentBackward', null), [{ key: 'Backspace' }]);
  assert.deepEqual(B.fromInput('insertLineBreak', null), [{ key: 'Enter' }]);
  assert.deepEqual(B.fromInput('insertText', ''), []);
  assert.deepEqual(B.fromInput('formatBold', null), []);
  assert.deepEqual(B.fromInput('insertCompositionText', 'に'), [], 'partial composition is not committed input');
  assert.deepEqual(Object.values(B.KEYBAR).sort(), ['ArrowLeft', 'ArrowRight', 'Backspace', 'Enter', 'Escape', 'Tab']);
});
```

- [ ] **Step 2: Run to verify failure**

Run: `node --test test/browser-view.test.mjs`
Expected: FAIL, module not found.

- [ ] **Step 3: Implement `view.js` and embed it**

- [ ] **Step 4: Run to verify pass**

Run: `npm test && cargo test -p cdash-agent terminal_assets_are_embedded`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add public/browser/view.js public/index.html test/browser-view.test.mjs crates/agent/src/http
git commit -m "feat(ui): add browser view input helpers"
```

---

### Task 8: Browser overlay and card button

**Files:**
- Modify: `public/index.html` (`#browser` overlay), `public/app.js`, `public/style.css`, `test/frontend-guards.test.mjs`

**Interfaces:**
- Consumes: `cdashBrowser` (Task 7); `/api/browser` protocol (Tasks 5, 6); `browser` row field (Task 2).

Markup: `<div id="browser" class="term browser" role="dialog" aria-modal="true" aria-labelledby="browser-title" hidden>` reusing `.term-bar`, `.term-status`, `.term-reconnect`, `.term-keys`. Bar: `#browser-title`, `#browser-status`, `#browser-reconnect`, `#browser-back`, `#browser-reload`, `#browser-close`; icon-only controls have accessible labels. Second row: `<select id="browser-tabs" aria-label="Browser tab">` whose option labels are tab origins (`No web origin` for JSON null, page title in the DOM `title` property), plus a disabled placeholder `Pick a tab` selected when `active` is null. Stage: `#browser-stage` holding `<img id="browser-screen" alt="Remote browser view">`. Visually concealed but focusable `<input id="browser-input" type="password" aria-label="Remote browser typing" autocomplete="off" autocapitalize="off" spellcheck="false">`; do not use `hidden`, `display:none` or `visibility:hidden` on that input. Key bar: `Kbd` (`data-bkey="kbd"`) then the `KEYBAR` keys in the spec's order; `.browser .term-keys` uses 7 columns.

Behaviour in `app.js`:
- `termUrl` becomes `wsUrl(q)` (same Tauri and `wss` rules) used by both overlays; update the existing Terminal helper tests when renaming it.
- `runningCard`: `browserBtn` after `termBtn`, only when `r.browser`: `<button class="action" type="button" data-browser="${esc(r.name)}" data-title="${esc(r.dir.split('/').pop())}">${ICONS.globe}<span>Browser</span></button>`. Add a `globe` icon to `ICONS`.
- State tracks `name`, `ws`, displayed/pending frame metadata, active target, object URL and retry. `openBrowser(name, title)` / `closeBrowser()` mirror `openTerm` / `closeTerm` (history push, popstate close, one retry on visibility, `tick()` paused while open: `if (term || browserView) return;`). Opening either overlay closes the other with existing history semantics; one socket/overlay at a time, restore focus on close.
- Frames: text `{frame}` is pending metadata; pair it with the next binary JPEG and promote it to displayed metadata only after image load. Revoke replaced object URLs. Clear image/metadata/input on tab change or reconnect; guard asynchronous image-load callbacks by socket/target/frame identity so stale decodes cannot restore input. Disable all input until a valid frame pair is displayed. `{tabs, active}` rebuilds options via DOM text/properties; `active === null` selects the placeholder and sets status `Tab closed. Pick a tab.` (no `showPicker()`).
- Close codes: 4002 shows `Browser not running`, 4003 `Browser closed`, with Reconnect; 4004 shows `Browser view unsupported` without automatic retry; 1011 shows `Browser connection failed`. Never display a raw server error. Close/disconnect clears input/composition, pending frame state and object URLs.
- Gestures on `#browser-stage` (pointer events, `touch-action: none`): a short tap with little movement sends `{tap: toPage(...)}`; two pointers pinch image scale (1 to 5) via CSS transform; one-pointer drag pans when scale > 1 and sends `{scroll:[x,y,dx,dy]}` at scale 1 (page CSS px; dragging up scrolls down). Compute coordinates from the transformed image's `getBoundingClientRect()`, excluding stage padding/pan offsets and letterboxing; drop pointers outside the image. Cancel pointer defaults on the image/key-bar only so keyboard focus stays open; do not cancel Kbd focus or native tab-picker behavior.
- Typing: `Kbd` focuses `#browser-input`. Keep a sentinel for empty-field Backspace. During `compositionstart` through `compositionend`, neither send partial `insertCompositionText` nor reset the field. At commit, send the final text once and reset; ignore a duplicate trailing commit input. Canceled composition sends nothing. Ordinary committed input uses `cdashBrowser.fromInput(e.inputType, e.data)`; key-bar buttons send `{key: KEYBAR[k]}`. Handle Enter on `keydown` once, prevent its default form action, and suppress duplicate input; reset all transient state on close/disconnect. Do not buffer/replay text entered without an active displayed frame.
- Back and Reload buttons send `{nav:"back"}` / `{nav:"reload"}`. Tab select change sends `{tab: value}`.

- [ ] **Step 1: Write the failing test** (append to `test/frontend-guards.test.mjs`)

```js
test('only sessions with a live browser get the Browser button', () => {
  const src = readFileSync(new URL('../public/app.js', import.meta.url), 'utf8');
  const card = src.slice(src.indexOf('function runningCard('), src.indexOf('\n}\n', src.indexOf('function runningCard(')) + 2);
  const context = { esc: s => s, effortLabel: s => s, gitBadge: () => '', copyIdBtn: () => '', fmtUp: () => '', armedKill: null, ICONS: new Proxy({}, { get: () => '' }) };
  runInNewContext(`${card}; this.runningCard = runningCard;`, context);
  const row = { name: 'cdash-a', dir: '/x/JOBS', working: false };
  assert.match(context.runningCard({ ...row, browser: true }), /data-browser="cdash-a"/);
  assert.doesNotMatch(context.runningCard(row), /data-browser/);
});
```

Add event-wiring tests using the existing frontend harness:

- `composition_is_forwarded_once_on_commit`: `compositionstart`, composing
  `input('insertCompositionText', 'に')`, `compositionend('日本')`, trailing
  commit input → exactly `[{text:'日本'}]`; partial updates send nothing and
  keep the composing value. Cancellation sends nothing; close/disconnect
  clears composition and the input. Keyboard Enter sends exactly one key.
- `tab_labels_are_text`: a title `<img src=x onerror=URL_CANARY>` stays literal
  text/title data; no element/event handler is created. `origin:null` becomes
  `No web origin`; `active:null` becomes `Pick a tab` with input disabled.
- `frame_pairing_survives_switch_and_reconnect`: delay frame A's image load,
  switch target/socket, then finish A; input remains disabled. Load frame B
  with different dimensions; taps use B's metadata and transformed image rect,
  never stage bounds or A. Replaced/closed URLs are revoked.
- `browser_and_terminal_lifecycles_do_not_conflict`: open each overlay in
  turn, use Back, reconnect and visibility retry; exactly one live socket,
  correct history/focus, unchanged Terminal URL behavior including Tauri/wss.

- [ ] **Step 2: Run to verify failure**

Run: `node --test test/frontend-guards.test.mjs`
Expected: the new test FAILS (no `data-browser`).

- [ ] **Step 3: Implement markup, styles and `app.js` wiring**

- [ ] **Step 4: Run the suites**

Run: `npm test && cargo test -p cdash-agent`
Expected: PASS.

- [ ] **Step 5: Smoke test against scratch Chromium profiles**

Use the chosen Chromium executable and a new, dedicated scratch profile,
not its default personal profile. Example on a native Linux host:

```bash
scratch_profile="$(mktemp -d -t cdash-bv.XXXXXX)"
"/path/to/chosen/chromium-browser" --user-data-dir="$scratch_profile" --remote-debugging-port=9399 https://example.com
```

On Windows, launch the chosen executable with the same flags and a unique
Windows scratch-profile path (from WSL, use `cmd.exe` as the existing wrapper
does). Check the listener is loopback-only. Temporarily merge
`{"<a tmux session's dir>":9399}` into the map after backing it up; preserve
other entries. Run `cargo run -p cdash-agent`, open
`http://127.0.0.1:23274` in a phone browser/mobile emulation. Check the Browser
button, example.com image, link tap, Back and origin. Check tab switch/close,
background streaming, desktop zoom/DPI, image pinch/pan and reconnect.
Use a local scratch test page for six-box MFA and committed/canceled IME
input; no real credentials. Close only the scratch browser: the overlay must
show `Browser closed`. Restore the map and remove only the scratch profile.
Repeat with Chrome/Chromium and another Chromium-based browser (Brave,
Vivaldi, Opera or Edge); record executable/version, OS and passed/pending
cases. Edge installation is never required. Locked-PC/real-phone checks
remain Task 10; do not claim them from mobile emulation.

- [ ] **Step 6: Commit**

```bash
git add public test/frontend-guards.test.mjs
git commit -m "feat(ui): add the Browser overlay"
```

---

### Task 9: README

**Files:**
- Modify: `README.md` (next to the current Terminal usage instructions; do not rely on old line numbers)

Cover, in the README's voice: remote login/MFA in a session's Playwright-controlled Chromium browser; any Chromium executable/channel with the required CDP capabilities, Edge optional; the `cdash-browsers.json` format with JOBS/INVEST examples; Playwright MCP `--config`, `browserName: "chromium"`, chosen channel/`--executable-path`, dedicated matching `--user-data-dir` and `launchOptions.args` `--remote-debugging-port`; Chrome 136+'s non-default-profile requirement; loopback-only listener, no redirects/proxies, local-process access risk; mirrored networking only for Windows-browser/WSL-agent setups; browser-owned popups/password/Windows Hello prompts are invisible, so type passwords. Logs contain open/close and fixed failure categories only, never errors, URLs or input. Describe `Browser view unsupported` honestly; do not present unrun browser/device checks as passed.

- [ ] **Step 1: Write the paragraph**
- [ ] **Step 2: Commit**

```bash
git add README.md
git commit -m "docs: describe the Browser view"
```

---

### Task 10: Launchers, port map and end-to-end check (user's machine, outside the repo)

**Files (back up each to `<file>.bak-YYYYMMDD-browser-view` first; these folders are not git repos):**
- Create or merge: `D:\p\Downloads\JOBS\tools\playwright-mcp.json` with `{ "browser": { "browserName": "chromium", "launchOptions": { "args": ["--remote-debugging-port=9301"] } } }`; preserve existing options/args and browser channel/executable/profile
- Modify: `D:\p\Downloads\JOBS\tools\playwright-jobs.cmd`: append `--config=D:/p/Downloads/JOBS/tools/playwright-mcp.json` before `%*`
- Modify: `D:\p\Downloads\JOBS\protocol\tools.md` line 36 setup command to match the `.cmd`
- Create or merge: `D:\p\Downloads\INVEST\tools\playwright-mcp.json` (port `9302`) and `D:\p\Downloads\INVEST\tools\playwright-invest.cmd` (same wrapper shape as JOBS, preserving INVEST's chosen browser, dedicated matching profile and config paths; CRLF line endings). Do not copy an Edge executable or a different browser's profile by assumption
- Modify: `D:\p\Downloads\INVEST\.mcp.json` and `D:\p\Downloads\INVEST\.codex\config.toml`: `playwright-invest` becomes `command = "cmd.exe"`, `args = ["/d", "/c", "D:\\p\\Downloads\\INVEST\\tools\\playwright-invest.cmd"]`
- Modify: `D:\p\Downloads\INVEST\protocol\tools.md` setup command (line 39) to name the wrapper
- Create or merge: `~/.claude/cdash-browsers.json` with `{"/mnt/d/p/Downloads/JOBS": 9301, "/mnt/d/p/Downloads/INVEST": 9302}` (use the active agent's `CLAUDE_DIR` if it differs); preserve other mappings

- [ ] **Step 1: Confirm no JOBS or INVEST session is running** (the profile lock would block the check), inspect their current launcher/protocol files, confirm the INVEST amendment and chosen executable/profile, then make the backed-up edits above. Preserve the profile gate. Do not switch browsers or migrate profiles as a side effect of enabling a port.
- [ ] **Step 2: Verify each launcher adds its port.** Drive the `.cmd` with a minimal MCP client (initialize, then `browser_navigate` to `about:blank`).
  - JOBS: from WSL, `curl --max-redirs 0 -s 127.0.0.1:9301/json/version`. Expected: valid JSON with a browser `webSocketDebuggerUrl`; do not require `Edg/` or any product name.
  - INVEST: **do not connect to 9302.** The INVEST amendment forbids agent connections. Inspect the configured browser process's command-line flags/profile and listening socket through the OS, independent of executable name. Check for `--remote-debugging-port=9302` and the configured INVEST `--user-data-dir`; report booleans/process identity, not whole command lines that may contain URLs. That 9302 answers is proven by cdash-agent's probe and the user's phone check (Step 4).
  - Both ports: inspect listening addresses (`Get-NetTCPConnection -State Listen -LocalPort 9301,9302` on Windows, `ss -ltnp` for same-host Linux browsers). Only `127.0.0.1`/`::1` are allowed; wildcard/LAN binding fails this step. This is OS inspection, not an AI CDP connection to INVEST.
  - Close each browser with `browser_close`.
- [ ] **Step 3: Install the build** using the README's current build/install workflow. Identify the active agent/service executable and `CLAUDE_DIR` from its actual launch configuration; do not rely on unpublished memory names. Restart the intended agent.
- [ ] **Step 4: Hand the user the phone check** from the spec's Testing section: a dashboard-launched JOBS session reaches a login page; on the phone with the PC locked, open Browser, check origin, type/submit login and MFA, close overlay, confirm the agent continues. Repeat once for INVEST through the dashboard only. Include background tabs, reconnect and the Task 8 Chromium browser matrix; record actual versions and pending cases.
- [ ] **Step 5: Record actual status** in the spec and commit. Feature implementation and phone/browser verification are separate facts; pending device checks remain explicitly pending. Do not mark the login/MFA success criterion passed without the user's real-phone result.
