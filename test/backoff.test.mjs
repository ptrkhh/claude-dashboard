import { test } from 'node:test';
import assert from 'node:assert/strict';

await import('../public/transport/backoff.js');
const b = globalThis.cdashBackoff;

test('a fresh state polls every 4 seconds', () => {
  const s = b.initial();
  assert.deepEqual(s, { i: 0, halted: false });
  assert.equal(b.delay(s), 4000);
});

test('failure climbs the ladder 8, 15, 30 and caps at 30', () => {
  let s = b.initial();
  s = b.next(s, 'fail'); assert.equal(b.delay(s), 8000);
  s = b.next(s, 'fail'); assert.equal(b.delay(s), 15000);
  s = b.next(s, 'fail'); assert.equal(b.delay(s), 30000);
  s = b.next(s, 'fail'); assert.equal(b.delay(s), 30000);
  s = b.next(s, 'fail'); assert.equal(b.delay(s), 30000);
});

test('success resets to 4 seconds', () => {
  let s = b.initial();
  s = b.next(s, 'fail');
  s = b.next(s, 'fail');
  s = b.next(s, 'ok');
  assert.equal(b.delay(s), 4000);
  assert.equal(s.halted, false);
});

test('an auth failure halts and stays halted', () => {
  let s = b.next(b.initial(), 'auth');
  assert.equal(s.halted, true);
  s = b.next(s, 'fail');
  assert.equal(s.halted, true);
});

test('next never mutates the state it is given', () => {
  const s = b.initial();
  b.next(s, 'fail');
  assert.equal(b.delay(s), 4000);
});
