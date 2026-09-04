# Adversarial review — Windows agent with WSL reach-through

Date: 2026-09-03
Subject: `2026-09-03-windows-agent-design.md`
Format: turn-based Critic / Writer / Moderator debate, one issue at a time,
settled by refutation, concession, fix, accepted risk or ruling. Mechanically
checkable claims were verified before any turn (ledger below). The Moderator
consulted [ponytail](https://github.com/DietrichGebert/ponytail) on every
proposed fix and applied the ladder: delete before add, native platform
feature before code, one call before a listing.
Outcome: **terminated on all criteria.** 9 material issues (3 HIGH, 6 MED)
and 6 LOW; 13 fixed, 2 accepted as documented risk with a checklist entry,
2 Critic objections withdrawn on evidence, 0 struck by the Moderator, 0
rulings needed. **Every agreed fix is applied to the design doc.**

---

## Frame

**Goals.** G1 a native Windows agent · G2 started at every logon with no
window · G3 both sides, native and WSL, listed / launched / resumed / killed ·
G4 verifiable through CI with no Windows machine · G5 the smallest design
that holds (ponytail).

**Constraints.** K1 `clippy.toml` forbids raw `Command`; every spawn goes
through the time-boxed `Runner` · K2 no Windows machine for the implementer ·
K3 Linux behaviour and its tests unchanged · K4 every session runs with
`--dangerously-skip-permissions`, so launch and kill inputs are the security
boundary.

**Success criteria.** S1 correctness: a kill never reaches a foreign process,
password entry works · S2 every claim is verified or on the "unverifiable
here" list · S3 the 4-second poll has no unbounded latency · S4 Linux parity ·
S5 smallest sufficient change.

**Evidence tiers.** E0 verified in this repo · E1 source doc and code · E2
domain standards · E3 external documentation or source · E4 declared
assumption.

**Ruling before any turn.** Windows and WSL runtime behaviour can only be E3
or E4 here. The doc's "unverifiable here" list is E4 by declaration:
attacking an item on it as unverified is struck; attacking its consequences,
or an omission from the list, is in frame.

---

## Verified evidence ledger, established before the debate

| # | Doc claim | Result |
|---|---|---|
| V1 | `Ctx::new` keeps its signature so fixtures compile | **Confirmed**, with a condition: fixtures build `Host` by struct literal (`spawn.rs:266`, `sessions.rs:223`, `external.rs:162`), so `Host` must gain no field. Added to §1 |
| V2 | The session scan filters on live pid + `claude`/`claude.exe` + `entrypoint == "cli"` | **Confirmed** (`external.rs:59-91`) |
| V3 | `poll_rc_link` takes a pid | **Confirmed** (`spawn.rs:93`) |
| V4 | `kill_session` = `assert_kill_name` then `tmux kill-session` | **Confirmed** (`spawn.rs:228`) |
| V5 | `claude_json_path` resolves to the same file with and without `CLAUDE_DIR` | **Confirmed** by case analysis of `spawn.rs:55-61` |
| V6 | Four `HOME` reads | **Confirmed** (`serve.rs:47`, `routes.rs:69`, `spawn.rs:59`, tauri `main.rs:19`) |
| V7 | `assert_path` is `Path::is_absolute` | **Confirmed** (`validate.rs:29`) |
| V8 | `Runner` time-box and its reason | **Confirmed** (`cmd.rs:6-9`: a 9P `git status` once stalled every poll for over 60 s) |
| V9 | `REQUIRED_BINARIES`, `:` split, `PermissionsExt` | **Confirmed** (`probe.rs`) |
| V10 | `compose_path` hard-codes `:` and Unix locations | **Confirmed** (`path.rs:5,12`) |
| V11 | The shell-dependent tests listed for `cfg(unix)` | **Incomplete**: `sessions.rs:322` asserts the root mount is `/` and was not listed. Added |
| V12 | `System::load_average()` is all zeros on Windows | **Refuted.** sysinfo computes a Windows value from the `\System\Processor Queue Length` PDH counter every 5 s (`src/windows/cpu.rs`); it counts waiting threads, not running ones. The conclusion (`global_cpu_usage`) stands; the reason is corrected |
| V13 | The client change is confined to `renderCrumbs` | **Refuted in part.** `openPicker` (`app.js:290`) seeds only `/`-prefixed input and `placeRow` (`app.js:322`) splits on `/` |
| V14 | `rustix` unconditional; `windows-sys` new | **Confirmed** / **overstated**: `windows-sys` is already in the lock at four versions via tokio and friends |
| V15 | CI gate is a banner grep via `cargo run` | **Confirmed** (`ci.yml`) |
| V16 | `wsl.missing` reaches the setup screen | Not claimed; no client reads `/api/hostinfo` at all |
| V17 | Integration tests are shell-free | **Confirmed** |
| V18 | The log echoes to stderr | **Confirmed** (`log.rs:29`) |
| V19 | `--name`, `--remote-control [name]`, inline `--settings`, `--resume <id or name>` exist | **Confirmed** (E3, CLI reference) |
| V20 | `PT0S` disables the time limit | **Confirmed** (E3) |
| V21 | `RestartOnFailure` retries an exit 3 three times | **Refuted.** Task Scheduler counts only an action it could not *start* as a failure; two independent archived answers, one from 2012, one from 2020 (E3) |
| V22 | Task priority: not mentioned | **Gap.** Default `Priority` 7 is `BELOW_NORMAL_PRIORITY_CLASS`, `IoPriorityLow`, `MEMORY_PRIORITY_LOW`; `CreateProcess` passes BELOW_NORMAL on to children (E3, both quoted from Microsoft docs) |
| V23 | `CREATE_NEW_CONSOLE` semantics | **Confirmed** (E3); `CREATE_NO_WINDOW` is ignored when combined with it |
| V24 | `claude` is resolved by `CreateProcess` through the runner's PATH | **Overstated.** Rust's `Command` searches the child's `PATH` first when set via `env`, then the exe's directory, System32, Windows, the parent's PATH (E3). Same outcome; wording fixed |
| V25 | `std::env::home_dir` warns on no toolchain | **Confirmed** (E3: not deprecated; `USERPROFILE` on Windows) |
| V26 | `AttachConsole` gives a usable console from a terminal | **Confirmed for output, refuted for input.** The launching shell keeps reading the same console (Microsoft Q&A; Tillett) |
| V27 | A console app started by a logon task opens a window | **Confirmed**, and with Windows Terminal as the default terminal it is a Windows Terminal window (terminal#15887) |
| V28 | WSL idle behaviour | E3: an instance stops about 15 s after its last process, the VM 60 s later (`vmIdleTimeout`) |
| V29 | Native installer layout on Windows | **Partial.** `bootstrap.ps1` downloads to `%USERPROFILE%\.claude\downloads`, runs `<binary> install`, deletes the download; what `install` leaves at `.local\bin\claude.exe` (copy or shim) is undocumented |
| V30 | `sysinfo::Disks` on Windows | E3 from source: `FindFirstVolumeW` loop, fixed and removable only, per volume `GetVolumeInformationW`, `GetDiskFreeSpaceExW`, `CreateFileW` + `DeviceIoControl` |
| V31 | NASM for `aws-lc-sys` | **Confirmed** (lock has `aws-lc-sys 0.44`; E3) |

---

## Debate record

### I1 — HIGH — §6: one GUI-subsystem binary breaks `set-password` and hides exit codes

**Critic.** V26, V27. A GUI-subsystem `cdash-agent.exe set-password` attaches
to the shell's console, but the shell has already returned to its prompt and
is reading the same console: keystrokes are split between two readers and
`SetConsoleMode` fights the shell's line editor. `install` exits 2 into a
shell that is not waiting. The password path is security-relevant (K4).

**Writer.** Concedes. A README note to run subcommands under `cmd /c` was
considered and rejected: a footnote on a password prompt. Smallest sufficient
fix is the `python`/`pythonw` idiom: a second `[[bin]]`, `cdash-agentw`, GUI
subsystem, serve only, about twenty lines calling the same `serve`;
`default-run = "cdash-agent"`; `test = false` on the GUI bin; `install`
registers `cdash-agentw.exe` beside itself. This deletes the `cfg_attr`, the
`AttachConsole` call, the "prompt returns before the banner" caveat and
checklist item 5.

**Critic.** Accepts. Checks: the Unix build needs a stub `main`; `release.sh`
builds `-p` and copies a named path, so it is unaffected; the CI artifact
must carry both exes; the Linux gate's `cargo run` needs `default-run`. All
in the fix.

**Moderator, ponytail.** The fix removes more than it adds. **Settled: fixed.**

### I2 — HIGH — §3 Kill: the scan lacked the liveness filter

**Critic.** V2. Session files outlive their process and pids are recycled.
`taskkill /T /F /PID` on a recycled pid kills whatever owns it now. Step 1
said only "scan session files for `name`".

**Writer.** The intent was the same scan as the list; concedes the text did
not say so. Fix: kill step 1 applies the identical predicate — pid live,
process `claude`/`claude.exe`, `entrypoint == "cli"` — before matching
`name`; no match is a 500 and no `taskkill`.

**Critic.** Accepts. **Settled: fixed.**

### I3 — HIGH — §5/§9: `RestartOnFailure` does not restart an exit 3; no crash recovery

**Critic.** V21. Two statements were false, and the consequence is worse than
the wording: a held port at logon, or any panic, leaves the agent dead until
the next logon, silently.

**Writer.** Concedes both statements. Fix that also buys recovery: a
`<Repetition>` of `PT5M` on the logon trigger. `IgnoreNew` makes each tick a
no-op while the agent lives; once it is gone the next tick starts it. Drop
`RestartOnFailure`, which covered only "the exe could not be started".
`install` ends by printing the URL, since the scheduled instance's exit is
invisible.

**Critic.** Accepts; asks the doc to state the bound (five minutes, while
logged on) and the cost (an "already running" history event every five
minutes; history is off by default). Both stated.

**Moderator, ponytail.** Native platform feature over a watchdog.
**Settled: fixed.**

### I4 — MED — §5: default task priority 7

**Critic.** V22. The agent and every `claude` it spawns run BELOW_NORMAL with
low I/O and memory priority.

**Writer.** `<Priority>4</Priority>`; `task_xml` test asserts it.
**Settled: fixed.**

### I5 — MED — §3: `conhost.exe` as a launcher

**Critic.** V23. `conhost.exe <cmd>` is undocumented behaviour;
`CREATE_NEW_CONSOLE` is the documented flag with the same effect, one fewer
process, and the returned pid is the child's. Proposed also dropping
`ByName` since the pid is now known.

**Writer.** Accepts the flag. Refutes dropping `ByName`: V29 leaves open that
`.local\bin\claude.exe` is a shim, in which case the pid is the shim's; and
kill must work after an agent restart, when no pid is remembered. `ByName`
stays for both.

**Critic.** Withdraws the `ByName` half. **Settled: fixed** (flag); **withdrawn** (`ByName`).

### I6 — MED — §1/§9: no latency bound on the WSL side

**Critic.** V8. The 5-second time-box exists because one 9P stall froze
every poll for a minute; `tokio::fs` over `\\wsl.localhost` has no box, and
§9 promised only "returns `None`".

**Writer.** Wrap the WSL side's collection in
`tokio::time::timeout(DEFAULT_TIMEOUT)`; on expiry the side is empty for that
poll, logged once. Known ceiling: the blocking-pool thread stays parked until
the redirector answers.

**Critic.** Accepts the ceiling as documented risk. **Settled: fixed + accepted risk.**

### I7 — MED — §5: re-install with a running agent is a silent no-op

**Critic.** `/Create /F` replaces the definition, `/Run` is ignored under
`IgnoreNew`, the old binary keeps running.

**Writer.** `install` runs `schtasks /End` first, failure ignored. This is also
how `setx` changes are applied. Upgrade path documented. **Settled: fixed.**

### I8 — MED — §2: polling keeps the WSL VM resident; no off switch

**Critic.** V28. Two spawns per 4 s reset the idle timers; a user with WSL
installed and no Claude in it pays for `vmmem` permanently.

**Writer.** Document the cost; add `CDASH_WSL=0`. Rejected: polling only when
`wsl -l --running` shows the distro, which is another UTF-16 spawn and a
parser per poll for the case the switch covers.

**Critic.** Accepts. **Settled: fixed** (switch, documented cost).

### I9 — MED — §10: the Windows process name is unverified and off the checklist

**Critic.** V29. If `.local\bin\claude.exe` is a shim, the live process is
`<version>.exe` and no native session ever lists.

**Writer.** Cannot verify here (E4). Checklist item 1, with the fallback
named: accept images under `.local\share\claude\versions\`.
**Settled: accepted risk, on the checklist.**

### LOW, fast-tracked, unchallenged

- **L1** §7 CPU reason corrected (V12).
- **L2** §7 disk: one `GetDiskFreeSpaceExW(mount)` replaces the `Disks`
  listing (V30): the same shape as `statvfs(mount)`, no per-volume handle and
  `DeviceIoControl` per poll, and a `DISK_EXTRA` on a mapped drive is not
  silently skipped. The Critic's first form of this — "the listing blocks on
  network drives" — was **withdrawn** on V30: sysinfo skips them.
- **L3** §8 client: `openPicker` seeds any non-empty value; `placeRow` splits
  on both separators (V13).
- **L4** §9: trust write over the share fails → logged, launch proceeds,
  `claude` shows its own prompt.
- **L5** §10: the `sessions.rs` root-disk assertion joins the `cfg(unix)`
  list (V11); `Host` gains no field (V1).
- **L6** §3: PATH resolution attributed to Rust's `Command` (V24); the
  cross-reference to §6 corrected to §7.

---

## Final ledger

| # | Sev | Issue | Disposition | Depends on |
|---|---|---|---|---|
| I1 | HIGH | GUI binary breaks `set-password`, hides exit codes | Fixed: two bins | — |
| I2 | HIGH | Kill scan without liveness filter | Fixed | V2 |
| I3 | HIGH | `RestartOnFailure` never fires on exit; no crash recovery | Fixed: PT5M repetition | I7 (`/End` before `/Run`) |
| I4 | MED | Task priority 7 inherited by sessions | Fixed: `Priority` 4 | — |
| I5 | MED | `conhost` launcher | Fixed: `CREATE_NEW_CONSOLE`; `ByName` kept | I9 |
| I6 | MED | WSL side unbounded latency | Fixed + accepted ceiling | V8 |
| I7 | MED | Re-install is a silent no-op | Fixed: `/End` first | I3 |
| I8 | MED | WSL VM kept resident, no switch | Fixed: `CDASH_WSL=0`, cost stated | — |
| I9 | MED | Windows process name unverified | Accepted risk, checklist 1 | — |
| L1–L6 | LOW | Wording, client seams, cfg list, disk call | Fixed | — |

**Accepted risks carried (E4).** R1 a parked blocking-pool thread per stalled
WSL read (pool of 512). R2 the WSL VM stays resident while the bridge is on.
R3 `.local\bin\claude.exe` may be a shim; fallback named. R4 the task XML
element order and `schtasks` acceptance are checklist item 5.

---

## Integration round

The Critic re-read the patched doc end to end for regressions, conflicts,
drift and new problems.

- **Two binaries** vs. the Linux gate: `default-run` keeps `cargo run -p
  cdash-agent` meaning the console binary; `release.sh` copies a named path.
  No conflict.
- **`CREATE_NEW_CONSOLE` from both exes**: from `cdash-agentw.exe` (no
  console) it creates one; from `cdash-agent.exe` in a terminal it gives the
  session its own window rather than the agent's terminal. Consistent with §6.
  **Corrected after the review (E0, `library/std/src/sys/process/windows.rs`
  on the pinned toolchain):** the second half is wrong. For `Stdio::Inherit`
  std duplicates a console parent's standard handles and sets
  `STARTF_USESTDHANDLES` whenever any of them is non-null; the flag does not
  override handles that were given. So from `cdash-agent.exe` in a terminal
  the session has its own window but its I/O is the agent's terminal. From
  `cdash-agentw.exe` the handles are null, the flag stays clear, and the
  child takes its new console's handles — the production path is unaffected.
  Disposition: accepted as a documented limitation of the console binary
  (§3, §11); `conhost.exe <argv…>` named as the fallback. The I5 verdict
  stands for the scheduled instance; its "works whether the agent has a
  console or not" wording was the error.
- **`/End` → `/Create` → `/Run` plus the repetition plus `IgnoreNew`**: no
  path to two instances. Consistent.
- **The time-box in §1** wraps `wsl.exe` spawns that already carry their own
  5-second box; same value, so a warm poll is unaffected.
- **Dependencies after I1**: `Win32_System_Console` is still needed for the
  password prompt; `Win32_Storage_FileSystem` added for L2; the two creation
  flags are literals. No orphaned feature.
- **Drift found and fixed**: the Topology row still said one exe; the §9 kill
  row had lost its tmux half; four paragraphs were left as over-long lines by
  the patch.
- **New problems**: none material. Noted, not acted on: `PORT` is a common
  variable name for `setx`; the Linux agent uses the same name, so consistency
  wins.

---

## Method note

Web verification was spent where a wrong belief changes the design: the
scheduler's restart semantics (V21) and priority default (V22), the console
input race (V26), the Windows Terminal handoff (V27), and sysinfo's Windows
internals (V12, V30). Everything about `wsl.exe --exec` quoting, the P9 share
and the installer's launcher stays E4 and on the checklist, ordered so the
first Windows run answers the cheapest question first.
