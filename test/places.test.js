import test from 'node:test';
import assert from 'node:assert/strict';
import os from 'node:os';
import fs from 'node:fs/promises';
import path from 'node:path';
import { pushRecent, toggleIn, readPlaces, addRecent, toggleFavorite, MAX_RECENTS } from '../lib/places.js';

test('pushRecent moves an existing entry to the front without duplicating', () => {
  assert.deepEqual(pushRecent(['/a', '/b', '/c'], '/c'), ['/c', '/a', '/b']);
  assert.deepEqual(pushRecent(['/a', '/b'], '/x'), ['/x', '/a', '/b']);
});

test('pushRecent caps the list length', () => {
  const many = Array.from({ length: MAX_RECENTS }, (_, i) => `/p${i}`);
  const out = pushRecent(many, '/new');
  assert.equal(out.length, MAX_RECENTS);
  assert.equal(out[0], '/new');
  assert.ok(!out.includes(`/p${MAX_RECENTS - 1}`)); // oldest dropped
});

test('toggleIn adds then removes', () => {
  assert.deepEqual(toggleIn([], '/a'), ['/a']);
  assert.deepEqual(toggleIn(['/a', '/b'], '/a'), ['/b']);
});

test('readPlaces returns empty shape for a missing file', async () => {
  assert.deepEqual(await readPlaces('/definitely/not/here.json'), { recents: [], favorites: [] });
});

test('addRecent and toggleFavorite persist to disk', async () => {
  const dir = await fs.mkdtemp(path.join(os.tmpdir(), 'cdash-places-'));
  const file = path.join(dir, 'places.json');
  try {
    await addRecent(file, '/home/x/one');
    await addRecent(file, '/home/x/two');
    let p = await readPlaces(file);
    assert.deepEqual(p.recents, ['/home/x/two', '/home/x/one']);

    await toggleFavorite(file, '/home/x/one');
    p = await readPlaces(file);
    assert.deepEqual(p.favorites, ['/home/x/one']);

    await toggleFavorite(file, '/home/x/one'); // toggle off
    p = await readPlaces(file);
    assert.deepEqual(p.favorites, []);
  } finally {
    await fs.rm(dir, { recursive: true, force: true });
  }
});
