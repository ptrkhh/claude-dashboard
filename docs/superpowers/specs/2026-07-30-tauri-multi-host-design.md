# Tauri client and multi-host deployment — design

Date: 2026-07-30
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

**The UI is already decoupled.** Every API call goes through one helper,
`api()` at `public/app.js:112`, using relative paths. Making the base URL and
auth headers configurable is a small, contained change.

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
enumerates the filesystem from `/`. An unauthenticated reach of the origin is
remote code execution as the running user. Cloudflare Access protects a
hostname, not a socket: any path that reaches the origin directly — an open
port, an accidentally published container port, another tenant on the box —
bypasses it entirely.

### Items to confirm during implementation

The Cloudflare MCP connector was not authorized in the design session, so the
following were taken from prior knowledge rather than verified live. Confirm
before relying on them:

- CF Access injects `Cf-Access-Jwt-Assertion` on proxied requests.
- The JWKS endpoint is `https://<team>.cloudflareaccess.com/cdn-cgi/access/certs`.
- Service-token requests carry a JWT with a `common_name` claim in place of `email`.
- Using a service token requires a Service Auth policy on the CF Access application.

If any of these differ, the `cf-access` guard changes but no other component does.

## Decisions

| Decision | Choice |
|---|---|
| Number of Tauri builds | One codebase; "local" and "VPS" are runtime profiles, not builds |
| Client transport | All API calls proxied through Rust, not webview `fetch` |
| macOS missing `tmux` | Detect and guide the user to `brew install tmux`; do not bundle |
| Auth architecture | Pluggable guard chain, bearer-first, composable |
| VPS web browser auth | CF Access JWT verification only, no bearer token |
| Default bind address | `127.0.0.1` (breaking change), explicit opt-out to expose |
| JWT verification | Add the `jose` dependency; do not hand-roll |

## Architecture

Three layers, split at the HTTP boundary that already exists.

### 1. Host agent — `server.js` + `lib/`

The only component that touches `tmux`, `ps`, `df`, `git`, and `~/.claude`.
Always runs on the machine where Claude sessions live. Remains plain Node with
no build step. Gains a `HostProfile` for OS-specific behavior and an auth guard
chain.

### 2. UI — `public/`

One copy, shared by all delivery modes. `api()` (`public/app.js:112`) stops
calling `fetch` directly and goes through a transport shim:

- `public/transport/web.js` — relative-path `fetch`; identical to today
- `public/transport/tauri.js` — forwards to Rust via `invoke`

Nothing else in the UI changes. It never learns which mode it is running in.

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

Nothing is relocated.

## Host agent changes

### `lib/host/` — OS abstraction

**PATH resolution.** At boot, probe the user's real PATH via
`$SHELL -l -c 'echo $PATH'` and prepend it to `process.env.PATH`. GUI-launched
applications on macOS and Linux inherit a minimal PATH that excludes
`/opt/homebrew/bin` and `~/.local/bin`, so `claude`, `tmux`, and `git` all
appear missing even though they work in a terminal.

**Binary resolution.** Resolve each required binary through a lookup chain —
bundled resource, then `PATH`, then known locations (`/opt/homebrew/bin`,
`/usr/local/bin`) — and record which are missing. This chain is the seam that
allows bundling a binary later without touching call sites.

**The `df` fix.** `lib/collect.js:223` uses `df -k --output=target,avail,size`,
which is GNU coreutils only and fails on macOS, where BSD `df` has no
`--output` and a different column order. Instead of branching the parser on
column order, change the contract: query one mount at a time and label the
result with the path that was requested, so no mount-name parsing is needed.

- Linux and Termux: `df -k --output=avail,size <path>` → avail at index 0, size at index 1
- macOS: `df -k <path>` → avail at index 3, size at index 1

This also removes a latent bug: mount points containing spaces mis-parse under
the current positional split.

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

Configured via `CDASH_AUTH`, comma-composable: `CDASH_AUTH=bearer,cf-access`
requires both.

**Dependency.** Add `jose` for JWT verification. Hand-rolling signature
verification invites algorithm-confusion and `alg: none` bugs. This takes the
project from one dependency to two.

### Bind address — breaking change

`server.js:70` currently calls `app.listen(port)`, binding all interfaces. The
new default is `127.0.0.1`, overridable with `CDASH_BIND=0.0.0.0`, which logs a
warning naming the RCE risk when `CDASH_AUTH=none`.

This breaks existing LAN access until users set `CDASH_BIND` explicitly. That
is intended — the dangerous topology should require a deliberate act — and the
README must document it.

### Health endpoints

- `GET /api/health` — unauthenticated, returns `{ ok: true }` and nothing more.
  The Tauri client polls it to detect when a managed server is ready, so it must
  not leak host details to an unauthenticated caller.
- `GET /api/hostinfo` — authenticated, returns platform, server version, and
  `missing: ["tmux"]`.

`hostinfo` delivers the macOS setup story: when `tmux` is missing the UI shows a
setup screen with the install command and a re-check button, rather than failing
every launch with an opaque error. Its version string also lets a client warn on
version skew against a server it did not ship.

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

- Secrets never enter the webview's JS context, so XSS cannot read them.
- CORS does not apply, so the server needs no `Access-Control-Allow-Origin`
  configuration for desktop clients.
- CF service-token headers can be attached, which a webview navigation cannot do.

### Secret storage

Use the `keyring` crate: macOS Keychain, Windows Credential Manager, Linux
Secret Service. Two fallbacks are designed in rather than discovered later:

- **Headless Linux** with no Secret Service running: fall back to a `0600` file
  in the app config directory, with a visible UI warning that the token is
  stored unencrypted.
- **Android**, which `keyring` does not cover: use app-private storage, which
  the OS isolates per-app. This is the same protection Termux's own data has.

Non-secret profile fields (name, URL, distro) go in `tauri-plugin-store` as
plain JSON.

### Managed server, per platform

**Linux and macOS.** Tauri sidecar. Bundle a per-arch `node` binary plus
`server.js` and `lib/` as resources. On launch: pick a free port, spawn
`node server.js` with `CDASH_BIND=127.0.0.1` and `CDASH_AUTH=none`, poll
`/api/health` until ready, then load the UI. No-auth is correct here precisely
because the socket is loopback-only and unreachable off-box. Tear the child down
explicitly on exit.

**Windows.** Spawn through `wsl.exe -d <distro> -- bash -lc "..."`. Two details
matter:

- **Copy the server into the distro** at `~/.cdash/<version>/` on first run and
  on version change, rather than executing from `/mnt/c/...`. Running Node
  across the 9p filesystem boundary is slow and occasionally unreliable.
- **Shutdown requires a pidfile.** Killing the `wsl.exe` process does not
  reliably kill the Linux process it started. The server writes its pid and
  teardown runs `wsl.exe -d <distro> -- kill <pid>`. Without this, orphaned
  servers accumulate and hold the port across restarts.

Distro selection comes from `wsl.exe -l -q`, shown as a settings dropdown
defaulting to the WSL default distro. Windows uses the distro's own Node — a
Windows Node binary cannot run inside WSL — with detect-and-guide if absent.
Node is near-certainly present, since Claude Code requires it.

Because the folder picker browses *the server's* filesystem, it naturally shows
WSL paths. No `\\wsl$\` path translation is needed anywhere. WSL2 forwards
loopback, so a server on `:8080` inside the distro is reachable from Windows at
`localhost:8080` with no extra configuration.

**Android.** No managed server, by OS design. The app ships a default profile
pointing at `http://localhost:8080` and expects Termux to run the server itself
via `termux-services`, or `termux-boot` to start on boot. Optionally, an opt-in
button fires a single `com.termux.RUN_COMMAND` intent and then polls
`/api/health`; because that channel returns no exit code or stdout, it is a
convenience, not supervision, and it requires the user to have set
`allow-external-apps=true` themselves.

**VPS profile, any platform.** `managed: None` — a base URL plus credentials.

### First run

Desktop platforms auto-create a working managed-local profile and go straight to
the dashboard, with the macOS `tmux` setup screen as the only possible detour.
Android and any VPS profile go through a short form — URL, auth method,
credentials — plus a **Test connection** button that calls `/api/health` and
`/api/hostinfo` and reports which of the two failed. Distinguishing "cannot
reach the host" from "reached it but auth was rejected" is the difference
between a two-minute fix and an hour of guessing.

The existing UI is already responsive with a touch-friendly picker, so it
carries to a phone webview without layout work.

### Service worker

`public/sw.js` caches `/`, `/app.js`, and other shell paths and assumes
same-origin `/api/`. Both assumptions break in a Tauri webview. Registration is
gated to web mode only.

### Remote-control links

No work needed. `rcLink` resolves to `https://claude.ai/code/<id>`
(`lib/collect.js:90`), an absolute public URL, so "Open in Claude" behaves
identically from a local app, a phone, or a VPS client.

## Error handling

Every failure names which layer broke. Today a missing binary yields an empty
string from `sh()` (`lib/collect.js:13-19`) and the UI silently shows nothing —
acceptable for a single-host local tool, not across four platforms and a
network.

| Failure | Detected by | Surfaced as |
|---|---|---|
| Managed server will not start | `/api/health` poll times out | Startup screen with the sidecar's captured stderr |
| `tmux` or `claude` missing | `HostProfile` probe via `/api/hostinfo` | Setup screen naming the binary and install command |
| Wrong WSL distro, or no Node in it | `wsl.exe` exit code and stderr | Settings error naming the distro |
| Host unreachable | `api_request` transport error | "Cannot reach host" plus the URL tried |
| Auth rejected | HTTP 401 or 403 | "Reached host, auth rejected" plus which guard failed |
| CF JWT expired in browser | 403 from the `cf-access` guard | Full-page reload, re-triggering the CF SSO redirect |
| Server version differs from client | `/api/hostinfo` version | Non-blocking banner |

Two behaviors are specified explicitly:

- **Polling backs off.** On repeated transport failure the 4-second poll backs
  off to 30 seconds and shows a single persistent "disconnected" indicator
  rather than a stream of errors. It recovers immediately on first success.
- **Auth failures do not retry.** A 401 stops the poll and requires user action.
  Only transport errors back off and retry. Otherwise a stale token generates
  login attempts against Cloudflare indefinitely.

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

**Integration, Node only, no Tauri required:** boot the real server on an
ephemeral port and assert that `/api/health` is reachable unauthenticated while
`/api/sessions` returns 401 under each auth mode. This is the test that catches
an accidental auth bypass and is the highest-value test in the suite.

**Manual, documented as a per-platform checklist:** the Rust and Tauri layer —
sidecar spawn, WSL lifecycle, keychain access, Android and Termux. Driving these
in CI costs more than it returns for a personal tool. Checklist per platform:
install, first run, launch a session, kill it, quit the app, confirm no orphaned
process.

**Known gap:** nothing automated proves the Windows pidfile teardown works. It
is on the manual checklist; orphaned servers holding the port are the symptom to
watch for.

## Sequencing

Each step leaves the tree working and testable.

1. `lib/host/` — PATH resolution, binary lookup chain, `df` fix
2. `lib/auth/` — guard chain, bind change, the two health endpoints
3. `public/transport/` — shim with web transport only; behavior unchanged
4. Tauri client — Linux and macOS managed profile
5. Windows and WSL profile
6. VPS profile — auth UI and keychain
7. Android

Steps 1 and 2 are independently valuable: they make the existing web app correct
on macOS and safe on a VPS, with or without any Tauri work.

## Out of scope

- Bundling a static `tmux` for macOS. The lookup chain makes this a later
  configuration change rather than a rewrite.
- Multi-user support. Auth here gates access to a single user's host; it does not
  partition sessions between users.
- iOS. Tauri supports it, but there is no iOS equivalent of Termux to run the
  host agent, so only the VPS profile could ever work.
- Live log streaming over SSE or WebSocket. Polling stays as-is.
