# Browser view for dashboard sessions

Date: 2026-10-21
Status: reviewed 2026-11-06; not yet implemented (original approval 2026-10-21)

## Why

Some sessions drive a visible Chromium-based browser through Playwright MCP:
the JOBS protocol (Indeed, LinkedIn and other job boards) and the INVEST
protocol (eToro, Gotrade). These sites periodically need a login, sometimes
with MFA. The user will not hand passwords to an agent, so the login has to be
typed by the user, inside that exact browser profile. Chrome, Chromium, Brave,
Edge and other Chromium-based browsers use the same CDP bridge; Edge is not a
dependency. Browser selection stays in the existing Playwright launcher/config.

Until now that meant sitting at the PC or using Chrome Remote Desktop. The user
no longer wants to depend on remote-desktop software. The dashboard already
lets a phone drive a session's TUI through the **Terminal** button; this adds
the same kind of button for the session's browser.

Success: away from the PC (which is usually locked), the user opens the
dashboard on a phone, taps **Browser** on the JOBS or INVEST session card, sees
the live login page, types username, password and MFA code, and the agent
carries on, logged in.

## Goals

- A **Browser** button on dashboard session cards whose folder has a
  configured browser debugging port, shown while that browser is running.
- A full-screen overlay, styled like the Terminal overlay, that shows the live
  tab and accepts taps and typing.
- Typing a password from the phone works on a locked PC.
- Browser-saved passwords work when the browser fills the form on page load.
  That case is a bonus, not a requirement.
- The INVEST Playwright server works from a dashboard (WSL tmux) session, which
  it does not today.

## Non-goals

- External sessions (started outside the dashboard). If an external session
  uses the same folder, the button appearing for it would be a bonus; nothing
  is built for it.
- The browser's own UI, which a page screencast cannot show: saved-password and
  autofill dropdowns, "Save password?" prompts, permission prompts, HTTP auth
  dialogs, file pickers, Windows Hello and PIN prompts.
- Navigating to arbitrary URLs, opening new tabs, downloads, uploads,
  clipboard sync, audio.
- Resizing or re-laying out the page for the phone (no device emulation).
- Notifying the user that a login is waiting.
- Remote Tauri profiles that need auth headers (same known limit as Terminal).
- Android-hosted agents and non-Chromium engines (Firefox/WebKit). Without a
  configured, reachable CDP port the button never appears. The transport has
  no Windows requirement; Windows/WSL launchers below describe the user's setup.

## Anti-goals

Any of these makes the feature a failure even if it works:

- The browser WebSocket becomes a general Chrome DevTools Protocol (CDP)
  pipe. The phone must only be able to view, tap, type, scroll, switch tab,
  go back and reload. cdash-agent never calls CDP methods that read cookies,
  storage or page state (`Network.*`, `Storage.*`, `Runtime.evaluate` and
  similar) on the user's behalf.
- Anything typed is logged, persisted, or retained after forwarding/canceling.
  Only transient input, IME composition and transport buffers are allowed;
  clear them on commit, cancellation, disconnect and overlay close.
- The debugging port is reachable from anything but loopback.
- The view disrupts the agent's own Playwright control: closing or launching
  the browser, changing the viewport, or holding a lock that makes Playwright fail.

## Constraints

- **Protocol amendment.** The INVEST protocol was amended on 2026-10-21
  (`INVEST/protocol/tools.md`, section "Dashboard browser view"). It allows
  `playwright-invest` to launch Edge with `--remote-debugging-port` set in the
  server's own config, loopback only. It forbids AI agents from connecting to
  that port. cdash-agent is the dashboard's server process, not an AI agent;
  the AI agent in the session keeps using only its Playwright MCP tools. The
  JOBS protocol has no remote-debugging ban. This is the original design's
  policy report, not a fresh audit of those external files. Before changing
  INVEST's browser selection, verify its current policy permits that
  executable/profile; the generic bridge does not broaden project policy.
- **The user accepted the risk.** While the port is open, any local process on
  the PC (Windows or WSL) can drive the logged-in tabs without a password.
- **The user's browser runs on Windows, cdash-agent runs in WSL.** Their WSL uses
  `networkingMode=mirrored`, so WSL `127.0.0.1` reaches the Windows loopback
  port. Under the default NAT mode the port would be unreachable; this design
  does not try to solve that. A browser on the same host as cdash-agent needs
  no mirrored networking, only a reachable loopback endpoint.
- **Playwright owns the browser.** It is launched lazily by Playwright MCP on the
  agent's first browser call, over Playwright's private pipe. Nothing in this
  design launches or replaces the browser.
- **A dedicated automation profile.** Keep each launcher's existing browser
  and matching profile; never reuse another browser's profile or the user's
  default personal profile. Chrome 136+ ignores remote-debugging switches on
  its default data directory ([Chrome documentation](https://developer.chrome.com/blog/remote-debugging-port)).
- **Browser compatibility is capability-based.** Accept the configured
  Chromium executable/channel, not a product-name allowlist or an `Edg/`
  version check. Required CDP methods are listed in the plan. Missing methods
  close the view with `4004`, `Browser view unsupported`; do not silently
  launch Edge or relax browser security flags. Locked/background operation
  must be verified per browser/version; the Edge spikes below are not proof
  for other browsers.
- **Same guards as Terminal.** The route sits in `guarded_routes!` (auth) and
  applies `origin_allowed` before upgrading, exactly like `GET /api/term`.

## Spike evidence (2026-10-21, throwaway, scratch profile)

Historical reports from the original design, not rerun in this review.

Run with `@playwright/mcp` 1.64.0-alpha and Edge 155.0.4283.62, launched
through a `.cmd` wrapper via `cmd.exe` from WSL, as JOBS does. The PC was
locked during the run (`LogonUI` running).

- `--config` with `{"browser":{"launchOptions":{"args":["--remote-debugging-port=9399"]}}}`
  adds the port. Playwright still launches Edge and keeps working.
- `http://127.0.0.1:9399/json/version` is reachable from WSL.
- A second CDP client works alongside Playwright: `Page.startScreencast`
  delivered 31 frames on the locked PC; `Input.dispatchMouseEvent`,
  `Input.insertText` and `Input.dispatchKeyEvent` (Enter) filled and submitted
  a form.
- Edge's autofill dropdown did not appear in the stream, and blind
  ArrowDown + Enter selected nothing; the form submitted empty. Typing is the
  reliable route.
- `Browser.getBrowserCommandLine` is refused ("--enable-automation not set"),
  so cdash-agent cannot ask Edge which profile it runs. Edge did not write a
  `DevToolsActivePort` file into the profile either. Hence the port map below.

A second run (same setup, PC unlocked) covered the agent browsing while the
user logs in, since both protocols tell the agent to carry on with other work
while a login waits:

- A background tab keeps streaming. With the agent's Playwright tab in front,
  the viewed tab still produced frames for typed input, and reported
  `document.visibilityState === "visible"`. A minimized Edge window streamed
  too. The view therefore never needs to bring a tab to the front or change
  the window state.
- Playwright kept working on its own tab (`browser_navigate`,
  `browser_snapshot`) while the viewed tab received input.
- A popup opened by the viewed tab (`window.open`) arrives in
  `Target.targetCreated` with `openerId` equal to the viewed tab. Tabs the
  agent opens through Playwright (`browser_tabs` new) have no `openerId`.
- Frame pixels are not page CSS pixels. A tap computed from frame pixels
  landed one field lower; on this PC the frame is about 1.3x the CSS size.
  Taps must be converted with the frame metadata.

A third run (design review, headless Edge 155.0.4283.62, scratch profile,
local test page) checked text entry against one-character-per-box MFA fields:

- `Input.insertText("123456")` into the first box filled only that box
  (`1_____`), whether the page advances focus on `input`, on `keyup`, or
  splits a `paste` event. One `Input.insertText` per character fixed the
  `input` and `paste` cases but not `keyup` (`1_____`).
- One `Input.dispatchKeyEvent` keyDown (`key` and `text` set to the character)
  plus keyUp per character filled all three (`123456`) and typed
  `Pa$$ w0rd!é` into a plain field unchanged.
- A field that drops keys by `keyCode` accepted the digits only when
  `windowsVirtualKeyCode` was set. A Backspace keyDown without
  `windowsVirtualKeyCode` deleted nothing; with 8 it deleted.

## Design

### 1. Launchers (user's folders, outside this repo)

Each project's Playwright MCP launcher gains a config file with a fixed port:

| Project | Port | Config file |
|---|---|---|
| JOBS | 9301 | `D:\p\Downloads\JOBS\tools\playwright-mcp.json` |
| INVEST | 9302 | `D:\p\Downloads\INVEST\tools\playwright-mcp.json` |

```json
{ "browser": { "browserName": "chromium", "launchOptions": { "args": ["--remote-debugging-port=9301"] } } }
```

Keep the existing browser channel or `--executable-path` and dedicated
`--user-data-dir`. For a custom Chromium browser, Playwright MCP accepts
`--executable-path` ([configuration](https://playwright.dev/mcp/configuration/options)).
The executable and profile must belong to the same chosen browser. This
port-only JSON does not select Edge or migrate any profile. Native Linux/macOS
launchers can add the same config without a Windows wrapper.

- JOBS: `tools\playwright-jobs.cmd` appends `--config=D:/p/Downloads/JOBS/tools/playwright-mcp.json`.
  Update the setup command in `JOBS/protocol/tools.md` to match, since the
  `.cmd` comment says its args match that file.
- INVEST: add `tools\playwright-invest.cmd`, the same shape as the JOBS one
  (Windows `npx`, configured Chromium executable, `--user-data-dir=D:/p/Downloads/INVEST/PlaywrightProfile`,
  `--config=...`). Point both `INVEST\.mcp.json` and `INVEST\.codex\config.toml`
  at it through `cmd.exe /d /c`. Today both call `npx` directly, which inside
  WSL resolves to Linux `/usr/bin/npx` and cannot launch a Windows browser executable.
  `cmd.exe` works from Windows-side sessions too. Update the setup command in
  `INVEST/protocol/tools.md`.
- Back up each file before editing (`.bak-YYYYMMDD-...`), as those folders
  are not git repositories.
- The profile gate checks server name and `--user-data-dir`; neither changes.
- Verify the actual listener is bound only to `127.0.0.1`/`::1`; reject wildcard
  or LAN bindings. Do not add `--remote-allow-origins=*` or disable a sandbox.

### 2. Port map (cdash-agent)

A JSON file `$CLAUDE_DIR/cdash-browsers.json`, next to `cdash-places.json`,
edited by hand:

```json
{ "/mnt/d/p/Downloads/JOBS": 9301, "/mnt/d/p/Downloads/INVEST": 9302 }
```

- A session matches the longest key that equals its `dir` or is a parent of
  it.
- Read on demand (it is tiny); a missing or malformed file means no browsers.
  Ports outside 1024..=65535 are ignored.
- cdash-agent only ever connects to `127.0.0.1:<port>` from this map.
- Every discovery HTTP request disables proxies **and redirects** with
  `reqwest::redirect::Policy::none()`; a redirect is a failed probe, never a
  new destination. Rebuild the CDP WebSocket authority from this mapped host
  and port, using only the validated `/devtools/browser/...` path from the
  response. Never follow a supplied WebSocket host, port, query or fragment.

### 3. Button visibility: `/api/sessions`

For each running dashboard (tmux) session whose `dir` matches the map,
cdash-agent probes `http://127.0.0.1:<port>/json/version` with a short timeout
(about 300 ms; probes run concurrently and a refused connection returns at
once). Success requires 2xx JSON with a usable browser WebSocket path, not
merely an HTTP service returning `{}`. The session row then carries `"browser": true`; otherwise the
key is absent. External sessions are not probed.

Add the field to `Session` in `collect/external.rs` as an optional field
skipped when absent, like `nativeTerminal`. The parity gate that struct's
comment mentions was deleted with the Node agent, so there is no exemption
list to update. Update every `Session` constructor: current `master` has
Claude, Codex, OpenCode and bare-terminal branches, the external constructor
and a serialization test. Match `pane_current_path` for both flags and the
WebSocket route, even when a provider's displayed `dir` differs.

### 4. `GET /api/browser` WebSocket

Query: `name` (the tmux session).

Checks before upgrading, in order: auth (guard layer), `origin_allowed`,
`assert_kill_name`, the session exists and its `dir` maps to a port. Failures
answer like `/api/term` does.

After the upgrade, cdash-agent:

1. Fetches `/json/version` and opens the browser-level `webSocketDebuggerUrl`
   with the existing `tokio-tungstenite` dependency (no new crate), confined
   to the mapped loopback authority as above.
2. Lists page targets and picks the first listed page. `/json/list` order is
   only a hint, not a guaranteed activity ordering. Show the selected origin
   before enabling input; the tab picker lets the user choose the login tab.
   With no pages, send `active:null` and wait for a user selection.
3. Attaches with `Target.attachToTarget {flatten:true}` and starts
   `Page.startScreencast` (JPEG, quality about 60, max width 1280). Each frame
   is sent to the phone as a binary message. `Page.screencastFrameAck` is
   sent only after the phone send completes, so a slow link lowers the frame
   rate instead of queueing frames.
4. Watches targets (`Target.setDiscoverTargets`). It sends the phone the tab
   list (id, title, origin) whenever it changes. The viewed tab changes only
   when the phone sends `tab`, so input can never land in a tab the user did
   not pick. A login popup ("Sign in with Google") appears in the list and
   the user picks it. When the viewed tab closes, cdash-agent sends the list
   with `"active":null` and drops input until the phone picks a tab.
5. Never calls `Target.activateTarget` or changes the window state. The agent
   decides which tab is in front; background tabs stream anyway (see spike).

**Phone to cdash-agent** (text frames, JSON; anything else is ignored):

| Message | CDP |
|---|---|
| `{"tap":[x,y]}` | `Input.dispatchMouseEvent` moved, pressed, released (left, clickCount 1) |
| `{"scroll":[x,y,dx,dy]}` | `Input.dispatchMouseEvent` type `mouseWheel` |
| `{"text":"..."}` | per character, `Input.dispatchKeyEvent` keyDown (`key` and `text` = the character) + keyUp |
| `{"key":"Enter"}` | `Input.dispatchKeyEvent` rawKeyDown (+ char for Enter) + keyUp, with `windowsVirtualKeyCode` |
| `{"tab":"<targetId>"}` | switch screencast to that page target |
| `{"nav":"back"}` / `{"nav":"reload"}` | `Page.getNavigationHistory`, then `Page.navigateToHistoryEntry` with the previous entry's id (only the id is used) / `Page.reload` |

- `x`/`y` are page CSS pixels. The phone converts from image pixels using the
  frame metadata cdash-agent forwards with each frame (`deviceWidth`,
  `deviceHeight`, `offsetTop`, `pageScaleFactor`).
  Measure relative to the displayed image after zoom/pan, not the stage.
  Forward metadata unchanged and pair it with its JPEG; enable input only
  after that pair is displayed. With image-relative coordinates, use
  `x / shownWidth * deviceWidth`, `y / shownHeight * deviceHeight`;
  `offsetTop` describes browser chrome above the image, not an extra image
  offset. DevTools' [input mapping](https://github.com/ChromeDevTools/devtools-frontend/blob/main/front_end/panels/screencast/InputModel.ts)
  likewise maps mouse input without dividing by `pageScaleFactor`. Test
  non-default desktop zoom/DPI rather than infer support from one Edge run.
- `key` is one of `Enter`, `Tab`, `Backspace`, `Escape`, `ArrowLeft`,
  `ArrowRight`, `ArrowUp`, `ArrowDown`. Anything else is dropped. Each is
  sent with its Windows virtual key code (Enter 13, Tab 9, Backspace 8,
  Escape 27, ArrowLeft 37, ArrowUp 38, ArrowRight 39, ArrowDown 40); Edge
  ignored a Backspace without one in the third spike run.
- `text` goes one character at a time as a key press, never as one
  `Input.insertText`: MFA fields with one box per digit keep only the first
  character of a whole insert, and boxes that advance on `keyup` need key
  events (third spike run). ASCII letters and digits also carry
  `windowsVirtualKeyCode` (the uppercase character code), because pages that
  filter by `keyCode` drop them otherwise.
- Coordinates are clamped to the frame's size, and text is capped (for example
  at 4096 bytes per message).
- `tab` must name a page target cdash-agent itself listed.

**cdash-agent to phone:** binary JPEG frames, and text JSON
`{"frame":{...metadata}}`, `{"tabs":[...],"active":"<id>"|null}`.
Each tab has `id`, `title` and `origin`. `origin` is the parsed URL's
serialized tuple origin, or JSON `null` for opaque/invalid origins. Never
fall back to the raw URL (which can contain file paths, data or tokens).
The phone renders origins/titles as text, never HTML.

**Teardown:** on phone disconnect, stop the screencast, detach, close the CDP
socket. The browser and session are untouched. If the browser goes away (CDP socket
closes), close the phone socket with a reason the overlay shows ("Browser
closed").

**Errors and logging:** use fixed categories only: `browser open <name>`,
`browser close <name>`, `browser refused: <category>`,
`browser failed <name>: <category>`. Categories are `origin`, `name`,
`session`, `mapping`, `connect`, `unsupported`, `protocol`, `timeout`, `send`.
Only validated session names may be interpolated. Never log raw HTTP/CDP
errors, rejected headers, URLs, titles, input or message bodies. Phone close
reasons are fixed: `4002 Browser not running`, `4003 Browser closed`,
`4004 Browser view unsupported`, `1011 Browser connection failed`.

### 5. Phone overlay (`public/`)

A `#browser` overlay built like `#term`, reusing its styles and its
status/Reconnect behaviour:

```
+--------------------------------+
| Browser - JOBS     Back  ⟳   X |
| https://secure.indeed.com  v   |
+--------------------------------+
|                                |
|   live picture of the tab      |
|                                |
+--------------------------------+
| Kbd  Tab  Bksp  <  >  Esc  Ent |
+--------------------------------+
```

- Session cards with `browser: true` get a **Browser** button next to
  **Terminal**.
- The second row shows the viewed tab's origin, taken from cdash-agent's tab
  list (the browser's URL, not page content), so the user can verify the site
  before typing a password. `origin:null` displays `No web origin`. It doubles
  as the tab picker. When the viewed tab closes (`"active":null`), select a
  disabled `Pick a tab` placeholder and show `Tab closed. Pick a tab.`; the
  user opens the picker. Do not invoke `showPicker()` from a WebSocket event:
  it requires [transient user activation](https://html.spec.whatwg.org/multipage/form-elements.html#dom-select-showpicker).
- The picture fits the screen width. Pinch zooms it; while zoomed, a one-finger
  drag pans the picture. At fit width, a drag scrolls the page (`scroll`). A
  tap is a `tap` at that point.
- **Kbd** focuses a hidden `type="password"` input with
  `autocomplete="off"`, `autocapitalize="off"`, `spellcheck="false"`. Visually
  conceal it while keeping it focusable (not `hidden` or `display:none`).
  Password mode requests private entry; third-party keyboard behavior is
  outside the dashboard's control. Pasting an MFA code still works.
  Committed input becomes `text`; deletes become `Backspace`. Keep a sentinel
  for empty-field Backspace, but never reset the field during composition.
  Send a committed composition once, including browsers' possible trailing
  input event; canceled compositions send nothing. Clear the field after
  forwarding and on close/disconnect. Handle Enter through `keydown` as a
  single-line input need not emit `insertLineBreak`.
- Closing the overlay closes the WebSocket only.

### 6. README

A **Browser** paragraph next to the Terminal one: what it is for, the port map
file, the launcher config, the loopback-only port and the local-process risk,
Windows/WSL mirrored-networking requirement, browser selection and dedicated
profile, and that the browser's own popups are not visible.

## Testing

- Unit: port-map parsing and longest-prefix matching; phone-message parsing
  and validation (unknown keys, bad coordinates, oversized text, unknown tab
  id).
- Integration, following the `/api/term` tests: a fake CDP server (axum with
  `ws`, already a dependency, because `/json/version` is plain HTTP) answers
  `/json/version` and the CDP calls, emits a screencast frame, and asserts
  that cdash-agent relays the frame, acks it, and turns `tap`/`text`/`key`
  into the expected CDP calls (`text` as one key press per character, keys
  with their virtual key codes). Also cover origin refusal and an unmapped
  session, hostile redirects/discovery URLs, missing CDP capabilities,
  secret-bearing errors, and opaque origins. Verify cleanup on phone close
  while CDP remains open; pending calls/tasks must end without closing the browser.
- Frontend, following `test/frontend-guards.test.mjs`: a card shows
  **Browser** only when the row has `browser: true`; preserve existing card
  helpers. Check IME commit/cancel/deduplication, safe tab labels and frame
  metadata/image pairing before input is enabled.
- Manual, on the real setup: a JOBS session launched from the dashboard
  reaches a login page; on a phone with the PC locked, open Browser, check the
  origin line, type into a field, submit, close the overlay, and confirm the
  agent continues. Repeat once for INVEST after the wrapper change. Before
  claiming browser compatibility, run the same scratch-profile smoke tests
  with Chrome/Chromium and another Chromium-based browser (Brave, Vivaldi,
  Opera or Edge); record executable/version, host OS and pending cases.
  Include background tabs, page zoom/DPI, six-box MFA, IME and reconnect.
  Never connect an AI agent directly to INVEST's debugging port.

## Left to the builder

- Gesture thresholds and overlay styling; frame mapping/pairing follows section 4.
- The exact probe timeout and whether to cache probe results briefly.
- Whether `Backspace` from the IME arrives as `beforeinput`
  (`deleteContentBackward`) or `keydown`; handle what Android Chrome and the
  Tauri webview actually send.

## Calls made without the user

- Fixed ports 9301 (JOBS) and 9302 (INVEST), and a hand-edited
  `cdash-browsers.json` port map.
- A narrow message protocol in cdash-agent instead of passing raw CDP through.
- The view changes tab only when the user picks one. Login popups are picked
  from the tab list, not followed automatically.
- The view never brings tabs to the front or touches the browser window.
- A hidden `type="password"` input for all typing.
- Showing the full origin (scheme and host) in the address line.
- Drag scrolls the page at fit width and pans the picture when zoomed.

## Review ledger (2026-11-06)

Frame: review the design and plan before feature implementation; any
Chromium-based browser exposing the required loopback CDP methods must work
without an Edge dependency. Baseline: `origin/master` `60cbf75`; original PR
head `8814cef`. Each issue followed Critic → Writer → Critic; one rebuttal
round each, no deadlocks. The Moderator applied Ponytail's existing-code,
native-feature and minimum-complete-change rules ([source](https://github.com/DietrichGebert/ponytail)).

| ID / severity | Evidence and strongest objection | Writer response / Critic disposition | Dependencies / verification |
|---|---|---|---|
| I1 HIGH | Original non-goals excluded non-Windows Edge; Task 10 expected `Edg/`. Chrome 136 requires a non-default profile. | Conceded: generic Chromium executable/channel, dedicated matching profile, capability failures; no adapter layer. Critic accepted. | Sections 1–4; Tasks 2, 4, 5, 8–10. Cross-browser runtime matrix pending. |
| I2 HIGH | Tasks 2/4 used `.no_proxy()` only; [reqwest follows redirects by default](https://docs.rs/reqwest/latest/reqwest/struct.ClientBuilder.html#method.redirect), defeating loopback confinement. | Conceded: disable redirects for all discovery; pin WebSocket authority. Critic required redirect-trap tests, accepted. | Tasks 2/4 share the same policy; HTTP and WebSocket paths both covered. |
| I3 HIGH | Plan forwarded `<error>` to logs and 1011; existing `term::close_reason` only truncates UTF-8 to 123 bytes. | Conceded: fixed categories/messages; discard raw errors and rejected headers. Critic required error canaries, accepted. | Tasks 4–6, README; same rule on every failure path. |
| I4 MED | `collect_sessions` now has four provider branches. Executing Task 8's original context produced `ReferenceError: copyIdBtn is not defined`. | Conceded: update all six `Session` literals and card helper context. With helper supplied, test fails on missing Browser button as intended. Critic accepted. | Latest master; Tasks 2/8; pane path must agree with route lookup. |
| I5 MED | [IME input events](https://w3c.github.io/input-events/#input-event-order-during-composition) use `insertCompositionText`; Task 8 reset the field on every event while its mapper ignored composition. | Conceded: defer forwarding/reset until commit; deduplicate and clear on cancellation/close. Critic accepted with event-sequence tests. | Tasks 7/8; live browser prototype unavailable, device checks pending. |
| I6 MED | Node URL probe: `data:text/plain,URL_CANARY` has origin `null`; original fallback exposed the entire URL as its origin. | Conceded: tuple origin or JSON null, fixed label, DOM text rendering. Critic required URL and HTML canaries, accepted. | Tasks 5/8; no raw URL fallback in any browser. |
| I7 MED | Spec auto-opened a picker but plan did not; [HTML requires user activation](https://html.spec.whatwg.org/multipage/form-elements.html#dom-select-showpicker). Metadata contracts also differed. | Conceded: explicit picker prompt in both; forward full metadata and use image-relative coordinates. Critic accepted; no custom picker. | I1 broadens validation matrix; Tasks 5/7/8 cover zoom, pan and pairing. |

Integrated Critic/buildability pass: reviewed both revised originals against
the merged baseline, provider/constructor alignment, existing Terminal
helpers, dependencies and protocol contracts. Clean pass: no material new
defect or consequential requirement gap. Implementation and runtime checks
below remain pending; this result is a documentation review, not a browser
compatibility test.

Residual risks: any local process can control the debugging port (previously
accepted); loopback cannot prove profile ownership; another controller can
navigate the same tab while the user types. Origin display is a user check,
not an atomic navigation lock. Do not lock or activate Playwright's tabs.
The historical Edge spikes do not establish locked/background/IME behavior
on other browser versions. Runtime and real-phone checks remain pending;
no feature or external launcher changes are part of this documentation PR.
