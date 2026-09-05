# claude-dashboard

A small local web dashboard for launching, monitoring, resuming, and killing Claude Code sessions running in tmux, with basic system stats (CPU/mem/disk) and a live log panel. The agent is a single Rust binary; the UI is vanilla HTML/CSS/JS with no build step. There is no authentication yet — sessions are launched with `--dangerously-skip-permissions`, so run it only on a trusted LAN or behind Cloudflare Access.

The launcher has a touch-friendly folder picker (the folder button in the directory field) that browses the server's filesystem from `/`, with server-backed **Recents** (auto-recorded on launch) and **Favorites**. Since it can enumerate any directory, keep the "trusted LAN / behind Cloudflare Access" caveat above in mind. Recents and favorites persist to `$CLAUDE_DIR/cdash-places.json`.

The dashboard polls `/api/sessions` on a graduated ladder — 4s, 8s, 15s, capped
at 30s — resetting on any successful poll, tab refocus, or button press. A 401
or 403 halts the poll until you act; throttling never does. The service worker
caches at runtime (network-first navigations, stale-while-revalidate assets), so
offline works from the second visit; there is no precache manifest to maintain.

## Desktop client (Linux)

```
cargo run -p cdash-tauri
```

A Tauri desktop wrapper around the same UI. The agent runs in-process (no separate server, no port to remember), started at app setup on a tokio task. The HTTP boundary is kept even in-process: the same `/api/*` calls still speak HTTP to loopback, tunnelled through the `api_request` Tauri command rather than webview `fetch`. Connection profiles are stored in the app's config directory via `tauri-plugin-store`. Secrets handling arrives in step 10.

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

## Run

```
cargo run -p cdash-agent     # http://127.0.0.1:8080
```

### Static release builds

`scripts/release.sh` builds static musl binaries for VPS/WSL (`x86_64`) and VPS/Termux (`aarch64`) and boots both as the release gate — the gate is the startup banner, since a binary that dies instantly would otherwise pass. Prereqs: `rustup target add x86_64-unknown-linux-musl aarch64-unknown-linux-musl`, `sudo apt install musl-tools qemu-user`, and the [musl.cc](https://musl.cc) `aarch64-linux-musl-cross` toolchain unpacked at `~/.local/opt/`.

CI gates the same two targets but sources its cross-toolchain from `taiki-e/setup-cross-toolchain-action` rather than musl.cc, which stopped responding from GitHub's runners and took every release build down with it. Use the same action if the local prereq above fails.

The `aarch64` binary is confirmed working on-device under Termux (Android, 2026-08-29): UI, host stats and live session data all functional.

Requires `tmux`, `claude` and `git` on `PATH` (`claude` and `git` on Windows, where tmux lives on the WSL side); the agent reports any that are missing at startup.

## Configuration

| Variable | Default | Meaning |
|---|---|---|
| `PORT` | `8080` | Port to listen on. `0` picks any free port. |
| `CDASH_BIND` | `127.0.0.1` | Address to bind. **Breaking change:** the Node agent bound every interface. LAN access now requires setting `CDASH_BIND=0.0.0.0` explicitly. |
| `CLAUDE_DIR` | `~/.claude` | Path to the Claude config/projects directory. |
| `DISK_EXTRA` | — | Optional second mount to report alongside `/`, e.g. `/mnt/d`, or `D:\` on Windows. |
| `CDASH_PUBLIC` | `public` | Directory served as static files. |
| `CDASH_AUTH` | `none` | Comma-composable guard chain, **AND** semantics: `none`, `bearer`, `password`, `trusted-proxy`, `cf-access`. An unknown value refuses to boot rather than falling back to `none`. |
| `CDASH_TOKEN` | — | Required by `bearer`. |
| `CDASH_PASSWORD_HASH` | — | Required by `password`. Produce it with `cdash-agent set-password`. |
| `CDASH_PUBLIC_URL` | — | Required by `password` on a non-loopback bind, and must be `https://` — `__Host-` cookies are discarded by browsers over plain HTTP with no error. |
| `CDASH_ALLOW_INSECURE_COOKIE` | — | `1` accepts session theft on a plain-HTTP origin; drops `Secure` and the `__Host-` prefix together. |
| `CDASH_PROXY_ALLOW` / `CDASH_PROXY_HEADER` | — / `X-Forwarded-Email` | Required by `trusted-proxy`. Unsafe unless the origin is unreachable except through the proxy. |
| `CDASH_CF_TEAM_DOMAIN` / `CDASH_CF_AUD` | — | Required by `cf-access`. |
| `CDASH_LOGIN_PENDING_MAX` | `1024` | Bound on delayed logins pending at once. |
| `CDASH_WSL` | — | Windows only. `0` skips the WSL side entirely. |
| `CDASH_WSL_DISTRO` | the default distro | Windows only. Which distro the WSL side is. |

### `cf-access`

For a VPS behind Cloudflare Access. Cloudflare authenticates the user with your
team's identity provider and injects a signed `Cf-Access-Jwt-Assertion` header;
the agent verifies it against Cloudflare's public keys. There is no cdash
password — one identity, managed in Cloudflare, and adding or revoking a person
changes nothing on the box.

```
CDASH_AUTH=cf-access
CDASH_CF_TEAM_DOMAIN=https://yourteam.cloudflareaccess.com
CDASH_CF_AUD=<the Application Audience tag>
```

The key set is fetched at startup and refreshed hourly, so Cloudflare's key
rotation is invisible. **If the keys cannot be fetched the agent refuses to
start**, naming the reason on stderr — rather than starting and returning 401 to
someone who just authenticated successfully. A failed *refresh* keeps the last
good key set, so a brief Cloudflare outage does not lock anyone out.

**Keep the origin unreachable except through Cloudflare** — a `cloudflared`
tunnel, or a firewall allowing only Cloudflare's addresses. Anyone who can reach
the origin directly skips Access entirely and lands on an agent that launches
sessions with `--dangerously-skip-permissions`. If the origin might be
reachable, use `CDASH_AUTH=password,cf-access`, which requires both.

**Behind a `cloudflared` tunnel, `CDASH_AUTH=none` is enough.** The tunnel dials
outward and proxies to loopback, so there is no inbound port to bypass and
nothing for a guard to protect; Cloudflare's own documentation says a tunnelled
origin need not validate the token. Keep `cf-access` on anyway if **other users
or services share the box** — with `none`, any local user reaching
`127.0.0.1:8080` gets code execution as *you*, which is a real escalation rather
than a no-op. `bearer` does not substitute here: browsers do not send
`Authorization` headers.

The JWKS fetch is gated on `cf-access` being in the chain, not on the two
`CDASH_CF_*` variables being set, so leaving them in a unit file after switching
to `none` does not couple boot to Cloudflare's availability.
