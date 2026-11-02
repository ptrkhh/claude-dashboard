import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { runInNewContext } from 'node:vm';

function terminalKeys() {
  const src = readFileSync(new URL('../public/app.js', import.meta.url), 'utf8');
  const open = src.slice(src.indexOf('function openTerm('), src.indexOf('function closeTerm('));
  let handler = () => true, selected = false;
  class Terminal {
    textarea = { setAttribute() {} };
    loadAddon() {} open() {} onData() {} onResize() {} focus() {}
    hasSelection() { return selected; }
    attachCustomKeyEventHandler(fn) { handler = fn; }
  }
  runInNewContext(`${open}\nopenTerm('test', 'Test');`, {
    term: null, Terminal,
    $: () => ({}), document: { documentElement: {} },
    getComputedStyle: () => ({ getPropertyValue: () => 'monospace' }),
    FitAddon: { FitAddon: class { fit() {} } },
    ResizeObserver: class { observe() {} },
    history: { pushState() {} }, setCtrl() {}, termConnect() {},
  });
  return (key, modifiers = {}, selection = false) => {
    selected = selection;
    return handler({ type: 'keydown', key, ctrlKey: false, metaKey: false, altKey: false, shiftKey: false, ...modifiers });
  };
}

test('paste shortcuts reach the browser instead of sending terminal control characters', () => {
  const key = terminalKeys();
  for (const modifiers of [{ ctrlKey: true }, { metaKey: true }, { ctrlKey: true, shiftKey: true }]) {
    assert.equal(key('v', modifiers), false);
    assert.equal(key('V', modifiers), false);
  }
});

test('copy shortcuts reach the browser only when terminal text is selected', () => {
  const key = terminalKeys();
  for (const modifiers of [{ ctrlKey: true }, { metaKey: true }, { ctrlKey: true, shiftKey: true }]) {
    assert.equal(key('c', modifiers, true), false);
    assert.equal(key('C', modifiers, true), false);
    assert.equal(key('c', modifiers), true, 'Ctrl+C without a selection still interrupts');
  }
});

test('typing and other terminal shortcuts keep xterm handling', () => {
  const key = terminalKeys();
  for (const letter of ['c', 'v']) {
    assert.equal(key(letter), true);
    assert.equal(key(letter, { altKey: true, ctrlKey: true }, true), true);
  }
  assert.equal(key('r', { ctrlKey: true }), true);
  assert.equal(key('Escape'), true);
});
