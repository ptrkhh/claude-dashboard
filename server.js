import express from 'express';
import os from 'node:os';
import path from 'node:path';
import { collectSessions, logBuffer, log } from './lib/collect.js';

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

const port = process.env.PORT || 8080;
app.listen(port, () => console.log(`claude-dashboard on http://localhost:${port}`));
