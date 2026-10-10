# Mobile terminal for dashboard sessions

Date: 2026-10-11
Status: approved design, pending implementation plan

## Goal

From a phone, open a dashboard session's real Claude Code TUI and drive it the
way an SSH client would: arrow through `/mcp` menus, toggle modes with
Shift+Tab, interrupt with Esc or Ctrl+C, scroll back. Letters, digits and
symbols (`/()?!`) come from the phone's own keyboard (IME). Keys the IME lacks
come from an on-screen key bar.

This replaces the single-line send bar (`! gcloud auth login` placeholder),
which can type a command but cannot operate anything that needs arrow keys.

### Success criteria

On a phone, in both the Android Tauri app and a mobile browser:

1. Tap Terminal on a session card. The TUI appears at phone width.
2. Run `/mcp`, arrow to an item, press Enter, press Esc to leave.
3. Rotate the phone and open/close the IME. The TUI reflows each time.
4. Scroll the conversation with PgUp/PgDn.
5. Close the terminal. The session keeps running and the card is still there.

## Scope

In scope:

- Terminal button on every dashboard-owned (tmux, `cdash-*`) session card.
- Full-screen terminal overlay with a two-row key bar.
- `GET /api/term` WebSocket endpoint on the agent, backed by a PTY running
  `tmux attach`.
- Dashboard sessions start in Claude Code's fullscreen renderer.
- Removal of the send bar and `POST /api/keys`.

Out of scope:

- External sessions (Claude started outside the dashboard). Their screen lives
  in another terminal app's PTY, and a second viewer cannot join an existing
  PTY. They get no Terminal button. A "Move to dashboard" action (kill and
  `claude --resume <sid>` inside tmux) may come later as a separate change.
- Swipe-to-scroll, tmux `mouse on`, pinch zoom, font-size controls.
- Auto-reconnect with backoff.
- Auth headers for remote Tauri profiles (see Known limits).

## Decisions

| Question | Decision |
|---|---|
| Mechanism | PTY + `tmux attach` + WebSocket + vendored xterm.js (approach A). Rejected: polling `capture-pane` snapshots (laggy, misses frames, no real cursor); bundling ttyd/gotty (extra binary, extra port, separate auth). |
| Presentation | Full-screen overlay on the dashboard page, not a separate route. |
| Key bar | Esc, Tab, Shift+Tab, sticky Ctrl, PgUp, PgDn, Ctrl+C, arrows, Enter. Two rows. |
| Window size with several clients | tmux default (latest active client wins). |
| Renderer | Every dashboard session starts with `"tui":"fullscreen"`, so PgUp/PgDn scroll inside Claude Code. |

## Design

### 1. Agent: `GET /api/term`

Registered in `guarded_routes!`, so the existing auth guard (password, CF
Access, or none) applies before the handler runs.

Query: `name` (tmux session), `cols`, `rows` (initial size).

Checks, in order, all before anything spawns:

1. Auth: existing guard layer.
2. Origin (new): refuse with 403 unless the `Origin` header is absent, its
   host[:port] equals the `Host` header, it equals the origin of
   `CDASH_PUBLIC_URL` when that is set, or it is a Tauri webview origin
   (`http://tauri.localhost`, `tauri://localhost`). A WebSocket upgrade is not
   subject to CORS, so without this any web page open on the phone could
   connect to `ws://localhost:<port>` and type into a session, which amounts
   to a shell.
   - Compare host[:port] only, never the scheme. Behind a TLS-terminating
     proxy (the README's `cloudflared` deployment) the agent sees plain HTTP
     while the browser sends `Origin: https://…`, so a scheme check refuses
     every browser. The scheme adds no protection: a hostile page must already
     be served from the agent's own host to pass.
   - `CDASH_PUBLIC_URL` covers a proxy that rewrites `Host`. Cloudflare's docs
     do not state whether `cloudflared` forwards the original `Host` by
     default; confirm during implementation and note the result in the README.
3. Name: `assert_kill_name`, the same rule `/api/kill` uses.
4. `cols`/`rows`: integers in 1..=1000, else 400.

New module `collect/term.rs` (`#[cfg(unix)]`; non-unix builds answer 501):

- Open a PTY with `rustix::pty` (`openpt`, `grantpt`, `unlockpt`, `ptsname`).
  Enable rustix's `pty` feature. No new crate. `openpt` has no non-blocking
  flag, so set `O_NONBLOCK` on the master with `fcntl_setfl` before wrapping
  it in `AsyncFd`.
- Set the initial window size with `tcsetwinsize` before spawning.
- Spawn `tmux attach -t =<name>` with tokio `Command`, the PTY slave as
  stdin/stdout/stderr, `TERM=xterm-256color`, `kill_on_drop(true)`. `pre_exec`
  calls `setsid` and `ioctl_tiocsctty` so the slave is the child's
  controlling terminal.
- Pump both directions:
  - PTY master to socket: binary frames.
  - Socket binary frame to PTY master: raw input bytes.
  - Socket text frame `{"resize":[cols,rows]}`: `tcsetwinsize` on the master.
    Any other text frame is ignored.
- Teardown:
  - Socket closes: drop the master and kill the `tmux attach` child. Only
    that client detaches; the tmux session keeps running.
  - `tmux attach` exits by itself: run `tmux has-session -t =<name>`. If the
    session is gone, close the socket with code 4000, reason `session ended`.
    If it still exists (the user detached, e.g. sticky Ctrl, `b`, `d`), close
    with 4001, reason `detached`.
  - Spawn failure: close with code 1011 and the error text as reason.
- Log one line on open and one on close, naming the session. Never log
  terminal content.

Also:

- axum gains the `ws` feature; tokio gains `net` for `AsyncFd` on the
  non-blocking PTY master.
- `POST /api/keys`, `collect/keys.rs` and their tests are deleted.

### 2. Agent: fullscreen renderer for dashboard sessions

`spawn_claude` (`collect/spawn.rs`) already passes a per-session `--settings`
override. It becomes:

```rust
r#"{"tui":"fullscreen","env":{"CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC":""}}"#,
```

- Covers new launches and Resume, which both go through `spawn_claude`.
- `--settings` outranks user and project settings, and the user's
  `~/.claude/settings.json` is not touched.
- Sessions already running keep their renderer until relaunched.
- Checked against Claude Code 2.1.296: `CLAUDE_CODE_DISABLE_ALTERNATE_SCREEN`,
  `CLAUDE_CODE_NO_FLICKER=0`, tmux `-CC` mode and a crash auto-off all beat
  `"tui":"fullscreen"`. A user who sets one gets the inline renderer, and
  PgUp/PgDn do not scroll. Document this in the README; do not unset the
  user's variables.

### 3. Tauri client: `agent_base` command

The webview cannot proxy a WebSocket through `invoke`. A new command
`agent_base` returns the existing `base_url(inproc, active)` result, and JS
swaps `http` for `ws`. Register it in `generate_handler!`.

### 4. UI

**Card.** A Terminal button joins the actions row (`data-term="<name>"`) on
non-external cards only. The send form, its `drafts` and `sending` state, its
input/submit handlers, the render freeze while the send input has focus, and
its CSS are removed.

**Overlay** (`#term`, fixed, full-screen, flex column):

- Top bar: session directory name, connection status, close button.
- Middle: xterm.js container. The fit addon computes cols/rows from it.
- Bottom: key bar.

```
[Esc] [Tab] [⇧Tab] [Ctrl] [PgUp] [PgDn]
[^C]  [ ← ] [ ↑  ] [ ↓  ] [ →  ] [ ⏎  ]
```

- The viewport meta gains `interactive-widget=resizes-content`, so the layout
  shrinks when the IME opens and the key bar sits above it. Fall back to
  `visualViewport` only if Android WebView ignores the meta in testing.
- Tapping the terminal focuses xterm's hidden textarea, which opens the IME.
  The textarea gets `autocapitalize="off"`, `autocorrect="off"`,
  `spellcheck="false"`.

**Keys** (`public/term/keys.js`, pure, no DOM):

| Button | Bytes |
|---|---|
| Esc | `\x1b` |
| Tab | `\t` |
| ⇧Tab | `\x1b[Z` |
| PgUp / PgDn | `\x1b[5~` / `\x1b[6~` |
| ← ↑ ↓ → | `\x1b[D` `\x1b[A` `\x1b[B` `\x1b[C` |
| ^C | `\x03` |
| ⏎ | `\r` |

- Sticky Ctrl: tapping Ctrl arms it (highlighted). The next single character
  from the IME in `a-z`, `A-Z`, `@ [ \ ] ^ _` is sent as `code & 0x1f`; any
  other input passes through unchanged. Either way Ctrl disarms after one
  input. Tapping Ctrl again while armed disarms it.
- Key bar buttons act on `pointerdown` with `preventDefault`, so a tap does
  not move focus and the IME stays open.

**Lifecycle:**

- Open: pause the poll, `history.pushState` (Android back closes the
  overlay), create the terminal, fit, connect with the fitted `cols`/`rows`.
- xterm `onData` goes to the socket as binary (`TextEncoder`). Socket binary
  goes to `term.write`.
- A `ResizeObserver` on the container refits and sends a resize message. This
  covers rotation and the IME opening.
- Close (button or back): close the socket, dispose the terminal, resume the
  poll.
- Unexpected drop (any close other than 4000, 4001 or one the UI started):
  retry once automatically when the page next becomes visible. A 4000 or 4001
  close never retries on its own; only the Reconnect button reconnects.

**Socket URL:**

- Browser/PWA: same origin, `wss:` when the page is `https:`, else `ws:`.
- Tauri: `invoke('agent_base')`, scheme swapped to `ws`.

**Vendor:** pinned UMD builds of xterm.js and the fit addon plus `xterm.css`
in `public/vendor/`. Add each to the `embedded_asset` list in
`http/serve.rs`, and load them with plain `<script>`/`<link>` tags. The
service worker caches them through its existing stale-while-revalidate path.
Note the versions and the MIT license in the vendor directory.

## Error handling

| Case | Agent | UI |
|---|---|---|
| Upgrade refused (401 auth, 403 Origin, 400 name/size) | HTTP error before upgrade, log line | "Couldn't connect". The browser hides the status; `/api/logs` has the reason. |
| PTY or `tmux attach` spawn fails | Close 1011 with reason | Shows the reason |
| Session killed while attached | Close 4000 `session ended` | "Session ended", no reconnect |
| User detaches tmux from the phone | Close 4001 `detached` | "Disconnected · Reconnect" |
| Network drop, agent restart, phone sleep | Socket gone; child killed on EOF | "Disconnected · Reconnect"; retries once automatically when the page becomes visible (unexpected drops only, never after 4000 or 4001) |
| Two phones or tabs on one session | Each gets its own `tmux attach` client | Both work; latest active sets the size |

## Testing

Rust unit:

- Origin rule: absent, same host, `https://<Host>` against a plain-HTTP
  request, the `CDASH_PUBLIC_URL` origin, `http://tauri.localhost` and
  `tauri://localhost` allowed; a foreign origin refused.
- Resize message parsing, size bounds.
- `spawn_claude` passes `"tui":"fullscreen"` (the stub-tmux test already
  records `new-session` args).

Rust integration (real tmux; skip when tmux is absent; CI installs it):

- Start `tmux new-session -d`, connect, send `echo hi\r`, read `hi` back.
- Send a resize; `#{client_width}` matches.
- Close the socket; `tmux list-clients` is empty and the session still exists.
- Kill the session while connected; the socket closes with 4000.
- `tmux detach-client` while connected; the socket closes with 4001 and the
  session still exists.

Node (`node --test`):

- `keys.js`: every button's bytes; sticky Ctrl turns `a` into `\x01` and `[`
  into `\x1b`, passes `1` through, and disarms after one input.
- `frontend-guards`: no send form remains; the Terminal button renders only on
  non-external cards.

CI: add `tmux` to the `apt-get install` list in `.github/workflows/ci.yml`.

Manual, on a phone (Tauri Android and mobile Chrome):

- `/mcp`: arrow to an item, Enter, Esc out.
- Shift+Tab toggles mode.
- Rotate, open and close the IME; TUI reflows.
- `Ctrl` then `o` opens transcript mode.
- PgUp/PgDn scroll in a freshly launched session.
- Close the overlay; the session still runs.

## Known limits

- Remote Tauri profiles that need auth headers cannot attach them to a
  browser WebSocket. Profiles support only `auth: none` today, so nothing
  breaks now. The remote-connection work must solve this (for example a
  short-lived token in the query string).
- Android IMEs (Gboard prediction in particular) can send composition events
  that xterm.js handles imperfectly. Autocorrect is off to minimise this.
  Treat any remaining quirk as a follow-up.
- Fullscreen renderer is a Claude Code research preview.
- The Origin check does not stop DNS rebinding. A hostile page at
  `http://evil.com:<port>` that rebinds `evil.com` to `127.0.0.1` sends a
  matching `Host` and `Origin`, and under `auth: none` the guard admits it.
  The existing HTTP API (`/api/launch`, `/api/kill`) has the same exposure
  today. The fix is a `Host` allowlist in the guard layer covering every
  route (loopback names, the `CDASH_PUBLIC_URL` host), done as a separate
  change rather than inside this feature.
