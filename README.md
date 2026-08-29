# claude-dashboard

A small local web dashboard for launching, monitoring, resuming, and killing Claude Code sessions running in tmux, with basic system stats (CPU/mem/disk) and a live log panel. The agent is a single Rust binary; the UI is vanilla HTML/CSS/JS with no build step. There is no authentication yet — sessions are launched with `--dangerously-skip-permissions`, so run it only on a trusted LAN or behind Cloudflare Access.

The launcher has a touch-friendly folder picker (the folder button in the directory field) that browses the server's filesystem from `/`, with server-backed **Recents** (auto-recorded on launch) and **Favorites**. Since it can enumerate any directory, keep the "trusted LAN / behind Cloudflare Access" caveat above in mind. Recents and favorites persist to `$CLAUDE_DIR/cdash-places.json`.

The dashboard polls `/api/sessions` on a graduated ladder — 4s, 8s, 15s, capped
at 30s — resetting on any successful poll, tab refocus, or button press. A 401
or 403 halts the poll until you act; throttling never does. The service worker
caches at runtime (network-first navigations, stale-while-revalidate assets), so
offline works from the second visit; there is no precache manifest to maintain.

## Desktop client (Linux, Windows)

```
cargo run -p cdash-tauri
```

A Tauri desktop wrapper around the same UI. The agent runs in-process (no separate server, no port to remember), started at app setup on a tokio task. The HTTP boundary is kept even in-process: the same `/api/*` calls still speak HTTP to loopback, tunnelled through the `api_request` Tauri command rather than webview `fetch`. Connection profiles are stored in the app's config directory via `tauri-plugin-store`. Secrets handling arrives in step 10.

**On Windows the client links no agent at all** — `cdash-agent` is a
`cfg(not(windows))` dependency. tmux, `claude` and `/proc` live in a WSL distro,
so the agent runs there and the client talks to it over WSL2's loopback relay:
run `PORT=8080 ./cdash-agent` inside WSL, then start `cdash-tauri.exe`. With no
profile saved it defaults to `http://localhost:8080`; a profile overrides that.
Launching the distro for you is step 6 — for now the distro is yours to start.

## Run

```
cargo run -p cdash-agent     # http://127.0.0.1:8080
```

### Release builds

`scripts/release.sh` builds all three artifacts:

| Artifact | Target | Runs on |
|---|---|---|
| `cdash-agent` | `x86_64-unknown-linux-musl` | VPS, WSL |
| `cdash-agent` | `aarch64-unknown-linux-musl` | VPS, Termux on Android (F-Droid Termux — see the design doc's Android section) |
| `cdash-tauri.exe` | `x86_64-pc-windows-msvc` | Windows desktop client |

Both agents are **booted** as the release gate — the gate is the startup banner,
since a binary that dies instantly would otherwise pass. The Windows client
cannot run on a Linux builder, so its gate is that it links, which is also the
gate on the `cfg(not(windows))` split that keeps the agent out of it.

Prereqs: `rustup target add x86_64-unknown-linux-musl aarch64-unknown-linux-musl x86_64-pc-windows-msvc`,
`pip install ziglang && cargo install cargo-zigbuild`,
`cargo install cargo-xwin`, `sudo apt install clang lld qemu-user-static`.

`cargo-zigbuild` supplies musl libc and a cross C compiler for `aws-lc-sys` from
one download, replacing both `musl-tools` and the [musl.cc](https://musl.cc)
toolchain that stopped responding and took every release build down with it. CI
gates the two musl targets through `taiki-e/setup-cross-toolchain-action`
instead, which serves the same purpose on a runner.

Requires `tmux`, `claude` and `git` on `PATH`; the agent reports any that are missing at startup.

## Configuration

| Variable | Default | Meaning |
|---|---|---|
| `PORT` | `8080` | Port to listen on. `0` picks any free port. |
| `CDASH_BIND` | `127.0.0.1` | Address to bind. **Breaking change:** the Node agent bound every interface. LAN access now requires setting `CDASH_BIND=0.0.0.0` explicitly. |
| `CLAUDE_DIR` | `~/.claude` | Path to the Claude config/projects directory. |
| `DISK_EXTRA` | — | Optional second mount to report alongside `/`, e.g. `/mnt/d`. |
| `CDASH_PUBLIC` | `public` | Directory served as static files. |
| `CDASH_AUTH` | `none` | Comma-composable guard chain, **AND** semantics: `none`, `bearer`, `password`, `trusted-proxy`, `cf-access`. An unknown value refuses to boot rather than falling back to `none`. |
| `CDASH_TOKEN` | — | Required by `bearer`. |
| `CDASH_PASSWORD_HASH` | — | Required by `password`. Produce it with `cdash-agent set-password`. |
| `CDASH_PUBLIC_URL` | — | Required by `password` on a non-loopback bind, and must be `https://` — `__Host-` cookies are discarded by browsers over plain HTTP with no error. |
| `CDASH_ALLOW_INSECURE_COOKIE` | — | `1` accepts session theft on a plain-HTTP origin; drops `Secure` and the `__Host-` prefix together. |
| `CDASH_PROXY_ALLOW` / `CDASH_PROXY_HEADER` | — / `X-Forwarded-Email` | Required by `trusted-proxy`. Unsafe unless the origin is unreachable except through the proxy. |
| `CDASH_CF_TEAM_DOMAIN` / `CDASH_CF_AUD` | — | Required by `cf-access`. |
| `CDASH_LOGIN_PENDING_MAX` | `1024` | Bound on delayed logins pending at once. |

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
