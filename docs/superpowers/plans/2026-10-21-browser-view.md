# Browser View Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A **Browser** button on dashboard session cards that streams the session's Playwright-driven Edge tab to the phone and forwards taps and typing, so the user can log in remotely.

**Architecture:** cdash-agent reads a hand-edited folder-to-port map, probes Edge's loopback CDP port for the session's folder, and exposes `GET /api/browser` (WebSocket). The bridge speaks CDP to Edge (`Page.startScreencast`, `Input.*`, `Target.*`) and a narrow JSON protocol to the phone. The phone overlay reuses the Terminal overlay's shell. Launcher changes in the user's JOBS and INVEST folders open the port.

**Tech Stack:** Rust (axum 0.8 `ws`, tokio, tokio-tungstenite 0.29, reqwest 0.13, serde_json), vanilla JS, `node --test`.

**Spec:** `docs/superpowers/specs/2026-10-21-browser-view-design.md`. Read it first; this plan argues from it.

## Global Constraints

- No new crates or npm packages. CDP client uses `tokio-tungstenite`; HTTP probes use `reqwest`; the test fake uses `axum` with `ws`.
- Port map file: `$CLAUDE_DIR/cdash-browsers.json`, JSON object of absolute WSL dir to port. Ports outside `1024..=65535` ignored.
- cdash-agent only connects to `127.0.0.1:<port>` from the map. Never another host.
- Never send an `Origin` header on the CDP WebSocket (Edge refuses upgrades with an Origin unless `--remote-allow-origins` is set).
- CDP methods cdash-agent may call: `Target.getTargets`, `Target.setDiscoverTargets`, `Target.attachToTarget`, `Target.detachFromTarget`, `Page.enable`, `Page.startScreencast`, `Page.stopScreencast`, `Page.screencastFrameAck`, `Page.getNavigationHistory`, `Page.navigateToHistoryEntry`, `Page.reload`, `Input.dispatchMouseEvent`, `Input.dispatchKeyEvent`. Nothing else, specifically never `Network.*`, `Storage.*`, `Runtime.*`, `Target.activateTarget`, `Browser.setWindowBounds`, `Input.insertText`.
- Log lines, exact: `browser open <name>`, `browser close <name>`, `browser refused: <reason>`, `browser failed <name>: <error>`. Never a URL, title or input.
- Text cap: `MAX_TEXT = 4096` bytes per `text` message.
- Keys and Windows virtual key codes: Enter 13, Tab 9, Backspace 8, Escape 27, ArrowLeft 37, ArrowUp 38, ArrowRight 39, ArrowDown 40.
- Close codes on `/api/browser`: `4002` reason `Browser not running`, `4003` reason `Browser closed`, `1011` with the error text for anything else.
- Screencast: JPEG, `quality: 60`, `maxWidth: 1280`. Ack only after the phone send completes.
- Ports: JOBS `9301`, INVEST `9302`.
- The view never changes the viewed tab by itself; only a phone `tab` message does (initial pick excepted).

## Review Focus

- Edge not running when Browser is tapped (button left over from an earlier poll): the overlay must show "Browser not running", not hang. Test: Task 5, `edge_not_running_closes_4002`.
- Edge closes while viewing (agent's `browser_close`, crash): the overlay shows "Browser closed" and cdash-agent stops cleanly. Test: Task 5, `edge_going_away_closes_4003`.
- A sibling folder sharing a prefix (`/mnt/d/p/Downloads/JOBS2` vs key `/mnt/d/p/Downloads/JOBS`) must not get JOBS's browser. Test: Task 1, `prefix_match_is_by_path_component`.
- Input with no frame yet, or a tap outside the frame: dropped or clamped, never sent with garbage coordinates. Test: Task 6, `input_before_first_frame_is_dropped` and Task 3, `clamp_keeps_points_inside_the_viewport`.
- Non-ASCII and astral text (`é`, `😀`): one key press per Unicode scalar, `text` equal to that character, no virtual key code. Test: Task 3, `chars_become_one_key_press_each`.

## Deviations from the spec

- **Tab picker does not open by itself** (spec section 5). `HTMLSelectElement.showPicker()` needs transient user activation and throws `NotAllowedError` from a WebSocket handler. When `active` is null the overlay selects the `Pick a tab` placeholder and shows `Tab closed. Pick a tab.`; the user opens the picker.
- **Frame metadata is `deviceWidth` and `deviceHeight` only** (spec section 4 also names `offsetTop` and `pageScaleFactor`). In DevTools' own screencast view `offsetTop` is the strip above the image, so a point measured from the image's top needs no offset; desktop Edge reports 0 anyway. `pageScaleFactor` has no use without pinch-zoom on the PC.

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
- Modify: `crates/agent/src/collect/external.rs` (`Session` field + `browser: None` in its constructor; delete the two stale parity-gate comments at lines 13-15 and 43)
- Modify: `crates/agent/src/collect/sessions.rs` (`browser: None` in its three `Session` literals, the third being the test at line 560; set flags after the pane loop)

**Interfaces:**
- Consumes: `read_port_map`, `port_for` (Task 1).
- Produces: `pub async fn browser_flags(claude_dir: &Path, dirs: &[&str]) -> Vec<bool>`; `Session.browser: Option<bool>` serialized as `"browser"`, `#[serde(skip_serializing_if = "Option::is_none")]`.

- [ ] **Step 1: Write the failing test**

```rust
#[tokio::test]
async fn flags_are_true_only_for_a_mapped_live_port() {
    let live = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let live_port = live.local_addr().unwrap().port();
    let app = axum::Router::new().route("/json/version", axum::routing::get(|| async { "{}" }));
    tokio::spawn(async move { axum::serve(live, app).await });
    let dead_port = { let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap(); l.local_addr().unwrap().port() };
    let dir = std::env::temp_dir().join(format!("cdash-bflags-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join(MAP_FILE), format!(r#"{{"/live":{live_port},"/dead":{dead_port}}}"#)).unwrap();
    assert_eq!(browser_flags(&dir, &["/live/x", "/dead", "/other"]).await, vec![true, false, false]);
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p cdash-agent browser::tests::flags_are_true_only_for_a_mapped_live_port`
Expected: compile error, `browser_flags` not found.

- [ ] **Step 3: Implement `browser_flags`**

Read the map once. For each dir with a port, `GET http://127.0.0.1:{port}/json/version` with a `reqwest::Client` built with `.no_proxy()` and `.timeout(Duration::from_millis(300))`; true on a 2xx. Probes run concurrently (`futures_util::future::join_all`). Unmapped dirs are `false` without a request.

- [ ] **Step 4: Add the field and wire it**

Add `browser: Option<bool>` to `Session` next to `native_terminal`, `browser: None` in all four `Session` literals. In `collect_sessions`, after the pane loop and before `running.extend(external_sessions(...))`, call `browser_flags(&ctx.claude_dir, &dirs)` with `dirs` = each pane's `p.path` (the `pane_current_path` the `/api/browser` handler also looks up; Codex rows carry a saved `dir` instead). Both loop branches push one row per pane, so `running[i]` is `panes[i]`; set `browser = Some(true)` where true. External rows are never probed.

- [ ] **Step 5: Run the agent suite**

Run: `cargo test -p cdash-agent && cargo clippy -p cdash-agent --all-targets -- -D warnings`
Expected: PASS, no warnings.

- [ ] **Step 6: Commit**

```bash
git add crates/agent/src/collect/browser.rs crates/agent/src/collect/external.rs crates/agent/src/collect/sessions.rs
git commit -m "feat(agent): flag sessions whose Edge debugging port is live"
```

---

### Task 3: Phone protocol and key translation

**Files:**
- Modify: `crates/agent/src/collect/browser.rs`

**Interfaces:**
- Produces:
  - `pub const MAX_TEXT: usize = 4096;`
  - `pub enum PhoneMsg { Tap(f64, f64), Scroll { x: f64, y: f64, dx: f64, dy: f64 }, Text(String), Key(&'static str), Tab(String), Back, Reload }` (derive `Debug, PartialEq`)
  - `pub fn parse_phone(text: &str) -> Option<PhoneMsg>`: the JSON shapes in the spec's message table (`{"tap":[x,y]}`, `{"scroll":[x,y,dx,dy]}`, `{"text":"…"}`, `{"key":"Enter"}`, `{"tab":"id"}`, `{"nav":"back"|"reload"}`). `None` for unknown shapes, non-finite numbers, empty or over-`MAX_TEXT` text, unknown key or nav names, an empty tab id.
  - `pub fn key_events(key: &str) -> Option<Vec<serde_json::Value>>`: `Input.dispatchKeyEvent` params in order.
  - `pub fn char_events(c: char) -> [serde_json::Value; 2]`
  - `pub struct Viewport { pub width: f64, pub height: f64 }` and `pub fn clamp_point(x: f64, y: f64, vp: &Viewport) -> (f64, f64)`

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

### Task 4: Minimal CDP client and fake Edge

**Files:**
- Create: `crates/agent/src/collect/cdp.rs` (+ `pub mod cdp;` in `collect/mod.rs`)

**Interfaces:**
- Produces:
  - `pub struct Event { pub method: String, pub params: serde_json::Value, pub session: Option<String> }`
  - `pub struct Cdp` with `pub async fn connect(ws_url: &str) -> Result<(Cdp, tokio::sync::mpsc::UnboundedReceiver<Event>), String>` and `pub async fn call(&self, method: &str, params: serde_json::Value, session: Option<&str>) -> Result<serde_json::Value, String>` (5 s timeout per call; a CDP `error` becomes `Err(message)`). Dropping `Cdp` closes the socket. The event receiver yields `None` once the socket closes; pending calls then fail.
  - `pub async fn browser_ws_url(port: u16) -> Result<String, String>`: `ws://127.0.0.1:{port}` plus the path of `webSocketDebuggerUrl` from `http://127.0.0.1:{port}/json/version`. Only the path is taken, so a process squatting the port cannot point cdash-agent at another host. Same client rules as `browser_flags` but a 5 s timeout.
  - `pub async fn page_order(port: u16) -> Vec<String>`: ids of `type == "page"` entries from `/json/list`, in the order Edge returns them; empty on any error.
  - Test-only, `#[cfg(test)] pub(crate) mod fake`: `pub(crate) async fn fake_edge(pages: &[(&str, &str, &str)]) -> FakeEdge` (id, url, title) where `FakeEdge { pub port: u16, pub calls: mpsc::UnboundedReceiver<(String, Value, Option<String>)>, pub events: mpsc::UnboundedSender<Value>, pub drop_socket: Arc<tokio::sync::Notify> }`. Serves `/json/version`, `/json/list` and one CDP WebSocket at `/devtools/browser/fake`. Replies: `Target.getTargets` lists `pages` as `targetInfos` (`targetId`, `type: "page"`, `url`, `title`); `/json/list` returns them as `[{id, type: "page", url, title}]`; `Target.attachToTarget` returns `{"sessionId":"S-<targetId>"}`; `Page.getNavigationHistory` returns `{"currentIndex":1,"entries":[{"id":10},{"id":11}]}`; anything else `{}`. Every received call is pushed to `calls`. A value sent on `events` is written to the socket as-is. `drop_socket.notify_one()` closes the CDP socket.

- [ ] **Step 1: Write the failing test**

```rust
#[tokio::test]
async fn calls_events_and_close() {
    let mut edge = fake::fake_edge(&[("P1", "https://a.example/", "A")]).await;
    let url = browser_ws_url(edge.port).await.unwrap();
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

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p cdash-agent cdp::tests`
Expected: compile error.

- [ ] **Step 3: Implement `Cdp` and the fake**

One reader task routes replies by `id` to `oneshot` senders in a `Mutex<HashMap>` and forwards messages that have `method` to the event channel. One writer task drains an `mpsc` of outgoing frames. Connect with `tokio_tungstenite::connect_async(ws_url)` (no Origin header) inside a 5 s `tokio::time::timeout`, so a hung Edge closes `4002` instead of leaving the overlay on "Connecting…". HTTP via `reqwest` with `.no_proxy()`.

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
- Modify: `crates/agent/src/collect/term.rs`: make `PING_EVERY`, `send_or_drop` and `close_reason` (the 1011 reason must fit 123 bytes) `pub(crate)`; `mod tests` becomes `pub(crate) mod tests` with `Session`, `have_tmux`, `tempdir`, `status_of`, `close_code` `pub(crate)`, plus `Session::in_dir(tag: &str, dir: &Path) -> Session` (`tmux new-session -d -s <name> -c <dir> ... sh`).

**Interfaces:**
- Consumes: `read_port_map`, `port_for` (Task 1); `Cdp`, `browser_ws_url`, `page_order`, `fake::fake_edge` (Task 4); `origin_allowed`, `assert_kill_name`.
- Produces: `pub async fn bridge(ctx: Arc<Ctx>, name: String, port: u16, ws: WebSocket)`. Phone-bound text frames `{"frame":{"deviceWidth":f64,"deviceHeight":f64}}` (copied from the CDP frame `metadata`; see Deviations) immediately before each binary JPEG frame, and `{"tabs":[{"id","title","origin"}],"active":"<id>"|null}`.

Handler order (all before upgrade): `origin_allowed` (403), `assert_kill_name` (400), session dir from `tmux list-panes -a -F PANE_FORMAT` (404 `no such session`), `port_for` (404 `no browser for this session`). Each refusal logs `browser refused: <reason>`.

Bridge: log `browser open <name>`; `browser_ws_url` + `Cdp::connect` failing closes `4002` `Browser not running`. Initial tab: first id of `page_order(port)` that `Target.getTargets` lists as a page, else the first page. `Target.setDiscoverTargets {discover:true}`, attach (`flatten:true`), `Page.enable`, `Page.startScreencast {format:"jpeg",quality:60,maxWidth:1280}`. On `Page.screencastFrame` for the viewed session: send metadata text, then binary (base64-decoded `data`), then `Page.screencastFrameAck {sessionId: params.sessionId}`. Ping like `term.rs`. Event channel ending closes `4003` `Browser closed`. Phone close: `Page.stopScreencast`, `Target.detachFromTarget`, drop `Cdp`. Log `browser close <name>`. `origin` in the tab list is `reqwest::Url::parse(url)` origin's ASCII serialization, or the raw URL when the origin is opaque.

- [ ] **Step 1: Write the failing tests** (in `browser_bridge.rs`; tests skip when `have_tmux()` is false)

- `refusals_happen_before_upgrade`: origin `https://evil.example` gives 403; `name=other` 400; `name=cdash-missing` 404; a tmux session in an unmapped dir 404.
- `frame_is_relayed_then_acked`: fake Edge with page `P1`; map the session dir to `edge.port`; connect; push `{"method":"Page.screencastFrame","sessionId":"S-P1","params":{"data":"SlBFRw==","sessionId":7,"metadata":{"deviceWidth":1000,"deviceHeight":600,"offsetTop":0}}}`. Assert the phone receives a text frame containing `"deviceWidth":1000.0` (or `1000`), then binary `b"JPEG"`, and `calls` then contains `Page.screencastFrameAck` with `sessionId == 7`. Assert the first phone text frame is a tab list with `"active":"P1"` and `origin` `https://a.example`.
- `edge_not_running_closes_4002`: map to a closed port; `close_code` is 4002.
- `edge_going_away_closes_4003`: `drop_socket.notify_one()`; `close_code` is 4003.
- `log_never_contains_urls`: after open and close, the log has `browser open cdash-…` and `browser close cdash-…`, and no line contains `a.example`.

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
git commit -m "feat(agent): stream a session's Edge tab over /api/browser"
```

---

### Task 6: Input, tab switching and navigation

**Files:**
- Modify: `crates/agent/src/collect/browser_bridge.rs`

**Interfaces:**
- Consumes: `parse_phone`, `key_events`, `char_events`, `clamp_point`, `Viewport` (Task 3); bridge state from Task 5.

Behaviour (spec section 4): the bridge keeps the last frame's `Viewport { width: deviceWidth, height: deviceHeight }`. `Tap`: clamp, then `Input.dispatchMouseEvent` `mouseMoved`, `mousePressed`, `mouseReleased` (`button:"left"`, `clickCount:1`). `Scroll`: clamp `x,y`, `mouseWheel` with `deltaX:dx, deltaY:dy`. `Text`: `char_events` for each `char`, in order. `Key`: `key_events`. All of these are dropped when there is no viewport yet or `active` is null. `Tab(id)`: ignored unless it is a listed page; else `Page.stopScreencast` + `Target.detachFromTarget` on the old session, attach + `Page.enable` + start on the new one, send the tab list, clear the viewport. `Back`: `Page.getNavigationHistory`, then `Page.navigateToHistoryEntry {entryId}` with `entries[currentIndex-1].id`; nothing at index 0. `Reload`: `Page.reload`. `Target.targetCreated` / `targetInfoChanged` for pages: resend the tab list, active unchanged. `Target.targetDestroyed` of the viewed tab: active becomes null, viewport cleared, list resent.

- [ ] **Step 1: Write the failing tests**

- `input_before_first_frame_is_dropped`: send `{"tap":[5,5]}` before any frame; then a frame (deviceWidth 1000, deviceHeight 600); then `{"tap":[2000,-1]}`. The only `Input.dispatchMouseEvent` calls are three with `x == 1000.0, y == 0.0`.
- `text_becomes_key_presses`: after a frame, `{"text":"a$"}` yields `Input.dispatchKeyEvent` calls in order: keyDown `a` VK 65, keyUp, keyDown `$` without VK, keyUp. No `Input.insertText` call ever appears.
- `backspace_has_vk_8`: `{"key":"Backspace"}` yields rawKeyDown and keyUp with `windowsVirtualKeyCode == 8`.
- `tab_switch_and_close`: pages `P1`, `P2`. `{"tab":"nope"}` makes no calls. `{"tab":"P2"}` makes `Target.attachToTarget` with `targetId == "P2"`, and the next tab list has `"active":"P2"`. A `Target.targetCreated` for `P3` resends the list with `"active":"P2"`. A `Target.targetDestroyed` for `P2` sends `"active":null`, and a following `{"text":"x"}` makes no call.
- `back_uses_the_previous_entry_id`: `{"nav":"back"}` calls `Page.navigateToHistoryEntry` with `entryId == 10`.

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
git commit -m "feat(agent): forward taps, typing, tabs and navigation to Edge"
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
  - `toPage(x, y, shown, meta) -> [number, number]`: `x`,`y` relative to the displayed image's top-left; `shown` `{ w, h }` its displayed size; `meta` the last frame metadata. Returns `[x / shown.w * meta.deviceWidth, y / shown.h * meta.deviceHeight]` (no `offsetTop`; see Deviations).
  - `fromInput(inputType, data) -> object[]`: `insertText`, `insertFromPaste`, `insertReplacementText` with non-empty `data` give `[{ text: data }]`; `deleteContentBackward` gives `[{ key: 'Backspace' }]`; `insertLineBreak` gives `[{ key: 'Enter' }]`; anything else `[]`.

- [ ] **Step 1: Write the failing test**

```js
import { test } from 'node:test';
import assert from 'node:assert/strict';
import '../public/browser/view.js';
const B = globalThis.cdashBrowser;

test('taps map from displayed image pixels to page CSS pixels', () => {
  const meta = { deviceWidth: 985, deviceHeight: 640 };
  assert.deepEqual(B.toPage(0, 0, { w: 394, h: 256 }, meta), [0, 0]);
  assert.deepEqual(B.toPage(197, 128, { w: 394, h: 256 }, meta), [492.5, 320]);
});

test('keyboard input becomes text or keys', () => {
  assert.deepEqual(B.fromInput('insertText', 'é'), [{ text: 'é' }]);
  assert.deepEqual(B.fromInput('insertFromPaste', '123456'), [{ text: '123456' }]);
  assert.deepEqual(B.fromInput('deleteContentBackward', null), [{ key: 'Backspace' }]);
  assert.deepEqual(B.fromInput('insertLineBreak', null), [{ key: 'Enter' }]);
  assert.deepEqual(B.fromInput('insertText', ''), []);
  assert.deepEqual(B.fromInput('formatBold', null), []);
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

Markup: `<div id="browser" class="term browser" hidden>` reusing `.term-bar`, `.term-status`, `.term-reconnect`, `.term-keys`. Bar: `#browser-title`, `#browser-status`, `#browser-reconnect`, `#browser-back`, `#browser-reload`, `#browser-close`. Second row: `<select id="browser-tabs">` whose option labels are tab origins (page title in `title`), plus a disabled placeholder `Pick a tab` selected when `active` is null. Stage: `#browser-stage` holding `<img id="browser-screen" alt="">`. Hidden `<input id="browser-input" type="password" autocomplete="off" autocapitalize="off" spellcheck="false">`. Key bar: `Kbd` (`data-bkey="kbd"`) then the `KEYBAR` keys in the spec's order; `.browser .term-keys` uses 7 columns.

Behaviour in `app.js`:
- `termUrl` becomes `wsUrl(q)` (same Tauri and `wss` rules) used by both overlays.
- `runningCard`: `browserBtn` after `termBtn`, only when `r.browser`: `<button class="action" type="button" data-browser="${esc(r.name)}" data-title="${esc(r.dir.split('/').pop())}">${ICONS.globe}<span>Browser</span></button>`. Add a `globe` icon to `ICONS`.
- State `browserView = { name, ws, meta, url, retry }`. `openBrowser(name, title)` / `closeBrowser()` mirror `openTerm` / `closeTerm` (history push, popstate close, one retry on visibility, `tick()` paused while open: `if (term || browserView) return;`).
- Frames: text `{frame}` sets `meta`; next binary becomes a `Blob` object URL on `#browser-screen`, revoking the previous one. `{tabs, active}` rebuilds the select; `active === null` selects the placeholder and sets status `Tab closed. Pick a tab.` (no `showPicker()`; see Deviations).
- Close codes: 4002 shows `Browser not running`, 4003 `Browser closed`, both with Reconnect.
- Gestures on `#browser-stage` (pointer events, `touch-action: none`): a short tap with little movement sends `{tap: toPage(...)}`; two pointers pinch the image scale (1 to 5) via CSS transform; one-pointer drag pans when scale > 1 and sends `{scroll:[x,y,dx,dy]}` (page CSS px, sign so dragging up scrolls down) at scale 1. Cancel `pointerdown`/`mousedown` default so the phone keyboard stays open.
- Typing: `Kbd` focuses `#browser-input`. Keep one sentinel character in it so Backspace on an "empty" field still fires `input`; on each `input` event send `cdashBrowser.fromInput(e.inputType, e.data)` messages, then reset the value to the sentinel. Key-bar buttons send `{key: KEYBAR[k]}`.
- Back and Reload buttons send `{nav:"back"}` / `{nav:"reload"}`. Tab select change sends `{tab: value}`.

- [ ] **Step 1: Write the failing test** (append to `test/frontend-guards.test.mjs`)

```js
test('only sessions with a live browser get the Browser button', () => {
  const src = readFileSync(new URL('../public/app.js', import.meta.url), 'utf8');
  const card = src.slice(src.indexOf('function runningCard('), src.indexOf('\n}\n', src.indexOf('function runningCard(')) + 2);
  const context = { esc: s => s, effortLabel: s => s, gitBadge: () => '', fmtUp: () => '', armedKill: null, ICONS: new Proxy({}, { get: () => '' }) };
  runInNewContext(`${card}; this.runningCard = runningCard;`, context);
  const row = { name: 'cdash-a', dir: '/x/JOBS', working: false };
  assert.match(context.runningCard({ ...row, browser: true }), /data-browser="cdash-a"/);
  assert.doesNotMatch(context.runningCard(row), /data-browser/);
});
```

- [ ] **Step 2: Run to verify failure**

Run: `node --test test/frontend-guards.test.mjs`
Expected: the new test FAILS (no `data-browser`).

- [ ] **Step 3: Implement markup, styles and `app.js` wiring**

- [ ] **Step 4: Run the suites**

Run: `npm test && cargo test -p cdash-agent`
Expected: PASS.

- [ ] **Step 5: Smoke test against a scratch Edge**

From WSL: `cmd.exe /d /c start "" "C:\Program Files (x86)\Microsoft\Edge\Application\msedge.exe" --user-data-dir=%TEMP%\cdash-bv --remote-debugging-port=9399 https://example.com`. Write `{"<a tmux session's dir>": 9399}` to `$CLAUDE_DIR/cdash-browsers.json`, run `cargo run -p cdash-agent`, open `http://127.0.0.1:23274` in Chrome with mobile emulation. Check: the Browser button appears within one poll; the picture shows example.com; tapping the "More information" link navigates; Back returns; the origin line reads `https://example.com`. Close Edge: the overlay shows `Browser closed`. Delete the scratch profile and the map entry afterwards.

- [ ] **Step 6: Commit**

```bash
git add public test/frontend-guards.test.mjs
git commit -m "feat(ui): add the Browser overlay"
```

---

### Task 9: README

**Files:**
- Modify: `README.md` (new paragraph after the Terminal paragraph, line 7)

Cover, in the README's voice: what Browser is for (typing logins and MFA into a session's Playwright Edge); the `cdash-browsers.json` format with the JOBS/INVEST example; the Playwright MCP `--config` with `launchOptions.args` `--remote-debugging-port`; loopback only, and that any local process can drive those tabs while the port is open; WSL `networkingMode=mirrored` required; Edge's own popups (saved-password dropdowns, save prompts, Windows Hello) are not visible, so type passwords; the log records only open and close.

- [ ] **Step 1: Write the paragraph**
- [ ] **Step 2: Commit**

```bash
git add README.md
git commit -m "docs: describe the Browser view"
```

---

### Task 10: Launchers, port map and end-to-end check (user's machine, outside the repo)

**Files (back up each to `<file>.bak-YYYYMMDD-browser-view` first; these folders are not git repos):**
- Create: `D:\p\Downloads\JOBS\tools\playwright-mcp.json` with `{ "browser": { "launchOptions": { "args": ["--remote-debugging-port=9301"] } } }`
- Modify: `D:\p\Downloads\JOBS\tools\playwright-jobs.cmd`: append `--config=D:/p/Downloads/JOBS/tools/playwright-mcp.json` before `%*`
- Modify: `D:\p\Downloads\JOBS\protocol\tools.md` line 36 setup command to match the `.cmd`
- Create: `D:\p\Downloads\INVEST\tools\playwright-mcp.json` (port `9302`) and `D:\p\Downloads\INVEST\tools\playwright-invest.cmd` (copy of the JOBS `.cmd` with the INVEST profile and config paths; CRLF line endings)
- Modify: `D:\p\Downloads\INVEST\.mcp.json` and `D:\p\Downloads\INVEST\.codex\config.toml`: `playwright-invest` becomes `command = "cmd.exe"`, `args = ["/d", "/c", "D:\\p\\Downloads\\INVEST\\tools\\playwright-invest.cmd"]`
- Modify: `D:\p\Downloads\INVEST\protocol\tools.md` setup command (line 39) to name the wrapper
- Create: `~/.claude/cdash-browsers.json` with `{"/mnt/d/p/Downloads/JOBS": 9301, "/mnt/d/p/Downloads/INVEST": 9302}` (use the agent's `CLAUDE_DIR` if it differs)

- [ ] **Step 1: Confirm no JOBS or INVEST session is running** (the profile lock would block the check), then make the edits above.
- [ ] **Step 2: Verify each launcher adds its port.** Drive the `.cmd` with a minimal MCP client (initialize, then `browser_navigate` to `about:blank`).
  - JOBS: from WSL, `curl -s 127.0.0.1:9301/json/version`. Expected: JSON with `"Browser": "Edg/…"`.
  - INVEST: **do not connect to 9302.** The INVEST amendment says agents never connect to that port, and the builder is an agent. Read Edge's command line instead: `/mnt/c/Windows/System32/WindowsPowerShell/v1.0/powershell.exe -NoProfile -Command "Get-CimInstance Win32_Process -Filter 'Name=''msedge.exe''' | % CommandLine" | grep -- '--remote-debugging-port=9302'`. Expected: a line that also holds `--user-data-dir=D:/p/Downloads/INVEST/PlaywrightProfile`. That 9302 answers is proven by cdash-agent's probe and the user's phone check (Step 4).
  - Close each browser with `browser_close`.
- [ ] **Step 3: Install the build** the user runs (per memory `cdash-release-install-paths`) and restart the agent.
- [ ] **Step 4: Hand the user the phone check** from the spec's Testing section: a dashboard-launched JOBS session reaches a login page; on the phone with the PC locked, open Browser, check the origin line, type into a field, submit, close the overlay, confirm the agent continues. Repeat once for INVEST.
- [ ] **Step 5: Mark the spec** `Status: implemented; phone verification <passed|pending>` and commit.
