# Claude Remote Control findings

Recovered from Claude session `3737d133-bd46-4507-a0d2-dba9076a6fad` and checked
against installed Claude Code 2.1.296 on 2026-10-14. This describes observed
behavior, not a supported third-party integration contract.

## Recovered session

The prior session extracted the Remote Control environment client, work loop,
headless child launcher, and CCR v2 transport. It discovered the process wrapper
hook but did not demonstrate a replacement worker or implement a Codex bridge.

Its transcript remains in
`~/.claude/projects/-mnt-d-git-claude-dashboard/3737d133-bd46-4507-a0d2-dba9076a6fad.jsonl`.
Temporary extraction artifacts remain under
`/tmp/claude-1000/-mnt-d-git-claude-dashboard/3737d133-bd46-4507-a0d2-dba9076a6fad/scratchpad/`.

| Artifact | Purpose |
| --- | --- |
| `pretty/0480.js` | Environment registration and work API |
| `pretty/0481.js` | Polling, dispatch, resume pointers, shutdown |
| `pretty/0460.js` | Work-secret validation, session URL, worker registration |
| `pretty/0474.js` | Headless child arguments, environment, token refresh |
| `pretty/0461.js` | CCR v2 SSE, events, acknowledgements, worker state |
| `claude.strings` | Includes the self-invocation/process-wrapper resolver |

These temporary paths are evidence locations, not runtime dependencies. No
extracted Claude implementation is included in the dashboard.

## Observed transport

The authenticated runner registers an environment and polls for work. Its
versioned work secret carries a session ingress token and API base URL. Worker
registration obtains an epoch. The child receives its session URL, ID, ingress
credential, and epoch from that runner.

Incoming events arrive as SSE `client_event` frames with a payload, event ID,
and sequence number. The service can encode sequence numbers as strings;
convert them to numbers before comparing. Reconnect uses `from_sequence_num`.

The session's worker API accepts event batches, delivery acknowledgements,
heartbeats, and state updates. Event batches contain `{payload}` envelopes;
stable payload UUIDs allow upload retries to be deduplicated. Approval requests
also need pending-action metadata for the remote interface.

`CLAUDE_CODE_PROCESS_WRAPPER` accepts an absolute executable path or a JSON argv
array. Self-invocation inserts the original Claude executable after that prefix.
Remote Control's headless child receives `--sdk-url`, `--session-id`, stream JSON
arguments, `CLAUDE_CODE_SESSION_ACCESS_TOKEN`, and `CLAUDE_CODE_WORKER_EPOCH`.
Credential rotation arrives on stdin as `update_environment_variables`.

Standalone Remote Control can reuse a recently saved environment and session
for its directory even without an explicit `--continue`. Replacing its child
while starting a fresh Codex thread must therefore isolate the runner directory.

The bridge uses the installed runner's login and eligibility checks. It does
not replace authentication or change organization policy.

## Checks in this session

- A temporary wrapper was actually invoked by the authenticated runner and
  received the expected credential and epoch. Only their presence was logged.
- The Codex worker initialized a real app-server thread, registered worker
  state, published its initialization event, and obtained a Claude RC link.
- Two text turns returned the requested marker replies; Claude accepted their
  durable reply events.
- Launching two sessions through the dashboard API in one project produced
  distinct tmux panes, Codex thread IDs, and Claude RC links.
- Dashboard send reached Codex and produced `APP_MANAGED_CODEX_OK`.
- Dashboard kill and resume retained the same Codex thread ID.
- The offline process integration check exercises split SSE frames, numeric
  string sequences, persisted resume cursors, credential isolation/rotation,
  and refusal of unsupported requests.

The Claude mobile/web interface's exact rendering has not been manually checked.
Protocol acceptance and session registration were checked against the real
service. The bridge currently accepts text and completed assistant messages;
it does not expose the native Codex terminal interface.

## Dashboard chat tap (Claude Code 2.1.322)

Observed on Claude Code 2.1.322 on 2026-11-12 while designing dashboard chat
(`docs/superpowers/specs/2026-11-12-chat-core-design.md`). These are internal
formats and behaviours, not a supported contract; an update can change them.
Chat reads them in `crates/agent/src/chat/tap.rs` and `claude.rs`.

- **Transcript records.** `user` with string content is a prompt; with
  `tool_result` blocks it is a tool result; a text block starting
  `[Request interrupted by user` marks a stopped turn. `assistant` writes one
  line per content block (`thinking`, `text`, `tool_use`), several lines
  sharing one `message.id`. `system` with subtype `turn_duration` (carrying
  `durationMs`) ends a turn. `isSidechain: true` is subagent traffic and
  `isMeta: true` is injected text. `<bash-input>` and
  `<bash-stdout>`/`<bash-stderr>` user records are `!` shell commands.
  `promptSource` is `typed`, `queued` or `system`.
- **Handed-back prompt.** Escape before any reply puts the prompt back in the
  input box. Its `user` record stays in the file with no marker, and the next
  prompt is written with the same `parentUuid`.
- **The TUI's own queue.** `queue-operation` records: `enqueue` carries the
  text, `dequeue` is followed by an ordinary `user` record, and `remove` with
  reason `absorbed_mid_turn` means Claude took the message mid-turn; then
  there is no `user` record, only an `attachment` of type `queued_command`
  (`commandMode: "prompt"`) holding the text.
- **Session state file.** `~/.claude/sessions/<pid>.json` carries `status`
  (`busy`, `idle`, `waiting`, `shell`), `statusUpdatedAt` and `waitingFor`
  (`dialog open`, `input needed`). Older versions write no `status`; chat then
  falls back to the transcript. How quickly `status` follows a turn end, a
  stop or a dialog was not timed.
- **Keys.** A bracketed paste (`tmux paste-buffer -p`) followed by Enter about
  0.4 s later is accepted as one prompt, multi-line included. Escape
  interrupts a running tool in about 0.3 s; with a message in the TUI's own
  queue, Escape stops the tool and then runs that message at once. Ctrl+U
  clears to the start of a row, about two presses per line of a multi-line
  input, and is harmless on an empty box.
- **Key table (read from the installed program, not tested).** `ctrl+l` is
  `chat:clearInput`; `ctrl+x enter` is `chat:queueSubmit`; `ctrl+enter` and
  `ctrl+x ctrl+s` are `chat:sendNow`.
