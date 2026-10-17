# Codex native terminal: plan to a merge-ready PR

## Execution status, 2026-10-17

Implemented on an isolated worktree after merging master `328733f`. Shared native
TUI/bridge feasibility, interrupt/approval routing, bootstrap, and reconnect were
proved with disposable real CLI sessions. The independent review found four
shutdown/reconnect/metadata issues; regression tests cover their corrections.

Passed: 332 Rust tests, all 41 Node tests, strict Clippy, release Linux agent and
Tauri builds, ARM64 musl agent build. The Node failed-download fixture uses a
local curl wrapper with `--connect-timeout 2` because this host drops connections
to the closed loopback test port. The wrapper changes no production code.

Real Chromium/native-session qualification passed: virtual arrows edit the
composer; PgUp and wheel scroll the transcript; one browser prompt produces one
turn; reply delivery, portrait/landscape resize, close/reopen, and live RC
attachment work, with no browser errors. This exposed and corrected a missing
experimentalApi handshake; regression coverage now rejects missing capabilities.

Installed-agent health, executable digest, and embedded assets are verified.
The user tested the deployed terminal and accepted its behavior on 2026-10-17.
The live cloud cancellation event returned HTTP 200 in a disposable session;
a real pending native approval resolution was verified separately. A full
interactive cloud approval round-trip remains unrun: this host's configured
permission policy declined the test request before requesting approval.

The Android release APK builds; an installable test signature can be applied
with the existing release tooling. Windows/macOS runtime testing is unrun.
Remaining merge gate: exact pushed-head CI and conflict verification. The checklist below is
the original acceptance plan; it is not a claim that pending gates passed.

## Goal and planning baseline

The dashboard terminal should show the native Codex TUI, with working input,
scrolling, interruption, resize, and reconnect. Claude Remote Control must
continue controlling the same Codex thread.

The current Codex tmux pane runs a line-based bridge, not the Codex TUI. Raw
terminal keys can therefore become prompt text. The installed CLI advertises
`--remote`, and app-server supports local socket transports; sharing a live
thread between the TUI and bridge had not yet been proven when this plan was written.

The uncommitted scrolling experiment in `collect/term.rs`, `public/app.js`, and
`test/frontend-guards.test.mjs` is not ready to ship. Inline-history tests pass;
the fullscreen PageUp regression fails. Keep the running agent unchanged until
the replacement is validated. Preserve unrelated `APP_REMOTE_PLAN.html`.

## 1. Prove the shared-session design before implementation

- [ ] Use a disposable project and test session, outside existing user sessions.
- [ ] Determine the smallest supported local transport for both clients to use
  one app-server and one thread: a session-owned Unix socket is preferred if
  supported by both clients. Check whether the existing daemon/proxy interface
  offers a simpler supported path. Do not expose a new network listener.
- [ ] Connect native Codex TUI and a minimal bridge client to that backend.
- [ ] Send one small turn from each client. Verify both see the same thread ID,
  history, active turn, and completion, with exactly one execution per input.
- [ ] Interrupt a turn, exercise approval/request routing, detach/reconnect each
  client, and check that the backend and other client stay usable.
- [ ] Check simultaneous input and notification echo: no duplicate turns,
  divergent histories, competing approval responses, or unexpected resumes.
- [ ] Record exact CLI version, commands, protocol behavior, and cleanup.

**Exit gate:** the shared session works end to end. If it cannot work reliably,
stop and present evidence and a revised design; do not replace this proof with
two independent processes resuming the same history file.

## 2. Specify ownership and compatibility from the probe

- [ ] Write a short design fixing who owns the backend, bridge, TUI, socket,
  and cleanup. Prefer extending the existing launcher rather than a new daemon.
- [ ] Define one session supervisor: closing a viewer or reconnecting Remote
  Control cannot destroy the backend; killing the session cleans up its entire
  process tree and private socket.
- [ ] Define how active thread identity, model, effort, display name, working
  status, and Remote Control link remain consistent.
- [ ] Preserve credential isolation: Claude ingress credentials must never enter
  the Codex backend, TUI, or its tool environment. Restrict socket access.
- [ ] Keep new launches and deliberate resumes compatible with existing state.
  Do not kill, respawn, or silently migrate currently running bridge sessions.
- [ ] Define honest behavior for legacy bridge sessions and unsupported Codex
  versions; do not present unsupported TUI controls as functional.

**Exit gate:** design reviewed against the probe; lifecycle and legacy behavior
are explicit before changing launch behavior.

## 3. Implement the smallest complete change

Primary files and responsibilities:

| Files | Responsibility |
| --- | --- |
| `crates/agent/src/collect/codex-remote.mjs` | Backend connection, supervisor lifecycle, Remote Control forwarding, native TUI launch, event deduplication |
| `crates/agent/src/collect/codex.rs` | Launch/resume arguments, persisted identity and metadata, capability checks |
| `crates/agent/src/collect/term.rs` | PTY attachment, resize, disconnect behavior; retain scroll changes only if required and verified |
| `public/app.js`, `public/term/keys.js` | Correct key and wheel handling for actual terminal modes; explicit legacy behavior |
| `public/index.html`, `public/style.css` | Change only if the proven design requires a legacy-session explanation or controls |
| `test/codex-remote.test.mjs`, terminal/frontend tests | Regression proof at process, protocol, and input boundaries |
| `README.md`, terminal design/status docs | Actual supported behavior, version requirements, remaining limits |

- [ ] Add failing regressions for the demonstrated failures before their fixes.
- [ ] Keep the existing Remote Control behavior while changing its backend
  transport; verify notifications are forwarded once regardless of input source.
- [ ] Put the native TUI in the pane the dashboard attaches to. Keep bridge logs
  from corrupting its display, and retain useful bounded diagnostics.
- [ ] Match native key behavior rather than emulating an entire terminal editor.
  Validate PgUp/PgDn, wheel, arrows, Esc, Tab, Shift+Tab, Enter, and Ctrl against
  the actual application's key bindings and terminal modes.
- [ ] Resolve the failing fullscreen scroll regression. Remove the experimental
  scroll routing if native TUI handling makes it unnecessary.

**Exit gate:** new sessions and resumes work locally; regression tests fail on
the old behavior and pass with the change; legacy sessions remain intact.

## 4. Verify failure handling and existing functionality

- [ ] Run the complete Rust workspace tests and Clippy with CI's warning policy.
- [ ] Run every Node test file. Record any timeout workaround or flaky result;
  do not report a focused rerun as a clean default-suite result.
- [ ] Build the release agent and affected Tauri targets using existing tooling.
- [ ] Test unavailable/old Codex, failed backend start, failed TUI attach, stale
  socket, Remote Control reconnect/token rotation, worker crash, backend crash,
  repeated opens, multiple viewers, session kill, and agent restart.
- [ ] Verify authenticated terminal access, Origin refusal, session targeting,
  input bounds, and credential isolation still hold.
- [ ] Confirm Claude session launch, terminal controls, fullscreen scrolling,
  resume, model selection, and external-session restrictions still work.

**Exit gate:** required checks pass with recorded evidence; no orphaned processes,
lost session identity, leaked credentials, or unrelated regression remains.

## 5. Prove the user-facing behavior, then update the local agent

- [ ] Browser automation against the release binary must operate a real native
  Codex session, not only a shell fixture or stubbed screen. Check resulting
  state as well as rendered output: keys must never become prompt text.
- [ ] Verify history scrolling, prompt editing, menus, interruption, rotation,
  reconnect, and close/back preserving the running session.
- [ ] Verify the same thread remains usable through Claude Remote Control.
- [ ] Install with a rollback binary, preserve the current environment, restart
  the local agent, and compare the live binary digest and embedded assets.
- [ ] Repeat the key regression against the installed agent.
- [ ] Complete physical mobile Chrome and Android Tauri tests: IME typing and
  composition, open/close keyboard, rotation, virtual keys, scrolling, resume,
  and reconnect. Record desktop touchpad results too.

**Exit gate:** the reported issue is reproduced before the fix and absent after
it on the deployed build. Device-dependent claims require actual device proof.

## 6. Prepare and review the PR

- [ ] Fetch current `master`, integrate newer changes if necessary, and rerun
  checks affected by that integration.
- [ ] Review the final diff and connected lifecycle/security code. Resolve every
  material finding; keep unrelated artifacts and abandoned experiments out.
- [ ] Confirm clean tracked state, focused commits, and no secrets or generated
  build payloads in the diff.
- [ ] Update the existing feature PR if it remains the correct review unit;
  otherwise create one focused PR against current `master`.
- [ ] Describe the concrete before/after behavior, shared-session design,
  compatibility/migration behavior, test evidence, and remaining limitations.
- [ ] Wait for required CI checks and resolve failures.

**Merge-ready means:** shared-session feasibility proven; all required automated
and real-device checks passed; compatibility and cleanup verified; review findings
resolved; current master integrated; CI green. Any unmet gate keeps the PR draft.

This plan does not authorize merging the PR or restarting active Codex sessions.
