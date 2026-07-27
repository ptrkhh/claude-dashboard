import test from 'node:test';
import assert from 'node:assert/strict';
import { parseUsage } from '../lib/usage.js';

test('parseUsage maps known buckets with labels and reset times', () => {
  const out = parseUsage({
    five_hour: { utilization: 66, resets_at: '2026-07-27T10:59:00Z' },
    seven_day: { utilization: 77, resets_at: '2026-07-29T04:59:00Z' },
  });
  assert.deepEqual(out, [
    { key: 'five_hour', short: 'Session', long: 'Current session', pct: 66, resetsAt: '2026-07-27T10:59:00Z' },
    { key: 'seven_day', short: 'Week', long: 'Current week (all models)', pct: 77, resetsAt: '2026-07-29T04:59:00Z' },
  ]);
});

test('parseUsage labels model-specific weekly buckets by model name', () => {
  const out = parseUsage({
    seven_day_fable: { utilization: 26, resets_at: '2026-07-29T05:00:00Z' },
    seven_day_opus: { utilization: 10, resets_at: null },
  });
  assert.deepEqual(out.map(u => [u.short, u.long, u.pct]), [
    ['Fable', 'Current week (Fable)', 26],
    ['Opus', 'Current week (Opus)', 10],
  ]);
});

test('parseUsage orders session first, then weekly-all, then model weeklies', () => {
  const out = parseUsage({
    seven_day_sonnet: { utilization: 5 },
    seven_day: { utilization: 50 },
    five_hour: { utilization: 20 },
  });
  assert.deepEqual(out.map(u => u.key), ['five_hour', 'seven_day', 'seven_day_sonnet']);
});

test('parseUsage clamps and rounds utilization to 0–100', () => {
  const out = parseUsage({
    five_hour: { utilization: 66.7 },
    seven_day: { utilization: 140 },
    seven_day_opus: { utilization: -3 },
  });
  assert.deepEqual(out.map(u => u.pct), [67, 100, 0]);
});

test('parseUsage ignores non-bucket fields and bad input', () => {
  assert.deepEqual(parseUsage(null), []);
  assert.deepEqual(parseUsage('nope'), []);
  assert.deepEqual(parseUsage({}), []);
  assert.deepEqual(
    parseUsage({ five_hour: { utilization: 10 }, note: 'hi', extra: { foo: 1 } }).map(u => u.key),
    ['five_hour'],
  );
});
