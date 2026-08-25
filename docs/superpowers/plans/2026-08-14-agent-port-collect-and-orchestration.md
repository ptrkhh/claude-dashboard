# Rust Agent Port — Collect and Orchestration Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Port `lib/collect.js`, `lib/browse.js`, `lib/places.js` and the remainder of `lib/stats.js` to Rust, so the agent can produce the whole `/api/sessions` payload and perform launch/resume without Node.

**Architecture:** A new `crates/agent/src/collect/` module tree over the `parse/` and `host/` layers built by the previous plan. Filesystem primitives, the two caches, and the shared `Ctx` land first; the orchestrator (`collect_sessions`) lands last, once every part it composes has its own tests. Nothing here binds a socket or defines a route — that is spec step 4.

**Tech Stack:** Rust (edition 2021), `tokio` (fs, io-util, process, time, rt), `serde`/`serde_json`, `sysinfo` 0.38.4, `regex`.

**Spec:** `docs/superpowers/specs/2026-07-30-tauri-multi-host-design.md` (§Sequencing step 3)

**Previous plan:** `docs/superpowers/plans/2026-07-30-agent-port-parsers-and-host-layer.md` (spec steps 1–2, complete)

## Global Constraints

Copied verbatim from the spec. Every task's requirements implicitly include this section.

- **`sysinfo` is pinned to `0.38.4`.** Version `0.39.x` requires Rust 1.95; do not upgrade.
- **`-D clippy::disallowed_types` is a REQUIRED build gate, not advisory.** A green build with this lint disabled is not a valid build. It is the only enforcement of the subprocess time-box.
- **No direct use of `std::process::Command` or `tokio::process::Command`** except at exactly **two** sanctioned sites, each carrying an explicit `#[allow(clippy::disallowed_types)]` and a comment saying why: `host/cmd.rs` (the helper itself) and `host/path.rs` (the PATH probe). A third site is a defect. **This plan adds no new site** — every subprocess goes through `Runner`.
- **Default subprocess time-box is 5 seconds**, killed hard on expiry. The PATH probe alone uses 2000 ms. The git-status refresh alone uses 20 000 ms.
- **Field semantics must match Node exactly** for every value that reaches `/api/sessions`. A later parity gate compares them field-by-field. Do not "improve" a field's shape, name, or rounding — an improvement is indistinguishable from a regression at that gate.
- **Rust version floor: 1.94.1.**

---

## The derived control checklist

The spec forbids deriving this from `git log` and requires reading every file in `lib/` and `server.js`. That read has been done; this table is its output. **Every row must end this plan with a named Rust test**, including the rows Node has no test for. Task 12 audits the table and fails if a row is unclaimed.

### A. Input validation at a trust boundary

| # | Control | Node site | Proven by |
|---|---|---|---|
| A1 | `assertPath` — value must be a string and absolute | `server.js:28-30` | `validate::assert_path_requires_an_absolute_path` |
| A2 | kill target must match `^cdash-[\w-]+$` before reaching `tmux kill-session` | `server.js:62` | `validate::assert_kill_name_admits_only_cdash_session_names`, `spawn::kill_rejects_a_name_that_is_not_ours_before_running_tmux` |
| A3 | `assertValidSid` — `^[0-9a-f-]{36}$/i` before `--resume` and before purge | `collect.js:167-170` | `validate::assert_valid_sid_admits_only_a_36_char_uuid_shape`, `spawn::resume_rejects_a_bad_sid_before_touching_the_shell` |
| A4 | `MODELS` allowlist before `--model` | `collect.js:108,160` | `validate::model_and_effort_are_allowlists_not_denylists` |
| A5 | `EFFORTS` allowlist before `--effort` | `collect.js:109,161` | `validate::model_and_effort_are_allowlists_not_denylists` |
| A6 | `dir` must stat as a directory before `tmux new-session -c dir` | `collect.js:162-163` | `spawn::launch_rejects_a_bad_model_effort_or_directory_before_spawning` |
| A7 | browse errno mapped to a 400 with a fixed message, not raw error text | `server.js:47` | `browse::a_nonexistent_path_yields_a_400_with_a_fixed_message`, `browse::a_file_target_yields_the_not_a_folder_message` |

### B. Bounds

| # | Control | Node site | Proven by |
|---|---|---|---|
| B1 | `TAIL_BYTES` — read only the last 128 KiB of a transcript | `collect.js:64-77` | `fsio::read_tail_reads_only_the_last_128_kib` |
| B2 | `TRANSCRIPT_CACHE_MAX` — 200 entries, cleared wholesale at the cap | `collect.js:50,59` | `cache::the_cache_is_cleared_at_the_cap_rather_than_growing_without_bound` |
| B3 | `MAX_ENTRIES` 1000 + the `truncated` flag | `browse.js:7,21-22` | `browse::a_directory_over_the_cap_is_truncated_and_says_so` |
| B4 | `MAX_RECENTS` 12 | `places.js:7,10` | `places::push_recent_caps_the_list_length` |
| B5 | resumable list capped at 20 | `collect.js:269` | `sessions::the_resumable_list_is_capped` |
| B6 | RC-link poll bounded at 60 iterations | `collect.js:143` | `spawn::the_poll_gives_up_after_its_attempt_budget` |
| B7 | tmux session base name truncated to 30 chars | `collect.js:127` | `spawn::tmux_name_is_prefixed_munged_and_length_capped` |
| B8 | history capped at 60 groups, last 3 prompts each | `sessions.js:31-32` | **done** — `parse::history::group_history_groups_sorts_and_keeps_last_three` |
| B9 | log ring capped at 200 | `collect.js:25` | **done** — `host::log::keeps_at_most_200_entries_dropping_oldest` |

### C. Time-boxes and their kill signals

| # | Control | Node site | Proven by |
|---|---|---|---|
| C1 | 5 s subprocess deadline with `killSignal: 'SIGKILL'` | `collect.js:12-13` | **done** — `host::cmd::a_hung_child_is_killed_at_the_timeout` |
| C2 | git status gets a 20 s ceiling, not the 5 s default | `collect.js:41` | `git::git_gets_a_ceiling_well_above_the_default_time_box` |
| C3 | PATH probe 2000 ms | spec §Host layer | **done** (prior plan, Task 8) |
| C4 | RC-link poll gives up after 30 s total | `collect.js:143-154` | `spawn::the_poll_budget_is_thirty_seconds` |

### D. Atomicity and ordering

| # | Control | Node site | Proven by |
|---|---|---|---|
| D1 | `~/.claude.json` written to `.cdash.tmp` then renamed | `collect.js:121-123` | `spawn::trust_dir_marks_the_directory_and_preserves_every_other_key` |
| D2 | places file written to `.tmp` then renamed | `places.js:29-33` | `places::the_write_is_atomic_and_leaves_no_temp_file` |
| D3 | RC poll aborts if the session was killed while polling | `collect.js:145` | `spawn::a_session_killed_during_the_poll_is_not_resurrected` |
| D4 | RC poll aborts between the read and the write — the resurrection guard. **No test exists in Node.** | `collect.js:148` | `spawn::the_write_step_alone_refuses_a_session_that_is_already_gone` |
| D5 | git cache `busy` flag — at most one refresh in flight per directory | `collect.js:36-39` | `git::a_busy_entry_is_never_refreshed_however_stale` |
| D6 | `purged.delete(sid)` happens before the resume spawn | `collect.js:177` | `spawn::resume_un_purges_the_session_it_is_bringing_back` |
| D7 | `meta.delete(name)` after a successful kill | `server.js:64` | `spawn::kill_forgets_the_session_meta` |
| D8 | the trust write preserves every other key of `~/.claude.json` | `collect.js:117-120` | `spawn::trust_dir_marks_the_directory_and_preserves_every_other_key` |
| D9 | a discovered `rcLink` is memoized into `meta` | `collect.js:234` | `sessions::a_link_discovered_from_the_session_file_is_memoized_into_meta` |

### E. Filters that exclude by design

| # | Control | Node site | Proven by |
|---|---|---|---|
| E1 | `entrypoint !== 'cli'` — excludes `sdk-cli` observers and SDK runs | `collect.js:193-195` | `external::an_sdk_cli_session_is_excluded` |
| E2 | git cache staleness rule: serve the last known answer, never block a poll | `collect.js:37,45` | `git::the_first_call_returns_none_immediately_and_does_not_block` |
| E3 | external sessions already represented by a tmux pane are dropped | `collect.js:190` | `external::a_pid_already_shown_as_a_pane_is_excluded` |
| E4 | only sessions whose pid is still alive | `collect.js:184,190` | `external::a_dead_pid_is_excluded` |
| E5 | a session file without `sessionId` or `cwd` is dropped | `collect.js:192` | `external::a_session_without_session_id_or_cwd_is_excluded` |
| E6 | resumable skips sids that are running or purged | `collect.js:270` | `sessions::a_purged_session_is_hidden` |
| E7 | resumable requires `assistantCount >= 3` | `collect.js:273` | `sessions::a_resumable_session_needs_three_assistant_turns` |
| E8 | resumable requires a parseable transcript | `collect.js:272` | `sessions::a_session_with_no_transcript_is_not_resumable` |
| E9 | `transcriptFor` takes `.jsonl` only, with a 5 s grace on session start | `collect.js:99-101` | `lookup::a_transcript_older_than_the_five_second_grace_is_rejected` |
| E10 | browse returns directories and symlinks only, dotfolders hidden unless asked | `browse.js:15-17` | `browse::returns_folders_only_case_insensitively_sorted_hidden_excluded` |
| E11 | external scan takes `.json` files with a numeric pid only | `collect.js:190` | `external::non_json_files_and_non_numeric_names_are_skipped` |
| E12 | tmux panes filtered to the `cdash-` prefix | `sessions.js:57` | **done** — `parse::tmux::filters_to_cdash_prefixed_sessions` |

**Residual, restated:** this is a human code-read. It is materially more complete than commit-message derivation, but it is not provably complete, and no adopted mechanism detects a control dropped during the port. The parity gate does not close it — a dropped bound agrees with Node on ordinary input and diverges only on the input the guard existed for.

## Two findings the derivation produced

Both are recorded here rather than silently fixed.

1. **The parity gate's exemption list is missing `stats.cpuPct` and `stats.ramUsedKb`.** Both are sampled machine quantities read fresh on every request (`stats.js:32-35`), so two agents run seconds apart cannot agree on them by equality — the gate would fail on its first run for a reason that is not a port defect. The spec sanctions exactly this category ("*unless the field is a sampled machine quantity*"), so step 5 should add them with `stats.ramTotalKb` and `disks[].totalKb` still compared for equality. Not this plan's change to make; flagged for step 5.
2. **`parseGitStatus('')` returns `dirty: -1` in Node.** Unreachable in practice — `collect.js:210` and `collect.js:257` both guard with `gitOut ? … : null` — which is why the Rust port's `saturating_sub` is not a parity divergence. No action; recorded so the next reader does not re-derive it.

---

### Task 1: `machine_stats` — load average and RAM totals

Ports `machineStats` (`lib/stats.js:32-35`), the last function left in that file. It lives on `Sampler` because `Sampler` already owns a long-lived `System`; a second one would double the refresh cost for no gain.

**Files:**
- Modify: `crates/agent/src/host/sample.rs`

**Interfaces:**
- Consumes: `Sampler` (prior plan, Task 12).
- Produces:
  - `pub struct MachineStats { pub cpu_pct: u32, pub ram_used_kb: u64, pub ram_total_kb: u64 }` (serialized as `cpuPct`, `ramUsedKb`, `ramTotalKb`)
  - `pub fn machine_stats(&mut self) -> MachineStats` on `Sampler`

- [x] **Step 1: Write the failing tests**

Add to the `tests` module in `crates/agent/src/host/sample.rs`:

```rust
    #[test]
    fn machine_stats_reports_plausible_ram_and_a_clamped_cpu() {
        let mut s = Sampler::new();
        let m = s.machine_stats();
        assert!(m.ram_total_kb > 0);
        assert!(m.ram_used_kb <= m.ram_total_kb);
        // Node: Math.min(100, ...) — a load average above core count must not
        // render a 340% CPU bar.
        assert!(m.cpu_pct <= 100);
    }

    #[test]
    fn machine_stats_serializes_with_nodes_field_names() {
        let m = MachineStats { cpu_pct: 12, ram_used_kb: 3, ram_total_kb: 4 };
        let j = serde_json::to_string(&m).unwrap();
        assert!(j.contains("\"cpuPct\":12"));
        assert!(j.contains("\"ramUsedKb\":3"));
        assert!(j.contains("\"ramTotalKb\":4"));
    }
```

- [x] **Step 2: Run tests to verify they fail**

Run: `cargo test -p cdash-agent machine_stats`
Expected: FAIL — `cannot find type 'MachineStats' in this scope`.

- [x] **Step 3: Write the implementation**

In `crates/agent/src/host/sample.rs`, extend the `use` line and add the struct plus the method:

```rust
use serde::Serialize;
```

```rust
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct MachineStats {
    #[serde(rename = "cpuPct")]
    pub cpu_pct: u32,
    #[serde(rename = "ramUsedKb")]
    pub ram_used_kb: u64,
    #[serde(rename = "ramTotalKb")]
    pub ram_total_kb: u64,
}
```

Add inside `impl Sampler`:

```rust
    /// Ports `machineStats` (`lib/stats.js:32-35`). `available_parallelism` is
    /// the logical-CPU count `os.cpus().length` reported, and needs no
    /// `System` refresh. Byte-to-KiB conversion rounds rather than truncating,
    /// because Node's `Math.round` did.
    pub fn machine_stats(&mut self) -> MachineStats {
        self.sys.refresh_memory();
        let cores = std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(1) as f64;
        let load = System::load_average().one;
        let pct = ((load / cores) * 100.0).round();
        let to_kb = |bytes: u64| (bytes as f64 / 1024.0).round() as u64;
        let total = self.sys.total_memory();
        MachineStats {
            cpu_pct: pct.clamp(0.0, 100.0) as u32,
            ram_used_kb: to_kb(total.saturating_sub(self.sys.free_memory())),
            ram_total_kb: to_kb(total),
        }
    }
```

- [x] **Step 4: Run tests to verify they pass**

Run: `cargo test -p cdash-agent machine_stats`
Expected: PASS, 2 tests.

- [x] **Step 5: Commit**

```bash
git add crates/agent/src/host/sample.rs
git commit -m "feat: port machineStats onto the long-lived Sampler"
```

---

### Task 2: Filesystem primitives — bounded reads and the atomic write

Ports `readIf` (`collect.js:29`), `readTail` (`collect.js:64-77`), and the write-then-rename shared by `trustDir` (`collect.js:121-123`) and `writePlaces` (`places.js:29-33`). One module, because these three are the file-I/O primitives whose *bounds and atomicity* are the point — the rest of the port calls them and inherits both.

Checklist rows: **B1**, and the mechanism behind **D1**/**D2**.

**Files:**
- Create: `crates/agent/src/collect/fsio.rs`
- Create: `crates/agent/src/collect/mod.rs`
- Modify: `crates/agent/src/lib.rs`
- Modify: `crates/agent/Cargo.toml`

**Interfaces:**
- Consumes: nothing.
- Produces:
  - `pub const TAIL_BYTES: u64` = 131072
  - `pub async fn read_if(file: &Path) -> Option<String>`
  - `pub async fn read_tail(file: &Path) -> Option<String>`
  - `pub async fn write_atomic(file: &Path, contents: &str, tmp_suffix: &str) -> std::io::Result<()>`

- [x] **Step 1: Write the failing tests**

`crates/agent/src/collect/fsio.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn tempdir(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("cdash-fsio-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[tokio::test]
    async fn read_if_returns_none_for_a_missing_file() {
        assert_eq!(read_if(Path::new("/no/such/cdash-file")).await, None);
    }

    #[tokio::test]
    async fn read_tail_returns_a_whole_short_file() {
        let dir = tempdir("short");
        let f = dir.join("a.jsonl");
        tokio::fs::write(&f, "hello\n").await.unwrap();
        assert_eq!(read_tail(&f).await.as_deref(), Some("hello\n"));
    }

    #[tokio::test]
    async fn read_tail_reads_only_the_last_128_kib() {
        // The bound exists because a long session's transcript can be tens of
        // MiB and only the last assistant turn is wanted.
        let dir = tempdir("long");
        let f = dir.join("big.jsonl");
        let filler = "x".repeat(TAIL_BYTES as usize);
        tokio::fs::write(&f, format!("HEAD{filler}TAIL")).await.unwrap();

        let got = read_tail(&f).await.unwrap();
        assert_eq!(got.len(), TAIL_BYTES as usize);
        assert!(got.ends_with("TAIL"));
        assert!(!got.contains("HEAD"), "the head of an oversized file must not be read");
    }

    #[tokio::test]
    async fn write_atomic_leaves_no_temp_file_behind() {
        let dir = tempdir("atomic");
        let f = dir.join("places.json");
        write_atomic(&f, "{\"a\":1}", ".tmp").await.unwrap();
        assert_eq!(tokio::fs::read_to_string(&f).await.unwrap(), "{\"a\":1}");
        assert!(!dir.join("places.json.tmp").exists(), "temp file must be renamed, not left");
    }

    #[tokio::test]
    async fn write_atomic_replaces_existing_content_wholesale() {
        let dir = tempdir("replace");
        let f = dir.join("x.json");
        write_atomic(&f, "old", ".cdash.tmp").await.unwrap();
        write_atomic(&f, "new", ".cdash.tmp").await.unwrap();
        assert_eq!(tokio::fs::read_to_string(&f).await.unwrap(), "new");
    }
}
```

- [x] **Step 2: Add the tokio filesystem features**

In `crates/agent/Cargo.toml`, replace the `tokio` line with:

```toml
tokio = { version = "1", features = ["process", "time", "rt", "rt-multi-thread", "macros", "fs", "io-util"] }
```

- [x] **Step 3: Run tests to verify they fail**

Register the module first — `crates/agent/src/collect/mod.rs`:

```rust
pub mod fsio;
```

Add to `crates/agent/src/lib.rs`:

```rust
pub mod collect;
```

Run: `cargo test -p cdash-agent fsio`
Expected: FAIL — `cannot find function 'read_if' in this scope`.

- [x] **Step 4: Write the implementation**

Prepend to `crates/agent/src/collect/fsio.rs`:

```rust
use std::path::{Path, PathBuf};
use tokio::io::{AsyncReadExt, AsyncSeekExt};

/// Only the tail of a transcript is ever wanted, and a transcript has no upper
/// size. Mirrors `TAIL_BYTES` in `lib/collect.js:64`.
pub const TAIL_BYTES: u64 = 128 * 1024;

/// Read a whole file, or `None` if it cannot be read for any reason.
/// Mirrors `readIf` (`lib/collect.js:29`).
pub async fn read_if(file: &Path) -> Option<String> {
    tokio::fs::read_to_string(file).await.ok()
}

/// Read at most the last `TAIL_BYTES` of a file. A cut mid-character yields
/// U+FFFD, exactly as Node's `buf.toString('utf8')` did.
pub async fn read_tail(file: &Path) -> Option<String> {
    let mut fh = tokio::fs::File::open(file).await.ok()?;
    let size = fh.metadata().await.ok()?.len();
    let start = size.saturating_sub(TAIL_BYTES);
    if start > 0 {
        fh.seek(std::io::SeekFrom::Start(start)).await.ok()?;
    }
    let mut buf = Vec::new();
    fh.read_to_end(&mut buf).await.ok()?;
    Some(String::from_utf8_lossy(&buf).into_owned())
}

/// Write-then-rename. A reader either sees the old file or the new one, never
/// a half-written one. `tmp_suffix` is a parameter because Node used two
/// different ones and both are observable on disk: `.cdash.tmp` for
/// `~/.claude.json` (`lib/collect.js:121`) and `.tmp` for the places file
/// (`lib/places.js:30`).
pub async fn write_atomic(file: &Path, contents: &str, tmp_suffix: &str) -> std::io::Result<()> {
    let mut tmp = file.as_os_str().to_os_string();
    tmp.push(tmp_suffix);
    let tmp = PathBuf::from(tmp);
    tokio::fs::write(&tmp, contents).await?;
    tokio::fs::rename(&tmp, file).await
}
```

- [x] **Step 5: Run tests to verify they pass**

Run: `cargo test -p cdash-agent fsio`
Expected: PASS, 5 tests.

- [x] **Step 6: Commit**

```bash
git add crates/agent/src/collect/fsio.rs crates/agent/src/collect/mod.rs crates/agent/src/lib.rs crates/agent/Cargo.toml Cargo.lock
git commit -m "feat: bounded tail read and the atomic write-then-rename"
```

---

### Task 3: The transcript cache

Ports `parseTranscriptCached` (`collect.js:48-62`): memoized by path, revalidated by mtime, cleared wholesale at 200 entries.

Checklist row: **B2**.

**Files:**
- Create: `crates/agent/src/collect/cache.rs`
- Modify: `crates/agent/src/collect/mod.rs`

**Interfaces:**
- Consumes: `read_if` (Task 2), `parse_transcript` and `Transcript` (prior plan, Task 3).
- Produces:
  - `pub const TRANSCRIPT_CACHE_MAX: usize` = 200
  - `pub struct TranscriptCache` with `pub fn new() -> Self` and `pub async fn get(&self, file: &Path) -> Option<Transcript>`
  - `pub fn mtime_ms(md: &std::fs::Metadata) -> f64`

- [x] **Step 1: Write the failing tests**

`crates/agent/src/collect/cache.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, SystemTime};

    fn tempdir(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("cdash-cache-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn msg(text: &str) -> String {
        format!(
            "{{\"type\":\"assistant\",\"message\":{{\"content\":[{{\"type\":\"text\",\"text\":\"{text}\"}}]}}}}\n"
        )
    }

    #[tokio::test]
    async fn missing_file_yields_none() {
        let c = TranscriptCache::new();
        assert!(c.get(Path::new("/no/such/cdash.jsonl")).await.is_none());
    }

    #[tokio::test]
    async fn an_unchanged_mtime_serves_the_memoized_parse_without_rereading() {
        // Node asserted object identity. Rust returns a clone, so identity
        // cannot be the assertion — instead the file is rewritten with its
        // mtime restored. A cache that re-read would see "second".
        let dir = tempdir("hit");
        let f = dir.join("x.jsonl");
        std::fs::write(&f, msg("first")).unwrap();
        let stamp = std::fs::metadata(&f).unwrap().modified().unwrap();

        let c = TranscriptCache::new();
        assert_eq!(c.get(&f).await.unwrap().last_assistant_text.as_deref(), Some("first"));

        std::fs::write(&f, msg("second")).unwrap();
        filetime_set(&f, stamp);
        assert_eq!(
            c.get(&f).await.unwrap().last_assistant_text.as_deref(),
            Some("first"),
            "same mtime must serve the cached parse"
        );
    }

    #[tokio::test]
    async fn a_changed_mtime_forces_a_reparse() {
        let dir = tempdir("miss");
        let f = dir.join("x.jsonl");
        std::fs::write(&f, msg("first")).unwrap();

        let c = TranscriptCache::new();
        assert_eq!(c.get(&f).await.unwrap().last_assistant_text.as_deref(), Some("first"));

        std::fs::write(&f, msg("second")).unwrap();
        filetime_set(&f, SystemTime::now() + Duration::from_secs(60));
        assert_eq!(c.get(&f).await.unwrap().last_assistant_text.as_deref(), Some("second"));
    }

    #[tokio::test]
    async fn the_cache_is_cleared_at_the_cap_rather_than_growing_without_bound() {
        let dir = tempdir("cap");
        let c = TranscriptCache::new();
        for i in 0..=TRANSCRIPT_CACHE_MAX {
            let f = dir.join(format!("{i}.jsonl"));
            std::fs::write(&f, msg("t")).unwrap();
            c.get(&f).await;
        }
        assert!(c.len() <= TRANSCRIPT_CACHE_MAX, "cache must not exceed its cap");
    }
}
```

The tests need one helper that sets mtime without a new dependency. Add it to the same `tests` module:

```rust
    /// `utimensat` via std is not exposed, and `filetime` is a dependency this
    /// crate does not need in production. `rustix` is already a dependency and
    /// has the syscall.
    fn filetime_set(p: &Path, t: SystemTime) {
        let d = t.duration_since(SystemTime::UNIX_EPOCH).unwrap();
        let ts = rustix::fs::Timestamps {
            last_access: rustix::fs::Timespec {
                tv_sec: d.as_secs() as _,
                tv_nsec: d.subsec_nanos() as _,
            },
            last_modification: rustix::fs::Timespec {
                tv_sec: d.as_secs() as _,
                tv_nsec: d.subsec_nanos() as _,
            },
        };
        rustix::fs::utimensat(rustix::fs::CWD, p, &ts, rustix::fs::AtFlags::empty()).unwrap();
    }
```

- [x] **Step 2: Run tests to verify they fail**

Add to `crates/agent/src/collect/mod.rs`:

```rust
pub mod cache;
```

Run: `cargo test -p cdash-agent cache`
Expected: FAIL — `cannot find type 'TranscriptCache' in this scope`.

- [x] **Step 3: Write the implementation**

Prepend to `crates/agent/src/collect/cache.rs`:

```rust
use super::fsio::read_if;
use crate::parse::transcript::{parse_transcript, Transcript};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::UNIX_EPOCH;

/// Mirrors `TRANSCRIPT_CACHE_MAX` (`lib/collect.js:50`).
pub const TRANSCRIPT_CACHE_MAX: usize = 200;

/// Node compared `stat.mtimeMs`, a float millisecond count. Keeping the same
/// representation keeps the revalidation predicate identical.
pub fn mtime_ms(md: &std::fs::Metadata) -> f64 {
    md.modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_secs_f64() * 1000.0)
        .unwrap_or(0.0)
}

struct Entry {
    mtime_ms: f64,
    result: Transcript,
}

/// Memoized transcript parse, keyed by path and revalidated by mtime.
/// Mirrors `parseTranscriptCached` (`lib/collect.js:51-62`).
pub struct TranscriptCache {
    map: Mutex<HashMap<PathBuf, Entry>>,
}

impl Default for TranscriptCache {
    fn default() -> Self {
        Self::new()
    }
}

impl TranscriptCache {
    pub fn new() -> Self {
        Self { map: Mutex::new(HashMap::new()) }
    }

    pub fn len(&self) -> usize {
        self.map.lock().unwrap_or_else(|e| e.into_inner()).len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub async fn get(&self, file: &Path) -> Option<Transcript> {
        let md = tokio::fs::metadata(file).await.ok()?.into();
        let stamp = mtime_ms(&md);
        {
            let map = self.map.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(hit) = map.get(file) {
                if hit.mtime_ms == stamp {
                    return Some(hit.result.clone());
                }
            }
        }
        let txt = read_if(file).await?;
        let result = parse_transcript(&txt);
        let mut map = self.map.lock().unwrap_or_else(|e| e.into_inner());
        // ponytail: crude cap, same as Node's — swap for an LRU only if a
        // profile ever shows the reparse storm mattering.
        if map.len() >= TRANSCRIPT_CACHE_MAX {
            map.clear();
        }
        map.insert(file.to_path_buf(), Entry { mtime_ms: stamp, result: result.clone() });
        Some(result)
    }
}
```

`tokio::fs::metadata` returns `std::fs::Metadata` already, so drop the `.into()` if the compiler objects — the line should read:

```rust
        let md = tokio::fs::metadata(file).await.ok()?;
```

- [x] **Step 4: Run tests to verify they pass**

Run: `cargo test -p cdash-agent cache`
Expected: PASS, 4 tests.

- [x] **Step 5: Commit**

```bash
git add crates/agent/src/collect/cache.rs crates/agent/src/collect/mod.rs
git commit -m "feat: mtime-revalidated transcript cache with the 200-entry cap"
```

---

### Task 4: The git-status cache

Ports `gitStatusFor` (`collect.js:31-46`). The rule that matters: **a poll never waits on git.** It takes the last known answer — `None` the first time — and schedules a refresh in the background if the entry is stale and no refresh is already in flight.

Checklist rows: **C2**, **D5**, **E2**.

**Files:**
- Create: `crates/agent/src/collect/git.rs`
- Modify: `crates/agent/src/collect/mod.rs`

**Interfaces:**
- Consumes: `Runner` (prior plan, Task 9).
- Produces:
  - `pub const GIT_TTL_MS: u64` = 15000, `pub const GIT_TIMEOUT: Duration` = 20 s
  - `pub struct GitCache` with `pub fn new() -> Self`
  - `pub fn status_for(self: &Arc<Self>, runner: Arc<Runner>, dir: &str, now_ms: u64) -> Option<String>`
  - `pub fn refresh_due(entry_ts_ms: u64, busy: bool, now_ms: u64) -> bool` — pure, testable

- [x] **Step 1: Write the failing tests**

`crates/agent/src/collect/git.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::host::log::LogBuffer;

    fn runner() -> Arc<Runner> {
        let log = Arc::new(LogBuffer::new());
        Arc::new(Runner::new(std::env::var("PATH").unwrap_or_default(), log))
    }

    #[test]
    fn a_fresh_entry_is_not_refreshed() {
        assert!(!refresh_due(10_000, false, 10_000 + GIT_TTL_MS));
    }

    #[test]
    fn a_stale_entry_is_refreshed() {
        assert!(refresh_due(10_000, false, 10_000 + GIT_TTL_MS + 1));
    }

    #[test]
    fn a_busy_entry_is_never_refreshed_however_stale() {
        // D5: without this, every 4s poll stacks another `git status` on a
        // repository that is already slow enough to still be running.
        assert!(!refresh_due(0, true, 10_000_000));
    }

    #[tokio::test]
    async fn the_first_call_returns_none_immediately_and_does_not_block() {
        let cache = Arc::new(GitCache::new());
        let started = std::time::Instant::now();
        let out = cache.status_for(runner(), "/tmp", 1_000_000);
        assert_eq!(out, None, "a cold entry serves None rather than waiting");
        assert!(started.elapsed() < Duration::from_millis(100));
    }

    #[tokio::test]
    async fn a_real_repository_is_populated_by_the_background_refresh() {
        let dir = std::env::temp_dir().join(format!("cdash-git-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let r = runner();
        let d = dir.to_str().unwrap();
        r.run("git", &["-C", d, "init", "-q"], "git-init").await;

        let cache = Arc::new(GitCache::new());
        assert_eq!(cache.status_for(r.clone(), d, 1_000_000), None);

        let mut got = None;
        for _ in 0..50 {
            tokio::time::sleep(Duration::from_millis(100)).await;
            if let Some(out) = cache.status_for(r.clone(), d, 1_000_000) {
                got = Some(out);
                break;
            }
        }
        let out = got.expect("the background refresh must eventually populate the entry");
        assert!(out.starts_with("## "), "porcelain -b output starts with the branch header");
    }
}
```

- [x] **Step 2: Run tests to verify they fail**

Add to `crates/agent/src/collect/mod.rs`:

```rust
pub mod git;
```

Run: `cargo test -p cdash-agent git`
Expected: FAIL — `cannot find function 'refresh_due' in this scope`.

- [x] **Step 3: Write the implementation**

Prepend to `crates/agent/src/collect/git.rs`:

```rust
use crate::host::cmd::Runner;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// Mirrors `GIT_TTL_MS` (`lib/collect.js:34`).
pub const GIT_TTL_MS: u64 = 15_000;
/// The 20 s ceiling from `lib/collect.js:41`, deliberately far above the 5 s
/// default: slower than this and the repository simply gets no git badge.
pub const GIT_TIMEOUT: Duration = Duration::from_secs(20);

#[derive(Clone, Default)]
struct Entry {
    out: Option<String>,
    ts: u64,
    busy: bool,
}

/// Pure refresh predicate, split out so the two rules that matter can be
/// tested without a repository: stale entries refresh, busy entries never do.
pub fn refresh_due(entry_ts_ms: u64, busy: bool, now_ms: u64) -> bool {
    !busy && now_ms.saturating_sub(entry_ts_ms) > GIT_TTL_MS
}

/// `git status` per directory, refreshed in the background. A poll never waits
/// on git: it gets the last known answer (or `None` the first time) and moves
/// on. Mirrors `gitStatusFor` (`lib/collect.js:35-46`).
pub struct GitCache {
    map: Mutex<HashMap<String, Entry>>,
}

impl Default for GitCache {
    fn default() -> Self {
        Self::new()
    }
}

impl GitCache {
    pub fn new() -> Self {
        Self { map: Mutex::new(HashMap::new()) }
    }

    pub fn status_for(self: &Arc<Self>, runner: Arc<Runner>, dir: &str, now_ms: u64) -> Option<String> {
        let mut map = self.map.lock().unwrap_or_else(|e| e.into_inner());
        let hit = map.entry(dir.to_string()).or_default();
        let known = hit.out.clone();
        if !refresh_due(hit.ts, hit.busy, now_ms) {
            return known;
        }
        hit.busy = true;
        drop(map);

        let cache = Arc::clone(self);
        let dir_owned = dir.to_string();
        tokio::spawn(async move {
            let out = runner
                .run_with_timeout(
                    "git",
                    &["-C", &dir_owned, "status", "--porcelain=v1", "-b"],
                    // The explicit key: `git <dir>`, so two failing repositories
                    // produce two log lines rather than collapsing into one.
                    &format!("git {dir_owned}"),
                    GIT_TIMEOUT,
                )
                .await;
            let mut map = cache.map.lock().unwrap_or_else(|e| e.into_inner());
            map.insert(
                dir_owned,
                Entry {
                    out: if out.is_empty() { None } else { Some(out) },
                    ts: now_ms,
                    busy: false,
                },
            );
        });

        known
    }
}
```

Note the `ts` written by the background task is `now_ms` — the timestamp of the *request that scheduled it*, not of completion. That matches Node closely enough for the TTL to behave the same and avoids threading a clock into the spawned task.

- [x] **Step 4: Run tests to verify they pass**

Run: `cargo test -p cdash-agent git -- --test-threads=1`
Expected: PASS, 5 tests.

- [x] **Step 5: Commit**

```bash
git add crates/agent/src/collect/git.rs crates/agent/src/collect/mod.rs
git commit -m "feat: background git-status cache that never blocks a poll"
```

---

### Task 5: `Ctx` and the `~/.claude` lookups

Defines the shared context in full — every later task consumes it and none extends it — and ports `sessionFileFor` (`collect.js:81-85`), `rcLinkFor` (`collect.js:87-91`) and `transcriptFor` (`collect.js:94-106`).

Checklist row: **E9**.

**Files:**
- Create: `crates/agent/src/collect/ctx.rs`
- Create: `crates/agent/src/collect/lookup.rs`
- Modify: `crates/agent/src/collect/mod.rs`

**Interfaces:**
- Consumes: `Host` (prior plan, Task 13), `TranscriptCache` (Task 3), `GitCache` (Task 4), `read_if` and `mtime_ms` (Tasks 2–3).
- Produces:
  - `pub struct Meta { pub model: Option<String>, pub effort: Option<String>, pub rc_link: Option<String> }`
  - `pub struct Ctx { pub host: Host, pub claude_dir: PathBuf, pub disk_extra: Option<String>, pub places_file: PathBuf, pub meta: Mutex<HashMap<String, Meta>>, pub purged: Mutex<HashSet<String>>, pub transcripts: TranscriptCache, pub git: Arc<GitCache>, pub runner: Arc<Runner> }`
  - `pub fn new(host: Host, claude_dir: PathBuf, disk_extra: Option<String>) -> Ctx`
  - `pub struct SessionFile { session_id, cwd, name, entrypoint, started_at, bridge_session_id }`
  - `pub async fn session_file_for(claude_dir: &Path, pid: i32) -> Option<SessionFile>`
  - `pub async fn rc_link_for(claude_dir: &Path, pid: i32) -> Option<String>`
  - `pub async fn transcript_for(claude_dir: &Path, cwd: &str, created_sec: i64) -> Option<(PathBuf, f64)>`

- [x] **Step 1: Write the failing tests**

`crates/agent/src/collect/lookup.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn claude_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("cdash-lookup-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("sessions")).unwrap();
        dir
    }

    #[tokio::test]
    async fn session_file_reads_the_fields_collect_depends_on() {
        let d = claude_dir("sess");
        std::fs::write(
            d.join("sessions/4242.json"),
            r#"{"sessionId":"abc","cwd":"/x","name":"api","entrypoint":"cli","startedAt":1700000000000,"bridgeSessionId":"session_z"}"#,
        )
        .unwrap();
        let s = session_file_for(&d, 4242).await.unwrap();
        assert_eq!(s.session_id.as_deref(), Some("abc"));
        assert_eq!(s.cwd.as_deref(), Some("/x"));
        assert_eq!(s.name.as_deref(), Some("api"));
        assert_eq!(s.entrypoint.as_deref(), Some("cli"));
        assert_eq!(s.started_at, Some(1_700_000_000_000.0));
    }

    #[tokio::test]
    async fn a_missing_or_malformed_session_file_is_none_not_an_error() {
        let d = claude_dir("bad");
        assert!(session_file_for(&d, 1).await.is_none());
        std::fs::write(d.join("sessions/2.json"), "not json").unwrap();
        assert!(session_file_for(&d, 2).await.is_none());
    }

    #[tokio::test]
    async fn rc_link_is_built_from_the_bridge_session_id() {
        let d = claude_dir("rc");
        std::fs::write(d.join("sessions/7.json"), r#"{"bridgeSessionId":"session_abc"}"#).unwrap();
        assert_eq!(
            rc_link_for(&d, 7).await.as_deref(),
            Some("https://claude.ai/code/session_abc")
        );
        std::fs::write(d.join("sessions/8.json"), r#"{"other":1}"#).unwrap();
        assert_eq!(rc_link_for(&d, 8).await, None);
    }

    #[tokio::test]
    async fn transcript_for_takes_the_newest_jsonl_at_or_after_session_start() {
        let d = claude_dir("tr");
        let proj = d.join("projects").join("-x");
        std::fs::create_dir_all(&proj).unwrap();
        std::fs::write(proj.join("old.jsonl"), "{}").unwrap();
        std::fs::write(proj.join("new.jsonl"), "{}").unwrap();
        std::fs::write(proj.join("notes.txt"), "ignored").unwrap();

        // now-ish start time; both files were just written so both qualify.
        let now_sec = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64;
        let (file, _) = transcript_for(&d, "/x", now_sec).await.unwrap();
        assert_eq!(file.extension().unwrap(), "jsonl", "non-jsonl files are not candidates");
    }

    #[tokio::test]
    async fn a_transcript_older_than_the_five_second_grace_is_rejected() {
        // E9: a stale transcript from a previous session in the same directory
        // must not be attributed to a pane that started later.
        let d = claude_dir("grace");
        let proj = d.join("projects").join("-y");
        std::fs::create_dir_all(&proj).unwrap();
        std::fs::write(proj.join("stale.jsonl"), "{}").unwrap();

        let far_future = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64
            + 3600;
        assert!(transcript_for(&d, "/y", far_future).await.is_none());
    }

    #[tokio::test]
    async fn a_missing_project_directory_is_none() {
        let d = claude_dir("noproj");
        assert!(transcript_for(&d, "/nope", 0).await.is_none());
    }
}
```

- [x] **Step 2: Run tests to verify they fail**

Add to `crates/agent/src/collect/mod.rs`:

```rust
pub mod ctx;
pub mod lookup;
```

Run: `cargo test -p cdash-agent lookup`
Expected: FAIL — `cannot find function 'session_file_for' in this scope`.

- [x] **Step 3: Write `Ctx`**

`crates/agent/src/collect/ctx.rs`:

```rust
use super::cache::TranscriptCache;
use super::git::GitCache;
use crate::host::cmd::Runner;
use crate::host::init::Host;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

/// What the dashboard knows about a session it launched itself. Mirrors the
/// values Node stored in `ctx.meta` (`lib/collect.js:137,149,234`).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Meta {
    pub model: Option<String>,
    pub effort: Option<String>,
    pub rc_link: Option<String>,
}

/// The shared context every collect entry point takes. Mirrors the `ctx`
/// object built in `server.js:20-26`, plus the two caches and the `Runner`,
/// which in Node were module-level globals.
pub struct Ctx {
    pub host: Host,
    pub runner: Arc<Runner>,
    pub claude_dir: PathBuf,
    /// A second mount to report alongside `/`, e.g. `/mnt/d` (`server.js:22`).
    pub disk_extra: Option<String>,
    pub places_file: PathBuf,
    pub meta: Mutex<HashMap<String, Meta>>,
    pub purged: Mutex<HashSet<String>>,
    pub transcripts: TranscriptCache,
    pub git: Arc<GitCache>,
}

impl Ctx {
    pub fn new(host: Host, claude_dir: PathBuf, disk_extra: Option<String>) -> Self {
        let runner = Arc::new(Runner::new(host.path.clone(), Arc::clone(&host.log)));
        Self {
            places_file: claude_dir.join("cdash-places.json"),
            host,
            runner,
            claude_dir,
            disk_extra,
            meta: Mutex::new(HashMap::new()),
            purged: Mutex::new(HashSet::new()),
            transcripts: TranscriptCache::new(),
            git: Arc::new(GitCache::new()),
        }
    }

    pub fn meta_get(&self, name: &str) -> Option<Meta> {
        self.meta.lock().unwrap_or_else(|e| e.into_inner()).get(name).cloned()
    }

    pub fn meta_set(&self, name: &str, m: Meta) {
        self.meta.lock().unwrap_or_else(|e| e.into_inner()).insert(name.to_string(), m);
    }

    pub fn meta_has(&self, name: &str) -> bool {
        self.meta.lock().unwrap_or_else(|e| e.into_inner()).contains_key(name)
    }

    pub fn meta_delete(&self, name: &str) {
        self.meta.lock().unwrap_or_else(|e| e.into_inner()).remove(name);
    }
}
```

`Host` already owns a `Runner`, but it is not behind an `Arc` and the git cache's background task needs one. Constructing a second `Runner` over the same resolved PATH and the same `LogBuffer` keeps the log-once set per-runner — acceptable, and noted here so the duplicate is deliberate rather than accidental.

- [x] **Step 4: Write the lookups**

Prepend to `crates/agent/src/collect/lookup.rs`:

```rust
use super::cache::mtime_ms;
use super::fsio::read_if;
use crate::parse::paths::project_dir_name;
use crate::parse::transcript::parse_rc_file;
use serde::Deserialize;
use std::path::{Path, PathBuf};

/// `~/.claude/sessions/<pid>.json` — the authoritative link between a pane's
/// pid and its session id, so the transcript never has to be guessed.
#[derive(Debug, Clone, Deserialize)]
pub struct SessionFile {
    #[serde(rename = "sessionId")]
    pub session_id: Option<String>,
    pub cwd: Option<String>,
    pub name: Option<String>,
    pub entrypoint: Option<String>,
    /// Epoch milliseconds. `f64` for the same reason `ts` is: the field is
    /// JSON-numeric and a wrongly-typed value must not drop the whole entry.
    #[serde(rename = "startedAt")]
    pub started_at: Option<f64>,
    #[serde(rename = "bridgeSessionId")]
    pub bridge_session_id: Option<String>,
}

fn session_path(claude_dir: &Path, pid: i32) -> PathBuf {
    claude_dir.join("sessions").join(format!("{pid}.json"))
}

pub async fn session_file_for(claude_dir: &Path, pid: i32) -> Option<SessionFile> {
    let txt = read_if(&session_path(claude_dir, pid)).await?;
    serde_json::from_str(&txt).ok()
}

pub async fn rc_link_for(claude_dir: &Path, pid: i32) -> Option<String> {
    let txt = read_if(&session_path(claude_dir, pid)).await?;
    parse_rc_file(&txt).map(|id| format!("https://claude.ai/code/{id}"))
}

/// The newest `.jsonl` in the project directory modified at or after session
/// start, with Node's 5-second grace (`lib/collect.js:101`) — a transcript
/// written just before the pane appeared still belongs to it.
pub async fn transcript_for(
    claude_dir: &Path,
    cwd: &str,
    created_sec: i64,
) -> Option<(PathBuf, f64)> {
    let dir = claude_dir.join("projects").join(project_dir_name(cwd));
    let mut entries = tokio::fs::read_dir(&dir).await.ok()?;
    let mut best: Option<(PathBuf, f64)> = None;
    while let Ok(Some(e)) = entries.next_entry().await {
        let p = e.path();
        if p.extension().and_then(|x| x.to_str()) != Some("jsonl") {
            continue;
        }
        let Ok(md) = e.metadata().await else { continue };
        let ms = mtime_ms(&md);
        if ms / 1000.0 < (created_sec - 5) as f64 {
            continue;
        }
        if best.as_ref().is_none_or(|(_, b)| ms > *b) {
            best = Some((p, ms));
        }
    }
    best
}
```

If `is_none_or` is unavailable on the pinned toolchain, use `best.as_ref().map_or(true, |(_, b)| ms > *b)`.

- [x] **Step 5: Run tests to verify they pass**

Run: `cargo test -p cdash-agent lookup`
Expected: PASS, 6 tests.

- [x] **Step 6: Commit**

```bash
git add crates/agent/src/collect/ctx.rs crates/agent/src/collect/lookup.rs crates/agent/src/collect/mod.rs
git commit -m "feat: shared Ctx and the ~/.claude session and transcript lookups"
```

---

### Task 6: Places — recents and favorites

Ports `lib/places.js` whole. The pure helpers stay pure; the file-backed pair goes through `write_atomic`.

Checklist rows: **B4**, **D2**.

**Files:**
- Create: `crates/agent/src/collect/places.rs`
- Modify: `crates/agent/src/collect/mod.rs`

**Interfaces:**
- Consumes: `read_if`, `write_atomic` (Task 2).
- Produces:
  - `pub const MAX_RECENTS: usize` = 12
  - `pub struct Places { pub recents: Vec<String>, pub favorites: Vec<String> }`
  - `pub fn push_recent(list: &[String], p: &str, max: usize) -> Vec<String>`
  - `pub fn toggle_in(list: &[String], p: &str) -> Vec<String>`
  - `pub async fn read_places(file: &Path) -> Places`
  - `pub async fn add_recent(file: &Path, p: &str) -> std::io::Result<Places>`
  - `pub async fn toggle_favorite(file: &Path, p: &str) -> std::io::Result<Places>`

- [x] **Step 1: Write the failing tests**

`crates/agent/src/collect/places.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| x.to_string()).collect()
    }

    fn tempfile(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("cdash-places-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir.join("places.json")
    }

    #[test]
    fn push_recent_moves_an_existing_entry_to_the_front_without_duplicating() {
        assert_eq!(push_recent(&s(&["/a", "/b", "/c"]), "/c", MAX_RECENTS), s(&["/c", "/a", "/b"]));
        assert_eq!(push_recent(&s(&["/a", "/b"]), "/x", MAX_RECENTS), s(&["/x", "/a", "/b"]));
    }

    #[test]
    fn push_recent_caps_the_list_length() {
        let many: Vec<String> = (0..MAX_RECENTS).map(|i| format!("/p{i}")).collect();
        let out = push_recent(&many, "/new", MAX_RECENTS);
        assert_eq!(out.len(), MAX_RECENTS);
        assert_eq!(out[0], "/new");
        assert!(!out.contains(&format!("/p{}", MAX_RECENTS - 1)), "oldest dropped");
    }

    #[test]
    fn toggle_in_adds_then_removes() {
        assert_eq!(toggle_in(&[], "/a"), s(&["/a"]));
        assert_eq!(toggle_in(&s(&["/a", "/b"]), "/a"), s(&["/b"]));
    }

    #[tokio::test]
    async fn read_places_returns_the_empty_shape_for_a_missing_or_malformed_file() {
        let p = read_places(Path::new("/definitely/not/here.json")).await;
        assert!(p.recents.is_empty() && p.favorites.is_empty());

        let f = tempfile("bad");
        tokio::fs::write(&f, "{\"recents\":\"not an array\"}").await.unwrap();
        let p = read_places(&f).await;
        assert!(p.recents.is_empty(), "a wrongly-typed field falls back to empty, not an error");
    }

    #[tokio::test]
    async fn add_recent_and_toggle_favorite_persist_to_disk() {
        let f = tempfile("persist");
        add_recent(&f, "/home/x/one").await.unwrap();
        add_recent(&f, "/home/x/two").await.unwrap();
        assert_eq!(read_places(&f).await.recents, s(&["/home/x/two", "/home/x/one"]));

        toggle_favorite(&f, "/home/x/one").await.unwrap();
        assert_eq!(read_places(&f).await.favorites, s(&["/home/x/one"]));

        toggle_favorite(&f, "/home/x/one").await.unwrap();
        assert!(read_places(&f).await.favorites.is_empty());
    }

    #[tokio::test]
    async fn the_write_is_atomic_and_leaves_no_temp_file() {
        let f = tempfile("atomic");
        add_recent(&f, "/a").await.unwrap();
        let tmp = f.with_file_name("places.json.tmp");
        assert!(!tmp.exists());
    }
}
```

- [x] **Step 2: Run tests to verify they fail**

Add to `crates/agent/src/collect/mod.rs`:

```rust
pub mod places;
```

Run: `cargo test -p cdash-agent places`
Expected: FAIL — `cannot find function 'push_recent' in this scope`.

- [x] **Step 3: Write the implementation**

Prepend to `crates/agent/src/collect/places.rs`:

```rust
use super::fsio::{read_if, write_atomic};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// Mirrors `MAX_RECENTS` (`lib/places.js:7`).
pub const MAX_RECENTS: usize = 12;

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Places {
    #[serde(default)]
    pub recents: Vec<String>,
    #[serde(default)]
    pub favorites: Vec<String>,
}

pub fn push_recent(list: &[String], p: &str, max: usize) -> Vec<String> {
    let mut out = vec![p.to_string()];
    out.extend(list.iter().filter(|x| x.as_str() != p).cloned());
    out.truncate(max);
    out
}

pub fn toggle_in(list: &[String], p: &str) -> Vec<String> {
    if list.iter().any(|x| x == p) {
        list.iter().filter(|x| x.as_str() != p).cloned().collect()
    } else {
        let mut out = list.to_vec();
        out.push(p.to_string());
        out
    }
}

/// Node returned the empty shape for a missing file, a malformed file, and a
/// file whose fields are the wrong type (`lib/places.js:19-27`). `#[serde(default)]`
/// covers absence; the outer `unwrap_or_default` covers the other two.
pub async fn read_places(file: &Path) -> Places {
    match read_if(file).await {
        Some(txt) => serde_json::from_str(&txt).unwrap_or_default(),
        None => Places::default(),
    }
}

async fn write_places(file: &Path, data: &Places) -> std::io::Result<()> {
    let json = serde_json::to_string_pretty(data).unwrap_or_else(|_| "{}".to_string());
    write_atomic(file, &json, ".tmp").await
}

pub async fn add_recent(file: &Path, p: &str) -> std::io::Result<Places> {
    let mut data = read_places(file).await;
    data.recents = push_recent(&data.recents, p, MAX_RECENTS);
    write_places(file, &data).await?;
    Ok(data)
}

pub async fn toggle_favorite(file: &Path, p: &str) -> std::io::Result<Places> {
    let mut data = read_places(file).await;
    data.favorites = toggle_in(&data.favorites, p);
    write_places(file, &data).await?;
    Ok(data)
}
```

A wrongly-typed `recents` makes the whole document fail to deserialize, so both fields fall back together — the same outcome Node produced via its `try/catch`.

- [x] **Step 4: Run tests to verify they pass**

Run: `cargo test -p cdash-agent places`
Expected: PASS, 6 tests.

- [x] **Step 5: Commit**

```bash
git add crates/agent/src/collect/places.rs crates/agent/src/collect/mod.rs
git commit -m "feat: port recents and favorites with the atomic write"
```

---

### Task 7: The directory browser

Ports `lib/browse.js`, plus the errno-to-message mapping that lives in `server.js:47` — it belongs with the function whose errors it translates, so step 4 cannot forget it.

Checklist rows: **A7**, **B3**, **E10**.

**Files:**
- Create: `crates/agent/src/collect/browse.rs`
- Modify: `crates/agent/src/collect/mod.rs`

**Interfaces:**
- Consumes: nothing.
- Produces:
  - `pub const MAX_ENTRIES: usize` = 1000
  - `pub struct DirEntry { pub name: String, pub path: String }`
  - `pub struct Listing { pub path: String, pub parent: Option<String>, pub entries: Vec<DirEntry>, pub truncated: bool }`
  - `pub struct BrowseError { pub message: String }` with `pub fn status(&self) -> u16` returning 400
  - `pub async fn list_dirs(target: &str, show_hidden: bool) -> Result<Listing, BrowseError>`

- [x] **Step 1: Write the failing tests**

`crates/agent/src/collect/browse.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(tag: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!("cdash-browse-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("alpha")).unwrap();
        std::fs::create_dir_all(root.join("Beta")).unwrap();
        std::fs::create_dir_all(root.join(".hidden")).unwrap();
        std::fs::write(root.join("a-file.txt"), "x").unwrap();
        root
    }

    #[tokio::test]
    async fn returns_folders_only_case_insensitively_sorted_hidden_excluded() {
        let root = fixture("basic");
        let d = list_dirs(root.to_str().unwrap(), false).await.unwrap();
        assert_eq!(
            d.entries.iter().map(|e| e.name.as_str()).collect::<Vec<_>>(),
            vec!["alpha", "Beta"],
            "no file, no dotdir, and 'alpha' sorts before 'Beta'"
        );
        assert_eq!(d.path, root.to_str().unwrap());
        assert_eq!(d.parent.as_deref(), root.parent().unwrap().to_str());
        assert_eq!(d.entries[0].path, root.join("alpha").to_str().unwrap());
        assert!(!d.truncated);
    }

    #[tokio::test]
    async fn includes_dotfolders_when_asked() {
        let root = fixture("hidden");
        let d = list_dirs(root.to_str().unwrap(), true).await.unwrap();
        assert_eq!(
            d.entries.iter().map(|e| e.name.as_str()).collect::<Vec<_>>(),
            vec![".hidden", "alpha", "Beta"]
        );
    }

    #[tokio::test]
    async fn reports_a_null_parent_at_the_filesystem_root() {
        let d = list_dirs("/", false).await.unwrap();
        assert_eq!(d.parent, None);
        assert_eq!(d.path, "/");
    }

    #[tokio::test]
    async fn a_directory_over_the_cap_is_truncated_and_says_so() {
        // B3: Node has no test for this. An enormous directory must not stall
        // a tap on the folder picker.
        let root = std::env::temp_dir().join(format!("cdash-browse-many-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        for i in 0..(MAX_ENTRIES + 5) {
            std::fs::create_dir(root.join(format!("d{i:05}"))).unwrap();
        }
        let d = list_dirs(root.to_str().unwrap(), false).await.unwrap();
        assert_eq!(d.entries.len(), MAX_ENTRIES);
        assert!(d.truncated);
    }

    #[tokio::test]
    async fn a_nonexistent_path_yields_a_400_with_a_fixed_message() {
        // A7: the raw OS error must not reach the client.
        let e = list_dirs("/no/such/dir/cdash-xyz", false).await.unwrap_err();
        assert_eq!(e.message, "No such folder");
        assert_eq!(e.status(), 400);
    }

    #[tokio::test]
    async fn a_file_target_yields_the_not_a_folder_message() {
        let root = fixture("notdir");
        let f = root.join("a-file.txt");
        let e = list_dirs(f.to_str().unwrap(), false).await.unwrap_err();
        assert_eq!(e.message, "Not a folder");
    }
}
```

- [x] **Step 2: Run tests to verify they fail**

Add to `crates/agent/src/collect/mod.rs`:

```rust
pub mod browse;
```

Run: `cargo test -p cdash-agent browse`
Expected: FAIL — `cannot find function 'list_dirs' in this scope`.

- [x] **Step 3: Write the implementation**

Prepend to `crates/agent/src/collect/browse.rs`:

```rust
use serde::Serialize;
use std::path::{Path, PathBuf};

/// Entries are capped so an enormous directory cannot stall a tap on the
/// folder picker. Mirrors `MAX_ENTRIES` (`lib/browse.js:7`).
pub const MAX_ENTRIES: usize = 1000;

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct DirEntry {
    pub name: String,
    pub path: String,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Listing {
    pub path: String,
    pub parent: Option<String>,
    pub entries: Vec<DirEntry>,
    pub truncated: bool,
}

/// A browse failure is always a 400 with a fixed message. Mirrors the errno
/// mapping in `server.js:47`, kept next to the function that produces the
/// errors so the HTTP layer cannot forget it.
#[derive(Debug, Clone, PartialEq)]
pub struct BrowseError {
    pub message: String,
}

impl BrowseError {
    pub fn status(&self) -> u16 {
        400
    }
}

fn map_err(e: &std::io::Error) -> BrowseError {
    use std::io::ErrorKind;
    let message = match e.kind() {
        ErrorKind::PermissionDenied => "Permission denied",
        ErrorKind::NotFound => "No such folder",
        ErrorKind::NotADirectory => "Not a folder",
        _ => "Cannot read folder",
    };
    BrowseError { message: message.to_string() }
}

/// Folders only — a project directory is what is being chosen — plus symlinks,
/// which commonly point at directories. Sorted case-insensitively.
pub async fn list_dirs(target: &str, show_hidden: bool) -> Result<Listing, BrowseError> {
    let abs = if target.is_empty() {
        PathBuf::from("/")
    } else {
        std::path::absolute(Path::new(target)).unwrap_or_else(|_| PathBuf::from(target))
    };
    let mut rd = tokio::fs::read_dir(&abs).await.map_err(|e| map_err(&e))?;

    let mut names: Vec<String> = Vec::new();
    while let Some(e) = rd.next_entry().await.map_err(|e| map_err(&e))? {
        let Ok(ft) = e.file_type().await else { continue };
        if !ft.is_dir() && !ft.is_symlink() {
            continue;
        }
        let name = e.file_name().to_string_lossy().into_owned();
        if !show_hidden && name.starts_with('.') {
            continue;
        }
        names.push(name);
    }

    // ponytail: lowercase compare stands in for localeCompare's
    // `sensitivity: 'base'`; it agrees on case and differs only on accent
    // folding. Swap for a collation crate only if that ever shows up.
    names.sort_by_key(|n| n.to_lowercase());

    let truncated = names.len() > MAX_ENTRIES;
    names.truncate(MAX_ENTRIES);

    Ok(Listing {
        parent: abs.parent().map(|p| p.to_string_lossy().into_owned()),
        entries: names
            .into_iter()
            .map(|name| DirEntry {
                path: abs.join(&name).to_string_lossy().into_owned(),
                name,
            })
            .collect(),
        path: abs.to_string_lossy().into_owned(),
        truncated,
    })
}
```

`Path::parent` already returns `None` at the filesystem root, so no explicit root comparison is needed.

If `std::path::absolute` or `ErrorKind::NotADirectory` are unavailable on the pinned toolchain, substitute `std::fs::canonicalize` (falling back to the input on error) and match `e.raw_os_error() == Some(20)` for `ENOTDIR` respectively — and note the substitution in the commit message.

- [x] **Step 4: Run tests to verify they pass**

Run: `cargo test -p cdash-agent browse`
Expected: PASS, 6 tests.

- [x] **Step 5: Commit**

```bash
git add crates/agent/src/collect/browse.rs crates/agent/src/collect/mod.rs
git commit -m "feat: port the directory browser with its cap and errno mapping"
```

---

### Task 8: Every trust-boundary validator, in one module

The four validators Node scattered across two files, plus the kill-target pattern that lived inline in a route. They go together so that step 4's HTTP layer calls them rather than re-deriving them, and so a reviewer can see all five at once.

Checklist rows: **A1**, **A2**, **A3**, **A4**, **A5**.

**Files:**
- Create: `crates/agent/src/collect/validate.rs`
- Modify: `crates/agent/src/collect/mod.rs`

**Interfaces:**
- Consumes: nothing.
- Produces:
  - `pub struct BadRequest(pub String)` — carries the 400 message
  - `pub fn assert_path(p: &str) -> Result<(), BadRequest>`
  - `pub fn assert_kill_name(name: &str) -> Result<(), BadRequest>`
  - `pub fn assert_valid_sid(sid: &str) -> Result<(), BadRequest>`
  - `pub fn assert_model(model: &str) -> Result<(), BadRequest>`
  - `pub fn assert_effort(effort: &str) -> Result<(), BadRequest>`
  - `pub const MODELS: &[&str]`, `pub const EFFORTS: &[&str]`

- [x] **Step 1: Write the failing tests**

`crates/agent/src/collect/validate.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn assert_path_requires_an_absolute_path() {
        assert!(assert_path("/home/x").is_ok());
        assert!(assert_path("relative/x").is_err());
        assert!(assert_path("").is_err());
    }

    #[test]
    fn assert_kill_name_admits_only_cdash_session_names() {
        // A2: this value is handed to `tmux kill-session -t`.
        assert!(assert_kill_name("cdash-backend-1531-a9f").is_ok());
        assert!(assert_kill_name("cdash-a_b-1").is_ok());
        assert!(assert_kill_name("other").is_err());
        assert!(assert_kill_name("cdash-").is_err(), "the suffix is required");
        assert!(assert_kill_name("cdash-x; rm -rf /").is_err());
        assert!(assert_kill_name("").is_err());
    }

    #[test]
    fn assert_valid_sid_admits_only_a_36_char_uuid_shape() {
        // A3: this value reaches `claude --resume <sid>` and a path join.
        assert!(assert_valid_sid("2f8a1c94-3b7e-4d51-9a02-6c5f8e1b7d34").is_ok());
        assert!(assert_valid_sid("2F8A1C94-3B7E-4D51-9A02-6C5F8E1B7D34").is_ok(), "case-insensitive");
        assert!(assert_valid_sid("not-a-uuid; rm -rf /").is_err());
        assert!(assert_valid_sid("../../etc/passwd").is_err());
        assert!(assert_valid_sid("").is_err());
    }

    #[test]
    fn model_and_effort_are_allowlists_not_denylists() {
        assert!(assert_model("sonnet").is_ok());
        assert!(assert_model("gpt-4").is_err());
        assert!(assert_model("").is_err());
        assert!(assert_effort("xhigh").is_ok());
        assert!(assert_effort("ludicrous").is_err());
    }

    #[test]
    fn the_rejection_message_names_the_offending_value() {
        let e = assert_model("gpt-4").unwrap_err();
        assert!(e.0.contains("gpt-4"));
    }
}
```

- [x] **Step 2: Run tests to verify they fail**

Add to `crates/agent/src/collect/mod.rs`:

```rust
pub mod validate;
```

Run: `cargo test -p cdash-agent validate`
Expected: FAIL — `cannot find function 'assert_path' in this scope`.

- [x] **Step 3: Write the implementation**

Prepend to `crates/agent/src/collect/validate.rs`:

```rust
use regex::Regex;
use std::sync::OnceLock;

/// A rejected request. The HTTP layer renders this as a 400 with `message` as
/// the `error` field, matching `server.js:41`.
#[derive(Debug, Clone, PartialEq)]
pub struct BadRequest(pub String);

/// Mirrors `MODELS` (`lib/collect.js:108`).
pub const MODELS: &[&str] = &["sonnet", "opus", "haiku", "fable"];
/// Mirrors `EFFORTS` (`lib/collect.js:109`).
pub const EFFORTS: &[&str] = &["low", "medium", "high", "xhigh", "max"];

/// Mirrors `assertPath` (`server.js:28-30`).
pub fn assert_path(p: &str) -> Result<(), BadRequest> {
    if std::path::Path::new(p).is_absolute() {
        Ok(())
    } else {
        Err(BadRequest(format!("bad path: {p}")))
    }
}

fn kill_name_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"^cdash-[\w-]+$").unwrap())
}

fn sid_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"^(?i)[0-9a-f-]{36}$").unwrap())
}

/// Mirrors the inline guard at `server.js:62`. The value reaches
/// `tmux kill-session -t`, so nothing outside this shape may pass.
pub fn assert_kill_name(name: &str) -> Result<(), BadRequest> {
    if kill_name_re().is_match(name) {
        Ok(())
    } else {
        Err(BadRequest("bad name".to_string()))
    }
}

/// Mirrors `assertValidSid` (`lib/collect.js:168-170`). Rust's `regex` has no
/// implicit multiline anchors, so `^…$` cannot be escaped by a newline the way
/// a hand-rolled JS check could be.
pub fn assert_valid_sid(sid: &str) -> Result<(), BadRequest> {
    if sid_re().is_match(sid) {
        Ok(())
    } else {
        Err(BadRequest(format!("bad sid: {sid}")))
    }
}

pub fn assert_model(model: &str) -> Result<(), BadRequest> {
    if MODELS.contains(&model) {
        Ok(())
    } else {
        Err(BadRequest(format!("bad model: {model}")))
    }
}

pub fn assert_effort(effort: &str) -> Result<(), BadRequest> {
    if EFFORTS.contains(&effort) {
        Ok(())
    } else {
        Err(BadRequest(format!("bad effort: {effort}")))
    }
}
```

- [x] **Step 4: Run tests to verify they pass**

Run: `cargo test -p cdash-agent validate`
Expected: PASS, 5 tests.

- [x] **Step 5: Commit**

```bash
git add crates/agent/src/collect/validate.rs crates/agent/src/collect/mod.rs
git commit -m "feat: collect every trust-boundary validator into one module"
```

---

### Task 9: Launch, resume, kill, purge, and the RC-link poll

Ports `trustDir` (`collect.js:111-124`), `tmuxName` (`collect.js:126-130`), `spawnClaude` (`collect.js:132-157`), `launchSession` (`collect.js:159-165`) and `resumeSession` (`collect.js:172-179`). The RC-link poll's two resurrection guards get the test Node never had.

Kill and purge come here too, not with the router. In Node they had no function — their whole bodies lived inline in a route (`server.js:60-68`) — but the spec assigns "launch/resume/kill/purge" to this step, and the parts that matter (the guard, the subprocess, the map mutation, the un-hide) are exactly the parts a route should not own. Step 4 is left with wiring.

Checklist rows: **A6**, **B6**, **B7**, **C4**, **D1**, **D3**, **D4**, **D6**, **D7**, **D8**.

**Files:**
- Create: `crates/agent/src/collect/spawn.rs`
- Modify: `crates/agent/src/collect/mod.rs`

**Interfaces:**
- Consumes: `Ctx`, `Meta` (Task 5), `rc_link_for` (Task 5), `write_atomic`/`read_if` (Task 2), the validators (Task 8), `group_history` (prior plan, Task 2).
- Produces:
  - `pub const RC_POLL_ATTEMPTS: u32` = 60, `pub const RC_POLL_INTERVAL: Duration` = 500 ms
  - `pub async fn trust_dir(claude_json: &Path, dir: &str) -> std::io::Result<()>`
  - `pub fn tmux_name(dir: &str) -> String`
  - `pub async fn poll_rc_link(ctx: Arc<Ctx>, name: String, pid: i32, attempts: u32, interval: Duration)`
  - `pub async fn launch_session(ctx: &Arc<Ctx>, dir: &str, model: &str, effort: &str) -> Result<String, BadRequest>`
  - `pub async fn resume_session(ctx: &Arc<Ctx>, sid: &str) -> Result<String, BadRequest>`
  - `pub async fn kill_session(ctx: &Arc<Ctx>, name: &str) -> Result<(), BadRequest>`
  - `pub fn purge_session(ctx: &Arc<Ctx>, sid: &str) -> Result<(), BadRequest>`

- [x] **Step 1: Write the failing tests**

`crates/agent/src/collect/spawn.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::host::log::LogBuffer;

    fn tempdir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("cdash-spawn-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("sessions")).unwrap();
        dir
    }

    async fn ctx_for(claude_dir: PathBuf) -> Arc<Ctx> {
        let log = Arc::new(LogBuffer::new());
        let path = std::env::var("PATH").unwrap_or_default();
        let host = crate::host::init::Host {
            runner: Runner::new(path.clone(), Arc::clone(&log)),
            log,
            path,
            sampler: std::sync::Mutex::new(crate::host::sample::Sampler::new()),
        };
        Arc::new(Ctx::new(host, claude_dir, None))
    }

    #[test]
    fn tmux_name_is_prefixed_munged_and_length_capped() {
        let n = tmux_name("/mnt/d/git/a project.with-dots");
        assert!(n.starts_with("cdash-"));
        assert!(!n.contains(' ') && !n.contains('.'));
        // B7: the base is capped at 30 chars, before the time and suffix.
        let base: &str = n.split('-').nth(1).unwrap();
        assert!(base.len() <= 30);
        // The result must survive the kill-target guard it will later be given to.
        crate::collect::validate::assert_kill_name(&n).unwrap();
    }

    #[test]
    fn two_names_for_the_same_directory_differ() {
        assert_ne!(tmux_name("/x/y"), tmux_name("/x/y"));
    }

    #[tokio::test]
    async fn trust_dir_marks_the_directory_and_preserves_every_other_key() {
        // D8: this file is the user's real ~/.claude.json. Losing an unrelated
        // key here is data loss, not a cosmetic bug.
        let d = tempdir("trust");
        let f = d.join(".claude.json");
        tokio::fs::write(
            &f,
            r#"{"theme":"dark","projects":{"/other":{"hasTrustDialogAccepted":true,"note":"keep me"}}}"#,
        )
        .await
        .unwrap();

        trust_dir(&f, "/new").await.unwrap();

        let v: serde_json::Value =
            serde_json::from_str(&tokio::fs::read_to_string(&f).await.unwrap()).unwrap();
        assert_eq!(v["theme"], "dark", "unrelated top-level keys survive");
        assert_eq!(v["projects"]["/other"]["note"], "keep me", "other projects survive");
        assert_eq!(v["projects"]["/new"]["hasTrustDialogAccepted"], true);
        assert!(!d.join(".claude.json.cdash.tmp").exists(), "D1: temp file renamed away");
    }

    #[tokio::test]
    async fn trust_dir_creates_the_file_when_it_does_not_exist_yet() {
        let d = tempdir("trust-fresh");
        let f = d.join(".claude.json");
        trust_dir(&f, "/new").await.unwrap();
        let v: serde_json::Value =
            serde_json::from_str(&tokio::fs::read_to_string(&f).await.unwrap()).unwrap();
        assert_eq!(v["projects"]["/new"]["hasTrustDialogAccepted"], true);
    }

    #[tokio::test]
    async fn the_rc_poll_writes_the_link_it_finds() {
        let d = tempdir("rc-ok");
        let ctx = ctx_for(d.clone()).await;
        ctx.meta_set("cdash-x", Meta::default());
        tokio::fs::write(
            d.join("sessions/99.json"),
            r#"{"bridgeSessionId":"session_found"}"#,
        )
        .await
        .unwrap();

        poll_rc_link(Arc::clone(&ctx), "cdash-x".into(), 99, 3, Duration::from_millis(10)).await;

        assert_eq!(
            ctx.meta_get("cdash-x").unwrap().rc_link.as_deref(),
            Some("https://claude.ai/code/session_found")
        );
    }

    #[tokio::test]
    async fn a_session_killed_during_the_poll_is_not_resurrected() {
        // D4: the guard with no test in Node. Without it, a poll that started
        // before the kill writes the entry back after `meta.delete`, and a dead
        // session reappears in the UI holding a live-looking RC link.
        let d = tempdir("rc-killed");
        let ctx = ctx_for(d.clone()).await;
        tokio::fs::write(
            d.join("sessions/98.json"),
            r#"{"bridgeSessionId":"session_ghost"}"#,
        )
        .await
        .unwrap();
        // meta deliberately NOT set — this stands for "killed before the tick".

        poll_rc_link(Arc::clone(&ctx), "cdash-dead".into(), 98, 3, Duration::from_millis(10)).await;

        assert!(
            ctx.meta_get("cdash-dead").is_none(),
            "the poll must not create an entry for a session that is gone"
        );
    }

    #[tokio::test]
    async fn the_poll_gives_up_after_its_attempt_budget() {
        // B6/C4: bounded, so a session that never publishes a link does not
        // leave a task polling forever.
        let d = tempdir("rc-timeout");
        let ctx = ctx_for(d).await;
        ctx.meta_set("cdash-y", Meta::default());
        let started = std::time::Instant::now();

        poll_rc_link(Arc::clone(&ctx), "cdash-y".into(), 1, 3, Duration::from_millis(10)).await;

        assert!(started.elapsed() < Duration::from_secs(1));
        assert_eq!(ctx.meta_get("cdash-y").unwrap().rc_link, None);
        assert!(ctx.host.log.lines().iter().any(|l| l.contains("rc-link timeout")));
    }

    #[tokio::test]
    async fn launch_rejects_a_bad_model_effort_or_directory_before_spawning() {
        let d = tempdir("launch-guard");
        let ctx = ctx_for(d.clone()).await;
        assert!(launch_session(&ctx, "/tmp", "gpt-4", "medium").await.is_err());
        assert!(launch_session(&ctx, "/tmp", "sonnet", "ludicrous").await.is_err());
        // A6: a path that is not a directory never reaches `tmux -c`.
        assert!(launch_session(&ctx, "/no/such/dir/cdash", "sonnet", "medium").await.is_err());
    }

    #[tokio::test]
    async fn resume_rejects_a_bad_sid_before_touching_the_shell() {
        let d = tempdir("resume-guard");
        let ctx = ctx_for(d).await;
        assert!(resume_session(&ctx, "not-a-uuid; rm -rf /").await.is_err());
        assert!(resume_session(&ctx, "").await.is_err());
    }

    #[tokio::test]
    async fn resume_rejects_a_sid_that_history_does_not_know() {
        let d = tempdir("resume-unknown");
        let ctx = ctx_for(d.clone()).await;
        tokio::fs::write(d.join("history.jsonl"), "").await.unwrap();
        let e = resume_session(&ctx, "2f8a1c94-3b7e-4d51-9a02-6c5f8e1b7d34").await.unwrap_err();
        assert!(e.0.contains("unknown session"));
    }

    #[tokio::test]
    async fn kill_rejects_a_name_that_is_not_ours_before_running_tmux() {
        // A2: the argument reaches `tmux kill-session -t`.
        let d = tempdir("kill-guard");
        let ctx = ctx_for(d).await;
        assert!(kill_session(&ctx, "other").await.is_err());
        assert!(kill_session(&ctx, "cdash-x; rm -rf /").await.is_err());
    }

    #[tokio::test]
    async fn kill_forgets_the_session_meta() {
        // D7: a stale meta entry would let the RC poll write a link back for a
        // session that no longer exists.
        let d = tempdir("kill-meta");
        let ctx = ctx_for(d).await;
        ctx.meta_set("cdash-gone-1200-abc", Meta::default());
        // tmux is not running a session by this name; the kill fails and the
        // meta must be dropped anyway, exactly as Node's route did.
        let _ = kill_session(&ctx, "cdash-gone-1200-abc").await;
        assert!(!ctx.meta_has("cdash-gone-1200-abc"));
    }

    #[tokio::test]
    async fn purge_rejects_a_bad_sid_and_records_a_good_one() {
        let d = tempdir("purge");
        let ctx = ctx_for(d).await;
        assert!(purge_session(&ctx, "../../etc/passwd").is_err());
        purge_session(&ctx, "2f8a1c94-3b7e-4d51-9a02-6c5f8e1b7d34").unwrap();
        assert!(ctx
            .purged
            .lock()
            .unwrap()
            .contains("2f8a1c94-3b7e-4d51-9a02-6c5f8e1b7d34"));
    }
}
```

- [x] **Step 2: Run tests to verify they fail**

Add to `crates/agent/src/collect/mod.rs`:

```rust
pub mod spawn;
```

Run: `cargo test -p cdash-agent spawn`
Expected: FAIL — `cannot find function 'tmux_name' in this scope`.

- [x] **Step 3: Write the implementation**

Prepend to `crates/agent/src/collect/spawn.rs`:

```rust
use super::ctx::{Ctx, Meta};
use super::fsio::{read_if, write_atomic};
use super::lookup::rc_link_for;
use super::validate::{assert_effort, assert_model, assert_valid_sid, BadRequest};
use crate::host::cmd::Runner;
use crate::parse::history::group_history;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// 60 × 500 ms = the 30 s budget from `lib/collect.js:143`.
pub const RC_POLL_ATTEMPTS: u32 = 60;
pub const RC_POLL_INTERVAL: Duration = Duration::from_millis(500);

/// Mark a directory trusted in `~/.claude.json` so `claude` does not open on a
/// trust prompt no one is sitting in front of. Read-modify-write: every other
/// key in the file is preserved, and the write is atomic.
pub async fn trust_dir(claude_json: &Path, dir: &str) -> std::io::Result<()> {
    let mut cfg: serde_json::Value = read_if(claude_json)
        .await
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_else(|| serde_json::json!({}));
    if !cfg.is_object() {
        cfg = serde_json::json!({});
    }
    let projects = cfg
        .as_object_mut()
        .expect("cfg is an object")
        .entry("projects")
        .or_insert_with(|| serde_json::json!({}));
    if !projects.is_object() {
        *projects = serde_json::json!({});
    }
    let entry = projects
        .as_object_mut()
        .expect("projects is an object")
        .entry(dir)
        .or_insert_with(|| serde_json::json!({}));
    if !entry.is_object() {
        *entry = serde_json::json!({});
    }
    entry
        .as_object_mut()
        .expect("entry is an object")
        .insert("hasTrustDialogAccepted".into(), serde_json::Value::Bool(true));

    let json = serde_json::to_string_pretty(&cfg).unwrap_or_else(|_| "{}".into());
    write_atomic(claude_json, &json, ".cdash.tmp").await
}

/// `~/.claude.json` lives in `$HOME` even when `CLAUDE_DIR` is overridden, but
/// for testability it is derived from `claude_dir`'s parent when the override
/// is set — the same rule as `lib/collect.js:114-116`.
pub fn claude_json_path(claude_dir: &Path) -> PathBuf {
    if std::env::var("CLAUDE_DIR").is_ok() {
        claude_dir
            .parent()
            .unwrap_or(Path::new("/"))
            .join(".claude.json")
    } else {
        PathBuf::from(std::env::var("HOME").unwrap_or_else(|_| "/".into())).join(".claude.json")
    }
}

/// `cdash-<base>-<HHMM>-<xxx>`. The shape is load-bearing: it is what the pane
/// filter matches and what the kill guard admits.
pub fn tmux_name(dir: &str) -> String {
    let base: String = Path::new(dir)
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '_' || c == '-' { c } else { '-' })
        .take(30)
        .collect();

    let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default();
    let secs_of_day = now.as_secs() % 86_400;
    let hhmm = format!("{:02}{:02}", secs_of_day / 3600, (secs_of_day % 3600) / 60);

    // ponytail: nanosecond entropy stands in for Math.random()'s 3 base36
    // chars and avoids a `rand` dependency. Collisions need the same directory,
    // the same minute, and the same nanosecond bucket.
    let n = now.subsec_nanos();
    let suffix: String = (0..3)
        .map(|i| char::from_digit((n >> (i * 6)) % 36, 36).unwrap_or('0'))
        .collect();

    format!("cdash-{base}-{hhmm}-{suffix}")
}

/// Wait for `claude` to publish its remote-control session id, then record the
/// link. Two guards, both required: the session can be killed while this
/// sleeps, and again between the read and the write.
pub async fn poll_rc_link(
    ctx: Arc<Ctx>,
    name: String,
    pid: i32,
    attempts: u32,
    interval: Duration,
) {
    for i in 0..attempts {
        tokio::time::sleep(interval).await;
        if !ctx.meta_has(&name) {
            return; // killed while polling
        }
        if let Some(link) = rc_link_for(&ctx.claude_dir, pid).await {
            if !ctx.meta_has(&name) {
                return; // killed between the check and the write
            }
            let mut m = ctx.meta_get(&name).unwrap_or_default();
            m.rc_link = Some(link);
            ctx.meta_set(&name, m);
            ctx.host.log.push(format!(
                "rc-link captured {name} ({}s)",
                (i + 1) as f64 * interval.as_secs_f64()
            ));
            return;
        }
    }
    ctx.host.log.push(format!(
        "rc-link timeout {name} ({}s)",
        attempts as f64 * interval.as_secs_f64()
    ));
}

async fn spawn_claude(
    ctx: &Arc<Ctx>,
    dir: &str,
    claude_args: &[&str],
    meta: Meta,
) -> Result<String, BadRequest> {
    if let Err(e) = trust_dir(&claude_json_path(&ctx.claude_dir), dir).await {
        ctx.host.log.push(format!("trust write failed for {dir}: {e}"));
    }
    let name = tmux_name(dir);

    let mut args: Vec<&str> = vec!["new-session", "-d", "-s", &name, "-c", dir, "claude"];
    args.extend_from_slice(claude_args);
    args.extend_from_slice(&["--dangerously-skip-permissions", "--remote-control", &name]);
    ctx.runner.run("tmux", &args, "tmux new-session").await;

    ctx.meta_set(&name, meta);
    ctx.host.log.push(format!(
        "launch {} → {name}",
        Path::new(dir).file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default()
    ));

    let pid_out = ctx
        .runner
        .run("tmux", &["display-message", "-p", "-t", &name, "#{pane_pid}"], "tmux display-message")
        .await;
    if let Ok(pid) = pid_out.trim().parse::<i32>() {
        tokio::spawn(poll_rc_link(
            Arc::clone(ctx),
            name.clone(),
            pid,
            RC_POLL_ATTEMPTS,
            RC_POLL_INTERVAL,
        ));
    } else {
        ctx.host.log.push(format!("rc-poll skipped {name}: no pane pid"));
    }

    Ok(name)
}

pub async fn launch_session(
    ctx: &Arc<Ctx>,
    dir: &str,
    model: &str,
    effort: &str,
) -> Result<String, BadRequest> {
    assert_model(model)?;
    assert_effort(effort)?;
    // A6: the directory must exist and be a directory before it reaches `-c`.
    let is_dir = tokio::fs::metadata(dir).await.map(|m| m.is_dir()).unwrap_or(false);
    if !is_dir {
        return Err(BadRequest(format!("not a directory: {dir}")));
    }
    spawn_claude(
        ctx,
        dir,
        &["--model", model, "--effort", effort],
        Meta { model: Some(model.into()), effort: Some(effort.into()), rc_link: None },
    )
    .await
}

pub async fn resume_session(ctx: &Arc<Ctx>, sid: &str) -> Result<String, BadRequest> {
    assert_valid_sid(sid)?;
    let hist = read_if(&ctx.claude_dir.join("history.jsonl")).await.unwrap_or_default();
    let cwd = group_history(&hist)
        .into_iter()
        .find(|g| g.sid == sid)
        .and_then(|g| g.cwd)
        .filter(|c| !c.is_empty())
        .ok_or_else(|| BadRequest(format!("unknown session: {sid}")))?;

    // D6: un-purge before spawning, so the resumed session is not filtered
    // straight back out of the resumable list it came from.
    ctx.purged.lock().unwrap_or_else(|e| e.into_inner()).remove(sid);

    spawn_claude(ctx, &cwd, &["--resume", sid], Meta::default()).await
}

/// Kill a session this dashboard owns. The name is guarded first — it becomes
/// a `tmux` argument — and the meta entry is dropped whether or not tmux
/// agreed, which is what stops a poll still in flight from resurrecting it.
pub async fn kill_session(ctx: &Arc<Ctx>, name: &str) -> Result<(), BadRequest> {
    assert_kill_name(name)?;
    ctx.runner.run("tmux", &["kill-session", "-t", name], "tmux kill-session").await;
    ctx.meta_delete(name);
    ctx.host.log.push(format!("kill {name}"));
    Ok(())
}

/// Hide a resumable session from the list. Purely a note to ourselves — no
/// file is touched and the transcript is not deleted.
pub fn purge_session(ctx: &Arc<Ctx>, sid: &str) -> Result<(), BadRequest> {
    assert_valid_sid(sid)?;
    ctx.purged.lock().unwrap_or_else(|e| e.into_inner()).insert(sid.to_string());
    Ok(())
}
```

Extend the `validate` import at the top of the file to bring in the kill guard:

```rust
use super::validate::{
    assert_effort, assert_kill_name, assert_model, assert_valid_sid, BadRequest,
};
```

`Runner` is imported for the test module's `Host` construction; if clippy flags it as unused in the non-test build, move the import into the `tests` module.

- [x] **Step 4: Run tests to verify they pass**

Run: `cargo test -p cdash-agent spawn -- --test-threads=1`
Expected: PASS, 13 tests.

- [x] **Step 5: Commit**

```bash
git add crates/agent/src/collect/spawn.rs crates/agent/src/collect/mod.rs
git commit -m "feat: launch, resume, kill, purge, and a tested RC-link resurrection guard"
```

---

### Task 10: External sessions

Ports `externalSessions` (`collect.js:181-217`) — Claude sessions this dashboard did not launch, read-only. Four filters decide what appears; all four get a test.

Checklist rows: **E1**, **E3**, **E4**, **E5**, **E11**.

**Files:**
- Create: `crates/agent/src/collect/external.rs`
- Modify: `crates/agent/src/collect/mod.rs`

**Interfaces:**
- Consumes: `Ctx`, `session_file_for` (Task 5), `read_tail` (Task 2), `GitCache` (Task 4), `ProcRow`/`Sampler` (prior plan).
- Produces:
  - `pub struct Session { … }` — the `/api/sessions` `running[]` entry, shared with Task 11
  - `pub async fn external_sessions(ctx: &Arc<Ctx>, rows: &[ProcRow], pane_pids: &HashSet<i32>, now_ms: f64) -> Vec<Session>`

- [x] **Step 1: Write the failing tests**

`crates/agent/src/collect/external.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::host::log::LogBuffer;

    fn tempdir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("cdash-ext-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("sessions")).unwrap();
        dir
    }

    fn ctx_for(claude_dir: PathBuf) -> Arc<Ctx> {
        let log = Arc::new(LogBuffer::new());
        let path = std::env::var("PATH").unwrap_or_default();
        let host = crate::host::init::Host {
            runner: crate::host::cmd::Runner::new(path.clone(), Arc::clone(&log)),
            log,
            path,
            sampler: std::sync::Mutex::new(crate::host::sample::Sampler::new()),
        };
        Arc::new(Ctx::new(host, claude_dir, None))
    }

    fn write_session(dir: &Path, pid: i32, body: &str) {
        std::fs::write(dir.join("sessions").join(format!("{pid}.json")), body).unwrap();
    }

    fn rows(pids: &[i32]) -> Vec<ProcRow> {
        pids.iter()
            .map(|p| ProcRow { pid: *p, ppid: 1, cpu: 1.0, rss_kb: 100 })
            .collect()
    }

    const CLI: &str = r#"{"sessionId":"s-1","cwd":"/proj","entrypoint":"cli","startedAt":1000,"name":"api"}"#;

    #[tokio::test]
    async fn a_live_cli_session_is_reported() {
        let d = tempdir("live");
        write_session(&d, 500, CLI);
        let out = external_sessions(&ctx_for(d), &rows(&[500]), &HashSet::new(), 61_000.0).await;
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].name, "api");
        assert_eq!(out[0].dir, "/proj");
        assert_eq!(out[0].sid.as_deref(), Some("s-1"));
        assert!(out[0].external);
        assert_eq!(out[0].uptime_sec, 60, "(61000 - 1000) / 1000");
    }

    #[tokio::test]
    async fn a_name_less_session_falls_back_to_the_directory_basename() {
        let d = tempdir("noname");
        write_session(&d, 501, r#"{"sessionId":"s","cwd":"/a/proj","entrypoint":"cli"}"#);
        let out = external_sessions(&ctx_for(d), &rows(&[501]), &HashSet::new(), 0.0).await;
        assert_eq!(out[0].name, "proj");
        assert_eq!(out[0].uptime_sec, 0, "no startedAt means zero, not a negative age");
    }

    #[tokio::test]
    async fn an_sdk_cli_session_is_excluded() {
        // E1: claude-mem observers and SDK runs are not sessions anyone is
        // sitting in front of. Showing them makes the list untrustworthy.
        let d = tempdir("sdk");
        write_session(&d, 502, r#"{"sessionId":"s","cwd":"/p","entrypoint":"sdk-cli"}"#);
        let out = external_sessions(&ctx_for(d), &rows(&[502]), &HashSet::new(), 0.0).await;
        assert!(out.is_empty());
    }

    #[tokio::test]
    async fn a_session_missing_its_entrypoint_is_excluded() {
        let d = tempdir("noentry");
        write_session(&d, 503, r#"{"sessionId":"s","cwd":"/p"}"#);
        let out = external_sessions(&ctx_for(d), &rows(&[503]), &HashSet::new(), 0.0).await;
        assert!(out.is_empty());
    }

    #[tokio::test]
    async fn a_dead_pid_is_excluded() {
        // E4: the session file outlives the process that wrote it.
        let d = tempdir("dead");
        write_session(&d, 504, CLI);
        let out = external_sessions(&ctx_for(d), &rows(&[999]), &HashSet::new(), 0.0).await;
        assert!(out.is_empty());
    }

    #[tokio::test]
    async fn a_pid_already_shown_as_a_pane_is_excluded() {
        // E3: without this the same session appears twice, once per source.
        let d = tempdir("dupe");
        write_session(&d, 505, CLI);
        let panes: HashSet<i32> = [505].into_iter().collect();
        let out = external_sessions(&ctx_for(d), &rows(&[505]), &panes, 0.0).await;
        assert!(out.is_empty());
    }

    #[tokio::test]
    async fn a_session_without_session_id_or_cwd_is_excluded() {
        let d = tempdir("partial");
        write_session(&d, 506, r#"{"cwd":"/p","entrypoint":"cli"}"#);
        write_session(&d, 507, r#"{"sessionId":"s","entrypoint":"cli"}"#);
        let out = external_sessions(&ctx_for(d), &rows(&[506, 507]), &HashSet::new(), 0.0).await;
        assert!(out.is_empty());
    }

    #[tokio::test]
    async fn non_json_files_and_non_numeric_names_are_skipped() {
        // E11: the sessions directory is not guaranteed to hold only pid files.
        let d = tempdir("junk");
        std::fs::write(d.join("sessions/notes.txt"), "x").unwrap();
        std::fs::write(d.join("sessions/abc.json"), CLI).unwrap();
        let out = external_sessions(&ctx_for(d), &rows(&[1]), &HashSet::new(), 0.0).await;
        assert!(out.is_empty());
    }

    #[tokio::test]
    async fn a_missing_sessions_directory_yields_an_empty_list() {
        let dir = std::env::temp_dir().join(format!("cdash-ext-none-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let out = external_sessions(&ctx_for(dir), &rows(&[1]), &HashSet::new(), 0.0).await;
        assert!(out.is_empty());
    }
}
```

- [x] **Step 2: Run tests to verify they fail**

Add to `crates/agent/src/collect/mod.rs`:

```rust
pub mod external;
```

Run: `cargo test -p cdash-agent external`
Expected: FAIL — `cannot find function 'external_sessions' in this scope`.

- [x] **Step 3: Make `GitStatus` serializable**

`Session.git` is `Option<GitStatus>` and `Session` is serialized straight into `/api/sessions`, but `GitStatus` was built by the previous plan with no `Serialize`. In `crates/agent/src/parse/git.rs`, add the import and extend the derive:

```rust
use serde::Serialize;
```

```rust
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct GitStatus {
```

Its four field names — `branch`, `dirty`, `ahead`, `behind` — already match Node's, so no `rename` is needed. Add this test to the `tests` module in `crates/agent/src/parse/git.rs`:

```rust
    #[test]
    fn git_status_serializes_with_nodes_field_names() {
        let j = serde_json::to_string(&parse_git_status("## main...origin/main [ahead 2]\n M x\n"))
            .unwrap();
        assert_eq!(j, r#"{"branch":"main","dirty":1,"ahead":2,"behind":0}"#);
    }
```

Run: `cargo test -p cdash-agent git_status_serializes`
Expected: PASS.

- [x] **Step 4: Write the implementation**

Prepend to `crates/agent/src/collect/external.rs`:

```rust
use super::ctx::Ctx;
use super::fsio::read_tail;
use super::lookup::session_file_for;
use crate::host::proc::ProcRow;
use crate::parse::git::GitStatus;
use crate::parse::paths::project_dir_name;
use crate::parse::transcript::parse_transcript;
use crate::parse::{git::parse_git_status, tmux::Pane};
use serde::Serialize;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// One entry of `/api/sessions` `running[]`. Field names and shapes are the
/// parity gate's contract — see the exemption list in the spec before changing
/// any of them.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Session {
    pub name: String,
    pub dir: String,
    pub pid: i32,
    #[serde(rename = "uptimeSec")]
    pub uptime_sec: i64,
    pub model: Option<String>,
    pub effort: Option<String>,
    #[serde(rename = "rcLink")]
    pub rc_link: Option<String>,
    pub git: Option<GitStatus>,
    pub working: bool,
    #[serde(rename = "lastMessage")]
    pub last_message: Option<String>,
    pub sid: Option<String>,
    pub cpu: Option<f32>,
    #[serde(rename = "rssKb")]
    pub rss_kb: u64,
    /// Rust-only; Node has no such field. Exempted by name in the parity gate.
    #[serde(rename = "cpuSampleAgeMs")]
    pub cpu_sample_age_ms: u128,
    /// Node omitted this key entirely for pane sessions and set it to `true`
    /// for external ones. `skip_serializing_if` reproduces that exactly.
    #[serde(skip_serializing_if = "is_false")]
    pub external: bool,
}

fn is_false(b: &bool) -> bool {
    !*b
}

/// Sessions this dashboard did not launch: every `~/.claude/sessions/<pid>.json`
/// whose pid is still alive. Read-only — they live in terminals we do not own.
pub async fn external_sessions(
    ctx: &Arc<Ctx>,
    rows: &[ProcRow],
    pane_pids: &HashSet<i32>,
    now_ms: f64,
) -> Vec<Session> {
    let alive: HashSet<i32> = rows.iter().map(|r| r.pid).collect();
    let dir = ctx.claude_dir.join("sessions");
    let Ok(mut entries) = tokio::fs::read_dir(&dir).await else {
        return Vec::new();
    };

    let mut out = Vec::new();
    while let Ok(Some(e)) = entries.next_entry().await {
        let p = e.path();
        if p.extension().and_then(|x| x.to_str()) != Some("json") {
            continue;
        }
        let Some(pid) = p
            .file_stem()
            .and_then(|s| s.to_str())
            .and_then(|s| s.parse::<i32>().ok())
        else {
            continue;
        };
        if pane_pids.contains(&pid) || !alive.contains(&pid) {
            continue;
        }
        let Some(sess) = session_file_for(&ctx.claude_dir, pid).await else { continue };
        let (Some(sid), Some(cwd)) = (sess.session_id.clone(), sess.cwd.clone()) else { continue };
        // E1: 'cli' is a session someone is sitting in front of; 'sdk-cli' is
        // programmatic and not ours to show.
        if sess.entrypoint.as_deref() != Some("cli") {
            continue;
        }

        let file = ctx
            .claude_dir
            .join("projects")
            .join(project_dir_name(&cwd))
            .join(format!("{sid}.jsonl"));
        let md = tokio::fs::metadata(&file).await.ok();
        let mut last_message = None;
        if md.is_some() {
            if let Some(txt) = read_tail(&file).await {
                last_message = parse_transcript(&txt).last_assistant_text;
            }
        }
        let working = md
            .as_ref()
            .map(|m| now_ms - super::cache::mtime_ms(m) < 10_000.0)
            .unwrap_or(false);

        let usage = {
            let mut s = ctx.host.sampler.lock().unwrap_or_else(|e| e.into_inner());
            s.tree_usage(pid)
        };
        let git_out = Arc::clone(&ctx.git).status_for(
            Arc::clone(&ctx.runner),
            &cwd,
            now_ms as u64,
        );

        out.push(Session {
            name: sess
                .name
                .filter(|n| !n.is_empty())
                .unwrap_or_else(|| basename(&cwd)),
            dir: cwd,
            pid,
            uptime_sec: sess
                .started_at
                .map(|t| (((now_ms - t) / 1000.0).round() as i64).max(0))
                .unwrap_or(0),
            model: None,
            effort: None,
            rc_link: sess
                .bridge_session_id
                .map(|id| format!("https://claude.ai/code/{id}")),
            git: git_out.as_deref().map(parse_git_status),
            working,
            last_message,
            sid: Some(sid),
            cpu: usage.cpu,
            rss_kb: usage.rss_kb,
            cpu_sample_age_ms: usage.cpu_sample_age_ms,
            external: true,
        });
    }
    out
}

fn basename(p: &str) -> String {
    Path::new(p)
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| p.to_string())
}
```

Remove the `Pane` and `PathBuf` imports if the compiler reports them unused here — they are listed because Task 11 shares this module's `Session` type.

- [x] **Step 5: Run tests to verify they pass**

Run: `cargo test -p cdash-agent external -- --test-threads=1`
Expected: PASS, 9 tests.

- [x] **Step 6: Commit**

```bash
git add crates/agent/src/collect/external.rs crates/agent/src/collect/mod.rs crates/agent/src/parse/git.rs
git commit -m "feat: port external session discovery with all four exclusion filters"
```

---

### Task 11: `collect_sessions` — the orchestrator and the response shape

Ports `collectSessions` (`collect.js:219-278`). This is the function `/api/sessions` returns verbatim, so its field names are the parity gate's contract.

Checklist rows: **B5**, **D9**, **E6**, **E7**, **E8**.

**Files:**
- Create: `crates/agent/src/collect/sessions.rs`
- Modify: `crates/agent/src/collect/mod.rs`
- Modify: `public/app.js:162`

**Interfaces:**
- Consumes: everything above.
- Produces:
  - `pub struct Resumable { pub sid: String, pub dir: Option<String>, pub ts: f64, pub branch: Option<String>, pub title: String, pub prompts: Vec<String> }`
  - `pub struct Stats { pub cpu_pct: u32, pub ram_used_kb: u64, pub ram_total_kb: u64, pub disks: Vec<DiskUsage> }`
  - `pub struct SessionsResponse { pub running: Vec<Session>, pub resumable: Vec<Resumable>, pub stats: Stats }`
  - `pub const RESUMABLE_MAX: usize` = 20
  - `pub async fn collect_sessions(ctx: &Arc<Ctx>) -> SessionsResponse`

- [x] **Step 1: Write the failing tests**

`crates/agent/src/collect/sessions.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::host::log::LogBuffer;

    fn tempdir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("cdash-sess-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("sessions")).unwrap();
        dir
    }

    fn ctx_for(claude_dir: PathBuf) -> Arc<Ctx> {
        let log = Arc::new(LogBuffer::new());
        let path = std::env::var("PATH").unwrap_or_default();
        let host = crate::host::init::Host {
            runner: crate::host::cmd::Runner::new(path.clone(), Arc::clone(&log)),
            log,
            path,
            sampler: std::sync::Mutex::new(crate::host::sample::Sampler::new()),
        };
        Arc::new(Ctx::new(host, claude_dir, None))
    }

    /// Write a history entry plus a transcript with `turns` assistant messages.
    fn seed(dir: &Path, sid: &str, cwd: &str, ts: i64, turns: usize) -> String {
        let proj = dir.join("projects").join(crate::parse::paths::project_dir_name(cwd));
        std::fs::create_dir_all(&proj).unwrap();
        let body: String = (0..turns)
            .map(|i| format!("{{\"type\":\"assistant\",\"message\":{{\"content\":[{{\"type\":\"text\",\"text\":\"t{i}\"}}]}}}}\n"))
            .collect();
        std::fs::write(proj.join(format!("{sid}.jsonl")), body).unwrap();
        format!("{{\"sessionId\":\"{sid}\",\"project\":\"{cwd}\",\"timestamp\":{ts},\"display\":\"do the thing\"}}\n")
    }

    #[tokio::test]
    async fn a_resumable_session_needs_three_assistant_turns() {
        // E7: one or two turns is an abandoned start, not work worth resuming.
        let d = tempdir("turns");
        let a = seed(&d, "aaa", "/p/deep", 300, 3);
        let b = seed(&d, "bbb", "/p/shallow", 200, 2);
        std::fs::write(d.join("history.jsonl"), format!("{a}{b}")).unwrap();

        let r = collect_sessions(&ctx_for(d)).await;
        let sids: Vec<&str> = r.resumable.iter().map(|x| x.sid.as_str()).collect();
        assert_eq!(sids, vec!["aaa"]);
    }

    #[tokio::test]
    async fn a_session_with_no_transcript_is_not_resumable() {
        // E8: history remembers sessions whose transcript is gone.
        let d = tempdir("notranscript");
        std::fs::write(
            d.join("history.jsonl"),
            "{\"sessionId\":\"ghost\",\"project\":\"/p\",\"timestamp\":1,\"display\":\"x\"}\n",
        )
        .unwrap();
        let r = collect_sessions(&ctx_for(d)).await;
        assert!(r.resumable.is_empty());
    }

    #[tokio::test]
    async fn a_purged_session_is_hidden() {
        // E6: purge is the user saying "stop showing me this".
        let d = tempdir("purged");
        let a = seed(&d, "ccc", "/p", 300, 3);
        std::fs::write(d.join("history.jsonl"), a).unwrap();
        let ctx = ctx_for(d);
        ctx.purged.lock().unwrap().insert("ccc".to_string());
        assert!(collect_sessions(&ctx).await.resumable.is_empty());
    }

    #[tokio::test]
    async fn the_resumable_list_is_capped() {
        // B5: history holds 60 groups; the UI gets at most 20.
        let d = tempdir("cap");
        let mut hist = String::new();
        for i in 0..(RESUMABLE_MAX + 5) {
            let sid = format!("{i:08}-0000-4000-8000-000000000000");
            hist.push_str(&seed(&d, &sid, &format!("/p{i}"), 1000 - i as i64, 3));
        }
        std::fs::write(d.join("history.jsonl"), hist).unwrap();
        let r = collect_sessions(&ctx_for(d)).await;
        assert_eq!(r.resumable.len(), RESUMABLE_MAX);
    }

    #[tokio::test]
    async fn the_title_falls_back_to_the_first_prompt_then_to_untitled() {
        let d = tempdir("title");
        let a = seed(&d, "ddd", "/p", 300, 3);
        std::fs::write(d.join("history.jsonl"), a).unwrap();
        let r = collect_sessions(&ctx_for(d)).await;
        assert_eq!(r.resumable[0].title, "do the thing");
    }

    #[tokio::test]
    async fn the_response_serializes_with_nodes_field_names() {
        let d = tempdir("shape");
        std::fs::write(d.join("history.jsonl"), "").unwrap();
        let r = collect_sessions(&ctx_for(d)).await;
        let j = serde_json::to_string(&r).unwrap();
        assert!(j.contains("\"running\":"));
        assert!(j.contains("\"resumable\":"));
        assert!(j.contains("\"cpuPct\":"));
        assert!(j.contains("\"ramUsedKb\":"));
        assert!(j.contains("\"ramTotalKb\":"));
        assert!(j.contains("\"disks\":"));
    }

    #[tokio::test]
    async fn the_root_disk_is_always_reported() {
        let d = tempdir("disks");
        std::fs::write(d.join("history.jsonl"), "").unwrap();
        let r = collect_sessions(&ctx_for(d)).await;
        assert_eq!(r.stats.disks[0].mount, "/");
        assert!(r.stats.disks[0].total_kb > 0);
    }

    #[test]
    fn a_pane_session_omits_the_external_key_entirely() {
        // Node set `external` only on external sessions; a pane entry has no
        // such key at all, and the gate compares keys.
        let s = Session {
            name: "x".into(), dir: "/x".into(), pid: 1, uptime_sec: 0,
            model: None, effort: None, rc_link: None, git: None, working: false,
            last_message: None, sid: None, cpu: None, rss_kb: 0,
            cpu_sample_age_ms: 0, external: false,
        };
        let j = serde_json::to_string(&s).unwrap();
        assert!(!j.contains("external"));
    }
}
```

- [x] **Step 2: Run tests to verify they fail**

Add to `crates/agent/src/collect/mod.rs`:

```rust
pub mod sessions;
```

Run: `cargo test -p cdash-agent sessions`
Expected: FAIL — `cannot find function 'collect_sessions' in this scope`.

- [x] **Step 3: Write the implementation**

Prepend to `crates/agent/src/collect/sessions.rs`:

```rust
use super::ctx::{Ctx, Meta};
use super::external::{external_sessions, Session};
use super::fsio::{read_if, read_tail};
use super::lookup::{session_file_for, transcript_for};
use crate::host::disk::{disk_usage, DiskUsage};
use crate::parse::git::parse_git_status;
use crate::parse::history::group_history;
use crate::parse::paths::project_dir_name;
use crate::parse::tmux::{parse_tmux_panes, PANE_FORMAT};
use crate::parse::transcript::parse_transcript;
use serde::Serialize;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// Mirrors the `resumable.length >= 20` break (`lib/collect.js:269`).
pub const RESUMABLE_MAX: usize = 20;

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Resumable {
    pub sid: String,
    pub dir: Option<String>,
    pub ts: f64,
    pub branch: Option<String>,
    pub title: String,
    pub prompts: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Stats {
    #[serde(rename = "cpuPct")]
    pub cpu_pct: u32,
    #[serde(rename = "ramUsedKb")]
    pub ram_used_kb: u64,
    #[serde(rename = "ramTotalKb")]
    pub ram_total_kb: u64,
    pub disks: Vec<DiskUsage>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct SessionsResponse {
    pub running: Vec<Session>,
    pub resumable: Vec<Resumable>,
    pub stats: Stats,
}

fn now_ms() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs_f64() * 1000.0)
        .unwrap_or(0.0)
}

pub async fn collect_sessions(ctx: &Arc<Ctx>) -> SessionsResponse {
    let panes_out = ctx
        .runner
        .run("tmux", &["list-panes", "-a", "-F", PANE_FORMAT], "tmux list-panes")
        .await;
    let panes = parse_tmux_panes(&panes_out);
    let now = now_ms();

    // One sample serves every session in this response; the 200 ms rule lives
    // inside `Sampler` and must not be re-implemented here.
    let rows = {
        let mut s = ctx.host.sampler.lock().unwrap_or_else(|e| e.into_inner());
        s.sample()
    };

    let mut running: Vec<Session> = Vec::new();
    for p in &panes {
        let meta = ctx.meta_get(&p.name).unwrap_or_default();
        let sess = session_file_for(&ctx.claude_dir, p.pid).await;

        let rc_link = meta.rc_link.clone().or_else(|| {
            sess.as_ref()
                .and_then(|s| s.bridge_session_id.clone())
                .map(|id| format!("https://claude.ai/code/{id}"))
        });
        // D9: memoize a link discovered from the session file, so a later poll
        // does not have to rediscover it.
        if rc_link.is_some() && meta.rc_link.is_none() {
            ctx.meta_set(&p.name, Meta { rc_link: rc_link.clone(), ..meta.clone() });
        }

        // Prefer the pane's own session id; only guess from mtime when the
        // session file has no id yet — several panes can share one cwd.
        let mut tr: Option<(PathBuf, f64)> = None;
        if let Some(sid) = sess.as_ref().and_then(|s| s.session_id.clone()) {
            let cwd = sess
                .as_ref()
                .and_then(|s| s.cwd.clone())
                .unwrap_or_else(|| p.path.clone());
            let file = ctx
                .claude_dir
                .join("projects")
                .join(project_dir_name(&cwd))
                .join(format!("{sid}.jsonl"));
            if let Ok(md) = tokio::fs::metadata(&file).await {
                tr = Some((file, super::cache::mtime_ms(&md)));
            }
        }
        if tr.is_none() {
            tr = transcript_for(&ctx.claude_dir, &p.path, p.created).await;
        }

        let (mut working, mut last_message, mut sid) = (false, None, None);
        if let Some((file, mtime)) = &tr {
            working = now - mtime < 10_000.0;
            sid = file
                .file_stem()
                .map(|s| s.to_string_lossy().into_owned());
            if let Some(txt) = read_tail(file).await {
                last_message = parse_transcript(&txt).last_assistant_text;
            }
        }

        let usage = crate::host::proc::proc_tree_usage(&rows, p.pid);
        let cpu_state = {
            let mut s = ctx.host.sampler.lock().unwrap_or_else(|e| e.into_inner());
            s.tree_usage(p.pid)
        };
        let git_out = Arc::clone(&ctx.git).status_for(Arc::clone(&ctx.runner), &p.path, now as u64);

        running.push(Session {
            name: p.name.clone(),
            dir: p.path.clone(),
            pid: p.pid,
            uptime_sec: ((now / 1000.0 - p.created as f64).round() as i64).max(0),
            model: meta.model.clone(),
            effort: meta.effort.clone(),
            rc_link,
            git: git_out.as_deref().map(parse_git_status),
            working,
            last_message,
            sid,
            cpu: cpu_state.cpu,
            rss_kb: usage.rss_kb,
            cpu_sample_age_ms: cpu_state.cpu_sample_age_ms,
            external: false,
        });
    }

    let pane_pids: HashSet<i32> = panes.iter().map(|p| p.pid).collect();
    running.extend(external_sessions(ctx, &rows, &pane_pids, now).await);

    let running_sids: HashSet<String> =
        running.iter().filter_map(|r| r.sid.clone()).collect();
    let hist = read_if(&ctx.claude_dir.join("history.jsonl")).await.unwrap_or_default();

    let mut resumable = Vec::new();
    for g in group_history(&hist) {
        if resumable.len() >= RESUMABLE_MAX {
            break;
        }
        if running_sids.contains(&g.sid)
            || ctx.purged.lock().unwrap_or_else(|e| e.into_inner()).contains(&g.sid)
        {
            continue;
        }
        let file = ctx
            .claude_dir
            .join("projects")
            .join(project_dir_name(g.cwd.as_deref().unwrap_or("")))
            .join(format!("{}.jsonl", g.sid));
        let Some(t) = ctx.transcripts.get(&file).await else { continue };
        if t.assistant_count < 3 {
            continue;
        }
        resumable.push(Resumable {
            title: t
                .title
                .clone()
                .or_else(|| g.prompts.first().cloned())
                .unwrap_or_else(|| "(untitled)".to_string()),
            sid: g.sid,
            dir: g.cwd,
            ts: g.ts,
            branch: t.branch.clone(),
            prompts: g.prompts,
        });
    }

    let machine = {
        let mut s = ctx.host.sampler.lock().unwrap_or_else(|e| e.into_inner());
        s.machine_stats()
    };
    let mut disks: Vec<DiskUsage> = Vec::new();
    if let Some(d) = disk_usage("/") {
        disks.push(d);
    }
    if let Some(extra) = ctx.disk_extra.as_deref() {
        if let Some(d) = disk_usage(extra) {
            disks.push(d);
        }
    }

    SessionsResponse {
        running,
        resumable,
        stats: Stats {
            cpu_pct: machine.cpu_pct,
            ram_used_kb: machine.ram_used_kb,
            ram_total_kb: machine.ram_total_kb,
            disks,
        },
    }
}
```

`g.ts` is `f64` in `HistoryGroup`; `Resumable::ts` matches it so the JSON number is unchanged. Drop the unused `Path` import if the compiler objects.

- [x] **Step 4: Run tests to verify they pass**

Run: `cargo test -p cdash-agent sessions -- --test-threads=1`
Expected: PASS, 8 tests.

- [x] **Step 5: Ship the one-line UI change that `cpu: null` requires**

The Rust agent reports `cpu` as `null` until two samples are 200 ms apart. `public/app.js:162` interpolates it directly, so an unsampled session would render `cpu null%`. This is the single documented exception to "the pivot does not rewrite `public/`", and it ships **here**, with the change that introduces the null — not with the later UI step.

In `public/app.js`, line 162, change:

```javascript
        ${chipHtml}<span>cpu ${r.cpu}%</span><span class="sep">·</span><span>${fmtKb(r.rssKb)}</span>
```

to:

```javascript
        ${chipHtml}<span>cpu ${r.cpu ?? '—'}%</span><span class="sep">·</span><span>${fmtKb(r.rssKb)}</span>
```

- [x] **Step 6: Verify the UI change by hand**

Run: `grep -n "cpu \${r.cpu" public/app.js`
Expected: one line, containing `?? '—'`.

- [x] **Step 7: Commit**

```bash
git add crates/agent/src/collect/sessions.rs crates/agent/src/collect/mod.rs public/app.js
git commit -m "feat: collect_sessions orchestrator and the /api/sessions response shape"
```

---

### Task 12: Checklist audit and the full gate

The plan's own closing argument. A control with no test is a control that will be dropped by the next refactor and noticed by nobody.

**Files:**
- Modify: `docs/superpowers/plans/2026-08-14-agent-port-collect-and-orchestration.md`

**Interfaces:**
- Consumes: everything.
- Produces: an audited checklist and a green suite.

- [x] **Step 1: Audit every checklist row against a real test name**

For each row in [the derived control checklist](#the-derived-control-checklist) whose "Ported in" column names a task in this plan, find the test that proves it and write the test's name into the row. Run this to list every test name available:

```bash
cargo test -p cdash-agent -- --list
```

A row with no test is **not** closed by writing prose. Add the missing test, then update the row.

- [x] **Step 2: Confirm no row is unclaimed**

Run:

```bash
grep -c '^| [A-E][0-9]' docs/superpowers/plans/2026-08-14-agent-port-collect-and-orchestration.md
```

Expected: 41 (A×7, B×9, C×4, D×9, E×12). Then read the table and confirm every row's rightmost column names either a test or `**done**` with the prior plan's task. **No row may defer to a later step** — every control in the table is either already proven or proven by this plan.

- [x] **Step 3: Run the full suite**

Run: `cargo test --all --locked -- --test-threads=1`
Expected: PASS. Single-threaded because the sampler's timing assertions and the temp-directory fixtures are sensitive to parallel execution.

- [x] **Step 4: Run the lint gate**

Run: `cargo clippy --all-targets --locked -- -D warnings -D clippy::disallowed_types`
Expected: exit 0.

Then confirm this plan added no subprocess site:

```bash
grep -rn "allow(clippy::disallowed_types)" crates/
```

Expected: exactly two lines, both in `host/path.rs` and `host/cmd.rs`. A third is a defect.

- [x] **Step 5: Confirm the Node suite still passes**

The Node tree is the parity reference and must remain runnable until step 5 retires it.

Run: `npm test`
Expected: PASS, 22 tests.

- [x] **Step 6: Commit**

```bash
git add docs/superpowers/plans/2026-08-14-agent-port-collect-and-orchestration.md
git commit -m "docs: audit the control checklist against the tests that prove it"
```

---

## Handoff to step 4 (the HTTP layer)

Every route body is a tested function after this plan. Three wiring obligations remain, listed so they cannot be lost between plans:

- **`assert_path` is not called by anything yet.** It guards the `/api/favorites` body (`server.js:52`), and `/api/favorites` is a route. The function is tested; step 4 must call it and test that a relative path yields a 400 rather than reaching `toggle_favorite`.
- **`addRecent` is fire-and-forget** (`server.js:56`): a failed recents write logs and does not fail the launch. The launch response is `{ name }` and must not wait on it. Note the route resolves the directory (`path.resolve(req.body.dir)`) before recording it, while `launch_session` receives the raw value — keep both.
- **Error status mapping.** `BadRequest` renders as 400 with `{ error: <message> }`, `BrowseError` likewise via `status()`, and anything else is a 500 — matching `server.js:41`. `/api/sessions` is the exception: it logs and returns a 500 with the message (`server.js:34`), never a 400.

## What this plan deliberately does not cover

- **Anything that binds a socket.** No router, no routes, no `serve`, no static serving.
- **Auth.** Spec step 6, and absent from the Node tree entirely — there is nothing to port.
- **Deleting the Node tree.** It remains the parity reference until the step 5 gate passes.

## Next plan starts here

Spec step 4: `router(ctx)`, the ten existing routes, static serving, and `serve(cfg)` returning the bound address. Every route's handler body is already a tested function in `collect::` — step 4 is wiring and status codes, plus the three handoff items above. After it, step 5 is the parity gate (adding the two exemptions this plan's derivation found), and the Node tree is deleted the moment it passes.
