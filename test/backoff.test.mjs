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

test('success clears a halt — the safety valve', () => {
  const s = b.next({ i: 0, halted: true }, 'ok');
  assert.deepEqual(s, { i: 0, halted: false });
});

test('next never mutates the state it is given', () => {
  const s = b.initial();
  b.next(s, 'fail');
  assert.equal(b.delay(s), 4000);
});

test('only 401 and 403 halt; throttling and transport failures back off', () => {
  // The rule this pins: a 429 that halted the poll would need a user action to
  // clear, turning a transient rate limit into a dead dashboard.
  assert.equal(b.outcomeFor(401), 'auth');
  assert.equal(b.outcomeFor(403), 'auth');
  for (const s of [429, 500, 502, 503, 400, 404, undefined]) {
    assert.equal(b.outcomeFor(s), 'fail', `${s} must not halt the poll`);
  }
  assert.equal(b.next(b.initial(), b.outcomeFor(429)).halted, false);
  assert.equal(b.next(b.initial(), b.outcomeFor(401)).halted, true);
});
