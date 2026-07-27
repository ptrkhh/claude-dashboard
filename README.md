# claude-dashboard

A small local web dashboard for launching, monitoring, resuming, and killing Claude Code sessions running in tmux, with basic system stats (CPU/mem/disk), your Claude subscription usage limits, and a live log panel. It's a vanilla Express + HTML/CSS/JS app with no build step and no authentication — sessions are launched with `--dangerously-skip-permissions`, so run it only on a trusted LAN or behind Cloudflare Access.

The stats strip mirrors the `claude /usage` limit bars: session and weekly usage percentages, each with its reset time on a line beneath the meter. These read the same OAuth-only `/api/oauth/usage` endpoint the CLI uses, so they only appear when you're signed in with a Claude subscription — API-key users see just CPU/RAM/disk.

The launcher has a touch-friendly folder picker (the folder button in the directory field) that browses the server's filesystem from `/`, with server-backed **Recents** (auto-recorded on launch) and **Favorites**. Since it can enumerate any directory, keep the "trusted LAN / behind Cloudflare Access" caveat above in mind. Recents and favorites persist to `$CLAUDE_DIR/cdash-places.json`.

## Run

```
npm install && npm start
```

## Environment variables

- `PORT` — port to listen on (default `8080`).
- `CLAUDE_DIR` — path to the Claude config/projects directory (default `~/.claude`). The subscription token for usage limits is read from `$CLAUDE_DIR/.credentials.json`.
- `DISK_EXTRA` — optional second mount path to include in disk stats (e.g. `/mnt/d`).
- `ANTHROPIC_BASE_URL` — API base for the usage-limits lookup (default `https://api.anthropic.com`).
