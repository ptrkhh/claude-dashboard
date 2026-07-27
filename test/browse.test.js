import test from 'node:test';
import assert from 'node:assert/strict';
import os from 'node:os';
import fs from 'node:fs/promises';
import path from 'node:path';
import { listDirs } from '../lib/browse.js';

async function fixture() {
  const root = await fs.mkdtemp(path.join(os.tmpdir(), 'cdash-browse-'));
  await fs.mkdir(path.join(root, 'alpha'));
  await fs.mkdir(path.join(root, 'Beta'));
  await fs.mkdir(path.join(root, '.hidden'));
  await fs.writeFile(path.join(root, 'a-file.txt'), 'x'); // must be excluded
  return root;
}

test('listDirs returns folders only, case-insensitively sorted, hidden excluded', async () => {
  const root = await fixture();
  try {
    const d = await listDirs(root);
    assert.deepEqual(d.entries.map(e => e.name), ['alpha', 'Beta']); // no file, no dotdir
    assert.equal(d.path, path.resolve(root));
    assert.equal(d.parent, path.dirname(root));
    assert.equal(d.entries[0].path, path.join(root, 'alpha'));
  } finally { await fs.rm(root, { recursive: true, force: true }); }
});

test('listDirs includes dotfolders when showHidden is set', async () => {
  const root = await fixture();
  try {
    const d = await listDirs(root, { showHidden: true });
    assert.deepEqual(d.entries.map(e => e.name), ['.hidden', 'alpha', 'Beta']);
  } finally { await fs.rm(root, { recursive: true, force: true }); }
});

test('listDirs reports null parent at the filesystem root', async () => {
  const d = await listDirs('/');
  assert.equal(d.parent, null);
  assert.equal(d.path, '/');
});

test('listDirs rejects a nonexistent path', async () => {
  await assert.rejects(() => listDirs('/no/such/dir/cdash-xyz'));
});
