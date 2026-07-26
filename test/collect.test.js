import test from 'node:test';
import assert from 'node:assert/strict';
import fs from 'node:fs/promises';
import os from 'node:os';
import path from 'node:path';
import { resumeSession, parseTranscriptCached } from '../lib/collect.js';

test('resumeSession rejects non-UUID sid before touching the shell', async () => {
  await assert.rejects(
    () => resumeSession({ claudeDir: '/nonexistent' }, 'not-a-uuid; rm -rf /'),
    e => e.status === 400
  );
  await assert.rejects(() => resumeSession({ claudeDir: '/nonexistent' }, undefined), e => e.status === 400);
});

test('parseTranscriptCached returns cached parse when mtime unchanged, reparses when it changes', async () => {
  const dir = await fs.mkdtemp(path.join(os.tmpdir(), 'cdash-transcript-'));
  const file = path.join(dir, 'x.jsonl');
  const mkMsg = txt => JSON.stringify({ type: 'assistant', message: { content: [{ type: 'text', text: txt }] } });

  await fs.writeFile(file, mkMsg('first') + '\n');
  const a = await parseTranscriptCached(file);
  assert.equal(a.lastAssistantText, 'first');

  // unchanged file (no write in between) → same mtimeMs → cache hit → identical (same object) result
  const b = await parseTranscriptCached(file);
  assert.equal(b, a, 'cache hit: unchanged mtime must return the memoized object, not reparse');

  // bump mtime forward and change content → must reparse and pick up new content
  await new Promise(r => setTimeout(r, 10));
  const future = new Date(Date.now() + 60_000);
  await fs.writeFile(file, mkMsg('second') + '\n');
  await fs.utimes(file, future, future);
  const c = await parseTranscriptCached(file);
  assert.equal(c.lastAssistantText, 'second', 'mtime changed: must reparse');
  assert.notEqual(c, a);

  await fs.rm(dir, { recursive: true, force: true });
});
