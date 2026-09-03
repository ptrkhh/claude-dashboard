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
so the agent runs there and the client talks to it over WSL2's loopback relay.
With no profile saved it defaults to `http://localhost:23274`; a profile
overrides that. Launching the distro for you is step 6 — for now the distro is
yours to start.

There are two Windows builds, x64 and ARM64, because Snapdragon machines run
Windows natively and an x64 build only gets there under Prism emulation. Each
bundles the agent for its own architecture — WSL is native to the host, so an
ARM64 host means an ARM64 distro. Running the wrong one copies an agent the
distro cannot exec; the reason lands in `~/cdash-agent.log`, not at copy time.

When the agent is unreachable the client shows the same setup dialog the
Android app does, with a command to paste into WSL. It hands the binary over
**through the filesystem, not a socket**: the app writes the bundled agent into
`%TEMP%\cdash` and the command copies it from `/mnt/c/…`, which WSL mounts by
default. Temp rather than app data because the copy is staging — dead once the
distro has it — and the dialog re-exports it every time it opens, so a cleaned
temp heals itself. A loopback handoff would not work —
under WSL2's default NAT networking the distro's `127.0.0.1` is its own, not
the host's, and that only changes under `networkingMode=mirrored`. Binding a
routable interface instead would serve the binary to the whole network.

Note the asymmetry: Windows→WSL loopback *does* work by default, which is why
the client reaches the agent on `localhost:23274` with no configuration. It is
only the reverse direction that needs `/mnt/c`.

## Android client

An APK thin client for the phone. It links no agent either — Android cannot
spawn the processes the agent drives, so the agent runs in **F-Droid Termux**
(the Play-store build cannot `exec` from its own data directory; see the design
doc's Android section) and the app talks to it on `localhost:23274`, which
Android does not isolate between apps.

The point of the app over "Add to home screen" is onboarding: the PWA is served
*by* the agent, so it cannot tell anyone how to install one. The APK carries its
own UI, so when the agent is unreachable it shows a setup screen with a
copy-paste command for Termux. The command downloads the agent — bundled in the
APK and served over loopback, since every route through shared storage needs
MediaStore or a permission Termux cannot hold on Android 11+ — and appends a
guard to `~/.bashrc` so the agent starts itself whenever Termux is opened. After
one paste the only thing to remember is to open Termux.

`test/install-script.test.mjs` runs that command in a scratch `$HOME` — it is
pasted into a shell we never see, so it is tested as one, against the template
read out of `app.js` rather than a copy.

## Run

```
cargo run -p cdash-agent     # http://127.0.0.1:23274
```

### Release builds

`scripts/release.sh` builds all three artifacts:

| Artifact | Target | Runs on |
|---|---|---|
| `cdash-agent` | `x86_64-unknown-linux-musl` | VPS, WSL |
| `cdash-agent` | `aarch64-unknown-linux-musl` | VPS, Termux on Android (F-Droid Termux — see the design doc's Android section) |
| `cdash-tauri.exe` | `x86_64-pc-windows-msvc` | Windows desktop client (x64), bundling the x86_64 agent |
| `cdash-tauri.exe` | `aarch64-pc-windows-msvc` | Windows desktop client (ARM64), bundling the aarch64 agent |
| `cdash-dashboard-android-arm64.apk` | `aarch64-linux-android` | Android thin client |

Both agents are **booted** as the release gate — the gate is the startup banner,
since a binary that dies instantly would otherwise pass. The Windows client
cannot run on a Linux builder, so its gate is that it links, which is also the
gate on the `cfg(not(windows))` split that keeps the agent out of it.

The APK step is skipped unless `ANDROID_HOME` and `NDK_HOME` are set, and
`gen/android/` is regenerated on every run rather than tracked. Two things about
it are worth knowing:

- It builds **release**, then signs with the SDK's debug key. Android refuses to
  install an unsigned APK at all, so "unsigned" is not a shippable state — the
  debug key is a local test signature, not a distribution one. The debug *build*
  is not the answer: its unstripped `.so` makes a 138 MB APK, against 21 MB for
  release.
- Gradle enables cleartext HTTP for debug only, so the script patches the
  release build type to allow it. Without that the client cannot reach the agent
  at `http://localhost:23274`, which is its entire job.

Prereqs: `rustup target add x86_64-unknown-linux-musl aarch64-unknown-linux-musl x86_64-pc-windows-msvc`,
`pip install ziglang && cargo install cargo-zigbuild`,
`cargo install cargo-xwin`, `sudo apt install clang lld qemu-user-static`, and
for the APK `cargo install tauri-cli --version "^2"` plus an Android SDK and
NDK. Use the **Rust** Tauri CLI, not the npm one: the npm CLI templates
`node tauri` into the generated gradle, which only resolves in an npm-layout
project, and this is a Rust workspace.

`cargo-zigbuild` supplies musl libc and a cross C compiler for `aws-lc-sys` from
one download, replacing both `musl-tools` and the [musl.cc](https://musl.cc)
toolchain that stopped responding and took every release build down with it. CI
gates the two musl targets through `taiki-e/setup-cross-toolchain-action`
instead, which serves the same purpose on a runner.

The `aarch64` binary is confirmed working on-device under Termux (Android, 2026-08-29): UI, host stats and live session data all functional.

Requires `tmux`, `claude` and `git` on `PATH`; the agent reports any that are missing at startup.

## Configuration

| Variable | Default | Meaning |
|---|---|---|
| `PORT` | `23274` | Port to listen on. `0` picks any free port. The default is CDASH on a phone keypad, picked to collide with nothing: below the 32768 ephemeral range so the kernel never hands it out, and clear of 3000/5000/8000/8080/8888. |
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
`127.0.0.1:23274` gets code execution as *you*, which is a real escalation rather
than a no-op. `bearer` does not substitute here: browsers do not send
`Authorization` headers.

The JWKS fetch is gated on `cf-access` being in the chain, not on the two
`CDASH_CF_*` variables being set, so leaving them in a unit file after switching
to `none` does not couple boot to Cloudflare's availability.
