# Rust Agent Port — Parsers and Host Layer Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Stand up the Rust agent crate and port every pure parser and the OS-abstraction layer from the Node implementation, with test coverage at least equal to what exists today.

**Architecture:** A new Cargo workspace at `crates/agent` containing a library (`cdash_agent`) and a binary. This plan builds two module trees: `parse/` (pure functions over strings, no I/O) and `host/` (PATH resolution, the subprocess helper, binary probing, disk and process stats). Nothing in this plan binds a socket or serves a request — that is the next plan. The Node tree stays untouched on disk; it is the parity reference until a later plan retires it.

**Tech Stack:** Rust (edition 2021), `serde`/`serde_json`, `sysinfo` 0.38.4, `rustix` (for `statvfs`), `regex`, `tokio` (process + time), `clippy` as a required CI gate.

## Global Constraints

Copied verbatim from `docs/superpowers/specs/2026-07-30-tauri-multi-host-design.md`. Every task's requirements implicitly include this section.

- **`sysinfo` is pinned to `0.38.4`.** Version `0.39.x` requires Rust 1.95; do not upgrade.
- **`-D clippy::disallowed_types` is a REQUIRED build gate, not advisory.** A green build with this lint disabled is not a valid build. It is the only enforcement of the subprocess time-box.
- **No direct use of `std::process::Command` or `tokio::process::Command`** except at exactly **two** sanctioned sites, each carrying an explicit `#[allow(clippy::disallowed_types)]` and a comment saying why: `host/cmd.rs` (the helper itself) and `host/path.rs` (the PATH probe, which must run before `Runner` exists because `Runner::new` takes the resolved PATH as an argument). A third site is a defect.
- **Default subprocess time-box is 5 seconds**, killed hard on expiry. The PATH probe alone uses 2000 ms.
- **The PATH probe never gates startup.** On timeout or non-zero exit, continue with the inherited PATH.
- **Field semantics must match Node exactly** for every value that reaches `/api/sessions`. A later parity gate compares them field-by-field. Do not "improve" a field's shape, name, or rounding — an improvement is indistinguishable from a regression at that gate.
- **Rust version floor: 1.94.1.**

---

### Task 1: Cargo workspace, crate skeleton, and the clippy gate

The lint gate lands first because it is a required gate for every later task, and retrofitting it means auditing code already written.

**Files:**
- Create: `Cargo.toml` (workspace root)
- Create: `crates/agent/Cargo.toml`
- Create: `crates/agent/src/lib.rs`
- Create: `crates/agent/src/main.rs`
- Create: `clippy.toml`
- Create: `.github/workflows/ci.yml`

**Interfaces:**
- Consumes: nothing.
- Produces: crate `cdash_agent`; the command `cargo test -p cdash-agent`; the gate `cargo clippy --all-targets -- -D warnings -D clippy::disallowed_types`.

- [x] **Step 1: Create the workspace root**

`Cargo.toml`:

```toml
[workspace]
members = ["crates/agent"]
resolver = "2"
```

- [x] **Step 2: Create the agent crate manifest**

`crates/agent/Cargo.toml`:

```toml
[package]
name = "cdash-agent"
version = "0.1.0"
edition = "2021"
rust-version = "1.94.1"

[lib]
name = "cdash_agent"
path = "src/lib.rs"

[[bin]]
name = "cdash-agent"
path = "src/main.rs"

[dependencies]
serde = { version = "1", features = ["derive"] }
serde_json = "1"
sysinfo = "=0.38.4"
regex = "1"
rustix = { version = "1", features = ["fs"] }
tokio = { version = "1", features = ["process", "time", "rt", "macros"] }
```

- [x] **Step 3: Create the library and binary entry points**

`crates/agent/src/lib.rs`:

```rust
pub mod parse;
```

`crates/agent/src/main.rs`:

```rust
fn main() {
    println!("cdash-agent {}", env!("CARGO_PKG_VERSION"));
}
```

Create `crates/agent/src/parse/mod.rs` as an empty file for now — later tasks add modules to it.

- [x] **Step 4: Add the clippy gate configuration**

`clippy.toml`:

```toml
disallowed-types = ["std::process::Command", "tokio::process::Command"]
```

- [x] **Step 5: Add CI running both the tests and the gate**

`.github/workflows/ci.yml`:

```yaml
name: ci
on: [push, pull_request]
jobs:
  test:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - uses: dtolnay/rust-toolchain@stable
        with:
          components: clippy
      - run: cargo test --all
      - run: cargo clippy --all-targets -- -D warnings -D clippy::disallowed_types
```

- [x] **Step 6: Verify the crate builds and the gate runs**

Run: `cargo test --all`
Expected: PASS, 0 tests.

Run: `cargo clippy --all-targets -- -D warnings -D clippy::disallowed_types`
Expected: no warnings, exit 0.

- [x] **Step 7: Verify the gate actually fails on a bypass**

A gate nobody has seen fail is not known to work. Temporarily add to `crates/agent/src/main.rs`:

```rust
#[allow(dead_code)]
fn bypass() {
    let _ = std::process::Command::new("true");
}
```

Run: `cargo clippy --all-targets -- -D warnings -D clippy::disallowed_types`
Expected: FAIL with `error: use of a disallowed type 'std::process::Command'`.

Then delete `bypass()` and re-run; expected: exit 0.

- [x] **Step 8: Commit**

```bash
git add Cargo.toml crates/agent/Cargo.toml crates/agent/src/lib.rs crates/agent/src/main.rs crates/agent/src/parse/mod.rs clippy.toml .github/workflows/ci.yml
git commit -m "feat: cargo workspace, agent crate skeleton, and the clippy disallowed-types gate"
```

---

### Task 2: Port `usablePrompts` and `groupHistory`

Ports `lib/sessions.js:1-35`. Node's `groupHistory` sorts newest-first, caps at 60 groups, and keeps the last 3 usable prompts per group.

**Files:**
- Create: `crates/agent/src/parse/history.rs`
- Modify: `crates/agent/src/parse/mod.rs`

**Interfaces:**
- Consumes: nothing.
- Produces:
  - `pub fn usable_prompts(displays: &[String]) -> Vec<String>`
  - `pub struct HistoryGroup { pub sid: String, pub cwd: Option<String>, pub ts: i64, pub prompts: Vec<String> }`
  - `pub fn group_history(jsonl: &str) -> Vec<HistoryGroup>`

- [x] **Step 1: Write the failing tests**

`crates/agent/src/parse/history.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| x.to_string()).collect()
    }

    #[test]
    fn usable_prompts_filters_junk() {
        let input = s(&["/model", "", "ok", "YES", "fix the auth bug", "continue", "add tests"]);
        assert_eq!(usable_prompts(&input), s(&["fix the auth bug", "add tests"]));
    }

    #[test]
    fn group_history_groups_sorts_and_keeps_last_three() {
        let jsonl = [
            r#"{"sessionId":"a","project":"/x","timestamp":100,"display":"first prompt"}"#,
            r#"{"sessionId":"b","project":"/y","timestamp":300,"display":"other session"}"#,
            r#"{"sessionId":"a","project":"/x","timestamp":200,"display":"p2"}"#,
            r#"{"sessionId":"a","project":"/x","timestamp":250,"display":"p3"}"#,
            r#"{"sessionId":"a","project":"/x","timestamp":260,"display":"p4"}"#,
            "not json — must be skipped",
        ]
        .join("\n");

        let g = group_history(&jsonl);
        assert_eq!(g[0].sid, "b");
        assert_eq!(g[1].sid, "a");
        assert_eq!(g[1].ts, 260);
        assert_eq!(g[1].cwd.as_deref(), Some("/x"));
        assert_eq!(g[1].prompts, s(&["p2", "p3", "p4"]));
    }

    #[test]
    fn group_history_skips_entries_without_a_session_id() {
        let jsonl = r#"{"project":"/x","timestamp":100,"display":"orphan"}"#;
        assert!(group_history(jsonl).is_empty());
    }
}
```

- [x] **Step 2: Run tests to verify they fail**

Run: `cargo test -p cdash-agent history`
Expected: FAIL — `cannot find function 'usable_prompts' in this scope`.

- [x] **Step 3: Write the implementation**

Prepend to `crates/agent/src/parse/history.rs`:

```rust
use serde::Deserialize;
use std::collections::HashMap;

const STOPWORDS: &[&str] = &[
    "continue", "resume", "exit", "usage", "ok", "yes", "no", "quit", "y", "n",
];

pub fn usable_prompts(displays: &[String]) -> Vec<String> {
    displays
        .iter()
        .filter(|d| {
            let t = d.trim();
            !t.is_empty()
                && !t.starts_with('/')
                && !STOPWORDS.contains(&t.to_lowercase().as_str())
        })
        .cloned()
        .collect()
}

#[derive(Debug, Clone, PartialEq)]
pub struct HistoryGroup {
    pub sid: String,
    pub cwd: Option<String>,
    pub ts: i64,
    pub prompts: Vec<String>,
}

#[derive(Deserialize)]
struct HistoryEntry {
    #[serde(rename = "sessionId")]
    session_id: Option<String>,
    project: Option<String>,
    timestamp: Option<i64>,
    display: Option<String>,
}

/// Parse newline-delimited JSON, silently skipping malformed lines.
/// Mirrors `parseLines` in `lib/sessions.js:10-17`.
fn parse_lines<T: for<'de> Deserialize<'de>>(text: &str) -> Vec<T> {
    text.lines()
        .filter(|l| !l.trim().is_empty())
        .filter_map(|l| serde_json::from_str::<T>(l).ok())
        .collect()
}

pub fn group_history(jsonl: &str) -> Vec<HistoryGroup> {
    struct Acc {
        sid: String,
        cwd: Option<String>,
        ts: i64,
        displays: Vec<String>,
    }

    let mut by_sid: HashMap<String, Acc> = HashMap::new();
    let mut order: Vec<String> = Vec::new();

    for e in parse_lines::<HistoryEntry>(jsonl) {
        let Some(sid) = e.session_id else { continue };
        let acc = by_sid.entry(sid.clone()).or_insert_with(|| {
            order.push(sid.clone());
            Acc { sid: sid.clone(), cwd: None, ts: 0, displays: Vec::new() }
        });
        // Node: `g.cwd = e.project ?? g.cwd` — only replaces when present.
        if e.project.is_some() {
            acc.cwd = e.project;
        }
        acc.ts = acc.ts.max(e.timestamp.unwrap_or(0));
        if let Some(d) = e.display {
            acc.displays.push(d);
        }
    }

    let mut groups: Vec<HistoryGroup> = order
        .into_iter()
        .filter_map(|sid| by_sid.remove(&sid))
        .map(|a| {
            let usable = usable_prompts(&a.displays);
            let start = usable.len().saturating_sub(3);
            HistoryGroup { sid: a.sid, cwd: a.cwd, ts: a.ts, prompts: usable[start..].to_vec() }
        })
        .collect();

    groups.sort_by(|a, b| b.ts.cmp(&a.ts));
    groups.truncate(60);
    groups
}
```

Add to `crates/agent/src/parse/mod.rs`:

```rust
pub mod history;
```

- [x] **Step 4: Run tests to verify they pass**

Run: `cargo test -p cdash-agent history`
Expected: PASS, 3 tests.

- [x] **Step 5: Run the lint gate**

Run: `cargo clippy --all-targets -- -D warnings -D clippy::disallowed_types`
Expected: exit 0.

- [x] **Step 6: Commit**

```bash
git add crates/agent/src/parse/history.rs crates/agent/src/parse/mod.rs
git commit -m "feat: port usablePrompts and groupHistory to Rust"
```

---

### Task 3: Port `parseTranscript` and `parseRcFile`

Ports `lib/sessions.js:37-51`.

**Files:**
- Create: `crates/agent/src/parse/transcript.rs`
- Modify: `crates/agent/src/parse/mod.rs`
- Modify: `crates/agent/src/parse/history.rs` (make `parse_lines` visible to siblings)

**Interfaces:**
- Consumes: `parse_lines` from Task 2.
- Produces:
  - `pub struct Transcript { pub branch: Option<String>, pub title: Option<String>, pub assistant_count: u32, pub last_assistant_text: Option<String> }`
  - `pub fn parse_transcript(jsonl: &str) -> Transcript`
  - `pub fn parse_rc_file(json: &str) -> Option<String>`

- [x] **Step 1: Write the failing tests**

`crates/agent/src/parse/transcript.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_branch_title_count_and_last_text() {
        let jsonl = [
            r#"{"type":"user","gitBranch":"main","message":{}}"#,
            r#"{"type":"ai-title","aiTitle":"Fix auth bug"}"#,
            r#"{"type":"assistant","message":{"content":[{"type":"text","text":"first reply"}]}}"#,
            r#"{"type":"assistant","message":{"content":[{"type":"tool_use"},{"type":"text","text":"done, tests pass"}]}}"#,
        ]
        .join("\n");

        let t = parse_transcript(&jsonl);
        assert_eq!(t.branch.as_deref(), Some("main"));
        assert_eq!(t.title.as_deref(), Some("Fix auth bug"));
        assert_eq!(t.assistant_count, 2);
        assert_eq!(t.last_assistant_text.as_deref(), Some("done, tests pass"));
    }

    #[test]
    fn drops_head_branch_and_nulls_when_absent() {
        let t = parse_transcript(r#"{"type":"user","gitBranch":"HEAD"}"#);
        assert_eq!(t.branch, None);
        assert_eq!(t.title, None);
        assert_eq!(t.last_assistant_text, None);
    }

    #[test]
    fn assistant_without_text_content_does_not_clear_last_text() {
        let jsonl = [
            r#"{"type":"assistant","message":{"content":[{"type":"text","text":"kept"}]}}"#,
            r#"{"type":"assistant","message":{"content":[{"type":"tool_use"}]}}"#,
        ]
        .join("\n");
        let t = parse_transcript(&jsonl);
        assert_eq!(t.assistant_count, 2);
        assert_eq!(t.last_assistant_text.as_deref(), Some("kept"));
    }

    #[test]
    fn rc_file_reads_bridge_session_id_or_none() {
        assert_eq!(
            parse_rc_file(r#"{"bridgeSessionId":"session_abc123"}"#).as_deref(),
            Some("session_abc123")
        );
        assert_eq!(parse_rc_file("garbage"), None);
        assert_eq!(parse_rc_file(r#"{"other":1}"#), None);
    }
}
```

- [x] **Step 2: Run tests to verify they fail**

Run: `cargo test -p cdash-agent transcript`
Expected: FAIL — `cannot find function 'parse_transcript' in this scope`.

- [x] **Step 3: Make `parse_lines` shareable**

In `crates/agent/src/parse/history.rs`, change the signature from `fn parse_lines` to:

```rust
pub(crate) fn parse_lines<T: for<'de> Deserialize<'de>>(text: &str) -> Vec<T> {
```

- [x] **Step 4: Write the implementation**

Prepend to `crates/agent/src/parse/transcript.rs`:

```rust
use super::history::parse_lines;
use serde::Deserialize;

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Transcript {
    pub branch: Option<String>,
    pub title: Option<String>,
    pub assistant_count: u32,
    pub last_assistant_text: Option<String>,
}

#[derive(Deserialize)]
struct ContentItem {
    #[serde(rename = "type")]
    kind: Option<String>,
    text: Option<String>,
}

#[derive(Deserialize)]
struct Message {
    #[serde(default)]
    content: Vec<ContentItem>,
}

#[derive(Deserialize)]
struct TranscriptEntry {
    #[serde(rename = "type")]
    kind: Option<String>,
    #[serde(rename = "gitBranch")]
    git_branch: Option<String>,
    #[serde(rename = "aiTitle")]
    ai_title: Option<String>,
    message: Option<Message>,
}

pub fn parse_transcript(jsonl: &str) -> Transcript {
    let mut t = Transcript::default();

    for e in parse_lines::<TranscriptEntry>(jsonl) {
        if t.branch.is_none() {
            if let Some(b) = e.git_branch.as_deref() {
                if !b.is_empty() && b != "HEAD" {
                    t.branch = Some(b.to_string());
                }
            }
        }
        if t.title.is_none() && e.kind.as_deref() == Some("ai-title") {
            if let Some(title) = e.ai_title.as_deref() {
                if !title.is_empty() {
                    t.title = Some(title.to_string());
                }
            }
        }
        if e.kind.as_deref() == Some("assistant") {
            t.assistant_count += 1;
            let text = e
                .message
                .as_ref()
                .and_then(|m| m.content.iter().find(|c| c.kind.as_deref() == Some("text")))
                .and_then(|c| c.text.as_deref())
                .filter(|s| !s.is_empty());
            // Node only assigns when a text part exists, so a tool-only
            // assistant turn leaves the previous value in place.
            if let Some(txt) = text {
                t.last_assistant_text = Some(txt.to_string());
            }
        }
    }

    t
}

pub fn parse_rc_file(json: &str) -> Option<String> {
    serde_json::from_str::<serde_json::Value>(json)
        .ok()?
        .get("bridgeSessionId")?
        .as_str()
        .map(|s| s.to_string())
}
```

Add to `crates/agent/src/parse/mod.rs`:

```rust
pub mod transcript;
```

- [x] **Step 5: Run tests to verify they pass**

Run: `cargo test -p cdash-agent transcript`
Expected: PASS, 4 tests.

- [x] **Step 6: Commit**

```bash
git add crates/agent/src/parse/transcript.rs crates/agent/src/parse/mod.rs crates/agent/src/parse/history.rs
git commit -m "feat: port parseTranscript and parseRcFile to Rust"
```

---

### Task 4: Port `parseTmuxPanes`, closing the `|`-in-path defect

Ports `lib/sessions.js:53-58`. Node splits on `|` with the path **third of four** (`collect.js:221` requests `#{session_name}|#{pane_pid}|#{pane_current_path}|#{session_created}`), so any project directory containing a `|` shifts every later field. The remedy is to move the path **last** in the format string and take it as the unsplit remainder.

This changes the tmux format string, which is consumed by a later plan. The format constant is defined here so both sides use one definition.

**Files:**
- Create: `crates/agent/src/parse/tmux.rs`
- Modify: `crates/agent/src/parse/mod.rs`

**Interfaces:**
- Consumes: nothing.
- Produces:
  - `pub const PANE_FORMAT: &str` — the tmux `-F` argument; the later HTTP/collect plan MUST use this constant rather than its own literal.
  - `pub struct Pane { pub name: String, pub pid: i32, pub path: String, pub created: i64 }`
  - `pub fn parse_tmux_panes(out: &str) -> Vec<Pane>`

- [x] **Step 1: Write the failing tests**

`crates/agent/src/parse/tmux.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filters_to_cdash_prefixed_sessions() {
        let out = "cdash-backend-1531|4242|1785050000|/mnt/d/git/backend\n\
                   other|1|1785050001|/tmp\n";
        assert_eq!(
            parse_tmux_panes(out),
            vec![Pane {
                name: "cdash-backend-1531".into(),
                pid: 4242,
                path: "/mnt/d/git/backend".into(),
                created: 1785050000,
            }]
        );
    }

    #[test]
    fn a_pipe_in_the_path_does_not_shift_fields() {
        // The defect this port closes: with the path third of four, this line
        // put "/mnt/d/we" in `path` and "ird" where `created` belonged.
        let out = "cdash-x-0900|7|1785050000|/mnt/d/we|ird|dir\n";
        let panes = parse_tmux_panes(out);
        assert_eq!(panes.len(), 1);
        assert_eq!(panes[0].path, "/mnt/d/we|ird|dir");
        assert_eq!(panes[0].created, 1785050000);
        assert_eq!(panes[0].pid, 7);
    }

    #[test]
    fn format_string_puts_path_last() {
        assert!(PANE_FORMAT.ends_with("#{pane_current_path}"));
    }

    #[test]
    fn malformed_lines_are_skipped_not_panicked_on() {
        assert_eq!(parse_tmux_panes("cdash-broken|notanum\n\n"), vec![]);
    }
}
```

- [x] **Step 2: Run tests to verify they fail**

Run: `cargo test -p cdash-agent tmux`
Expected: FAIL — `cannot find function 'parse_tmux_panes' in this scope`.

- [x] **Step 3: Write the implementation**

Prepend to `crates/agent/src/parse/tmux.rs`:

```rust
/// tmux `-F` format. The path is LAST so that a `|` inside a directory name
/// cannot shift later fields: `splitn(4, '|')` leaves it as the remainder.
/// `\x1f` is not usable as a delimiter — tmux emits the four printable bytes
/// `\037` rather than the control character.
pub const PANE_FORMAT: &str =
    "#{session_name}|#{pane_pid}|#{session_created}|#{pane_current_path}";

#[derive(Debug, Clone, PartialEq)]
pub struct Pane {
    pub name: String,
    pub pid: i32,
    pub path: String,
    pub created: i64,
}

pub fn parse_tmux_panes(out: &str) -> Vec<Pane> {
    out.lines()
        .filter(|l| !l.is_empty())
        .filter_map(|l| {
            let mut it = l.splitn(4, '|');
            let name = it.next()?;
            let pid = it.next()?.parse::<i32>().ok()?;
            let created = it.next()?.parse::<i64>().ok()?;
            let path = it.next()?;
            Some(Pane {
                name: name.to_string(),
                pid,
                path: path.to_string(),
                created,
            })
        })
        .filter(|p| p.name.starts_with("cdash-"))
        .collect()
}
```

Add to `crates/agent/src/parse/mod.rs`:

```rust
pub mod tmux;
```

- [x] **Step 4: Run tests to verify they pass**

Run: `cargo test -p cdash-agent tmux`
Expected: PASS, 4 tests.

- [x] **Step 5: Commit**

```bash
git add crates/agent/src/parse/tmux.rs crates/agent/src/parse/mod.rs
git commit -m "feat: port parseTmuxPanes with the path last, closing the pipe-in-path defect"
```

---

### Task 5: Port `parseGitStatus` and `projectDirName`

Ports `lib/sessions.js:60-73`.

**Files:**
- Create: `crates/agent/src/parse/git.rs`
- Create: `crates/agent/src/parse/paths.rs`
- Modify: `crates/agent/src/parse/mod.rs`

**Interfaces:**
- Consumes: nothing.
- Produces:
  - `pub struct GitStatus { pub branch: String, pub dirty: usize, pub ahead: u32, pub behind: u32 }`
  - `pub fn parse_git_status(out: &str) -> GitStatus`
  - `pub fn project_dir_name(cwd: &str) -> String`

- [x] **Step 1: Write the failing tests**

`crates/agent/src/parse/git.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_branch_dirty_ahead_behind() {
        let out = "## main...origin/main [ahead 2, behind 1]\n M server.js\n?? new.txt\n";
        assert_eq!(
            parse_git_status(out),
            GitStatus { branch: "main".into(), dirty: 2, ahead: 2, behind: 1 }
        );
    }

    #[test]
    fn branch_with_no_upstream_has_zero_ahead_behind() {
        assert_eq!(
            parse_git_status("## feature-x\n"),
            GitStatus { branch: "feature-x".into(), dirty: 0, ahead: 0, behind: 0 }
        );
    }

    #[test]
    fn empty_output_does_not_panic() {
        assert_eq!(
            parse_git_status(""),
            GitStatus { branch: "".into(), dirty: 0, ahead: 0, behind: 0 }
        );
    }
}
```

`crates/agent/src/parse/paths.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn munges_non_alphanumerics_to_dashes() {
        assert_eq!(project_dir_name("/mnt/d/git/backend"), "-mnt-d-git-backend");
    }

    #[test]
    fn dots_and_underscores_are_munged_too() {
        assert_eq!(project_dir_name("/a/b_c.d"), "-a-b-c-d");
    }
}
```

- [x] **Step 2: Run tests to verify they fail**

Run: `cargo test -p cdash-agent -- git paths`
Expected: FAIL — `cannot find function 'parse_git_status' in this scope`.

- [x] **Step 3: Write the implementations**

Prepend to `crates/agent/src/parse/git.rs`:

```rust
use regex::Regex;
use std::sync::OnceLock;

#[derive(Debug, Clone, PartialEq)]
pub struct GitStatus {
    pub branch: String,
    pub dirty: usize,
    pub ahead: u32,
    pub behind: u32,
}

fn ahead_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"ahead (\d+)").unwrap())
}

fn behind_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"behind (\d+)").unwrap())
}

fn captured(re: &Regex, hay: &str) -> u32 {
    re.captures(hay)
        .and_then(|c| c.get(1))
        .and_then(|m| m.as_str().parse().ok())
        .unwrap_or(0)
}

pub fn parse_git_status(out: &str) -> GitStatus {
    let lines: Vec<&str> = out.lines().filter(|l| !l.is_empty()).collect();
    let head = lines.first().copied().unwrap_or("");
    let branch = head
        .strip_prefix("## ")
        .unwrap_or(head)
        .split("...")
        .next()
        .unwrap_or("")
        .trim()
        .to_string();

    GitStatus {
        branch,
        dirty: lines.len().saturating_sub(1),
        ahead: captured(ahead_re(), head),
        behind: captured(behind_re(), head),
    }
}
```

Prepend to `crates/agent/src/parse/paths.rs`:

```rust
pub fn project_dir_name(cwd: &str) -> String {
    cwd.chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect()
}
```

Add to `crates/agent/src/parse/mod.rs`:

```rust
pub mod git;
pub mod paths;
```

- [x] **Step 4: Run tests to verify they pass**

Run: `cargo test -p cdash-agent`
Expected: PASS, all tests including the 5 new ones.

- [x] **Step 5: Commit**

```bash
git add crates/agent/src/parse/git.rs crates/agent/src/parse/paths.rs crates/agent/src/parse/mod.rs
git commit -m "feat: port parseGitStatus and projectDirName to Rust"
```

---

### Task 6: Port the process-tree walk

Ports `lib/stats.js:3-23`. The walk survives as logic; its input becomes typed rows instead of parsed `ps` text. Keeping it a pure function over a row slice is what lets it be tested without `sysinfo`.

**Files:**
- Create: `crates/agent/src/host/mod.rs`
- Create: `crates/agent/src/host/proc.rs`
- Modify: `crates/agent/src/lib.rs`

**Interfaces:**
- Consumes: nothing.
- Produces:
  - `pub struct ProcRow { pub pid: i32, pub ppid: i32, pub cpu: f32, pub rss_kb: u64 }`
  - `pub struct TreeUsage { pub cpu: f32, pub rss_kb: u64 }`
  - `pub fn proc_tree_usage(rows: &[ProcRow], root_pid: i32) -> TreeUsage`

- [x] **Step 1: Write the failing tests**

`crates/agent/src/host/proc.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn rows() -> Vec<ProcRow> {
        vec![
            ProcRow { pid: 1, ppid: 0, cpu: 0.0, rss_kb: 1000 },
            ProcRow { pid: 100, ppid: 1, cpu: 5.0, rss_kb: 50000 },   // root
            ProcRow { pid: 200, ppid: 100, cpu: 10.0, rss_kb: 20000 }, // child
            ProcRow { pid: 300, ppid: 200, cpu: 1.5, rss_kb: 4000 },   // grandchild
            ProcRow { pid: 400, ppid: 1, cpu: 9.9, rss_kb: 99999 },    // unrelated
        ]
    }

    #[test]
    fn sums_the_tree_rooted_at_pid() {
        let u = proc_tree_usage(&rows(), 100);
        assert_eq!(u.cpu, 16.5);
        assert_eq!(u.rss_kb, 74000);
    }

    #[test]
    fn unknown_pid_yields_zeros() {
        let u = proc_tree_usage(&rows(), 999);
        assert_eq!(u.cpu, 0.0);
        assert_eq!(u.rss_kb, 0);
    }

    #[test]
    fn a_parent_cycle_terminates() {
        // Defensive: /proc can race such that ppid chains form a loop.
        let cyclic = vec![
            ProcRow { pid: 10, ppid: 11, cpu: 1.0, rss_kb: 10 },
            ProcRow { pid: 11, ppid: 10, cpu: 2.0, rss_kb: 20 },
        ];
        let u = proc_tree_usage(&cyclic, 10);
        assert_eq!(u.rss_kb, 30);
    }
}
```

- [x] **Step 2: Run tests to verify they fail**

Run: `cargo test -p cdash-agent proc`
Expected: FAIL — `cannot find function 'proc_tree_usage' in this scope`.

- [x] **Step 3: Write the implementation**

Prepend to `crates/agent/src/host/proc.rs`:

```rust
use std::collections::{HashMap, HashSet};

#[derive(Debug, Clone, PartialEq)]
pub struct ProcRow {
    pub pid: i32,
    pub ppid: i32,
    pub cpu: f32,
    pub rss_kb: u64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct TreeUsage {
    pub cpu: f32,
    pub rss_kb: u64,
}

pub fn proc_tree_usage(rows: &[ProcRow], root_pid: i32) -> TreeUsage {
    let mut children: HashMap<i32, Vec<i32>> = HashMap::new();
    for r in rows {
        children.entry(r.ppid).or_default().push(r.pid);
    }
    let by_pid: HashMap<i32, &ProcRow> = rows.iter().map(|r| (r.pid, r)).collect();

    let mut cpu = 0.0f32;
    let mut rss_kb = 0u64;
    let mut seen: HashSet<i32> = HashSet::new();
    let mut stack: Vec<i32> = if by_pid.contains_key(&root_pid) {
        vec![root_pid]
    } else {
        vec![]
    };

    while let Some(pid) = stack.pop() {
        // The Node version has no cycle guard; a ppid loop would hang it.
        if !seen.insert(pid) {
            continue;
        }
        if let Some(r) = by_pid.get(&pid) {
            cpu += r.cpu;
            rss_kb += r.rss_kb;
        }
        if let Some(kids) = children.get(&pid) {
            stack.extend(kids.iter().copied());
        }
    }

    // Node: Math.round(cpu * 10) / 10
    TreeUsage { cpu: (cpu * 10.0).round() / 10.0, rss_kb }
}
```

`crates/agent/src/host/mod.rs`:

```rust
pub mod proc;
```

Add to `crates/agent/src/lib.rs`:

```rust
pub mod host;
```

- [x] **Step 4: Run tests to verify they pass**

Run: `cargo test -p cdash-agent proc`
Expected: PASS, 3 tests.

- [x] **Step 5: Commit**

```bash
git add crates/agent/src/host/mod.rs crates/agent/src/host/proc.rs crates/agent/src/lib.rs
git commit -m "feat: port the process-tree walk, with a cycle guard Node lacked"
```

---

### Task 7: The log buffer

The subprocess helper in Task 9 needs a place to write its log-once failures, and `/api/logs` serves it in a later plan. Ports `lib/collect.js:21-27`: a 200-entry ring with `HH:MM:SS` prefixes.

**Files:**
- Create: `crates/agent/src/host/log.rs`
- Modify: `crates/agent/src/host/mod.rs`

**Interfaces:**
- Consumes: nothing.
- Produces:
  - `pub struct LogBuffer` with `pub fn new() -> Self`, `pub fn push(&self, line: impl AsRef<str>)`, `pub fn lines(&self) -> Vec<String>`
  - `LogBuffer` is `Send + Sync` and cheap to clone-by-reference (`Arc` it at the call site).

- [x] **Step 1: Write the failing tests**

`crates/agent/src/host/log.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_at_most_200_entries_dropping_oldest() {
        let buf = LogBuffer::new();
        for i in 0..250 {
            buf.push(format!("line {i}"));
        }
        let lines = buf.lines();
        assert_eq!(lines.len(), 200);
        assert!(lines[0].ends_with("line 50"));
        assert!(lines[199].ends_with("line 249"));
    }

    #[test]
    fn each_line_carries_an_hhmmss_prefix() {
        let buf = LogBuffer::new();
        buf.push("hello");
        let line = buf.lines().remove(0);
        assert_eq!(line.len(), "00:00:00 hello".len());
        assert_eq!(&line[2..3], ":");
        assert_eq!(&line[5..6], ":");
        assert!(line.ends_with(" hello"));
    }
}
```

- [x] **Step 2: Run tests to verify they fail**

Run: `cargo test -p cdash-agent log`
Expected: FAIL — `cannot find type 'LogBuffer' in this scope`.

- [x] **Step 3: Add the time-formatting dependency**

In `crates/agent/Cargo.toml`, add under `[dependencies]`:

```toml
time = { version = "0.3", features = ["formatting", "local-offset"] }
```

- [x] **Step 4: Write the implementation**

Prepend to `crates/agent/src/host/log.rs`:

```rust
use std::collections::VecDeque;
use std::sync::Mutex;
use time::OffsetDateTime;

const MAX_LINES: usize = 200;

/// A 200-entry ring of `HH:MM:SS`-prefixed lines, mirroring
/// `logBuffer` in `lib/collect.js:21-27`. Also echoes to stderr.
pub struct LogBuffer {
    lines: Mutex<VecDeque<String>>,
}

impl Default for LogBuffer {
    fn default() -> Self {
        Self::new()
    }
}

impl LogBuffer {
    pub fn new() -> Self {
        Self { lines: Mutex::new(VecDeque::with_capacity(MAX_LINES)) }
    }

    pub fn push(&self, line: impl AsRef<str>) {
        let now = OffsetDateTime::now_local().unwrap_or_else(|_| OffsetDateTime::now_utc());
        let stamped = format!(
            "{:02}:{:02}:{:02} {}",
            now.hour(),
            now.minute(),
            now.second(),
            line.as_ref()
        );
        eprintln!("{stamped}");
        // A poisoned mutex must not take down logging; recover the guard.
        let mut guard = self.lines.lock().unwrap_or_else(|e| e.into_inner());
        if guard.len() == MAX_LINES {
            guard.pop_front();
        }
        guard.push_back(stamped);
    }

    pub fn lines(&self) -> Vec<String> {
        let guard = self.lines.lock().unwrap_or_else(|e| e.into_inner());
        guard.iter().cloned().collect()
    }
}
```

Add to `crates/agent/src/host/mod.rs`:

```rust
pub mod log;
```

- [x] **Step 5: Run tests to verify they pass**

Run: `cargo test -p cdash-agent log`
Expected: PASS, 2 tests.

- [x] **Step 6: Commit**

```bash
git add crates/agent/src/host/log.rs crates/agent/src/host/mod.rs crates/agent/Cargo.toml
git commit -m "feat: port the 200-entry log ring buffer"
```

---

### Task 8: PATH resolution

Implements the spec's PATH probe. The probe runs `$SHELL -l -c 'echo $PATH'`, is time-boxed at 2000 ms, and **never gates startup** — on any failure the agent proceeds with the inherited PATH plus the known-location backstop.

This task uses `tokio::process::Command` directly and therefore carries the `#[allow]`. Task 9 moves all other callers behind the helper.

**Files:**
- Create: `crates/agent/src/host/path.rs`
- Modify: `crates/agent/src/host/mod.rs`

**Interfaces:**
- Consumes: `LogBuffer` from Task 7.
- Produces:
  - `pub async fn probe_path(log: &LogBuffer) -> String` — the resolved PATH value
  - `pub fn compose_path(probed: Option<&str>, inherited: &str) -> String` — pure, testable

- [x] **Step 1: Write the failing tests**

`crates/agent/src/host/path.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn probed_path_comes_first_then_known_locations_then_inherited() {
        let out = compose_path(Some("/home/u/.local/bin:/usr/bin"), "/usr/bin:/bin");
        assert_eq!(
            out,
            "/home/u/.local/bin:/usr/bin:/opt/homebrew/bin:/usr/local/bin:/bin"
        );
    }

    #[test]
    fn a_failed_probe_still_gets_the_known_location_backstop() {
        // This is the whole point of the backstop: on a macOS GUI launch the
        // inherited PATH is exactly the minimal one the probe existed to fix.
        let out = compose_path(None, "/usr/bin:/bin");
        assert_eq!(out, "/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin");
    }

    #[test]
    fn duplicates_are_removed_keeping_first_occurrence() {
        let out = compose_path(Some("/usr/bin"), "/usr/bin:/bin:/usr/bin");
        assert_eq!(out, "/usr/bin:/opt/homebrew/bin:/usr/local/bin:/bin");
    }

    #[test]
    fn empty_segments_are_dropped() {
        let out = compose_path(Some(""), "/bin::/usr/bin");
        assert_eq!(out, "/opt/homebrew/bin:/usr/local/bin:/bin:/usr/bin");
    }

    #[tokio::test]
    async fn probe_never_panics_and_always_returns_a_usable_path() {
        let log = LogBuffer::new();
        let p = probe_path(&log).await;
        assert!(!p.is_empty());
        assert!(p.contains("/usr/local/bin"));
    }
}
```

- [x] **Step 2: Run tests to verify they fail**

Run: `cargo test -p cdash-agent path`
Expected: FAIL — `cannot find function 'compose_path' in this scope`.

- [x] **Step 3: Write the implementation**

Prepend to `crates/agent/src/host/path.rs`:

```rust
use super::log::LogBuffer;
use std::time::Duration;

const PROBE_TIMEOUT: Duration = Duration::from_millis(2000);
const KNOWN_LOCATIONS: &[&str] = &["/opt/homebrew/bin", "/usr/local/bin"];

/// Compose the child PATH: probed value first (so a user's own ordering wins),
/// then the known-location backstop, then whatever we inherited. Deduped,
/// first occurrence kept, empty segments dropped.
pub fn compose_path(probed: Option<&str>, inherited: &str) -> String {
    let mut out: Vec<&str> = Vec::new();
    let sources = [probed.unwrap_or(""), &KNOWN_LOCATIONS.join(":"), inherited];
    for src in sources {
        for seg in src.split(':') {
            if !seg.is_empty() && !out.contains(&seg) {
                out.push(seg);
            }
        }
    }
    out.join(":")
}

/// Probe the user's login-shell PATH. Never returns an error and never gates
/// startup: on timeout, spawn failure, or non-zero exit, fall back to the
/// inherited PATH and record why.
pub async fn probe_path(log: &LogBuffer) -> String {
    let inherited = std::env::var("PATH").unwrap_or_default();
    let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".to_string());

    // The one deliberate direct use outside host/cmd.rs: the helper in
    // crates/agent/src/host/cmd.rs depends on the value this function
    // produces, so it cannot itself be routed through the helper.
    #[allow(clippy::disallowed_types)]
    let spawned = tokio::process::Command::new(&shell)
        .args(["-l", "-c", "echo $PATH"])
        .stdin(std::process::Stdio::null())
        .kill_on_drop(true)
        .output();

    let probed = match tokio::time::timeout(PROBE_TIMEOUT, spawned).await {
        Err(_) => {
            log.push("PATH probe failed (timed out after 2000ms); using inherited PATH");
            None
        }
        Ok(Err(e)) => {
            log.push(format!("PATH probe failed ({e}); using inherited PATH"));
            None
        }
        Ok(Ok(out)) if !out.status.success() => {
            log.push(format!(
                "PATH probe failed (exit {}); using inherited PATH",
                out.status.code().unwrap_or(-1)
            ));
            None
        }
        Ok(Ok(out)) => Some(String::from_utf8_lossy(&out.stdout).trim().to_string()),
    };

    compose_path(probed.as_deref(), &inherited)
}
```

Add to `crates/agent/src/host/mod.rs`:

```rust
pub mod path;
```

- [x] **Step 4: Run tests to verify they pass**

Run: `cargo test -p cdash-agent path`
Expected: PASS, 5 tests.

- [x] **Step 5: Verify the timeout path by hand**

Run:

```bash
SHELL=/bin/sh cargo test -p cdash-agent path -- --nocapture
```

Expected: the async test passes. If a `PATH probe failed` line appears on stderr, that is the fallback working, not a failure.

- [x] **Step 6: Commit**

```bash
git add crates/agent/src/host/path.rs crates/agent/src/host/mod.rs
git commit -m "feat: PATH probe with a 2000ms time-box and a non-gating fallback"
```

---

### Task 9: The subprocess helper

The single place a subprocess may be constructed. Carries the resolved PATH, the 5-second time-box, and the log-once-per-key failure rule — replacing `sh()` (`lib/collect.js:12-19`) and closing its dedupe-key defect, where `` `${cmd} ${args[0]}` `` collapsed **every** `git status` failure across every repository to the single key `"git -C"`.

**Files:**
- Create: `crates/agent/src/host/cmd.rs`
- Modify: `crates/agent/src/host/mod.rs`

**Interfaces:**
- Consumes: `LogBuffer` (Task 7).
- Produces:
  - `pub struct Runner { .. }` with `pub fn new(path: String, log: Arc<LogBuffer>) -> Self`
  - `pub async fn run(&self, program: &str, args: &[&str], key: &str) -> String` — stdout on success, empty string on any failure, logging once per `key`
  - `pub async fn run_with_timeout(&self, program: &str, args: &[&str], key: &str, timeout: Duration) -> String`

- [x] **Step 1: Write the failing tests**

`crates/agent/src/host/cmd.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn runner() -> (Runner, Arc<LogBuffer>) {
        let log = Arc::new(LogBuffer::new());
        let path = std::env::var("PATH").unwrap_or_default();
        (Runner::new(path, log.clone()), log)
    }

    #[tokio::test]
    async fn returns_stdout_on_success() {
        let (r, _) = runner();
        let out = r.run("echo", &["hello"], "echo").await;
        assert_eq!(out.trim(), "hello");
    }

    #[tokio::test]
    async fn returns_empty_string_on_failure() {
        let (r, _) = runner();
        assert_eq!(r.run("false", &[], "false").await, "");
    }

    #[tokio::test]
    async fn logs_once_per_key_not_once_per_failure() {
        let (r, log) = runner();
        for _ in 0..3 {
            r.run("false", &[], "git /repo-a").await;
        }
        assert_eq!(log.lines().len(), 1);
    }

    #[tokio::test]
    async fn distinct_keys_log_separately() {
        // The defect this closes: under the old `cmd + args[0]` key both of
        // these collapsed to "git -C" and the second was silenced.
        let (r, log) = runner();
        r.run("false", &["-C", "/repo-a", "status"], "git /repo-a").await;
        r.run("false", &["-C", "/repo-b", "status"], "git /repo-b").await;
        assert_eq!(log.lines().len(), 2);
    }

    #[tokio::test]
    async fn a_hung_child_is_killed_at_the_timeout() {
        let (r, log) = runner();
        let started = std::time::Instant::now();
        let out = r
            .run_with_timeout("sleep", &["30"], "sleep", Duration::from_millis(300))
            .await;
        assert_eq!(out, "");
        assert!(started.elapsed() < Duration::from_secs(5));
        assert!(log.lines()[0].contains("timed out"));
    }

    #[tokio::test]
    async fn the_resolved_path_reaches_the_child() {
        let log = Arc::new(LogBuffer::new());
        let r = Runner::new("/nonexistent-dir-for-test".to_string(), log);
        // `echo` is not on the supplied PATH, so the spawn must fail.
        assert_eq!(r.run("echo", &["hi"], "echo").await, "");
    }
}
```

- [x] **Step 2: Run tests to verify they fail**

Run: `cargo test -p cdash-agent cmd`
Expected: FAIL — `cannot find type 'Runner' in this scope`.

- [x] **Step 3: Write the implementation**

Prepend to `crates/agent/src/host/cmd.rs`:

```rust
use super::log::LogBuffer;
use std::collections::HashSet;
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// Default subprocess deadline. This exists because `git status` on a 9P mount
/// once took over 60 seconds and stalled every 4-second poll. Do not raise it
/// without measuring; do not remove it.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(5);

/// The only sanctioned way to run a subprocess. `clippy.toml` forbids
/// `std::process::Command` and `tokio::process::Command` everywhere else, and
/// `-D clippy::disallowed_types` is a required CI gate, because this helper is
/// the sole enforcement of the time-box above.
pub struct Runner {
    path: String,
    log: Arc<LogBuffer>,
    failed: Mutex<HashSet<String>>,
}

impl Runner {
    pub fn new(path: String, log: Arc<LogBuffer>) -> Self {
        Self { path, log, failed: Mutex::new(HashSet::new()) }
    }

    pub async fn run(&self, program: &str, args: &[&str], key: &str) -> String {
        self.run_with_timeout(program, args, key, DEFAULT_TIMEOUT).await
    }

    pub async fn run_with_timeout(
        &self,
        program: &str,
        args: &[&str],
        key: &str,
        timeout: Duration,
    ) -> String {
        #[allow(clippy::disallowed_types)]
        let fut = tokio::process::Command::new(program)
            .args(args)
            .env("PATH", &self.path)
            .stdin(std::process::Stdio::null())
            .kill_on_drop(true) // the timeout must actually kill the child
            .output();

        match tokio::time::timeout(timeout, fut).await {
            Err(_) => {
                self.log_once(key, &format!("timed out after {}ms", timeout.as_millis()));
                String::new()
            }
            Ok(Err(e)) => {
                self.log_once(key, &e.to_string());
                String::new()
            }
            Ok(Ok(out)) if !out.status.success() => {
                self.log_once(
                    key,
                    &format!("exit {}", out.status.code().unwrap_or(-1)),
                );
                String::new()
            }
            Ok(Ok(out)) => String::from_utf8_lossy(&out.stdout).into_owned(),
        }
    }

    /// Log a given failing key once per process lifetime. The KEY IS EXPLICIT:
    /// deriving it from `program + args[0]` is what made every `git status`
    /// failure across every repository collapse into one silenced entry.
    fn log_once(&self, key: &str, reason: &str) {
        let mut failed = self.failed.lock().unwrap_or_else(|e| e.into_inner());
        if failed.insert(key.to_string()) {
            self.log.push(format!("sh failed: {key}: {reason}"));
        }
    }
}
```

Add to `crates/agent/src/host/mod.rs`:

```rust
pub mod cmd;
```

- [x] **Step 4: Run tests to verify they pass**

Run: `cargo test -p cdash-agent cmd`
Expected: PASS, 6 tests.

- [x] **Step 5: Run the lint gate**

Run: `cargo clippy --all-targets -- -D warnings -D clippy::disallowed_types`
Expected: exit 0. Only `host/cmd.rs` and `host/path.rs` carry `#[allow(clippy::disallowed_types)]`.

- [x] **Step 6: Commit**

```bash
git add crates/agent/src/host/cmd.rs crates/agent/src/host/mod.rs
git commit -m "feat: subprocess helper with explicit log-once keys, closing the git -C collapse"
```

---

### Task 10: Missing-binary detection

A pure function over a PATH string. Feeds `/api/hostinfo`'s `missing: [...]` in a later plan, which drives the macOS `tmux` setup screen. `ps` and `df` are deliberately **not** in the list — they are no longer invoked.

**Files:**
- Create: `crates/agent/src/host/probe.rs`
- Modify: `crates/agent/src/host/mod.rs`

**Interfaces:**
- Consumes: nothing.
- Produces:
  - `pub const REQUIRED_BINARIES: &[&str]` — exactly `["tmux", "claude", "git"]`
  - `pub fn missing_binaries(path: &str) -> Vec<String>`

- [x] **Step 1: Write the failing tests**

`crates/agent/src/host/probe.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::os::unix::fs::PermissionsExt;

    fn tempdir(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("cdash-probe-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn make_executable(dir: &std::path::Path, name: &str) {
        let p = dir.join(name);
        fs::write(&p, "#!/bin/sh\n").unwrap();
        fs::set_permissions(&p, fs::Permissions::from_mode(0o755)).unwrap();
    }

    #[test]
    fn reports_all_required_binaries_when_path_is_empty() {
        let missing = missing_binaries("");
        assert_eq!(missing, vec!["tmux", "claude", "git"]);
    }

    #[test]
    fn a_present_executable_is_not_reported_missing() {
        let dir = tempdir("present");
        make_executable(&dir, "tmux");
        let missing = missing_binaries(dir.to_str().unwrap());
        assert!(!missing.contains(&"tmux".to_string()));
        assert!(missing.contains(&"git".to_string()));
    }

    #[test]
    fn a_non_executable_file_still_counts_as_missing() {
        let dir = tempdir("nonexec");
        fs::write(dir.join("git"), "not executable").unwrap();
        assert!(missing_binaries(dir.to_str().unwrap()).contains(&"git".to_string()));
    }

    #[test]
    fn ps_and_df_are_not_required() {
        assert!(!REQUIRED_BINARIES.contains(&"ps"));
        assert!(!REQUIRED_BINARIES.contains(&"df"));
    }
}
```

- [x] **Step 2: Run tests to verify they fail**

Run: `cargo test -p cdash-agent probe`
Expected: FAIL — `cannot find function 'missing_binaries' in this scope`.

- [x] **Step 3: Write the implementation**

Prepend to `crates/agent/src/host/probe.rs`:

```rust
use std::path::Path;

/// `ps` and `df` are absent by design: the Rust agent uses `sysinfo` and
/// `statvfs` and never shells out to them.
pub const REQUIRED_BINARIES: &[&str] = &["tmux", "claude", "git"];

fn is_executable(p: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::metadata(p)
            .map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
            .unwrap_or(false)
    }
    #[cfg(not(unix))]
    {
        p.is_file()
    }
}

pub fn missing_binaries(path: &str) -> Vec<String> {
    REQUIRED_BINARIES
        .iter()
        .filter(|bin| {
            !path
                .split(':')
                .filter(|d| !d.is_empty())
                .any(|dir| is_executable(&Path::new(dir).join(bin)))
        })
        .map(|b| b.to_string())
        .collect()
}
```

Add to `crates/agent/src/host/mod.rs`:

```rust
pub mod probe;
```

- [x] **Step 4: Run tests to verify they pass**

Run: `cargo test -p cdash-agent probe`
Expected: PASS, 4 tests.

- [x] **Step 5: Commit**

```bash
git add crates/agent/src/host/probe.rs crates/agent/src/host/mod.rs
git commit -m "feat: missing-binary detection over the resolved PATH"
```

---

### Task 11: Disk stats via `statvfs`

Replaces `df -k --output=target,avail,size` (`lib/collect.js:223`) and `parseDf` (`lib/stats.js:25-30`). The caller names the mount, so there is no mount column to parse — which is why the space-in-path defect cannot recur.

Field names and units must match Node's output exactly: `{ mount, freeKb, totalKb }`.

**Files:**
- Create: `crates/agent/src/host/disk.rs`
- Modify: `crates/agent/src/host/mod.rs`

**Interfaces:**
- Consumes: nothing.
- Produces:
  - `pub struct DiskUsage { pub mount: String, pub free_kb: u64, pub total_kb: u64 }` (serialized as `mount`, `freeKb`, `totalKb`)
  - `pub fn disk_usage(mount: &str) -> Option<DiskUsage>`

- [x] **Step 1: Write the failing tests**

`crates/agent/src/host/disk.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn root_reports_plausible_totals() {
        let u = disk_usage("/").expect("/ must be statvfs-able");
        assert_eq!(u.mount, "/");
        assert!(u.total_kb > 0);
        assert!(u.free_kb <= u.total_kb);
    }

    #[test]
    fn a_mount_path_containing_a_space_is_not_mangled() {
        // The Node defect: `df` output was split on whitespace, so this path
        // truncated to "/tmp/with" and shifted totalKb into freeKb.
        let dir = std::env::temp_dir().join("cdash disk test");
        std::fs::create_dir_all(&dir).unwrap();
        let u = disk_usage(dir.to_str().unwrap()).expect("temp dir must be statvfs-able");
        assert_eq!(u.mount, dir.to_str().unwrap());
        assert!(u.total_kb > 0);
    }

    #[test]
    fn a_nonexistent_path_yields_none_rather_than_panicking() {
        assert!(disk_usage("/definitely/not/a/real/mount/point").is_none());
    }

    #[test]
    fn serializes_with_nodes_field_names() {
        let u = DiskUsage { mount: "/".into(), free_kb: 1, total_kb: 2 };
        let j = serde_json::to_string(&u).unwrap();
        assert!(j.contains("\"freeKb\":1"));
        assert!(j.contains("\"totalKb\":2"));
    }
}
```

- [x] **Step 2: Run tests to verify they fail**

Run: `cargo test -p cdash-agent disk`
Expected: FAIL — `cannot find function 'disk_usage' in this scope`.

- [x] **Step 3: Write the implementation**

Prepend to `crates/agent/src/host/disk.rs`:

```rust
use serde::Serialize;

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct DiskUsage {
    pub mount: String,
    #[serde(rename = "freeKb")]
    pub free_kb: u64,
    #[serde(rename = "totalKb")]
    pub total_kb: u64,
}

/// Disk usage for one named mount. The caller supplies the label, so no mount
/// column is parsed and a path containing a space cannot shift the numbers.
pub fn disk_usage(mount: &str) -> Option<DiskUsage> {
    let stat = rustix::fs::statvfs(mount).ok()?;
    let block = stat.f_frsize.max(1);
    // Node reported 1K blocks (`df -k`); keep the same unit.
    let to_kb = |blocks: u64| blocks.saturating_mul(block) / 1024;
    Some(DiskUsage {
        mount: mount.to_string(),
        free_kb: to_kb(stat.f_bavail),
        total_kb: to_kb(stat.f_blocks),
    })
}
```

Add to `crates/agent/src/host/mod.rs`:

```rust
pub mod disk;
```

- [x] **Step 4: Run tests to verify they pass**

Run: `cargo test -p cdash-agent disk`
Expected: PASS, 4 tests.

If `f_frsize` or `f_bavail` do not resolve, check the `rustix::fs::StatVfs` field names for the pinned version and adjust — the semantics wanted are fragment size, blocks available to a non-privileged user, and total blocks.

- [x] **Step 5: Commit**

```bash
git add crates/agent/src/host/disk.rs crates/agent/src/host/mod.rs
git commit -m "feat: disk stats via statvfs, retiring df column parsing"
```

---

### Task 12: Process sampling with the 200 ms CPU rule

`sysinfo`'s `cpu_usage()` is a **sampled** quantity: it returns `0.0` unless the process has been refreshed twice at least `MINIMUM_CPU_UPDATE_INTERVAL` (200 ms) apart, and a refresh interval *shorter* than 200 ms returns a silently **deflated** number rather than zero. A deflated number is worse than a zero because it is plausible.

`collectSessions` is request-driven — there is no server-side timer — so a long-lived `System` is held and CPU is reported as `null` when the last sample is too recent to be trustworthy. The threshold governs **when to re-sample, not what to serve**: a fresh sample taken moments ago is served, not suppressed.

**Files:**
- Create: `crates/agent/src/host/sample.rs`
- Modify: `crates/agent/src/host/mod.rs`

**Interfaces:**
- Consumes: `ProcRow`, `TreeUsage`, `proc_tree_usage` (Task 6).
- Produces:
  - `pub struct Sampler` with `pub fn new() -> Self` and `pub fn sample(&mut self) -> Vec<ProcRow>`
  - `pub struct SampledUsage { pub cpu: Option<f32>, pub rss_kb: u64, pub cpu_sample_age_ms: u128 }`
  - `pub fn tree_usage(&mut self, root_pid: i32) -> SampledUsage`
  - `MIN_CPU_INTERVAL: Duration` = 200 ms

- [x] **Step 1: Write the failing tests**

`crates/agent/src/host/sample.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_sample_reports_cpu_as_none_not_zero() {
        // A single refresh cannot produce a CPU number. Reporting 0.0 would be
        // a plausible lie; None is the honest answer.
        let mut s = Sampler::new();
        let u = s.tree_usage(std::process::id() as i32);
        assert_eq!(u.cpu, None);
        assert!(u.rss_kb > 0, "RSS is available on the first refresh");
    }

    #[test]
    fn a_second_sample_after_the_interval_produces_a_cpu_number() {
        let mut s = Sampler::new();
        let _ = s.tree_usage(std::process::id() as i32);
        std::thread::sleep(MIN_CPU_INTERVAL + Duration::from_millis(50));
        let u = s.tree_usage(std::process::id() as i32);
        assert!(u.cpu.is_some(), "cpu must be Some after a >=200ms gap");
        assert!(u.cpu.unwrap() >= 0.0);
    }

    #[test]
    fn a_sub_interval_call_serves_the_last_good_sample_rather_than_resampling() {
        // The threshold governs when to RE-SAMPLE, not what to serve. An
        // imperative poll moments after a good sample must not render a dash.
        let mut s = Sampler::new();
        let _ = s.tree_usage(std::process::id() as i32);
        std::thread::sleep(MIN_CPU_INTERVAL + Duration::from_millis(50));
        let first = s.tree_usage(std::process::id() as i32);
        let second = s.tree_usage(std::process::id() as i32); // immediate
        assert_eq!(first.cpu, second.cpu);
        assert!(second.cpu_sample_age_ms < MIN_CPU_INTERVAL.as_millis());
    }

    #[test]
    fn unknown_pid_yields_zero_rss() {
        let mut s = Sampler::new();
        let u = s.tree_usage(-1);
        assert_eq!(u.rss_kb, 0);
    }
}
```

- [x] **Step 2: Run tests to verify they fail**

Run: `cargo test -p cdash-agent sample`
Expected: FAIL — `cannot find type 'Sampler' in this scope`.

- [x] **Step 3: Write the implementation**

Prepend to `crates/agent/src/host/sample.rs`:

```rust
use super::proc::{proc_tree_usage, ProcRow};
use std::time::{Duration, Instant};
use sysinfo::{ProcessRefreshKind, ProcessesToUpdate, System};

/// `sysinfo::MINIMUM_CPU_UPDATE_INTERVAL`. Two refreshes closer together than
/// this return a deflated CPU number, not an error and not a zero.
pub const MIN_CPU_INTERVAL: Duration = Duration::from_millis(200);

#[derive(Debug, Clone, PartialEq)]
pub struct SampledUsage {
    /// `None` until two refreshes at least `MIN_CPU_INTERVAL` apart have run.
    pub cpu: Option<f32>,
    pub rss_kb: u64,
    pub cpu_sample_age_ms: u128,
}

/// Holds a long-lived `System` across requests. `collectSessions` is
/// request-driven, so without this every call would be a first refresh and
/// every CPU number would be zero.
pub struct Sampler {
    sys: System,
    last_refresh: Option<Instant>,
    cpu_valid: bool,
}

impl Default for Sampler {
    fn default() -> Self {
        Self::new()
    }
}

impl Sampler {
    pub fn new() -> Self {
        Self { sys: System::new(), last_refresh: None, cpu_valid: false }
    }

    /// Refresh only when the previous sample is at least `MIN_CPU_INTERVAL`
    /// old. A sub-interval refresh would deflate the CPU figure.
    fn refresh_if_due(&mut self) {
        let due = match self.last_refresh {
            None => true,
            Some(t) => t.elapsed() >= MIN_CPU_INTERVAL,
        };
        if !due {
            return;
        }
        self.sys.refresh_processes_specifics(
            ProcessesToUpdate::All,
            true,
            ProcessRefreshKind::nothing().with_cpu().with_memory(),
        );
        // The first refresh establishes a baseline; only from the second
        // onward is cpu_usage() meaningful.
        if self.last_refresh.is_some() {
            self.cpu_valid = true;
        }
        self.last_refresh = Some(Instant::now());
    }

    pub fn sample(&mut self) -> Vec<ProcRow> {
        self.refresh_if_due();
        self.sys
            .processes()
            .values()
            .map(|p| ProcRow {
                pid: p.pid().as_u32() as i32,
                ppid: p.parent().map(|x| x.as_u32() as i32).unwrap_or(0),
                cpu: p.cpu_usage(),
                rss_kb: p.memory() / 1024, // sysinfo reports bytes; Node reported KiB
            })
            .collect()
    }

    pub fn tree_usage(&mut self, root_pid: i32) -> SampledUsage {
        let rows = self.sample();
        let usage = proc_tree_usage(&rows, root_pid);
        SampledUsage {
            cpu: if self.cpu_valid { Some(usage.cpu) } else { None },
            rss_kb: usage.rss_kb,
            cpu_sample_age_ms: self
                .last_refresh
                .map(|t| t.elapsed().as_millis())
                .unwrap_or(0),
        }
    }
}
```

Add to `crates/agent/src/host/mod.rs`:

```rust
pub mod sample;
```

- [x] **Step 4: Run tests to verify they pass**

Run: `cargo test -p cdash-agent sample -- --test-threads=1`
Expected: PASS, 4 tests. Single-threaded because the timing assertions are sensitive to scheduler noise under parallel test execution.

- [x] **Step 5: Verify the deflation claim rather than trusting it**

Create `crates/agent/examples/cpu_sampling.rs`:

```rust
use cdash_agent::host::sample::{Sampler, MIN_CPU_INTERVAL};
use std::time::Duration;

fn main() {
    let pid = std::process::id() as i32;
    let mut s = Sampler::new();
    println!("first:      {:?}", s.tree_usage(pid).cpu);
    std::thread::sleep(Duration::from_millis(50));
    println!("after 50ms: {:?}", s.tree_usage(pid).cpu);
    std::thread::sleep(MIN_CPU_INTERVAL + Duration::from_millis(50));
    println!("after 250ms:{:?}", s.tree_usage(pid).cpu);
}
```

Run: `cargo run -p cdash-agent --example cpu_sampling`
Expected: `first: None`, `after 50ms: None` (no re-sample was due), `after 250ms: Some(..)`.

Record the observed values in the commit message. If `after 250ms` is `None`, the refresh predicate is wrong and Task 12 is not complete.

- [x] **Step 6: Commit**

```bash
git add crates/agent/src/host/sample.rs crates/agent/src/host/mod.rs crates/agent/examples/cpu_sampling.rs
git commit -m "feat: long-lived process sampler with the 200ms CPU rule and null-when-unsampled"
```

---

### Task 13: Wire the host layer together and confirm the whole gate

A single constructor so the next plan has one entry point, plus a full-suite run.

**Files:**
- Create: `crates/agent/src/host/init.rs`
- Modify: `crates/agent/src/host/mod.rs`
- Modify: `crates/agent/src/main.rs`

**Interfaces:**
- Consumes: everything above.
- Produces:
  - `pub struct Host { pub runner: Runner, pub log: Arc<LogBuffer>, pub path: String, pub sampler: Mutex<Sampler> }`
  - `pub async fn init() -> Host`
  - `pub fn missing(&self) -> Vec<String>` — re-probes on demand, per UX-5; does NOT return a boot-time cache

- [x] **Step 1: Write the failing test**

`crates/agent/src/host/init.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn init_produces_a_usable_host() {
        let host = init().await;
        assert!(!host.path.is_empty());
        assert_eq!(host.runner.run("echo", &["ok"], "echo").await.trim(), "ok");
    }

    #[tokio::test]
    async fn missing_is_recomputed_on_each_call_not_cached() {
        // UX-5: a user who installs tmux while the agent runs and presses
        // re-check must get the new answer.
        let host = init().await;
        let a = host.missing();
        let b = host.missing();
        assert_eq!(a, b);
        assert!(a.iter().all(|m| super::super::probe::REQUIRED_BINARIES.contains(&m.as_str())));
    }
}
```

- [x] **Step 2: Run test to verify it fails**

Run: `cargo test -p cdash-agent init`
Expected: FAIL — `cannot find function 'init' in this scope`.

- [x] **Step 3: Write the implementation**

Prepend to `crates/agent/src/host/init.rs`:

```rust
use super::cmd::Runner;
use super::log::LogBuffer;
use super::path::probe_path;
use super::probe::missing_binaries;
use super::sample::Sampler;
use std::sync::{Arc, Mutex};

pub struct Host {
    pub runner: Runner,
    pub log: Arc<LogBuffer>,
    pub path: String,
    pub sampler: Mutex<Sampler>,
}

impl Host {
    /// Re-probes every call. Deliberately not cached: the macOS setup screen's
    /// re-check button is worthless against a boot-time answer.
    pub fn missing(&self) -> Vec<String> {
        missing_binaries(&self.path)
    }
}

pub async fn init() -> Host {
    let log = Arc::new(LogBuffer::new());
    let path = probe_path(&log).await;
    Host {
        runner: Runner::new(path.clone(), log.clone()),
        log,
        path,
        sampler: Mutex::new(Sampler::new()),
    }
}
```

Add to `crates/agent/src/host/mod.rs`:

```rust
pub mod init;
```

- [x] **Step 4: Make the binary exercise it**

`crates/agent/src/main.rs`:

```rust
use cdash_agent::host;

#[tokio::main]
async fn main() {
    let h = host::init::init().await;
    println!("cdash-agent {}", env!("CARGO_PKG_VERSION"));
    println!("PATH: {}", h.path);
    let missing = h.missing();
    if missing.is_empty() {
        println!("all required binaries found");
    } else {
        println!("missing: {}", missing.join(", "));
    }
}
```

Add `rt-multi-thread` to the tokio features in `crates/agent/Cargo.toml`:

```toml
tokio = { version = "1", features = ["process", "time", "rt", "rt-multi-thread", "macros"] }
```

- [x] **Step 5: Run the full suite and the gate**

Run: `cargo test --all -- --test-threads=1`
Expected: PASS, all tests from Tasks 2–13.

Run: `cargo clippy --all-targets -- -D warnings -D clippy::disallowed_types`
Expected: exit 0.

Run: `cargo run -p cdash-agent`
Expected: prints a version, a PATH containing `/usr/local/bin`, and either "all required binaries found" or a `missing:` line naming real absences.

- [x] **Step 6: Commit**

```bash
git add crates/agent/src/host/init.rs crates/agent/src/host/mod.rs crates/agent/src/main.rs crates/agent/Cargo.toml
git commit -m "feat: host layer entry point with on-demand binary re-probing"
```

---

## What this plan deliberately does not cover

- **`machineStats`** (`lib/stats.js:32-35`) — load-average and RAM totals. It belongs with the `/api/sessions` response shape, so it lands in the collect plan alongside its only caller.
- **`browse.js` and `places.js`** — they are I/O against the filesystem with no pure core, and both carry trust-boundary guards (`MAX_ENTRIES`, `assertPath`) that must be ported against the checklist derived at the start of the next plan.
- **Anything that binds a socket.** No router, no routes, no `serve`.
- **Deleting the Node tree.** It remains the parity reference until the parity gate passes.

## Next plan starts here

The next plan covers spec step 3 (collect and orchestration) and **its first task is the checklist derivation**: read every file in `lib/` and `server.js` and enumerate the five categories the spec names — trust-boundary validation, bounds, time-boxes, atomicity/ordering, and exclude-by-design filters. That derivation's output is the task list for the rest of that plan, which is why it could not be written in advance.
