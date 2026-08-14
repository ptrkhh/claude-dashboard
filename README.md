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
