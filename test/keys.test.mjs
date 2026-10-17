import { test } from 'node:test';
import assert from 'node:assert/strict';

await import('../public/term/keys.js');
const k = globalThis.cdashKeys;

test('each key-bar button sends the bytes a terminal sends for it', () => {
  assert.deepEqual(k.KEYS, {
    esc: '\x1b', tab: '\t', stab: '\x1b[Z', pgup: '\x1b[5~', pgdn: '\x1b[6~',
    left: '\x1b[D', up: '\x1b[A', down: '\x1b[B', right: '\x1b[C',
    ctrlc: '\x03', enter: '\r',
  });
});

test('sticky Ctrl turns one character into its control code', () => {
  assert.equal(k.applyCtrl('a'), '\x01');
  assert.equal(k.applyCtrl('A'), '\x01');
  assert.equal(k.applyCtrl('o'), '\x0f'); // Ctrl+O: transcript mode
  assert.equal(k.applyCtrl('r'), '\x12');
  assert.equal(k.applyCtrl('@'), '\x00');
  assert.equal(k.applyCtrl('['), '\x1b');
  assert.equal(k.applyCtrl('_'), '\x1f');
});

test('sticky Ctrl passes through what has no control code', () => {
  assert.equal(k.applyCtrl('1'), '1');
  assert.equal(k.applyCtrl('/'), '/');
  assert.equal(k.applyCtrl('`'), '`');
  // An IME commits whole words at once; never mangle those.
  assert.equal(k.applyCtrl('hello'), 'hello');
  assert.equal(k.applyCtrl(''), '');
});
