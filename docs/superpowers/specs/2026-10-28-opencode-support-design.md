# OpenCode sessions with native Terminal and Claude Remote Control

Status: proposed; awaiting design approval. No product code has changed.

## Intent and agreed scope

Add OpenCode as a third coding tool alongside Claude Code and Codex. The user
selected the Codex-style experience, explicitly including **Open in Claude**.
The same OpenCode session must accept work from its native terminal and Claude
Remote Control, retain its history, and survive a Claude connection outage.

The implementation uses the existing Rust agent, Node bridge, tmux lifecycle,
browser terminal, and dashboard components. It adds no package dependencies.
OpenCode owns its provider credentials, session storage, models, and tool policy;
Claude continues to own Remote Control registration and authentication.

## Approaches

1. **Codex-style integration, selected by the user.** Share the concrete Claude
   worker transport and terminal patterns. Add an OpenCode API adapter and
   supervisor, with one OpenCode backend/session for both clients. This delivers
   Open in Claude without duplicating the coding agent or interpreting its TUI.
2. **OpenCode web handoff.** Launch OpenCode and link to its web interface. This
   would be smaller but does not deliver the selected Open in Claude workflow.
3. **Headless prompts only.** Run a fresh `opencode run` process for each prompt.
   This avoids a native client but does not provide the interactive terminal or
   shared running session that the existing Codex integration provides.

## Current evidence and compatibility

The checkout and remote default branch both resolve to `master` at `99c21f8`.
The installed Linux executable reports `opencode v2.1.4`. Its help includes
`--server`, `--session`, `serve --stdio --port`, and the `api` command.

OpenCode V2 has HTTP endpoints for sessions, model selection, prompts, message
history, interruption, permissions, usage, and events. Its event stream explicitly
does not replay events missed during disconnection. The installed database has
`session_v2` and `session_message` tables, distinct from legacy V1 tables.

The author's compatibility choice is OpenCode **2.1.4 or later within major
version 2**, with required API capabilities checked. V1 and unknown future major
versions receive an actionable unsupported-version error before a pane is
created. This minimum is a qualification baseline, not a claim that every later
minor version has already been tested.

Sources checked on 2026-10-28:

- [V2 CLI](https://opencode.ai/v2/docs/cli/)
- [V2 API](https://opencode.ai/v2/docs/api)
- [V2 commands](https://opencode.ai/v2/docs/cli/commands/)
- [OpenCode standalone server ownership](https://github.com/anomalyco/opencode/blob/dev/packages/cli/src/services/standalone.ts)
- [OpenCode server authentication](https://github.com/anomalyco/opencode/blob/dev/packages/cli/src/services/server-connection.ts)

The OpenCode sources show the standalone server protocol: a random
`OPENCODE_PASSWORD`, `serve --stdio --port 0`, a JSON readiness line containing
the URL, HTTP Basic authentication as `opencode`, and stdin EOF ending the server
lease. Validate this protocol against the installed release before depending on
it; the linked development branch is supporting evidence, not a pinned contract.

## User-visible behavior

- The existing model picker includes clearly prefixed OpenCode models. Preserve
  provider identity, for example `opencode:anthropic/claude-sonnet-5-5`, even when
  Claude Code or Codex can use a model with the same underlying name.
- OpenCode model variants come from OpenCode's catalog. Label its secondary
  picker **Variant** rather than pretending every variant is a Claude/Codex
  effort. Hide that picker when no selectable variants exist. Preserve the saved
  model and variant on resume unless the user deliberately changes them.
  Resolve the catalog for the selected project location and revalidate the
  choice on launch, since project configuration can change available models.
- Launch creates one named OpenCode session in the chosen directory. Its running
  card offers Terminal and, once connected, Open in Claude. Working state, the
  latest reply, model, variant, Git information, and host resource usage remain
  consistent with the existing cards.
- Terminal shows the actual OpenCode TUI. Closing the browser terminal detaches;
  it does not stop the session. TUI navigation bytes never become chat prompts.
- Open in Claude supports text prompts, replies, intermediate tool starts and
  results, Working state, interruption, and supported permission approvals.
  Unsupported attachments and interactive forms produce an explicit instruction
  to use Terminal, without silently executing or approving anything.
- Resumable cards retain the OpenCode session ID, directory, model, variant,
  preview, and generated summary through the existing summary workflow. Resume
  reuses the same OpenCode session rather than creating a fork or new thread.
- Provider usage meters appear only for quota information OpenCode actually
  reports. A provider with no quota data contributes no fabricated usage tile.
- Purge hides a card, as it does today. It never deletes the OpenCode session.

## Backend and process ownership

Add `collect/opencode.rs` for optional-tool discovery, catalogs, owned session
metadata, history, launch, and resume. Use `opencode:<provider>/<model>` picker
values and a separate validated `ses_...` ID path. Do not loosen the existing
UUID validator for Claude/Codex. Reject cross-tool resume choices on the server.

Add `collect/opencode-remote.mjs` for OpenCode API access, event translation,
native supervision, and its Claude worker entry point. Extract only concrete
Claude transport code that both bridges use into a shared module. Keep Codex
RPC, native transport, and legacy-session handling in the Codex implementation;
do not introduce a general provider/plugin framework or emulate Codex RPC.

Each managed pane has one private OpenCode server and one native OpenCode TUI.
Start the server with a random password and a loopback ephemeral port using its
stdio ownership lease. Validate the readiness URL as loopback HTTP with an
actual assigned port. Do not expose this backend as a new dashboard/public route.
Create or load the session through that server, then start
`opencode --server <url> --session <sid>` in the pane. The same server handles
the Claude bridge. Model and variant selection happen through OpenCode's session
API, not unsupported TUI flags or synthetic inference prompts.

The supervisor owns the TUI, backend lease, status subscriber, and Claude runner.
Exiting the TUI or killing the pane interrupts its active work and cleans up only
these owned processes. Do not restart, stop, or reconfigure the user's shared
OpenCode service or other sessions. A Claude runner failure removes a stale link
and retries using the existing Codex backoff and continue-session behavior;
Terminal and native metadata remain usable throughout the outage.

Store bridge state under a private `CLAUDE_DIR/cdash-opencode` directory, following
the existing managed Codex convention. Use atomic files, restrictive permissions,
and separate native metadata from Remote Control delivery checkpoints. Accept
snapshots only when both PID and session ID match their active pane.

Strip Claude worker credentials and bridge control variables from OpenCode and
its tools. Keep the backend password out of URLs, argv, persisted state, and logs;
pass it through OpenCode's supported environment/authentication mechanism. Verify
how OpenCode isolates its own lease credential from tools before qualification.
Keep Remote Control logs separate from the native terminal. Reuse the existing
HTTP authentication, terminal Origin checks, and tmux name validation.

## Events, replay, and permissions

Translate OpenCode's native text, execution, and tool events into the Claude
worker messages already used by Codex. Include tool starts before completion,
errors, and active execution state. Omit reasoning, private provider metadata,
credentials, and unsupported binary content; retain the existing bounded tool
output policy. Treat transcript content as data rather than bridge instructions.

Use stable outbound message UUIDs scoped to the Claude remote identity and native
OpenCode message/tool IDs. Give each Claude-origin prompt a stable OpenCode input
ID and save its association before submission. A reconnect must never execute
an already accepted prompt again or create duplicate user bubbles.

Match Codex's shared-turn behavior: a Claude prompt arriving during execution
uses OpenCode's native `steer` delivery rather than interrupting the turn,
starting a second backend, or forking the session. OpenCode owns admission and
safe-boundary scheduling for concurrent native input. Claude Stop interrupts
the current execution of that shared session, including work started from
Terminal. Submission and interruption tests must exercise both input origins.

Subscribe before history catch-up, buffer live events during replay, and reconcile
against paginated authoritative message history. Repeat reconciliation after
every OpenCode event-stream reconnect; its stream is volatile and a saved SSE
sequence cannot recover missed native events. Restore current execution and
pending permission state as part of that reconciliation. Only checkpoint a
completed history upload after delivery succeeds. Heartbeats run during replay.

Map supported permission questions into Claude approval controls. Recheck that a
request is still pending before applying its answer. Resolution from Terminal
cancels the matching Claude control; late replies must have no effect. Preserve
OpenCode's configured policy and explicit denies. Do not globally enable
auto-approval as a shortcut for this integration.

## History, summaries, and polling

Match Codex's current ownership scope: initially list sessions launched by this
dashboard, including their later continuation in OpenCode. Record ownership using
session metadata and the local managed-session index; preserve it across agent
restarts. Exclude child and archived sessions. Do not silently import or take
ownership of every unrelated OpenCode session on the host.

Read current metadata and history from OpenCode's APIs. For stopped owned
sessions, use a bounded metadata-only OpenCode query server when needed and
close its lease afterward; cache catalogs and history following Codex's existing
TTLs. No model inference runs during discovery or routine polling. A missing,
signed-out, unsupported, or broken OpenCode installation must not break Claude
or Codex model discovery, session collection, or launch.

Materialize text-only normalized transcript snapshots for owned sessions in the
private state directory and pass these through the existing `SummaryCache` and
summary generator. Preserve original OpenCode data. Refresh snapshots only when
conversation content changes so metadata polling does not trigger inference.
Use readable prompt previews while a summary is absent or generation fails.
Retain the existing subscription-backed summary routing and failure backoff.

## Files and contracts affected

- New OpenCode Rust/Node integration and focused test fixtures.
- `collect/mod.rs` and `ctx.rs`: register the module and bounded caches.
- `collect/spawn.rs` and `validate.rs`: launch/resume dispatch and separate ID/model
  validation, including purge without widening existing UUID acceptance.
- `collect/sessions.rs`: managed running/resumable cards, previews, and optional
  usage; `external.rs` only if the shared serialized shape needs an additive field.
- `http/routes.rs`: combine optional model catalogs and accept validated OpenCode
  launch/resume requests while preserving existing routes and error shapes.
- `codex-remote.mjs` and any extracted shared transport: preserve Codex behavior.
  Account for both embedded `node -e` metadata queries and materialized modules.
- `summaries.rs` and `summary.mjs`: normalized text support and installation of
  any newly shared module required by the copied Codex summary bridge.
- `public/app.js`: tool-specific picker filtering, variants, cards, and correct
  resume payloads; existing terminal components and authentication stay shared.
- Relevant Rust, Node bridge/native/summary/frontend tests and README setup notes.
  Update native wrapper code only if embedding a new asset actually requires it.

## Validation and completion

First qualify the installed OpenCode server lease, authenticated API, explicit
session creation, model/variant round-trip, native TUI attachment, event shapes,
pagination, and interruption. Use isolated project/session fixtures. A failed
protocol assumption changes the adapter before wider implementation proceeds.

Focused automated checks cover absent OpenCode, version rejection, catalog
isolation, provider/model identity, IDs and malicious inputs, cross-tool resume
rejection, saved variants, process ownership/cleanup, credential isolation,
text/tools/errors, live Working state, approvals and stale answers, interruption,
duplicate suppression, reconnect/history races, previews, summaries, and purge.
Existing Claude and Codex regression suites remain required.

Run the applicable Rust and Node suites, strict Clippy, JS syntax checks, and
`git diff --check`. Run a real isolated launch, Terminal interaction, Claude
Remote Control round-trip, reconnect, and resume against the same native session
before claiming end-to-end support. Report separately any unavailable browser,
physical Android, provider, or live Claude validation. Do not replace the user's
running agent or publish a release as part of this implementation request.

## Builder decisions and limits

The builder owns concrete helper boundaries, fixture construction, cache layout,
and the minimal additive JSON fields needed for variants. The compatibility
baseline, private backend lifecycle, managed-history scope, and text-only Claude
input policy above are the author's proposals, not additional user statements.
The builder check found one ambiguity about concurrent input; the shared-turn
and Stop behavior above resolve it by following the existing Codex model.

OpenCode login/account management, native OpenCode web handoff, V1 compatibility,
arbitrary external-session takeover, new model providers, and attachment/form
translation are outside the selected Codex-equivalent slice. No original
transcripts or credentials are rewritten. Rollback is removal of this isolated
feature branch; live sessions are not migrated or restarted automatically.
