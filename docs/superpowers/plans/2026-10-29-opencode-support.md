# OpenCode Support Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add OpenCode sessions with native Terminal and Open in Claude, sharing the same session, model, history, and execution.

**Architecture:** Keep the existing Rust agent, tmux terminal, and subscription-backed summary pipeline. Add a concrete OpenCode V2 API client and native/Remote Control supervisor; extract only the Claude worker transport shared with Codex. One authenticated private OpenCode server per managed pane serves both clients.

**Tech Stack:** Rust 1.94.1, Tokio/Axum/Serde, Node.js 22+, native fetch and child processes, tmux, existing browser JavaScript/xterm.js.

**Spec:** `docs/superpowers/specs/2026-10-28-opencode-support-design.md`

## Global Constraints

- OpenCode **2.1.4 or later within major version 2**, with required API capabilities checked; reject V1 and unknown future major versions before creating a pane.
- No new package dependencies, direct OpenCode database coupling, generic provider framework, or emulated Codex RPC.
- One native OpenCode TUI and one private server/session per pane; preserve the native session ID on resume.
- Preserve **Open in Claude**, intermediate tool progress, Working state, interruption, and supported approvals.
- Use `opencode:<provider>/<model>` values; split provider/model at the first `/` only. OpenCode variants are not Claude/Codex efforts.
- Claude allow becomes `once`, deny becomes `reject`; never persist `always` rules or globally enable auto-approval.
- Strip Claude ingress credentials and bridge control variables from OpenCode/tool environments. Backend passwords never enter argv, URLs, state files, or logs.
- Keep native state separate from Claude delivery checkpoints. Claude outages never kill the TUI/backend.
- Discover only dashboard-owned, top-level, non-archived OpenCode sessions. Purge hides; it does not delete.
- Normalize text-only snapshots to the existing Claude JSONL format; leave `summary.mjs` behavior and summary provider routing unchanged.
- No OpenCode usage meters, V1 compatibility, external-session takeover, attachment/form translation, live agent replacement, or release publication.
- Concurrent execution of one session through separate OpenCode servers is unsupported; do not claim cross-server coordination.

## Review Focus

1. A Claude submission survives a worker crash while still in the OpenCode inbox: resume must not run it twice. Pin inbox/history checks in Task 3.
2. A native turn completes during history upload or SSE reconnection: final status and messages must be correct without duplicate tools. Pin replay races in Task 3.
3. Project/remote-host selection changes while catalogs load: an old response must not replace the new project's choices. Pin stale responses in Task 5.
4. A saved model/variant disappears from the catalog: unchanged resume preserves the pair; a changed choice is freshly validated. Pin both in Tasks 4 and 5.
5. Missing Node/OpenCode, corrupt metadata, or failed discovery must leave Claude/Codex usable and create no phantom panes. Pin failure isolation in Task 4.

## Files and Interfaces

New files:

- `crates/agent/src/collect/opencode-api.mjs`: private server lease, authenticated API requests, bounded pagination, SSE transport, catalog normalization.
- `crates/agent/src/collect/opencode-remote.mjs`: query modes, managed metadata/transcript snapshots, native supervisor, Claude worker/event mapping.
- `crates/agent/src/collect/claude-remote.mjs`: the existing concrete `Remote` worker transport, exported for both bridges.
- `crates/agent/src/collect/opencode.rs`: optional-tool checks, script materialization, cached catalogs/history, launch/resume, pane snapshots.
- `test/opencode-api.test.mjs`, `test/opencode-remote.test.mjs`, `test/opencode-native.test.mjs`, `test/opencode-live.test.mjs`: focused protocol, bridge, lifecycle, and opt-in live qualification checks.
- `crates/agent/tests/opencode_integration.rs`: authenticated route and serialized-response checks.

Modify existing files only where the tasks below require them. Keep Codex-specific RPC, legacy access rules, authentication, and terminal components in their existing modules.

### Task 1: Qualify and implement the private OpenCode API client

**Files:** Create `collect/opencode-api.mjs` and `test/opencode-api.test.mjs`; create the API-only cases in `test/opencode-live.test.mjs`. All `collect/` paths here are relative to `crates/agent/src/`.

**Interfaces:**

- `startServer({ cwd, env, onError }): Promise<OpenCode>` starts only an owned `opencode serve --stdio --hostname 127.0.0.1 --port 0` child and returns its authenticated client.
- `OpenCode({ url, password })` attaches to that same backend without owning it.
- `client.request(path, { method = 'GET', body, signal } = {}): Promise<object | undefined>` decodes successful responses; rejects sanitized HTTP/transport errors.
- `client.subscribe(onEvent, onConnected, signal): Promise<void>` subscribes/reconnects to `/api/event`, calls `onConnected` after headers on every connection, and delivers parsed events in order. It does not claim durable event replay.
- `client.close(): Promise<void>` closes an owned lease and process group; an attaching worker never kills the server.
- `normalizeModels(payload): ModelChoice[]`, where each choice has `{ value, model, label, provider: 'opencode', variants: string[], defaultVariant: string | null }`. `model` is the canonical `<provider>/<native-model-id>` without the dashboard's `opencode:` prefix; an unspecified/default variant is `null`.

- [ ] **Step 1: Write failing API tests.** Assert the following cases using a fake CLI plus local HTTP server, without provider requests:

  ```js
  assert.equal(choice.value, 'opencode:nvidia/deepseek-ai/deepseek-v4.1-flash');
  assert.deepEqual(choice.variants, ['fast', 'deep']);
  assert.equal(startArgs.includes('--service'), false);
  assert.equal(startArgs.some(a => a.includes(password)), false);
  assert.equal(await rejectsNonLoopbackReadyURL(), true);
  assert.equal(await ownedServerExitsAfterClose(), true);
  ```

  Also pin readiness timeout/early exit, invalid JSON, malformed SSE, repeated pagination cursors, abort cleanup, credential-free errors, and a worker attachment closing without terminating its backend.
- [ ] **Step 2: Run `node --test test/opencode-api.test.mjs`.** Expect failure because the new module/exports do not exist.
- [ ] **Step 3: Qualify installed 2.1.4 in an isolated temporary data/config/project directory.** Capture sanitized fixtures for readiness, Basic auth, `/api/info`, model/ref/variant shapes, session create/get, model/variant selection round-trip, message/inbox pagination, permission reply, and SSE. Attach the actual native TUI to the fixture session without submitting an inference prompt. Verify that plain `serve --stdio` does not invoke managed-service recovery or resume unrelated work. Verify stdin EOF cleanup. Do not use `opencode service start/restart` or mutate the user's store.
- [ ] **Step 4: Implement the interfaces against those release fixtures.** Generate a random password, enforce loopback readiness, keep draining stdout, preserve lease ownership, and use bounded requests/pages/events. Do not add the npm SDK. Project location is explicit in model/session requests.
- [ ] **Step 5: Run `node --test test/opencode-api.test.mjs` and `CDASH_OPENCODE_INTEGRATION=1 node --test --test-name-pattern='OpenCode API qualification' test/opencode-live.test.mjs`.** Name the Task 1 live case `OpenCode API qualification`. Expect all assertions to pass, one created fixture session, zero inference requests, and no surviving owned child. Record version, commands, and actual protocol differences in the execution ledger below. A disproved assumption changes this client before Task 3 starts.
- [ ] **Step 6: Commit only this deliverable** as `feat: add private OpenCode API client`.

### Task 2: Share the existing Claude worker transport without changing Codex

**Files:** Create `collect/claude-remote.mjs`; modify `collect/codex-remote.mjs`, `collect/codex.rs`, `collect/summaries.rs`, and the existing Codex/summary tests that exercise copied or embedded scripts.

**Interfaces:** Export `Remote` with its existing constructor `(url, token, epoch, sid)` and existing `request`, `retry`, `state`, `event`, `ack`, `listen`, `sequence`, `model`, and `stopped` behavior. Default model metadata remains `codex` for existing clients. The OpenCode worker sets its actual model before publishing state.

- [ ] **Step 1: Write failing packaging regressions.** Exercise the copied summary `bridge.mjs` and a materialized Codex worker with the new transport module absent. Assert an explicit missing-module failure; then require the publisher to install that module alongside each script. Retain a metadata `node -e` catalog/usage test that runs from an unrelated cwd.
- [ ] **Step 2: Run `node --test test/codex-remote.test.mjs test/codex-stream.test.mjs test/summary.test.mjs`.** Confirm the new packaging assertions fail for the expected missing module/publishing behavior.
- [ ] **Step 3: Move only `Remote` and its local dependencies into `claude-remote.mjs`.** Load it inside Codex's worker path so embedded metadata queries do not acquire a relative top-level import. Materialize the shared module both on Codex launch and in `summaries.rs`; leave Codex RPC and stable UUID logic in place.
- [ ] **Step 4: Run the same Node tests plus `cargo test -p cdash-agent --locked summaries`.** Expect existing transport/retry/token rotation/history behavior and summary routing to pass, including copied-script execution.
- [ ] **Step 5: Commit** as `refactor: share Claude Remote Control transport`.

### Task 3: Run one native OpenCode session through Terminal and Claude

**Files:** Create `collect/opencode-remote.mjs`, `test/opencode-remote.test.mjs`, and `test/opencode-native.test.mjs`; extend the opt-in live cases from Task 1.

**Interfaces:**

- CLI `--models --dir <absolute-dir>` prints normalized Task 1 catalog JSON and exits without inference.
- CLI `--threads --root <private-root> --limit <N>` returns current owned history rows; `N` is capped at 100. Rows are `{ sid, dir, ts, display_name, model, model_label, variant, prompts, source_file }`.
- CLI `--state <path> --name <tmux-name>` supervises the native session. Initial state is `{ dir, display_name, sid: null | string, model, variant }`, using Task 1's canonical model and nullable variant; saved state adds `{ pid, native_terminal: true, working, last_message, ts, rc_unavailable }`.
- CLI `--worker` uses Claude's existing authenticated child arguments plus private `CDASH_OPENCODE_STATE`, `CDASH_OPENCODE_URL`, and `CDASH_OPENCODE_PASSWORD` environment variables. These are removed from OpenCode/tool environments.
- Ownership is recorded per ID in `<root>/<sid>.owned.json`, plus OpenCode session metadata. Pane state/checkpoints use `<name>.json`; native status uses `<name>.native`; the ready Claude link uses `<name>.link`. Text snapshots use `<sid>.jsonl`.

- [ ] **Step 1: Write failing cross-process tests.** Reuse the Codex suites' fake CLI/preloaded transport style. Assert one backend and one native TUI; both clients address the same session. Native text/tool input mirrors into Claude, Claude text uses stable IDs and `steer`, and Claude Stop interrupts native-origin work. Assert output tool text is capped at 65,536 characters; reasoning/private metadata never appears.
- [ ] **Step 2: Add delivery/approval race assertions.** A submission present in the inbox or history is never resubmitted after a worker crash; lost HTTP responses retry the same input ID. Replay/live UUIDs match. A turn ending during initial upload/replay leaves idle status and exactly one completion. Restored permissions appear after reconnect; Terminal resolution cancels Claude's control; late replies have no effect; allow is `once`, deny is `reject`, never `always`.
- [ ] **Step 3: Run `node --test test/opencode-remote.test.mjs test/opencode-native.test.mjs`.** Expect failures because the supervisor/worker do not exist.
- [ ] **Step 4: Implement native supervision and query modes.** Use Task 1's private lease, actual session create/load/model APIs, and `opencode --server <url> --session <sid>`. Publish native metadata independently of Claude; keep credentials in process memory/environment only. Keep backend/Claude logs away from the TUI. Retry Claude at Codex's existing backoff, retaining the remote identity when reconnecting. Clean up owned groups and the lease on TUI exit, pane kill, signals, or startup failure.
- [ ] **Step 5: Implement worker event mapping and reconciliation.** Reuse Task 2's `Remote`. Subscribe before paginated catch-up and buffer live events. Reconcile message history, inbox, current execution, model, and permissions after every OpenCode reconnect. Persist incoming Claude input association before submission and check both inbox/history on recovery. Checkpoint only successful uploads; run heartbeats during replay. Unsupported forms/attachments tell the user to use Terminal and execute nothing.
- [ ] **Step 6: Implement managed history and normalized snapshots.** Read only owned, top-level, unarchived sessions through bounded metadata API leases. Write Claude-shaped text JSONL atomically, mode 0600, only when content changes. Keep stale readable snapshots on transient query errors, never rewrite original OpenCode data, and preserve the displayed model/variant when continued elsewhere.
- [ ] **Step 7: Run both new suites and Task 1 API tests.** Require zero leaked children, no plaintext secrets in output/state, unchanged native metadata during Claude outage, no lost/duplicate accepted prompts, and no inference in metadata-only modes.
- [ ] **Step 8: Commit** as `feat: bridge native OpenCode sessions to Claude`.

### Task 4: Add Rust launch, resume, history, and HTTP support

**Files:** Create `collect/opencode.rs` and `crates/agent/tests/opencode_integration.rs`; modify `collect/{mod,ctx,validate,spawn,sessions,external,codex,summaries}.rs` only for registration, additive fields, dispatch, or affected fixtures/callers; modify `http/routes.rs`.

**Interfaces:** All Rust I/O functions below are asynchronous.

- `opencode::models(ctx: &Ctx, dir: &str) -> Result<Vec<ModelChoice>, Refused>`; Task 1's JSON fields plus Rust Serde definitions. Cache by absolute project directory, TTL 60 seconds, maximum 32 directory entries.
- `opencode::history(ctx: &Ctx, limit: usize) -> Vec<Resumable>` caches bounded owned metadata for 10 seconds; each row's `source_file` points at its normalized snapshot.
- `opencode::snapshot(ctx: &Ctx, name: &str, pid: i32) -> Option<OpenCodeSession>` verifies both the pane PID and matching native/session IDs; `opencode::link(ctx: &Ctx, name: &str) -> Option<String>` reads the ready link.
- `opencode::launch(ctx: &Arc<Ctx>, dir: &str, model: &str, variant: Option<&str>, previous: Option<OpenCodeSession>) -> Result<String, Refused>` receives Task 1's canonical model after dispatch removes `opencode:`; it validates version, binaries, directory, current selection, and ownership before starting tmux.
- `opencode::resume(ctx: &Arc<Ctx>, sid: &str, model: Option<&str>, variant: Option<&str>) -> Result<String, Refused>` accepts only a valid, owned native OpenCode ID.
- Add final `variant: Option<&str>` parameters to `spawn::launch_session` and `spawn::resume_session`; update every caller and existing test with `None` where appropriate. Existing effort behavior stays unchanged for Claude/Codex.
- Add optional `variant` to HTTP launch/resume bodies and running/resumable JSON, omitted for other tools. Omitted variant on resume preserves it; `""` explicitly selects the model default; a nonempty value selects that named variant. OpenCode does not consume the `effort` field.
- `GET /api/models?dir=<absolute-dir>` keeps the existing array response and appends OpenCode choices. Missing `dir` uses the host home directory. Validate explicit directories before subprocess use. Log an optional-tool discovery failure and preserve other tools' choices.

- [ ] **Step 1: Write failing Rust tests.** Assert a missing/signed-out/unsupported/broken OpenCode returns no phantom panes and does not block existing tools. Assert `opencode:nvidia/deepseek-ai/deepseek-v4.1-flash` retains its model ID. Separate `assert_opencode_sid(&str)` accepts `ses_` plus a bounded ASCII alphanumeric/underscore/hyphen suffix; reject slashes, traversal, empty suffixes, whitespace, oversized IDs, and shell metacharacters. Keep Claude/Codex UUID tests unchanged.
- [ ] **Step 2: Add resume/cache/response assertions.** Reject cross-tool models and unowned OpenCode IDs. Preserve an unchanged saved pair removed from the catalog; freshly validate deliberately changed selections. Keep cache entries project-specific and bounded. Reject stale PID/native snapshots. Running IDs are excluded from resumable cards; purge only hides; a deliberate resume unpurges. Assert OpenCode summary snapshots reach `SummaryCache`, with prompt fallback and no `summary.mjs` parser change.
- [ ] **Step 3: Run `cargo test -p cdash-agent --locked opencode`.** Expect compile/test failures identifying absent interfaces/dispatch.
- [ ] **Step 4: Implement the module, caches, and dispatch.** Materialize `opencode-remote.mjs`, `opencode-api.mjs`, and `claude-remote.mjs` in the private state directory; use existing bounded `Runner` calls. Dispatch OpenCode IDs before the UUID-only resume path, and validate purge IDs according to their tool. Preserve exact Claude/Codex behavior and the existing terminal/authentication routes. Use metadata snapshots rather than running model inference on request paths.
- [ ] **Step 5: Implement guarded route tests.** Assert catalog isolation, old launch/resume payload compatibility, OpenCode variant omission/default semantics, invalid-path/ID rejection before spawn, and unauthorized access rejection using the existing integration harness.
- [ ] **Step 6: Run `cargo test -p cdash-agent --locked`.** Expect all old and new tests to pass, including summaries, Codex, authentication, and terminal refusal cases.
- [ ] **Step 7: Commit** as `feat: expose managed OpenCode sessions in the agent`.

### Task 5: Add project-aware OpenCode model and variant controls

**Files:** Modify `public/app.js` and `test/frontend-guards.test.mjs`; use the existing dropdown, polling, folder picker, terminal, and card rendering.

**Interfaces:** `refreshModels(g, dir)` obtains `/api/models?dir=...` for the current host generation and directory. Keep a bounded per-directory catalog map for OpenCode cards; load a card's catalog when its model picker is opened. `pickState(s)` additionally returns `{ variant, variants, isOpenCode }`; `picks[s.sid].variant` preserves deliberate choices across renders.

- [ ] **Step 1: Write failing frontend assertions** using the existing VM fixture style:

  ```js
  assert.match(openCodeCard, /Open in Claude/);
  assert.match(openCodePickerWithVariants, /Variant/);
  assert.doesNotMatch(openCodePickerWithoutVariants, /Effort|Variant/);
  assert.equal(openCodePayload.variant, 'deep');
  assert.equal('effort' in openCodePayload, false);
  assert.equal(claudePayload.effort, 'high');
  assert.equal('variant' in claudePayload, false);
  ```

  Also assert tool-specific model filtering, preserving missing saved model/variant values, `variant: ''` for an explicit default, and clearing a variant when deliberately switching to a model that does not support it.
- [ ] **Step 2: Add stale-response assertions.** Switch project or host while discovery is pending; old results cannot replace current choices. Opening an OpenCode resume picker loads its own directory catalog rather than the launcher's. Failed OpenCode discovery preserves Claude/Codex and the saved resume pair.
- [ ] **Step 3: Run `node --test test/frontend-guards.test.mjs`.** Expect the new OpenCode assertions to fail.
- [ ] **Step 4: Implement the smallest UI changes.** Use the existing custom dropdown and keyboard behavior; update its visible/accessibility label for Variant and hide it when no variants exist. Send only the applicable effort/variant field. Prefix OpenCode model labels, show its variant chip, and reuse Terminal/Open in Claude/waiting/fallback cards. Do not add screens, terminal behavior, or a second permission UI.
- [ ] **Step 5: Run `node --test test/frontend-guards.test.mjs test/polling.test.mjs test/keys.test.mjs test/theme.test.mjs`.** Expect accessible dropdowns, expanded summaries, host/project races, and old tool behavior to pass. Run `node --check public/app.js`.
- [ ] **Step 6: Commit** as `feat: add OpenCode model and variant controls`.

### Task 6: Validate the full integration and document setup

**Files:** Complete `test/opencode-live.test.mjs`; update `README.md` and this plan's execution ledger. Apply only fixes justified by failed checks or review.

- [ ] **Step 1: Add the opt-in live acceptance cases before final fixes.** `CDASH_OPENCODE_INTEGRATION=1 node --test test/opencode-live.test.mjs` must use isolated project/session fixtures and report the installed OpenCode version. Default Node tests skip real account/network cases. Assert native and Claude prompts produce exactly one accepted native input each, the same session/model/variant survives resume, interruption works across both origins, and the native TUI remains alive while Claude reconnects.
- [ ] **Step 2: Run a real temporary-agent/browser smoke test.** Bind a separate agent to loopback with `PORT=0` and isolated `CLAUDE_DIR`. Verify model selection, Launch, native Terminal navigation, Open in Claude, tool start/result/Working rendering, supported approvals, reconnect history, summary/prompt fallback, purge, Kill, and Resume. Never replace the normal agent or reuse an unrelated user session. Verify actual tool environment isolation without logging secret values. Record unavailable account/browser/device gates as unrun.
- [ ] **Step 3: Document OpenCode setup** with the qualified V2 floor, Node requirement, existing OpenCode provider login plus Claude Remote Control login, project models/variants, text-only bridge limits, managed-history scope, and unsupported same-session execution through separate servers. Keep the README's Claude-first introduction intact.
- [ ] **Step 4: Run release-appropriate local gates:**

  ```bash
  NO_PROXY=127.0.0.1,localhost npm test
  cargo test --all --locked
  cargo clippy --all-targets --locked -- -D warnings -D clippy::disallowed_types
  cargo build --release --locked -p cdash-agent
  node --check public/app.js
  node --check crates/agent/src/collect/opencode-api.mjs
  node --check crates/agent/src/collect/opencode-remote.mjs
  node --check crates/agent/src/collect/claude-remote.mjs
  git diff --check
  ```

  Required output: zero test failures, compiler/linter errors, or whitespace errors. If host libraries prevent workspace/Tauri checks, run the agent-specific equivalents and report that workspace gate as unrun; do not treat a narrower pass as workspace validation. Use a bounded temporary curl wrapper if the existing intentionally failed-download test stalls; record assisted validation accurately.
- [ ] **Step 5: Request one independent whole-branch review.** Review connected Claude/Codex behavior, embedded/materialized asset paths, ownership/security, API/fixture fidelity, races, and the spec checklist. Fix findings and rerun only the affected checks, then record passed/open/unrun gates. A review is required before claiming complete support.
- [ ] **Step 6: Commit** as `feat: qualify and document OpenCode support`. Leave the isolated branch reviewable. Do not merge, publish, deploy, build distribution APKs, or claim physical Android validation without that work being requested and actually performed.

## Execution Ledger

- Design: approved 2026-10-29; user revisions preserved.
- Implementation plan: drafted and self-reviewed; awaiting user review and execution-method selection.
- Tasks 1–6: not started. No product code changes or new runtime qualification yet.
- Planning evidence: current source read directly. Codebase-memory `index_status` and exact-path coverage requests returned `Transport closed`; graph completeness is not claimed.
- Each task records its actual commit, relevant commands/results, protocol deviations, and remaining live/device gates here as it completes.
