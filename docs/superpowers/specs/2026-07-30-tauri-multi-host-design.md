# Tauri client and multi-host deployment — design

Date: 2026-07-30
Revised: 2026-07-30 (adversarial review; see
`2026-07-30-tauri-multi-host-design-review.md` for the full ledger)
Status: approved, pending implementation plan

## Problem

`claude-dashboard` is a local-only web app. It launches, monitors, resumes, and
kills Claude Code sessions in tmux. Today it runs one way: `npm start` on the
machine where the sessions live, no authentication, bound to all interfaces.

We want three delivery modes:

1. Web, served from either a local machine or a VPS
2. A desktop app for local use, installable as one self-contained thing
3. A desktop/mobile app that connects to a VPS, where the VPS address is not
   known at build time and access is guarded by Cloudflare Access (with other
   auth methods possible)

**Amended requirement (2026-07-30).** The site — e.g. `claude.myweb.site` — must
be reachable from a plain browser on the public internet **with no separate LAN
app and no mandatory identity provider**: no Tailscale, no Cloudflare Zero
Trust. A user who *wants* those may still use them. This makes CF Access an
optional path rather than the browser path.

A reverse proxy **on the VPS itself** (Caddy, nginx) is not a "separate LAN app":
the user installs nothing and no third party is involved. That is what lets
`CDASH_BIND=127.0.0.1` remain the recommended public posture, with TLS
terminated in front of the host agent.

This requirement is met by the [`password` guard](#browser-authentication--the-password-guard), and
it is the reason that guard exists: without it, `cf-access` is optional,
`bearer` cannot be attached by a browser, `trusted-proxy` needs the excluded
proxy, and `none` is an unauthenticated RCE origin — leaving a browser no way
in at all.

## Constraints that shape the design

**The backend cannot move into the client.** `lib/collect.js` shells out to
`tmux`, `ps`, `df`, and `git`, and reads `~/.claude/projects` and
`~/.claude/history.jsonl`. All of that must execute on the machine where Claude
sessions run. A Tauri app is therefore always a *client* onto a host-side
server, never a replacement for it.

**The UI has one API chokepoint.** Every API call goes through `api()` at
`public/app.js:112`, using relative paths — verified as the only `fetch` in the
UI. Rerouting the transport is a one-site change. Note what this does *not*
mean: `api()` currently discards the HTTP status
(`throw new Error(data.error || res.statusText)`), and `poll()` uses a bare
`catch {}`, so the UI cannot presently distinguish a 401 from a dead socket.
Making it do so is a separate, larger change — see [Error handling](#error-handling).

**Cloudflare Access cannot authenticate a webview navigation.** CF Access
authenticates browser navigations via an SSO redirect and a `CF_Authorization`
cookie. Native clients use *service tokens* — the `CF-Access-Client-Id` and
`CF-Access-Client-Secret` headers. Custom headers cannot be attached to a
webview navigation, so a Tauri client must ship its UI assets locally and make
authenticated HTTP calls outside the webview. This constraint determines the
transport design.

**Android cannot spawn processes in Termux.** App sandboxing prevents executing
binaries in another app's data directory. The only sanctioned channel is the
`com.termux.RUN_COMMAND` intent, which requires `allow-external-apps=true` in
Termux's config plus a manifest permission, and is fire-and-forget: no stdout,
no exit code, no supervision.

**This app is dangerous to expose.** Sessions launch with
`--dangerously-skip-permissions` (`lib/collect.js:136`) and `/api/browse`
enumerates the filesystem from `/` with no root confinement (`lib/browse.js:10`).
An unauthenticated reach of the origin is remote code execution as the running
user. Cloudflare Access protects a hostname, not a socket: any path that reaches
the origin directly — an open port, an accidentally published container port,
another tenant on the box — bypasses it entirely.

### Items to confirm during implementation

The Cloudflare MCP connector was not authorized in the design session, so the
following were taken from prior knowledge rather than verified live. Confirm
before relying on them:

- CF Access injects `Cf-Access-Jwt-Assertion` on proxied requests.
- The JWKS endpoint is `https://<team>.cloudflareaccess.com/cdn-cgi/access/certs`.
- Service-token requests carry a JWT with a `common_name` claim in place of `email`.
- Using a service token requires a Service Auth policy on the CF Access application.

If any of these differ, the `cf-access` guard changes but no other component does.

Nothing in this design was verified on macOS, Windows, WSL, Android, or against
a live Cloudflare tenant. Every claim about those five is reasoned rather than
tested. The first manual checklist run is the first real test of steps 5–8, not
a formality.

## Decisions

| Decision | Choice |
|---|---|
| Number of Tauri builds | One codebase; "local" and "VPS" are runtime profiles, not builds |
| Client transport | All API calls proxied through Rust, not webview `fetch` |
| macOS missing `tmux` | Detect and guide the user to `brew install tmux`; do not bundle |
| Auth architecture | Pluggable guard chain, composable, AND-semantics |
| Service worker | Runtime caching, no precache manifest; exists for PWA installability |
| VPS web browser auth | First-party `password` guard: scrypt hash + opaque session cookie. CF Access optional, not required |
| Browser session storage | Opaque server-side session id in a `__Host-` cookie. No stateless token, so logout and restart both revoke |
| Client session storage | In memory only, never persisted. The passphrase is keychain-stored, so the client re-logs in on launch without the user present |
| TLS under `password` | On a non-loopback bind, boot refuses without an `https://` public URL unless `CDASH_ALLOW_INSECURE_COOKIE=1`, because a dropped `Secure` cookie is undiagnosable from its symptom. **Loopback is exempt** — `http://localhost` is a secure context, and refusing it would force stripping protections the browser would have honoured |
| Termux server auth | `password`, not `none`. Android does not isolate loopback between apps, so a loopback bind is not a perimeter there |
| Login throttling | Delay, never deny. Counts distinct credentials, not attempts |
| Default bind address | `127.0.0.1` (breaking change), explicit opt-out to expose |
| JWT verification | Add the `jose` dependency; do not hand-roll |
| UI module system | Classic scripts only; transport branch inside `api()`; no build step, no bundler |

## Architecture

Three layers, split at the HTTP boundary that already exists.

### 1. Host agent — `server.js` + `lib/`

The only component that touches `tmux`, `ps`, `df`, `git`, and `~/.claude`.
Always runs on the machine where Claude sessions live. Remains plain Node with
no build step. Gains a `HostProfile` for OS-specific behavior and an auth guard
chain.

### 2. UI — `public/`

One copy, shared by all delivery modes. The transport branch lives **inside
`api()`** (`public/app.js:112`), which the design already identifies as the
single chokepoint:

```js
async function api(path, body) {
  if (isTauri) return invokeTauri(path, body);   // ~7 lines inside the existing function
  ...                                            // existing fetch path, unchanged
}
```

No new files, no script tags, no load ordering. `api()`'s body evaluates when
it is *called*, long after `app.js` has finished parsing, so the synchronous
top-level `poll()` at `app.js:386` is safe by construction rather than by
careful arrangement.

A separate-file transport layer was rejected: `public/app.js` is a classic
script with no `import`/`export` statements, so selecting between implementation
files requires either `type="module"` (which changes scope and execution timing
throughout a 388-line file relying on top-level globals) or a runtime loader
(every mechanism available to a classic script is asynchronous, so it races
`poll()` and throws `TypeError` on first paint). Branching inside the chokepoint
has neither problem.

`public/transport/backoff.js` is the one new file, and it exists for
**testability**, not transport selection — see [UI logic tests](#testing).

**The Tauri detection predicate must be measured, not assumed.** `isTauri` keys
on a global the Tauri runtime injects before page scripts. Whether that global
is present is a *configuration* question, not a runtime guarantee — current
Tauri exposes it only on opt-in, and the documented alternative is importing the
API as a module, which needs a bundler and would break the no-build-step
property. Confirm the flag during step 5 and record the predicate here. This
matters twice: the predicate gates the transport branch **and** service-worker
registration, so a wrong answer silently gives a Tauri webview the web transport
*and* a registered service worker whose same-origin `/api/` assumption is
exactly what breaks there.

Nothing else in the UI changes *for the transport swap itself*. The error-model
changes under [Error handling](#error-handling) ship alongside it.

### 3. Tauri client — `src-tauri/`

One Rust codebase, one binary per platform, driven by user-configured profiles:

```
Profile {
  name, base_url,
  managed: None | Node { .. } | Wsl { distro, .. },   // does the client start the server?
  auth:    None | Password | Bearer | CfServiceToken | Bearer+CfServiceToken,
}
```

A single install can hold several profiles and switch between them.

`Password` is the variant for the amended requirement: the client posts the
stored passphrase to `POST /api/login` and thereafter presents the session it
receives. It composes with `CfServiceToken` exactly as
`CDASH_AUTH=password,cf-access` does on the server — the guard chain's
AND-semantics are mirrored by the client attaching every credential the profile
holds.

### Where the server runs, per platform

| Platform | Server location | Client starts it? |
|---|---|---|
| Linux desktop | native | yes — Node sidecar |
| macOS | native | yes — Node sidecar |
| Windows | inside WSL | yes — via `wsl.exe -d <distro>` |
| Android | inside Termux | no — thin client to loopback |
| VPS (any client) | remote | no — thin client with auth |

"Self-contained" is a per-platform capability, not a build variant.

### Repository layout

```
server.js        lib/  public/  test/    existing, plus lib/host/ and lib/auth/
public/transport/                        new, small
src-tauri/                               new
docs/superpowers/specs/                  new
```

Nothing is relocated. (Middleware *registration order* inside `server.js` does
change — see [Guard placement](#guard-placement).)

## Host agent changes

### `lib/host/` — OS abstraction

**PATH resolution.** At boot, probe the user's real PATH via
`$SHELL -l -c 'echo $PATH'` and prepend it to `process.env.PATH`. GUI-launched
applications on macOS and Linux inherit a minimal PATH that excludes
`/opt/homebrew/bin` and `~/.local/bin`, so `claude`, `tmux`, and `git` all
appear missing even though they work in a terminal. This mechanism exists for
**inherited child environment** — the `claude` process spawned into tmux, and
anything it spawns in turn, must see the user's real PATH.

The probe is time-boxed at **2000 ms with `killSignal: 'SIGKILL'`**. The value
is arbitrary: a login shell echoing `$PATH` completes in tens of milliseconds,
so this is two orders of magnitude of headroom, and it is half the existing
`sh()` default (`collect.js:12`) because this call runs *before* the server
listens while every other time-boxed call runs after. **On timeout or non-zero
exit, boot continues with the inherited PATH — the probe never gates `listen`.**
The failure is written to `logBuffer` *and to stderr* as
`PATH probe failed (<reason>); using inherited PATH`. Binaries that cannot then
be found are reported via `/api/hostinfo`'s `missing: [...]`, producing the setup
screen rather than a hang. Without the timeout, a login shell that prompts or a
`.zprofile` under `set -u` hangs the agent before it listens, and the startup
screen shows the sidecar's captured stderr — which is empty, because the process
hung rather than failed.

Known locations are folded into the same assignment rather than being a
resolution tier:

```js
process.env.PATH = dedupe([probedPath, '/opt/homebrew/bin', '/usr/local/bin',
                           inheritedPath]).join(path.delimiter);
```

`/opt/homebrew/bin` and `/usr/local/bin` are **the backstop for a failed probe**,
not a step toward bundling. If the probe times out, boot continues with the
inherited PATH — which on a macOS GUI launch is exactly the minimal PATH the
probe existed to fix, so without these entries the fallback fixes nothing. The
probed login-shell PATH still comes first, so a user's own ordering wins.

**No absolute-path binary resolution, and no call-site changes.** All seven
invocations (`lib/collect.js:41,135,141,221,222,223` and `server.js:63`) pass
bare names, and one `process.env.PATH` assignment resolves all of them — the
effect is process-global and reaches a spawn issued from any module, including
`server.js:63`. Verified.

A bundled-resource → PATH → known-locations lookup chain returning absolute
paths was rejected: it would change seven call sites to achieve what one
assignment achieves, and its only stated purpose is enabling a bundled `tmux`,
which §Out of scope says will not happen. If that is ever reversed, the seven
call-site edits are one mechanical commit — which is what "a later configuration
change rather than a rewrite" always meant.

**Missing-binary detection** is a pure function, not a side effect of
resolution: `fs.accessSync(…, X_OK)` over `PATH.split(path.delimiter)` for each
of `tmux`, `claude`, `git`, `ps`, `df`. No subprocess, unit-testable, and it is
what feeds `/api/hostinfo`'s `missing: [...]`.

**The `df` fix.** `lib/collect.js:223` uses `df -k --output=target,avail,size`,
which is GNU coreutils only and fails on macOS, where BSD `df` has no
`--output` and a different column order. Instead of branching the parser on
column order, change the contract: query one mount at a time and label the
result with the path that was requested, so no mount-name parsing is needed.

- Linux and Termux: `df -k --output=avail,size <path>` → avail at index 0, size at index 1
- macOS: `df -k <path>` → avail at index 3, size at index 1

This also removes a latent bug: a mount path containing a space currently yields
`freeKb: NaN` and silently shifts `totalKb` to the wrong column.

This change is bounded but **not contained to one function**. It touches
`parseDf`'s signature (`lib/stats.js:25`), its caller (`collect.js:277`), the
`Promise.all` at `collect.js:220`, its existing tests (`test/stats.test.js`),
and `sh()` — see below.

**`sh()` gains an explicit dedupe key.** `collect.js:15` builds its
log-once-per-failure key as `` `${cmd} ${args[0] || ''}` ``, which for every
`df` variant is the constant string `"df -k"`. Harmless with one `df` call;
after the per-mount split, the first mount to fail burns the key for the
process lifetime and every later per-mount failure is silenced, naming no mount.

```js
const sh = async (cmd, args, { timeout = 5000, key } = {}) => {
  try { return (await run(cmd, args, { timeout, killSignal: 'SIGKILL' })).stdout; }
  catch (e) {
    const k = key || `${cmd} ${args[0] || ''}`;   // explicit key wins; default unchanged
    if (!shFailed.has(k)) { shFailed.add(k); log(`sh failed: ${k}: ${e.message}`); }
    return '';
  }
};
```

Per-mount calls pass `{ key: `df ${mountPath}` }`. The default branch is
unchanged, so the other three `sh()` sites are unaffected — except
`collect.js:41`, which passes a positional timeout and becomes
`{ timeout: 20_000 }`.

`ps -eo pid=,ppid=,%cpu=,rss=` (`lib/collect.js:222`) works unchanged on macOS,
Linux, and Termux.

### `lib/auth/` — guard chain

`buildGuard(config)` returns one Express middleware composed of independent
guards. **All** configured guards must pass.

- `none` — the local default.
- `bearer` — constant-time compare against `CDASH_TOKEN`.
- `cf-access` — verify the `Cf-Access-Jwt-Assertion` header (RS256) against
  `https://<team>.cloudflareaccess.com/cdn-cgi/access/certs`, checking `aud`
  against the configured Application Audience tag. Accepts either a user
  identity (`email`) or a service token (`common_name`); this is what allows one
  guard to serve both browser SSO and the Tauri client. JWKS is cached with
  periodic refresh.
- `trusted-proxy` — accept an identity header such as `X-Forwarded-Email`, but
  only from a configured upstream IP allowlist. Off by default and documented as
  unsafe unless the origin is unreachable. This is the escape hatch for
  Authelia, oauth2-proxy, and Tailscale.

Configured via `CDASH_AUTH`, comma-composable. Because the composition is AND,
the value must match what the origin's clients can actually present:

| Origin serves | `CDASH_AUTH` | Why |
|---|---|---|
| Local only, loopback bind | `none` | The socket is unreachable off-box |
| **Public VPS, browsers and Tauri, no third party** | **`password`** | **The default for the amended requirement. One guard serves the browser via cookie and the Tauri client via `api_request` login.** |
| VPS, browsers **and** Tauri clients, CF Access in front | `cf-access` | One guard; `email` for browser SSO, `common_name` for the service token |
| Public VPS, defence in depth | `password,cf-access` | Both required; the optional-CF case |
| VPS, Tauri clients only, defence in depth | `bearer,cf-access` | **Locks browsers out by design** — no browser can attach a bearer header |
| Behind Authelia/oauth2-proxy/Tailscale, origin unreachable | `trusted-proxy` | Escape hatch; unsafe if the origin is reachable |

A browser cannot attach `Authorization: Bearer` — there is no UI to configure
one, and keeping secrets out of the webview forbids creating one. So
`bearer,cf-access` is not a general-purpose recommendation; it is the
Tauri-only configuration.

**Rejection body.** A rejected request returns `{ "error": "unauthorized" }` and
nothing else — no guard name, no chain composition, no hint about which leg
failed. Which guard rejected is written to `logBuffer`, readable only at
`/api/logs`, which sits behind the guard. The no-leak rule applies to *all*
unauthenticated responses, not only to `/api/health`.

The client's "Reached host, auth rejected" message is composed from what the
client already holds — the active profile's configured auth method and base URL
— not from the server's response. Naming your own configuration requires no
disclosure.

**Dependencies.** The **host agent** goes from one runtime dependency to two:
`express`, plus `jose` for JWT verification. Hand-rolling signature verification
invites algorithm-confusion and `alg: none` bugs. That property — a Node host
agent with two dependencies and no build step — is what makes the agent
bundleable as a Tauri resource and runnable inside a WSL distro from a copied
directory.

The **Tauri client** is a separate budget and is not small: `tauri`, `reqwest`,
`keyring`, `tauri-plugin-store`, `serde`, and their transitive graph. This is
the real cost of delivery modes 2 and 3, and it is accepted. It is stated so
that "one dependency to two" is not read as the project's total.


### Browser authentication — the `password` guard

The guard that satisfies the amended requirement. A single-secret, first-party,
cookie-session login served by the origin itself. **Dependency delta: zero** —
`node:crypto` plus express's existing `res.cookie`.

```
CDASH_AUTH=password
CDASH_PASSWORD_HASH=scrypt$16384$8$1$<salt>$<dk>     # set once, never plaintext
```

- `GET /login` — self-contained HTML, reachable unauthenticated.
- `POST /api/login` — reachable unauthenticated, throttled, `{password}` →
  `Set-Cookie: __Host-cdash_sid=<43 chars>; HttpOnly; Secure; SameSite=Lax; Path=/; Max-Age=<session>`.
- `POST /api/logout` — behind the guard; deletes the session server-side.
- The guard looks up the sid in an in-memory `Map<sid, expiresAt>`. Present and
  unexpired → `next()`. Otherwise `/api/*` → `401 {error:"unauthorized"}`,
  anything else → `302 → /login`.

**Why an opaque session id and not a signed token, given `jose` is already
installed.** A stateless token must be signed, must carry an expiry the server
re-checks, must pin its algorithm, and **cannot be revoked**. An opaque id in a
`Map` has none of those properties to get wrong: no algorithm field to
influence, expiry is a number the server owns, `sessions.delete(sid)` is a
working logout, and a restart is a working panic button. Reaching for the
installed dependency here would have been the more fashionable and less safe
choice. The "do not hand-roll" rule still binds and is not violated — it targets
signature verification of attacker-supplied tokens, which is `cf-access`'s job
and stays with `jose`. There is no signature here.

**Why not HTTP Basic.** Basic satisfies the requirement on its face — plain
browser, no agent, no third party, no login page, no cookie, no session store —
and it dissolves several of this design's failure modes. It was rejected for two
reasons. **Cost:** verifying a password on every request means running the KDF
on every request; an eight-asset page load costs ~750 ms of KDF and the
4-second poll costs ~186 ms per cycle. Avoiding that means caching the verified
credential server-side, which is a session store under another name — Basic then
carries all of this machinery and none of the cookie controls. **Security, and
this decides it:** browsers attach cached Basic credentials to cross-site
requests automatically, and **there is no `SameSite` for an `Authorization`
header** — no attribute, no opt-out. Under Basic the sibling-subdomain exposure
below becomes fully cross-site. The cookie design is better precisely because
`SameSite` exists at all, even given that it is site- rather than origin-scoped.

Everything below Basic was surveyed and rejected: TLS client certificates
require an install (excluded, and hostile on mobile); a secret in a URL query
string puts an RCE credential into browser history, `Referer`, and `logBuffer`,
which `/api/logs` returns verbatim; magic links need a mail provider, i.e. a
mandatory third party; IP allowlisting fails the "from anywhere" premise.

**Credential storage.** One secret, hashed, in an environment variable. No user
database, and none is possible. Set with `npm run set-password` (~15 lines,
`node:readline` with echo suppressed), which requires **≥12 characters**, never
writes a file, and never echoes. Plaintext was rejected because *this* process
serves `/api/browse` (enumerates from `/`) and `/api/logs` (returns `logBuffer`
verbatim, which accumulates absolute paths) — a plaintext secret in its
environment is one disclosure away from total compromise. Boot **refuses to
start** if `CDASH_AUTH` includes `password` and the hash is unset or
unparseable: a named stderr error, never a silent fall back to `none`.

**Boot also refuses to start without TLS, and this is not belt-and-braces.**
`__Host-` mandates `Secure`, so a browser reaching the origin over plain HTTP
**discards the session cookie without any error**. `POST /api/login` returns 200
with a `Set-Cookie` the browser drops on the floor; the next request has no
cookie, the guard 401s, and non-`/api` paths redirect to `/login` — an endless
loop in which the user never sees "Incorrect password" and every symptom points
at the passphrase, which is fine. Nothing server-side detects this: the header is
correctly formed and correctly sent. Test 8 asserts `Secure` is *present*, which
is exactly the property that causes the failure.

It will not appear in testing either. Browsers treat `http://localhost` as a
secure context and honour the cookie, so this reproduces **only** on a first
public deployment — the moment the design can least afford a misleading symptom.

So: if `CDASH_AUTH` includes `password` **and the bind address is not loopback**,
boot requires either `CDASH_PUBLIC_URL` with an `https://` scheme, or an explicit
`CDASH_ALLOW_INSECURE_COOKIE=1`, which drops the `__Host-` prefix and the
`Secure` attribute together (the prefix is refused without `Secure`, so keeping
one without the other yields a cookie no browser will store) and logs a warning
naming the session-theft exposure on a plain-HTTP origin. Same shape as the
unset-hash rule above, for the same reason: a misconfiguration that cannot be
diagnosed from its symptom must be refused at boot rather than debugged in
production.

**Loopback is exempt, and the exemption is load-bearing.** When `CDASH_BIND` is
`127.0.0.1` or `::1`, boot proceeds with `Secure` and `__Host-` intact and no
`https://` URL required. Browsers treat `http://localhost` as a secure context
and store the cookie normally, so the failure this rule exists to prevent cannot
occur. Without the exemption the rule inverts: a password-guarded loopback server
would refuse to boot, and the only way forward — `CDASH_ALLOW_INSECURE_COOKIE=1`
— would *strip two protections the browser was willing to honour*. A safety check
whose remedy is less safe than the configuration it rejected is a defect.

This is not a corner case. It is the **Termux configuration** (see
[Android's loopback exposure](#deployment-topology-and-trust-boundary)), where a
password on a loopback bind is the recommended posture rather than an unusual
one, and it is also how anyone runs a password-guarded server on their own
machine.

Hashing uses **`crypto.scrypt` (async, libuv threadpool)**, not `scryptSync`.
The parameters are `N=16384, r=8, p=1, 32-byte key`; the *cost* is
machine-dependent (42 ms and 93.5 ms measured on two boxes, likely slower on a
modest VPS) and is deliberately not quoted as a design constant. A slower box
strengthens brute-force resistance and, because the throttle delays rather than
denies, costs no availability. `timingSafeEqual` throws on length mismatch and
is wrapped to return `false`.

**Session lifetime, and what a stolen session gets.** Stated plainly: a stolen
session cookie **is** remote code execution as the running user. Every
authenticated route is equally privileged; there is no lesser tier. Bounds: a
**12-hour absolute lifetime with no sliding renewal** (conventional 7–30 day
sessions are calibrated for accounts that can be re-secured after theft, and
this one cannot be); `POST /api/logout` revokes immediately; a restart revokes
everything; `HttpOnly` keeps it out of JS; `Secure` keeps it off plain HTTP.

The store needs no sweeper. Entries are minted only by a successful login, so
they cannot be grown without the passphrase; twice-daily logins accumulate under
75 KB a year, expired entries are rejected at lookup, and every restart resets
it to zero.

### Login throttling — delay, never deny

Three rules. The first two bound a *client*; the third bounds an *attacker*, and
it carries the safety load.

**Rule A — one login attempt per credential generation.** `api_request` holds
`login_attempted` per profile, cleared only by the user editing the credential
or by a success. On 401 with the flag unset: attempt login once, set the flag,
retry the request. On 401 with the flag set: **halt the poll**, surface
*"Reached host, credentials rejected — the stored password may be out of date"*
with an **Update password** action. No automatic retry, ever.

Without this, a stale stored passphrase turns the 4-second poll into a login
attempt every 4 seconds — which is precisely what the error-model rule against
retrying auth failures exists to forbid, aimed at the origin's own throttle
instead of Cloudflare's.

**Rule B — count distinct credentials, not attempts.** A failure whose
credential fingerprint equals the previous failure's does not advance the
counter. Fingerprint is an HMAC under a boot-random key, **one value retained** —
no growth, no stored password, no reusable hash.

This is the discriminator: a stale client repeats itself, a brute-forcer must
vary to learn anything. Replaying one password 50 times leaves the counter at 1;
40 distinct guesses advance it 40 times; alternating between two wrong passwords
is strictly worse for the attacker than distinct guessing. **Rule B is an
optimisation, not a guarantee** — two clients with *different* stale credentials
advance the counter exactly as a brute-forcer does, which is an ordinary state
for a multi-device, multi-profile design. That is why Rule C must be sound
alone.

**Rule C — the throttle delays; it never denies.** After 5 distinct failures,
each subsequent attempt is **delayed before evaluation** by
`min(1s · 2^(n−5), 20s)`, then processed normally. The counter resets on success
or after 15 minutes idle. The throttle lives in `POST /api/login` only: a caller
holding a valid session is never affected.

**A login attempt is never rejected for throttle reasons.** Once accepted it is
always eventually evaluated. Pending delayed logins are bounded by
`CDASH_LOGIN_PENDING_MAX`, **default 1024**, derived from what a pending request
actually costs — one socket and one entry in a shared wake list, with scrypt
running after the delay on the threadpool, so nothing pending holds CPU or a
timer. On overflow the response is **503 with `Retry-After`**, and that path is
reachable only under volumetric load.

The bound is set by resource cost, not by guesswork, and the difference is the
whole point:

| Pending bound | Connections to hold it | Sustained rate | Verdict |
|---|---|---|---|
| 4 | 4 | **0.2 req/s** | A design defect — trivially cheap denial |
| **1024** | **1024** | **51.2 req/s** | Ordinary volumetric DoS |

Sustaining 1024 concurrent connections at ~51 req/s against a single-user Node
process is not an attack on this throttle — the same load aimed at `GET /` is
just as effective, and defending it belongs to the reverse proxy that already
terminates TLS. That is a fact of the internet, not a property of this
mechanism. At 0.2 req/s it would have been the latter.

**What an attacker achieves:** a sustained guessing attack adds latency to new
logins — up to 20 seconds per attempt — and does not deny them. Existing
sessions are unaffected. Because sessions are 12-hour absolute, a sufficiently
long attack does eventually bite: a new device cannot log in while one is
running.

The counter is **global, not per-IP**. Per-IP is defeated by rotation and is
meaningless behind a reverse proxy, where every request carries the proxy's
address.

### CSRF — the layering, stated correctly

**The primary control is that `express.json()` parses `application/json` only**,
combined with the absence of CORS headers. A form POST arrives with
`req.body = {}` and every handler rejects at validation; a cross-origin `fetch`
carrying JSON triggers a preflight that fails. Verified for all four
content-types **with a valid session cookie attached**.

**`SameSite=Lax` is defence in depth, not the primary control.** `SameSite` is
scoped to the registrable domain, not the origin: for `claude.myweb.site` every
sibling — `blog.myweb.site`, a parked subdomain, one carrying an XSS — is
**same-site**, and Lax attaches the session cookie to their POSTs in full. It
blocks fully cross-site requests and does nothing against a sibling.

`__Host-cdash_sid` closes the adjacent hole. The prefix is refused unless the
cookie is `Secure`, `Path=/`, and carries **no `Domain`**, so a sibling cannot
mint one scoped to the registrable domain; anything it sets stays host-only to
itself. Shadowing becomes structurally impossible rather than merely detectable,
at a cost of seven characters. (The flags already specified satisfy the prefix
with no change.)

No GET performs an attacker-useful state change. `GET /api/sessions` does
refresh caches and spawn the same read-only probes the dashboard runs every four
seconds anyway — so "read-only" is inaccurate — but nothing creates, kills, or
resumes a session, and every mutation is POST. That is the premise `SameSite`
depends on and it is stated in its true form.

**Consequence for deployment:** every host under the registrable domain is
inside this origin's trust boundary. Prefer a domain with no untrusted siblings.

### What `/login` may contain

A password field, a submit button, an error region. **No product name, no
version, no logo, no favicon reference, no title beyond "Sign in."** Failure
text is *"Incorrect password"* — identical for a wrong password and an expired
session.

The no-leak rule that governs `/api/health` and the uniform rejection body
applies here too: naming the product on an unauthenticated page tells a scanner
that a successful guess against this endpoint yields RCE as the running user.
A generic prompt is the same number of lines.

**`login.html` must remain asset-free** — inline CSS, no external stylesheet,
script, image, or font. This is what keeps the unauthenticated exception count
at three; a logo would silently add a fourth.

### Guard placement

`buildGuard` is registered in `server.js` **after** `express.json()` and the
`/api/health` route, and **before** `express.static(public)` and every other
`/api` route:

```
express.json()
GET  /api/health     ← exception 1: liveness, {ok:true}, nothing more
GET  /login          ← exception 2: static HTML, no host data
POST /api/login      ← exception 3: throttled, no host data
buildGuard(config)
express.static(public)
… every other /api route
```

The three exceptions are enumerated, not implied, and they are complete:
`GET /login` is an explicit route registered before the guard, so
`public/login.html` stays behind `express.static` and `/login.html` itself
correctly redirects. A browser's automatic `/favicon.ico` request redirects
harmlessly.

Consequence: `/api/health` is the only unauthenticated endpoint on the origin,
and UI assets — including `sw.js` — require a credential. Under
`CDASH_AUTH=none` the guard is a pass-through and behaviour is identical to
today.

This ordering is not forced by the current file layout; it is chosen.
`express.static` currently sits at `server.js:15`, above every route, and moving
it below the guard is what makes the two requirements — unauthenticated health,
guarded assets — simultaneously satisfiable. Verified.

A `bearer`-only origin therefore cannot serve the web UI at all. That is
correct: per the table above, such an origin exists to serve Tauri clients,
which ship their UI locally and never fetch `public/`.

### Bind address — breaking change

`server.js:71` currently calls `app.listen(port, cb)`, binding all interfaces.
The new default is `127.0.0.1`, overridable with `CDASH_BIND=0.0.0.0`, which
logs a warning naming the RCE risk when `CDASH_AUTH=none`.

This breaks existing LAN access until users set `CDASH_BIND` explicitly. That
is intended — the dangerous topology should require a deliberate act — and the
README must document it.

### Server structure

`server.js` gains an export, because the integration test cannot otherwise be
written: the file currently exports nothing and `app.listen(port, cb)` discards
the returned `Server`, while the startup log prints the `port` *variable* rather
than the bound port (so `PORT=0` logs `http://localhost:0`).

```js
export function createApp(ctx) { /* app construction, all routes, guard */ return app; }

const port = process.env.PORT || 8080;
const host = process.env.CDASH_BIND || '127.0.0.1';
const server = createApp(ctx).listen(port, host, () =>
  console.log(`claude-dashboard on http://${host}:${server.address().port}`));
server.on('error', e => { /* see EADDRINUSE handling below */ });
```

Taking `ctx` as a parameter also means the integration test constructs an app
with a test `ctx` rather than depending on the developer's real `~/.claude`.
Logging the **bound** port is what the managed sidecar needs when it picks a
free port.

**`EADDRINUSE` is a diagnosed condition, not an uncaught exception.** Without an
`error` listener, `listen` on a held port kills the process with an uncaught
throw — verified. The server instead writes `port <p> already in use` to
**stderr**, exits with code **3**, and writes **no pidfile**.

### Health endpoints

- `GET /api/health` — unauthenticated, returns `{ ok: true }` and nothing more.
  It must not leak host details to an unauthenticated caller. **This endpoint
  already exists** (`server.js:17`) and already sits ahead of every other route;
  only its position relative to the new guard is a decision.
- `GET /api/hostinfo` — authenticated, returns platform, server version, and
  `missing: ["tmux"]`.

**Health answers liveness, not readiness.** `/api/health` says *something* is
listening; it cannot say *your* server is listening, because it is
unauthenticated by design. It must never be used alone to gate UI load — see
[Managed-server readiness](#managed-server-readiness).

`hostinfo` delivers the macOS setup story: when `tmux` is missing the UI shows a
setup screen with the install command and a re-check button, rather than failing
every launch with an opaque error.

**Version.** `package.json` has no `version` field today; add `"version":
"0.1.0"`. The host agent reads it at boot and reports it at `/api/hostinfo`.
It is the cache key for the WSL copy-in path and the input to the version-skew
banner.

The Tauri client carries its own version, independent of the host agent's. They
are compared, never required to match: a mismatch produces a non-blocking banner
and nothing else. Skew cannot arise on a Linux/macOS managed sidecar, which
bundles that exact `server.js`. It can arise on any profile that contacts a
server the client did not just start: VPS, Termux, and a managed WSL profile
that reaches a previous-generation server holding the port.

Bumping the version is what invalidates the WSL copy-in cache. Forgetting to
bump it after editing `lib/` leaves a stale server in the distro, and **no
adopted mechanism detects that** — the versions match, so the skew banner cannot
fire. It is on the Windows manual checklist.

## Tauri client

### Commands exposed to the webview

Deliberately narrow. The webview receives no filesystem, shell, or network
capability.

```
api_request(method, path, body) -> { status, body }
profiles_list / profile_save / profile_delete / profile_activate
server_start / server_stop / server_state
host_platform()
```

`api_request` is the entire data path. It attaches the active profile's auth
headers in Rust and calls the server via `reqwest`. Consequences:

- CORS does not apply, so the server needs no `Access-Control-Allow-Origin`
  configuration for desktop clients.
- CF service-token headers can be attached, which a webview navigation cannot do.

**Secret handling.** A secret enters the webview's JS context exactly once: the
user types it into the first-run or settings form, and it is passed to Rust via
`profile_save(profile, secret)`. It is written to the credential store and
**never returned to JS**. `profiles_list` returns records with the secret field
replaced by a boolean `has_secret`; there is no command that reads a stored
secret back into the webview.

Consequence: an XSS present at the moment of entry can capture that one
keystroke stream; an XSS at any later time reads nothing. This is weaker than
"secrets never touch JS" and is stated as the actual property.

### Secret storage

Non-secret profile fields (name, URL, distro) go in `tauri-plugin-store` as
plain JSON.

There is **one** credential lifecycle: user-entered, persisted, long-lived. A CF
service-token pair, a bearer token the user obtained elsewhere, or the `password`
passphrase — typed once into a form and expected to persist. Replaced only by the
user.

**The session id is not stored.** It lives in memory for the life of the client
process and is discarded on exit. Persisting it was considered and rejected: the
stated benefit was avoiding a re-prompt on launch, and there is no re-prompt to
avoid — the passphrase is already persisted, so the client logs in for itself
without the user present. The real saving would have been one round trip and one
KDF (~100 ms) per app launch, against a credential that grants RCE for up to 12
hours *without* the passphrase and that cannot be revoked by changing it. Not
worth a second credential class.

Stored credentials live in the `keyring` crate: macOS Keychain, Windows
Credential Manager, Linux Secret Service. The user can see that one exists
(`has_secret`) and can replace or delete it. No credential is machine-generated
and no managed server is spawned with one. Two fallbacks are designed in rather
than discovered later:

- **Headless Linux** with no Secret Service running: fall back to a `0600` file
  in the app config directory, with a visible UI warning that the token is
  stored unencrypted.
- **Android**, which `keyring` does not cover: use app-private storage, which
  the OS isolates per-app. This is the same protection Termux's own data has,
  and it is weaker than hardware-backed Keystore.

**Session transport.** `reqwest` is configured with **no cookie jar**. The client
reads the sid out of `Set-Cookie` on login, holds it in memory, and attaches it
explicitly on each request. Explicit attachment keeps one rule for all
credentials: `api_request` adds what the profile holds, and nothing is added by a
layer beneath it.

### What the user actually does

Stated plainly, because a login flow that quietly re-prompts is worse than no
login flow at all:

| Moment | What the user does |
|---|---|
| Creating a VPS profile | Types the passphrase **once**, into the profile form |
| Every app launch after that | **Nothing.** The client reads the passphrase from the keychain and logs in for itself — one request, no UI |
| Session expiring mid-use (12 h) | **Nothing.** The next 401 spends the launch's login attempt, and the request is retried |
| Passphrase changed on the server | Types the new one **once**, via the **Update password** action that the terminal 401 surfaces |
| In a browser | Signs in at `/login` when the cookie expires or the server restarts — a browser has no keychain to log in from |

**Rule A makes the automatic path work.** `login_attempted` is **in-memory, per
process**, so a fresh launch always carries exactly one login attempt. The client
starts with no session, its first request 401s, that spends the attempt, and the
request is retried with the new session. A server restart or an expired session
therefore recovers by itself.

The bound Rule A exists for still holds. A *stale passphrase* yields one attempt
per app launch — user-initiated, not automatic — and under Rule B every one of
those replays the same credential, so the throttle counter never advances past 1.
Persisting the flag instead would be strictly worse: it would make "restart the
app" fail to recover from an expired session, which is the ordinary case.

`POST /api/logout` discards the in-memory session on success **and on failure** —
a logout that cannot reach the server must not leave one live in the process.

### Managed-server readiness

Readiness is `GET /api/health` returning 200. Managed servers run with
`CDASH_AUTH=none`, so there is no credential to mismatch and no authenticated
second step.

**Precedence, which must be stated because the two signals can disagree.** The
client spawns a server *and* polls health, and when an orphan holds the port
those return contradictory answers: the spawned child exits 3 with
`port <p> already in use` on stderr while `/api/health` returns 200 from the
orphan. **The spawn result wins.** A client whose child exited non-zero does not
proceed on a successful health poll; it reports the startup failure with the
child's stderr and stops. Otherwise the client silently binds its session to a
server it did not start, of a previous generation, with only a non-blocking
banner to say so.

A previous-generation orphan that *does* answer successfully — because the
client's own spawn was never attempted, or succeeded on another port — is caught
by the version-skew banner, which is the design's only detector for that case.

### Managed server, per platform

**Linux and macOS.** Tauri sidecar. Bundle a per-arch `node` binary plus
`server.js` and `lib/` as resources. On launch: pick a free port, spawn
`node server.js` with `CDASH_BIND=127.0.0.1` and `CDASH_AUTH=none`, run the
readiness probe, then load the UI. No-auth is correct here precisely
because the socket is loopback-only **on the same kernel**, with no relay in
between, and free-port selection means a new client never contacts an orphan.
Tear the child down explicitly on exit.

**Windows.** Spawn through `wsl.exe -d <distro> -- bash -lc "..."`.

- **Spawn contract:** `CDASH_BIND=127.0.0.1` and `CDASH_AUTH=none` inside the
  distro — identical to the Linux/macOS sidecar. The exposure is "any local
  process on the Windows box," which is the same exposure the sidecar's
  same-kernel loopback argument already accepts on two other platforms.
- **Measure the relay before building around it.** Whether WSL2's localhost
  forwarding reaches a `127.0.0.1` listener inside the distro is unverified, and
  step 6 cannot be implemented without a Windows machine anyway — at which point
  the check costs one command. Do it first.

  **There is no code path that binds `0.0.0.0` with `CDASH_AUTH=none`.** If the
  client cannot reach the server it started, it reports *"the server started
  inside `<distro>` but is not reachable at `localhost:<port>`; WSL loopback
  forwarding is not working on this system"* and **stops**. It does not retry on
  another interface, does not fall back, and offers no setting that would. If
  the measurement comes out badly, Windows delivery is blocked until this is
  redesigned with a measured fact in hand. That risk is real and is stated here
  rather than concealed behind a credential that made the design independent of
  an answer nobody had looked up.

  Note this constraint is a design constant in the client, not a runtime
  assertion: `CDASH_BIND=0.0.0.0` remains permitted generally, because the bind
  decision deliberately allows it behind a warning. Nothing mechanically
  prevents a future edit from adding the fallback.
- **Copy the server into the distro** at `~/.cdash/<version>/` on first run and
  on version change, rather than executing from `/mnt/c/...`. Running Node
  across the 9p filesystem boundary is slow and occasionally unreliable.
- **Pidfile rules.** Shutdown depends on the pidfile, so its semantics are
  specified rather than assumed:
  1. The pidfile is written **only after the `listening` event fires — never
     before**. It contains `{pid, port, version, startedAt}`.
  2. It is deleted on clean exit.
  3. On `EADDRINUSE` the server exits 3 with a diagnosed stderr line and writes
     **no pidfile** (see [Server structure](#server-structure)).

  Rules 1 and 3 together mean the pidfile can only ever name a process that
  actually listened. Without them, a server that dies on a held port before
  listening leaves a pidfile naming a dead pid, and teardown kills a corpse while
  the real orphan survives every restart. This is required for teardown alone and
  does not depend on any recovery protocol.
- **Cleanup.** The copy-in path is keyed by version, so a new version creates a
  new directory rather than replacing one. Uninstalling the Tauri app leaves
  `~/.cdash/` behind, because an uninstaller cannot reach inside a WSL distro;
  the README documents `rm -rf ~/.cdash` as the manual step, and the Windows
  checklist includes it. This is the only place in the design that writes
  persistent state into a namespace an uninstaller cannot reach.

  No automatic garbage collection. Deleting directories inside a filesystem the
  app does not own is real behaviour with a real blast radius, traded against a
  few megabytes per version on a developer's home directory — a cost, not a
  failure. Accumulation is unbounded but slow, single-platform, visible, and
  removable by the documented command.

Distro selection comes from `wsl.exe -l -q` (note: UTF-16LE output), shown as a
settings dropdown defaulting to the WSL default distro. Windows uses the distro's
own Node — a Windows Node binary cannot run inside WSL — with detect-and-guide if
absent. Node is near-certainly present, since Claude Code requires it.

Because the folder picker browses *the server's* filesystem, it naturally shows
WSL paths. No `\\wsl$\` path translation is needed anywhere.

**Android.** No managed server, by OS design. The app ships a default profile
pointing at `http://localhost:8080` and expects Termux to run the server itself
via `termux-services`, or `termux-boot` to start on boot. This requires an
INTERNET permission and a cleartext-traffic exemption for the `http://` loopback
URL. Optionally, an opt-in button fires a single `com.termux.RUN_COMMAND` intent
and then runs the readiness probe; because that channel returns no exit code or
stdout, it is a convenience, not supervision, and it requires the user to have
set `allow-external-apps=true` themselves.

The default Termux profile carries `auth: Password`, not `auth: None`, per
[the loopback exposure](#deployment-topology-and-trust-boundary) — a loopback
bind is not a perimeter on Android. The setup documentation configures the Termux
server with `CDASH_AUTH=password` and `CDASH_BIND=127.0.0.1`, which the loopback
exemption permits with `Secure` and `__Host-` intact.

**A phone holds both kinds of profile.** Reaching a VPS and reaching its own
Termux server are both first-class, not one plus a fallback, so the profile
switcher is a primary surface on Android rather than a settings detail — the two
targets differ in reachability minute to minute (Termux dies in the background;
the VPS does not), which is exactly when a user wants to switch.

**VPS profile, any platform.** `managed: None` — a base URL plus credentials.

### First run

Desktop platforms auto-create a working managed-local profile and go straight to
the dashboard, with the macOS `tmux` setup screen as the only possible detour.
Android and any VPS profile go through a short form — URL, auth method,
credentials — plus a **Test connection** button that calls `/api/health` and
`/api/hostinfo` and reports which of the two failed. Distinguishing "cannot
reach the host" from "reached it but auth was rejected" is the difference
between a two-minute fix and an hour of guessing — and it is carried by the
*status* difference between the two endpoints, so it needs no detail in the
rejection body.

The existing UI is already responsive with a touch-friendly picker, so it
carries to a phone webview without layout work.

### Service worker

**Why `sw.js` exists.** Not primarily for offline caching. `public/manifest.json`
declares `"display": "standalone"` with `start_url` and icons, and
`index.html:22` links it — this is a genuine installable PWA, and installability
requires a service worker with a fetch handler. Its load-bearing job is the
**Android delivery mode**; caching is secondary. State that before deciding how
much caching machinery it deserves.

**No precache manifest.** The `SHELL` array and `install`'s `addAll` are deleted.
A hand-maintained precache list is a build artifact in a project with no build
step, and keeping it synchronised with `index.html` produced four separate
defects during design review, every one caught by a person reading and none by a
tool, each invisible in development because a browser with an empty cache works
perfectly. Runtime caching needs no manifest, so there is nothing to drift.

```js
const CACHE = 'cdash';   // namespace, not a version — never bumped
self.addEventListener('fetch', e => {
  const url = new URL(e.request.url);
  if (url.pathname.startsWith('/api/')) return;                  // network only, unchanged
  if (e.request.mode === 'navigate') {
    e.respondWith((async () => {
      try {
        const r = await fetch(e.request);
        if (r.status === 200 && !r.redirected &&
            new URL(e.request.url).pathname === '/')               // ← the key, and its guard
          (await caches.open(CACHE)).put('/', r.clone());
        return r;
      } catch { return (await caches.match('/')) || Response.error(); }
    })());
    return;
  }
  e.respondWith(caches.match(e.request).then(hit => {             // stale-while-revalidate
    const fresh = fetch(e.request)
      .then(r => { if (r.ok) caches.open(CACHE).then(c => c.put(e.request, r.clone())); return r; })
      .catch(() => hit);
    return hit || fresh;
  }));
});
```

**The navigation branch writes under the fixed key `/`, and only when the
request's pathname is `/`.** That is symmetric with the offline fallback's
`caches.match('/')` and makes the login page unwritable to the shell key by
construction. Without the pathname test, the third redirect path escapes: after
the browser follows a `302 → /login`, it issues a *fresh* navigation whose
response is `status 200` with `redirected === false` — passing both the status
and redirect tests, and caching a login page as the application shell.

**`if (r.ok)` on every `cache.put` is load-bearing, not defensive.** `addAll`
rejected the whole install if any response was not ok — a fail-closed guarantee
that made it *impossible* to cache an error body. `cache.put` has no such
guarantee and writes whatever it is handed, so removing the manifest silently
replaces a fail-closed primitive with a fail-open one. Two concrete failures the
condition prevents, both on adopted behaviour:

- `public/` sits behind the guard, so a background revalidation fired while the
  session has expired receives a **401 body**, and unguarded that body is cached
  as `/app.js` and served as the application script until a revalidation
  succeeds.
- Navigations are fetched with `redirect: "manual"`, so an expired session yields
  an **opaqueredirect** (status 0, null body) — which is the entire mechanism
  that lets the SSO redirect fire. Caching it poisons the offline fallback;
  awaiting a `put` that rejects on it routes into `.catch()` and serves the
  cached shell *instead of* following the redirect. `r.ok` is false for both 401
  and status 0, and closes both.

**Navigations are network-first** so a full-page reload reaches the origin and
any redirect fires; sub-resources are stale-while-revalidate, so a new `app.js`
is picked up on the next load automatically. That is what removes the need for a
cache version: there is no constant to bump and no rule to remember. `CACHE`
stays a plain namespace string, and `activate`'s delete-non-matching logic
remains as a one-time reset lever.

**Registration is gated to web mode only**, on the same predicate as the `api()`
transport branch, with a `.catch()` on `register()`. Both of `sw.js`'s
assumptions break in a Tauri webview.

**Cost, stated plainly.** A service worker does not control the page load during
which it installs, and `sw.js` sets neither `skipWaiting` nor `clients.claim`.
Under precache, `install`'s `addAll` fetched the shell independently, so
visit-1-then-offline worked. Under runtime caching it does not: **offline works
from the second visit onward.** For a monitor of a server — whose entire content
is unavailable offline, leaving a shell that renders "disconnected" — the
difference between that shell and a browser error page, in the window between
installing and using it a second time, does not justify a manifest that misfired
four times.

### Remote-control links

No work needed. `rcLink` resolves to `https://claude.ai/code/<id>`
(`lib/collect.js:90`, `:209`, `:233`), an absolute public URL, so "Open in
Claude" behaves identically from a local app, a phone, or a VPS client.

## Error handling

Every failure names which layer broke. Today a missing binary yields an empty
string from `sh()` (`lib/collect.js:13-19`) and the session list shows nothing —
the failure *is* logged once per command to `/api/logs`, so it is not silent
everywhere, but it is silent where the user is looking. Acceptable for a
single-host local tool, not across four platforms and a network.

| Failure | Detected by | Surfaced as |
|---|---|---|
| Managed server will not start | readiness probe times out, **or the spawned child exits non-zero** | Startup screen with the child's captured stderr |
| Port held by another process | spawned child exits 3 with `port <p> already in use` | Same startup screen, naming the port. The spawn result outranks a successful health poll |
| `tmux` or `claude` missing | `HostProfile` probe via `/api/hostinfo` | Setup screen naming the binary and install command |
| Wrong WSL distro, or no Node in it | `wsl.exe` exit code and stderr | Settings error naming the distro |
| Host unreachable | `api_request` transport error | "Cannot reach host" plus the URL tried |
| Auth rejected | HTTP 401 or 403 | "Reached host, auth rejected" plus the profile's configured auth method and URL. The server body names no guard. Halts the poll |
| Stored password out of date | 401 after the profile's one login attempt | "Reached host, credentials rejected — the stored password may be out of date" with an **Update password** action. Halts the poll; no further login attempt until the credential is edited |
| Host throttling logins | HTTP 429, or 503 with `Retry-After` | Transient: back off and retry, honouring `Retry-After`. **Never halts** — a halt here would let an attacker who saturates the throttle stop a client whose credentials are valid |
| CF JWT expired in browser | 403 from the `cf-access` guard | Full-page reload; the service worker passes navigations to the network, so the CF SSO redirect fires |
| Server version differs from client | `/api/hostinfo` version | Non-blocking banner |

Two behaviors are specified explicitly:

- **Polling backs off on a graduated ladder**, not a cliff: `4s → 8s → 15s → 30s`
  (cap), resetting to 4s on any successful poll, on `visibilitychange` to
  visible, and on any user-initiated action. A single persistent "disconnected"
  indicator replaces a stream of errors. A local restart therefore reconnects
  within 8s in the common case, and only a sustained outage reaches 30s — which
  is the situation where 30s is desired.

  *Residual risk:* after roughly a minute of continuous unreachability the
  interval reaches 30 seconds, so a server restarted after a long outage may
  take up to 30 seconds to be noticed if the tab has stayed in the foreground
  the whole time. Switching away and back forces an immediate poll. Symptom: the
  "disconnected" indicator persists briefly after `npm start` returns.

- **Auth failures do not retry — but throttling is not an auth failure.** Only
  transport errors and transient throttling — HTTP 429 and 503, honouring
  `Retry-After` when present — back off and retry. Any **terminal**
  authentication failure — HTTP 401 or 403 — halts the poll and requires user
  action. Otherwise a stale credential generates login attempts indefinitely.

  The 401/429 split is load-bearing. A 401 means *your credential is wrong*:
  terminal, human action required. A 429 means *try again later*: transient by
  definition. Classifying 429 as terminal would let an attacker who saturates
  the login throttle permanently halt clients whose credentials are valid, with
  every manual resume re-halting within one poll.

Both require `api()` to propagate the HTTP status and `poll()` to branch on it —
neither of which it does today. The decision logic lives in
`public/transport/backoff.js` as a pure function `next(state, outcome)` with
`outcome ∈ {ok, fail, auth}`; `poll()` keeps only the DOM wiring.

## Testing

The existing suite is pure-function `node --test` against parsers, with no
mocking framework. Follow that pattern; do not introduce one.

**Pure-function tests with real fixtures:**

- `parseDf` — captured real GNU and BSD `df -k` output, including a mount path
  containing a space
- `HostProfile` binary resolution — lookup chain order and missing-binary reporting
- `bearer` guard — constant-time compare
- `cf-access` guard — against a locally generated RSA keypair and stub JWKS,
  covering a valid user token, a valid service token (`common_name`), wrong
  `aud`, expired, tampered signature, and `alg: none`. The last is the specific
  class of bug `jose` exists to prevent and gets an explicit test.
- Guard composition — `bearer,cf-access` requires both
- `backoff` — the ladder, reset-on-success, and the 401-halt rule
- **Missing-binary detection** — the `fs.accessSync` PATH walk, against a
  temporary directory with and without an executable present

No shell-consistency test: with the precache manifest deleted there is no
manifest to be inconsistent with. That test existed to police a coupling that no
longer exists — the root cause is removed rather than guarded.

**UI logic tests.** Decision logic in `public/` that is not DOM manipulation
lives in a file under `public/transport/` containing **no `import`/`export`
statements**, assigning to `globalThis` via `(function (g) { … })(globalThis)`.
Such a file is simultaneously a classic script the browser loads via a static
`<script>` tag and a side-effect ES module `node --test` can `await import(...)`.
This gives `public/` automated coverage with no build step, no bundler, no
mocking framework, and no DOM. DOM wiring stays untested, as it is today.

The `(function (g) { … })(globalThis)` form is load-bearing, not stylistic: top-level
`this` is `undefined` in a module and `globalThis` in a classic script, so
`this.X = …` works in the browser and fails under `node --test`.

**Integration, Node only, no Tauri required.** Boot the real server on an
ephemeral port via `createApp(ctx)`. Under each non-`none` auth mode assert:

1. `GET /api/health` → 200 **unauthenticated**.
2. **Every other route registered on the app** → 401 unauthenticated. The route
   list is derived by enumerating the Express router at test time, not
   hand-written, so a route added later without a guard fails the test on the day
   it is added.
3. `GET /` and `GET /sw.js` → 401 unauthenticated. This pins the guard-placement
   decision; router enumeration alone cannot, because `express.static` and the
   guard are non-route layers invisible to enumeration.
4. With a valid credential, `/api/sessions` → 200.

Under `CDASH_AUTH=password`, additionally assert:

7. `GET /login` → **200** unauthenticated; `POST /api/login` with a wrong
   password → **401** with no `Set-Cookie`; with the right password → **200 +
   `Set-Cookie`** containing `HttpOnly`, `Secure`, `SameSite=Lax` and the
   `__Host-` prefix. Then replay 2 and 3 with the cookie and require 200/302→200.
8. **CSRF invariants.** A POST to `/api/kill` with `content-type: text/plain`
   **and a valid session cookie** → **400, not 200**, proving the body was not
   parsed; and no response carries `Access-Control-Allow-Origin`. *This is the
   only mechanical enforcement of the primary CSRF control — if it is ever
   dropped, the sibling-subdomain exposure becomes HIGH.*
9. **Throttle.** After arming the throttle with distinct wrong passwords, a
   correct login returns **200 after a delay and never 429**, and a login issued
   while other delayed logins are pending also returns **200 after a delay**.
   Replaying one wrong password does not advance the counter.
10. **Cookie splitter.** Both orderings of a duplicate `__Host-cdash_sid`, a
    malformed pair with no `=`, an empty value, and a trailing `;` — asserting
    last-wins deterministically and no throw. The splitter is the one piece of
    hand-rolled parsing on attacker-influenced input; it is exempt from "do not
    hand-roll" not because the rule fails to apply but because it is small
    enough for a test to discharge it.
11. **Boot refusals.** `CDASH_AUTH=password` with the hash unset, with an
    unparseable hash, and with neither an `https://` `CDASH_PUBLIC_URL` nor
    `CDASH_ALLOW_INSECURE_COOKIE=1` each **exit non-zero with a named stderr
    message and never listen**. With `CDASH_ALLOW_INSECURE_COOKIE=1`, boot
    succeeds and the `Set-Cookie` carries **neither `Secure` nor the `__Host-`
    prefix** — the two must move together, since either alone yields a cookie no
    browser will store.
12. **Loopback exemption.** `CDASH_AUTH=password` with `CDASH_BIND=127.0.0.1`,
    no `CDASH_PUBLIC_URL` and no `CDASH_ALLOW_INSECURE_COOKIE` **boots**, and its
    `Set-Cookie` carries `Secure` **and** the `__Host-` prefix. This is the
    Termux posture; the assertion is that the safe configuration is reachable
    without setting the flag that would degrade it.

Under `CDASH_AUTH=none`, assert every route returns **anything other than 401 or
403** — the invariant is that the guard refactor did not start rejecting the
local default, not that every handler succeeds on an empty body (five routes
correctly return 400 on one). **No test drives `/api/launch`, `/api/resume`, or
`/api/kill` to success**, because that spawns or kills real tmux sessions with
`--dangerously-skip-permissions` as a side effect of `npm test`.

This is the test that catches an accidental auth bypass and is the highest-value
test in the suite.

**Manual, documented as a per-platform checklist:** the Rust and Tauri layer —
sidecar spawn, WSL lifecycle, keychain access, Android and Termux. Driving these
in CI costs more than it returns for a personal tool. Checklist per platform:
install, first run, launch a session, kill it, quit the app, confirm no orphaned
process. Windows adds: measure loopback forwarding before anything else; force-quit the
app and relaunch, confirming the held port is diagnosed by name rather than the
client silently attaching to the orphan; and confirm `rm -rf ~/.cdash` is the
documented removal step.

**Known gaps:** nothing automated proves the Windows pidfile teardown works.
Nothing detects a stale-but-same-version WSL copy-in.

## Sequencing

Each step leaves the tree working and testable.

1. **`lib/host/`** — PATH prepend (probed + known locations + inherited,
   deduped) with its 2000 ms timeout and inherited-PATH fallback, the pure
   `missing` probe, `df` contract change, `sh()` explicit dedupe key. **No
   call-site changes.**
2. **UI** — `backoff.js` + its test (one script tag); `api()` propagates status
   **and** gains the transport branch; `poll()` applies `next()`; `sw.js`
   navigations network-first and cache-populating, sub-resources
   stale-while-revalidate, precache deleted; service-worker registration gated
   to web mode on the same predicate. Ships before any auth exists, so 401 is
   unreachable and the only user-visible change is reconnect latency after a
   sustained outage.
3. **`server.js` restructure** — `export createApp(ctx)`, log the bound port,
   `server.on('error')` → diagnosed stderr and exit 3.
   `const host = process.env.CDASH_BIND` **with no default**, which is
   byte-equivalent to today's `listen(port, cb)`. Pure refactor.
4. **`lib/auth/`** — guard chain (`none`, `bearer`, `cf-access`,
   `trusted-proxy`, **`password`**), **`GET /login` + `POST /api/login` +
   `POST /api/logout`**, **`public/login.html`**, **`npm run set-password`**,
   the three-rule login throttle, registration order with its three enumerated
   exceptions, **bind default `127.0.0.1` lands here**, `/api/hostinfo`,
   `package.json` version field, and the integration test.
5. Tauri client — Linux and macOS managed profile; `/api/health` readiness with
   spawn-result precedence. **Confirm the Tauri detection predicate here.**
6. Windows and WSL profile — **measure the loopback relay first**;
   `CDASH_BIND=127.0.0.1 CDASH_AUTH=none`; copy-in; pidfile written after
   `listening` and deleted on exit; teardown by pid; fail loudly and stop if
   unreachable.
7. VPS profile — auth UI and keychain, the `Password` profile variant and
   Rule A's login-once-per-credential-generation; the client-side failure rows.
8. Android.

Steps 1–4 are independently valuable: they make the existing web app correct on
macOS, safe on a VPS, **and operable on a VPS**, with or without any Tauri work.

## Tradeoffs carried

- **The entire safety guarantee is a line-ordering property of one file,
  defended by one test file.** Any future edit that registers a route above the
  guard in `server.js` is an unauthenticated reach of an origin that runs
  `--dangerously-skip-permissions` and enumerates from `/`. The integration
  test's router enumeration plus the explicit `/` and `/sw.js` assertions is the
  only mechanism standing between a one-line reordering and a breach.
- **The version string is load-bearing in two places and bumped by hand.**
  `/api/hostinfo` and the WSL copy-in cache key both read it; nothing verifies
  it, and no cheap check exists. Forgetting to bump it after editing `lib/`
  leaves a stale server in the distro, undetected — the version-skew banner
  cannot fire, because the versions match.
- **An orphaned WSL server is diagnosed, not recovered.** Automatic reclamation
  was removed with the credential that made it necessary. A held port becomes a
  startup failure naming the port; a previous-generation orphan reached on
  another port is caught only by the version-skew banner.
- **The untested surface is the newest surface.** Automated coverage reaches
  steps 1–4 well. Everything from step 5 on — spawn precedence, WSL lifecycle,
  the loopback measurement, keychain access — is manual-checklist-only.
- **Every host under the registrable domain is inside the trust boundary.** A
  compromised sibling subdomain is same-site, and only the content-type layer
  stands between it and an authenticated POST.
- **Denial of new logins remains achievable at volumetric scale** (~1024
  concurrent connections, ~51 req/s). This process cannot defend that and does
  not claim to; existing sessions are unaffected, but a 12-hour absolute
  lifetime means a long enough attack eventually bites.
- **Every app launch costs one login round trip** (~100 ms of KDF on the server).
  The alternative — persisting the session id — was rejected: it buys only that
  round trip, since the passphrase is already stored and no re-prompt is avoided,
  and it costs a credential that grants RCE for up to 12 hours without the
  passphrase and cannot be revoked by changing it.
- **A forgotten passphrase is unrecoverable** — no reset flow, by design, since
  one needs an identity provider. Remedy is shell access to re-set the hash.
- **Offline works from the second visit.** A service worker does not control the
  page load during which it installs, and precaching was removed.
- **Every credential path terminates at the same blast radius.** Nothing here
  reduces what an authenticated caller can do — correctly, since confining
  `/api/browse` is out of scope. The design's safety is a perimeter argument with
  no defence in depth behind it.

## Deployment topology and trust boundary

Five facts that were implicit for most of this design's life and are stated here
because two HIGH-severity defects came from reasoning about the origin in
isolation when it is not isolated.

1. **The origin is a public hostname with siblings.** Every host under the
   registrable domain is same-site and inside the trust boundary. A sibling with
   an XSS is a sibling with the session cookie's `SameSite` protection. Prefer a
   domain with no untrusted siblings.
2. **The origin has two classes of caller, one of them automated.** The desktop
   client polls every 4 seconds — **21,600 times a day**. Any per-origin
   counter, limiter, or lock is amplified by that factor. The checkable question
   for any shared mechanism is: *what does this look like when the desktop
   client does it 21,600 times a day?* Asking it of the login throttle would
   have found the retry-loop defect without measuring it.
3. **The blast radius behind every credential is identical** —
   `--dangerously-skip-permissions` and unconfined `/api/browse`. Perimeter
   only, no defence in depth, deliberately, at personal-tool scope.
4. **Anything shared across callers is a coupling**, and **any bound shared
   across callers must state what an unauthenticated caller must spend to
   exhaust it.** The login throttle's pending-request bound is the only such
   shared resource today; it took a HIGH-severity finding to notice it, and a
   second one to notice that its overflow behaviour mattered more than its size.
5. **"Loopback is safe" is a desktop assumption that does not hold on Android.**
   Everywhere else in this design, binding `127.0.0.1` reduces the exposure to
   "any local process on this box," and that is accepted: on a desktop or a VPS
   the local process population is the user's own software. **Android does not
   isolate loopback between apps.** Any installed application can open
   `http://127.0.0.1:8080` and reach a Termux-hosted host agent — which means
   RCE inside Termux for any app that scans local ports, with no permission
   prompt and nothing in the UI to indicate it happened.

   Consequence: **`CDASH_AUTH=password` is the recommended posture for the Termux
   server, not just for the VPS.** It is the only configuration in this design
   where a loopback bind alone is not a sufficient perimeter. This is what makes
   the loopback exemption in
   [the `password` guard](#browser-authentication--the-password-guard) necessary
   rather than a convenience — without it, the recommended Android posture is one
   the boot check refuses to start.

   The exposure is not created by this design and cannot be closed by it: any
   server Termux hosts has the same property. What this design owes is to not
   *recommend* the unguarded configuration, and to make the guarded one bootable.

## Out of scope

- Bundling a static `tmux` for macOS. The lookup chain makes this a later
  configuration change rather than a rewrite.
- Multi-user support. Auth here gates access to a single user's host; it does not
  partition sessions between users.
- iOS. Tauri supports it, but there is no iOS equivalent of Termux to run the
  host agent, so only the VPS profile could ever work.
- Live log streaming over SSE or WebSocket. Polling stays as-is; the graduated
  backoff bounds its worst case.
- Root-confining `/api/browse`. The guard chain is the answer to origin exposure.
