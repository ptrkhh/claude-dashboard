import express from 'express';
import os from 'node:os';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { collectSessions, logBuffer, log, launchSession, resumeSession, assertValidSid } from './lib/collect.js';
import { listDirs } from './lib/browse.js';
import { readPlaces, addRecent, toggleFavorite } from './lib/places.js';
import { execFile } from 'node:child_process';
import { promisify } from 'node:util';
const run = promisify(execFile);
const __dirname = path.dirname(fileURLToPath(import.meta.url));

const app = express();
app.use(express.json());
app.use(express.static(path.join(__dirname, 'public')));

app.get('/api/health', (_req, res) => res.json({ ok: true }));

const claudeDir = process.env.CLAUDE_DIR || path.join(os.homedir(), '.claude');
const ctx = {
  claudeDir,
  diskExtra: process.env.DISK_EXTRA || null, // e.g. /mnt/d — second df mount
  placesFile: path.join(claudeDir, 'cdash-places.json'), // recents + favorites
  meta: new Map(),
  purged: new Set(),
};

const assertPath = p => {
  if (typeof p !== 'string' || !path.isAbsolute(p)) throw Object.assign(new Error(`bad path: ${p}`), { status: 400 });
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

app.get('/api/browse', handle(async req => {
  try { return await listDirs(req.query.path || os.homedir(), { showHidden: req.query.hidden === '1' }); }
  catch (e) {
    const msg = { EACCES: 'Permission denied', ENOENT: 'No such folder', ENOTDIR: 'Not a folder' }[e.code] || e.message;
    throw Object.assign(new Error(msg), { status: 400 });
  }
}));
app.get('/api/places', handle(async () => readPlaces(ctx.placesFile)));
app.post('/api/favorites', handle(async req => { assertPath(req.body.path); return toggleFavorite(ctx.placesFile, req.body.path); }));

app.post('/api/launch', handle(async req => {
  const name = await launchSession(ctx, req.body);
  if (req.body?.dir) addRecent(ctx.placesFile, path.resolve(req.body.dir)).catch(e => log(`recent write failed: ${e.message}`));
  return { name };
}));
app.post('/api/resume', handle(async req => ({ name: await resumeSession(ctx, req.body.sid) })));
app.post('/api/kill', handle(async req => {
  const { name } = req.body;
  if (!/^cdash-[\w-]+$/.test(name || '')) throw Object.assign(new Error('bad name'), { status: 400 });
  await run('tmux', ['kill-session', '-t', name]);
  ctx.meta.delete(name);
  log(`kill ${name}`);
  return { ok: true };
}));
app.post('/api/purge', handle(async req => { assertValidSid(req.body.sid); ctx.purged.add(req.body.sid); return { ok: true }; }));

const port = process.env.PORT || 8080;
app.listen(port, () => console.log(`claude-dashboard on http://localhost:${port}`));
