# Native Codex terminal and shared Remote Control

Status: implemented; device qualification remains open. Protocol probes use CLI 0.162.1.

The terminal must attach to a native Codex TUI, not the line-oriented Remote
Control bridge. Keep one app-server and one thread for both clients. No extra
TCP listener, dependency install on the user's machine, or dummy inference turn.

## Proven protocol behavior

- Unix WebSocket connections support native TUI plus an RPC bridge. Two real
  turns appeared in both clients and produced exactly two history turns.
- `thread/name/set` persists a newly created empty thread. Without it, another
  client cannot resume the thread until a turn is persisted. Name the thread
  before exposing either interface; this is the dashboard's existing session
  display name, not a synthetic prompt.
- The bridge can interrupt a TUI turn; native Esc can interrupt a bridge turn.
  Reconnecting a bridge does not change the thread. Closing the TUI connection
  does not kill an independently owned backend.
- A native approval request reaches the bridge. Rejecting it resolves the TUI
  prompt, emits `serverRequest/resolved`, and does not execute the command.

Probe receipts are recorded in this plan's execution ledger. The isolated
0.163.0 probe confirms that empty-thread naming is required there too.

## Ownership and transport

The existing launcher supervises a private Unix-socket app-server, the native
TUI, and the Claude Remote Control process. A temporary launcher RPC connection
initializes/resumes and names the thread and saves its actual model/effort/ID.
It remains a passive subscription to track the active turn for graceful shutdown.
It never answers approval requests. Closing this connection cannot kill the backend.
The supervisor writes live native metadata to a separate `.native` snapshot; the
worker owns Remote Control delivery checkpoints in `.json`. The dashboard accepts
native metadata only when its PID and thread match the active session. An RC
outage therefore cannot freeze live status or overwrite native model choices.

Native TUI inherits the tmux pane's terminal. Explicitly enable its fullscreen
transcript and alternate screen for owned scrolling; browser PgUp and wheel
scroll the actual transcript instead of editing the composer. Native RPC clients
advertise `experimentalApi` because active-turn resume pagination requires it. Remote Control output goes to a
private log instead of overwriting the TUI. Closing a dashboard overlay only
detaches its tmux client. Exiting the TUI or killing the session cleans up owned
process groups and the private socket directory. A Remote Control process failure
must leave the native TUI usable and remove the stale remote link.

Use the agent's existing Rust WebSocket dependencies for a small JSONL-to-Unix-
WebSocket CLI adapter. The Node bridge keeps its current RPC implementation.
Standalone agent and in-process Tauri entry points recognize this adapter mode;
Android/Windows continue using their separately installed agent. No public HTTP
route or network transport is added.

Strip Claude ingress credentials and bridge environment variables from backend,
TUI, and tool processes. Keep the socket directory private to the user. Reuse
existing authentication and Origin guards for the browser terminal.

## Events and compatibility

Preserve Remote Control history replay. Forward native user messages into Remote
Control, but never execute echoed messages again. Pass `clientUserMessageId` for
bridge submissions and distinguish their echoes from native inputs. Stable event
UUIDs must match history replay. Native workers catch up on every reconnect,
including reconnects to the same remote identity. Persist only the UUIDs of RC
user submissions, scoped to that remote, to avoid replaying their original user
bubbles. Codex retains these client IDs in persisted history. A worker attaching
to an existing native backend reads its effective settings without overriding
them. Restore the active turn before steering or publishing worker status.
Remove pending approvals when another client
resolves them, so late answers cannot act on a resolved request.

New launches and deliberate resumes use the native supervisor and record
`native_terminal` in the session state. Existing running bridges retain their
legacy launcher. Their cards explain that native terminal access requires a new
launch/resume; the endpoint refuses legacy bridge attachment rather than accepting
unsupported navigation bytes. Do not restart active sessions automatically.

Qualify CLI 0.162.1 or newer. Reject unsupported versions clearly before creating
a session. Keep ordinary Claude terminal behavior and wire protocols unchanged.
The experimental tmux scroll rewrite is excluded: native applications interpret
the terminal's keys and wheel events.

## Completion gates

Regression tests cover adapter framing/EOF/errors, supervisor process cleanup and
credential isolation, native input mirroring and echo suppression, approval
cancellation, launch/resume metadata, legacy access refusal, and ordinary Claude
behavior. Run all workspace/Node tests, strict Clippy, release builds, and a real
native-session browser smoke test. A final independent review and required CI must
pass. Physical Android/Chrome IME and gesture checks remain a user gate before
marking the PR ready to merge.
