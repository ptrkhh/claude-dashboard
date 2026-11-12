# Dashboard-native chat, part 1: chat core

Status: draft for owner review, 2026-11-12. No product code has changed.
Code references were collected at v0.6.0 (`911bf88`) and should be re-checked
before planning.

## Intent

RC Dashboard starts and resumes coding sessions, but the conversation itself
happens in Anthropic's Claude app through Claude Remote Control ("Open in
Claude"). The owner wants the dashboard to be self-sufficient: the same
conversation, inside the dashboard, talking only to the `cdash-agent` on the
host, for Claude Code, Codex and OpenCode alike. Nothing went wrong with Remote
Control; the goal is independence and feature parity, with a feel that is as
fluid and reliable as the Claude app, or better.

Remote Control support stays. "Open in Claude" must keep working exactly as it
does today.

## Program context

The whole effort is too large for one design. The owner agreed to this split
and order. Each part gets its own design and plan.

1. **Chat core (this document).** Send text, live replies, tool rows, stop,
   queue, history, reconnect, for all three tools.
2. Prompts and menus: permission card, questions, plan approval, slash
   commands, model, effort, permission mode.
3. Attachments: images and files.
4. Reliability polish and push notifications.
5. Later tier: diff viewer, usage sheet, side chat, and the rest of the
   Claude app's feature list.
6. Claude token streaming spike (see "Token streaming" below).

## Decisions made by the owner

- **Independence means no relay.** Chat goes device, then `cdash-agent`, then
  the coding tool. It must work when claude.ai is unreachable and for accounts
  without Remote Control.
- **Three doors on one live session:** dashboard chat, Terminal, and Open in
  Claude, usable at the same time.
- **Terminal stays a first-class door on every session.** The owner uses it
  often, because no chat can cover every TUI feature (`! command` was the
  example that fails over Remote Control today).
- **Claude Code keeps its interactive TUI as the engine** ("option B"). Chat
  taps the running TUI instead of replacing it with a headless process. The
  owner chose this knowing the costs: no token-by-token streaming for Claude at
  first, and a dependence on Claude Code internals that an update can break.
  The owner would choose it even if token streaming never becomes possible.
- **Desktop layout:** the dashboard folds into its existing narrow layout as a
  left column, and chat sits on the right. Clicking another card switches chat.
- **Targets:** every current client (browser and PWA, desktop app, Android
  app). The owner chats on a Windows workstation (sessions in WSL), an Android
  tablet (sessions in Termux), and a remote Windows server (sessions in WSL)
  reached from a laptop and phone.
- **Stop means stop.** Asked what should happen to messages still waiting
  when Stop is pressed, the owner took the recommended option: they stay
  held, each with Send, Edit and Remove, and nothing runs that the owner did
  not confirm. How the queue is built to make that true is the author's call
  (see "Author's calls" 4, 13 and 14).
- For the remaining choices the owner said to use the author's
  recommendation. Those are listed under "Author's calls".

## Goals

- A **Chat** button on every running session card that opens that session's
  conversation inside the dashboard.
- Send a text prompt and see the reply arrive live, with each tool call shown
  as it happens.
- Stop a running turn. Send while the agent is working.
- See everything that happens in the session, whichever door it came from.
- Open a long conversation quickly, page back through history, and survive a
  dropped connection without losing or duplicating messages.
- The same chat screen and wire format for all three tools.

## Non-goals for this part

- Answering permission requests, questions or plan approvals from chat. Chat
  shows that the session is waiting and points to Terminal.
- Attachments, slash-command and model menus, push notifications, diff viewer,
  usage sheets, side chat, dictation.
- Chat for sessions the dashboard cannot type into: external sessions, bare
  Terminal sessions, legacy (non-native) Codex sessions, and resumable
  sessions that are not running.
- Token-by-token streaming for Claude Code.
- Syntax highlighting in code blocks.

## Anti-goals

- Breaking or changing "Open in Claude" for any tool.
- Changing how Claude Code sessions are launched in a way that disables Remote
  Control. (Setting `ANTHROPIC_BASE_URL` to a non-Anthropic host does exactly
  that since Claude Code v2.1.196, so it is off the table here.)
- Losing a message, showing one twice, or sending one twice.
- Letting chat text act as terminal control input.
- A chat that feels like a polling page: visible full re-renders, scroll
  jumps, or a spinner where the Claude app shows progress.

## User-visible behaviour

### Entry and layout

Every running card for a supported session gets a **Chat** button. It becomes
the primary (accent) button; **Open in Claude** and **Terminal** stay on the
card beside it. Existing card behaviour that tests pin down (Open in Claude,
"Waiting for link", copy ID, no Terminal for external sessions) is unchanged.

- **Phone and narrow windows:** chat opens full screen over the dashboard, the
  way Terminal does. The header has a back arrow, the session title, the state
  chip, and one-tap buttons for Terminal and Open in Claude. The Android back
  button closes it.
- **Desktop and wide windows:** the dashboard becomes a left column using its
  existing narrow layout (launcher, meters, cards). Chat fills the right.
  Clicking another card's Chat button switches the conversation.

### Conversation

- User prompts appear as bubbles. Assistant text renders as Markdown with code
  blocks.
- Each tool call is one line: a verb, the target, and a running or finished
  state (for example "Running `npm test` 8s", "Edited README.md"). Consecutive
  calls of one kind may be grouped ("Read 3 files"). Tapping a row opens a
  detail sheet with the command or input, the output, and any error.
- While the agent works, a status line shows that it is working and for how
  long. Thinking is shown as status only.
- Subagent work is shown as the single tool row that started it. Its inner
  messages are not shown in this part.
- A shell command typed with `!` and its output are shown as a command row,
  not as a prompt bubble.
- Text that tools inject into the conversation for the model's benefit (system
  reminders, task notifications) is not shown as a user bubble.
- The view follows the newest message. If the user scrolls up, it stops
  following and shows a "jump to latest" control.

### Composer, queue and stop

- The composer sends plain text, including multi-line text.
- While the agent works, the placeholder changes to "Queue a message…" and the
  send button becomes **Stop**. Stop interrupts the running turn.
- A message sent while the agent works is **held by the dashboard**, not
  handed to the coding tool. This is the same for all three tools. Held
  messages are listed in order above the composer, each with three controls:
  - **Send now** hands the message to the session at once, whatever it is
    doing. While a turn runs this steers it: Claude takes the message at its
    next step, Codex and OpenCode receive it as steering.
  - **Edit** moves the text back into the composer and removes the message
    from the queue.
  - **Remove** deletes it.
- When a turn ends on its own, the dashboard sends the next held message, one
  per turn, in order.
- When a turn is stopped (from chat, Terminal or the Claude app), held
  messages are **not** sent. They stay listed until the owner uses Send now,
  Edit or Remove on each. A new message typed while the session is idle goes
  straight to the session; it does not release the held ones.
- If Stop is pressed before the session produced any reply, Claude Code hands
  the prompt back instead of keeping it. Chat then marks that message "not
  sent" and offers its text for editing, and the next message sent from chat
  must arrive on its own, not joined to the returned text.
- A prompt that begins with `/` or `!` is passed through unchanged. For Claude
  Code this means slash commands and shell commands work from chat, because
  the text reaches the real TUI.
- An empty or whitespace-only message is refused.
- A sent message shows "Sending" until the session's own record shows it,
  then looks like any other message. If that does not happen in time, it shows
  as not confirmed. See "Delivery" for retry rules.

### When the session needs an answer

If the session is waiting on a permission request, a question, or a plan
approval, chat shows a banner saying so with a button to open Terminal, and
the composer is disabled until the session moves on. Part 2 replaces the
banner with real controls.

### Three doors

A prompt typed in Terminal or in the Claude app appears in chat like any
other. A prompt sent from chat appears in Terminal and in the Claude app. Chat
never needs Remote Control to be connected.

### Connection and history

- Opening chat shows the most recent part of the conversation at once and
  loads older messages when the user scrolls up.
- A banner shows "Connecting", "Reconnecting" or "Offline" when the link to
  the agent is not healthy. When it returns, chat catches up from the last
  event it had, with nothing missing and nothing repeated.
- Several devices can have the same chat open. All see the same stream and
  any of them can send.
- If the conversation in the pane changes (for example `/clear` or `/resume`
  in the Claude TUI gives the pane a new session ID), chat resets to the new
  conversation.
- When the session ends, chat says so and stops accepting input.

## How it works

### Common shape

Every supported session has two things the agent can use:

1. **An ordered event record** of the conversation that can be read from any
   point. This gives history, live updates and catch-up after a reconnect
   from one mechanism.
2. **An input channel** for "send this text" and "stop".

The agent turns each tool's record into one normalized event stream and
serves it to clients. The normalized vocabulary follows the Claude Agent SDK
message shapes: `user`, `assistant` with `text`, `thinking` and `tool_use`
blocks, `tool_result`, turn start and end, and ephemeral text deltas. Reasons:
the Claude transcript already has this shape, the Codex and OpenCode bridges
already translate into it for Remote Control (`toolMessages`, `replayHistory`,
`historyMessages`), and the Claude app renders from it.

### Claude Code: tapping the TUI

Launch and resume are unchanged: an interactive `claude` in tmux with
`--remote-control` (`crates/agent/src/collect/spawn.rs`).

- **Event record:** the session transcript,
  `~/.claude/projects/<project>/<session-id>.jsonl`, found through the existing
  pane-to-transcript lookup (`lookup::session_file_for`, `transcript_for`).
  The agent only reads it.
- **Send:** tmux, through `Runner`. Load the text into a tmux buffer, paste
  it into the pane with bracketed paste (`paste-buffer -p`), then send Enter.
- **Stop:** send Escape to the pane.
- **Held messages:** the agent keeps the queue itself and pastes a message
  only when it is that message's turn (see "Delivery"). The TUI's own queue
  is not used for holding, because of what the second spike found.

Two throwaway spikes on 2026-11-12 (Claude Code 2.1.322, started the way the
dashboard starts it, on a private tmux socket) measured this:

| Check | Result |
|---|---|
| Pasted prompt accepted | 0.45 s; a multi-line prompt stayed one prompt |
| Paste while the TUI shows a suggested prompt | the suggestion is not included |
| Prompt sent mid-turn | queued by the TUI, picked up at the next tool boundary |
| Escape during a running tool, TUI queue empty | turn interrupted in 0.3 s |
| Escape during a running tool, one message in the TUI queue | tool stopped, then the queued message **ran at once** and was answered 1 s later |
| Escape 0.25 s after Enter, before any reply | the prompt went **back into the TUI input box**; nothing ran |
| Ctrl+U on the input box | clears back to the start of the line; about two presses per line of a multi-line text, one per wrapped row; harmless when empty |
| TUI "send now" chord (Ctrl+X Ctrl+S) with one queued message | running tool moved to the background, message delivered about 1 s later |
| `! echo …` and `/exit` pasted | both worked; `/exit` with a background shell running opens a confirmation dialog first |
| Transcript write delay | 0.1 to 0.2 s typical, 2.2 s worst seen |
| Hooks passed through `--settings` | command and HTTP hooks both fire with Remote Control active |

Transcript records the spikes saw, and what chat uses them for:

- `user` with string content: a prompt. With `tool_result` blocks: a tool
  result. With text "[Request interrupted by user…]": the turn was stopped.
- `assistant`: one line per content block (`thinking`, `text`, `tool_use`),
  several lines sharing one message ID.
- `system` with subtype `turn_duration` or `stop_hook_summary`: the turn
  ended.
- `queue-operation`: the TUI's own queue changed. `enqueue` carries the
  message text. `dequeue` is followed by an ordinary `user` record for that
  message. `remove` with reason `absorbed_mid_turn` means Claude took the
  message in the middle of a turn; in that case there is **no** `user`
  record, only an `attachment` record of type `queued_command` holding the
  text. Chat needs these to show mid-turn prompts from any door.
- `parentUuid` on every record: the conversation is a chain, not the file
  order. A prompt that Claude handed back (the Escape-before-reply case)
  stays in the file as a `user` record, with no marker, and the next prompt
  is written with the same parent. The reader must follow the chain from the
  newest record, so an abandoned record is not shown as part of the
  conversation. (Author's guess, not tested: rewinding a conversation in the
  TUI leaves abandoned records the same way.)
- `mode` and `permission-mode`: current modes.
- `isSidechain: true`: subagent traffic.
- Many other record types (`api-request*`, `ai-title`, other `attachment`
  types and more) are not conversation content and are ignored. A
  three-prompt session produced a 467 KB transcript, mostly such records, so
  reading must filter rather than load whole files.

**Session state file (found while planning, 2026-11-12).** The dashboard
already reads `~/.claude/sessions/<pid>.json` for the session ID and the
Remote Control link. On Claude Code 2.1.322 the same file also carries
`status` (`busy`, `idle`, `waiting`, `shell`), `statusUpdatedAt`, and
`waitingFor` (`dialog open`, `input needed`). A running turn showed `busy`;
idle sessions showed `idle`. Files written by older versions have no
`status`. Chat uses this as its working / waiting signal, because it also
covers dialogs and handed-back prompts that leave no transcript record, and
falls back to the transcript when the field is absent. How quickly the file
follows a turn end, a Stop, or a dialog was not timed; the plan's
real-session check covers it.

Hooks are **not** used in this part. The spike showed they miss interrupted
turns and tool calls the harness rejects before running, so the transcript
has to be the source of truth anyway, and it already carries prompt
acceptance, tool start and end, reply text and turn end. Hooks come in with
part 2, where answering prompts needs them. Not adding hooks also means the
Claude launch command does not change in this part.

**When Remote Control is unavailable.** The launch command keeps
`--remote-control`. Claude Code's documentation says that a session started
with that flag keeps running locally when Remote Control fails to connect or
is blocked for the account, so chat and Terminal still work. The builder
should confirm this where it can be reproduced (for example a start with no
route to claude.ai). If the TUI does not survive, that is a finding to bring
back to the owner, not a reason to change launch flags quietly.

Today a Claude card in that situation shows "Waiting for link…" indefinitely,
because `rc_unavailable` is always false for Claude panes
(`crates/agent/src/collect/sessions.rs`). In this part, when no link arrives
within the existing polling window, the card says Remote Control is
unavailable, as Codex and OpenCode cards already can, and Chat and Terminal
stay usable.

Known limits of the tap, accepted with option B:

- The transcript format is internal to Claude Code and can change between
  releases. The reader must skip what it does not understand, and chat must
  degrade to a clear "cannot read this session, use Terminal" state rather
  than show wrong content.
- Replies arrive one completed block at a time, not token by token.
- Pasted text lands wherever the TUI's focus is. If someone has half-typed
  text in the Terminal door, the two combine. If a dialog is open, the text
  goes to the dialog; the "needs an answer" banner and disabled composer
  exist to avoid that case.
- A prompt cannot carry an ID into the TUI, so matching a sent message to its
  transcript record is by content and order.
- A message that is already in the TUI's own queue is out of the dashboard's
  hands. That covers a message typed in Terminal or sent from the Claude app
  while a turn runs, and a held message after Send now until Claude picks it
  up. Pressing Stop then stops the running tool and Claude starts that
  message immediately. Chat shows such a message as sent and waiting, so the
  state is visible, but cannot hold it back.
- Some TUI dialogs leave no trace in the transcript, so chat cannot tell
  they are open. The one the spike met is the confirmation that `/exit`
  shows while background work is running. Text sent from chat at that moment
  goes to the dialog.

### Codex and OpenCode

Each of these already runs one engine per pane with a Node supervisor
(`codex-remote.mjs`, `opencode-remote.mjs`), a native TUI attached to it, and
a Remote Control worker that Claude's runner starts through
`CLAUDE_CODE_PROCESS_WRAPPER`.

Today the code that shapes conversation events lives in that worker, so it
only runs while Remote Control is connected. For chat it must run in the
supervisor, which is alive for the whole life of the pane and already holds a
client to the native engine. The Remote Control worker keeps doing its job
unchanged.

- **Event record:** produced by the supervisor from the native API (Codex
  app-server threads and turns, OpenCode session messages and events),
  including history for resumed sessions.
- **Send and stop:** through the supervisor to the native API, reusing what
  the worker does now: Codex `turn/start`, or `turn/steer` when a turn is
  active, and `turn/interrupt`; OpenCode `/prompt` with `delivery: "steer"`
  and its saved prompt IDs, and `/interrupt`. Steering is what **Send now**
  uses during a turn. A plain send during a turn is held by the agent, as for
  Claude. Messages arriving through Remote Control keep today's behaviour and
  steer straight away; the worker is not changed.
- **Token streaming:** forwarded from the native event streams as ephemeral
  deltas where the pinned tool versions provide them. If a tool does not,
  it falls back to completed blocks like Claude.

How the supervisor and the agent exchange events and input is left to the
builder, within these limits: local only, owner-only permissions as
`input.sock` has today, no new TCP listener, and no Claude or bridge
credentials crossing into tool environments.

### Agent chat endpoint

- One WebSocket route per open chat, registered in `guarded_routes!` so the
  configured auth guards cover it, with the same origin check the Terminal
  WebSocket uses (`crates/agent/src/collect/term.rs`). A WebSocket is needed
  because the desktop and Android apps' IPC bridge (`api_request`) cannot
  carry a stream; the Terminal already dials the agent directly through
  `agent_base`, and chat does the same.
- Every durable event carries a per-conversation sequence position. A client
  connects with the last position it has and receives everything after it. A
  client can ask for a page of events before a position.
- Ephemeral deltas are sent live and never replayed. The completed block
  that follows replaces them.
- Sends are serialized per session, so two clients cannot interleave pastes.
- Session state the chat needs (working, waiting for an answer, ended,
  conversation changed) and the list of held messages are pushed on the same
  connection, so every device sees the same queue and any of them can use
  Send now, Edit or Remove.
- The agent logs chat open, close and send events without message text,
  matching how the Terminal logs open and close but not typed input.

### Delivery

- A send is confirmed when the session's own record shows the prompt.
- **Claude Code sends are never retried automatically.** A paste cannot be
  made idempotent, so a blind retry risks a duplicate prompt. An unconfirmed
  message is shown as such, with a manual "send again".
- OpenCode and Codex sends can be retried safely where their APIs take a
  client message ID (`submitPrompt` in `opencode-remote.mjs`,
  `clientUserMessageId` for Codex).
- **Held messages** live in the agent, per session. The agent sends the next
  one when the session's record shows a turn ended on its own, and not when
  the record shows the turn was interrupted or the session is waiting for an
  answer. A held message is never handed to the coding tool before its turn,
  except by Send now.
- **Claude, prompt handed back.** When Stop lands before any reply, the TUI
  puts the prompt back into its input box. The agent must empty the input
  box before the next paste, and chat marks the message "not sent". The
  agent never sends a bare Enter and never pastes an empty message.

### Frontend

- Vanilla JavaScript and CSS with no build step, in the existing style and
  design tokens (`public/style.css`). New files under `public/` must be added
  to the embedded asset list in `crates/agent/src/http/serve.rs`.
- Chat is a view over the dashboard, opened and closed like Terminal
  (`history.pushState`, back closes it). Dashboard polling behaves as it does
  under the Terminal overlay.
- Rendering appends and updates nodes; it does not rebuild the conversation
  on each event. Streaming updates are coalesced per frame.
  `prefers-reduced-motion` is respected.
- The frontend guard tests slice `app.js` by literal anchors
  (`test/frontend-guards.test.mjs`); those anchors and tested card behaviours
  must survive. The test "the send bar is gone" forbids `/api/keys`,
  `send-input` and `.send` styles. It guards against the old one-line send
  bar, which was removed because it could not operate TUI menus. Chat does
  not bring that bar back, but the test must be revised so it no longer
  blocks a chat composer.

## Token streaming for Claude

Out of scope here, recorded so later parts can pick it up. The Claude app
streams replies token by token. Under option B there are two possible routes:

1. A local pass-through gateway set with `ANTHROPIC_BASE_URL`. Documented, but
   it disables Remote Control for that session, so it could only ever be a
   per-session opt-in.
2. `HTTPS_PROXY` with a local certificate authority. One third-party report
   says Remote Control keeps working. Undocumented and unverified; it means
   intercepting the user's own TLS traffic.

Part 6 is a spike of route 2. The wire format in this part must already carry
ephemeral deltas (Codex and OpenCode use them), so Claude streaming can be
added later without changing clients.

## Constraints and conventions

- No new package dependencies; `package.json` has none. Vendored single files
  under `public/vendor/` are the existing pattern (xterm).
- Agent subprocesses go through `Runner`; clippy forbids direct
  `std::process::Command`.
- Validate input at the HTTP edge, as the existing routes do.
- Mark deliberate shortcuts with `ponytail:` comments naming the ceiling.
- Tests use fakes and fixtures, never real provider accounts: `node --test`
  with fake engines, Rust unit tests with a stub `tmux`. CI runs `node --test`,
  `cargo test --locked` and clippy with warnings denied.
- Supported versions stay as they are today: Node 22 or later, Codex 0.162.1
  or later, OpenCode 2.1.4 or later within major version 2. The Claude tap was
  checked against Claude Code 2.1.322.

## Security

- Chat can drive a coding session that runs with broad permissions. The chat
  route gets the same guards and origin check as the Terminal, and nothing
  weaker.
- **Text sent to a Claude pane is terminal input.** Control characters other
  than newline and tab must be removed or the send refused before pasting. An
  escape sequence in the text could otherwise end bracketed paste early and
  be interpreted as keystrokes.
- **Everything rendered is untrusted:** model output, tool output, file
  contents. Markdown rendering must not pass through raw HTML, must restrict
  link schemes, and must escape by construction. A cross-site scripting hole
  here is control of the session.
- Credential isolation rules in the bridges stay: Claude and bridge
  credentials are never placed in a tool's environment or in state files.

## Evidence

- **Claude app feature inventory:** static analysis of the official Android
  app 1.261102.19, whose signing certificate matches the fingerprint Anthropic
  publishes. It is native Jetpack Compose with no web bundle. Its session
  protocol uses Agent SDK message types, token streaming through
  `stream_event`, optimistic send with delivery states, reconnect with
  sequence-number catch-up, and paged history. Those behaviours set the bar
  for this part. The inventory report is a local working file, not part of
  the repository.
- **Claude Code control channels:** the documented way to drive Claude Code
  from a program is headless mode with stream-json, which requires owning the
  process. Attaching a program to a live TUI is not documented, which is why
  option B reads the transcript and types through tmux.
- **Spikes:** results above. The first covered paste, queue, Escape with an
  empty queue, transcript timing and hooks. The second covered Escape with a
  message queued, Escape before any reply, clearing the input box and the
  TUI's own send-now chord; it is the reason the queue is held by the agent.
- **Claude Code 2.1.322 key table (read from the installed program, not
  tested):** `ctrl+l` is bound to `chat:clearInput`, `ctrl+x enter` to
  `chat:queueSubmit`, and `ctrl+enter` as well as `ctrl+x ctrl+s` to
  `chat:sendNow`.

## Author's calls

The owner has not decided these. They are the author's choices and are open
to change on review.

1. Chat becomes the primary card button.
2. Chat uses a WebSocket, as Terminal does, and shares the dashboard's origin
   and auth guards. Whatever path opens the dashboard carries chat.
3. Chat core reads the Claude transcript only; hooks wait for part 2.
4. The queue is held by the dashboard agent, not by the coding tool. This
   replaces an earlier call ("Queued" badge only, TUI holds the queue),
   which the second spike ruled out: with a message in Claude's own queue,
   Stop runs that message instead of stopping.
5. While a session waits for an answer, chat shows a banner and disables the
   composer.
6. No chat for external, bare Terminal, legacy Codex, or not-running
   sessions in this part.
7. The normalized event vocabulary follows Agent SDK message shapes.
8. Claude sends are never retried automatically.
9. Thinking is shown as a status line only; subagent inner messages are
   hidden.
10. No syntax highlighting yet. Markdown comes from a small vendored
    single-file parser with raw HTML disabled, or a minimal renderer written
    for the purpose; the builder picks, and the security rules above apply
    either way.
11. The frontend stays vanilla JavaScript. "As fluid as the Claude app" is
    pursued through incremental rendering and live updates, not a rewrite in
    a framework or native UI.
12. Claude launch keeps `--remote-control` for every login. A Claude card
    whose link never arrives shows "Remote Control unavailable" instead of
    waiting forever. There is no switch to launch without Remote Control.
13. One queue behaviour for all three tools. For Codex and OpenCode this is
    a change from what their Remote Control bridges do: a chat message sent
    during a turn waits instead of steering at once, and steering takes one
    more tap (Send now). The gain is one rule everywhere and a Stop that
    never starts new work. The alternative is to keep instant steering for
    those two tools and hold only for Claude.
14. Held messages are sent one per turn, in order, and are kept in the
    agent's memory only. If the agent restarts, the queue is gone; the device
    that wrote a message still has its text and shows it as "not sent".

## Left to the builder

- The exact WebSocket frames, and how sequence positions are derived for each
  tool.
- How the Codex and OpenCode supervisors hand events to the agent and take
  input from it.
- How much history the first screen loads, the page size, and how transcript
  reading stays fast on large files.
- Matching sent messages to transcript records, and the confirmation timeout.
- The width at which the layout switches from full screen to two columns.
- Tool row wording and grouping rules, and the detail sheet layout.
- Whether the short pause between paste and Enter that the spike used is
  needed.
- How the agent notices that Claude handed a prompt back (no interrupt
  record follows the prompt; the text is visible in the pane), and which
  keys empty the input box. Ctrl+U was measured and needs repeating;
  `ctrl+l` is listed as `chat:clearInput` and was not tested.
- How Send now reaches Claude during a turn: a plain paste and Enter (taken
  at the next tool boundary, measured) or the TUI's send-now chord (running
  tool moved to the background, delivered in about a second, measured with
  a message already queued).
- File and module layout, within the conventions above.

## Acceptance checks

- With Remote Control unavailable, chat can send, receive and stop for
  Claude Code, Codex and OpenCode.
- With Remote Control available, "Open in Claude" works as before for all
  three, and a prompt sent from any one door shows in the other two.
- A prompt typed in Terminal appears in chat within about a second for Codex
  and OpenCode, and within about three seconds for Claude.
- Dropping the connection mid-turn and reconnecting yields the full
  conversation with no gaps and no duplicates.
- Stop ends a running turn within about a second.
- With a message held while any of the three tools works, Stop ends the turn
  and the held message does not run. It stays listed with Send now, Edit and
  Remove.
- When a turn ends on its own, the next held message is sent without a tap.
- For Claude, a message stopped before any reply shows as "not sent" with
  its text recoverable, and the next message sent from chat reaches Claude
  alone, not joined to the earlier text.
- A message typed in Terminal while Claude works appears in chat once Claude
  takes it.
- A message containing escape sequences cannot inject keystrokes into a
  Claude pane, and Markdown containing HTML or script cannot execute.
- A Claude transcript with unknown record types still renders the parts chat
  understands.
- `node --test`, `cargo test --locked` and clippy pass.
