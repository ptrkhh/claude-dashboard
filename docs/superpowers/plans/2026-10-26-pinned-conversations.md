# Pinned Conversations Implementation Plan

> **For agentic workers:** Use superpowers:executing-plans to implement this plan task by task.

**Goal:** Keep selected resumable conversations easy to find after restarting the computer.

**Architecture:** Save session IDs atomically in `cdash-pins.json` beside the agent's existing state. Return separate running, pinned, and resumable lists. Running sessions take precedence; pins survive while a conversation runs. Pinned conversations bypass the ordinary history page limit.

**Tech Stack:** Existing Rust, Tokio, Axum, and browser JavaScript; no new dependencies.

**Spec:** The user's request: allow pinning resumable conversations, show Running / Pinned / Resumable, and never duplicate a running conversation in either inactive section.

## Global Constraints

- Preserve existing model/effort choices, summaries, and Resume behavior in both inactive sections.
- Persist on the agent machine so all clients share pins.
- Keep authentication and session-ID validation on the new mutation route.
- No automatic launch, transcript deletion, or changes to the original checkout.

## Review Focus

- Old pins remain visible beyond the history scan and display limits.
- Running and purged sessions appear in neither inactive section.
- Concurrent pin requests cannot overwrite each other.
- Corrupt or unwritable state fails without overwriting saved pins.
- Polling preserves open summaries, dropdowns, and keyboard focus in both sections.

### Task 1: Persist and collect pins

**Files:** `collect/pins.rs`, `collect/mod.rs`, `collect/ctx.rs`, `collect/sessions.rs`.

- [x] Add failing collection tests for old Claude/Codex pins, restart, pagination, and running exclusion.
- [x] Add atomic read/set operations, serialize writes with a context lock, and partition inactive sessions before pagination.
- [x] Run focused Rust tests.

### Task 2: Expose the guarded mutation

**Files:** `http/routes.rs`, `http/serve.rs`.

- [x] Add failing HTTP tests for persistence, idempotence, invalid input, concurrent writes, and file failures.
- [x] Add `POST /api/pin` accepting `{sid, pinned}` and return success only after saving.
- [x] Run route and authentication tests.

### Task 3: Render and validate

**Files:** `public/app.js`, `public/index.html`, `public/style.css`, `test/frontend-guards.test.mjs`.

- [x] Add failing rendering tests; share existing cards and controls between Pinned and Resumable.
- [x] Add accessible Pin/Unpin controls and preserve summary/dropdown behavior.
- [x] Run Node and Rust suites, Clippy, browser checks, and `git diff --check`.
Delivery: review the diff, commit, push, create the requested PR, and verify its state.

## Validation evidence

- `cargo test --all --locked`: 343 tests passed.
- Browser with an isolated real agent: pins survived a process restart, disappeared while running, returned after stopping, and could be removed.
- Browser at mobile width: no horizontal overflow or page errors.
- Independent review found two UI regressions. Both failed before their fixes and now have passing tests: preserve an unrelated open picker when another conversation starts, and preserve expanded summaries when moving between sections.
- Browser confirmed summary expansion in both directions and keyboard focus after Pin/Unpin.
- `npm test`: 64 tests passed.
- `cargo clippy --all-targets --locked -- -D warnings -D clippy::disallowed_types`: passed.
- `git diff --check`: passed.
