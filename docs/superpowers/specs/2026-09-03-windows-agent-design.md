# Windows agent with WSL reach-through — design

Date: 2026-09-03
Status: approved in discussion, pending spec review
Supersedes: the "Windows" paragraphs of
[`2026-07-30-tauri-multi-host-design.md`](2026-07-30-tauri-multi-host-design.md)
under *Managed server, per platform* and sequencing step 9. Everything else in
that spec stands.

## Problem

On a Windows machine the dashboard currently has no agent of its own. The old
plan was to run the Linux agent inside a WSL distro and have the Tauri client
spawn it, copying a musl binary into the distro and managing a pidfile. That
leaves two gaps:

1. Nothing starts the agent at logon. A WSL distro is not running until
   something invokes it, and a Linux process inside it has no way to register
   itself with Windows.
2. Claude Code installed natively on Windows writes its own `%USERPROFILE%\.claude`
   and runs as `claude.exe`. An agent inside WSL cannot see those processes and
   was never asked to read that directory.

This design replaces the Linux-agent-in-WSL plan with a **native Windows agent**
that registers itself in Task Scheduler, starts at every logon, and reads and
controls Claude Code sessions on **both** the Windows side and the WSL side.

## Decisions

| Decision | Choice | Why |
|---|---|---|
| Topology | One native `cdash-agent.exe`; the WSL side is reached through `wsl.exe` and the `\\wsl.localhost` share | The user asked for the Linux agent to be replaced, not wrapped. One process, one port, no copy-in, no pidfile |
| Trigger | Task Scheduler **logon** trigger for the current user, interactive token | A startup-triggered task runs in session 0: no `\\wsl.localhost` share, no user distro instance, no desktop for a console window. Both sides need the user's session |
| Windows-side scope | Full: list, launch, resume, kill | Chosen by the user over monitor-only |
| Windows launcher | `conhost.exe claude …` in a new console window; ownership by `--name cdash-…` | No tmux on Windows. `conhost` creates the console regardless of whether the agent has one; the name is the only durable identifier without tmux |
| WSL file access | Direct reads over `\\wsl.localhost\<distro>\…` with the existing `tokio::fs` code | One spawn per file read through `wsl.exe` would make a poll take seconds |
| WSL commands | `wsl.exe [-d D] --exec /usr/bin/env PATH=<probed> <program> <args>` | `--exec` bypasses the distro shell so arguments arrive unchanged; `env` applies the login-shell PATH without sourcing a profile per call |
| WSL process rows | One `ps` per poll, parsed | `sysinfo` sees only the Windows kernel |
| Distros | One: the default, or `CDASH_WSL_DISTRO` | Each distro would add a `wsl.exe` spawn per poll and wake stopped distros. Not needed yet |
| Hidden window | Windows release binary is GUI-subsystem and attaches to a parent console when one exists | A console app started by Task Scheduler at logon opens a visible window. Every other route flashes one |
| Verification | A `windows-latest` CI job; pure parsers tested on every host | No Windows machine is available to the implementer |

## Scope

In: everything under *Design*. Out: see *Out of scope*.

## Design

### 1. Sides

A **side** is one Claude Code installation the agent can see. The crate stays
`cdash-agent`; platform differences are confined to the `host` layer and the
new side-aware pieces of `collect` behind `cfg(windows)`. Every parser and every
collector body is shared.

```rust
pub struct Side {
    pub label: &'static str,   // "native" | "wsl" — log keys and /api/hostinfo
    pub claude_dir: PathBuf,   // C:\Users\u\.claude, ~/.claude, or \\wsl.localhost\Ubuntu\home\u\.claude
    pub runner: Arc<Runner>,   // native, or wsl.exe-prefixed
    pub backend: Backend,      // Tmux | Console
    pub procs: Procs,          // Sampler | Ps
    pub wsl: Option<WslPaths>, // unc_root and distro, WSL sides only
}
pub enum Backend { Tmux, Console }
pub enum Procs { Sampler, Ps }
```

| Platform | Sides |
|---|---|
| Linux, macOS, Termux, VPS | one: `native`, Tmux, Sampler |
| Windows | `native`, Console, Sampler; plus `wsl`, Tmux, Ps when the probe in §2 succeeds |

`Ctx` gains `sides: Vec<Side>` with the native side first. `Ctx::new` keeps
its signature and builds the native side, so every existing test fixture
still compiles; `serve` appends the WSL side on Windows. `ctx.claude_dir`,
`ctx.runner` and `ctx.places_file` keep referring to the native side, so the
places file and the majority of call sites do not move. Variants only Windows
constructs — `Backend::Console`, `Procs::Ps` — are `cfg(windows)`-gated so
`-D warnings` holds on Unix without dead arms. Shared state stays
shared: `meta` is keyed by session name, `purged` by session id, the transcript
cache by file path and the git cache by working directory, and none of those
collide across sides. The git cache already takes the runner as a parameter,
so each side passes its own.

`collect_sessions` runs the per-side work for each side in order and
concatenates `running`. For a Tmux side that is the existing pane loop plus
the session-file scan with pane pids excluded; for a Console side it is the
session-file scan alone. The resumable loop runs per side exactly as today —
its three-turn filter, purge check and `RESUMABLE_MAX` cap included — then the
per-side lists are merged by timestamp descending and truncated to
`RESUMABLE_MAX` again. Machine stats and disks come from the native side only.

`Procs::Sampler` uses `ctx.host.sampler` as today. `Procs::Ps` runs
`ps -eo pid=,ppid=,%cpu=,rss=,comm=` through the side's runner once per poll
and feeds the rows to the existing `proc_tree_usage`. Its CPU figure is the
lifetime average `ps` reports, which is what the Node agent showed; it is
returned as `Some`, with `cpu_sample_age_ms: 0`.

### 2. WSL bridge

Windows only. At boot, after the native host is initialised:

1. Run, through the native runner with a **30 second** timeout because the
   first call may cold-start the distro:

   ```
   wsl.exe [-d <CDASH_WSL_DISTRO>] --exec /bin/sh -lc 'printf "%s\n%s\n" "$PATH" "$(wslpath -w "$HOME")"'
   ```

   Line 1 is the login-shell PATH inside the distro. Line 2 is the user's home
   as a UNC path, `\\wsl.localhost\Ubuntu\home\u` on current WSL or
   `\\wsl$\Ubuntu\home\u` on older builds. Both prefixes are accepted.
2. `parse_wsl_probe(out) -> Option<WslProbe { path, home_unc }>` is a pure
   function. From `home_unc`: the distro is the third path component, the
   `unc_root` is the first three (`\\wsl.localhost\Ubuntu`), and the Claude
   directory is `home_unc\.claude`.
3. Build the WSL runner: the existing `Runner` with a **prefix** of
   `["wsl.exe", ("-d", distro)?, "--exec", "/usr/bin/env", "PATH=<line 1>"]`.
   `Runner` gains a `prefix: Vec<String>` field; when non-empty,
   `run_checked_with_timeout` spawns `prefix[0]` with `prefix[1..] ++ [program] ++ args`.
   The runner's own `PATH` env stays the Windows PATH, which is what locates
   `wsl.exe`. Its log lines carry a `wsl ` prefix, so a failed
   `tmux list-panes` names its side; the log-once set is per runner already.

If any step fails — `wsl.exe` absent, non-zero exit, timeout, unparseable
output — the agent logs one line, `wsl: <reason>; Windows side only`, and runs
with the native side alone. `/api/hostinfo` reports `"wsl": null`.

`CDASH_WSL_DISTRO` unset means no `-d` flag and therefore the WSL default
distro; no listing of distros is ever parsed.

Path conversion, pure and tested:

- `to_unc(unc_root, "/home/u/p")` → `\\wsl.localhost\Ubuntu\home\u\p`
- `from_unc(unc_root, unc)` → `Some("/home/u/p")` when `unc` starts with
  `\\wsl.localhost\<distro>` or `\\wsl$\<distro>` for the configured distro,
  compared case-insensitively; the bare root maps to `/`; anything else is
  `None`.

`/api/hostinfo` gains `"wsl": {"distro", "missing": [...]}` where `missing`
is recomputed per call by running
`sh -c 'for b in tmux claude git; do command -v "$b" >/dev/null 2>&1 || printf "%s\n" "$b"; done'`
through the WSL runner. The native `missing` array is unchanged.

### 3. Windows-native launcher

`spawn_claude(ctx, side, dir, claude_args, meta)` builds one argv,

```
claude --settings '{"env":{"CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC":""}}' <claude_args…>
       --dangerously-skip-permissions --remote-control <name> --name <name>
```

where the `--settings` value is a single argument, and the side's backend
wraps it:

- **Tmux**: `tmux new-session -d -s <name> -c <dir> <argv…>` through the
  side's runner, checked, exactly as today. `--name <name>` is the one addition,
  so the session file carries the same name on every backend.
- **Console**: `conhost.exe <argv…>` with the working directory set to `dir`,
  through a new `Runner::spawn_detached(program, args, cwd) -> Result<(), String>`
  that spawns without waiting, without `kill_on_drop`, without stdio
  overrides and without `CREATE_NO_WINDOW`. `conhost` creates the console
  window itself, so the launch works whether the agent has a console or not,
  and on Windows 11 with Windows Terminal as the default terminal it opens as
  a tab there. The spawned pid is `conhost`'s and is not used.

`claude` is resolved by `CreateProcess` through the `PATH` the runner sets,
which is the composed PATH of §6. Only the native installer's `claude.exe` is
supported; an npm `claude.cmd` cannot be started this way and is reported as
missing by the binary check, which is the honest signal.

**Ownership.** The session-file scan in `external.rs` already reads every
`<claude_dir>\sessions\<pid>.json` whose pid is a live `claude`/`claude.exe`
with `entrypoint == "cli"`. It gains one branch: a file whose `name` starts
with `cdash-` is **ours** — `external: false`, `model`/`effort` from `ctx.meta`
when present — and any other name is external as before. On Tmux sides pane
pids are excluded before the scan, as today, so Linux behaviour is unchanged.

**RC-link poll.** `poll_rc_link` takes a locator: `ByPid(claude_dir, pid)` for
tmux launches, where the pane pid is known, or `ByName(claude_dir, name)` for
console launches, where it scans the sessions directory for a file with that
`name` and reads `bridgeSessionId`. Budget and guards are unchanged.

**Kill.** `kill_session(ctx, name)` keeps `assert_kill_name` first, then:

1. For each Console side, scan session files for `name`; on a match run
   `taskkill /T /F /PID <pid>` through the native runner, checked.
2. Otherwise, for each Tmux side, `tmux kill-session -t <name>`, checked.
3. On success drop the meta entry and log, as today; the first failure's
   message is the error.

On Linux only step 2 exists, so the current behaviour and its tests hold.

### 4. Routing by path shape

A launch must land on one side, chosen from the directory the user gave.
Resume learns its side from whichever history held the sid, and kill finds
its side by discovery (§3); neither routes by path. `side_for(sides, dir)`
returns the side index and the directory in that side's own notation, or a
400:

| Input shape | Windows | Unix |
|---|---|---|
| `X:\…` or `X:/…` | native, as given | 400 |
| `\\wsl.localhost\<d>\…`, `\\wsl$\<d>\…` | WSL, via `from_unc`; 400 if no WSL side or another distro | 400 |
| `/…` | WSL, as given; 400 if no WSL side | native, as given |
| anything else | 400 `bad path` | 400 `bad path` |

These are string checks, not `Path::is_absolute`, because on Windows `/x` is
not absolute and `assert_path` must accept it. `assert_path` becomes "the
shape is one of the rows above". `side_for` is written as two
platform-agnostic functions, one per column, with a `cfg` alias selecting the
platform's own, so both tables are tested on every host.

- **Launch** validates model and effort, routes, checks the directory exists
  and is a directory — through the share for a WSL side, using `to_unc` — then
  calls `spawn_claude` on that side.
- **Resume** searches every side's `history.jsonl` for the sid; the first hit
  supplies the cwd, and the side is the one whose history held it.
- **Trust dialog.** `claude_json_path` becomes `side.claude_dir.parent()/.claude.json`
  unconditionally, dropping the `CLAUDE_DIR` env check. It resolves to the same
  file as today in both the default and the overridden case, and it gives the
  WSL side `\\wsl.localhost\…\home\u\.claude.json`.
- The recents entry recorded after a launch is the path the user submitted:
  native paths pass through `std::path::absolute` as today; a path that routed
  to the WSL side is recorded verbatim, because `absolute` on Windows would
  turn `/home/u/p` into `C:\home\u\p`.

### 5. Task Scheduler

Two Windows-only subcommands in `main.rs`, running `schtasks` through a
`Runner` with a 30 second timeout so the time-box rule holds.

`cdash-agent install`:

1. `task_xml(exe, working_dir, user) -> String`, pure, where `user` is
   `USERDOMAIN\USERNAME` from the environment and `exe` is `current_exe()`.
2. Write it UTF-16LE with BOM to `%TEMP%\cdash-agent-task.xml`, matching what
   `schtasks /Query /XML` exports.
3. `schtasks /Create /TN cdash-agent /XML <file> /F`, then
   `schtasks /Run /TN cdash-agent` so no re-login is needed; delete the file.
4. Print what was registered; on failure print `schtasks`' output and exit 2.

`cdash-agent uninstall`: `schtasks /End /TN cdash-agent` (ignored if not
running), then `schtasks /Delete /TN cdash-agent /F`.

The XML, with the settings that matter and why:

```xml
<?xml version="1.0" encoding="UTF-16"?>
<Task version="1.4" xmlns="http://schemas.microsoft.com/windows/2004/02/mit/task">
  <Triggers>
    <LogonTrigger><Enabled>true</Enabled><UserId>DOMAIN\user</UserId></LogonTrigger>
  </Triggers>
  <Principals>
    <Principal id="Author">
      <UserId>DOMAIN\user</UserId>
      <LogonType>InteractiveToken</LogonType>   <!-- the user's desktop session: WSL, the share, console windows -->
      <RunLevel>LeastPrivilege</RunLevel>
    </Principal>
  </Principals>
  <Settings>
    <MultipleInstancesPolicy>IgnoreNew</MultipleInstancesPolicy>  <!-- one agent; a second logon trigger is a no-op -->
    <DisallowStartIfOnBatteries>false</DisallowStartIfOnBatteries>
    <StopIfGoingOnBatteries>false</StopIfGoingOnBatteries>
    <ExecutionTimeLimit>PT0S</ExecutionTimeLimit>                 <!-- the default PT72H kills the agent after three days -->
    <RestartOnFailure><Interval>PT1M</Interval><Count>3</Count></RestartOnFailure>
    <Enabled>true</Enabled>
  </Settings>
  <Actions Context="Author">
    <Exec>
      <Command>C:\path\cdash-agent.exe</Command>
      <WorkingDirectory>C:\path</WorkingDirectory>                <!-- public/ beside the binary is found either way -->
    </Exec>
  </Actions>
</Task>
```

Configuration stays environment-based. A logon-triggered task inherits the
user's environment block, so `setx PORT 8080`, `setx CDASH_BIND 0.0.0.0`,
`setx CDASH_WSL_DISTRO Ubuntu` and the rest take effect on the next start.
The README documents this.

There is no pidfile. `MultipleInstancesPolicy=IgnoreNew` prevents a second
instance from the scheduler, and a port held by anything else is the existing
exit 3, which `RestartOnFailure` retries three times a minute apart before
giving up.

### 6. Hidden window, console and password prompt

`main.rs` gets
`#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]`.
A release build started by Task Scheduler therefore opens no window. At the
top of `main` on Windows, in every build, `AttachConsole(ATTACH_PARENT_PROCESS)`
is called and its result ignored — a debug build that already owns a console
gets an error and nothing changes. Started from a terminal, the agent's banner, warnings and
log echo appear there; started by the scheduler, there is no parent console
and output is discarded, which Rust's stdio does silently for a process with no
console handles. `/api/logs` remains the log surface either way. Known
cosmetic caveat, documented: a shell does not wait for a GUI-subsystem
process, so its prompt returns before the banner prints.

Debug builds keep the console subsystem, so `cargo run` and `cargo test` on
Windows behave normally.

`prompt_hidden` for `set-password` uses `rustix::termios` on Unix, as today,
and on Windows clears `ENABLE_ECHO_INPUT` with `GetConsoleMode`/`SetConsoleMode`
on the stdin handle, restoring it afterwards. The pipe fallback is unchanged.

Dependencies: `rustix` moves to `[target.'cfg(unix)'.dependencies]`, and its
two uses — `statvfs` in `disk.rs` and termios in `main.rs` — sit under
`cfg(unix)`; `windows-sys` with the `Win32_System_Console` feature is added under
`[target.'cfg(windows)'.dependencies]`. Every `Runner` spawn on Windows sets
`creation_flags(CREATE_NO_WINDOW)` so `wsl.exe`, `git`, `taskkill` and
`schtasks` never flash a console; `spawn_detached` is the one exception.

### 7. Stats, disks and PATH on Windows

- **CPU.** `System::load_average()` is all zeros on Windows, so `machine_stats`
  there uses `global_cpu_usage()`. `refresh_if_due` also calls
  `refresh_cpu_usage()`, under the same 200 ms rule and the same "first sample
  is a baseline" logic, so the first poll reports 0 and the second onward is
  real. The Unix formula is untouched.
- **Disk.** `disk_usage(mount)` on Windows finds the entry in
  `sysinfo::Disks::new_with_refreshed_list()` whose mount point equals `mount`
  and reports its available and total space in KiB. The root mount is
  `%SystemDrive%\`, typically `C:\`; `DISK_EXTRA` works as before, for example
  `D:\`. The WSL virtual disk is not reported.
- **PATH.** `probe_path` on Windows skips the login-shell probe and composes
  the inherited PATH with a backstop of `%USERPROFILE%\.local\bin`, where the
  native Claude installer puts `claude.exe`. `compose_path` takes the separator
  from a `PATH_SEP` constant, `;` on Windows and `:` elsewhere. Known
  locations become a function returning the platform's list.
- **Binaries.** `missing_binaries` splits on `PATH_SEP` and on Windows tests
  `<dir>\<bin>.exe` for `is_file`. `REQUIRED_BINARIES` is `["claude", "git"]`
  on Windows; tmux is required only where tmux is the backend, and the WSL
  side's binaries are reported under `wsl.missing` (§2).
- **Home.** Every `HOME` read — `serve.rs`, `routes.rs`, `spawn.rs` and the
  Tauri app — becomes `std::env::home_dir()`, which reads `$HOME` on Unix and
  the profile directory on Windows. No deprecation warning on the pinned
  toolchain.
- `/api/hostinfo` `platform` already reports `std::env::consts::OS`, so it
  says `windows`.

### 8. Browse root and crumbs

`Listing` gains `crumbs: Vec<Crumb { name, path }>`, built server-side from
`Path::components()`: the prefix and root form the first crumb, each normal
component appends one. On Unix the first crumb is `/`; on Windows it is `C:\`
or `\\wsl.localhost\Ubuntu\`, preceded by a virtual root crumb `/`.

On Windows the path `/` is the **roots listing**: one entry per drive letter
whose `X:\` exists, plus `\\wsl.localhost\<distro>\` when a WSL side exists,
with `parent: null` and `path: "/"`. The parent of a drive root or of the WSL
share root is `/`. `list_dirs` takes the roots from the route, which has the
context. Linux behaviour is unchanged: `/` is the filesystem root as before,
and its crumbs are what the client used to compute.

The client change is confined to `renderCrumbs`, which renders `d.crumbs`
instead of splitting `d.path` on `/`. Navigation, picking, favourites and the
dead-end guard are untouched. The `parent` field stays in the response.

### 9. Error handling

| Condition | Behaviour |
|---|---|
| `wsl.exe` missing, failing or timing out at boot | One log line; Windows side only; `hostinfo.wsl = null` |
| A WSL command fails or times out during a poll | Swallowed per call as today; the session list for that side is empty for that poll |
| A WSL command fails during launch, resume or kill | `run_checked` error → 500 with the reason, as today |
| Share unreachable mid-run | File reads return `None`; sessions on that side drop out of the list until it returns. No panic, no crash |
| `conhost` spawn fails | 500 with the OS error; no meta entry is written |
| Kill of a session whose process is already gone | `taskkill` exits non-zero → 500, matching the tmux path |
| A path that routes to a missing WSL side | 400 `bad path` naming the reason |
| `schtasks` fails | Its output on stderr, exit 2 |
| Port held at logon | Exit 3 as today; the scheduler retries three times, one minute apart |

### 10. Testing and verification

**Pure tests, every host, no `cfg`:** `parse_ps` including a command name with
a space; `parse_wsl_probe` for both share prefixes and for garbage;
`to_unc`/`from_unc` including the bare root and a foreign distro; `side_for`
for every row of the routing table on both platforms, using string inputs;
`task_xml` contains `PT0S`, `IgnoreNew`, `InteractiveToken`, a `LogonTrigger`
and the exe path; `compose_path` with the platform separator; crumbs for a
Unix path. The ownership split in the session-file scan is tested with the
existing temp-dir fixtures: a `cdash-` name yields `external: false` and
carries meta, another name yields `external: true`.

**Existing tests that need a shell** — the stub `tmux` scripts in `spawn.rs`
and `sessions.rs`, `false`/`sleep` in `cmd.rs`, `PermissionsExt` in `probe.rs`,
`/` in `disk.rs`, the login-shell probe — are marked `#[cfg(unix)]`. Windows
gains cheap equivalents where they exist: `cmd /c echo` for the runner's
success path, `C:\` for disk, a `.exe` file for the binary check, and crumbs
for a drive and a UNC path.

**CI.** A `windows` job on `windows-latest` with the pinned toolchain:
`cargo test -p cdash-agent --locked`, `cargo clippy -p cdash-agent --all-targets
--locked -- -D warnings -D clippy::disallowed_types`, `cargo build --release
--locked -p cdash-agent`, then a boot gate that starts the release binary with
`PORT=18080`, waits, requests `/api/health`, asserts `{"ok":true}` and stops the
process. The gate is a health request rather than a banner grep because the
release binary is GUI-subsystem. The exe is uploaded as
`cdash-agent-x86_64-pc-windows-msvc`. The job adds NASM via
`ilammy/setup-nasm` for `aws-lc-sys`; CMake is on the image. The Tauri crate
is not built on Windows.

**Unverifiable here, listed so the first Windows run checks them in order:**

1. `tokio::fs` reads and `read_dir` over `\\wsl.localhost\…` under a 4-second poll.
2. `wsl.exe --exec` delivers the `--settings` JSON argument to `tmux` unchanged.
3. `conhost.exe claude …` opens a window with a working `claude` in it, from a
   GUI-subsystem parent.
4. `schtasks /Create /XML` accepts the generated UTF-16 file and the task
   fires at logon with an interactive token.
5. `AttachConsole` makes the banner visible when run from a terminal.
6. The `--name` value appears as `name` in `sessions\<pid>.json`.

### 11. Deployment

The README gains a *Windows* section: download `cdash-agent.exe` and the
`public/` directory from the CI artifact into one folder, run
`cdash-agent.exe install` once, configure with `setx`, remove with
`cdash-agent.exe uninstall`. Requirements: the native Claude Code installer,
Git for Windows, and for the WSL side a WSL 2 distro with `tmux`, `claude` and
`git` on its login-shell PATH. `scripts/release.sh` is unchanged; the Windows
binary comes from CI.

## Out of scope

- Multiple WSL distros. The side list makes this a later loop, not a redesign.
- WSL disk usage in the stats bar.
- Cross-compiling the Windows binary from Linux.
- A startup trigger. Session 0 cannot reach either side; the nearest thing is
  Windows auto-logon, which is the user's choice.
- npm-installed Claude Code (`claude.cmd`).
- The Tauri client on Windows. Consequence worth recording: with this crate
  compiling natively, Tauri on Windows can link the agent in-process exactly as
  Linux and macOS do, which deletes the old spec's copy-in, readiness probe and
  pidfile design for Windows entirely.
