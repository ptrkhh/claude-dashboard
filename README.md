# claude-dashboard

A small local web dashboard for launching, monitoring, resuming, and killing Claude Code sessions running in tmux, with basic system stats (CPU/mem/disk) and a live log panel. It's a vanilla Express + HTML/CSS/JS app with no build step and no authentication — sessions are launched with `--dangerously-skip-permissions`, so run it only on a trusted LAN or behind Cloudflare Access.

Each running session card has a **send bar** that types straight into that session's TUI via `tmux send-keys`. It exists because the Claude app's remote control can send prompts but can't type into the TUI, so a session that stops and asks you to run something interactively (`! gcloud auth login`) is a dead end from a phone. Anything you type is sent as one line followed by Enter, exactly as if you'd typed it at the terminal — including `!` shell commands, `/` commands, and plain answers to a question Claude asked. External sessions aren't in tmux, so they don't get the bar. Delete `lib/keys.js`, `POST /api/keys`, and the `.send` block in `public/` once remote control grows an input of its own.

The launcher has a touch-friendly folder picker (the folder button in the directory field) that browses the server's filesystem from `/`, with server-backed **Recents** (auto-recorded on launch) and **Favorites**. Since it can enumerate any directory, keep the "trusted LAN / behind Cloudflare Access" caveat above in mind. Recents and favorites persist to `$CLAUDE_DIR/cdash-places.json`.

## Run

```
npm install && npm start
```

## Environment variables

- `PORT` — port to listen on (default `8080`).
- `CLAUDE_DIR` — path to the Claude config/projects directory (default `~/.claude`).
- `DISK_EXTRA` — optional second mount path to include in disk stats (e.g. `/mnt/d`).
