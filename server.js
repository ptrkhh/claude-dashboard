import express from 'express';
import os from 'node:os';
import path from 'node:path';
import { collectSessions, logBuffer, log, launchSession, resumeSession } from './lib/collect.js';
import { execFile } from 'node:child_process';
import { promisify } from 'node:util';
const run = promisify(execFile);

const app = express();
app.use(express.json());
app.use(express.static('public'));

app.get('/api/health', (_req, res) => res.json({ ok: true }));

const ctx = {
  claudeDir: process.env.CLAUDE_DIR || path.join(os.homedir(), '.claude'),
  diskExtra: process.env.DISK_EXTRA || null, // e.g. /mnt/d — second df mount
  meta: new Map(),
  purged: new Set(),
};

app.get('/api/sessions', async (_req, res) => {
  try { res.json(await collectSessions(ctx)); }
  catch (e) { log(`error sessions: ${e.message}`); res.status(500).json({ error: e.message }); }
});

app.get('/api/logs', (_req, res) => res.json({ lines: logBuffer }));

const handle = fn => async (req, res) => {
  try { res.json(await fn(req)); }
  catch (e) { log(`error: ${e.message}`); res.status(e.status || 500).json({ error: e.message }); }
};

app.post('/api/launch', handle(async req => ({ name: await launchSession(ctx, req.body) })));
app.post('/api/resume', handle(async req => ({ name: await resumeSession(ctx, req.body.sid) })));
app.post('/api/kill', handle(async req => {
  const { name } = req.body;
  if (!/^cdash-[\w-]+$/.test(name || '')) throw Object.assign(new Error('bad name'), { status: 400 });
  await run('tmux', ['kill-session', '-t', name]);
  ctx.meta.delete(name);
  log(`kill ${name}`);
  return { ok: true };
}));
app.post('/api/purge', handle(async req => { ctx.purged.add(req.body.sid); return { ok: true }; }));

const port = process.env.PORT || 8080;
app.listen(port, () => console.log(`claude-dashboard on http://localhost:${port}`));
