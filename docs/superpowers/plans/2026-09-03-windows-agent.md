# Windows Agent with WSL Reach-Through Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** One native Windows `cdash-agent` that Task Scheduler starts at every logon and that lists, launches, resumes and kills Claude Code sessions on both the Windows side and the WSL side.

**Architecture:** The existing Rust crate compiles for Windows behind `cfg` gates in the `host` layer. A `Side` per Claude installation carries its own Claude directory, command runner, process source and session backend; every collector body is shared and runs once per side. The WSL side is reached over the `\\wsl.localhost` share for files and through `wsl.exe --exec` for commands. A second, windowless binary of the same crate is what the scheduled task runs.

**Tech Stack:** Rust 1.94.1, tokio, axum, sysinfo 0.38.4, `windows-sys` 0.61 (Windows only), `schtasks`, GitHub Actions `windows-latest`.

**Spec:** `docs/superpowers/specs/2026-09-03-windows-agent-design.md` — read it first; every task below cites its section. The adversarial review that shaped it is `docs/superpowers/specs/2026-09-03-windows-agent-design-review.md`.

## Global Constraints

- Toolchain is pinned: `rust-version = "1.94.1"`. Every command below runs with `--locked`; if a step changes `Cargo.lock`, the step says so and commits it.
- `clippy.toml` forbids `std::process::Command` and `tokio::process::Command` everywhere except `crates/agent/src/host/cmd.rs`. CI runs `cargo clippy --all-targets --locked -- -D warnings -D clippy::disallowed_types`. Every subprocess goes through `Runner`. Never add an `#[allow(clippy::disallowed_types)]` outside `cmd.rs`.
- Linux behaviour and its tests are unchanged. `Host` gains no field (test fixtures build it by struct literal). `Ctx::new(host, claude_dir, disk_extra)` keeps its signature.
- No new dependency except `windows-sys = "0.61"` under `[target.'cfg(windows)'.dependencies]` with features `Win32_Foundation`, `Win32_System_Console`, `Win32_Storage_FileSystem`. `rustix` moves under `[target.'cfg(unix)'.dependencies]`.
- Windows-only code cannot be *run* in the implementation environment (Linux), but it can be *type-checked*: the `x86_64-pc-windows-gnu` target and MinGW are installed, and `cargo check --locked --target x86_64-pc-windows-gnu -p cdash-agent` gets through every dependency (verified 2026-09-03; the only errors are the crate's own Unix-only calls, which Tasks 1, 2 and 4 remove). From Task 4 onward every task runs the Windows check below and it must be clean. Runtime behaviour on Windows is exercised only by the `windows` CI job from Task 12. Enum variants only Windows constructs are `#[cfg(windows)]`-gated so `-D warnings` holds on Unix.
- Creation flags are literals in `cmd.rs`: `CREATE_NO_WINDOW = 0x0800_0000`, `CREATE_NEW_CONSOLE = 0x10` (verified against `windows-sys` 0.61.2 `Win32::System::Threading`).
- Every `Runner` spawn on Windows sets `CREATE_NO_WINDOW`; `spawn_detached` is the one exception and sets `CREATE_NEW_CONSOLE`.
- Commit after every task with the messages given. Do not push until Task 12 says to.

**Verification commands used throughout:**

```bash
# unit + integration tests for the agent crate
cargo test -p cdash-agent --locked
# the CI lint gate
cargo clippy -p cdash-agent --all-targets --locked -- -D warnings -D clippy::disallowed_types
# the Linux boot gate (exit 124 from timeout is the pass; the banner is the assertion)
PORT=0 timeout 5 cargo run --locked -p cdash-agent 2>&1 | grep -q "cdash-agent .* on http://127.0.0.1:" && echo BOOT_OK
# the Windows type-check: lib, bins and test modules compiled for Windows, clippy-clean, never run.
# Prereqs (already present here): rustup target add x86_64-pc-windows-gnu; apt-get install mingw-w64
cargo clippy --locked --target x86_64-pc-windows-gnu -p cdash-agent --all-targets -- -D warnings -D clippy::disallowed_types
```

Baseline before Task 1 (verified 2026-09-03): 229 unit tests, 8 + 9 integration tests, all passing; clippy clean.

## File Structure

| File | Responsibility after this plan |
|---|---|
| `crates/agent/Cargo.toml` | Target-specific deps; second `[[bin]]`; `default-run` |
| `crates/agent/src/main.rs` | Console binary: arg parsing, `set-password`, `install`, `uninstall`, then `serve_from_env` |
| `crates/agent/src/main_w.rs` | **new** — windowless binary: `serve_from_env` only |
| `crates/agent/src/host/cmd.rs` | `Runner` gains a command prefix, a log label, stderr in errors, Windows creation flags, `spawn_detached` |
| `crates/agent/src/host/path.rs` | `PATH_SEP`, `known_locations()`, `home()`; Windows skips the login-shell probe |
| `crates/agent/src/host/probe.rs` | Per-platform required binaries; `.exe` check on Windows |
| `crates/agent/src/host/disk.rs` | `root_mount()`; Windows `disk_usage` via `GetDiskFreeSpaceExW` |
| `crates/agent/src/host/sample.rs` | Windows CPU via `global_cpu_usage` |
| `crates/agent/src/host/wsl.rs` | **new** — WSL probe script, prefix builder, `ps` parser, UNC↔Linux path conversion, `probe_wsl` (Windows) |
| `crates/agent/src/host/task.rs` | **new** — Task Scheduler XML, UTF-16 writer, `install`/`uninstall` (Windows) |
| `crates/agent/src/host/mod.rs` | registers `wsl` and `task` |
| `crates/agent/src/collect/side.rs` | **new** — `Side`, `Backend`, `Procs`, path shapes, routing |
| `crates/agent/src/collect/ctx.rs` | `sides`, `wsl_poll_timed_out`, `native()`, `wsl_paths()` |
| `crates/agent/src/collect/external.rs` | `live_session_files` (the liveness predicate), `file_sessions` (ours + external by name) |
| `crates/agent/src/collect/sessions.rs` | per-side collection, WSL time-box, resumable merge |
| `crates/agent/src/collect/spawn.rs` | side-aware spawn/resume/kill, RC poll by pid or name, `--name` |
| `crates/agent/src/collect/validate.rs` | `assert_path` via the shape rules |
| `crates/agent/src/collect/browse.rs` | `crumbs`, roots listing at `/` on Windows |
| `crates/agent/src/collect/mod.rs` | registers `side` |
| `crates/agent/src/http/routes.rs` | `hostinfo.wsl`, browse roots, launch recents rule |
| `crates/agent/src/http/serve.rs` | WSL side at boot; `serve_from_env` |
| `crates/tauri-app/src/main.rs` | `home_dir()` |
| `public/app.js` | server crumbs; picker seeds any path; place names split on both separators |
| `.github/workflows/ci.yml` | `windows` job |
| `README.md` | Windows section |

---

### Task 1: Portability groundwork — deps, home directory, password prompt

Spec §6 (dependencies, `prompt_hidden`), §7 (home), §4 (trust dialog path).

**Files:**
- Modify: `crates/agent/Cargo.toml`
- Modify: `Cargo.lock` (regenerated by `cargo build`)
- Modify: `crates/agent/src/main.rs` (`prompt_hidden`)
- Modify: `crates/agent/src/host/path.rs` (add `home()`)
- Modify: `crates/agent/src/http/serve.rs:47-48,81-83`
- Modify: `crates/agent/src/http/routes.rs:69`
- Modify: `crates/agent/src/collect/spawn.rs:52-61` (`claude_json_path`)
- Modify: `crates/tauri-app/src/main.rs:19-24`

**Interfaces:**
- Produces: `cdash_agent::host::path::home() -> PathBuf`; `claude_json_path(claude_dir: &Path) -> PathBuf` no longer reads the environment.

- [ ] **Step 1: Write the failing test for `claude_json_path`**

In `crates/agent/src/collect/spawn.rs`, inside `mod tests`, add:

```rust
    #[test]
    fn claude_json_lives_beside_the_claude_dir_on_every_side() {
        // The WSL side's file is `\\wsl.localhost\…\home\u\.claude.json`,
        // beside its `.claude`; deriving from the directory rather than from
        // HOME is what makes one rule serve both sides.
        assert_eq!(
            claude_json_path(Path::new("/home/u/.claude")),
            PathBuf::from("/home/u/.claude.json")
        );
        assert_eq!(
            claude_json_path(Path::new("/custom/dir")),
            PathBuf::from("/custom/.claude.json")
        );
    }
```

- [ ] **Step 2: Run it to verify it fails**

Run: `cargo test -p cdash-agent --locked claude_json_lives`
Expected: FAIL — the second assertion resolves to `$HOME/.claude.json` because `CLAUDE_DIR` is unset.

- [ ] **Step 3: Replace `claude_json_path`**

Replace the function and its doc comment in `crates/agent/src/collect/spawn.rs`:

```rust
/// `~/.claude.json` sits beside the Claude directory on every side: the
/// native `~/.claude` and a WSL side's `\\wsl.localhost\…\home\u\.claude`
/// alike. This resolves to the same file the old `HOME`-or-`CLAUDE_DIR` rule
/// did in both of its cases, and reads nothing from the environment.
pub fn claude_json_path(claude_dir: &Path) -> PathBuf {
    claude_dir.parent().unwrap_or(Path::new("/")).join(".claude.json")
}
```

- [ ] **Step 4: Run the test to verify it passes**

Run: `cargo test -p cdash-agent --locked claude_json_lives`
Expected: PASS

- [ ] **Step 5: Move `rustix` under Unix and add `windows-sys` under Windows**

In `crates/agent/Cargo.toml`, delete the line `rustix = { version = "1", features = ["fs", "termios"] }` from `[dependencies]` and append at the end of the file:

```toml
[target.'cfg(unix)'.dependencies]
rustix = { version = "1", features = ["fs", "termios"] }

# Console mode for the password prompt, GetDiskFreeSpaceExW for disk usage.
# Already in the lock at this version through tokio; this makes it explicit.
[target.'cfg(windows)'.dependencies]
windows-sys = { version = "0.61", features = ["Win32_Foundation", "Win32_System_Console", "Win32_Storage_FileSystem"] }
```

- [ ] **Step 6: Add `home()` to `crates/agent/src/host/path.rs`**

Add after the `KNOWN_LOCATIONS` constant:

```rust
/// The user's home. `std::env::home_dir` reads `$HOME` on Unix and the
/// profile directory on Windows, and is not deprecated on the pinned
/// toolchain (verified: `rustc -D deprecated` accepts it on 1.94.1).
pub fn home() -> std::path::PathBuf {
    std::env::home_dir().unwrap_or_else(|| std::path::PathBuf::from("/"))
}
```

- [ ] **Step 7: Replace the three `HOME` reads in the agent**

`crates/agent/src/http/serve.rs`, in `Config::from_env`: replace

```rust
        let home = std::env::var("HOME").unwrap_or_else(|_| "/".to_string());
```
with
```rust
        let home = crate::host::path::home();
```
and replace
```rust
                .unwrap_or_else(|_| PathBuf::from(&home).join(".claude")),
```
with
```rust
                .unwrap_or_else(|_| home.join(".claude")),
```

`crates/agent/src/http/routes.rs`, in `get_browse`: replace
```rust
    let home = std::env::var("HOME").unwrap_or_else(|_| "/".to_string());
```
with
```rust
    let home = crate::host::path::home().to_string_lossy().into_owned();
```

`crates/tauri-app/src/main.rs`, in `server_config`: replace
```rust
    let home = std::env::var("HOME").unwrap_or_else(|_| "/".to_string());
```
with
```rust
    let home = std::env::home_dir().unwrap_or_else(|| PathBuf::from("/"));
```
and `claude_dir: PathBuf::from(home).join(".claude"),` with `claude_dir: home.join(".claude"),`.

- [ ] **Step 8: Split `prompt_hidden` by platform in `crates/agent/src/main.rs`**

Put `#[cfg(unix)]` directly above the existing `fn prompt_hidden` and add this second version after it:

```rust
/// Echo suppression via the console mode. When stdin is not a console (a
/// pipe), `GetConsoleMode` fails and the read is unsuppressed, which is what
/// keeps the subcommand scriptable — the same fallback as the termios path.
#[cfg(windows)]
fn prompt_hidden(prompt: &str) -> Result<String, String> {
    use std::io::{BufRead, Write};
    use windows_sys::Win32::System::Console::{
        GetConsoleMode, GetStdHandle, SetConsoleMode, ENABLE_ECHO_INPUT, STD_INPUT_HANDLE,
    };
    eprint!("{prompt}");
    std::io::stderr().flush().ok();

    // SAFETY: plain Win32 calls on the process's own stdin handle; a null or
    // invalid handle makes GetConsoleMode return 0 and nothing is changed.
    let handle = unsafe { GetStdHandle(STD_INPUT_HANDLE) };
    let mut mode = 0u32;
    let saved = (unsafe { GetConsoleMode(handle, &mut mode) } != 0).then_some(mode);
    if let Some(m) = saved {
        unsafe { SetConsoleMode(handle, m & !ENABLE_ECHO_INPUT) };
    }

    let mut line = String::new();
    let read = std::io::stdin().lock().read_line(&mut line);
    if let Some(m) = saved {
        unsafe { SetConsoleMode(handle, m) };
        eprintln!();
    }
    read.map_err(|e| e.to_string())?;

    Ok(line.trim_end_matches(['\n', '\r']).to_string())
}
```

- [ ] **Step 9: Regenerate the lockfile and check the diff is only the new edge**

Run: `cargo build -p cdash-agent && git diff --stat Cargo.lock && git diff Cargo.lock | grep '^[-+]' | grep -v '^[-+][-+]' | head -20`
Expected: builds; the diff adds `windows-sys 0.61.2` to the `cdash-agent` package's dependency list and nothing else (0.61.2 is already in the lock).

- [ ] **Step 10: Run the full suite and clippy**

Run: `cargo test -p cdash-agent --locked && cargo clippy -p cdash-agent --all-targets --locked -- -D warnings -D clippy::disallowed_types`
Expected: all pass, no warnings.

- [ ] **Step 11: Commit**

```bash
git add crates/agent/Cargo.toml Cargo.lock crates/agent/src/main.rs crates/agent/src/host/path.rs crates/agent/src/http/serve.rs crates/agent/src/http/routes.rs crates/agent/src/collect/spawn.rs crates/tauri-app/src/main.rs
git commit -m "agent: platform-gate rustix, add windows-sys, resolve home and .claude.json portably

rustix moves under cfg(unix) and windows-sys arrives under cfg(windows) with
the console and filesystem features the Windows arms need. HOME reads become
std::env::home_dir, and ~/.claude.json is derived from the Claude directory
on every side rather than from the environment. The password prompt gains a
SetConsoleMode arm beside the termios one."
```

---

### Task 2: PATH separator, known locations and the binary check on Windows

Spec §7 (PATH, binaries).

**Files:**
- Modify: `crates/agent/src/host/path.rs`
- Modify: `crates/agent/src/host/probe.rs`

**Interfaces:**
- Produces: `host::path::PATH_SEP: char` (`;` on Windows, `:` elsewhere); `host::path::known_locations() -> Vec<String>`; `host::probe::REQUIRED_BINARIES` is `["claude", "git"]` on Windows.
- `compose_path` and `missing_binaries` keep their signatures.

- [ ] **Step 1: Write the platform-neutral failing test in `path.rs`**

The existing `mod tests` in `crates/agent/src/host/path.rs` asserts Unix strings and runs a login shell. Put `#[cfg(unix)]` directly above its `mod tests` line, and add this second module after it:

```rust
#[cfg(test)]
mod portable_tests {
    use super::*;

    #[test]
    fn compose_path_dedupes_on_the_platform_separator_and_keeps_the_backstop() {
        let sep = PATH_SEP.to_string();
        let a = std::env::temp_dir().join("cdash-a").to_string_lossy().into_owned();
        let b = std::env::temp_dir().join("cdash-b").to_string_lossy().into_owned();
        let out = compose_path(Some(&a), &format!("{a}{sep}{b}{sep}{a}"));
        let segs: Vec<&str> = out.split(PATH_SEP).collect();
        assert_eq!(segs[0], a, "the probed value comes first");
        assert_eq!(segs.iter().filter(|s| **s == a).count(), 1, "deduped");
        assert!(segs.contains(&b.as_str()));
        for k in known_locations() {
            assert!(segs.contains(&k.as_str()), "backstop {k} missing from {out}");
        }
    }
}
```

- [ ] **Step 2: Run it to verify it fails**

Run: `cargo test -p cdash-agent --locked portable_tests`
Expected: FAIL to compile — `PATH_SEP` and `known_locations` do not exist.

- [ ] **Step 3: Implement `PATH_SEP`, `known_locations`, and the Windows probe branch**

In `crates/agent/src/host/path.rs`, replace the `KNOWN_LOCATIONS` constant with:

```rust
#[cfg(windows)]
pub const PATH_SEP: char = ';';
#[cfg(not(windows))]
pub const PATH_SEP: char = ':';

/// Where an installer puts binaries that a GUI launch's PATH lacks: Homebrew
/// and /usr/local on Unix, the native Claude installer's
/// `%USERPROFILE%\.local\bin` on Windows.
pub fn known_locations() -> Vec<String> {
    #[cfg(windows)]
    {
        vec![home().join(".local").join("bin").to_string_lossy().into_owned()]
    }
    #[cfg(not(windows))]
    {
        vec!["/opt/homebrew/bin".to_string(), "/usr/local/bin".to_string()]
    }
}
```

Replace `compose_path` with:

```rust
/// Compose the child PATH: probed value first (so a user's own ordering wins),
/// then the known-location backstop, then whatever we inherited. Deduped,
/// first occurrence kept, empty segments dropped.
pub fn compose_path(probed: Option<&str>, inherited: &str) -> String {
    let sep = PATH_SEP.to_string();
    let known = known_locations().join(&sep);
    let mut out: Vec<&str> = Vec::new();
    for src in [probed.unwrap_or(""), known.as_str(), inherited] {
        for seg in src.split(PATH_SEP) {
            if !seg.is_empty() && !out.contains(&seg) {
                out.push(seg);
            }
        }
    }
    out.join(&sep)
}
```

In `probe_path`, insert directly after the `let inherited = …;` line:

```rust
    // No login shell to ask on Windows: a logon-triggered task inherits the
    // user's own environment block, so the inherited PATH is the user's PATH.
    #[cfg(windows)]
    {
        let _ = log;
        return compose_path(None, &inherited);
    }
```

The rest of `probe_path` (the `$SHELL -l -c` probe) stays as it is; on Windows it is now unreachable code after a `return`, which rustc accepts inside a `#[cfg]`-split function only if the remainder is also gated — so wrap the remainder: put `#[cfg(not(windows))]` on a block containing everything from `let shell = …` through the final `compose_path(...)` expression:

```rust
    #[cfg(not(windows))]
    {
        let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".to_string());
        // … existing body unchanged …
        compose_path(probed.as_deref(), &inherited)
    }
```

- [ ] **Step 4: Run the path tests**

Run: `cargo test -p cdash-agent --locked host::path`
Expected: PASS (both modules on Linux).

- [ ] **Step 5: Write the failing binary-check test in `probe.rs`**

In `crates/agent/src/host/probe.rs`, the existing `mod tests` uses `PermissionsExt`; put `#[cfg(unix)]` above it and change its first test to compare against the constant rather than a literal list, so the same assertion holds on both platforms:

```rust
    #[test]
    fn reports_all_required_binaries_when_path_is_empty() {
        let missing = missing_binaries("");
        assert_eq!(missing.iter().map(String::as_str).collect::<Vec<_>>(), REQUIRED_BINARIES);
    }
```

Then add a Windows-only module after it:

```rust
#[cfg(all(test, windows))]
mod windows_tests {
    use super::*;

    #[test]
    fn an_exe_file_on_a_semicolon_separated_path_is_found() {
        let dir = std::env::temp_dir().join(format!("cdash-probe-win-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("git.exe"), "MZ").unwrap();
        let path = format!("C:\\definitely-not-here;{}", dir.display());
        let missing = missing_binaries(&path);
        assert!(!missing.contains(&"git".to_string()), "{missing:?}");
        assert!(missing.contains(&"claude".to_string()));
    }
}
```

- [ ] **Step 6: Run the probe tests before the change**

Run: `cargo test -p cdash-agent --locked host::probe`
Expected: PASS on Linux — the Unix arm is unchanged by this task; there is no locally failing test for the Windows arm, which the `windows` CI job (Task 12) exercises through `windows_tests`.

- [ ] **Step 7: Implement the platform arms in `probe.rs`**

Replace everything above `pub fn missing_binaries` with:

```rust
use super::path::PATH_SEP;
use std::path::Path;

/// `ps` and `df` are absent by design: the Rust agent uses `sysinfo` and
/// `statvfs` and never shells out to them. tmux is required only where tmux
/// is the session backend; on Windows the WSL side reports its own list
/// through `/api/hostinfo`.
#[cfg(windows)]
pub const REQUIRED_BINARIES: &[&str] = &["claude", "git"];
#[cfg(not(windows))]
pub const REQUIRED_BINARIES: &[&str] = &["tmux", "claude", "git"];

#[cfg(unix)]
fn is_executable(p: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(p)
        .map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

/// Only the native installer's `claude.exe` counts: an npm `claude.cmd` is
/// not an executable image and cannot be spawned by CreateProcess.
#[cfg(windows)]
fn is_executable(p: &Path) -> bool {
    p.with_extension("exe").is_file()
}
```

and in `missing_binaries` change `.split(':')` to `.split(PATH_SEP)`.

- [ ] **Step 8: Run the tests and clippy**

Run: `cargo test -p cdash-agent --locked host:: && cargo clippy -p cdash-agent --all-targets --locked -- -D warnings -D clippy::disallowed_types`
Expected: PASS, no warnings.

- [ ] **Step 9: Commit**

```bash
git add crates/agent/src/host/path.rs crates/agent/src/host/probe.rs
git commit -m "host: platform PATH separator, Windows known location, .exe binary check

PATH composes on ';' on Windows with %USERPROFILE%\.local\bin as the backstop
and skips the login-shell probe there; the binary check looks for .exe and
no longer requires tmux on Windows, where tmux is not the backend."
```

---

### Task 3: `Runner` — command prefix, log label, stderr in errors, creation flags, `spawn_detached`

Spec §2 (WSL runner prefix, log prefix), §3 (`spawn_detached`), §6 (creation flags).

**Files:**
- Modify: `crates/agent/src/host/cmd.rs`

**Interfaces:**
- Produces:
  - `Runner::with_prefix(prefix: Vec<String>, label: &'static str, path: String, log: Arc<LogBuffer>) -> Runner` — every command becomes `prefix[0] prefix[1..] program args`; `label` is prepended to failure log lines (`"wsl "` for the WSL side, `""` for native).
  - `Runner::new(path, log)` unchanged, equal to `with_prefix(Vec::new(), "", path, log)`.
  - `pub async fn run_checked_with_timeout(&self, program, args, key, timeout) -> Result<String, String>` — was private; callers in later tasks need the 30-second variant.
  - `pub fn spawn_detached(&self, program: &str, args: &[&str], cwd: &str, key: &str) -> Result<(), String>` — spawns without waiting; must be called from within the tokio runtime (route handlers are).
  - Error strings from checked runs now end with the last stderr line when there is one: `"<key>: exit 7: boom"`.

- [ ] **Step 1: Write the failing tests**

Append inside `mod tests` in `crates/agent/src/host/cmd.rs`:

```rust
    #[tokio::test]
    async fn a_prefix_wraps_every_command() {
        // The WSL runner is `wsl.exe --exec env PATH=… <program> <args>`.
        // `env` stands in for wsl.exe here, so the composition is proven
        // without a distro: a variable set by the prefix reaches the child.
        let log = Arc::new(LogBuffer::new());
        let path = std::env::var("PATH").unwrap_or_default();
        let r = Runner::with_prefix(
            vec!["env".into(), "CDASH_PREFIX_TEST=42".into()],
            "wsl ",
            path,
            log,
        );
        let out = r.run("sh", &["-c", "echo $CDASH_PREFIX_TEST"], "sh").await;
        assert_eq!(out.trim(), "42");
    }

    #[tokio::test]
    async fn the_label_prefixes_the_failure_line() {
        // Two sides share one log; a failed `tmux list-panes` must say which.
        let log = Arc::new(LogBuffer::new());
        let path = std::env::var("PATH").unwrap_or_default();
        let r = Runner::with_prefix(Vec::new(), "wsl ", path, log.clone());
        r.run("false", &[], "tmux list-panes").await;
        assert!(
            log.lines()[0].contains("sh failed: wsl tmux list-panes"),
            "{:?}",
            log.lines()
        );
    }

    #[tokio::test]
    async fn stderr_reaches_the_error_message() {
        // `schtasks` and `wsl.exe` explain themselves on stderr; "exit 1"
        // alone sends the operator to guess.
        let (r, _) = runner();
        let e = r
            .run_checked("sh", &["-c", "echo boom >&2; exit 7"], "sh")
            .await
            .unwrap_err();
        assert!(e.contains("exit 7: boom"), "{e}");
    }

    #[tokio::test]
    async fn spawn_detached_returns_at_once_and_reports_a_missing_program() {
        let (r, _) = runner();
        let started = std::time::Instant::now();
        r.spawn_detached("sleep", &["3"], "/", "sleep").unwrap();
        assert!(started.elapsed() < Duration::from_secs(1), "must not wait for the child");
        assert!(r.spawn_detached("cdash-no-such-program", &[], "/", "nope").is_err());
    }
```

- [ ] **Step 2: Run them to verify they fail**

Run: `cargo test -p cdash-agent --locked host::cmd`
Expected: compile errors — `with_prefix` and `spawn_detached` do not exist.

- [ ] **Step 3: Rewrite the non-test part of `cmd.rs`**

Replace everything above `#[cfg(test)]` in `crates/agent/src/host/cmd.rs` with:

```rust
use super::log::LogBuffer;
use std::collections::HashSet;
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// Default subprocess deadline. This exists because `git status` on a 9P mount
/// once took over 60 seconds and stalled every 4-second poll. Do not raise it
/// without measuring; do not remove it.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(5);

/// `CREATE_NO_WINDOW`: a console child of a windowless parent must not open a
/// console of its own. Ignored when combined with `CREATE_NEW_CONSOLE`.
#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;
/// `CREATE_NEW_CONSOLE`: the one spawn that wants a window — a Claude session.
#[cfg(windows)]
const CREATE_NEW_CONSOLE: u32 = 0x10;

/// The only sanctioned way to run a subprocess. `clippy.toml` forbids
/// `std::process::Command` and `tokio::process::Command` everywhere else, and
/// `-D clippy::disallowed_types` is a required CI gate, because this helper is
/// the sole enforcement of the time-box above.
pub struct Runner {
    path: String,
    log: Arc<LogBuffer>,
    failed: Mutex<HashSet<String>>,
    /// Prepended to every command: `["wsl.exe", "--exec", "/usr/bin/env",
    /// "PATH=…"]` turns this runner into the WSL side's. Empty for native.
    prefix: Vec<String>,
    /// Prepended to failure log lines so two sides sharing one log are told
    /// apart: `"wsl "` or `""`.
    label: &'static str,
}

impl Runner {
    pub fn new(path: String, log: Arc<LogBuffer>) -> Self {
        Self::with_prefix(Vec::new(), "", path, log)
    }

    pub fn with_prefix(
        prefix: Vec<String>,
        label: &'static str,
        path: String,
        log: Arc<LogBuffer>,
    ) -> Self {
        Self { path, log, failed: Mutex::new(HashSet::new()), prefix, label }
    }

    /// Swallowing: failure is an empty string. Correct for the 4-second poll,
    /// where a broken `git status` must not fail the whole request — and wrong
    /// for anything that changes state, which is what `run_checked` is for.
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
        self.run_checked_with_timeout(program, args, key, timeout).await.unwrap_or_default()
    }

    /// Fallible: the caller learns the command failed. Every mutating route
    /// uses this, because reporting a kill that did not happen is worse than
    /// reporting an error.
    pub async fn run_checked(
        &self,
        program: &str,
        args: &[&str],
        key: &str,
    ) -> Result<String, String> {
        self.run_checked_with_timeout(program, args, key, DEFAULT_TIMEOUT).await
    }

    /// The prefix applied: `(program, args)` becomes
    /// `(prefix[0], prefix[1..] ++ [program] ++ args)`.
    fn compose<'a>(&'a self, program: &'a str, args: &[&'a str]) -> (&'a str, Vec<&'a str>) {
        match self.prefix.first() {
            None => (program, args.to_vec()),
            Some(head) => {
                let mut all: Vec<&str> = self.prefix[1..].iter().map(String::as_str).collect();
                all.push(program);
                all.extend_from_slice(args);
                (head.as_str(), all)
            }
        }
    }

    pub async fn run_checked_with_timeout(
        &self,
        program: &str,
        args: &[&str],
        key: &str,
        timeout: Duration,
    ) -> Result<String, String> {
        let (program, args) = self.compose(program, args);
        #[allow(clippy::disallowed_types)]
        let mut cmd = tokio::process::Command::new(program);
        cmd.args(&args)
            .env("PATH", &self.path)
            .stdin(std::process::Stdio::null())
            .kill_on_drop(true); // the timeout must actually kill the child
        #[cfg(windows)]
        cmd.creation_flags(CREATE_NO_WINDOW);
        let fut = cmd.output();

        let reason = match tokio::time::timeout(timeout, fut).await {
            Err(_) => format!("timed out after {}ms", timeout.as_millis()),
            Ok(Err(e)) => e.to_string(),
            Ok(Ok(out)) if !out.status.success() => {
                let code = out.status.code().unwrap_or(-1);
                // wsl.exe's own messages are UTF-16; dropping the NULs leaves
                // readable ASCII rather than "E R R O R".
                let err = String::from_utf8_lossy(&out.stderr).replace('\0', "");
                match err.lines().rev().find(|l| !l.trim().is_empty()) {
                    Some(last) => format!("exit {code}: {}", last.trim()),
                    None => format!("exit {code}"),
                }
            }
            Ok(Ok(out)) => return Ok(String::from_utf8_lossy(&out.stdout).into_owned()),
        };
        self.log_once(key, &reason);
        Err(format!("{key}: {reason}"))
    }

    /// Start a program and do not wait for it: a Claude session in its own
    /// console window. No time-box applies because nothing is awaited; no
    /// `kill_on_drop`, because the session must outlive this process. On
    /// Windows the child gets a new console; from a windowless parent it also
    /// gets that console's standard handles (see spec §3 for the console
    /// parent's limitation). Must be called from within the tokio runtime.
    pub fn spawn_detached(
        &self,
        program: &str,
        args: &[&str],
        cwd: &str,
        key: &str,
    ) -> Result<(), String> {
        let (program, args) = self.compose(program, args);
        #[allow(clippy::disallowed_types)]
        let mut cmd = tokio::process::Command::new(program);
        cmd.args(&args).current_dir(cwd).env("PATH", &self.path);
        #[cfg(windows)]
        cmd.creation_flags(CREATE_NEW_CONSOLE);
        match cmd.spawn() {
            Ok(_child) => Ok(()), // dropped: tokio reaps it, it keeps running
            Err(e) => {
                let reason = e.to_string();
                self.log_once(key, &reason);
                Err(format!("{key}: {reason}"))
            }
        }
    }

    /// Log a given failing key once per process lifetime. The KEY IS EXPLICIT:
    /// deriving it from `program + args[0]` is what made every `git status`
    /// failure across every repository collapse into one silenced entry.
    fn log_once(&self, key: &str, reason: &str) {
        let mut failed = self.failed.lock().unwrap_or_else(|e| e.into_inner());
        if failed.insert(key.to_string()) {
            self.log.push(format!("sh failed: {}{key}: {reason}", self.label));
        }
    }
}
```

- [ ] **Step 4: Gate the shell-dependent tests for Unix**

The existing `mod tests` in `cmd.rs` runs `echo`, `false` and `sleep`, none of which is an executable on Windows. Put `#[cfg(unix)]` directly above `mod tests` (the `#[cfg(test)]` line stays). For the same reason put `#[cfg(unix)]` directly above `init_produces_a_usable_host` in `crates/agent/src/host/init.rs` — it runs `echo`; its sibling `missing_is_recomputed_on_each_call_not_cached` is portable and stays. Add a Windows-only success-path test after the `cmd.rs` module:

```rust
#[cfg(all(test, windows))]
mod windows_tests {
    use super::*;

    #[tokio::test]
    async fn returns_stdout_on_success_through_cmd() {
        let log = Arc::new(LogBuffer::new());
        let r = Runner::new(std::env::var("PATH").unwrap_or_default(), log);
        let out = r.run("cmd", &["/c", "echo hello"], "cmd").await;
        assert_eq!(out.trim(), "hello");
    }
}
```

- [ ] **Step 5: Run the tests and clippy**

Run: `cargo test -p cdash-agent --locked host::cmd && cargo clippy -p cdash-agent --all-targets --locked -- -D warnings -D clippy::disallowed_types`
Expected: all `host::cmd` tests PASS including the four new ones; no warnings. The existing `a_hung_child_is_killed_at_the_timeout` and `logs_once_per_key` tests still pass — the label for `Runner::new` is empty, so their log lines are unchanged.

- [ ] **Step 6: Commit**

```bash
git add crates/agent/src/host/cmd.rs
git commit -m "host: Runner prefix, log label, stderr in errors, Windows creation flags, spawn_detached

A prefixed Runner is how the WSL side runs tmux and git through wsl.exe
--exec; the label tells the two sides' failures apart in one log. Checked
runs now carry the last stderr line, which is where schtasks and wsl.exe
explain themselves. spawn_detached starts a Claude session in its own console
and returns; every other spawn on Windows sets CREATE_NO_WINDOW."
```

---

### Task 4: Disk and CPU on Windows, `root_mount()`

Spec §7 (CPU, disk). After this task the Windows type-check is clean for the first time.

**Files:**
- Modify: `crates/agent/src/host/disk.rs`
- Modify: `crates/agent/src/host/sample.rs`
- Modify: `crates/agent/src/collect/sessions.rs` (the `disk_usage("/")` call and its test)

**Interfaces:**
- Produces: `host::disk::root_mount() -> String` — `"/"` on Unix, `"C:\"` (from `%SystemDrive%`) on Windows. `disk_usage(mount)` keeps its signature on both platforms.

- [ ] **Step 1: Make the root-disk tests platform-neutral (they fail to compile first)**

In `crates/agent/src/host/disk.rs` `mod tests`, replace `root_reports_plausible_totals` with:

```rust
    #[test]
    fn the_root_mount_reports_plausible_totals() {
        let m = root_mount();
        let u = disk_usage(&m).expect("the root mount must be statable");
        assert_eq!(u.mount, m);
        assert!(u.total_kb > 0);
        assert!(u.free_kb <= u.total_kb);
    }
```

In `crates/agent/src/collect/sessions.rs` `mod tests`, replace the body of `the_root_disk_is_always_reported` with:

```rust
        let d = tempdir("disks");
        std::fs::write(d.join("history.jsonl"), "").unwrap();
        let r = collect_sessions(&ctx_for(d)).await;
        assert_eq!(r.stats.disks[0].mount, crate::host::disk::root_mount());
        assert!(r.stats.disks[0].total_kb > 0);
```

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test -p cdash-agent --locked root_mount`
Expected: compile error — `root_mount` does not exist.

- [ ] **Step 3: Implement `root_mount` and the Windows `disk_usage`**

In `crates/agent/src/host/disk.rs`, put `#[cfg(unix)]` directly above the existing `pub fn disk_usage` and add after the `DiskUsage` struct:

```rust
/// The mount the stats bar always reports: `/` on Unix, the system drive on
/// Windows (`C:\` unless Windows was installed elsewhere).
pub fn root_mount() -> String {
    #[cfg(windows)]
    {
        format!("{}\\", std::env::var("SystemDrive").unwrap_or_else(|_| "C:".to_string()))
    }
    #[cfg(not(windows))]
    {
        "/".to_string()
    }
}

/// One `GetDiskFreeSpaceExW` call — the same shape as `statvfs(mount)`: the
/// caller names the directory and nothing is listed or parsed. A mapped drive
/// or a UNC path is answered by the same call; a path that does not exist is
/// `None`. `sysinfo::Disks` was rejected: it opens every fixed and removable
/// volume with `DeviceIoControl` on each poll and skips network drives.
#[cfg(windows)]
pub fn disk_usage(mount: &str) -> Option<DiskUsage> {
    use windows_sys::Win32::Storage::FileSystem::GetDiskFreeSpaceExW;
    let wide: Vec<u16> = mount.encode_utf16().chain(std::iter::once(0)).collect();
    let (mut free, mut total) = (0u64, 0u64);
    // SAFETY: `wide` is NUL-terminated and outlives the call; the out-pointers
    // are to locals; the fourth pointer is documented as optional.
    let ok = unsafe {
        GetDiskFreeSpaceExW(wide.as_ptr(), &mut free, &mut total, std::ptr::null_mut())
    };
    if ok == 0 {
        return None;
    }
    Some(DiskUsage { mount: mount.to_string(), free_kb: free / 1024, total_kb: total / 1024 })
}
```

In `crates/agent/src/collect/sessions.rs`, change `disk_usage("/")` in `collect_sessions` to `disk_usage(&crate::host::disk::root_mount())`.

- [ ] **Step 4: Implement the Windows CPU figure in `sample.rs`**

In `crates/agent/src/host/sample.rs`, inside `refresh_if_due`, directly after the `self.sys.refresh_processes_specifics(...)` call add:

```rust
        // The global figure needs its own refresh, under the same interval
        // rule and the same "first sample is a baseline" logic.
        #[cfg(windows)]
        self.sys.refresh_cpu_usage();
```

Replace the body of `machine_stats` with:

```rust
        self.sys.refresh_memory();
        #[cfg(not(windows))]
        let pct = {
            let cores = std::thread::available_parallelism()
                .map(|n| n.get())
                .unwrap_or(1) as f64;
            let load = System::load_average().one;
            ((load / cores) * 100.0).round()
        };
        // sysinfo's Windows "load average" is an estimate from the processor
        // queue length — threads waiting, not running — and reads near zero on
        // a busy machine. Use the measured global CPU; like the per-process
        // figures it is valid only from the second refresh, so the first poll
        // says 0 rather than a number that means nothing.
        #[cfg(windows)]
        let pct = {
            self.refresh_if_due();
            if self.cpu_valid { f64::from(self.sys.global_cpu_usage()).round() } else { 0.0 }
        };
        let to_kb = |bytes: u64| (bytes as f64 / 1024.0).round() as u64;
        let total = self.sys.total_memory();
        MachineStats {
            cpu_pct: pct.clamp(0.0, 100.0) as u32,
            ram_used_kb: to_kb(total.saturating_sub(self.sys.free_memory())),
            ram_total_kb: to_kb(total),
        }
```

- [ ] **Step 5: Run the Linux tests and clippy**

Run: `cargo test -p cdash-agent --locked && cargo clippy -p cdash-agent --all-targets --locked -- -D warnings -D clippy::disallowed_types`
Expected: PASS, no warnings.

- [ ] **Step 6: Run the Windows type-check — the first time it must be clean**

Run: `cargo clippy --locked --target x86_64-pc-windows-gnu -p cdash-agent --all-targets -- -D warnings -D clippy::disallowed_types`
Expected: no errors and no warnings. If `main.rs` still names `rustix`, Task 1's `#[cfg(unix)]` on the Unix `prompt_hidden` was missed; if `probe.rs` fails on `mode()`, Task 2's split was missed. Fix those, not this task.

- [ ] **Step 7: Commit**

```bash
git add crates/agent/src/host/disk.rs crates/agent/src/host/sample.rs crates/agent/src/collect/sessions.rs
git commit -m "host: Windows disk usage via GetDiskFreeSpaceExW, measured CPU, root_mount

The stats bar's root mount is the system drive on Windows. Disk usage is
one GetDiskFreeSpaceExW call per mount, the same shape as statvfs. The CPU
figure on Windows is global_cpu_usage under the sampler's 200 ms rule,
because sysinfo's Windows load average counts waiting threads. The crate
now type-checks for x86_64-pc-windows-gnu."
```

---

### Task 5: The WSL bridge's pure parts — `host/wsl.rs`

Spec §2 (probe, prefix, path conversion, `ps`). Everything in this task compiles and is tested on every host; the Windows-only `probe_wsl` comes in Task 9.

**Files:**
- Create: `crates/agent/src/host/wsl.rs`
- Modify: `crates/agent/src/host/mod.rs` (add `pub mod wsl;`)

**Interfaces:**
- Produces (all in `cdash_agent::host::wsl`):
  - `PROBE_SCRIPT: &str`, `MISSING_SCRIPT: &str`, `PS_ARGS: &[&str]`
  - `struct WslProbe { path: String, home_unc: String, distro_flag: Option<String> }`; `parse_wsl_probe(out: &str) -> Option<WslProbe>` (sets `distro_flag: None`)
  - `wsl_prefix(distro_flag: Option<&str>, path: &str) -> Vec<String>`
  - `struct WslPaths { unc_root: String, distro: String }` with `from_home_unc(&str) -> Option<Self>`, `to_unc(&self, linux: &str) -> String`, `from_unc(&self, unc: &str) -> Option<String>`
  - `parse_ps(out: &str) -> Vec<ProcRow>`

- [ ] **Step 1: Create the module with its tests first**

Create `crates/agent/src/host/wsl.rs` containing only the tests and a module doc, so the next step fails to compile on every missing item:

```rust
//! The WSL bridge's pure parts: how the distro is reached, how its paths map
//! onto the `\\wsl.localhost` share, and how its process list is read. All of
//! it compiles and is tested on every host; only `probe_wsl` is Windows.

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn probe_output_is_path_then_home() {
        let p = parse_wsl_probe("/home/u/.local/bin:/usr/bin\n\\\\wsl.localhost\\Ubuntu\\home\\u\n").unwrap();
        assert_eq!(p.path, "/home/u/.local/bin:/usr/bin");
        assert_eq!(p.home_unc, "\\\\wsl.localhost\\Ubuntu\\home\\u");
        assert_eq!(p.distro_flag, None);
    }

    #[test]
    fn probe_output_without_a_unc_home_is_rejected() {
        // A distro whose wslpath is broken prints something else on line 2;
        // building a share path from it would read the wrong disk.
        assert!(parse_wsl_probe("/usr/bin\n/home/u\n").is_none());
        assert!(parse_wsl_probe("").is_none());
        assert!(parse_wsl_probe("/usr/bin\n").is_none());
    }

    #[test]
    fn the_prefix_names_the_distro_only_when_asked() {
        assert_eq!(
            wsl_prefix(None, "/usr/bin"),
            vec!["wsl.exe", "--exec", "/usr/bin/env", "PATH=/usr/bin"]
        );
        assert_eq!(
            wsl_prefix(Some("Debian"), "/usr/bin"),
            vec!["wsl.exe", "-d", "Debian", "--exec", "/usr/bin/env", "PATH=/usr/bin"]
        );
    }

    #[test]
    fn paths_come_from_the_home_line_under_either_share_host() {
        let new = WslPaths::from_home_unc("\\\\wsl.localhost\\Ubuntu\\home\\u").unwrap();
        assert_eq!(new.unc_root, "\\\\wsl.localhost\\Ubuntu");
        assert_eq!(new.distro, "Ubuntu");
        let old = WslPaths::from_home_unc("\\\\wsl$\\Ubuntu-22.04\\root").unwrap();
        assert_eq!(old.unc_root, "\\\\wsl$\\Ubuntu-22.04");
        assert_eq!(old.distro, "Ubuntu-22.04");
        assert!(WslPaths::from_home_unc("\\\\server\\share\\x").is_none());
        assert!(WslPaths::from_home_unc("C:\\Users\\u").is_none());
    }

    fn ubuntu() -> WslPaths {
        WslPaths { unc_root: "\\\\wsl.localhost\\Ubuntu".into(), distro: "Ubuntu".into() }
    }

    #[test]
    fn to_unc_maps_a_linux_path_onto_the_share() {
        assert_eq!(ubuntu().to_unc("/home/u/p"), "\\\\wsl.localhost\\Ubuntu\\home\\u\\p");
        assert_eq!(ubuntu().to_unc("/"), "\\\\wsl.localhost\\Ubuntu\\");
    }

    #[test]
    fn from_unc_accepts_this_distro_under_either_host_and_nothing_else() {
        let w = ubuntu();
        assert_eq!(w.from_unc("\\\\wsl.localhost\\Ubuntu\\home\\u\\p").as_deref(), Some("/home/u/p"));
        assert_eq!(w.from_unc("\\\\wsl$\\ubuntu\\home\\u\\").as_deref(), Some("/home/u"), "case-insensitive, trailing separator dropped");
        assert_eq!(w.from_unc("\\\\wsl.localhost\\Ubuntu\\").as_deref(), Some("/"));
        assert_eq!(w.from_unc("\\\\wsl.localhost\\Ubuntu").as_deref(), Some("/"));
        assert_eq!(w.from_unc("\\\\wsl.localhost\\Debian\\home"), None, "a foreign distro is not ours to launch into");
        assert_eq!(w.from_unc("\\\\server\\share\\x"), None);
        assert_eq!(w.from_unc("/home/u"), None);
    }

    #[test]
    fn ps_rows_parse_with_padding_and_a_space_in_the_command_name() {
        let out = "    1     0  0.0  1024 init\n\
                   4242  1000 12.5 51200 claude\n\
                   4300  4242  0.3  4096 my helper\n\
                   junk line\n";
        let rows = parse_ps(out);
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[1].pid, 4242);
        assert_eq!(rows[1].ppid, 1000);
        assert_eq!(rows[1].cpu, 12.5);
        assert_eq!(rows[1].rss_kb, 51200);
        assert_eq!(rows[1].name, "claude");
        assert_eq!(rows[2].name, "my helper", "comm is last so its spaces cannot shift the numbers");
    }
}
```

Add `pub mod wsl;` to `crates/agent/src/host/mod.rs`.

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p cdash-agent --locked host::wsl`
Expected: compile errors for every missing item.

- [ ] **Step 3: Implement the module above the tests**

Insert between the module doc and `#[cfg(test)]`:

```rust
use crate::host::proc::ProcRow;

/// Run once at boot inside the distro with `sh -lc`: line 1 the login-shell
/// PATH, line 2 the home directory as the share sees it (`wslpath -w`).
pub const PROBE_SCRIPT: &str = r#"printf "%s\n%s\n" "$PATH" "$(wslpath -w "$HOME")""#;

/// One line per binary the WSL side needs and lacks. Re-run per
/// `/api/hostinfo` call, like the native list.
pub const MISSING_SCRIPT: &str =
    r#"for b in tmux claude git; do command -v "$b" >/dev/null 2>&1 || printf "%s\n" "$b"; done"#;

/// `ps` columns, `comm` last so a space in a command name cannot shift the
/// numeric fields.
pub const PS_ARGS: &[&str] = &["-eo", "pid=,ppid=,%cpu=,rss=,comm="];

#[derive(Debug, Clone, PartialEq)]
pub struct WslProbe {
    /// The login-shell PATH inside the distro.
    pub path: String,
    /// `\\wsl.localhost\Ubuntu\home\u` or, on older WSL, `\\wsl$\Ubuntu\home\u`.
    pub home_unc: String,
    /// `CDASH_WSL_DISTRO` when set. `None` means the default distro: no `-d`.
    pub distro_flag: Option<String>,
}

pub fn parse_wsl_probe(out: &str) -> Option<WslProbe> {
    let mut lines = out.lines().map(str::trim).filter(|l| !l.is_empty());
    let path = lines.next()?.to_string();
    let home_unc = lines.next()?.to_string();
    if !home_unc.starts_with("\\\\") {
        return None;
    }
    Some(WslProbe { path, home_unc, distro_flag: None })
}

/// The command prefix that turns a native `Runner` into the WSL side's:
/// `--exec` skips the distro's shell so arguments arrive unchanged, and `env`
/// applies the probed login PATH without sourcing a profile per call.
pub fn wsl_prefix(distro_flag: Option<&str>, path: &str) -> Vec<String> {
    let mut v = vec!["wsl.exe".to_string()];
    if let Some(d) = distro_flag {
        v.push("-d".to_string());
        v.push(d.to_string());
    }
    v.push("--exec".to_string());
    v.push("/usr/bin/env".to_string());
    v.push(format!("PATH={path}"));
    v
}

/// Both spellings of the share host; the share itself is case-insensitive.
const SHARE_HOSTS: &[&str] = &["wsl.localhost", "wsl$"];

#[derive(Debug, Clone, PartialEq)]
pub struct WslPaths {
    /// `\\wsl.localhost\Ubuntu`, no trailing separator.
    pub unc_root: String,
    pub distro: String,
}

impl WslPaths {
    /// From the probe's home line.
    pub fn from_home_unc(home_unc: &str) -> Option<Self> {
        let rest = home_unc.strip_prefix("\\\\")?;
        let mut parts = rest.split('\\');
        let host = parts.next()?;
        let distro = parts.next()?;
        if distro.is_empty() || !SHARE_HOSTS.iter().any(|h| h.eq_ignore_ascii_case(host)) {
            return None;
        }
        Some(Self { unc_root: format!("\\\\{host}\\{distro}"), distro: distro.to_string() })
    }

    pub fn to_unc(&self, linux: &str) -> String {
        format!("{}{}", self.unc_root, linux.replace('/', "\\"))
    }

    /// `\\wsl.localhost\<distro>\a\b` or `\\wsl$\<distro>\a\b` → `/a/b`, for
    /// this distro only. The bare root maps to `/`. Anything else is `None`,
    /// which the router turns into a 400 rather than a launch elsewhere.
    pub fn from_unc(&self, unc: &str) -> Option<String> {
        let rest = unc.strip_prefix("\\\\")?;
        let (host, after_host) = rest.split_once('\\')?;
        if !SHARE_HOSTS.iter().any(|h| h.eq_ignore_ascii_case(host)) {
            return None;
        }
        let (distro, tail) = match after_host.split_once('\\') {
            Some((d, t)) => (d, t),
            None => (after_host, ""),
        };
        if !distro.eq_ignore_ascii_case(&self.distro) {
            return None;
        }
        let linux = format!("/{}", tail.replace('\\', "/"));
        Some(if linux.len() > 1 { linux.trim_end_matches('/').to_string() } else { linux })
    }
}

/// `ps -eo pid=,ppid=,%cpu=,rss=,comm=` → rows. `%cpu` is the process's
/// lifetime average, which is what the Node agent showed. Lines that do not
/// parse are skipped.
pub fn parse_ps(out: &str) -> Vec<ProcRow> {
    out.lines()
        .filter_map(|l| {
            let mut it = l.split_whitespace();
            let pid = it.next()?.parse::<i32>().ok()?;
            let ppid = it.next()?.parse::<i32>().ok()?;
            let cpu = it.next()?.parse::<f32>().ok()?;
            let rss_kb = it.next()?.parse::<u64>().ok()?;
            let name = it.collect::<Vec<_>>().join(" ");
            if name.is_empty() {
                return None;
            }
            Some(ProcRow { pid, ppid, name, cpu, rss_kb })
        })
        .collect()
}
```

- [ ] **Step 4: Run the tests, clippy, and the Windows type-check**

Run:
```bash
cargo test -p cdash-agent --locked host::wsl
cargo clippy -p cdash-agent --all-targets --locked -- -D warnings -D clippy::disallowed_types
cargo clippy --locked --target x86_64-pc-windows-gnu -p cdash-agent --all-targets -- -D warnings -D clippy::disallowed_types
```
Expected: 7 tests PASS; both clippy runs clean.

- [ ] **Step 5: Commit**

```bash
git add crates/agent/src/host/wsl.rs crates/agent/src/host/mod.rs
git commit -m "host: WSL bridge parsers — probe output, runner prefix, share paths, ps rows

Pure functions, tested on every host: the boot probe's two-line output, the
wsl.exe --exec prefix, UNC<->Linux conversion for the configured distro
under both share hosts, and the ps parser with comm last."
```

---

### Task 6: `Side`, path shapes and routing; `Ctx.sides`; `assert_path`

Spec §1 (sides), §4 (routing by path shape). No behaviour changes on Linux: the one native side is built by `Ctx::new`, and every existing call site still reads `ctx.claude_dir` and `ctx.runner`.

**Files:**
- Create: `crates/agent/src/collect/side.rs`
- Modify: `crates/agent/src/collect/mod.rs` (add `pub mod side;`)
- Modify: `crates/agent/src/collect/ctx.rs`
- Modify: `crates/agent/src/collect/validate.rs` (`assert_path`)

**Interfaces:**
- Consumes: `host::wsl::{WslPaths, WslProbe, wsl_prefix, parse_ps, PS_ARGS, MISSING_SCRIPT}` (Task 5); `Runner::with_prefix` (Task 3).
- Produces (all in `cdash_agent::collect::side`):
  - `enum Backend { Tmux, #[cfg(windows)] Console }`, `enum Procs { Sampler, #[cfg(windows)] Ps }` — both `Copy + PartialEq`.
  - `struct Side { label: &'static str, claude_dir: PathBuf, runner: Arc<Runner>, backend: Backend, procs: Procs, wsl: Option<WslPaths> }`
  - `Side::native(claude_dir: PathBuf, runner: Arc<Runner>) -> Side`; `#[cfg(windows)] Side::wsl(probe: &WslProbe, log: Arc<LogBuffer>) -> Option<Side>`
  - `Side::is_wsl(&self) -> bool`; `async fn proc_rows(&self, sampler: &Mutex<Sampler>) -> Vec<ProcRow>`; `fn tree_usage(&self, sampler: &Mutex<Sampler>, rows: &[ProcRow], pid: i32) -> SampledUsage`; `async fn wsl_missing(&self) -> Vec<String>`
  - `enum Shape { Drive, Unc, Posix }`, `shape_of(&str) -> Option<Shape>`, `enum Route { Native(String), Wsl(String) }`, `route_windows(Option<&WslPaths>, &str) -> Result<Route, BadRequest>`, `route_unix(&str) -> Result<Route, BadRequest>`, `side_for<'a>(&'a [Side], &str) -> Result<(&'a Side, String), BadRequest>`, `path_is_valid(&str) -> bool`
  - `Ctx.sides: Vec<Side>` (native first), `Ctx.wsl_poll_timed_out: AtomicBool`, `Ctx::native(&self) -> &Side`, `Ctx::wsl_paths(&self) -> Option<&WslPaths>`

- [ ] **Step 1: Create `side.rs` with its tests**

Create `crates/agent/src/collect/side.rs`:

```rust
//! One Claude Code installation the agent can see, and the routing that picks
//! one for a launch. Linux has one side. Windows has the native side and, when
//! the boot probe succeeds, a second side reached through `wsl.exe` and the
//! `\\wsl.localhost` share.

#[cfg(test)]
mod tests {
    use super::*;
    use crate::host::log::LogBuffer;

    fn ubuntu() -> WslPaths {
        WslPaths { unc_root: "\\\\wsl.localhost\\Ubuntu".into(), distro: "Ubuntu".into() }
    }

    #[test]
    fn shapes_are_string_checks() {
        assert_eq!(shape_of("C:\\Users\\u"), Some(Shape::Drive));
        assert_eq!(shape_of("d:/git"), Some(Shape::Drive));
        assert_eq!(shape_of("\\\\wsl.localhost\\Ubuntu\\home"), Some(Shape::Unc));
        assert_eq!(shape_of("/home/u"), Some(Shape::Posix));
        assert_eq!(shape_of("relative/x"), None);
        assert_eq!(shape_of(""), None);
        assert_eq!(shape_of("C:"), None, "a bare drive letter is not a directory");
    }

    #[test]
    fn the_windows_routing_table() {
        let w = ubuntu();
        assert_eq!(route_windows(Some(&w), "C:\\p").unwrap(), Route::Native("C:\\p".into()));
        assert_eq!(route_windows(None, "C:\\p").unwrap(), Route::Native("C:\\p".into()));
        assert_eq!(
            route_windows(Some(&w), "\\\\wsl.localhost\\Ubuntu\\home\\u").unwrap(),
            Route::Wsl("/home/u".into())
        );
        assert_eq!(route_windows(Some(&w), "/home/u").unwrap(), Route::Wsl("/home/u".into()));
        assert!(route_windows(None, "/home/u").is_err(), "a / path needs a WSL side");
        assert!(route_windows(None, "\\\\wsl.localhost\\Ubuntu\\home").is_err());
        assert!(route_windows(Some(&w), "\\\\wsl.localhost\\Debian\\home").is_err(), "another distro");
        assert!(route_windows(Some(&w), "\\\\server\\share").is_err());
        assert!(route_windows(Some(&w), "relative").is_err());
        let e = route_windows(None, "/x").unwrap_err();
        assert!(e.0.starts_with("bad path: /x"), "{e:?}");
    }

    #[test]
    fn the_unix_routing_table() {
        assert_eq!(route_unix("/home/u").unwrap(), Route::Native("/home/u".into()));
        assert!(route_unix("C:\\p").is_err());
        assert!(route_unix("\\\\wsl.localhost\\Ubuntu\\home").is_err());
        assert!(route_unix("relative").is_err());
    }

    #[test]
    fn side_for_picks_the_native_side_on_its_own_platform() {
        let log = Arc::new(LogBuffer::new());
        let runner = Arc::new(Runner::new(String::new(), log));
        let sides = vec![Side::native(PathBuf::from("/tmp/.claude"), runner)];
        let native_dir = if cfg!(windows) { "C:\\p" } else { "/p" };
        let (s, d) = side_for(&sides, native_dir).unwrap();
        assert_eq!(s.label, "native");
        assert!(!s.is_wsl());
        assert_eq!(d, native_dir);
        assert!(side_for(&sides, "relative").is_err());
    }

    #[test]
    fn a_favorite_path_has_one_of_the_platform_shapes() {
        assert!(path_is_valid("/home/u"));
        assert!(!path_is_valid("relative/x"));
        assert!(!path_is_valid(""));
        assert_eq!(path_is_valid("C:\\Users"), cfg!(windows));
    }
}
```

Add `pub mod side;` to `crates/agent/src/collect/mod.rs`.

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p cdash-agent --locked collect::side`
Expected: compile errors for every missing item.

- [ ] **Step 3: Implement the module above the tests**

Insert between the module doc and `#[cfg(test)]`:

```rust
use super::validate::BadRequest;
use crate::host::cmd::Runner;
use crate::host::proc::ProcRow;
use crate::host::sample::{SampledUsage, Sampler};
use crate::host::wsl::{WslPaths, MISSING_SCRIPT};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Backend {
    /// Sessions live in tmux: `list-panes`, `new-session`, `kill-session`.
    Tmux,
    /// Sessions live in their own console window; ownership goes by `--name`.
    #[cfg(windows)]
    Console,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Procs {
    /// `sysinfo` through the shared sampler: the kernel we run on.
    Sampler,
    /// One `ps` per poll through the side's runner: a kernel we do not.
    #[cfg(windows)]
    Ps,
}

pub struct Side {
    /// `"native"` or `"wsl"`: log keys and `/api/hostinfo`.
    pub label: &'static str,
    /// `~/.claude`, `C:\Users\u\.claude`, or `\\wsl.localhost\Ubuntu\home\u\.claude`.
    pub claude_dir: PathBuf,
    /// Native, or `wsl.exe`-prefixed (Task 3).
    pub runner: Arc<Runner>,
    pub backend: Backend,
    pub procs: Procs,
    /// Set on WSL sides only: the share root and the distro name.
    pub wsl: Option<WslPaths>,
}

impl Side {
    pub fn native(claude_dir: PathBuf, runner: Arc<Runner>) -> Self {
        Side {
            label: "native",
            claude_dir,
            runner,
            #[cfg(windows)]
            backend: Backend::Console,
            #[cfg(not(windows))]
            backend: Backend::Tmux,
            procs: Procs::Sampler,
            wsl: None,
        }
    }

    /// From a successful boot probe (Task 9). `None` only when the home line
    /// is not a share path this crate understands.
    #[cfg(windows)]
    pub fn wsl(
        probe: &crate::host::wsl::WslProbe,
        log: Arc<crate::host::log::LogBuffer>,
    ) -> Option<Self> {
        let paths = WslPaths::from_home_unc(&probe.home_unc)?;
        let prefix = crate::host::wsl::wsl_prefix(probe.distro_flag.as_deref(), &probe.path);
        // The runner's own PATH stays the Windows one: it is what finds wsl.exe.
        let runner = Runner::with_prefix(
            prefix,
            "wsl ",
            std::env::var("PATH").unwrap_or_default(),
            log,
        );
        Some(Side {
            label: "wsl",
            claude_dir: PathBuf::from(format!("{}\\.claude", probe.home_unc)),
            runner: Arc::new(runner),
            backend: Backend::Tmux,
            procs: Procs::Ps,
            wsl: Some(paths),
        })
    }

    pub fn is_wsl(&self) -> bool {
        self.wsl.is_some()
    }

    /// The process rows this side's sessions are checked against, once per poll.
    pub async fn proc_rows(&self, sampler: &Mutex<Sampler>) -> Vec<ProcRow> {
        match self.procs {
            Procs::Sampler => sampler.lock().unwrap_or_else(|e| e.into_inner()).sample(),
            #[cfg(windows)]
            Procs::Ps => crate::host::wsl::parse_ps(
                &self.runner.run("ps", crate::host::wsl::PS_ARGS, "wsl ps").await,
            ),
        }
    }

    /// CPU and RSS of the tree under `pid`. The `ps` figure is a lifetime
    /// average, reported as `Some` with no sample age, as the Node agent did.
    #[cfg_attr(not(windows), allow(unused_variables))]
    pub fn tree_usage(&self, sampler: &Mutex<Sampler>, rows: &[ProcRow], pid: i32) -> SampledUsage {
        match self.procs {
            Procs::Sampler => sampler.lock().unwrap_or_else(|e| e.into_inner()).tree_usage(pid),
            #[cfg(windows)]
            Procs::Ps => {
                let u = crate::host::proc::proc_tree_usage(rows, pid);
                SampledUsage { cpu: Some(u.cpu), rss_kb: u.rss_kb, cpu_sample_age_ms: 0 }
            }
        }
    }

    /// Binaries this side lacks, re-probed per call like the native list.
    pub async fn wsl_missing(&self) -> Vec<String> {
        self.runner
            .run("sh", &["-c", MISSING_SCRIPT], "wsl missing")
            .await
            .lines()
            .map(str::to_string)
            .collect()
    }
}

/// The three shapes a directory can arrive in. String checks, not
/// `Path::is_absolute`: on Windows `/x` is not absolute, yet it is exactly how
/// a WSL directory is named.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Shape {
    /// `C:\…` or `C:/…`
    Drive,
    /// `\\host\…`
    Unc,
    /// `/…`
    Posix,
}

pub fn shape_of(p: &str) -> Option<Shape> {
    let b = p.as_bytes();
    if b.len() >= 3 && b[0].is_ascii_alphabetic() && b[1] == b':' && (b[2] == b'\\' || b[2] == b'/') {
        Some(Shape::Drive)
    } else if b.len() > 2 && p.starts_with("\\\\") {
        Some(Shape::Unc)
    } else if p.starts_with('/') {
        Some(Shape::Posix)
    } else {
        None
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Route {
    /// The native side, directory as given.
    Native(String),
    /// The WSL side, directory in Linux notation.
    Wsl(String),
}

fn bad(p: &str, why: &str) -> BadRequest {
    BadRequest(format!("bad path: {p} ({why})"))
}

/// Spec §4, the Windows column. Compiled everywhere so the table is tested
/// everywhere; `side_for` selects the platform's own.
pub fn route_windows(wsl: Option<&WslPaths>, dir: &str) -> Result<Route, BadRequest> {
    match shape_of(dir) {
        Some(Shape::Drive) => Ok(Route::Native(dir.to_string())),
        Some(Shape::Unc) => {
            let w = wsl.ok_or_else(|| bad(dir, "no WSL side"))?;
            w.from_unc(dir).map(Route::Wsl).ok_or_else(|| bad(dir, "not the configured distro"))
        }
        Some(Shape::Posix) => {
            wsl.ok_or_else(|| bad(dir, "no WSL side"))?;
            Ok(Route::Wsl(dir.to_string()))
        }
        None => Err(bad(dir, "not absolute")),
    }
}

/// Spec §4, the Unix column.
pub fn route_unix(dir: &str) -> Result<Route, BadRequest> {
    match shape_of(dir) {
        Some(Shape::Posix) => Ok(Route::Native(dir.to_string())),
        _ => Err(BadRequest(format!("bad path: {dir}"))),
    }
}

/// The side a launch lands on and the directory in that side's notation.
/// `sides[0]` is always the native side (`Ctx::new` guarantees one).
pub fn side_for<'a>(sides: &'a [Side], dir: &str) -> Result<(&'a Side, String), BadRequest> {
    #[cfg(windows)]
    let route = route_windows(sides.iter().find_map(|s| s.wsl.as_ref()), dir)?;
    #[cfg(not(windows))]
    let route = route_unix(dir)?;
    match route {
        Route::Native(d) => Ok((&sides[0], d)),
        Route::Wsl(d) => sides
            .iter()
            .find(|s| s.is_wsl())
            .map(|s| (s, d))
            .ok_or_else(|| bad(dir, "no WSL side")),
    }
}

/// Shape only, for favourites: a path a launch could route is a path worth
/// remembering. Whether a side exists for it is a launch-time question.
pub fn path_is_valid(p: &str) -> bool {
    #[cfg(windows)]
    {
        shape_of(p).is_some()
    }
    #[cfg(not(windows))]
    {
        shape_of(p) == Some(Shape::Posix)
    }
}
```

- [ ] **Step 4: Give `Ctx` its sides**

In `crates/agent/src/collect/ctx.rs`, add the imports:

```rust
use super::side::Side;
use crate::host::wsl::WslPaths;
use std::sync::atomic::AtomicBool;
```

Add two fields to `pub struct Ctx` after `password`:

```rust
    /// Every Claude installation this agent can see. The native side is first
    /// and always present; a WSL side is appended at boot on Windows. Fixed
    /// once `Ctx` is shared.
    pub sides: Vec<Side>,
    /// Set the first time the WSL side's poll time-box fires, so it is logged
    /// once and not every four seconds.
    pub wsl_poll_timed_out: AtomicBool,
```

Replace the body of `Ctx::new` with (note `claude_dir` is moved last):

```rust
        // `Host` owns a `Runner` too, but not behind an `Arc`, and the git
        // cache's background task needs one. Same resolved PATH, same log
        // buffer; only the log-once set is per-runner.
        let runner = Arc::new(Runner::new(host.path.clone(), Arc::clone(&host.log)));
        Self {
            places_file: claude_dir.join("cdash-places.json"),
            sides: vec![Side::native(claude_dir.clone(), Arc::clone(&runner))],
            host,
            runner,
            claude_dir,
            disk_extra,
            meta: Mutex::new(HashMap::new()),
            purged: Mutex::new(HashSet::new()),
            transcripts: TranscriptCache::new(),
            git: Arc::new(GitCache::new()),
            password: std::sync::OnceLock::new(),
            wsl_poll_timed_out: AtomicBool::new(false),
        }
```

Add to `impl Ctx`:

```rust
    pub fn native(&self) -> &Side {
        &self.sides[0]
    }

    pub fn wsl_paths(&self) -> Option<&WslPaths> {
        self.sides.iter().find_map(|s| s.wsl.as_ref())
    }
```

- [ ] **Step 5: Route `assert_path` through the shape rules**

In `crates/agent/src/collect/validate.rs`, replace `assert_path`:

```rust
/// Mirrors `assertPath` (`server.js:28-30`), extended to the platform's path
/// shapes (spec §4): a drive, a `\\wsl…` share or a `/` path on Windows, `/`
/// only elsewhere. Shape only — routing to a side is `side::side_for`.
pub fn assert_path(p: &str) -> Result<(), BadRequest> {
    if super::side::path_is_valid(p) {
        Ok(())
    } else {
        Err(BadRequest(format!("bad path: {p}")))
    }
}
```

- [ ] **Step 6: Run the tests, clippy, and the Windows type-check**

Run:
```bash
cargo test -p cdash-agent --locked
cargo clippy -p cdash-agent --all-targets --locked -- -D warnings -D clippy::disallowed_types
cargo clippy --locked --target x86_64-pc-windows-gnu -p cdash-agent --all-targets -- -D warnings -D clippy::disallowed_types
```
Expected: all PASS including the five new `collect::side` tests and the unchanged `validate` tests; both clippy runs clean.

- [ ] **Step 7: Commit**

```bash
git add crates/agent/src/collect/side.rs crates/agent/src/collect/mod.rs crates/agent/src/collect/ctx.rs crates/agent/src/collect/validate.rs
git commit -m "collect: a Side per Claude installation, path-shape routing, Ctx.sides

A side carries its Claude directory, runner, process source and session
backend. Linux has one; Windows appends a WSL side at boot. Launch routing is
two platform-agnostic tables selected by cfg, so both are tested on every
host, and assert_path accepts the platform's shapes rather than
Path::is_absolute, which rejects /x on Windows."
```

---

### Task 7: Session-file ownership and per-side collection

Spec §1 (per-side loop, resumable merge, WSL time-box), §3 (ownership by `--name`).

**Files:**
- Modify: `crates/agent/src/collect/external.rs`
- Modify: `crates/agent/src/collect/sessions.rs`

**Interfaces:**
- Consumes: `Side`, `Backend`, `Ctx.sides`, `Ctx.wsl_poll_timed_out` (Task 6); `root_mount` (Task 4); `DEFAULT_TIMEOUT` (`host::cmd`).
- Produces (in `collect::external`):
  - `pub async fn live_session_files(side: &Side, rows: &[ProcRow], exclude: &HashSet<i32>) -> Vec<(i32, SessionFile)>` — the liveness predicate: pid live as `claude`/`claude.exe`, `entrypoint == "cli"`, not excluded. Kill reuses it in Task 8.
  - `pub async fn file_sessions(ctx: &Arc<Ctx>, side: &Side, rows: &[ProcRow], pane_pids: &HashSet<i32>, now_ms: f64) -> Vec<Session>` — replaces `external_sessions`; a file whose `name` starts with `cdash-` is ours (`external: false`, meta applied), any other is external.
- `collect_sessions(ctx)` keeps its signature.

- [ ] **Step 1: Write the failing ownership tests in `external.rs`**

In `crates/agent/src/collect/external.rs` `mod tests`, change the import line `use super::*;` block to also bring `Meta`:

```rust
    use super::*;
    use crate::collect::ctx::Meta;
    use crate::host::log::LogBuffer;
    use std::path::PathBuf;
```

Replace every `external_sessions(&ctx_for(d), …)` call in the existing tests with the two-line form, for example:

```rust
        let ctx = ctx_for(d);
        let out = file_sessions(&ctx, ctx.native(), &rows(&[500]), &HashSet::new(), 61_000.0).await;
```

(`a_missing_sessions_directory_yields_an_empty_list` passes `dir` rather than `d`; same change.) Then add:

```rust
    #[tokio::test]
    async fn a_cdash_named_session_is_ours_and_carries_its_meta() {
        // Without tmux there is no session name to match: the `--name` the
        // launcher passes is the only durable mark of a session we started.
        let d = tempdir("ours");
        write_session(
            &d,
            508,
            r#"{"sessionId":"s-8","cwd":"/proj","entrypoint":"cli","name":"cdash-proj-1200-abc","bridgeSessionId":"session_ours"}"#,
        );
        let ctx = ctx_for(d);
        ctx.meta_set(
            "cdash-proj-1200-abc",
            Meta { model: Some("opus".into()), effort: Some("high".into()), rc_link: None },
        );
        let out = file_sessions(&ctx, ctx.native(), &rows(&[508]), &HashSet::new(), 0.0).await;
        assert_eq!(out.len(), 1);
        assert!(!out[0].external, "a cdash- name is a session this dashboard launched");
        assert_eq!(out[0].name, "cdash-proj-1200-abc");
        assert_eq!(out[0].model.as_deref(), Some("opus"));
        assert_eq!(out[0].rc_link.as_deref(), Some("https://claude.ai/code/session_ours"));
    }

    #[tokio::test]
    async fn live_session_files_applies_the_liveness_predicate() {
        // Kill reuses this: a stale file whose pid was recycled must never
        // match, or taskkill would reach a foreign process.
        let d = tempdir("live");
        write_session(&d, 600, CLI);
        write_session(&d, 601, CLI);
        write_session(&d, 602, r#"{"sessionId":"s","cwd":"/p","entrypoint":"sdk-cli"}"#);
        let ctx = ctx_for(d);
        let mut live = rows(&[600, 602]);
        live.push(ProcRow { pid: 601, ppid: 1, name: "bash".into(), cpu: 0.0, rss_kb: 1 });
        let files = live_session_files(ctx.native(), &live, &HashSet::new()).await;
        let pids: Vec<i32> = files.iter().map(|(p, _)| *p).collect();
        assert_eq!(pids, vec![600], "601 is a recycled pid, 602 is not a cli session");
    }
```

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test -p cdash-agent --locked collect::external`
Expected: compile errors — `file_sessions` and `live_session_files` do not exist.

- [ ] **Step 3: Replace the non-test body of `external.rs`**

Replace the imports and everything from `fn basename` through the end of `external_sessions` with:

```rust
use super::ctx::{Ctx, Meta};
use super::fsio::read_tail;
use super::lookup::{session_file_for, SessionFile};
use super::side::Side;
use crate::host::proc::ProcRow;
use crate::parse::git::{parse_git_status, GitStatus};
use crate::parse::paths::project_dir_name;
use crate::parse::transcript::parse_transcript;
use serde::Serialize;
use std::collections::HashSet;
use std::path::Path;
use std::sync::Arc;
```

(keep the `Session` struct exactly as it is, then:)

```rust
fn basename(p: &str) -> String {
    Path::new(p)
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| p.to_string())
}

/// Every `<claude_dir>/sessions/<pid>.json` whose pid is a live `claude`
/// process with a `cli` entrypoint, minus `exclude`. This is the one liveness
/// predicate: the list uses it to show sessions, kill uses it to refuse a
/// recycled pid. Session files outlive their process.
pub async fn live_session_files(
    side: &Side,
    rows: &[ProcRow],
    exclude: &HashSet<i32>,
) -> Vec<(i32, SessionFile)> {
    let alive: HashSet<i32> = rows
        .iter()
        .filter(|r| r.name == "claude" || r.name == "claude.exe")
        .map(|r| r.pid)
        .collect();
    let dir = side.claude_dir.join("sessions");
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
        if exclude.contains(&pid) || !alive.contains(&pid) {
            continue;
        }
        let Some(sess) = session_file_for(&side.claude_dir, pid).await else { continue };
        // E1: 'cli' is a session someone is sitting in front of; 'sdk-cli' is
        // programmatic and not ours to show.
        if sess.entrypoint.as_deref() != Some("cli") {
            continue;
        }
        out.push((pid, sess));
    }
    out
}

/// Sessions known from their session files: ours when the `name` carries the
/// `cdash-` prefix the launcher sets, external otherwise. On a tmux side the
/// pane pids are excluded first, so a tmux session is never listed twice.
pub async fn file_sessions(
    ctx: &Arc<Ctx>,
    side: &Side,
    rows: &[ProcRow],
    pane_pids: &HashSet<i32>,
    now_ms: f64,
) -> Vec<Session> {
    let mut out = Vec::new();
    for (pid, sess) in live_session_files(side, rows, pane_pids).await {
        let (Some(sid), Some(cwd)) = (sess.session_id.clone(), sess.cwd.clone()) else { continue };
        let name = sess.name.clone().filter(|n| !n.is_empty());
        let ours = name.as_deref().is_some_and(|n| n.starts_with("cdash-"));
        let meta = match &name {
            Some(n) if ours => ctx.meta_get(n).unwrap_or_default(),
            _ => Meta::default(),
        };

        let file = side
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

        let usage = side.tree_usage(&ctx.host.sampler, rows, pid);
        let git_out =
            Arc::clone(&ctx.git).status_for(Arc::clone(&side.runner), &cwd, now_ms as u64);
        let rc_link = meta.rc_link.clone().or_else(|| {
            sess.bridge_session_id
                .clone()
                .map(|id| format!("https://claude.ai/code/{id}"))
        });

        out.push(Session {
            name: name.unwrap_or_else(|| basename(&cwd)),
            dir: cwd,
            pid,
            uptime_sec: sess
                .started_at
                .map(|t| (((now_ms - t) / 1000.0).round() as i64).max(0))
                .unwrap_or(0),
            model: meta.model,
            effort: meta.effort,
            rc_link,
            git: git_out.as_deref().map(parse_git_status),
            working,
            last_message,
            sid: Some(sid),
            cpu: usage.cpu,
            rss_kb: usage.rss_kb,
            cpu_sample_age_ms: usage.cpu_sample_age_ms,
            external: !ours,
        });
    }
    out
}
```

- [ ] **Step 4: Run the external tests**

Run: `cargo test -p cdash-agent --locked collect::external`
Expected: all PASS, including the two new ones and the eleven rewritten ones.

- [ ] **Step 5: Write the failing merge test in `sessions.rs`**

In `crates/agent/src/collect/sessions.rs` `mod tests`, add:

```rust
    #[tokio::test]
    async fn resumable_rows_from_every_side_are_merged_newest_first() {
        // Two sides here are two native sides pointing at two Claude dirs —
        // the merge does not care what a side is, only what its history says.
        let d1 = tempdir("merge-1");
        let d2 = tempdir("merge-2");
        let a = seed(&d1, "aaa", "/p/a", 100, 3);
        let b = seed(&d2, "bbb", "/p/b", 300, 3);
        std::fs::write(d1.join("history.jsonl"), a).unwrap();
        std::fs::write(d2.join("history.jsonl"), b).unwrap();

        let log = Arc::new(LogBuffer::new());
        let path = std::env::var("PATH").unwrap_or_default();
        let host = crate::host::init::Host {
            runner: crate::host::cmd::Runner::new(path.clone(), Arc::clone(&log)),
            log,
            path,
            sampler: std::sync::Mutex::new(crate::host::sample::Sampler::new()),
        };
        let mut c = Ctx::new(host, d1, None);
        c.sides.push(crate::collect::side::Side::native(d2, Arc::clone(&c.runner)));

        let r = collect_sessions(&Arc::new(c)).await;
        let sids: Vec<&str> = r.resumable.iter().map(|x| x.sid.as_str()).collect();
        assert_eq!(sids, vec!["bbb", "aaa"], "the second side's newer row sorts first");
    }
```

Also move the tmux-stub tests into a Unix-only nested module. Inside `mod tests`, wrap `fake_tmux`, `ctx_with_path`, `a_pane_becomes_a_running_session_carrying_its_rc_link` and `a_link_discovered_from_the_session_file_is_memoized_into_meta` in:

```rust
    #[cfg(unix)]
    mod tmux_tests {
        use super::*;
        // … the four items, unchanged …
    }
```

- [ ] **Step 6: Run to verify the merge test fails**

Run: `cargo test -p cdash-agent --locked merged_newest_first`
Expected: FAIL — only `aaa` is listed; the second side is ignored.

- [ ] **Step 7: Rewrite `collect_sessions` around a per-side function**

In `crates/agent/src/collect/sessions.rs`, replace the imports with:

```rust
use super::ctx::{Ctx, Meta};
use super::external::{file_sessions, Session};
use super::fsio::{read_if, read_tail};
use super::lookup::{session_file_for, transcript_for};
use super::side::{Backend, Side};
use crate::host::cmd::DEFAULT_TIMEOUT;
use crate::host::disk::{disk_usage, root_mount, DiskUsage};
use crate::parse::git::parse_git_status;
use crate::parse::history::group_history;
use crate::parse::paths::project_dir_name;
use crate::parse::tmux::{parse_tmux_panes, PANE_FORMAT};
use crate::parse::transcript::parse_transcript;
use serde::Serialize;
use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::sync::Arc;
```

Replace `collect_sessions` entirely with:

```rust
/// One side's running and resumable sessions. On a tmux side the pane loop
/// runs first and its pids are excluded from the session-file scan; on a
/// console side the scan is the whole list.
async fn collect_side(ctx: &Arc<Ctx>, side: &Side, now: f64) -> (Vec<Session>, Vec<Resumable>) {
    // One sample serves every session in this response; the 200 ms rule lives
    // inside `Sampler` and must not be re-implemented here.
    let rows = side.proc_rows(&ctx.host.sampler).await;
    let mut running: Vec<Session> = Vec::new();
    let mut pane_pids: HashSet<i32> = HashSet::new();

    if matches!(side.backend, Backend::Tmux) {
        let panes_out = side
            .runner
            .run("tmux", &["list-panes", "-a", "-F", PANE_FORMAT], "tmux list-panes")
            .await;
        for p in parse_tmux_panes(&panes_out) {
            pane_pids.insert(p.pid);
            let meta = ctx.meta_get(&p.name).unwrap_or_default();
            let sess = session_file_for(&side.claude_dir, p.pid).await;

            let rc_link = meta.rc_link.clone().or_else(|| {
                sess.as_ref()
                    .and_then(|s| s.bridge_session_id.clone())
                    .map(|id| format!("https://claude.ai/code/{id}"))
            });
            // D9: memoize a link discovered from the session file, so a later
            // poll does not have to rediscover it.
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
                let file = side
                    .claude_dir
                    .join("projects")
                    .join(project_dir_name(&cwd))
                    .join(format!("{sid}.jsonl"));
                if let Ok(md) = tokio::fs::metadata(&file).await {
                    tr = Some((file, super::cache::mtime_ms(&md)));
                }
            }
            if tr.is_none() {
                tr = transcript_for(&side.claude_dir, &p.path, p.created).await;
            }

            let (mut working, mut last_message, mut sid) = (false, None, None);
            if let Some((file, mtime)) = &tr {
                working = now - mtime < 10_000.0;
                sid = file.file_stem().map(|s| s.to_string_lossy().into_owned());
                if let Some(txt) = read_tail(file).await {
                    last_message = parse_transcript(&txt).last_assistant_text;
                }
            }

            let usage = side.tree_usage(&ctx.host.sampler, &rows, p.pid);
            let git_out = Arc::clone(&ctx.git).status_for(
                Arc::clone(&side.runner),
                &p.path,
                now as u64,
            );

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
                cpu: usage.cpu,
                rss_kb: usage.rss_kb,
                cpu_sample_age_ms: usage.cpu_sample_age_ms,
                external: false,
            });
        }
    }

    running.extend(file_sessions(ctx, side, &rows, &pane_pids, now).await);

    let running_sids: HashSet<String> = running.iter().filter_map(|r| r.sid.clone()).collect();
    let hist = read_if(&side.claude_dir.join("history.jsonl")).await.unwrap_or_default();

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
        let file = side
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
    (running, resumable)
}

pub async fn collect_sessions(ctx: &Arc<Ctx>) -> SessionsResponse {
    let now = now_ms();
    let mut running: Vec<Session> = Vec::new();
    let mut resumable: Vec<Resumable> = Vec::new();

    for side in &ctx.sides {
        let part = if side.is_wsl() {
            // `tokio::fs` over the share has no time-box of its own, and the
            // 5-second rule exists because one 9P stall once froze every poll
            // for a minute. On expiry the side is empty for this poll.
            match tokio::time::timeout(DEFAULT_TIMEOUT, collect_side(ctx, side, now)).await {
                Ok(p) => p,
                Err(_) => {
                    if !ctx.wsl_poll_timed_out.swap(true, Ordering::Relaxed) {
                        ctx.host.log.push(format!(
                            "wsl poll timed out after {}ms; WSL side empty until it answers",
                            DEFAULT_TIMEOUT.as_millis()
                        ));
                    }
                    (Vec::new(), Vec::new())
                }
            }
        } else {
            collect_side(ctx, side, now).await
        };
        running.extend(part.0);
        resumable.extend(part.1);
    }

    // Each side capped itself; the merge is sorted and capped again.
    resumable.sort_by(|a, b| b.ts.partial_cmp(&a.ts).unwrap_or(std::cmp::Ordering::Equal));
    resumable.truncate(RESUMABLE_MAX);

    let machine = {
        let mut s = ctx.host.sampler.lock().unwrap_or_else(|e| e.into_inner());
        s.machine_stats()
    };
    let mut disks: Vec<DiskUsage> = Vec::new();
    if let Some(d) = disk_usage(&root_mount()) {
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

- [ ] **Step 8: Run the tests, clippy, and the Windows type-check**

Run:
```bash
cargo test -p cdash-agent --locked
cargo clippy -p cdash-agent --all-targets --locked -- -D warnings -D clippy::disallowed_types
cargo clippy --locked --target x86_64-pc-windows-gnu -p cdash-agent --all-targets -- -D warnings -D clippy::disallowed_types
```
Expected: all PASS (the merge test now lists `bbb, aaa`); both clippy runs clean. If clippy flags `too_many_arguments` or `type_complexity`, it will not — the signatures above are within its defaults.

- [ ] **Step 9: Commit**

```bash
git add crates/agent/src/collect/external.rs crates/agent/src/collect/sessions.rs
git commit -m "collect: per-side collection, ownership by session name, WSL time-box

The session-file scan now yields our own sessions too — a cdash- name is
the launcher's mark where there is no tmux session to match — and its
liveness predicate is a function of its own so kill can reuse it.
collect_sessions runs the same loop per side, time-boxes the WSL side, and
merges the resumable lists newest first."
```

---

### Task 8: Side-aware launch, resume and kill; RC poll by name; the recents rule

Spec §3 (launcher, ownership, RC poll, kill), §4 (routing, resume, trust dialog, recents).

**Files:**
- Modify: `crates/agent/src/collect/spawn.rs`
- Modify: `crates/agent/src/http/routes.rs` (`post_launch`, one test)

**Interfaces:**
- Consumes: `side_for`, `Side`, `Backend` (Task 6); `live_session_files` (Task 7); `Runner::spawn_detached` (Task 3); `claude_json_path` (Task 1).
- Produces (in `collect::spawn`):
  - `pub const SETTINGS_JSON: &str`
  - `pub enum RcLocator { ByPid(i32), ByName }`
  - `pub async fn rc_link_by_name(claude_dir: &Path, name: &str) -> Option<String>`
  - `pub async fn poll_rc_link(ctx: Arc<Ctx>, claude_dir: PathBuf, name: String, locator: RcLocator, attempts: u32, interval: Duration)`
  - `pub struct Launched { pub name: String, pub native: bool }`; `launch_session(ctx, dir, model, effort) -> Result<Launched, Refused>`
  - `resume_session(ctx, sid) -> Result<String, Refused>` and `kill_session(ctx, name) -> Result<(), Refused>` keep their signatures.

- [ ] **Step 1: Write the failing tests in `spawn.rs`**

In `crates/agent/src/collect/spawn.rs` `mod tests`, first update the three existing `poll_rc_link` callers to the new signature — each becomes:

```rust
        poll_rc_link(Arc::clone(&ctx), d.clone(), "cdash-x".into(), RcLocator::ByPid(99), 3, Duration::from_millis(10)).await;
```
(`the_rc_poll_writes_the_link_it_finds` with pid 99, `a_session_killed_during_the_poll_is_not_resurrected` with pid 98 and name `cdash-dead`, `the_poll_gives_up_after_its_attempt_budget` with pid 1 and name `cdash-y`; in the last one `d` is not cloned today — change `let ctx = ctx_for(d).await;` to `let ctx = ctx_for(d.clone()).await;`.)

Then add:

```rust
    #[tokio::test]
    async fn the_rc_poll_can_find_the_session_by_name() {
        // A console launch knows no pid: the session file is found by the
        // `--name` the launcher passed, not by a pid we never learned.
        let d = tempdir("rc-name");
        let ctx = ctx_for(d.clone()).await;
        ctx.meta_set("cdash-byname", Meta::default());
        tokio::fs::write(
            d.join("sessions/77.json"),
            r#"{"name":"cdash-byname","bridgeSessionId":"session_named"}"#,
        )
        .await
        .unwrap();
        tokio::fs::write(
            d.join("sessions/78.json"),
            r#"{"name":"other","bridgeSessionId":"session_other"}"#,
        )
        .await
        .unwrap();

        poll_rc_link(Arc::clone(&ctx), d.clone(), "cdash-byname".into(), RcLocator::ByName, 3, Duration::from_millis(10)).await;

        assert_eq!(
            ctx.meta_get("cdash-byname").unwrap().rc_link.as_deref(),
            Some("https://claude.ai/code/session_named")
        );
    }
```

Move `stub_tmux`, `ctx_with_tmux`, `resume_un_purges_the_session_it_is_bringing_back`, `kill_forgets_the_session_meta` and `a_kill_that_failed_is_reported_and_keeps_the_session` into a nested Unix-only module inside `mod tests`, and add the cross-side resume test to it:

```rust
    #[cfg(unix)]
    mod tmux_tests {
        use super::*;

        // … stub_tmux, ctx_with_tmux and the three tests, unchanged …

        #[tokio::test]
        async fn resume_finds_the_sid_in_a_later_sides_history_and_names_the_session() {
            // Two native sides stand in for native + WSL: the sid lives only
            // in the second side's history, so that is where the spawn goes.
            let d1 = tempdir("resume-side-1");
            let d2 = tempdir("resume-side-2");
            let path = stub_tmux(&d1, 0);
            let log = Arc::new(LogBuffer::new());
            let host = crate::host::init::Host {
                runner: Runner::new(path.clone(), Arc::clone(&log)),
                log,
                path,
                sampler: std::sync::Mutex::new(crate::host::sample::Sampler::new()),
            };
            let mut c = Ctx::new(host, d1.clone(), None);
            c.sides.push(crate::collect::side::Side::native(d2.clone(), Arc::clone(&c.runner)));
            let ctx = Arc::new(c);

            let sid = "3f8a1c94-3b7e-4d51-9a02-6c5f8e1b7d34";
            tokio::fs::write(d1.join("history.jsonl"), "").await.unwrap();
            tokio::fs::write(
                d2.join("history.jsonl"),
                format!("{{\"sessionId\":\"{sid}\",\"project\":\"/tmp\",\"timestamp\":1,\"display\":\"x\"}}\n"),
            )
            .await
            .unwrap();

            let name = resume_session(&ctx, sid).await.unwrap();

            let args = std::fs::read_to_string(d1.join("tmux-args")).unwrap();
            assert!(args.contains(&format!("--resume {sid}")), "{args}");
            assert!(args.contains(&format!("--name {name}")), "the session file will carry our name: {args}");
            assert!(args.contains("-c /tmp"), "{args}");
        }
    }
```

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test -p cdash-agent --locked collect::spawn`
Expected: compile errors — `RcLocator` does not exist and `poll_rc_link` has the old arity.

- [ ] **Step 3: Rewrite the non-test body of `spawn.rs`**

Replace everything from the imports through the end of `kill_session` (keep `trust_dir`, `claude_json_path`, `tmux_name`, `purge_session` and the two `RC_POLL_*` constants as they are) with the following, in this order:

```rust
use super::ctx::{Ctx, Meta};
#[cfg(windows)]
use super::external::live_session_files;
use super::fsio::{read_if, write_atomic};
use super::lookup::{rc_link_for, SessionFile};
use super::side::{side_for, Backend, Side};
use super::validate::{
    assert_effort, assert_kill_name, assert_model, assert_valid_sid, BadRequest, Refused,
};
use crate::parse::history::group_history;
#[cfg(windows)]
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// 60 × 500 ms = the 30 s budget from `lib/collect.js:143`.
pub const RC_POLL_ATTEMPTS: u32 = 60;
pub const RC_POLL_INTERVAL: Duration = Duration::from_millis(500);

/// The explicit Remote Control request must win over this user-level opt-out,
/// but only for the dashboard child; the user's settings file stays untouched.
pub const SETTINGS_JSON: &str = r#"{"env":{"CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC":""}}"#;
```

(then the unchanged `trust_dir`, `claude_json_path`, `tmux_name`), then:

```rust
/// How the RC-link poll finds the session file: by the pane pid tmux reported,
/// or, where nothing reported a pid, by the `--name` the launcher passed.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum RcLocator {
    ByPid(i32),
    ByName,
}

/// The RC link of the session file whose `name` is `name`, if it has one yet.
pub async fn rc_link_by_name(claude_dir: &Path, name: &str) -> Option<String> {
    let mut entries = tokio::fs::read_dir(claude_dir.join("sessions")).await.ok()?;
    while let Ok(Some(e)) = entries.next_entry().await {
        let p = e.path();
        if p.extension().and_then(|x| x.to_str()) != Some("json") {
            continue;
        }
        let Some(txt) = read_if(&p).await else { continue };
        let Ok(sess) = serde_json::from_str::<SessionFile>(&txt) else { continue };
        if sess.name.as_deref() == Some(name) {
            if let Some(id) = sess.bridge_session_id {
                return Some(format!("https://claude.ai/code/{id}"));
            }
        }
    }
    None
}

/// Wait for `claude` to publish its remote-control session id, then record the
/// link. Two guards, both required: the session can be killed while this
/// sleeps, and again between the read and the write.
pub async fn poll_rc_link(
    ctx: Arc<Ctx>,
    claude_dir: PathBuf,
    name: String,
    locator: RcLocator,
    attempts: u32,
    interval: Duration,
) {
    for i in 0..attempts {
        tokio::time::sleep(interval).await;
        if !ctx.meta_has(&name) {
            return; // killed while polling
        }
        let found = match locator {
            RcLocator::ByPid(pid) => rc_link_for(&claude_dir, pid).await,
            RcLocator::ByName => rc_link_by_name(&claude_dir, &name).await,
        };
        if let Some(link) = found {
            if !ctx.meta_update(&name, |m| m.rc_link = Some(link.clone())) {
                return; // killed between the check and the write
            }
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

/// Start `claude` on one side. The argv is the same everywhere; the backend
/// decides what wraps it: a detached tmux session, or its own console window.
/// `--name` makes the session file carry the same name on every backend.
async fn spawn_claude(
    ctx: &Arc<Ctx>,
    side: &Side,
    dir: &str,
    claude_args: &[&str],
    meta: Meta,
) -> Result<String, Refused> {
    if let Err(e) = trust_dir(&claude_json_path(&side.claude_dir), dir).await {
        ctx.host.log.push(format!("trust write failed for {dir}: {e}"));
    }
    let name = tmux_name(dir);

    let mut argv: Vec<&str> = vec!["claude", "--settings", SETTINGS_JSON];
    argv.extend_from_slice(claude_args);
    argv.extend_from_slice(&[
        "--dangerously-skip-permissions",
        "--remote-control",
        &name,
        "--name",
        &name,
    ]);

    match side.backend {
        Backend::Tmux => {
            let mut args: Vec<&str> = vec!["new-session", "-d", "-s", &name, "-c", dir];
            args.extend_from_slice(&argv);
            // Checked: without this a failed `tmux new-session` still answered
            // 200 with a session name, and left a meta entry for a session
            // that does not exist.
            side.runner
                .run_checked("tmux", &args, "tmux new-session")
                .await
                .map_err(Refused::Failed)?;
        }
        #[cfg(windows)]
        Backend::Console => {
            side.runner
                .spawn_detached(argv[0], &argv[1..], dir, "claude spawn")
                .map_err(Refused::Failed)?;
        }
    }

    ctx.meta_set(&name, meta);
    ctx.host.log.push(format!(
        "launch {} → {name}",
        Path::new(dir)
            .file_name()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default()
    ));

    // The pid lookup stays best-effort — losing it costs the rc-link poll,
    // not the session. A console launch never has one and polls by name.
    let locator = match side.backend {
        Backend::Tmux => {
            let pid_out = side
                .runner
                .run(
                    "tmux",
                    &["display-message", "-p", "-t", &name, "#{pane_pid}"],
                    "tmux display-message",
                )
                .await;
            match pid_out.trim().parse::<i32>() {
                Ok(pid) => Some(RcLocator::ByPid(pid)),
                Err(_) => {
                    ctx.host.log.push(format!("rc-poll skipped {name}: no pane pid"));
                    None
                }
            }
        }
        #[cfg(windows)]
        Backend::Console => Some(RcLocator::ByName),
    };
    if let Some(locator) = locator {
        tokio::spawn(poll_rc_link(
            Arc::clone(ctx),
            side.claude_dir.clone(),
            name.clone(),
            locator,
            RC_POLL_ATTEMPTS,
            RC_POLL_INTERVAL,
        ));
    }

    Ok(name)
}

/// What a launch reports back: the session name, and whether it landed on the
/// native side — the recents entry is resolved only for native paths.
pub struct Launched {
    pub name: String,
    pub native: bool,
}

pub async fn launch_session(
    ctx: &Arc<Ctx>,
    dir: &str,
    model: &str,
    effort: &str,
) -> Result<Launched, Refused> {
    assert_model(model)?;
    assert_effort(effort)?;
    let (side, dir) = side_for(&ctx.sides, dir)?;
    // A6: the directory must exist and be a directory before it reaches `-c`.
    // A WSL directory is checked through the share.
    let check = match &side.wsl {
        Some(w) => PathBuf::from(w.to_unc(&dir)),
        None => PathBuf::from(&dir),
    };
    let is_dir = tokio::fs::metadata(&check).await.map(|m| m.is_dir()).unwrap_or(false);
    if !is_dir {
        return Err(Refused::BadRequest(format!("not a directory: {dir}")));
    }
    let name = spawn_claude(
        ctx,
        side,
        &dir,
        &["--model", model, "--effort", effort],
        Meta { model: Some(model.into()), effort: Some(effort.into()), rc_link: None },
    )
    .await?;
    Ok(Launched { name, native: !side.is_wsl() })
}

pub async fn resume_session(ctx: &Arc<Ctx>, sid: &str) -> Result<String, Refused> {
    assert_valid_sid(sid)?;
    // The side is whichever history holds the sid; its cwd is already in
    // that side's notation.
    let mut found: Option<(&Side, String)> = None;
    for side in &ctx.sides {
        let hist = read_if(&side.claude_dir.join("history.jsonl")).await.unwrap_or_default();
        let cwd = group_history(&hist)
            .into_iter()
            .find(|g| g.sid == sid)
            .and_then(|g| g.cwd)
            .filter(|c| !c.is_empty());
        if let Some(cwd) = cwd {
            found = Some((side, cwd));
            break;
        }
    }
    let (side, cwd) =
        found.ok_or_else(|| Refused::BadRequest(format!("unknown session: {sid}")))?;

    // D6: un-purge before spawning, so the resumed session is not filtered
    // straight back out of the resumable list it came from.
    ctx.purged.lock().unwrap_or_else(|e| e.into_inner()).remove(sid);

    spawn_claude(ctx, side, &cwd, &["--resume", sid], Meta::default()).await
}

/// Kill a session this dashboard owns. The name is guarded first — it becomes
/// a `tmux` or `taskkill` argument. Console sides are searched first, with the
/// same liveness scan the list uses, so a stale file whose pid was recycled
/// never matches and nothing foreign is killed; then every tmux side is tried.
pub async fn kill_session(ctx: &Arc<Ctx>, name: &str) -> Result<(), Refused> {
    assert_kill_name(name)?;

    #[cfg(windows)]
    for side in ctx.sides.iter().filter(|s| matches!(s.backend, Backend::Console)) {
        let rows = side.proc_rows(&ctx.host.sampler).await;
        let hit = live_session_files(side, &rows, &HashSet::new())
            .await
            .into_iter()
            .find(|(_, s)| s.name.as_deref() == Some(name));
        if let Some((pid, _)) = hit {
            let pid = pid.to_string();
            side.runner
                .run_checked("taskkill", &["/T", "/F", "/PID", &pid], "taskkill")
                .await
                .map_err(Refused::Failed)?;
            ctx.meta_delete(name);
            ctx.host.log.push(format!("kill {name} (pid {pid})"));
            return Ok(());
        }
    }

    // Checked, and the meta entry is dropped only after the kill succeeds:
    // reporting a kill that did not happen removes the card from the UI while
    // the session keeps running.
    let mut last_err = None;
    for side in ctx.sides.iter().filter(|s| matches!(s.backend, Backend::Tmux)) {
        match side
            .runner
            .run_checked("tmux", &["kill-session", "-t", name], "tmux kill-session")
            .await
        {
            Ok(_) => {
                ctx.meta_delete(name);
                ctx.host.log.push(format!("kill {name}"));
                return Ok(());
            }
            Err(e) => last_err = Some(e),
        }
    }
    Err(Refused::Failed(last_err.unwrap_or_else(|| format!("no such session: {name}"))))
}
```

Note `BadRequest` in the `use super::validate::{…}` line is still needed by `purge_session`. If clippy reports it unused, `purge_session`'s signature was changed by mistake.

- [ ] **Step 4: Apply the recents rule in `routes.rs` and make its directory test platform-neutral**

In `crates/agent/src/http/routes.rs`, replace the body of `post_launch` with:

```rust
    let launched = launch_session(&ctx, &body.dir, &body.model, &body.effort).await?;

    // Fire-and-forget, exactly as `server.js:56`: a failed recents write logs
    // and does not fail the launch. The entry is the path as submitted: a
    // native path resolved as before, a WSL path verbatim — on Windows
    // `absolute("/home/u/p")` would be `C:\home\u\p`.
    let places_file = ctx.places_file.clone();
    let resolved = if launched.native {
        std::path::absolute(&body.dir)
            .unwrap_or_else(|_| std::path::PathBuf::from(&body.dir))
            .to_string_lossy()
            .into_owned()
    } else {
        body.dir.clone()
    };
    let log = Arc::clone(&ctx.host.log);
    tokio::spawn(async move {
        if let Err(e) = add_recent(&places_file, &resolved).await {
            log.push(format!("recent write failed: {e}"));
        }
    });

    Ok(Json(serde_json::json!({ "name": launched.name })).into_response())
```

In its `mod tests`, replace the body of `launch_rejects_a_directory_that_is_not_one` so the path is native on both platforms:

```rust
        let b = serve(cfg_for(tempdir("launch-dir"))).await.unwrap();
        let missing = std::env::temp_dir().join("no-such-cdash-dir");
        let req = serde_json::json!({ "dir": missing }).to_string();
        let (status, body) = http_post(&format!("http://{}/api/launch", b.addr), &req).await;
        assert_eq!(status, 400);
        assert!(body.contains("not a directory"));
```

- [ ] **Step 5: Run the tests, clippy, and the Windows type-check**

Run:
```bash
cargo test -p cdash-agent --locked
cargo clippy -p cdash-agent --all-targets --locked -- -D warnings -D clippy::disallowed_types
cargo clippy --locked --target x86_64-pc-windows-gnu -p cdash-agent --all-targets -- -D warnings -D clippy::disallowed_types
```
Expected: all PASS — including `the_rc_poll_can_find_the_session_by_name`, the cross-side resume test, and the unchanged kill tests; both clippy runs clean.

- [ ] **Step 6: Commit**

```bash
git add crates/agent/src/collect/spawn.rs crates/agent/src/http/routes.rs
git commit -m "collect: launch, resume and kill per side; RC poll by name; --name on every launch

A launch routes on the directory's shape and checks a WSL directory through
the share. Resume takes its side from whichever history holds the sid. Kill
searches console sides with the list's own liveness scan before falling
back to tmux, so a recycled pid is never taskkilled. The RC-link poll finds
a console session by the --name the launcher passes, which every backend now
passes. Recents keep a WSL path verbatim."
```

---

### Task 9: The WSL side at boot, and `hostinfo.wsl`

Spec §2 (probe, `CDASH_WSL`, `CDASH_WSL_DISTRO`, `hostinfo.wsl`, error handling at boot).

**Files:**
- Modify: `crates/agent/src/host/wsl.rs` (add `probe_wsl`, Windows only)
- Modify: `crates/agent/src/http/serve.rs` (`serve`: append the WSL side)
- Modify: `crates/agent/src/http/routes.rs` (`get_hostinfo`)

**Interfaces:**
- Consumes: `Side::wsl` (Task 6), `Runner::run_checked_with_timeout` (Task 3), `parse_wsl_probe`, `PROBE_SCRIPT` (Task 5).
- Produces: `#[cfg(windows)] pub async fn probe_wsl(native: &Runner, log: &LogBuffer) -> Option<WslProbe>`; `/api/hostinfo` gains `"wsl": null | {"distro": string, "missing": [string]}`.

- [ ] **Step 1: Write the failing `hostinfo` test**

In `crates/agent/src/http/routes.rs` `mod tests`, add:

```rust
    #[tokio::test]
    async fn hostinfo_reports_the_wsl_side_or_null() {
        // Off Windows there is never a WSL side; the key is still present so
        // a client can tell "no WSL" from "old agent".
        let b = serve(cfg_for(tempdir("hostinfo-wsl"))).await.unwrap();
        let body = reqwest_get(&format!("http://{}/api/hostinfo", b.addr)).await;
        let v: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert!(v.get("wsl").is_some(), "the key must exist: {body}");
        if !cfg!(windows) {
            assert!(v["wsl"].is_null());
        }
    }
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p cdash-agent --locked hostinfo_reports_the_wsl_side`
Expected: FAIL — no `wsl` key in the response.

- [ ] **Step 3: Add `probe_wsl` to `host/wsl.rs`**

Append before `#[cfg(test)]` in `crates/agent/src/host/wsl.rs`:

```rust
/// The first `wsl.exe` call may cold-start the distro; 5 seconds is not
/// enough for that and 30 is.
#[cfg(windows)]
const PROBE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

/// Ask the distro for its login PATH and its home as the share sees it.
/// `None` — with the reason logged — means the agent runs with the native
/// side alone: `wsl.exe` absent, the distro failing, a timeout, or output
/// that is not the two lines `PROBE_SCRIPT` prints.
#[cfg(windows)]
pub async fn probe_wsl(
    native: &crate::host::cmd::Runner,
    log: &crate::host::log::LogBuffer,
) -> Option<WslProbe> {
    if std::env::var("CDASH_WSL").as_deref() == Ok("0") {
        log.push("wsl: disabled by CDASH_WSL=0; Windows side only");
        return None;
    }
    let distro_flag = std::env::var("CDASH_WSL_DISTRO").ok().filter(|s| !s.is_empty());
    let mut args: Vec<&str> = Vec::new();
    if let Some(d) = distro_flag.as_deref() {
        args.push("-d");
        args.push(d);
    }
    args.extend_from_slice(&["--exec", "/bin/sh", "-lc", PROBE_SCRIPT]);

    match native.run_checked_with_timeout("wsl.exe", &args, "wsl probe", PROBE_TIMEOUT).await {
        Ok(out) => match parse_wsl_probe(&out) {
            Some(mut p) => {
                p.distro_flag = distro_flag;
                Some(p)
            }
            None => {
                log.push(format!(
                    "wsl: unexpected probe output {:?}; Windows side only",
                    out.trim()
                ));
                None
            }
        },
        Err(e) => {
            log.push(format!("wsl: {e}; Windows side only"));
            None
        }
    }
}
```

- [ ] **Step 4: Append the WSL side in `serve`**

In `crates/agent/src/http/serve.rs`, inside `pub async fn serve`, replace

```rust
    let h = host::init::init().await;
    let ctx = Arc::new(Ctx::new(h, cfg.claude_dir, cfg.disk_extra));
```
with
```rust
    let h = host::init::init().await;
    let mut ctx = Ctx::new(h, cfg.claude_dir, cfg.disk_extra);
    // The WSL side is appended before `Ctx` is shared; `sides` is fixed after.
    #[cfg(windows)]
    if let Some(probe) = crate::host::wsl::probe_wsl(&ctx.host.runner, &ctx.host.log).await {
        match crate::collect::side::Side::wsl(&probe, Arc::clone(&ctx.host.log)) {
            Some(side) => {
                ctx.host.log.push(format!(
                    "wsl: {} at {}",
                    side.wsl.as_ref().map(|w| w.distro.as_str()).unwrap_or("?"),
                    side.claude_dir.display()
                ));
                ctx.sides.push(side);
            }
            None => ctx.host.log.push(format!(
                "wsl: cannot read a distro from {:?}; Windows side only",
                probe.home_unc
            )),
        }
    }
    let ctx = Arc::new(ctx);
```

On Unix `ctx` is declared `mut` and never mutated, which `-D warnings` rejects. Add `#[cfg_attr(not(windows), allow(unused_mut))]` directly above the `let mut ctx = …` line.

- [ ] **Step 5: Report the side in `get_hostinfo`**

In `crates/agent/src/http/routes.rs`, replace `get_hostinfo` with:

```rust
/// Authenticated: it names the host's platform and which binaries are absent.
/// `/api/health` is the unauthenticated one and says only `{ok:true}`.
pub async fn get_hostinfo(State(ctx): State<Arc<Ctx>>) -> Response {
    // Re-probed per request on both sides, never a boot-time cache: the setup
    // screen's re-check button is worthless against a stale answer.
    let wsl = match ctx.sides.iter().find(|s| s.is_wsl()) {
        Some(s) => serde_json::json!({
            "distro": s.wsl.as_ref().map(|w| w.distro.as_str()),
            "missing": s.wsl_missing().await,
        }),
        None => serde_json::Value::Null,
    };
    Json(serde_json::json!({
        "platform": std::env::consts::OS,
        "version": env!("CARGO_PKG_VERSION"),
        "missing": ctx.host.missing(),
        "wsl": wsl,
    }))
    .into_response()
}
```

- [ ] **Step 6: Run the tests, clippy, and the Windows type-check**

Run:
```bash
cargo test -p cdash-agent --locked
cargo clippy -p cdash-agent --all-targets --locked -- -D warnings -D clippy::disallowed_types
cargo clippy --locked --target x86_64-pc-windows-gnu -p cdash-agent --all-targets -- -D warnings -D clippy::disallowed_types
```
Expected: all PASS; both clippy runs clean. The Linux boot gate still prints the banner: `PORT=0 timeout 5 cargo run --locked -p cdash-agent 2>&1 | grep -q "cdash-agent .* on http://127.0.0.1:" && echo BOOT_OK`.

- [ ] **Step 7: Commit**

```bash
git add crates/agent/src/host/wsl.rs crates/agent/src/http/serve.rs crates/agent/src/http/routes.rs
git commit -m "agent: probe WSL at boot on Windows and report the side in hostinfo

One sh -lc through wsl.exe --exec yields the distro's login PATH and its
home as the share sees it; from those the WSL side is built and appended
before Ctx is shared. Any failure logs once and leaves the native side
alone. CDASH_WSL=0 skips the bridge; CDASH_WSL_DISTRO names a distro.
/api/hostinfo gains a wsl field with the distro and its missing binaries."
```

---

### Task 10: Browse crumbs and the roots listing; the picker

Spec §8.

**Files:**
- Modify: `crates/agent/src/collect/browse.rs`
- Modify: `crates/agent/src/http/routes.rs` (`get_browse`)
- Modify: `public/app.js` (`openPicker`, `placeRow`, `browseTo`, `renderCrumbs`)

**Interfaces:**
- Produces: `Listing.crumbs: Vec<Crumb>`; `pub struct Crumb { pub name: String, pub path: String }`; `pub fn crumbs_for(abs: &Path) -> Vec<Crumb>`; `list_dirs(target: &str, show_hidden: bool, roots: &[String])`; `#[cfg(windows)] pub fn drive_roots() -> Vec<String>`.
- The JSON shape gains `crumbs: [{name, path}]`; `parent` stays.

- [ ] **Step 1: Write the failing crumb tests in `browse.rs`**

In `crates/agent/src/collect/browse.rs` `mod tests`, add:

```rust
    #[test]
    fn crumbs_for_a_unix_path_start_at_the_root() {
        let c = crumbs_for(Path::new("/a/b"));
        let pairs: Vec<(&str, &str)> = c.iter().map(|x| (x.name.as_str(), x.path.as_str())).collect();
        if cfg!(windows) {
            // `/a/b` is not a Windows shape; the drive tests below cover Windows.
            assert_eq!(pairs.first().map(|p| p.0), Some("/"));
        } else {
            assert_eq!(pairs, vec![("/", "/"), ("a", "/a"), ("b", "/a/b")]);
        }
    }

    #[tokio::test]
    async fn a_listing_carries_its_crumbs() {
        let root = fixture("crumbs");
        let d = list_dirs(root.to_str().unwrap(), false, &[]).await.unwrap();
        assert_eq!(d.crumbs.last().unwrap().path, d.path, "the last crumb is the listing itself");
        assert!(d.crumbs.len() >= 2);
    }
```

and a Windows-only module after `mod tests`:

```rust
#[cfg(all(test, windows))]
mod windows_tests {
    use super::*;

    #[test]
    fn crumbs_for_a_drive_and_a_share_path_begin_with_the_virtual_root() {
        let c = crumbs_for(Path::new(r"C:\Users\u"));
        let pairs: Vec<(&str, &str)> = c.iter().map(|x| (x.name.as_str(), x.path.as_str())).collect();
        assert_eq!(pairs, vec![("/", "/"), (r"C:\", r"C:\"), ("Users", r"C:\Users"), ("u", r"C:\Users\u")]);

        let c = crumbs_for(Path::new(r"\\wsl.localhost\Ubuntu\home"));
        let pairs: Vec<(&str, &str)> = c.iter().map(|x| (x.name.as_str(), x.path.as_str())).collect();
        assert_eq!(
            pairs,
            vec![("/", "/"), (r"\\wsl.localhost\Ubuntu\", r"\\wsl.localhost\Ubuntu\"), ("home", r"\\wsl.localhost\Ubuntu\home")]
        );
    }

    #[tokio::test]
    async fn the_slash_path_lists_the_given_roots_and_a_drive_root_has_slash_as_parent() {
        let roots = vec![r"C:\".to_string(), r"\\wsl.localhost\Ubuntu\".to_string()];
        let d = list_dirs("/", false, &roots).await.unwrap();
        assert_eq!(d.path, "/");
        assert_eq!(d.parent, None);
        assert_eq!(d.entries.iter().map(|e| e.path.as_str()).collect::<Vec<_>>(), roots);
        assert_eq!(d.crumbs.len(), 1);

        let c = list_dirs(r"C:\", false, &roots).await.unwrap();
        assert_eq!(c.parent.as_deref(), Some("/"));
    }
}
```

Every existing `list_dirs(x, y)` call in `mod tests` gains a third argument `&[]`.

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test -p cdash-agent --locked collect::browse`
Expected: compile errors — `crumbs_for`, `Crumb`, and the third parameter do not exist.

- [ ] **Step 3: Implement crumbs and roots**

In `crates/agent/src/collect/browse.rs`, add after `DirEntry`:

```rust
/// One breadcrumb: what to show and where it navigates. Built here rather
/// than in the client so the client never learns a path separator.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Crumb {
    pub name: String,
    pub path: String,
}
```

add `pub crumbs: Vec<Crumb>,` as the last field of `Listing`, and add after `map_err`:

```rust
/// Crumbs from the components: the prefix and root make the first crumb
/// (`/`, `C:\`, `\\wsl.localhost\Ubuntu\`), every normal component appends
/// one. On Windows a virtual root crumb `/` comes first — the roots listing.
pub fn crumbs_for(abs: &Path) -> Vec<Crumb> {
    use std::path::Component;
    let mut out = Vec::new();
    let mut acc = PathBuf::new();
    if cfg!(windows) {
        out.push(Crumb { name: "/".into(), path: "/".into() });
    }
    for c in abs.components() {
        match c {
            Component::Prefix(p) => {
                acc = PathBuf::from(p.as_os_str());
                acc.push(std::path::MAIN_SEPARATOR_STR);
                let s = acc.to_string_lossy().into_owned();
                out.push(Crumb { name: s.clone(), path: s });
            }
            Component::RootDir => {
                if acc.as_os_str().is_empty() {
                    acc.push(std::path::MAIN_SEPARATOR_STR);
                    let s = acc.to_string_lossy().into_owned();
                    out.push(Crumb { name: s.clone(), path: s });
                }
            }
            Component::Normal(n) => {
                acc.push(n);
                out.push(Crumb {
                    name: n.to_string_lossy().into_owned(),
                    path: acc.to_string_lossy().into_owned(),
                });
            }
            Component::CurDir | Component::ParentDir => {}
        }
    }
    out
}

/// Every drive letter whose root exists, as `X:\`.
#[cfg(windows)]
pub fn drive_roots() -> Vec<String> {
    ('A'..='Z')
        .map(|d| format!("{d}:\\"))
        .filter(|r| Path::new(r).is_dir())
        .collect()
}
```

Replace `list_dirs` with:

```rust
/// Folders only — a project directory is what is being chosen — plus symlinks,
/// which commonly point at directories. Sorted case-insensitively. On Windows
/// the path `/` is the roots listing: `roots` are the drives and the WSL share
/// the route supplies, and a drive's parent is `/`.
pub async fn list_dirs(
    target: &str,
    show_hidden: bool,
    roots: &[String],
) -> Result<Listing, BadRequest> {
    if cfg!(windows) && target == "/" {
        return Ok(Listing {
            path: "/".into(),
            parent: None,
            entries: roots.iter().map(|r| DirEntry { name: r.clone(), path: r.clone() }).collect(),
            truncated: false,
            crumbs: vec![Crumb { name: "/".into(), path: "/".into() }],
        });
    }
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

    let parent = abs
        .parent()
        .map(|p| p.to_string_lossy().into_owned())
        .or_else(|| if cfg!(windows) { Some("/".to_string()) } else { None });
    Ok(Listing {
        parent,
        entries: names
            .into_iter()
            .map(|name| DirEntry {
                path: abs.join(&name).to_string_lossy().into_owned(),
                name,
            })
            .collect(),
        crumbs: crumbs_for(&abs),
        path: abs.to_string_lossy().into_owned(),
        truncated,
    })
}
```

- [ ] **Step 4: Pass the roots from the route**

In `crates/agent/src/http/routes.rs`, replace `get_browse` with:

```rust
pub async fn get_browse(
    State(ctx): State<Arc<Ctx>>,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Response, ApiError> {
    let home = crate::host::path::home().to_string_lossy().into_owned();
    let target = q.get("path").filter(|p| !p.is_empty()).cloned().unwrap_or(home);
    let hidden = q.get("hidden").map(|h| h == "1").unwrap_or(false);
    Ok(Json(list_dirs(&target, hidden, &browse_roots(&ctx)).await?).into_response())
}

/// What `/` lists on Windows: every drive, then the WSL share root when there
/// is a WSL side. Nothing elsewhere — `/` is the real root.
fn browse_roots(ctx: &Ctx) -> Vec<String> {
    #[cfg(windows)]
    {
        let mut roots = crate::collect::browse::drive_roots();
        if let Some(w) = ctx.wsl_paths() {
            roots.push(format!("{}\\", w.unc_root));
        }
        roots
    }
    #[cfg(not(windows))]
    {
        let _ = ctx;
        Vec::new()
    }
}
```

- [ ] **Step 5: Change the picker in `public/app.js`**

Replace `openPicker`'s second line:

```js
  pkPath = typed || null; // seed from any typed value; the dead-end guard falls back to home on a bad one
```

Replace the first line of `placeRow`:

```js
  const name = path.split(/[\\/]/).filter(Boolean).pop() || path;
```

In `browseTo`, replace `renderCrumbs(d.path);` with `renderCrumbs(d.crumbs);`, and replace the whole `renderCrumbs` function with:

```js
function renderCrumbs(crumbs) {
  // The server builds the crumbs: this client never learns a path separator,
  // so C:\Users and \\wsl.localhost\Ubuntu render the same way / does.
  pkCrumbs.innerHTML = crumbs.map((c, i) =>
    (i ? '<span class="picker-crumb-sep">›</span>' : '') +
    `<button class="picker-crumb" type="button" data-nav="${esc(c.path)}">${esc(c.name)}</button>`
  ).join('');
  pkCrumbs.scrollLeft = pkCrumbs.scrollWidth; // keep the deepest crumb in view
}
```

- [ ] **Step 6: Run the tests, clippy, the JS tests, and the Windows type-check**

Run:
```bash
cargo test -p cdash-agent --locked
cargo clippy -p cdash-agent --all-targets --locked -- -D warnings -D clippy::disallowed_types
node --test
node --check public/app.js
cargo clippy --locked --target x86_64-pc-windows-gnu -p cdash-agent --all-targets -- -D warnings -D clippy::disallowed_types
```
Expected: all PASS; `node --check` prints nothing; both clippy runs clean.

- [ ] **Step 7: Commit**

```bash
git add crates/agent/src/collect/browse.rs crates/agent/src/http/routes.rs public/app.js
git commit -m "browse: server-built crumbs, a roots listing at / on Windows

The listing carries its breadcrumbs so the client never splits a path on a
separator, which C:\ and \\wsl.localhost\ would break. On Windows the path /
lists every drive and the WSL share root, and a drive's parent is /. The
picker seeds from any typed value and names places after either separator."
```

---

### Task 11: The windowless binary, `serve_from_env`, and Task Scheduler `install`/`uninstall`

Spec §5 (Task Scheduler), §6 (two binaries).

**Files:**
- Create: `crates/agent/src/host/task.rs`
- Create: `crates/agent/src/main_w.rs`
- Modify: `crates/agent/src/host/mod.rs` (add `pub mod task;`)
- Modify: `crates/agent/src/http/serve.rs` (add `serve_from_env`)
- Modify: `crates/agent/src/main.rs` (subcommands; `main` calls `serve_from_env`)
- Modify: `crates/agent/Cargo.toml` (`default-run`, second `[[bin]]`)

**Interfaces:**
- Consumes: `Runner::run_checked_with_timeout` (Task 3).
- Produces:
  - `cdash_agent::http::serve::serve_from_env() -> ()` — async; reads `Config::from_env`, warns, binds, prints the banner, parks forever; exits the process with the documented codes on failure.
  - `cdash_agent::host::task::{TASK_NAME, SCHEDULED_EXE, task_xml(exe, working_dir, user) -> String, utf16le_bom(&str) -> Vec<u8>}`; `#[cfg(windows)] install(&Runner) -> Result<String, String>`, `#[cfg(windows)] uninstall(&Runner) -> Result<String, String>`.
  - A second binary target `cdash-agentw` (`src/main_w.rs`).

- [ ] **Step 1: Write the failing XML tests**

Create `crates/agent/src/host/task.rs` with the tests only:

```rust
//! Task Scheduler registration for the scheduled binary. `task_xml` is pure
//! and tested everywhere; `install` and `uninstall` drive `schtasks` on
//! Windows only.

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_task_has_every_setting_the_design_depends_on() {
        let xml = task_xml(r"C:\cdash\cdash-agentw.exe", r"C:\cdash", r"PC\pat");
        // The default PT72H kills the agent after three days.
        assert!(xml.contains("<ExecutionTimeLimit>PT0S</ExecutionTimeLimit>"), "{xml}");
        // A repetition tick while the agent lives must be a no-op.
        assert!(xml.contains("<MultipleInstancesPolicy>IgnoreNew</MultipleInstancesPolicy>"));
        // The only restart the scheduler offers: RestartOnFailure never fires on an exit.
        assert!(xml.contains("<Interval>PT5M</Interval>"));
        assert!(!xml.contains("RestartOnFailure"));
        // The user's desktop session: WSL, the share, console windows.
        assert!(xml.contains("<LogonType>InteractiveToken</LogonType>"));
        assert!(xml.contains("<LogonTrigger>"));
        // The default 7 is BELOW_NORMAL, inherited by every claude the agent spawns.
        assert!(xml.contains("<Priority>4</Priority>"));
        assert!(xml.contains(r"<Command>C:\cdash\cdash-agentw.exe</Command>"));
        assert!(xml.contains(r"<WorkingDirectory>C:\cdash</WorkingDirectory>"));
        assert_eq!(xml.matches(r"<UserId>PC\pat</UserId>").count(), 2, "trigger and principal");
        assert!(xml.starts_with(r#"<?xml version="1.0" encoding="UTF-16"?>"#));
    }

    #[test]
    fn xml_special_characters_in_paths_are_escaped() {
        let xml = task_xml(r"C:\a & b\cdash-agentw.exe", r"C:\a & b", "u");
        assert!(xml.contains(r"C:\a &amp; b\cdash-agentw.exe"));
        assert!(!xml.contains("a & b"));
    }

    #[test]
    fn the_file_is_utf16le_with_a_bom() {
        let bytes = utf16le_bom("<T/>");
        assert_eq!(&bytes[..2], &[0xFF, 0xFE]);
        let units: Vec<u16> = bytes[2..].chunks(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
        assert_eq!(String::from_utf16(&units).unwrap(), "<T/>");
    }
}
```

Add `pub mod task;` to `crates/agent/src/host/mod.rs`.

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test -p cdash-agent --locked host::task`
Expected: compile errors — `task_xml` and `utf16le_bom` do not exist.

- [ ] **Step 3: Implement `task.rs`**

Insert between the module doc and `#[cfg(test)]`:

```rust
#[cfg(windows)]
use super::cmd::Runner;
#[cfg(windows)]
use std::time::Duration;

pub const TASK_NAME: &str = "cdash-agent";
/// The windowless twin the task runs; it must sit beside `cdash-agent.exe`.
pub const SCHEDULED_EXE: &str = "cdash-agentw.exe";
#[cfg(windows)]
const SCHTASKS_TIMEOUT: Duration = Duration::from_secs(30);

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

/// The task definition. Element order follows what `schtasks /Query /XML`
/// exports, because the schema is a sequence. The settings that matter:
/// `PT0S` (the default PT72H kills the agent after three days), `IgnoreNew`
/// (a repetition tick or a second logon while the agent lives is a no-op),
/// the five-minute repetition (the only restart the scheduler offers —
/// `RestartOnFailure` counts only an action it could not start), and
/// priority 4 (the default 7 is BELOW_NORMAL with low I/O and memory
/// priority, inherited by every `claude` the agent spawns).
pub fn task_xml(exe: &str, working_dir: &str, user: &str) -> String {
    let (exe, dir, user) = (xml_escape(exe), xml_escape(working_dir), xml_escape(user));
    format!(
        r#"<?xml version="1.0" encoding="UTF-16"?>
<Task version="1.4" xmlns="http://schemas.microsoft.com/windows/2004/02/mit/task">
  <Triggers>
    <LogonTrigger>
      <Repetition>
        <Interval>PT5M</Interval>
        <StopAtDurationEnd>false</StopAtDurationEnd>
      </Repetition>
      <Enabled>true</Enabled>
      <UserId>{user}</UserId>
    </LogonTrigger>
  </Triggers>
  <Principals>
    <Principal id="Author">
      <UserId>{user}</UserId>
      <LogonType>InteractiveToken</LogonType>
      <RunLevel>LeastPrivilege</RunLevel>
    </Principal>
  </Principals>
  <Settings>
    <MultipleInstancesPolicy>IgnoreNew</MultipleInstancesPolicy>
    <DisallowStartIfOnBatteries>false</DisallowStartIfOnBatteries>
    <StopIfGoingOnBatteries>false</StopIfGoingOnBatteries>
    <Enabled>true</Enabled>
    <ExecutionTimeLimit>PT0S</ExecutionTimeLimit>
    <Priority>4</Priority>
  </Settings>
  <Actions Context="Author">
    <Exec>
      <Command>{exe}</Command>
      <WorkingDirectory>{dir}</WorkingDirectory>
    </Exec>
  </Actions>
</Task>
"#
    )
}

/// What `schtasks /Query /XML` exports: UTF-16LE with a byte-order mark.
pub fn utf16le_bom(s: &str) -> Vec<u8> {
    let mut out = vec![0xFF, 0xFE];
    for u in s.encode_utf16() {
        out.extend_from_slice(&u.to_le_bytes());
    }
    out
}

/// Register the task for the current user and start it. Order matters: an
/// earlier instance is ended first, or `IgnoreNew` makes `/Run` a silent
/// no-op and an upgrade keeps running the old binary. Re-running `install`
/// is also how `setx` changes are applied. On a first install the `/End`
/// line fails and is ignored; its log echo on stderr is expected.
#[cfg(windows)]
pub async fn install(runner: &Runner) -> Result<String, String> {
    let exe = std::env::current_exe().map_err(|e| format!("current_exe: {e}"))?;
    let dir = exe.parent().ok_or("the executable has no parent directory")?.to_path_buf();
    let agentw = dir.join(SCHEDULED_EXE);
    if !agentw.is_file() {
        return Err(format!(
            "{} not found beside {}; the scheduled task runs the windowless binary",
            agentw.display(),
            exe.display()
        ));
    }
    let user = format!(
        "{}\\{}",
        std::env::var("USERDOMAIN").unwrap_or_default(),
        std::env::var("USERNAME").unwrap_or_default()
    );
    let xml = task_xml(&agentw.to_string_lossy(), &dir.to_string_lossy(), &user);
    let tmp = std::env::temp_dir().join("cdash-agent-task.xml");
    std::fs::write(&tmp, utf16le_bom(&xml)).map_err(|e| format!("write {}: {e}", tmp.display()))?;
    let tmp_s = tmp.to_string_lossy().into_owned();

    let _ = runner
        .run_checked_with_timeout("schtasks", &["/End", "/TN", TASK_NAME], "schtasks end", SCHTASKS_TIMEOUT)
        .await;
    let created = runner
        .run_checked_with_timeout(
            "schtasks",
            &["/Create", "/TN", TASK_NAME, "/XML", &tmp_s, "/F"],
            "schtasks create",
            SCHTASKS_TIMEOUT,
        )
        .await;
    let _ = std::fs::remove_file(&tmp);
    created?;
    runner
        .run_checked_with_timeout("schtasks", &["/Run", "/TN", TASK_NAME], "schtasks run", SCHTASKS_TIMEOUT)
        .await?;

    // The scheduled instance's exit status is invisible to anyone; the URL is
    // the check.
    let port = std::env::var("PORT").unwrap_or_else(|_| "8080".to_string());
    Ok(format!(
        "registered task {TASK_NAME}: {} at logon for {user}, retried every 5 minutes while stopped\nopen http://127.0.0.1:{port}",
        agentw.display()
    ))
}

#[cfg(windows)]
pub async fn uninstall(runner: &Runner) -> Result<String, String> {
    let _ = runner
        .run_checked_with_timeout("schtasks", &["/End", "/TN", TASK_NAME], "schtasks end", SCHTASKS_TIMEOUT)
        .await;
    runner
        .run_checked_with_timeout(
            "schtasks",
            &["/Delete", "/TN", TASK_NAME, "/F"],
            "schtasks delete",
            SCHTASKS_TIMEOUT,
        )
        .await?;
    Ok(format!("removed task {TASK_NAME}"))
}
```

- [ ] **Step 4: Run the XML tests**

Run: `cargo test -p cdash-agent --locked host::task`
Expected: 3 PASS.

- [ ] **Step 5: Move the serving body into `serve_from_env`**

In `crates/agent/src/http/serve.rs`, add after `serve`:

```rust
/// Everything `main` does after argument parsing, shared by the console
/// binary and the windowless one: read the environment, name the two
/// undiagnosable exposures, bind, print the banner, park. Failures exit the
/// process with the codes the README documents; on success this never
/// returns.
pub async fn serve_from_env() {
    let cfg = match Config::from_env() {
        Ok(c) => c,
        Err(e) => {
            // A misconfiguration that would otherwise open the origin is
            // refused at boot rather than debugged in production.
            eprintln!("{e}");
            std::process::exit(2);
        }
    };
    let (bind, port) = (cfg.bind, cfg.port);

    // Both exposures are undiagnosable from their symptom, so they are named
    // here rather than left for the operator to infer.
    if !bind.is_loopback() && cfg.auth.is_open() {
        eprintln!(
            "warning: CDASH_BIND={bind} with CDASH_AUTH=none — every session runs with \
             --dangerously-skip-permissions, so anyone who can reach this port has \
             remote code execution on this host"
        );
    }
    if cfg.password.as_ref().is_some_and(|p| !p.policy.secure_cookie) {
        eprintln!(
            "warning: CDASH_ALLOW_INSECURE_COOKIE=1 — the session cookie has lost Secure \
             and the __Host- prefix and now crosses the wire in clear; anyone on the path \
             can steal a logged-in session"
        );
    }

    match serve(cfg).await {
        Ok(b) => {
            println!("cdash-agent {} on http://{}", env!("CARGO_PKG_VERSION"), b.addr);
            let missing = b.ctx.host.missing();
            if !missing.is_empty() {
                println!("missing: {}", missing.join(", "));
            }
            // The task inside `serve` owns the accept loop; park here.
            std::future::pending::<()>().await;
        }
        Err(e) if e.kind() == std::io::ErrorKind::AddrInUse => {
            // The spec's diagnosed condition: stderr, exit 3, no pidfile.
            eprintln!("port {port} already in use");
            std::process::exit(3);
        }
        Err(e) if e.kind() == std::io::ErrorKind::InvalidData => {
            // A startup refusal, e.g. cf-access could not obtain its keys.
            // Named, non-zero, and nothing ever listened.
            eprintln!("{e}");
            std::process::exit(2);
        }
        Err(e) => {
            eprintln!("cannot bind {bind}:{port}: {e}");
            std::process::exit(1);
        }
    }
}
```

- [ ] **Step 6: Slim `main.rs` and add the subcommands**

In `crates/agent/src/main.rs`, delete the `use cdash_agent::http::serve::{serve, Config};` line and replace the whole `main` function with:

```rust
#[tokio::main]
async fn main() {
    match std::env::args().nth(1).as_deref() {
        None => {}
        Some("set-password") => match read_password_twice() {
            Ok(hash) => {
                println!("{hash}");
                return;
            }
            Err(e) => {
                eprintln!("{e}");
                std::process::exit(2);
            }
        },
        // Task Scheduler registration of the windowless twin (spec §5).
        #[cfg(windows)]
        Some(cmd @ ("install" | "uninstall")) => {
            use cdash_agent::host::cmd::Runner;
            use cdash_agent::host::log::LogBuffer;
            let runner = Runner::new(
                std::env::var("PATH").unwrap_or_default(),
                std::sync::Arc::new(LogBuffer::new()),
            );
            let done = if cmd == "install" {
                cdash_agent::host::task::install(&runner).await
            } else {
                cdash_agent::host::task::uninstall(&runner).await
            };
            match done {
                Ok(msg) => {
                    println!("{msg}");
                    return;
                }
                Err(e) => {
                    eprintln!("{e}");
                    std::process::exit(2);
                }
            }
        }
        // Without this, `cdash-agent --version` binds a port and parks forever.
        Some(other) => {
            let extra = if cfg!(windows) { "|install|uninstall" } else { "" };
            eprintln!("unknown argument: {other}\nusage: cdash-agent [set-password{extra}]");
            std::process::exit(2);
        }
    }

    cdash_agent::http::serve::serve_from_env().await;
}
```

- [ ] **Step 7: Add the windowless binary**

Create `crates/agent/src/main_w.rs`:

```rust
//! The windowless twin of `cdash-agent`, the binary Task Scheduler runs: no
//! console at logon, no subcommands, the same server. The `python`/`pythonw`
//! idiom. Writes to the missing stdout are discarded by Rust's stdio; the log
//! is read at `/api/logs`.
#![cfg_attr(windows, windows_subsystem = "windows")]

#[cfg(windows)]
#[tokio::main]
async fn main() {
    cdash_agent::http::serve::serve_from_env().await;
}

/// The target exists on every platform so `cargo build` and `-D warnings`
/// behave the same everywhere; off Windows it does nothing.
#[cfg(not(windows))]
fn main() {}
```

In `crates/agent/Cargo.toml`, add `default-run = "cdash-agent"` to `[package]` (after `rust-version`), and after the existing `[[bin]]` block add:

```toml
# Built for every platform, meaningful on Windows: see src/main_w.rs.
[[bin]]
name = "cdash-agentw"
path = "src/main_w.rs"
test = false
bench = false
```

- [ ] **Step 8: Run everything, both boot gates, and the Windows type-check**

Run:
```bash
cargo test -p cdash-agent --locked
cargo clippy -p cdash-agent --all-targets --locked -- -D warnings -D clippy::disallowed_types
cargo build --locked -p cdash-agent && ls target/debug/cdash-agent target/debug/cdash-agentw
PORT=0 timeout 5 cargo run --locked -p cdash-agent 2>&1 | grep -q "cdash-agent .* on http://127.0.0.1:" && echo BOOT_OK
cargo run --locked -p cdash-agent -- bogus; echo "exit=$?"
cargo clippy --locked --target x86_64-pc-windows-gnu -p cdash-agent --all-targets -- -D warnings -D clippy::disallowed_types
```
Expected: tests PASS; both binaries exist; `BOOT_OK`; the bogus argument prints `usage: cdash-agent [set-password]` and `exit=2`; both clippy runs clean. `cargo run -p cdash-agent` resolving without `--bin` proves `default-run`.

- [ ] **Step 9: Commit**

```bash
git add crates/agent/Cargo.toml crates/agent/src/main.rs crates/agent/src/main_w.rs crates/agent/src/host/task.rs crates/agent/src/host/mod.rs crates/agent/src/http/serve.rs
git commit -m "agent: windowless twin binary and Task Scheduler install/uninstall

cdash-agentw is the same server without a console, which is what the
scheduled task runs so no window opens at logon; cdash-agent keeps its
console for the banner, set-password, and the new install and uninstall
subcommands. install writes the task XML — logon trigger with a five-minute
repetition under IgnoreNew, PT0S, priority 4 — ends any earlier instance,
registers with schtasks and starts it."
```

---

### Task 12: The `windows` CI job, the README, and the push

Spec §10 (CI), §11 (deployment).

**Files:**
- Modify: `.github/workflows/ci.yml`
- Modify: `README.md`

- [ ] **Step 1: Add the `windows` job**

Append to `.github/workflows/ci.yml` (same indentation as the existing jobs):

```yaml
  # The only place Windows code runs: tests, clippy, and two boot gates — the
  # console binary by its banner, the windowless one by a health request,
  # because a GUI-subsystem process has no banner to grep.
  windows:
    runs-on: windows-latest
    steps:
      - uses: actions/checkout@v4
      - uses: dtolnay/rust-toolchain@1.94.1
        with:
          components: clippy
      # aws-lc-sys assembles with NASM on x86_64-pc-windows-msvc; CMake is on the image.
      - uses: ilammy/setup-nasm@v1
      - run: cargo test -p cdash-agent --locked
      - run: cargo clippy -p cdash-agent --all-targets --locked -- -D warnings -D clippy::disallowed_types
      - run: cargo build --release --locked -p cdash-agent
      - name: Execute the console binary
        shell: pwsh
        run: |
          $env:PORT = '0'
          $p = Start-Process -FilePath target\release\cdash-agent.exe -PassThru -NoNewWindow -RedirectStandardOutput boot.log -RedirectStandardError boot.err
          Start-Sleep -Seconds 5
          Stop-Process -Id $p.Id -Force
          Get-Content boot.log, boot.err
          if (-not (Select-String -Path boot.log -Pattern 'cdash-agent .* on http://127.0.0.1:' -Quiet)) { exit 1 }
      - name: Execute the windowless binary
        shell: pwsh
        run: |
          $env:PORT = '18080'
          $p = Start-Process -FilePath target\release\cdash-agentw.exe -PassThru
          Start-Sleep -Seconds 5
          try { $r = Invoke-WebRequest -UseBasicParsing http://127.0.0.1:18080/api/health } finally { Stop-Process -Id $p.Id -Force }
          if ($r.Content -ne '{"ok":true}') { Write-Error "unexpected health body: $($r.Content)"; exit 1 }
      - uses: actions/upload-artifact@v4
        with:
          name: cdash-agent-x86_64-pc-windows-msvc
          path: |
            target/release/cdash-agent.exe
            target/release/cdash-agentw.exe
```

- [ ] **Step 2: Document the Windows install in `README.md`**

Insert this section directly after the `## Desktop client (Linux)` section:

```markdown
## Windows

One native agent, started by Task Scheduler at every logon, sees Claude Code
on **both** sides of the machine: sessions started from a Windows terminal
(`%USERPROFILE%\.claude`, `claude.exe`) and sessions inside your WSL distro,
reached over `\\wsl.localhost` and `wsl.exe`. Windows-side sessions open in
their own console window; WSL-side sessions run in tmux as on Linux. A path
decides the side: `C:\…` is Windows, `/home/…` or `\\wsl.localhost\<distro>\…`
is WSL.

1. Download `cdash-agent.exe`, `cdash-agentw.exe` and the `public/` directory
   from the `cdash-agent-x86_64-pc-windows-msvc` CI artifact into one folder.
2. Run `cdash-agent.exe install` once. It registers a logon task for your user,
   starts it, and prints the URL to open. No re-login is needed.
3. Configure with user environment variables, then run `install` again to
   apply: `setx PORT 8080`, `setx CDASH_BIND 0.0.0.0`, `setx CDASH_WSL_DISTRO Ubuntu`,
   `setx CDASH_WSL 0` to leave WSL alone.

`cdash-agentw.exe` is the same server without a console window; the task runs
it. `cdash-agent.exe` keeps its console for `set-password`, `install`,
`uninstall`, and a first check with a visible banner — a session launched from
that instance reads and writes its terminal, so the scheduled instance is the
one to use.

The task retries every five minutes while you are logged on, so a crash or a
port freed after logon costs at most five minutes; nothing runs before logon.
Upgrade by `cdash-agent.exe uninstall`, replacing the three files, `install`.

Requirements: the native Claude Code installer (`claude.exe`; an npm
`claude.cmd` is reported as missing), Git for Windows, and for the WSL side a
WSL 2 distro with `tmux`, `claude` and `git` on its login-shell PATH.
`/api/hostinfo` reports the distro and anything it lacks under `wsl`. While
the WSL side is on, polling keeps the distro and its VM resident; `CDASH_WSL=0`
is the switch for a machine whose WSL has no Claude in it.
```

In the `## Configuration` table add three rows after `DISK_EXTRA`:

```markdown
| `CDASH_WSL` | — | Windows only. `0` skips the WSL side entirely. |
| `CDASH_WSL_DISTRO` | the default distro | Windows only. Which distro the WSL side is. |
```

and change the `DISK_EXTRA` row's example to `e.g. `/mnt/d`, or `D:\` on Windows`. Also amend the sentence `Requires `tmux`, `claude` and `git` on `PATH`` to: `Requires `tmux`, `claude` and `git` on `PATH` (`claude` and `git` on Windows, where tmux lives on the WSL side)`.

- [ ] **Step 3: Validate the workflow file and run the full local gate one last time**

Run:
```bash
node -e "require('fs').readFileSync('.github/workflows/ci.yml','utf8')" && python3 -c "import yaml,sys; yaml.safe_load(open('.github/workflows/ci.yml')); print('yaml ok')"
cargo test --all --locked 2>&1 | tail -3 || true
cargo test -p cdash-agent --locked
cargo clippy -p cdash-agent --all-targets --locked -- -D warnings -D clippy::disallowed_types
cargo clippy --locked --target x86_64-pc-windows-gnu -p cdash-agent --all-targets -- -D warnings -D clippy::disallowed_types
node --test
```
Expected: `yaml ok`; the agent crate green on both targets; JS tests green. (`cargo test --all` may fail here only in `cdash-tauri`'s build script for want of webkit2gtk — that is the CI `test` job's concern and unchanged by this plan; the one line changed in the Tauri crate is `home_dir()`.)

- [ ] **Step 4: Commit and push**

```bash
git add .github/workflows/ci.yml README.md
git commit -m "ci: windows job with both boot gates; README: Windows install

The windows-latest job runs the agent's tests and clippy, builds both
binaries, boots the console one by its banner and the windowless one by a
health request, and uploads both. The README gains the install, upgrade and
configuration story for Windows and the two WSL variables."
git push -u origin claude/windows-agent-task-scheduler-llod3o
```

- [ ] **Step 5: Watch the `windows` job and fix what it finds**

Open the Actions run for the push. All three jobs must be green. A failure in the `windows` job is a real defect in this plan's code, never a flake: read the failing test or step, fix it in the task that owns the file, re-run the local gates, commit, push. The likeliest first findings and their owners: a test needing a shell that Task 7 or 8 forgot to move under `#[cfg(unix)]`; an integration test in `crates/agent/tests/` using a Unix path — gate that single test with `#[cfg(unix)]`, nothing more; the boot gates' `Start-Process` needing `-WorkingDirectory (Get-Location)`.

After green, the spec's "unverifiable here" list (§10) is what the first run on a real Windows machine checks, in that order.
