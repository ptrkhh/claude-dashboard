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
tested. The first manual checklist run is the first real test of steps 6–9, not
a formality.

## Decisions

| Decision | Choice |
|---|---|
| Number of Tauri builds | One codebase; "local" and "VPS" are runtime profiles, not builds |
| Client transport | All API calls proxied through Rust, not webview `fetch` |
| macOS missing `tmux` | Detect and guide the user to `brew install tmux`; do not bundle |
| Auth architecture | Pluggable guard chain, composable, AND-semantics |
| VPS web browser auth | CF Access JWT verification only, no bearer token |
| Default bind address | `127.0.0.1` (breaking change), explicit opt-out to expose |
| JWT verification | Add the `jose` dependency; do not hand-roll |
| UI module system | Classic scripts publishing globals; no build step, no bundler |

## Architecture

Three layers, split at the HTTP boundary that already exists.

### 1. Host agent — `server.js` + `lib/`

The only component that touches `tmux`, `ps`, `df`, `git`, and `~/.claude`.
Always runs on the machine where Claude sessions live. Remains plain Node with
no build step. Gains a `HostProfile` for OS-specific behavior and an auth guard
chain.

### 2. UI — `public/`

One copy, shared by all delivery modes. `api()` (`public/app.js:112`) stops
calling `fetch` directly and goes through a transport selected at runtime.

Four **statically declared classic scripts**, loaded in document order before
`app.js`:

```html
<script src="transport/backoff.js"></script>   <!-- globalThis.CDASH_BACKOFF    -->
<script src="transport/web.js"></script>       <!-- CDASH_TRANSPORTS.web        -->
<script src="transport/tauri.js"></script>     <!-- CDASH_TRANSPORTS.tauri      -->
<script src="transport/select.js"></script>    <!-- globalThis.CDASH_TRANSPORT  -->
<script src="app.js"></script>
```

`select.js` **selects over already-loaded globals and loads nothing**. This is
load-bearing: `app.js:386` calls `poll()` synchronously at top level, which
reaches `api()` and therefore the transport global. Every runtime script-loading
mechanism available to a classic script is asynchronous, so any arrangement in
which `select.js` *fetches* an implementation throws `TypeError` on first paint.
Parser-inserted classic scripts without `async`/`defer` execute synchronously in
document order, which is what makes the global safe to read.

Accepted costs: `tauri.js` ships to web clients and `web.js` ships to Tauri
clients (a few hundred inert bytes each, each guarding its own availability),
and four script tags must stay mirrored in `sw.js`'s `SHELL` — which the
shell-manifest test enforces.

`type="module"` was rejected: `public/app.js` is a 388-line classic script with
no `import`/`export` statements that relies on top-level globals, so converting
it changes scope and execution timing throughout.

Nothing else in the UI changes *for the transport swap itself*. Two adjacent
changes ship alongside it: the service-worker shell list and cache version, and
the error-model changes under [Error handling](#error-handling).

### 3. Tauri client — `src-tauri/`

One Rust codebase, one binary per platform, driven by user-configured profiles:

```
Profile {
  name, base_url,
  managed: None | Node { .. } | Wsl { distro, .. },   // does the client start the server?
  auth:    None | Bearer | CfServiceToken | Both,
}
```

A single install can hold several profiles and switch between them.

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

**Binary resolution.** `host.bin('tmux')` returns an **absolute path**, resolved
through a lookup chain — bundled resource, then `PATH`, then known locations
(`/opt/homebrew/bin`, `/usr/local/bin`) — recording which are missing. Every
call site passes the resolved absolute path as `cmd`. This touches all seven
current sites: `lib/collect.js:41,135,141,221,222,223` and `server.js:63`.

The claim that this chain "allows bundling a binary later without touching call
sites" applies to *subsequent* changes; introducing the seam necessarily touches
every site that will use it.

`server.js:63`'s `run('tmux', ['kill-session', …])` additionally moves into
`lib/collect.js` as an exported `killSession(ctx, name)`, so `server.js` contains
no subprocess call at all and the invariant "all subprocess spawning lives in
`lib/`" becomes checkable by grep. `server.js` keeps the name-regex validation.

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
| VPS, browsers **and** Tauri clients | `cf-access` | One guard; `email` for browser SSO, `common_name` for the service token |
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

### Guard placement

`buildGuard` is registered in `server.js` **after** `express.json()` and the
`/api/health` route, and **before** `express.static(public)` and every other
`/api` route:

```
express.json() → /api/health → buildGuard(config) → express.static(public) → all /api routes
```

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
adopted mechanism detects that** — the token matches, the version matches, and
reclamation does not fire. It is on the Windows manual checklist.

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

**Two credential classes.**

**Class 1 — user credentials.** A CF service-token pair or a bearer token the
user obtained elsewhere, typed once into a form, and expects to persist. Stored
via the `keyring` crate: macOS Keychain, Windows Credential Manager, Linux
Secret Service. Long-lived; survives app restarts; the user can see that one
exists (`has_secret`) and can replace or delete it. Two fallbacks are designed
in rather than discovered later:

- **Headless Linux** with no Secret Service running: fall back to a `0600` file
  in the app config directory, with a visible UI warning that the token is
  stored unencrypted.
- **Android**, which `keyring` does not cover: use app-private storage, which
  the OS isolates per-app. This is the same protection Termux's own data has,
  and it is weaker than hardware-backed Keystore.

**Class 2 — ephemeral managed-server credentials.** Machine-generated by the
Tauri client at spawn time for a server *it starts* — currently the WSL
profile's `CDASH_TOKEN`. Never shown to the user, never written to the keyring,
never written to the fallback file, never placed in `tauri-plugin-store`; held
only in the Tauri process's memory and passed to the child through its
environment. The user cannot see it, is never asked for it, and must never be
asked to fix it — **a prompt for a Class 2 credential is a bug by definition.**

**The governing rule.** *An ephemeral credential must not outlive, nor be
outlived by, the server process it authenticates.* The client owns both ends of
the pair and must keep them in the same generation.

**Where the rule is violated, and what must follow.** The client cannot
guarantee the server dies with it — killing `wsl.exe` does not reliably kill the
Linux process, orphans hold the port, and nothing automated proves teardown
works. So the rule is enforced by **detection and reclamation**, not by
assumption: a managed profile that reaches a listening server it cannot
authenticate to has, by the rule, found a previous generation of its own server.
It must reclaim and respawn, never surface the condition as an auth failure.

The Linux/macOS sidecar uses `CDASH_AUTH=none`, has no Class 2 credential, and
no generation to mismatch; the rule is vacuous there. If a future change gives
it a token, the reclamation path becomes mandatory for it too.

### Managed-server readiness

Readiness is a **two-step probe**, on every managed platform:

1. Poll `GET /api/health` until 200 — *something* is listening.
2. Call `GET /api/hostinfo` **with the profile's credentials**. Only a 200 here
   declares the managed server ready.

- `hostinfo` **200** → ready; load the UI.
- `hostinfo` **401/403 on a managed profile** → the port is held by a server this
  client cannot authenticate to. Diagnosed as **"a previous server is still
  running"**, not as an auth failure. Reclaim (below) and respawn **once**. If
  the second attempt also fails, report and stop.
- `hostinfo` **401/403 on a VPS profile** → genuine auth failure.

Using an unauthenticated liveness probe as a readiness signal is what would
otherwise let an orphaned server answer 200 while rejecting every real call, on
a profile where the user was never asked for a credential and has nothing to
correct.

### Managed server, per platform

**Linux and macOS.** Tauri sidecar. Bundle a per-arch `node` binary plus
`server.js` and `lib/` as resources. On launch: pick a free port, spawn
`node server.js` with `CDASH_BIND=127.0.0.1` and `CDASH_AUTH=none`, run the
two-step readiness probe, then load the UI. No-auth is correct here precisely
because the socket is loopback-only **on the same kernel**, with no relay in
between, and free-port selection means a new client never contacts an orphan.
Tear the child down explicitly on exit.

**Windows.** Spawn through `wsl.exe -d <distro> -- bash -lc "..."`.

- **Spawn contract:** `CDASH_BIND=127.0.0.1` inside the distro, plus
  `CDASH_AUTH=bearer` and `CDASH_TOKEN=<random per-launch secret>` generated by
  the client and held in memory (Class 2). The client attaches it via
  `api_request`, so no UI or user action is involved. Rationale: whether WSL2's
  localhost relay reaches a `127.0.0.1` listener inside the distro, or requires
  `0.0.0.0`, is unverified. The token makes the safety posture **independent of
  that answer** — if a fix later requires `0.0.0.0`, the listener is still
  credentialed. Cost is near zero; the token path already exists.
- **Copy the server into the distro** at `~/.cdash/<version>/` on first run and
  on version change, rather than executing from `/mnt/c/...`. Running Node
  across the 9p filesystem boundary is slow and occasionally unreliable.
- **Pidfile rules.** Shutdown and reclamation both depend on the pidfile, so its
  semantics are specified rather than assumed:
  1. The pidfile is written **only after the `listening` event fires — never
     before**. It contains `{pid, port, version, startedAt}`.
  2. It is deleted on clean exit.
  3. On `EADDRINUSE` the server exits 3 with stderr and writes **no pidfile**
     (see [Server structure](#server-structure)).
  4. Reclamation trusts a pidfile only if its recorded `port` equals the port
     being probed **and** its `pid` is live. Otherwise the client reports
     *"port `<p>` is held by a process this client did not start"* — naming
     **the port, never a pid**. A dead or mismatched pid is never shown.
  5. After the kill, the client polls until `/api/health` stops answering
     (bounded, 5s) before respawning. Exactly one respawn.

  Rules 1 and 3 together mean the pidfile can only ever name a process that
  actually listened. Without them, a server that dies on a held port before
  listening leaves a pidfile naming a dead pid; reclamation then kills a corpse,
  the respawn fails, the client reports a pid that was never the orphan, and the
  real orphan survives every restart.
- **Cleanup.** The copy-in path is keyed by version, so a new version creates a
  new directory rather than replacing one. On a successful start, delete every
  `~/.cdash/*` directory other than the one just started from. Uninstalling the
  Tauri app leaves `~/.cdash/` behind, because an uninstaller cannot reach inside
  a WSL distro; the README documents `rm -rf ~/.cdash` as the manual step. This
  is the only place in the design that writes persistent state into a namespace
  an uninstaller cannot reach.

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

`public/sw.js` caches `/`, `/app.js`, and other shell paths. Three changes:

**Navigations become network-first.** Today the fetch handler is cache-first for
everything outside `/api/`, and `SHELL` includes `/`. That means a full-page
reload is answered from cache and never reaches the network — so the prescribed
recovery for an expired CF session (reload, re-trigger the SSO redirect) cannot
execute, because the request never reaches Cloudflare's edge.

```js
self.addEventListener('fetch', e => {
  const url = new URL(e.request.url);
  if (url.pathname.startsWith('/api/')) return;                     // network only, unchanged
  if (e.request.mode === 'navigate') {
    e.respondWith(fetch(e.request).catch(() => caches.match('/'))); // network first, cache is offline fallback
    return;
  }
  e.respondWith(caches.match(e.request).then(hit => hit || fetch(e.request)));
});
```

Sub-resources stay cache-first, so the offline story is unchanged; the `catch`
bounds the online-but-slow cost at one failed request.

**Registration is gated to web mode only.** Both of `sw.js`'s assumptions break
in a Tauri webview. The gate is an `index.html:106` edit reading the same
detection `select.js` uses, with a `.catch()` on `register()`.

**Standing rule: any change to `public/` bumps `CACHE`, and adding a script or
stylesheet adds it to `SHELL`.** `install` only re-runs when `sw.js`'s bytes
change, and only primes from `SHELL`; a bump without the corresponding `SHELL`
entry re-primes an incomplete shell, and a `SHELL` entry without a bump never
reaches an installed client. Either mistake is invisible in development — a
browser with an empty cache works perfectly — and breaks only an installed PWA,
only offline. The shell-manifest test enforces the `SHELL` half; nothing
enforces the bump.

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
| Managed server will not start | readiness probe times out | Startup screen with the sidecar's captured stderr |
| Port held by a server we cannot authenticate to | `/api/health` 200 but `/api/hostinfo` 401 on a managed profile | "A previous server is still running" → pidfile reclamation and one respawn |
| `tmux` or `claude` missing | `HostProfile` probe via `/api/hostinfo` | Setup screen naming the binary and install command |
| Wrong WSL distro, or no Node in it | `wsl.exe` exit code and stderr | Settings error naming the distro |
| Host unreachable | `api_request` transport error | "Cannot reach host" plus the URL tried |
| Auth rejected | HTTP 401 or 403 | "Reached host, auth rejected" plus the profile's configured auth method and URL. The server body names no guard. |
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

- **Auth failures do not retry.** A 401 stops the poll and requires user action.
  Only transport errors back off and retry. Otherwise a stale token generates
  login attempts against Cloudflare indefinitely.

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
- **Service-worker shell consistency** — parse `public/index.html` for local
  `src`/`href` references and assert each appears in `public/sw.js`'s `SHELL`.
  Pure, ~15 lines, passes green against HEAD today. This defect class recurred
  three times during design review and was caught by a human every time; the
  failure is invisible in development and breaks only an installed PWA, offline.
  It is one-directional and does not catch a missing `CACHE` bump.

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
process. Windows adds: force-quit the app and relaunch, confirming the orphan is
reclaimed rather than stranding the client; and confirm only one `~/.cdash/*`
version directory remains after an upgrade.

**Known gaps:** nothing automated proves the Windows pidfile teardown or
reclamation works. Nothing detects a stale-but-same-version WSL copy-in.

## Sequencing

Each step leaves the tree working and testable.

1. **`lib/host/`** — PATH resolution with its timeout, absolute-path binary
   chain (7 sites incl. `server.js:63` → `killSession`), `df` contract, `sh()`
   explicit dedupe key. **Plus the shell-manifest test**, which lands green
   before any `public/` change and therefore guards every step that follows.
2. **UI error model, web mode** — `backoff.js` + its test; `api()` propagates
   status; `poll()` applies `next()`; `sw.js` navigations network-first;
   `/transport/backoff.js` added to `SHELL`; `CACHE` bumped. Ships before any
   auth exists, so 401 is unreachable and the only user-visible change is
   reconnect latency after a sustained outage.
3. **`server.js` restructure** — `export createApp(ctx)`, log the bound port,
   `server.on('error')`. `const host = process.env.CDASH_BIND` **with no
   default**, which is byte-equivalent to today's `listen(port, cb)`. Pure
   refactor.
4. **`lib/auth/`** — guard chain, registration order, **bind default
   `127.0.0.1` lands here**, `/api/hostinfo`, `package.json` version field, and
   the integration test.
5. **`public/transport/`** — four statically declared files, `index.html` script
   tags, service-worker mode gate, `sw.js` `SHELL` + `CACHE` bumped again.
6. Tauri client — Linux and macOS managed profile; two-step readiness probe.
7. Windows and WSL profile — Class 2 token, pidfile teardown and reclamation,
   copy-in cleanup.
8. VPS profile — auth UI and keychain (Class 1); the client-side failure rows.
9. Android.

Steps 1–4 are independently valuable: they make the existing web app correct on
macOS, safe on a VPS, **and operable on a VPS**, with or without any Tauri work.

## Tradeoffs carried

- **The entire safety guarantee is a line-ordering property of one file,
  defended by one test file.** Any future edit that registers a route above the
  guard in `server.js` is an unauthenticated reach of an origin that runs
  `--dangerously-skip-permissions` and enumerates from `/`. The integration
  test's router enumeration plus the explicit `/` and `/sw.js` assertions is the
  only mechanism standing between a one-line reordering and a breach.
- **The version string is load-bearing in three places and bumped by hand.**
  `/api/hostinfo`, the WSL copy-in cache key, and the copy-in cleanup key all
  read it; nothing verifies it, and no cheap check exists.
- **Cache coherence has three couplings and one-and-a-half enforcement
  mechanisms.** `index.html` ↔ `SHELL` is enforced; `SHELL` ↔ `CACHE` is not;
  "every `public/` step bumps" is convention only.
- **The untested surface grew faster than the tested one.** Automated coverage
  reaches steps 1–5 well. The two-step readiness probe, the reclamation
  protocol, Class 2 token generation, and copy-in cleanup are all
  manual-checklist-only, and three of them are destructive or lifecycle-critical.
- **Every credential path terminates at the same blast radius.** Nothing here
  reduces what an authenticated caller can do — correctly, since confining
  `/api/browse` is out of scope. The design's safety is a perimeter argument with
  no defence in depth behind it.

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
