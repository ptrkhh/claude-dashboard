import test from 'node:test';
import assert from 'node:assert/strict';
import { procTreeUsage, parseDf } from '../lib/stats.js';

test('procTreeUsage sums the tree rooted at pid', () => {
  const ps = [
    '   1     0  0.0  1000',
    ' 100     1  5.0 50000',   // root
    ' 200   100 10.0 20000',   // child
    ' 300   200  1.5  4000',   // grandchild
    ' 400     1  9.9 99999',   // unrelated
  ].join('\n');
  const u = procTreeUsage(ps, 100);
  assert.equal(u.cpu, 16.5);
  assert.equal(u.rssKb, 74000);
});

test('procTreeUsage unknown pid → zeros', () => {
  assert.deepEqual(procTreeUsage('1 0 0.0 100', 999), { cpu: 0, rssKb: 0 });
});

test('parseDf skips header, parses numbers', () => {
  const out = 'Mounted on   Avail  1K-blocks\n/            41000000 100000000\n/mnt/d        8000000  50000000\n';
  assert.deepEqual(parseDf(out), [
    { mount: '/', freeKb: 41000000, totalKb: 100000000 },
    { mount: '/mnt/d', freeKb: 8000000, totalKb: 50000000 },
  ]);
});
