# Latest model picker implementation plan

**Goal:** Show the latest available GPT model per family and versioned Claude aliases in launch and resume pickers.

**Architecture:** Keep complete Codex catalogs for launch validation and history. Filter the picker in the browser using numeric versions and family suffixes. Read Claude alias labels through the CLI initialization control response without generating a turn; reuse the existing metadata subprocess and 60-second cache. Preserve generic Claude labels if discovery fails, and saved models when resuming.

**Scope:** Existing metadata bridge, Rust catalog/cache, frontend picker and regression tests. No dependencies, release, deployment, or changes to saved sessions.

- [x] Add failing tests for numeric version filtering, unknown model IDs, versioned Claude labels, saved legacy models and metadata subprocess cleanup.
- [x] Implement catalog discovery and picker filtering in existing files.
- [ ] Run Node and Rust suites, inspect the diff, and verify live read-only catalogs.
- [ ] Commit, push the isolated branch, create a PR and check CI.
