// A WebView with DOM storage disabled throws on *reading* window.localStorage,
// not just on get/set. One unguarded touch at the top of app.js took the whole
// file down and shipped a dead shell stuck on "Connecting…", twice. The rule
// that prevents it: nothing reaches localStorage except the `store` wrapper.
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { runInNewContext } from 'node:vm';

function statsRenderer() {
  const src = readFileSync(new URL('../public/app.js', import.meta.url), 'utf8');
  const statTile = src.slice(src.indexOf('function statTile('), src.indexOf('function runningCard('));
  const render = src.slice(src.indexOf('function render(d) {'), src.indexOf("document.body.addEventListener('click'", src.indexOf('function render(d) {')));
  const nodes = {};
  const context = {
    $: id => nodes[id] ||= {},
    document: { activeElement: null },
    esc: s => s, fmtKb: String, fmtReset: String,
    runningCard: () => '', resumableCard: () => '',
    resLimit: 20,
  };
  runInNewContext(`${statTile}\n${render}; this.render = render;`, context);
  return stats => {
    context.render({ running: [], resumable: [], stats: { cpuPct: 1, ramUsedKb: 1, ramTotalKb: 2, disks: [], ...stats } });
    return nodes['#stats'].innerHTML;
  };
}

test('battery tile shows percentage, meter and connection only with both readings', () => {
  const render = statsRenderer();
  const show = battery => render({ battery });
  for (const battery of [undefined, null, {}, { pct: 50 }, { pct: null, plugged: true }, { pct: -1, plugged: false }, { pct: 101, plugged: true }])
    assert.ok(!show(battery).includes('Battery'));
  assert.match(show({ pct: 100, plugged: true }), /Battery.*100.*Plugged in/);
  assert.match(show({ pct: 0, plugged: false }), /Battery.*0.*Unplugged/);
  for (const pct of [0, 50, 100]) {
    const tile = show({ pct, plugged: false }).split('Battery')[1];
    assert.match(tile, new RegExp(`class="meter-fill [^"]*" style="width:${pct}%"`));
  }
  assert.ok(!show(undefined).includes('Battery'), 'tile disappears if a later poll loses readings');
});

test('hardware and usage tiles stay in separate rows as optional readings disappear', () => {
  const show = statsRenderer();
  const stats = {
    disks: [{ mount: '/', freeKb: 50, totalKb: 100 }, { mount: '/data', freeKb: 25, totalKb: 100 }],
    battery: { pct: 50, plugged: true },
    claudeUsage: [{ short: '5h', long: 'Five hours', pct: 25 }],
    codexUsage: [{ short: 'Codex 5h', long: 'Codex five hours', pct: 75 }],
  };
  const rows = html => html.split('<div class="stats-row">').slice(1);
  const both = rows(show(stats));
  assert.equal(both.length, 2);
  assert.match(both[0], /CPU.*RAM.*\/.*\/data.*Battery/);
  assert.doesNotMatch(both[0], /Claude|Codex/);
  assert.match(both[1], /Claude 5h.*Codex 5h/);
  const claudeOnly = rows(show({ ...stats, battery: null, codexUsage: [] }));
  assert.equal(claudeOnly.length, 2);
  assert.doesNotMatch(claudeOnly[0], /Battery/);
  assert.match(claudeOnly[1], /Claude 5h/);
  assert.doesNotMatch(claudeOnly[1], /Codex/);
  assert.equal(rows(show({})).length, 1, 'no empty usage row');
});

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

test('resumable summary uses escaped, keyboard-expandable peek and retains prompt fallback', () => {
  const src = readFileSync(new URL('../public/app.js', import.meta.url), 'utf8');
  const card = src.slice(src.indexOf('function resumableCard('), src.indexOf('function renderResumable('));
  const context = {
    esc: s => String(s).replaceAll('&', '&amp;').replaceAll('<', '&lt;').replaceAll('>', '&gt;').replaceAll('"', '&quot;'),
    fmtTs: () => '', gitBadge: () => '', pickHtml: () => '', effortLabel: String,
    ICONS: { refresh: '', trash: '' },
  };
  runInNewContext(`${card}; this.card = resumableCard;`, context);
  const session = { sid: 'one', title: 'Title', prompts: ['Earlier prompt'] };
  const fallback = context.card(session);
  assert.match(fallback, /Earlier prompt/);
  assert.doesNotMatch(fallback, /<details/);
  const summary = context.card({ ...session, summary: '<script>alert("x")</script>' });
  assert.match(summary, /<details class="peek summary" data-summary="one"><summary class="peek-text">/);
  assert.match(summary, /&lt;script&gt;/);
  assert.doesNotMatch(summary, /<script>|Earlier prompt/);
  assert.match(summary, /data-resume="one"/);
});

test('polling preserves expanded summaries and keyboard focus by session ID', () => {
  const src = readFileSync(new URL('../public/app.js', import.meta.url), 'utf8');
  const render = src.slice(src.indexOf('function renderResumable('), src.indexOf('function render(d) {'));
  const before = [{ dataset: { summary: 'one' } }];
  let focused = false, replaced = false;
  const after = ['one', 'two'].map(sid => ({
    dataset: { summary: sid }, open: false,
    querySelector: () => ({ focus: options => { focused = options.preventScroll; } }),
  }));
  const container = {
    querySelectorAll: () => replaced ? after : before,
    set innerHTML(_) { replaced = true; },
  };
  const context = {
    $: () => container, lastRes: [{ sid: 'one' }, { sid: 'two' }], resLimit: 20,
    resumableCard: () => '', document: { activeElement: { closest: () => before[0] } },
  };
  runInNewContext(`${render}; renderResumable();`, context);
  assert.equal(after[0].open, true);
  assert.equal(after[1].open, false);
  assert.equal(focused, true);
});
