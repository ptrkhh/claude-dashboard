// A WebView with DOM storage disabled throws on *reading* window.localStorage,
// not just on get/set. One unguarded touch at the top of app.js took the whole
// file down and shipped a dead shell stuck on "Connecting…", twice. The rule
// that prevents it: nothing reaches localStorage except the `store` wrapper.
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';

test('app.js is wrapped so a second evaluation cannot throw', () => {
  // The page evaluated app.js twice on Android. Top-level `const`s are
  // instantiated before any statement runs, so the re-run died at parse time
  // with "already been declared" — and that masked whatever broke the first
  // run. Inside a function scope, with a boot claimed once, a re-run is inert.
  const src = readFileSync(new URL('../public/app.js', import.meta.url), 'utf8');
  const body = src.replace(/^\s*\/\*[\s\S]*?\*\//, '').trimStart();
  assert.ok(body.startsWith('(() => {'), 'app.js opens with an IIFE');
  assert.ok(src.trimEnd().endsWith('})();'), 'and closes it');
  assert.ok(src.includes('window.__cdashBooted'), 'and claims the boot once');
});

test('localStorage is reached only through the guarded store wrapper', () => {
  const src = readFileSync(new URL('../public/app.js', import.meta.url), 'utf8');
  const start = src.indexOf('const store = {');
  assert.ok(start > 0, 'app.js still defines the store wrapper');
  const wrapper = src.slice(start, src.indexOf('};', start));

  // Comments discuss localStorage on purpose; only code counts.
  const code = t => t.replace(/\/\*[\s\S]*?\*\//g, '').replace(/^\s*\/\/.*$/gm, '');
  const outside = code(src.slice(0, start) + src.slice(start + wrapper.length));
  const stray = outside.split('\n').filter(l => l.includes('localStorage'));
  assert.deepEqual(stray, [], 'use store.get/set/remove, not localStorage directly');

  // and the wrapper itself must actually catch
  for (const method of ['get(', 'set(', 'remove(']) {
    const i = wrapper.indexOf(method);
    assert.ok(i > 0, `wrapper has ${method}`);
    assert.ok(wrapper.slice(i, wrapper.indexOf('\n', i)).includes('catch'),
      `${method} swallows the throw`);
  }
});

test('the send bar is gone: the terminal replaced it', () => {
  // It typed one line into the pane and could not operate anything that
  // needs arrow keys (/mcp menus), which is why the terminal replaced it.
  const app = readFileSync(new URL('../public/app.js', import.meta.url), 'utf8');
  const css = readFileSync(new URL('../public/style.css', import.meta.url), 'utf8');
  assert.ok(!app.includes('/api/keys'), 'app.js no longer posts to /api/keys');
  assert.ok(!app.includes('send-input'), 'no send input is rendered');
  assert.ok(!/\.send[\s{.:-]/.test(css), 'no .send styles remain');
});

test('the Terminal button exists only for tmux-owned sessions', () => {
  // External sessions live in another terminal app's PTY; nothing can attach.
  const app = readFileSync(new URL('../public/app.js', import.meta.url), 'utf8');
  const m = app.match(/const termBtn = r\.external \? '' *\n?\s*: `<button[^`]*data-term=/);
  assert.ok(m, 'termBtn is empty for external sessions and carries data-term otherwise');
});

test('terminal scripts load before app.js', () => {
  const html = readFileSync(new URL('../public/index.html', import.meta.url), 'utf8');
  const at = s => html.indexOf(`<script src="${s}"></script>`);
  for (const s of ['vendor/xterm.js', 'vendor/addon-fit.js', 'term/keys.js']) {
    assert.ok(at(s) !== -1 && at(s) < at('app.js'), `${s} loads before app.js`);
  }
  assert.ok(html.includes('interactive-widget=resizes-content'), 'the IME shrinks the layout');
});

test('openTerm resets the Ctrl highlight after creating the session state', () => {
  // The key bar is shared DOM: a Ctrl left armed by the previous session would
  // look armed while term.ctrl is false, so the next key goes out unmodified.
  const app = readFileSync(new URL('../public/app.js', import.meta.url), 'utf8');
  const body = app.slice(app.indexOf('function openTerm'), app.indexOf('function closeTerm'));
  // Directly after the assignment: later setCtrl(false) calls (onData) don't count.
  assert.ok(/\n  term = \{[^\n]*\n  setCtrl\(false\);/.test(body), 'setCtrl(false) follows term = {');
});
