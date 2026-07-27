# claude-dashboard

A small local web dashboard for launching, monitoring, resuming, and killing Claude Code sessions running in tmux, with basic system stats (CPU/mem/disk) and a live log panel. It's a vanilla Express + HTML/CSS/JS app with no build step and no authentication — sessions are launched with `--dangerously-skip-permissions`, so run it only on a trusted LAN or behind Cloudflare Access.

The launcher has a touch-friendly folder picker (the folder button in the directory field) that browses the server's filesystem from `/`, with server-backed **Recents** (auto-recorded on launch) and **Favorites**. Since it can enumerate any directory, keep the "trusted LAN / behind Cloudflare Access" caveat above in mind. Recents and favorites persist to `$CLAUDE_DIR/cdash-places.json`.

## Run

```
npm install && npm start
```

## Environment variables

- `PORT` — port to listen on (default `8080`).
- `CLAUDE_DIR` — path to the Claude config/projects directory (default `~/.claude`).
- `DISK_EXTRA` — optional second mount path to include in disk stats (e.g. `/mnt/d`).
