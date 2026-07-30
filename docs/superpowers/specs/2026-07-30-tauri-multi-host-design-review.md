# Adversarial review — Tauri multi-host design

Date: 2026-07-30
Subject: `2026-07-30-tauri-multi-host-design.md`
Format: turn-based Writer / Critic / Moderator debate, 4 rebuttal rounds plus an
integration pass. Pass A defended the doc's existing decisions; Pass B proposed
amendments.
Outcome: **terminated on all criteria.** Zero open HIGH objections; every
objection dispositioned; both roles independently confirmed internal consistency.

---

## Frame

**Goals.** G1 web (local or VPS) · G2 local desktop app · G3 desktop/mobile
client onto a VPS whose address is unknown at build time.

**Constraints.** K1 backend cannot move client-side · K2 CF Access cannot
authenticate a webview navigation · K3 Android cannot spawn in Termux ·
K4 the origin is RCE-equivalent.

**Success criteria.** S1 correctness · S2 feasibility (one person, no build step
for the host agent) · S3 safety (safety defects HIGH by default) · S4
consistency · S5 preserved scope, judged as a personal tool · S6 incrementality.

**Quality criteria for subjective points.** Q1 "too complex" needs a named cost ·
Q2 "cleaner" struck unless tied to S1–S6 · Q3 "under-specified" must name the
undecidable · Q4 other-projects comparisons lose to repo facts · Q5 multi-user
is out of frame, single-user-on-a-named-platform is in.

**Evidence tiers.** E0 verified in this repo/container · E1 source doc and code ·
E2 domain standards · E3 external · E4 declared assumption.

**Two rulings issued before any turn.** The doc's four Cloudflare "items to
confirm" were declared E4; attacking them as unverified was struck as redundant,
attacking the consequences of them being wrong was in frame. No role could claim
E0 for Cloudflare, macOS, Windows/WSL, Termux or Android behaviour.

---

## Verified evidence ledger (E0), established before the debate

| # | Doc claim | Result |
|---|---|---|
| V1 | Dependencies = `{express}`; `jose` makes two | **Confirmed** |
| V2 | `api()` at `app.js:112` is the sole UI `fetch` | **Confirmed** |
| V3 | UI otherwise unchanged | **Refuted in part.** `api()` discards the HTTP status; `poll()` uses a bare `catch {}` |
| V4 | Bind call at `server.js:70` | **Off by one** — line 71 |
| V5 | `/api/health` is new | **Refuted in part.** Already exists at `server.js:17` |
| V6 | `--dangerously-skip-permissions` at `collect.js:136` | **Confirmed** |
| V7 | `df` at `collect.js:223` | **Confirmed**; queries `/` **and** `DISK_EXTRA` in one invocation |
| V8 | `ps` flags at `collect.js:222` | **Confirmed**, verified running |
| V9 | A missing binary fails silently | **Overstated.** Logged once per command via `shFailed`, surfaced at `/api/logs` |
| V10 | Linux `df -k --output=avail,size <path>` → idx 0, 1 | **Confirmed** by execution |
| V11 | The `df` fix is contained | **Refuted as "contained."** Signature + caller + tests all change |
| V12 | `rcLink` built at `collect.js:90` | **Incomplete** — three sites; conclusion unaffected |
| V13 | `sw.js` shell + same-origin `/api/` assumptions | **Confirmed**; registered unconditionally |
| V14 | `/api/browse` enumerates from `/` | **Confirmed**, no root confinement |
| V15 | Guard placement vs. `express.static` | **Gap** — never stated in the doc |
| V16 | Test suite is pure-function only | **Confirmed**; no server-boot test exists |
| V17 | No CORS headers | **Confirmed** |
| V21 | CSRF posture of the POST endpoints | Facts confirmed; exploitability **not** verified |
| V22 | Baseline | 22/22 green |

---

## Final ledger — Pass A

| Change | Verdict | Objections (severity) | Disposition |
|---|---|---|---|
| **C1** One codebase, runtime profiles | **Adopted** | — (clean pass) | G3's build-time-unknown address mechanically excludes build variants |
| **C2** Rust-proxied `api_request` | **Adopted with revisions** | C2-1 secrets *do* enter JS at entry (MED) | Revised: true weaker property stated; `profiles_list` returns `has_secret` |
| **C3** Transport shim | **Adopted with revisions** | C3-1 `app.js` is a classic script; PWA breaks offline; `cdash-v4` never invalidates (MED) · R2-1 the classic-script fix cannot publish its global in time (MED) | Four **statically declared** scripts; `select.js` selects, never loads; `SHELL` + `CACHE` fixed |
| **C4** `lib/host/` PATH + binary chain | **Adopted with revisions** | C4-1 two mechanisms, neither primary; `server.js:63` outside `lib/` (MED) | Binary resolution returns absolute paths, 7 sites; `killSession` moves into `lib/` |
| **C5** `df` contract change | **Adopted with revisions** | C5-1 `sh()` dedupe key is the constant `"df -k"`; per-mount failures go silent (MED) | `sh()` gains an explicit `key`; `collect.js:41`'s positional timeout converted |
| **C6** Guard chain | **Adopted with revisions** | C6-1 AND-composed `bearer,cf-access` locks browsers out of a G1+G3 origin (MED) · C6-2 guard placement unstated (MED) | `CDASH_AUTH=cf-access` serves both; "bearer-first" struck; registration order stated |
| **C7** Bind `127.0.0.1` | **Adopted** | — (clean pass) | Two lines; converts the verified worst-case topology into a deliberate act |
| **C8** Health + hostinfo | **Adopted with revisions** | C8-1 no `version` field exists, yet three behaviours read one (MED) | `"version": "0.1.0"` added; client/host coupling stated; stale copy-in volunteered |
| **C9** Managed server per platform | **Adopted with revisions** | C9-1 WSL bind unspecified while the no-auth argument rests on it (MED) · R2-3 credentialed WSL server + unauthenticated readiness probe strands the user (MED) | Class 2 token; two-step readiness; pidfile repurposed to reclamation |
| **C10** Secret storage | **Adopted with revisions** | — (clean pass, then reopened on lifecycle) | Two credential classes + the governing rule |
| **C11** Error handling | **Adopted with revisions** | C11-1 the SW serves `/` cache-first, so the CF reload remedy never executes (MED) · C11-2 naming the failed guard leaks config to an unauthenticated caller (MED) | Navigations network-first; rejection body `{error:"unauthorized"}` |
| **C12** Testing + sequencing | **Adopted with revisions** | C12-1 the integration test is unwritable — no exports, env port logged (MED) · C12-2 assertions too narrow, blind to the placement gap (MED) · C12-3 C11 has no sequence position; step 2 creates the hazard C11 forbids (MED) | `createApp(ctx)`; router-enumerated assertions + `/` and `/sw.js`; 9-step sequence |

## Final ledger — Pass B amendments

| ID | Verdict | Severity | Basis |
|---|---|---|---|
| **A1** Dual-loadable `globalThis` convention for testable UI logic | **Adopted** | MED | `public/` had zero coverage as safety-relevant logic moved into it; verified working by both roles independently |
| **A2** `index.html` ↔ `sw.js` `SHELL` consistency test | **Adopted** | MED | The class recurred three times in three rounds, caught by a human each time; verified green against HEAD |
| **A3** Dependency-budget honesty | **Adopted** | LOW | "One dependency to two" is true of the host agent only |
| **A4** WSL copy-in cleanup + uninstall | **Adopted** | LOW–MED | The only place the design writes state where an uninstaller cannot reach |

## Final ledger — integration round

| ID | Verdict | Severity | Disposition |
|---|---|---|---|
| **INT-1** Step 2 adds a script tag but not the `SHELL` entry; the enforcing test is at step 5 | **Fixed** | MED | The shell-manifest test moves to **step 1**, ahead of every `public/` change |
| **INT-2** Reclamation's only input is a pidfile with unspecified write ordering; `EADDRINUSE` is a fatal uncaught throw | **Fixed** | MED | Five pidfile rules; write only after `listening`; exit 3 on `EADDRINUSE`; never show a pid |
| **INT-3** No step owns the service-worker mode gate | **Fixed** | LOW | Assigned to step 5, which already edits `index.html` and bumps `CACHE` |
| PATH-probe timeout (Moderator-ruled) | **Fixed** | — | 2000 ms / `SIGKILL`, stated as arbitrary; falls back to inherited PATH, never gates `listen` |
| C8 version-banner sentence | **Corrected** | — | Reported by the Critic rather than filed, as it could construct no failure from it |

---

## Resolved vs. unresolved

**Every objection was resolved by agreement.** Nothing required a Moderator
ruling to break a deadlock. Two Moderator rulings were issued on process rather
than substance:

1. **The PATH-probe timeout must be closed.** Flagged by the Writer in Round 1
   as something it would not paper over, discarded by the Critic as duplicative,
   still open after four rounds. Ruled that declining to decide was no longer
   available. Closed at 2000 ms.
2. **Reopening scope.** C1 and C7 not reopened; C10 reopened narrowly on the
   ephemeral-credential-lifecycle question only, adopting the Critic's own
   scoping and rejecting the broader one it declined to seek.

**One item reported but not filed.** The Critic believed C8's version-banner
sentence was factually wrong, could construct no live failure from it, and
reported it for correction rather than filing it as a defect. The Writer agreed
and corrected it.

---

## Assumptions carried (E4)

- The four Cloudflare facts in the doc's "Items to confirm." If the header name
  is wrong the guard **fails closed**, which is the correct direction; if
  `common_name` is absent for service tokens, the Tauri VPS client cannot
  authenticate, which is the consequential risk.
- macOS BSD `df -k` column indices (avail at 3, size at 1). A wrong index gives
  *incorrect* numbers rather than absent ones, which is worse than today.
- WSL2 loopback forwarding. Deliberately **neutralised** — the Class 2 token
  makes the safety posture independent of the answer.
- `wsl.exe -l -q` emits UTF-16LE; a parsing bug shows as an empty distro dropdown.
- Android app-private storage as equivalent to Termux's own protection: true for
  a non-rooted device, weaker than hardware-backed Keystore.
- A BSD `df` fixture must be captured from a machine not available here; until
  it is, the macOS branch is tested against a hand-written string.

---

## Debate record

**Critic:** 22 objections raised (0 HIGH, 19 MED, 3 LOW), 21 sustained, 1
withdrawn, 0 struck by the Moderator. 3 clean passes. 7 objections self-discarded
before filing, including a traced refutation of its own CSRF theory. 4
compositions constructed and reported as unbreakable in the integration round.
Found no frame drift and said so on the merits.

**Writer:** 6 concessions volunteered before being attacked. 18 REVISE, 1
REBUT+REVISE, 0 silent concessions, 0 defects carried as undocumented risk.
Volunteered three defects created by its own remedies. Declined to argue anything
back in from §Out of scope, item by item.

**Two errors each side acknowledged.** The Critic's forcing argument on guard
placement was refuted by a counterexample it ran itself, and its leak-mechanism
attribution was wrong (the dedupe key strips the path; `e.message` carries it).
The Writer's Round 1 "complete enumeration" for the `df` change was not complete,
and its "no execution-timing change" claim held only for an arrangement its own
file list contradicted.

**The best exchange.** OBJ-R2-3: an unauthenticated liveness probe used as a
readiness signal, composed with a teardown gap the design already admitted it
could not test. The response — repurposing the pidfile from teardown to
reclamation — turned a defect into a capability the design did not previously
have, and the residual gap in that remedy (INT-2) was found and closed in the
integration round.

---

## Method note

Every mechanically checkable claim was checked rather than argued. That is what
produced the debate's substance: the guard-placement counterexample, the
dual-loadable module convention, the `EADDRINUSE` semantics, the constant
dedupe key, the missing `version` field, the discarded CSRF theory, and the
shell-manifest test that passes green today were all settled by running code, not
by reasoning about it. The findings that could not be checked — everything on
macOS, Windows, WSL, Android and Cloudflare — are the ones marked E4, and they
are where the remaining risk lives.
