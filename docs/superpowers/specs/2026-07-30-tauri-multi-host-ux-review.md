# User-experience review — every client × target × lifecycle combination

Date: 2026-07-30
Reviews: `2026-07-30-tauri-multi-host-design.md`
Status: findings open; resolutions to be folded into the design and the plans

## Scope

Every combination a user can actually be in, worked through for what they *do*,
not what the system does. Three axes:

- **Client** — desktop browser, mobile browser (PWA), Tauri Linux, Tauri macOS,
  Tauri Windows, Tauri Android
- **Target** — the same machine (localhost), or a VPS. **The VPS is always
  Linux**, so target-side OS variation exists only in the localhost column.
- **Lifecycle** — first run, ordinary launch, session expiry, server restart,
  passphrase change, offline, missing host binary

The Tauri client's own OS matters for *spawning* the host agent. The VPS being
uniformly Linux means every remote path collapses to one server behaviour, and
all remaining OS variance is local. That asymmetry is worth naming: **five of the
six clients have an identical VPS experience, and a completely different
localhost experience.**

## Matrix 1 — client × target

| Client | → localhost | → VPS |
|---|---|---|
| Desktop browser | Server started by hand (`npm start`). `auth=none`. No login. | `auth=password`. Sign in at `/login`. |
| Desktop browser (Windows) | Server started by hand **inside WSL**; browser reaches it at `localhost:8080` via WSL forwarding | Identical to any other browser |
| Mobile browser / PWA | Termux server. **`auth=password`** (loopback is not a perimeter on Android). Sign in at `/login`. | `auth=password`. Sign in at `/login`. Installable. |
| Tauri Linux | Managed Node sidecar, `auth=none`, random free port. No login. | Passphrase typed once at profile setup; silent thereafter. |
| Tauri macOS | Same, plus the `tmux` setup screen if Homebrew hasn't provided it | Same as Linux |
| Tauri Windows | Managed sidecar **inside WSL**, `auth=none`, pidfile teardown | Same as Linux |
| Tauri Android | **Cannot spawn.** Termux server run by the user, `auth=password` | Same as Linux |

## Matrix 2 — lifecycle, by client class

"Types passphrase" is the only column that matters; everything else is the
system's problem.

| Moment | Tauri (any OS) | Browser / PWA | Local `auth=none` |
|---|---|---|---|
| First run | Types once, at profile setup | Types once, at `/login` | Nothing |
| Ordinary launch | **Nothing** — keychain login, one round trip | **Nothing** if the cookie is live | Nothing |
| Session expires (12 h) | **Nothing** — 401 spends the launch attempt, retries | **Types passphrase again** | n/a |
| Server restarts | **Nothing** — same path | **Types passphrase again** | Nothing |
| Passphrase changed | Types once, via **Update password** | Types once, at `/login` | n/a |
| Offline | Disconnected indicator; shell from cache (2nd visit on) | Same | Same |
| `tmux` missing | Setup screen naming `brew install tmux` | Same via `/api/hostinfo` | Same |

The asymmetry in rows 3 and 4 is the design's largest UX cost and is the subject
of **UX-2**.

---

# Findings

## UX-1 (HIGH) — first run on a VPS is the hardest experience in the design, and is entirely undesigned

Every VPS path in both matrices begins with a step that has no design coverage.
To reach the first `/login` page a user must: SSH in, clone, `npm install`,
`npm run set-password`, choose `CDASH_AUTH`, set `CDASH_BIND`, set
`CDASH_PUBLIC_URL`, install and configure a reverse proxy, obtain a TLS
certificate, arrange a process supervisor so the agent survives reboot, and open
a firewall port.

The spec designs exactly one of those (`npm run set-password`) and mentions a
reverse proxy only as an assumption. This is the gate on delivery modes 1 and 3
for every one of the six clients, and it is the single least-specified part of
the design.

It also interacts with a boot check: `CDASH_PUBLIC_URL` must be `https://` or the
agent refuses to start. A user who has not yet configured TLS meets a refusal
before they ever see the product.

**Recommendation.** A VPS quickstart carrying a sample `Caddyfile` and a sample
systemd unit. Caddy is the right default precisely because it obtains a
certificate automatically — it turns "configure TLS" into a two-line file, which
is what makes the `https://` boot requirement reasonable rather than hostile.
Belongs to **Plan A**, since it is the deployment story for the work Plan A ships.

## UX-2 (HIGH) — browsers re-authenticate far more often than the design implies, and a restart is both a panic button and a daily logout

Sessions are a 12-hour absolute lifetime with no sliding renewal, held in an
in-memory `Map`. Both properties are deliberate and well-argued. Their combined
UX consequence is not stated anywhere:

- Every VPS restart — deploy, `unattended-upgrades` reboot, a crash and systemd
  restart — signs out **every browser**, immediately.
- The 12-hour boundary signs out every browser again, mid-session, with no
  warning and no renewal on activity.
- Tauri clients are unaffected in both cases: they hold the passphrase in the
  keychain and log in again silently.

So the browser user retypes a ≥12-character passphrase on a schedule set by their
VPS's uptime, while the desktop user never types it at all. "A restart is a
working panic button" and "a restart logs out every browser every time you
deploy" are the same sentence.

This is not obviously wrong — it may be exactly the trade you want for an origin
that grants RCE. It is wrong to leave it undocumented, because it is the property
most likely to make someone reach for a longer lifetime later without seeing what
it was buying.

**Recommendation.** State it in Tradeoffs. Do **not** add sliding renewal (it
converts a 12-hour bound into an unbounded one for an active attacker) and do
**not** persist sessions to disk (that is the class-2 credential already
rejected). If it proves intolerable in practice, the honest lever is the lifetime
constant, changed knowingly.

## UX-3 (HIGH) — Tauri Android's value over the installable PWA is one narrow property, and it should be stated before step 8 is built

The PWA already reaches both targets a phone needs, is installable to the home
screen, and needs no Android toolchain. Tauri Android cannot spawn the Termux
server, so it adds no capability there either. Walking the matrix, exactly one
row differs: **session expiry and server restart**, where the PWA retypes the
passphrase and the Tauri client re-logs in silently from the keychain.

On Android that row is not trivial — Termux dies in the background constantly
(**UX-4**), so "server restarted" is the normal state, and a PWA user would
retype their passphrase repeatedly through the day. That is a real benefit.

But it is *the* benefit, and it costs the Android SDK/NDK toolchain, a mobile
build and release channel, a cleartext-traffic exemption, an INTERNET permission,
and `keyring`'s weakest storage fallback. Step 8 is the largest step in the
sequence for the narrowest gain.

**Recommendation.** Record the value proposition in the spec in one sentence, and
**defer Plan E until the PWA has been used against a real Termux server**. If
retyping proves rare, step 8 may not be worth building; if it proves constant,
Plan E is justified by evidence instead of assumption. Nothing else depends on
step 8.

## UX-4 (MEDIUM) — Termux dying in the background is the Android experience, not an edge case

Android aggressively kills background processes. Without a wakelock
(`termux-wake-lock`) and a battery-optimisation exemption, the Termux-hosted
agent will be dead most times the user opens either client, and the localhost
profile will show "disconnected" as its normal state.

The spec treats `termux-services` / `termux-boot` as the setup story. Those
handle *starting*; neither keeps a process alive against the OOM killer.

**Recommendation.** Document `termux-wake-lock` and the battery-optimisation
exemption as required, not optional. And when a `managed: None` localhost profile
is unreachable, surface the `RUN_COMMAND` restart action **in that error state**
rather than in settings — it is the one moment it is useful. Note honestly that
`RUN_COMMAND` is fire-and-forget, so the button promises an attempt, not a
result. Belongs to **Plan E**, and to the Termux setup docs regardless of whether
Plan E is built.

## UX-5 (MEDIUM) — the macOS setup screen's re-check must re-probe PATH, not just re-read a cached result

The `tmux` setup screen tells the user to `brew install tmux` and offers a
re-check. But the PATH probe (`$SHELL -l -c 'echo $PATH'`) and the `missing`
probe run **at boot**. A user who installs `tmux` while the app is running and
presses re-check will get the boot-time answer — still missing — and the only
escape is a restart nobody prompted for.

This is the exact moment the design promised to convert an opaque failure into a
guided one, and a stale cache un-converts it.

**Recommendation.** `/api/hostinfo` re-runs both probes on demand, with the same
2000 ms timeout. Cheap, and it is the difference between the setup screen working
and appearing broken. Belongs to **Plan A** (step 1 and step 4).

## UX-6 (MEDIUM) — the managed sidecar's port is unreachable to the user who wants a browser on it

The desktop Tauri client picks a free port, so the managed agent lives at
`http://127.0.0.1:<random>` with `auth=none`. A user who wants to also open the
dashboard in a real browser — for devtools, a second window, or a tab they keep
pinned — has no way to learn that port. The client logs it; the user does not
read logs.

**Recommendation.** Surface it in the client: "Local agent: `http://127.0.0.1:PORT`",
copyable. One line of UI. Belongs to **Plan B**.

## UX-7 (MEDIUM) — the Windows loopback measurement gates the plain-web path too, not just the Tauri client

Step 6's go/no-go — does WSL2 forward `localhost` to a `127.0.0.1` listener
inside the distro — is scoped to the Tauri Windows profile. But **delivery mode 1
on Windows depends on the same fact**: "run `npm start` in WSL, open
`localhost:8080` in a Windows browser" is row 2 of Matrix 1, and it is unusable if
the answer is no.

So the measurement's blast radius is larger than the plan that contains it, and
today no plan owns the plain-web Windows story at all.

**Recommendation.** Move the measurement earlier — it costs one command on any
Windows box and needs none of Plan D's other work — and record the answer in the
spec. If loopback does not forward, both the Windows Tauri profile and the
Windows plain-web path need `CDASH_BIND=0.0.0.0` inside the distro, which changes
the exposure argument for both.

## UX-8 (LOW–MEDIUM) — `CDASH_AUTH` composes with AND only, so "browsers use passwords, Tauri uses service tokens" is not expressible

Adding `cf-access` to the chain applies it to **every** caller. A user who wants
CF Access for browser SSO and service tokens for their Tauri clients writes
`password,cf-access` and discovers the Tauri client must now present a passphrase
*and* a service token, and the browser must pass CF *and* sign in at `/login`.

In practice this is fine — `password` alone already serves both callers, so the
mixed configuration is unnecessary rather than unavailable. But the table's
`password,cf-access` row reads as "defence in depth" without saying it doubles
the credential burden on every client.

**Recommendation.** One clarifying sentence on that row. No mechanism change;
OR-semantics would weaken the chain's central guarantee for a configuration
nobody needs.

## UX-9 (LOW) — no defined active profile on launch

A client can hold several profiles — on a phone, holding both a VPS and a Termux
profile is now the expected case. Nothing says which is active on launch.

**Recommendation.** Last-active wins, persisted with the non-secret profile
fields. If the last-active profile is unreachable, show its error state with the
switcher in reach rather than silently failing over — a silent switch between a
VPS and a local agent would show the user a different machine's sessions without
telling them, which is worse than an error.

## UX-10 (LOW) — a stale cached shell plus an expired session looks like a broken app

Offline with an expired session: the service worker serves the cached shell
(navigation falls back to cache), the shell's API calls 401, and the user sees a
dashboard chrome with no data and a "disconnected" indicator — not a login page,
because reaching `/login` requires the network.

Correct behaviour, misleading appearance.

**Recommendation.** When `poll()` classifies a terminal 401 while offline, say
"signed out — reconnect to sign in" rather than "disconnected". A string, not a
mechanism.

---

# What this changes

| Finding | Severity | Plan |
|---|---|---|
| UX-1 VPS quickstart (Caddyfile + systemd) | HIGH | A |
| UX-2 document the re-login cadence | HIGH | A (spec only) |
| UX-3 state Tauri Android's value; defer step 8 | HIGH | E (defer) |
| UX-5 `/api/hostinfo` re-probes on demand | MEDIUM | A |
| UX-7 pull the WSL loopback measurement forward | MEDIUM | A or D |
| UX-8 clarify AND-semantics on the auth table | LOW–MED | A (spec only) |
| UX-10 signed-out-while-offline string | LOW | A |
| UX-6 surface the managed agent's port | MEDIUM | B |
| UX-9 last-active profile, no silent failover | LOW | B |
| UX-4 Termux wakelock + in-place restart action | MEDIUM | E + docs |

Seven of ten land in Plan A, which is consistent with the design's own
observation that steps 1–4 carry the whole web story. Two are new UI in Plan B.
Only one is genuinely Android's, and the largest finding about Android is an
argument for building it later, on evidence.
