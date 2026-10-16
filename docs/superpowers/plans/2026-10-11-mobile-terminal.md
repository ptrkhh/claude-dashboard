# Mobile Terminal Implementation Plan

## Implementation status (2026-10-17)

Tasks 1–7 and the README changes in task 8 are implemented. The step checkboxes
below describe the original implementation procedure, not remaining work.
Current `origin/master` (`a1a2f24`) is merged, including branch metadata, the
60-second polling interval, and the stats-row and battery-meter fixes.

Verification after the merge:

- `cargo test --all --locked`: 324 tests passed, including real tmux terminal
  input, resize, close codes, disconnect cleanup, and route authentication.
- `cargo clippy --all-targets --locked -- -D warnings -D clippy::disallowed_types`:
  passed.
- `cargo build --release --locked -p cdash-agent`: passed.
- All 28 Node tests passed with `npm test -- --test-concurrency=1 test/*.test.mjs`.
  A temporary curl wrapper limited connection setup to two seconds because this
  computer drops connections to the failed-download fixture's port 1. Initial
  parallel runs had an installer timing failure and a Codex thread-list timeout;
  both passed on rerun. No production code or test assertions were changed.
- Chromium smoke test against the release binary: typed input, key-bar Enter,
  sticky Ctrl, portrait-to-landscape resize, reopen, close button, and browser
  Back passed. Closing detached the client and preserved the tmux session.
- The local `~/cdash-agent` was replaced and restarted with its existing
  environment. Health, the running binary's SHA-256, embedded assets, and the
  same browser smoke test passed against port 23274. The previous binary is
  retained as `~/cdash-agent.before-mobile-terminal-20261017`.

Physical Android Tauri and mobile Chrome checks in task 8 remain unrun: native
IME composition and keyboard resizing, `/mcp`, Shift+Tab, and fullscreen
conversation scrolling require device testing. Browser viewport resizing does
not prove native keyboard behavior. Remote profiles requiring authentication
headers and DNS-rebinding protection retain the design's documented limits.

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace the single-line send bar with a Terminal button that opens a dashboard session's real Claude Code TUI on a phone, with a key bar for the keys a phone keyboard lacks.

**Architecture:** The agent gains `GET /api/term`, a WebSocket that runs `tmux -u attach-session -t =<name>` inside a PTY (opened with `rustix::pty`) and pipes bytes both ways. The UI gains a full-screen overlay rendering that stream with vendored xterm.js, plus a two-row key bar. Dashboard sessions launch with `"tui":"fullscreen"` so PgUp/PgDn scroll inside Claude Code.

**Tech Stack:** Rust (axum 0.8.9 `ws`, tokio `net`/`AsyncFd`, rustix 1 `pty`/`process`/`termios`), tokio-tungstenite 0.29 (tests only), plain JS (no bundler), xterm.js 5.5.0 + addon-fit 0.10.0 UMD builds, node `--test`, Tauri 2.

**Spec:** `docs/superpowers/specs/2026-10-11-mobile-terminal-design.md`

## Global Constraints

- Terminal button only on dashboard-owned (tmux, `cdash-*`) cards; external sessions get none.
- Origin rule: allow when `Origin` is absent, its host[:port] equals the `Host` header, it equals the host[:port] of `CDASH_PUBLIC_URL` when set, or it is exactly `http://tauri.localhost` or `tauri://localhost`. Compare host[:port] only, never the scheme. Refuse with 403 otherwise.
- Name check: `assert_kill_name`, same rule as `/api/kill`.
- `cols`/`rows`: integers in 1..=1000, else 400.
- Close codes: 4000 `session ended` (session gone), 4001 `detached` (session still exists), 1011 with error text (spawn failure).
- Never log terminal content. Log one line on open and one on close, naming the session.
- Key bytes: Esc `\x1b`, Tab `\t`, ⇧Tab `\x1b[Z`, PgUp `\x1b[5~`, PgDn `\x1b[6~`, ← `\x1b[D`, ↑ `\x1b[A`, ↓ `\x1b[B`, → `\x1b[C`, ^C `\x03`, ⏎ `\r`.
- Sticky Ctrl: next single IME character in `@`, `A-Z`, `[ \ ] ^ _`, `a-z` becomes `code & 0x1f`; anything else passes through; disarm after one input; tap again to cancel.
- Key bar layout, two rows: `Esc Tab ⇧Tab Ctrl PgUp PgDn` / `^C ← ↑ ↓ → ⏎`.
- Auto-retry once on page-visible only for unexpected drops; never after 4000 or 4001.
- `--settings` for dashboard children becomes `{"tui":"fullscreen","env":{"CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC":""}}`.
- No new runtime crate. New dev-dependencies only: `tokio-tungstenite = "0.29"`, `futures-util = "0.3"`.
- `tokio::process::Command` is a clippy `disallowed_type`; the one new use needs `#[allow(clippy::disallowed_types)]` with a reason, like `host/cmd.rs`.
- Commits end with:
  ```
  Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>
  Claude-Session: https://claude.ai/code/session_01M311G6RRBCf9gUczbjT9kc
  ```

**Deviation from spec, deliberate:** the spec says non-unix builds answer 501. The agent crate already cannot build off unix (rustix `termios` is unconditional, and `crates/tauri-app/Cargo.toml:32` only links the agent on non-Windows, non-Android targets). So no `cfg(not(unix))` branch is written.

## Review Focus

1. **Agent started inside a tmux session** (Termux users often run it from a shell inside tmux, so `TMUX` is set). Expected: attach works and reaches the same server `spawn_claude`'s `new-session` used. tmux refuses to nest only when the client's tty is one of its own pane ttys, and the bridge's tty is a fresh PTY, so attach works with `TMUX` set (checked on tmux 3.4). `TMUX` also names the server socket, so removing it would send attach to the default socket while `new-session` and `has-session` follow `TMUX`: under a non-default socket every terminal would close 4000 "session ended". Pinned by the `attach_command` test in Task 4 (`TMUX` inherited).
2. **Agent environment without a UTF-8 locale** (boot script, systemd unit). The tmux client replaces every non-ASCII glyph, including Claude's box drawing, with `_`. Expected: correct glyphs. Pinned by the `attach_command` test in Task 4 (`-u` flag).
3. **Large paste or burst of input** larger than the PTY buffer. A non-blocking write returns short or `EAGAIN`. Expected: every byte arrives, in order. Pinned by `write_all_delivers_every_byte_through_a_full_pipe` in Task 4.
4. **Phone drops the connection without a close frame** (sleep, network loss). Expected: the `tmux attach` client goes away, the session stays. A TCP close is pinned by `an_unclean_drop_detaches_the_client`. Network loss sends no FIN at all, and neither axum nor the server sets a keepalive, so only the bridge's own ping (`PING_EVERY`) reveals it. Pinned by `a_client_that_stops_answering_is_detached` in Task 4.
5. **Close code lost to a TCP reset.** The bridge sends 4000/4001 and drops the socket. A frame the browser sent just before (a pong to the bridge's ping, a keystroke) is then unread, so Linux answers RST, and the browser can discard the close frame and see 1006, which shows "Disconnected" and auto-retries instead of "Session ended". Expected: the code arrives. `bridge` drains until the browser's Close reply (1 s cap). Pinned by the 4000/4001 tests: without the drain they failed 72 of 150 isolated runs, with it 0 of 150.
6. **Session name that is a prefix of another session's name.** `tmux -t cdash-a` would match `cdash-ab`. Expected: no attach to the wrong session; the socket closes 4000. Pinned by `a_prefix_name_does_not_attach_a_longer_session` in Task 4.

---

## File map

| File | Change | Responsibility |
|---|---|---|
| `crates/agent/src/collect/spawn.rs` | modify | `--settings` adds `"tui":"fullscreen"` |
| `crates/agent/src/collect/keys.rs` | delete | old send bar backend |
| `crates/agent/src/collect/term.rs` | create | Origin rule, size parsing, PTY, `tmux attach` bridge |
| `crates/agent/src/collect/mod.rs` | modify | swap `keys` for `term` |
| `crates/agent/src/http/routes.rs` | modify | drop `post_keys`, add `get_term` |
| `crates/agent/src/http/serve.rs` | modify | route table, embedded vendor/term assets |
| `crates/agent/Cargo.toml` | modify | axum `ws`, tokio `net`, rustix `pty`+`process`, dev-deps |
| `.github/workflows/ci.yml` | modify | install `tmux` |
| `crates/tauri-app/src/lib.rs` | modify | `agent_base` command |
| `public/vendor/*` | create | xterm.js, addon-fit, xterm.css, licenses |
| `public/term/keys.js` | create | key bytes + sticky Ctrl, DOM-free |
| `test/keys.test.mjs` | create | node tests for `keys.js` |
| `public/index.html` | modify | viewport meta, vendor tags, overlay markup |
| `public/app.js` | modify | remove send bar, add Terminal button + overlay |
| `public/style.css` | modify | remove `.send*`, add `.term*` |
| `test/frontend-guards.test.mjs` | modify | send bar gone, Terminal button guarded |
| `README.md` | modify | replace send-bar paragraph with Terminal docs |

---

### Task 1: Launch dashboard sessions in the fullscreen renderer

**Files:**
- Modify: `crates/agent/src/collect/spawn.rs:144` and the test at `crates/agent/src/collect/spawn.rs:544-557`

**Interfaces:**
- Consumes: nothing.
- Produces: nothing other tasks call. Behavior only.

- [ ] **Step 1: Update the existing test to expect the new settings JSON**

Replace the body of `launch_overrides_the_setting_that_disables_remote_control` (`spawn.rs:544`) and rename it:

```rust
    #[tokio::test]
    async fn launch_passes_the_dashboard_settings_override() {
        let d = tempdir("rc-setting");
        let ctx = ctx_with_tmux(d.clone(), 0).await;

        launch_session(&ctx, d.to_str().unwrap(), "sonnet", "medium").await.unwrap();

        let args = std::fs::read_to_string(d.join("tmux-args")).unwrap();
        let first = args.lines().next().unwrap();
        assert!(
            first.contains(
                r#"--settings {"tui":"fullscreen","env":{"CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC":""}}"#
            ),
            "dashboard launches keep Remote Control enabled and start in the \
             fullscreen renderer, so PgUp/PgDn scroll from the phone: {args}"
        );
    }
```

- [ ] **Step 2: Run it to see it fail**

Run: `cargo test -p cdash-agent launch_passes_the_dashboard_settings_override`
Expected: FAIL, assertion message shows the old `{"env":...}` JSON.

- [ ] **Step 3: Change the override**

In `spawn_claude` (`spawn.rs:132-145`), update the comment and the literal:

```rust
    // Per dashboard child only; the user's settings file stays untouched.
    // The explicit Remote Control request must win over the user-level
    // opt-out, and the fullscreen renderer is what makes PgUp/PgDn scroll the
    // conversation from the phone's terminal key bar.
    let mut args: Vec<&str> = vec![
        "new-session",
        "-d",
        "-s",
        &name,
        "-c",
        dir,
        "claude",
        "--settings",
        r#"{"tui":"fullscreen","env":{"CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC":""}}"#,
    ];
```

- [ ] **Step 4: Run the spawn tests**

Run: `cargo test -p cdash-agent collect::spawn`
Expected: all PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/agent/src/collect/spawn.rs
git commit -m "feat(spawn): start dashboard sessions in the fullscreen renderer

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_01M311G6RRBCf9gUczbjT9kc"
```

---

### Task 2: Remove the send bar and `POST /api/keys`

**Files:**
- Delete: `crates/agent/src/collect/keys.rs`
- Modify: `crates/agent/src/collect/mod.rs` (drop `pub mod keys;`)
- Modify: `crates/agent/src/http/routes.rs:104` (import), `:198-216` (`KeysBody`, `post_keys`), `:432-473` (three `keys_*`/`a_failed_send_*` tests)
- Modify: `crates/agent/src/http/serve.rs:42` (route line)
- Modify: `public/app.js:15,17,33,217-224,234,285-287,316-339`
- Modify: `public/style.css:380-403`
- Modify: `test/frontend-guards.test.mjs`

**Interfaces:**
- Consumes: nothing.
- Produces: the actions row in `runningCard` (`public/app.js`) is `<div class="actions">${rc}${kill}</div>` with no form after it. Task 7 inserts the Terminal button there.

- [ ] **Step 1: Write the failing guard test**

Append to `test/frontend-guards.test.mjs`:

```js
test('the send bar is gone: the terminal replaced it', () => {
  // It typed one line into the pane and could not operate anything that
  // needs arrow keys (/mcp menus), which is why the terminal replaced it.
  const app = readFileSync(new URL('../public/app.js', import.meta.url), 'utf8');
  const css = readFileSync(new URL('../public/style.css', import.meta.url), 'utf8');
  assert.ok(!app.includes('/api/keys'), 'app.js no longer posts to /api/keys');
  assert.ok(!app.includes('send-input'), 'no send input is rendered');
  assert.ok(!/\.send[\s{.:-]/.test(css), 'no .send styles remain');
});
```

- [ ] **Step 2: Run it to see it fail**

Run: `node --test test/frontend-guards.test.mjs`
Expected: FAIL on "app.js no longer posts to /api/keys".

- [ ] **Step 3: Remove the UI half**

In `public/app.js`:
- Delete line 15 (`const drafts = {};` …) and line 17 (`const sending = new Set();` …).
- Delete the `send:` entry from `ICONS` (line 33).
- In `runningCard`, delete the `// Types into the session's TUI` comment and the whole `const send = …;` declaration (lines 217-224), and delete the `${send}` line after `<div class="actions">${rc}${kill}</div>`.
- In `render`, replace the guarded assignment (lines 283-287, the comment plus `if (!document.activeElement?.classList.contains('send-input'))`) with the plain assignment:

```js
  $('#running').innerHTML = d.running.map(runningCard).join('') || '<div class="empty">No running sessions</div>';
```

- Delete the whole `// Send-to-TUI:` block: the `input` listener and the `submit` listener (lines 316-339).

In `public/style.css`, delete lines 378-403 (the `/* Send-to-TUI bar … */` comment and every `.send`, `.send-input`, `.send-btn` rule).

- [ ] **Step 4: Remove the agent half**

```bash
git rm crates/agent/src/collect/keys.rs
```

- `crates/agent/src/collect/mod.rs`: delete `pub mod keys;`.
- `crates/agent/src/http/routes.rs`: delete `use crate::collect::keys::send_keys;`, the `KeysBody` struct with its doc comments, and `post_keys` with its doc comment. In the test module delete `keys_rejects_a_name_that_is_not_a_cdash_session`, `keys_rejects_an_absent_or_empty_text_before_touching_tmux` and `a_failed_send_is_a_500_not_a_cheerful_ok`.
- `crates/agent/src/http/serve.rs`: delete `post "/api/keys" => routes::post_keys,`.

- [ ] **Step 5: Run everything**

Run: `node --test && cargo test -p cdash-agent && cargo clippy -p cdash-agent --all-targets -- -D warnings -D clippy::disallowed_types`
Expected: all PASS, no warnings (an unused-import warning means a leftover reference).

- [ ] **Step 6: Commit**

```bash
git add -A crates/agent public test
git commit -m "refactor!: remove the send bar and POST /api/keys

The terminal replaces it: one typed line could not operate anything that
needs arrow keys, such as the /mcp menu.

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_01M311G6RRBCf9gUczbjT9kc"
```

---

### Task 3: Terminal request validation (Origin, size, resize message)

**Files:**
- Create: `crates/agent/src/collect/term.rs`
- Modify: `crates/agent/src/collect/mod.rs` (add `pub mod term;`)

**Interfaces:**
- Consumes: `crate::collect::validate::BadRequest` (tuple struct `BadRequest(pub String)`).
- Produces (used by Task 4):
  - `pub fn origin_allowed(origin: Option<&str>, host: Option<&str>, public_url: Option<&str>) -> bool`
  - `pub fn parse_size(cols: Option<&str>, rows: Option<&str>) -> Result<(u16, u16), BadRequest>`
  - `pub fn parse_resize(text: &str) -> Option<(u16, u16)>`

- [ ] **Step 1: Write the failing tests**

Create `crates/agent/src/collect/term.rs` with only the test module, so it fails to compile against missing functions:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn origin_allows_what_only_the_dashboard_itself_can_send() {
        // Non-browser clients send no Origin and are not a cross-site vector.
        assert!(origin_allowed(None, Some("localhost:23274"), None));
        assert!(origin_allowed(Some("http://localhost:23274"), Some("localhost:23274"), None));
        assert!(origin_allowed(Some("http://LOCALHOST:23274"), Some("localhost:23274"), None));
        // Behind a TLS-terminating proxy the agent sees plain HTTP while the
        // browser sends https; the scheme must not matter.
        assert!(origin_allowed(Some("https://dash.example.com"), Some("dash.example.com"), None));
        // A proxy that rewrites Host is covered by CDASH_PUBLIC_URL.
        assert!(origin_allowed(
            Some("https://dash.example.com"),
            Some("127.0.0.1:23274"),
            Some("https://dash.example.com/"),
        ));
        assert!(origin_allowed(Some("http://tauri.localhost"), Some("localhost:23274"), None));
        assert!(origin_allowed(Some("tauri://localhost"), Some("localhost:23274"), None));
    }

    #[test]
    fn origin_refuses_every_other_page() {
        assert!(!origin_allowed(Some("https://evil.example"), Some("localhost:23274"), None));
        // Same host, different port: a different origin, possibly another app.
        assert!(!origin_allowed(Some("http://localhost:3000"), Some("localhost:23274"), None));
        // Sandboxed iframes and file:// pages send the literal "null".
        assert!(!origin_allowed(Some("null"), Some("localhost:23274"), None));
        assert!(!origin_allowed(Some("http://localhost:23274"), None, None));
        assert!(!origin_allowed(
            Some("https://evil.example"),
            Some("127.0.0.1:23274"),
            Some("https://dash.example.com"),
        ));
        // Lookalikes of the Tauri origins are not Tauri.
        assert!(!origin_allowed(Some("http://tauri.localhost.evil.example"), Some("x"), None));
    }

    #[test]
    fn size_must_be_two_integers_in_range() {
        assert_eq!(parse_size(Some("80"), Some("24")).unwrap(), (80, 24));
        assert_eq!(parse_size(Some("1"), Some("1000")).unwrap(), (1, 1000));
        for (c, r) in [
            (None, Some("24")),
            (Some("80"), None),
            (Some("0"), Some("24")),
            (Some("80"), Some("1001")),
            (Some("-1"), Some("24")),
            (Some("eighty"), Some("24")),
        ] {
            let e = parse_size(c, r).unwrap_err();
            assert!(e.0.contains("bad size"), "{c:?}x{r:?} gave {}", e.0);
        }
    }

    #[test]
    fn resize_message_is_the_one_text_frame_the_bridge_reads() {
        assert_eq!(parse_resize(r#"{"resize":[100,30]}"#), Some((100, 30)));
        assert_eq!(parse_resize(r#"{"resize":[0,30]}"#), None);
        assert_eq!(parse_resize(r#"{"resize":[100,5000]}"#), None);
        assert_eq!(parse_resize(r#"{"resize":[100]}"#), None);
        assert_eq!(parse_resize("hello"), None);
    }
}
```

Add `pub mod term;` to `crates/agent/src/collect/mod.rs`.

- [ ] **Step 2: Run to see it fail**

Run: `cargo test -p cdash-agent collect::term`
Expected: compile error, `cannot find function origin_allowed`.

- [ ] **Step 3: Implement**

Put this above the test module in `term.rs`:

```rust
//! A live terminal on a dashboard session: `tmux attach` in a PTY, bridged to
//! a WebSocket. This half is the request validation; the bridge follows.

use super::validate::BadRequest;
use serde::Deserialize;

/// Upper bound for either dimension. Far past any real screen, low enough
/// that a hostile value cannot ask tmux for an absurd allocation.
const MAX_DIM: u16 = 1000;

/// Whether a WebSocket upgrade carrying `origin` may attach.
///
/// A WebSocket upgrade is not subject to CORS, so without this any page open
/// on the phone could reach `ws://localhost:<port>` and type into a session,
/// which amounts to a shell. Only host[:port] is compared, never the scheme:
/// behind a TLS-terminating proxy the agent sees plain HTTP while the browser
/// sends `https://`, and a page must already be served from the agent's own
/// host to pass either way. `public_url` (`CDASH_PUBLIC_URL`) covers a proxy
/// that rewrites `Host`. Does not stop DNS rebinding; see the spec.
pub fn origin_allowed(origin: Option<&str>, host: Option<&str>, public_url: Option<&str>) -> bool {
    let Some(origin) = origin else { return true };
    if origin == "http://tauri.localhost" || origin == "tauri://localhost" {
        return true;
    }
    let Some(o) = authority(origin) else { return false };
    host.is_some_and(|h| h.eq_ignore_ascii_case(o))
        || public_url.and_then(authority).is_some_and(|p| p.eq_ignore_ascii_case(o))
}

/// `host[:port]` of an absolute URL: what follows `://`, up to the first `/`.
fn authority(url: &str) -> Option<&str> {
    let rest = url.split_once("://")?.1;
    let a = rest.split('/').next().unwrap_or_default();
    (!a.is_empty()).then_some(a)
}

fn dim(v: Option<&str>) -> Option<u16> {
    v?.parse::<u16>().ok().filter(|n| (1..=MAX_DIM).contains(n))
}

/// The initial `cols`/`rows` query values. Required, so the first frame is
/// already drawn at phone width rather than reflowed from 80x24.
pub fn parse_size(cols: Option<&str>, rows: Option<&str>) -> Result<(u16, u16), BadRequest> {
    match (dim(cols), dim(rows)) {
        (Some(c), Some(r)) => Ok((c, r)),
        _ => Err(BadRequest(format!("bad size: cols and rows must be 1..={MAX_DIM}"))),
    }
}

#[derive(Deserialize)]
struct Resize {
    resize: [u16; 2],
}

/// `{"resize":[cols,rows]}`, the only text frame the browser sends. Anything
/// else is `None` and the bridge ignores it.
pub fn parse_resize(text: &str) -> Option<(u16, u16)> {
    let [c, r] = serde_json::from_str::<Resize>(text).ok()?.resize;
    let ok = |n: u16| (1..=MAX_DIM).contains(&n);
    (ok(c) && ok(r)).then_some((c, r))
}
```

- [ ] **Step 4: Run the tests**

Run: `cargo test -p cdash-agent collect::term`
Expected: 4 PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/agent/src/collect/term.rs crates/agent/src/collect/mod.rs
git commit -m "feat(term): validate terminal upgrades: origin, size, resize frames

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_01M311G6RRBCf9gUczbjT9kc"
```

---

### Task 4: PTY bridge and `GET /api/term`

**Files:**
- Modify: `crates/agent/Cargo.toml`
- Modify: `crates/agent/src/collect/term.rs` (bridge, PTY, tests)
- Modify: `crates/agent/src/http/routes.rs` (`get_term`)
- Modify: `crates/agent/src/http/serve.rs` (route line)
- Modify: `.github/workflows/ci.yml:16-17`

**Interfaces:**
- Consumes: Task 3's `origin_allowed`, `parse_size`, `parse_resize`; `crate::collect::validate::assert_kill_name(&str) -> Result<(), BadRequest>`; `Ctx { host.path: String, host.log: Arc<LogBuffer>, runner: Arc<Runner> }`; `Runner::run_checked(&self, &str, &[&str], &str) -> Result<String, String>`; `LogBuffer::push(&self, impl AsRef<str>)`; test helpers `crate::http::serve::serve`, `crate::http::serve::tests::cfg_for(PathBuf) -> Config`.
- Produces:
  - Route `GET /api/term?name=<cdash-…>&cols=<n>&rows=<n>` (WebSocket). Binary frames both ways are raw terminal bytes. Text frame `{"resize":[c,r]}` from the client resizes. Close codes 4000 / 4001 / 1011 as in Global Constraints. Task 7 depends on this contract.
  - `pub async fn bridge(ctx: Arc<Ctx>, name: String, cols: u16, rows: u16, ws: WebSocket)`

- [ ] **Step 1: Dependencies and CI**

`crates/agent/Cargo.toml`, change these lines:

```toml
rustix = { version = "1", features = ["fs", "termios", "pty", "process"] }
tokio = { version = "1", features = ["process", "time", "rt", "rt-multi-thread", "macros", "fs", "io-util", "net"] }
axum = { version = "0.8.9", features = ["ws"] }
```

and append a new section (the crate has none yet):

```toml
[dev-dependencies]
tokio-tungstenite = "0.29"
futures-util = "0.3"
```

`.github/workflows/ci.yml`, the apt line becomes:

```yaml
            libwebkit2gtk-4.1-dev libayatana-appindicator3-dev tmux
```

Run: `cargo build -p cdash-agent`
Expected: builds. `Cargo.lock` changes.

- [ ] **Step 2: Write the failing tests**

Append inside `mod tests` in `term.rs`. These need a real tmux and skip themselves without one. They use the user's default tmux server with unique `cdash-termtest-*` names and kill what they create.

```rust
    use crate::http::serve::{serve, tests::cfg_for};
    use futures_util::{SinkExt, StreamExt};
    use std::time::Duration;
    use tokio_tungstenite::tungstenite::{self, client::IntoClientRequest, Message as WsMsg};

    type Ws = tokio_tungstenite::WebSocketStream<
        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
    >;

    fn have_tmux() -> bool {
        #[allow(clippy::disallowed_types)] // test-only probe, no Runner in scope
        let ok = std::process::Command::new("tmux").arg("-V").output().is_ok();
        if !ok {
            eprintln!("tmux not installed; skipping");
        }
        ok
    }

    fn tmux(args: &[&str]) -> std::process::Output {
        #[allow(clippy::disallowed_types)] // test-only fixture control
        std::process::Command::new("tmux").args(args).output().unwrap()
    }

    /// A detached `sh` session, killed on drop.
    struct Session(String);
    impl Session {
        fn new(tag: &str) -> Self {
            let name = format!("cdash-termtest-{}-{tag}", std::process::id());
            let _ = tmux(&["kill-session", "-t", &format!("={name}")]);
            assert!(tmux(&["new-session", "-d", "-s", &name, "-x", "80", "-y", "24", "sh"])
                .status
                .success());
            Self(name)
        }
        fn clients(&self) -> String {
            let o = tmux(&["list-clients", "-t", &format!("={}", self.0), "-F", "#{client_width}x#{client_height}"]);
            String::from_utf8_lossy(&o.stdout).trim().to_string()
        }
    }
    impl Drop for Session {
        fn drop(&mut self) {
            let _ = tmux(&["kill-session", "-t", &format!("={}", self.0)]);
        }
    }

    fn tempdir(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("cdash-term-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("sessions")).unwrap();
        dir
    }

    async fn connect(
        addr: std::net::SocketAddr,
        query: &str,
        origin: Option<&str>,
    ) -> Result<Ws, tungstenite::Error> {
        let mut req = format!("ws://{addr}/api/term?{query}").into_client_request().unwrap();
        if let Some(o) = origin {
            req.headers_mut().insert("origin", o.parse().unwrap());
        }
        tokio_tungstenite::connect_async(req).await.map(|(ws, _)| ws)
    }

    fn status_of(r: Result<Ws, tungstenite::Error>) -> u16 {
        match r {
            Err(tungstenite::Error::Http(res)) => res.status().as_u16(),
            Err(e) => panic!("expected an HTTP refusal, got {e}"),
            Ok(_) => panic!("expected an HTTP refusal, got an open socket"),
        }
    }

    /// Reads binary frames until their accumulated text contains `needle`.
    async fn read_until(ws: &mut Ws, needle: &str) -> String {
        let mut seen = String::new();
        tokio::time::timeout(Duration::from_secs(5), async {
            while !seen.contains(needle) {
                match ws.next().await {
                    Some(Ok(WsMsg::Binary(b))) => seen.push_str(&String::from_utf8_lossy(&b)),
                    Some(Ok(_)) => {}
                    other => panic!("socket ended before {needle:?}: {other:?}\n{seen}"),
                }
            }
        })
        .await
        .unwrap_or_else(|_| panic!("timed out waiting for {needle:?}; saw:\n{seen}"));
        seen
    }

    /// Drains frames until the close frame and returns its code.
    async fn close_code(ws: &mut Ws) -> u16 {
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                match ws.next().await {
                    Some(Ok(WsMsg::Close(Some(f)))) => return u16::from(f.code),
                    Some(Ok(WsMsg::Close(None))) | None => panic!("closed without a code"),
                    Some(Ok(_)) => {}
                    Some(Err(e)) => panic!("socket error before close: {e}"),
                }
            }
        })
        .await
        .expect("no close frame within 5s")
    }

    async fn eventually(what: &str, mut f: impl FnMut() -> bool) {
        for _ in 0..50 {
            if f() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        panic!("never happened: {what}");
    }

    #[tokio::test]
    async fn refusals_happen_before_anything_spawns() {
        let b = serve(cfg_for(tempdir("refuse"))).await.unwrap();
        let ok = "name=cdash-x&cols=80&rows=24";
        assert_eq!(status_of(connect(b.addr, ok, Some("https://evil.example")).await), 403);
        assert_eq!(status_of(connect(b.addr, "name=other&cols=80&rows=24", None).await), 400);
        assert_eq!(status_of(connect(b.addr, "name=cdash-x&cols=0&rows=24", None).await), 400);
    }

    #[test]
    fn attach_command_survives_a_nested_and_non_utf8_environment() {
        // Review focus 1 and 2: an agent started inside tmux inherits TMUX,
        // which names the server `new-session` used, so attach must keep it;
        // an agent without a UTF-8 locale makes the tmux client draw every
        // box character as `_`.
        let cmd = attach_command("cdash-a", "/usr/bin:/bin");
        let std = cmd.as_std();
        let args: Vec<_> = std.get_args().map(|a| a.to_string_lossy().into_owned()).collect();
        assert_eq!(args, ["-u", "attach-session", "-t", "=cdash-a"]);
        let envs: Vec<_> = std.get_envs().collect();
        assert!(envs.iter().all(|(k, _)| *k != "TMUX"), "TMUX is inherited: {envs:?}");
        assert!(envs.contains(&(
            std::ffi::OsStr::new("TERM"),
            Some(std::ffi::OsStr::new("xterm-256color"))
        )));
    }

    #[tokio::test]
    async fn write_all_delivers_every_byte_through_a_full_pipe() {
        // Review focus 3: a paste bigger than the kernel buffer comes back
        // short or EAGAIN from a non-blocking write.
        let (mut r, w) = std::io::pipe().unwrap();
        let w = OwnedFd::from(w);
        rustix::fs::fcntl_setfl(&w, rustix::fs::OFlags::NONBLOCK).unwrap();
        let w = tokio::io::unix::AsyncFd::new(w).unwrap();
        let data: Vec<u8> = (0..300_000u32).map(|i| (i % 251) as u8).collect();
        let reader = std::thread::spawn(move || {
            let mut out = Vec::new();
            std::io::Read::read_to_end(&mut r, &mut out).unwrap();
            out
        });
        write_all(&w, &data).await.unwrap();
        drop(w);
        assert_eq!(reader.join().unwrap(), data);
    }

    #[tokio::test]
    async fn typed_bytes_reach_the_pane_and_its_output_comes_back() {
        if !have_tmux() {
            return;
        }
        let s = Session::new("echo");
        let b = serve(cfg_for(tempdir("echo"))).await.unwrap();
        let mut ws = connect(b.addr, &format!("name={}&cols=80&rows=24", s.0), None).await.unwrap();
        // Arithmetic, so the echo of the typed line cannot satisfy the wait.
        ws.send(WsMsg::binary(b"echo hi-$((40+2))\r".to_vec())).await.unwrap();
        read_until(&mut ws, "hi-42").await;
    }

    #[tokio::test]
    async fn resize_reaches_the_tmux_client() {
        if !have_tmux() {
            return;
        }
        let s = Session::new("resize");
        let b = serve(cfg_for(tempdir("resize"))).await.unwrap();
        let mut ws = connect(b.addr, &format!("name={}&cols=60&rows=20", s.0), None).await.unwrap();
        eventually("initial size", || s.clients() == "60x20").await;
        ws.send(WsMsg::text(r#"{"resize":[100,30]}"#)).await.unwrap();
        eventually("resized", || s.clients() == "100x30").await;
    }

    #[tokio::test]
    async fn closing_the_socket_detaches_only_that_client() {
        if !have_tmux() {
            return;
        }
        let s = Session::new("close");
        let b = serve(cfg_for(tempdir("close"))).await.unwrap();
        let mut ws = connect(b.addr, &format!("name={}&cols=80&rows=24", s.0), None).await.unwrap();
        eventually("attached", || !s.clients().is_empty()).await;
        ws.close(None).await.unwrap();
        eventually("client gone", || s.clients().is_empty()).await;
        assert!(tmux(&["has-session", "-t", &format!("={}", s.0)]).status.success());
    }

    #[tokio::test]
    async fn an_unclean_drop_detaches_the_client() {
        // Review focus 4: a phone that sleeps sends no close frame.
        if !have_tmux() {
            return;
        }
        let s = Session::new("drop");
        let b = serve(cfg_for(tempdir("drop"))).await.unwrap();
        let ws = connect(b.addr, &format!("name={}&cols=80&rows=24", s.0), None).await.unwrap();
        eventually("attached", || !s.clients().is_empty()).await;
        drop(ws); // TCP closed, no WebSocket close frame
        eventually("client gone", || s.clients().is_empty()).await;
        assert!(tmux(&["has-session", "-t", &format!("={}", s.0)]).status.success());
    }

    #[tokio::test]
    async fn a_client_that_stops_answering_is_detached() {
        // Review focus 4: a phone that loses its network sends no FIN, so
        // only the missing pong reveals it. This client never reads, so it
        // never answers the bridge's ping.
        if !have_tmux() {
            return;
        }
        let s = Session::new("silent");
        let b = serve(cfg_for(tempdir("silent"))).await.unwrap();
        let _ws = connect(b.addr, &format!("name={}&cols=80&rows=24", s.0), None).await.unwrap();
        eventually("attached", || !s.clients().is_empty()).await;
        eventually("client gone", || s.clients().is_empty()).await;
        assert!(tmux(&["has-session", "-t", &format!("={}", s.0)]).status.success());
    }

    #[tokio::test]
    async fn killing_the_session_closes_with_4000() {
        if !have_tmux() {
            return;
        }
        let s = Session::new("kill");
        let b = serve(cfg_for(tempdir("kill"))).await.unwrap();
        let mut ws = connect(b.addr, &format!("name={}&cols=80&rows=24", s.0), None).await.unwrap();
        eventually("attached", || !s.clients().is_empty()).await;
        tmux(&["kill-session", "-t", &format!("={}", s.0)]);
        assert_eq!(close_code(&mut ws).await, 4000);
    }

    #[tokio::test]
    async fn detaching_from_tmux_closes_with_4001_and_keeps_the_session() {
        if !have_tmux() {
            return;
        }
        let s = Session::new("detach");
        let b = serve(cfg_for(tempdir("detach"))).await.unwrap();
        let mut ws = connect(b.addr, &format!("name={}&cols=80&rows=24", s.0), None).await.unwrap();
        eventually("attached", || !s.clients().is_empty()).await;
        tmux(&["detach-client", "-s", &format!("={}", s.0)]);
        assert_eq!(close_code(&mut ws).await, 4001);
        assert!(tmux(&["has-session", "-t", &format!("={}", s.0)]).status.success());
    }

    #[tokio::test]
    async fn a_prefix_name_does_not_attach_a_longer_session() {
        // Review focus 6: plain `-t cdash-x` would match `cdash-xy`.
        if !have_tmux() {
            return;
        }
        let s = Session::new("prefixlonger");
        let prefix = s.0.trim_end_matches("longer").to_string();
        let b = serve(cfg_for(tempdir("prefix"))).await.unwrap();
        let mut ws = connect(b.addr, &format!("name={prefix}&cols=80&rows=24"), None).await.unwrap();
        assert_eq!(close_code(&mut ws).await, 4000);
        assert!(s.clients().is_empty(), "nothing attached to {}", s.0);
    }
```

- [ ] **Step 3: Run to see it fail**

Run: `cargo test -p cdash-agent collect::term`
Expected: compile errors for `attach_command`, `write_all`, and (once those exist) 404s from the missing route.

- [ ] **Step 4: Implement the bridge**

Add to `term.rs`, below `parse_resize` and above `mod tests`. Extend the `use` block at the top to:

```rust
use super::ctx::Ctx;
use super::validate::BadRequest;
use axum::extract::ws::{CloseFrame, Message, WebSocket};
use rustix::fd::{AsFd, BorrowedFd, OwnedFd};
use serde::Deserialize;
use std::io;
use std::sync::Arc;
use tokio::io::unix::AsyncFd;
```

Code:

```rust
/// A WebSocket close reason may carry at most 123 bytes.
fn close_reason(s: &str) -> String {
    let mut end = s.len().min(123);
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    s[..end].to_string()
}

fn set_size(fd: impl AsFd, cols: u16, rows: u16) -> io::Result<()> {
    let ws = rustix::termios::Winsize { ws_col: cols, ws_row: rows, ws_xpixel: 0, ws_ypixel: 0 };
    Ok(rustix::termios::tcsetwinsize(fd, ws)?)
}

/// A PTY pair. The master is non-blocking (for `AsyncFd`); `openpt` has no
/// flag for that, so it is set afterwards. The size is set before anything
/// attaches, so tmux draws its first frame at the phone's width.
fn open_pty(cols: u16, rows: u16) -> io::Result<(OwnedFd, OwnedFd)> {
    use rustix::fs::{Mode, OFlags};
    use rustix::pty::{grantpt, openpt, ptsname, unlockpt, OpenptFlags};
    let master = openpt(OpenptFlags::RDWR | OpenptFlags::NOCTTY | OpenptFlags::CLOEXEC)?;
    grantpt(&master)?;
    unlockpt(&master)?;
    let path = ptsname(&master, Vec::new())?;
    let slave = rustix::fs::open(path.as_c_str(), OFlags::RDWR | OFlags::NOCTTY | OFlags::CLOEXEC, Mode::empty())?;
    set_size(&master, cols, rows)?;
    rustix::fs::fcntl_setfl(&master, OFlags::NONBLOCK)?;
    Ok((master, slave))
}

/// `tmux attach` as the bridge runs it. `-u` because the agent's environment
/// may carry no UTF-8 locale, and the tmux client would then draw every box
/// character as `_`. `TMUX` stays: it names the server `new-session` used, and
/// tmux refuses to nest only when the client's tty is one of its pane ttys,
/// which this fresh PTY never is. `=name` is an exact match, where a bare name
/// would also match a longer session it prefixes.
#[allow(clippy::disallowed_types)] // a PTY child, which Runner (piped output, timeout) cannot express
fn attach_command(name: &str, path: &str) -> tokio::process::Command {
    let mut cmd = tokio::process::Command::new("tmux");
    cmd.args(["-u", "attach-session", "-t", &format!("={name}")])
        .env("PATH", path)
        .env("TERM", "xterm-256color")
        .kill_on_drop(true);
    cmd
}

fn spawn_attach(name: &str, path: &str, slave: OwnedFd) -> io::Result<tokio::process::Child> {
    let mut cmd = attach_command(name, path);
    cmd.stdin(std::process::Stdio::from(slave.try_clone()?))
        .stdout(std::process::Stdio::from(slave.try_clone()?))
        .stderr(std::process::Stdio::from(slave));
    // SAFETY: runs in the forked child before exec, after stdio is
    // redirected. setsid and TIOCSCTTY are single syscalls with no allocation,
    // which is all a post-fork closure may do. Together they make the PTY
    // (now fd 0) the child's controlling terminal, so tmux gets SIGWINCH.
    unsafe {
        cmd.pre_exec(|| {
            rustix::process::setsid()?;
            rustix::process::ioctl_tiocsctty(BorrowedFd::borrow_raw(0))?;
            Ok(())
        });
    }
    cmd.spawn()
    // `cmd` drops here, closing the parent's copies of the slave, so the
    // master reads EIO once tmux exits.
}

/// Write every byte, however the non-blocking fd splits it.
async fn write_all(fd: &AsyncFd<OwnedFd>, mut b: &[u8]) -> io::Result<()> {
    while !b.is_empty() {
        let mut g = fd.writable().await?;
        match g.try_io(|f| Ok(rustix::io::write(f.get_ref(), b)?)) {
            Ok(Ok(n)) => b = &b[n..],
            Ok(Err(e)) => return Err(e),
            Err(_would_block) => continue,
        }
    }
    Ok(())
}

/// How often the bridge pings the browser. A phone that loses its network
/// sends no FIN, so without this `recv` waits forever and the `tmux attach`
/// child leaks; a peer that answers nothing for one full interval is gone.
/// Short under test so a silent client is caught within the test's patience.
const PING_EVERY: std::time::Duration =
    std::time::Duration::from_secs(if cfg!(test) { 2 } else { 30 });

enum Ended {
    /// The browser went away; nothing left to tell it.
    Client,
    /// `tmux attach` exited: the session died or the user detached.
    Tmux,
}

async fn pump(ctx: &Ctx, name: &str, cols: u16, rows: u16, ws: &mut WebSocket) -> io::Result<Ended> {
    let (master, slave) = open_pty(cols, rows)?;
    let _child = spawn_attach(name, &ctx.host.path, slave)?; // killed on drop
    let master = AsyncFd::new(master)?;
    let mut buf = vec![0u8; 16 * 1024];
    let mut ping = tokio::time::interval(PING_EVERY);
    // Burst (the default) would fire missed ticks back to back after a stall
    // (a slow send on a bad mobile link), and the second would see `heard`
    // still false and cut off a live client.
    ping.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut heard = true;
    loop {
        tokio::select! {
            _ = ping.tick() => {
                if !std::mem::take(&mut heard) {
                    return Ok(Ended::Client);
                }
                if ws.send(Message::Ping(axum::body::Bytes::new())).await.is_err() {
                    return Ok(Ended::Client);
                }
            }
            r = master.readable() => {
                let mut g = r?;
                match g.try_io(|f| Ok(rustix::io::read(f.get_ref(), &mut buf[..])?)) {
                    Ok(Ok(0)) => return Ok(Ended::Tmux),
                    Ok(Ok(n)) => {
                        let frame = Message::Binary(axum::body::Bytes::copy_from_slice(&buf[..n]));
                        if ws.send(frame).await.is_err() {
                            return Ok(Ended::Client);
                        }
                    }
                    // Linux reports a PTY whose last slave closed as EIO.
                    Ok(Err(e)) if e.raw_os_error() == Some(rustix::io::Errno::IO.raw_os_error()) => {
                        return Ok(Ended::Tmux)
                    }
                    Ok(Err(e)) => return Err(e),
                    Err(_would_block) => {}
                }
            }
            msg = ws.recv() => {
                heard = true;
                match msg {
                    Some(Ok(Message::Binary(b))) => write_all(&master, &b).await?,
                    Some(Ok(Message::Text(t))) => {
                        if let Some((c, r)) = parse_resize(t.as_str()) {
                            set_size(master.get_ref(), c, r)?;
                        }
                    }
                    Some(Ok(Message::Close(_))) | Some(Err(_)) | None => return Ok(Ended::Client),
                    Some(Ok(_)) => {} // ping/pong: axum answers pings itself
                }
            },
        }
    }
}

/// Run one terminal connection to the end, then tell the browser why it ended.
pub async fn bridge(ctx: Arc<Ctx>, name: String, cols: u16, rows: u16, mut ws: WebSocket) {
    ctx.host.log.push(format!("term open {name}"));
    let close = match pump(&ctx, &name, cols, rows, &mut ws).await {
        Ok(Ended::Client) => None,
        Ok(Ended::Tmux) => {
            let target = format!("={name}");
            let alive = ctx
                .runner
                .run_checked("tmux", &["has-session", "-t", &target], "tmux has-session")
                .await
                .is_ok();
            Some(if alive { (4001, "detached".to_string()) } else { (4000, "session ended".to_string()) })
        }
        Err(e) => {
            ctx.host.log.push(format!("term failed {name}: {e}"));
            Some((1011, close_reason(&e.to_string())))
        }
    };
    if let Some((code, reason)) = close {
        let _ = ws.send(Message::Close(Some(CloseFrame { code, reason: reason.into() }))).await;
        // Wait for the browser's Close reply. Dropping the socket with a frame
        // still unread (a pong, a keystroke) makes the kernel answer RST, and
        // the browser can then lose the close code and see a plain 1006.
        let drain = async { while let Some(Ok(_)) = ws.recv().await {} };
        let _ = tokio::time::timeout(std::time::Duration::from_secs(1), drain).await;
    }
    ctx.host.log.push(format!("term close {name}"));
}
```

Note: if `Winsize` cannot be built with a struct literal on the rustix version in `Cargo.lock` (it is a libc/linux-raw-sys re-export), use `let mut ws: Winsize = unsafe { std::mem::zeroed() }; ws.ws_col = cols; ws.ws_row = rows;` with a `// SAFETY: plain-integer C struct` comment.

- [ ] **Step 5: Add the route**

`crates/agent/src/http/routes.rs`, next to the other imports near line 104:

```rust
use crate::collect::term::{bridge, origin_allowed, parse_size};
use crate::collect::validate::assert_kill_name;
```

Handler, after `post_kill`:

```rust
/// The session's real TUI over a WebSocket. Every check runs before the
/// upgrade, so a refused request spawns nothing; the browser sees only a
/// failed handshake, so each refusal is also logged.
pub async fn get_term(
    State(ctx): State<Arc<Ctx>>,
    Query(q): Query<HashMap<String, String>>,
    headers: axum::http::HeaderMap,
    ws: axum::extract::ws::WebSocketUpgrade,
) -> Result<Response, ApiError> {
    let h = |k: axum::http::HeaderName| headers.get(k).and_then(|v| v.to_str().ok());
    let origin = h(axum::http::header::ORIGIN);
    let public = std::env::var("CDASH_PUBLIC_URL").ok();
    if !origin_allowed(origin, h(axum::http::header::HOST), public.as_deref()) {
        ctx.host.log.push(format!("term refused: origin {origin:?}"));
        return Err(ApiError { status: StatusCode::FORBIDDEN, message: "origin not allowed".into() });
    }
    let name = q.get("name").cloned().unwrap_or_default();
    let checked = assert_kill_name(&name)
        .and_then(|()| parse_size(q.get("cols").map(String::as_str), q.get("rows").map(String::as_str)));
    let (cols, rows) = match checked {
        Ok(size) => size,
        Err(e) => {
            ctx.host.log.push(format!("term refused: {}", e.0));
            return Err(e.into());
        }
    };
    Ok(ws.on_upgrade(move |socket| bridge(ctx, name, cols, rows, socket)))
}
```

`crates/agent/src/http/serve.rs`, in `guarded_routes!` after `get "/api/hostinfo"`:

```rust
    get "/api/term" => routes::get_term,
```

- [ ] **Step 6: Run the tests**

Run: `cargo test -p cdash-agent collect::term -- --test-threads=4`
Expected: all PASS (with tmux installed; the live tests print "skipping" without it).

Then: `cargo test --all --locked && cargo clippy --all-targets --locked -- -D warnings -D clippy::disallowed_types`
Expected: all PASS, no warnings. `every_guarded_route_is_listed_for_the_bypass_test` still passes because the route went through the macro.

- [ ] **Step 7: Commit**

```bash
git add crates/agent/Cargo.toml Cargo.lock crates/agent/src .github/workflows/ci.yml
git commit -m "feat(term): GET /api/term bridges tmux attach in a PTY to a WebSocket

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_01M311G6RRBCf9gUczbjT9kc"
```

---

### Task 5: Tauri `agent_base` command

**Files:**
- Modify: `crates/tauri-app/src/lib.rs` (new command after `api_request` near line 307; register in `generate_handler!` near line 594)

**Interfaces:**
- Consumes: existing `addr_locked`, `open_store`, `active_record`, `base_url`, `ServerState`.
- Produces: JS `invoke('agent_base')` resolves to the agent's base URL string, e.g. `"http://localhost:23274"`, or rejects with `"server not running"`. Task 7 uses it.

- [ ] **Step 1: Add the command**

After `api_request`:

```rust
/// The agent's base URL, for the one connection `api_request` cannot carry:
/// the terminal's WebSocket, which the webview opens itself.
#[tauri::command]
fn agent_base(app: tauri::AppHandle, state: tauri::State<'_, ServerState>) -> Result<String, String> {
    let inproc = addr_locked(&state.0)?;
    let active = open_store(&app).ok().and_then(|s| active_record(&s));
    base_url(inproc, active.as_ref()).ok_or_else(|| "server not running".to_string())
}
```

Add `agent_base,` to the `generate_handler!` list after `api_request,`.

- [ ] **Step 2: Build and test**

Run: `cargo test -p cdash-tauri`
Expected: builds, existing tests PASS. No new unit test: `base_url` is already tested and this only composes it.

- [ ] **Step 3: Commit**

```bash
git add crates/tauri-app/src/lib.rs
git commit -m "feat(tauri): agent_base command for the terminal WebSocket

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_01M311G6RRBCf9gUczbjT9kc"
```

---

### Task 6: Vendored xterm.js and the key module

**Files:**
- Create: `public/vendor/xterm.js`, `public/vendor/xterm.css`, `public/vendor/addon-fit.js`, `public/vendor/LICENSE-xterm`, `public/vendor/README.md`
- Create: `public/term/keys.js`
- Create: `test/keys.test.mjs`
- Modify: `crates/agent/src/http/serve.rs` (`embedded_asset` arms, test)

**Interfaces:**
- Consumes: nothing.
- Produces (Task 7 relies on these globals once loaded by `<script>`):
  - `window.Terminal` (xterm.js class)
  - `window.FitAddon.FitAddon` (fit addon class)
  - `window.cdashKeys.KEYS`: `{ esc, tab, stab, pgup, pgdn, left, up, down, right, ctrlc, enter }` → byte strings
  - `window.cdashKeys.applyCtrl(data: string) => string`
  - Served paths: `/vendor/xterm.js`, `/vendor/xterm.css`, `/vendor/addon-fit.js`, `/term/keys.js`

- [ ] **Step 1: Vendor xterm.js**

```bash
S=$(mktemp -d) && cd "$S"
npm pack @xterm/xterm@5.5.0 @xterm/addon-fit@0.10.0
mkdir x f && tar -xzf xterm-xterm-5.5.0.tgz -C x && tar -xzf xterm-addon-fit-0.10.0.tgz -C f
cd /mnt/d/git/claude-dashboard && mkdir -p public/vendor
cp "$S/x/package/lib/xterm.js" public/vendor/xterm.js
cp "$S/x/package/css/xterm.css" public/vendor/xterm.css
cp "$S/f/package/lib/addon-fit.js" public/vendor/addon-fit.js
cp "$S/x/package/LICENSE" public/vendor/LICENSE-xterm
grep -c "sourceMappingURL" public/vendor/*.js
```

Remove any trailing `//# sourceMappingURL=…` line from both `.js` files (the maps are not served). Verify the UMD globals:

```bash
head -c 300 public/vendor/xterm.js; echo; head -c 300 public/vendor/addon-fit.js
```

Expected: both start with a UMD wrapper (`!function(e,t){"object"==typeof exports...`) assigning `Terminal` and `FitAddon` on the global when no module system is present.

Create `public/vendor/README.md`:

```markdown
Vendored, unmodified except for removed `sourceMappingURL` lines. MIT, see LICENSE-xterm
(addon-fit ships under the same license from the same project).

- xterm.js, xterm.css: @xterm/xterm 5.5.0 (lib/xterm.js, css/xterm.css)
- addon-fit.js: @xterm/addon-fit 0.10.0 (lib/addon-fit.js)

Upgrade: `npm pack` the new versions, copy the same three files, and keep the
`embedded_asset` arms in crates/agent/src/http/serve.rs in step.
```

- [ ] **Step 2: Write the failing key tests**

Create `test/keys.test.mjs`:

```js
import { test } from 'node:test';
import assert from 'node:assert/strict';

await import('../public/term/keys.js');
const k = globalThis.cdashKeys;

test('each key-bar button sends the bytes a terminal sends for it', () => {
  assert.deepEqual(k.KEYS, {
    esc: '\x1b', tab: '\t', stab: '\x1b[Z', pgup: '\x1b[5~', pgdn: '\x1b[6~',
    left: '\x1b[D', up: '\x1b[A', down: '\x1b[B', right: '\x1b[C',
    ctrlc: '\x03', enter: '\r',
  });
});

test('sticky Ctrl turns one character into its control code', () => {
  assert.equal(k.applyCtrl('a'), '\x01');
  assert.equal(k.applyCtrl('A'), '\x01');
  assert.equal(k.applyCtrl('o'), '\x0f'); // Ctrl+O: transcript mode
  assert.equal(k.applyCtrl('r'), '\x12');
  assert.equal(k.applyCtrl('@'), '\x00');
  assert.equal(k.applyCtrl('['), '\x1b');
  assert.equal(k.applyCtrl('_'), '\x1f');
});

test('sticky Ctrl passes through what has no control code', () => {
  assert.equal(k.applyCtrl('1'), '1');
  assert.equal(k.applyCtrl('/'), '/');
  assert.equal(k.applyCtrl('`'), '`');
  // An IME commits whole words at once; never mangle those.
  assert.equal(k.applyCtrl('hello'), 'hello');
  assert.equal(k.applyCtrl(''), '');
});
```

Run: `node --test test/keys.test.mjs`
Expected: FAIL, module not found.

- [ ] **Step 3: Implement `public/term/keys.js`**

```js
/* Terminal key-bar bytes and sticky Ctrl, free of DOM so node --test can
   exercise them. Loaded as a classic script by index.html; the IIFE form makes
   the same file a valid side-effect ES module under node --test. */
(function (g) {
  g.cdashKeys = {
    KEYS: {
      esc: '\x1b', tab: '\t', stab: '\x1b[Z', pgup: '\x1b[5~', pgdn: '\x1b[6~',
      left: '\x1b[D', up: '\x1b[A', down: '\x1b[B', right: '\x1b[C',
      ctrlc: '\x03', enter: '\r',
    },
    /* Ctrl+<c> for a single character that has a C0 control code: @, letters,
       [ \ ] ^ _. Anything else, including a multi-character IME commit,
       passes through unchanged. */
    applyCtrl(data) {
      const c = data.length === 1 ? data.charCodeAt(0) : -1;
      const has = (c >= 0x40 && c <= 0x5f) || (c >= 0x61 && c <= 0x7a);
      return has ? String.fromCharCode(c & 0x1f) : data;
    },
  };
})(typeof globalThis === 'undefined' ? this : globalThis);
```

Run: `node --test test/keys.test.mjs`
Expected: 3 PASS.

- [ ] **Step 4: Serve the new assets (failing test first)**

In `crates/agent/src/http/routes.rs` tests, next to `embedded_ui_is_served`:

```rust
    #[tokio::test]
    async fn terminal_assets_are_embedded() {
        let b = serve(cfg_for(tempdir("term-assets"))).await.unwrap();
        for (path, needle) in [
            ("/vendor/xterm.js", "Terminal"),
            ("/vendor/addon-fit.js", "FitAddon"),
            ("/vendor/xterm.css", ".xterm"),
            ("/term/keys.js", "cdashKeys"),
        ] {
            let body = reqwest_get(&format!("http://{}{path}", b.addr)).await;
            assert!(body.contains(needle), "{path} is served");
        }
    }
```

Run: `cargo test -p cdash-agent terminal_assets_are_embedded`
Expected: FAIL, 404.

Add arms to `embedded_asset` in `serve.rs`, after `"transport/backoff.js"`:

```rust
        "term/keys.js" => (
            "text/javascript; charset=utf-8",
            include_bytes!("../../../../public/term/keys.js"),
        ),
        "vendor/xterm.js" => (
            "text/javascript; charset=utf-8",
            include_bytes!("../../../../public/vendor/xterm.js"),
        ),
        "vendor/addon-fit.js" => (
            "text/javascript; charset=utf-8",
            include_bytes!("../../../../public/vendor/addon-fit.js"),
        ),
        "vendor/xterm.css" => (
            "text/css; charset=utf-8",
            include_bytes!("../../../../public/vendor/xterm.css"),
        ),
```

Run: `cargo test -p cdash-agent terminal_assets_are_embedded && node --test`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add public/vendor public/term test/keys.test.mjs crates/agent/src/http
git commit -m "feat(ui): vendor xterm.js and add the terminal key module

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_01M311G6RRBCf9gUczbjT9kc"
```

---

### Task 7: Terminal button and overlay

**Files:**
- Modify: `public/index.html`
- Modify: `public/app.js`
- Modify: `public/style.css`
- Modify: `test/frontend-guards.test.mjs`

**Interfaces:**
- Consumes: `GET /api/term` contract (Task 4); `invoke('agent_base')` (Task 5); `window.Terminal`, `window.FitAddon.FitAddon`, `window.cdashKeys` (Task 6); existing `$`, `esc`, `isTauri`, `invoke`, `poll`, `ICONS`, `svg` in `app.js`.
- Produces: user-facing feature. Nothing downstream.

- [ ] **Step 1: Write the failing guard test**

Append to `test/frontend-guards.test.mjs`:

```js
test('the Terminal button exists only for tmux-owned sessions', () => {
  // External sessions live in another terminal app's PTY; nothing can attach.
  const app = readFileSync(new URL('../public/app.js', import.meta.url), 'utf8');
  const m = app.match(/const termBtn = r\.external \? '' *\n?\s*: `<button[^`]*data-term=/);
  assert.ok(m, 'termBtn is empty for external sessions and carries data-term otherwise');
});

test('terminal scripts load before app.js', () => {
  const html = readFileSync(new URL('../public/index.html', import.meta.url), 'utf8');
  const at = s => html.indexOf(`<script src="${s}"></script>`);
  for (const s of ['vendor/xterm.js', 'vendor/addon-fit.js', 'term/keys.js']) {
    assert.ok(at(s) !== -1 && at(s) < at('app.js'), `${s} loads before app.js`);
  }
  assert.ok(html.includes('interactive-widget=resizes-content'), 'the IME shrinks the layout');
});
```

Run: `node --test test/frontend-guards.test.mjs`
Expected: both new tests FAIL.

- [ ] **Step 2: `index.html`**

Viewport meta (line 5) becomes:

```html
<meta name="viewport" content="width=device-width, initial-scale=1, viewport-fit=cover, interactive-widget=resizes-content">
```

After `<link rel="stylesheet" href="style.css">`:

```html
<link rel="stylesheet" href="vendor/xterm.css">
```

Before `<div id="toast" …>`:

```html
<div id="term" class="term" hidden>
  <div class="term-bar">
    <span id="term-title" class="term-title"></span>
    <span id="term-status" class="term-status" role="status" aria-live="polite"></span>
    <button id="term-reconnect" class="term-reconnect" type="button" hidden>Reconnect</button>
    <button id="term-close" class="icon-btn" type="button" aria-label="Close terminal"></button>
  </div>
  <div id="term-screen" class="term-screen"></div>
  <div id="term-keys" class="term-keys">
    <button type="button" data-key="esc">Esc</button>
    <button type="button" data-key="tab">Tab</button>
    <button type="button" data-key="stab" aria-label="Shift Tab">⇧Tab</button>
    <button type="button" data-key="ctrl" aria-pressed="false">Ctrl</button>
    <button type="button" data-key="pgup">PgUp</button>
    <button type="button" data-key="pgdn">PgDn</button>
    <button type="button" data-key="ctrlc" aria-label="Control C">^C</button>
    <button type="button" data-key="left" aria-label="Left">←</button>
    <button type="button" data-key="up" aria-label="Up">↑</button>
    <button type="button" data-key="down" aria-label="Down">↓</button>
    <button type="button" data-key="right" aria-label="Right">→</button>
    <button type="button" data-key="enter" aria-label="Enter">⏎</button>
  </div>
</div>
```

Scripts, replacing the last two lines before `</body>`:

```html
<script src="transport/backoff.js"></script>
<script src="vendor/xterm.js"></script>
<script src="vendor/addon-fit.js"></script>
<script src="term/keys.js"></script>
<script src="app.js"></script>
```

- [ ] **Step 3: `app.js`: button on the card**

Add to `ICONS`:

```js
  terminal: svg('<path d="M4 17l6-5-6-5"/><path d="M12 19h8"/>'),
```

In `runningCard`, after the `kill` declaration:

```js
  // The session's real TUI. tmux-owned panes only: an external session lives
  // in another terminal app's PTY, which nothing can attach to.
  const termBtn = r.external ? ''
    : `<button class="action" type="button" data-term="${esc(r.name)}" data-title="${esc(r.dir.split('/').pop())}">${ICONS.terminal}<span>Terminal</span></button>`;
```

and the actions row becomes:

```js
      <div class="actions">${rc}${termBtn}${kill}</div>
```

In the body click delegate, put this before the existing `closest('[data-kill],…')` line:

```js
  const t = e.target.closest('[data-term]');
  if (t) { openTerm(t.dataset.term, t.dataset.title); return; }
```

- [ ] **Step 4: `app.js`: the overlay**

Add this block after the click delegate:

```js
/* ---------- Terminal: the session's real TUI over a WebSocket ----------
   The agent runs `tmux attach` in a PTY and pipes it here. tmux owns the
   history, so xterm keeps no scrollback; PgUp/PgDn scroll inside Claude Code's
   fullscreen renderer instead. */
let term = null; // { name, xterm, fit, ro, ws, ctrl, retry } while the overlay is open

async function termUrl(name, cols, rows) {
  const q = `/api/term?name=${encodeURIComponent(name)}&cols=${cols}&rows=${rows}`;
  // The webview cannot proxy a WebSocket through invoke, so it dials the agent.
  if (isTauri) return (await invoke('agent_base')).replace(/^http/, 'ws') + q;
  return `${location.protocol === 'https:' ? 'wss' : 'ws'}://${location.host}${q}`;
}

function termStatus(text, reconnect) {
  $('#term-status').textContent = text;
  $('#term-reconnect').hidden = !reconnect;
}

function termSend(data) {
  if (term?.ws?.readyState === WebSocket.OPEN) term.ws.send(new TextEncoder().encode(data));
}

async function termConnect() {
  const t = term;
  t.retry = false;
  t.xterm.reset();
  termStatus('Connecting…', false);
  let url;
  try { url = await termUrl(t.name, t.xterm.cols, t.xterm.rows); }
  catch (e) { termStatus(String(e?.message ?? e), true); return; }
  if (term !== t) return; // closed while resolving the URL
  const ws = new WebSocket(url);
  ws.binaryType = 'arraybuffer';
  t.ws = ws;
  let opened = false;
  ws.onopen = () => {
    opened = true;
    termStatus('', false);
    // onResize drops sizes while connecting, and the keyboard that focus()
    // opens usually lands then; send the size the screen has now.
    ws.send(JSON.stringify({ resize: [t.xterm.cols, t.xterm.rows] }));
  };
  ws.onmessage = e => { if (typeof e.data !== 'string') t.xterm.write(new Uint8Array(e.data)); };
  ws.onclose = e => {
    if (term !== t || t.ws !== ws) return; // the UI closed it, or a newer socket replaced it
    t.ws = null;
    if (e.code === 4000) return termStatus('Session ended', false);
    if (e.code === 4001) return termStatus('Disconnected', true);
    // Unexpected: network, agent restart, phone sleep. One automatic retry
    // when the page is next visible; the button covers the rest.
    termStatus(e.reason || (opened ? 'Disconnected' : 'Couldn’t connect'), true);
    t.retry = true;
  };
}

function setCtrl(on) {
  term.ctrl = on;
  const b = $('#term-keys [data-key="ctrl"]');
  b.classList.toggle('on', on);
  b.setAttribute('aria-pressed', String(on));
}

function openTerm(name, title) {
  if (term) return;
  $('#term-title').textContent = title;
  $('#term').hidden = false;
  const xterm = new Terminal({
    fontFamily: getComputedStyle(document.documentElement).getPropertyValue('--font-mono'),
    fontSize: 13,
    cursorBlink: true,
    scrollback: 0,
    // Dark regardless of the dashboard theme: Claude Code's own colors assume one.
    theme: { background: '#1a1917', foreground: '#f4f2ec' },
  });
  const fit = new FitAddon.FitAddon();
  xterm.loadAddon(fit);
  xterm.open($('#term-screen'));
  // The hidden textarea is what the phone keyboard types into.
  for (const [k, v] of [['autocapitalize', 'off'], ['autocorrect', 'off'], ['autocomplete', 'off'], ['spellcheck', 'false']])
    xterm.textarea.setAttribute(k, v);
  fit.fit();
  term = { name, xterm, fit, ro: null, ws: null, ctrl: false, retry: false };
  xterm.onData(d => {
    if (term.ctrl) { d = cdashKeys.applyCtrl(d); setCtrl(false); }
    termSend(d);
  });
  xterm.onResize(({ cols, rows }) => {
    if (term.ws?.readyState === WebSocket.OPEN) term.ws.send(JSON.stringify({ resize: [cols, rows] }));
  });
  // Rotation and the keyboard opening both resize the container.
  term.ro = new ResizeObserver(() => fit.fit());
  term.ro.observe($('#term-screen'));
  history.pushState({ term: name }, ''); // Android back closes the overlay
  termConnect();
  xterm.focus();
}

function closeTerm() {
  if (!term) return;
  const t = term;
  term = null;
  t.ro.disconnect();
  t.ws?.close();
  t.xterm.dispose();
  $('#term').hidden = true;
  poll(); // the poll paused while the overlay was open
}

$('#term-close').innerHTML = ICONS.x;
$('#term-close').addEventListener('click', () => history.back()); // popstate closes it
addEventListener('popstate', closeTerm);
$('#term-reconnect').addEventListener('click', () => { if (term && !term.ws) termConnect(); });
// Act on pointerdown and cancel it, so a tap neither moves focus nor closes
// the phone keyboard; mousedown is cancelled too for browsers that still
// focus on it.
$('#term-keys').addEventListener('mousedown', e => e.preventDefault());
$('#term-keys').addEventListener('pointerdown', e => {
  const b = e.target.closest('[data-key]');
  if (!b || !term) return;
  e.preventDefault();
  const k = b.dataset.key;
  if (k === 'ctrl') return setCtrl(!term.ctrl);
  if (term.ctrl) setCtrl(false); // Ctrl applies to the next typed character only
  termSend(cdashKeys.KEYS[k]);
  if (document.activeElement !== term.xterm.textarea) term.xterm.focus();
});
document.addEventListener('visibilitychange', () => {
  if (!document.hidden && term?.retry && !term.ws) termConnect();
});
```

Pause the poll while the overlay is open. At the very top of `tick()` (the function containing `if (document.hidden && polled) { arm(); return; }`), add:

```js
  if (term) return; // paused under the terminal; closeTerm() restarts it
```

- [ ] **Step 5: `style.css`**

Append:

```css
/* ---------- Terminal overlay ---------- */
.term {
  position: fixed; inset: 0; z-index: 50;
  display: flex; flex-direction: column;
  height: 100dvh;
  background: #1a1917; color: #f4f2ec;
  padding-top: env(safe-area-inset-top);
}
.term[hidden] { display: none; }
.term-bar { display: flex; align-items: center; gap: 10px; padding: 6px 8px 6px 14px; }
.term-title { font-weight: 600; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
.term-status { flex: 1; color: #b0aea5; font-size: 13px; white-space: nowrap; overflow: hidden; text-overflow: ellipsis; }
.term-reconnect {
  padding: 6px 12px; border-radius: var(--r-pill); border: 1px solid #47443f;
  background: transparent; color: inherit; font: inherit; font-size: 13px;
}
.term .icon-btn { color: #b0aea5; }
.term-screen { flex: 1; min-height: 0; padding: 0 4px; }
.term-keys {
  display: grid; grid-template-columns: repeat(6, 1fr); gap: 6px;
  padding: 6px 6px calc(6px + env(safe-area-inset-bottom));
  background: #201f1d; border-top: 1px solid #383632;
  touch-action: manipulation; user-select: none; -webkit-user-select: none;
}
.term-keys button {
  min-height: 40px; border-radius: var(--r-sm); border: 1px solid #383632;
  background: #262523; color: #f4f2ec; font: 500 14px var(--font-mono);
}
.term-keys button:active { background: #302e2b; }
.term-keys button.on { background: var(--accent); border-color: transparent; color: #1a1917; }
```

- [ ] **Step 6: Run the tests**

Run: `node --test && cargo test -p cdash-agent embedded_ui_is_served`
Expected: all PASS.

- [ ] **Step 7: Smoke-test in a desktop browser**

```bash
tmux new-session -d -s cdash-smoke-1 sh
cargo run -p cdash-agent
```

Open `http://localhost:23274`, then in DevTools device mode (a phone profile):
- The `cdash-smoke-1` card shows a Terminal button; tap it. The overlay opens with a shell prompt sized to the screen.
- Type `ls`, tap ⏎. Output appears.
- Tap `Ctrl` (it highlights), type `l`. The screen clears (Ctrl+L) and `Ctrl` un-highlights.
- Resize the device frame. The prompt reflows.
- Press the browser Back button. The overlay closes and the cards refresh.
- From another shell: `tmux kill-session -t cdash-smoke-1` while the overlay is open. Status shows "Session ended" with no Reconnect button.

Stop the agent afterwards.

- [ ] **Step 8: Commit**

```bash
git add public test
git commit -m "feat(ui): Terminal button opens the session's TUI with a mobile key bar

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_01M311G6RRBCf9gUczbjT9kc"
```

---

### Task 8: README and on-phone verification

**Files:**
- Modify: `README.md:7` (the send-bar paragraph) and the `cloudflared` section near line 217-230

**Interfaces:**
- Consumes: the finished feature.
- Produces: docs.

- [ ] **Step 1: Replace the send-bar paragraph (README line 7)**

```markdown
Each running session card has a **Terminal** button that opens that session's real Claude Code TUI, the way an SSH client would: arrow through `/mcp` menus, toggle modes with Shift+Tab, interrupt with Esc. The agent runs `tmux attach` in a PTY and streams it over a WebSocket (`GET /api/term`). Letters, digits and symbols come from the phone's own keyboard; a key bar adds Esc, Tab, Shift+Tab, a sticky Ctrl (tap Ctrl, then a letter), PgUp/PgDn, Ctrl+C, the arrows and Enter. Closing the terminal only detaches; the session keeps running. Dashboard sessions start in Claude Code's fullscreen renderer so PgUp/PgDn scroll the conversation; `CLAUDE_CODE_DISABLE_ALTERNATE_SCREEN` or `CLAUDE_CODE_NO_FLICKER=0` in the agent's environment overrides that, and then PgUp/PgDn do nothing (use Ctrl, then `o`, for transcript mode). The terminal is command execution for anyone who can reach the dashboard, so it sits behind the same `CDASH_AUTH` guards, and it refuses WebSocket upgrades from any page whose `Origin` is not the dashboard itself (or `CDASH_PUBLIC_URL`, or the desktop/Android app). The log panel records when a terminal opens and closes, never what is typed. External sessions aren't in tmux, so they don't get the button.
```

- [ ] **Step 2: Confirm and document `cloudflared` Host forwarding**

Check Cloudflare's current docs for `cloudflared` ingress `originRequest.httpHostHeader` and whether the original `Host` reaches the origin by default. Add one sentence to the `cloudflared` section (near README line 223) stating the result, in this form:

```markdown
The terminal's Origin check compares the browser's `Origin` with the `Host` the agent receives. `cloudflared` <forwards / rewrites> the original `Host` by default, so <nothing more is needed / set `CDASH_PUBLIC_URL` to the public `https://` address>.
```

Fill in the two choices with what the docs say. If the docs are unclear, test it: run the tunnel, open the terminal from the public URL, and check `/api/logs` for a `term refused: origin` line.

- [ ] **Step 3: On-phone checklist**

Install a build on an Android phone (Tauri app) and also open the dashboard in mobile Chrome. For each:
- Launch a new session from the dashboard, then open Terminal. `/tui` reports fullscreen.
- `/mcp`: arrow to an item, ⏎, Esc out.
- ⇧Tab cycles the permission mode.
- Rotate the phone and open/close the keyboard. The TUI reflows, and the key bar stays above the keyboard. If the key bar hides behind the keyboard in the Tauri webview, stop and report: the spec's `visualViewport` fallback is then needed.
- Tapping key-bar buttons keeps the keyboard open.
- `Ctrl` then `o` opens transcript mode; `q` leaves it.
- PgUp/PgDn scroll the conversation.
- Lock the phone for 30 seconds, unlock. The terminal reconnects once by itself.
- Close the overlay. The session still runs and its card is still there.

Record the results (pass/fail per line) in the PR description.

- [ ] **Step 4: Commit**

```bash
git add README.md
git commit -m "docs: document the terminal and its Origin rule

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_01M311G6RRBCf9gUczbjT9kc"
```
