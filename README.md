# claude-dashboard

A small local web dashboard for launching, monitoring, resuming, and killing Claude Code sessions running in tmux, with basic system stats (CPU/mem/disk) and a live log panel. The agent is a single Rust binary; the UI is vanilla HTML/CSS/JS with no build step. There is no authentication yet — sessions are launched with `--dangerously-skip-permissions`, so run it only on a trusted LAN or behind Cloudflare Access.

The launcher has a touch-friendly folder picker (the folder button in the directory field) that browses the server's filesystem from `/`, with server-backed **Recents** (auto-recorded on launch) and **Favorites**. Since it can enumerate any directory, keep the "trusted LAN / behind Cloudflare Access" caveat above in mind. Recents and favorites persist to `$CLAUDE_DIR/cdash-places.json`.

## Run

```
cargo run -p cdash-agent     # http://127.0.0.1:8080
```

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

### `cf-access` is partial

The Cloudflare Access JWT verifier is implemented and tested — signature, `aud`
array membership, `iss`, expiry, the `service_token_status` discriminator, the
`common_name` rejection, and `alg: none`. **The JWKS fetch is not wired**: the
agent links no HTTP client, and adding one is a deliberate dependency decision
rather than an implementation detail. Until a fetcher is supplied,
`CDASH_AUTH=cf-access` rejects every request — a guard that fails open is not a
guard.
