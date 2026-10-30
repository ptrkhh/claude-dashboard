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

test('the send bar is gone: the terminal replaced it', () => {
  // It typed one line into the pane and could not operate anything that
  // needs arrow keys (/mcp menus), which is why the terminal replaced it.
  const app = readFileSync(new URL('../public/app.js', import.meta.url), 'utf8');
  const css = readFileSync(new URL('../public/style.css', import.meta.url), 'utf8');
  assert.ok(!app.includes('/api/keys'), 'app.js no longer posts to /api/keys');
  assert.ok(!app.includes('send-input'), 'no send input is rendered');
  assert.ok(!/\.send[\s{.:-]/.test(css), 'no .send styles remain');
});

test('legacy Codex sessions do not offer unsupported terminal controls', () => {
  const app = readFileSync(new URL('../public/app.js', import.meta.url), 'utf8');
  const code = app.slice(app.indexOf('function runningCard('), app.indexOf('let lastRes ='));
  const context = { esc: s => String(s || ''), ICONS: {}, gitBadge: () => '', fmtUp: () => '', armedKill: null };
  runInNewContext(`${code}; this.card = runningCard;`, context);
  const session = { name: 'cdash-test', dir: '/tmp/project', provider: 'codex' };
  assert.doesNotMatch(context.card(session), /data-term=/);
  assert.match(context.card(session), /Terminal.*new or resumed sessions/);
  assert.match(context.card({ ...session, nativeTerminal: true }), /data-term=/);
  for (const rcUnavailable of [undefined, false]) {
    const card = context.card({ ...session, nativeTerminal: true, rcUnavailable });
    assert.match(card, /Waiting for link/);
    assert.doesNotMatch(card, /Claude link unavailable/);
  }
  const failed = { ...session, nativeTerminal: true, rcUnavailable: true };
  assert.match(context.card(failed), /Claude link unavailable/);
  assert.doesNotMatch(context.card(failed), /Waiting for link/);
  assert.match(context.card({ ...failed, rcLink: 'https://claude.ai/code/session_test' }), /Open in Claude/);
  assert.match(context.card({ ...session, provider: undefined }), /data-term=/);
  assert.doesNotMatch(context.card({ ...session, nativeTerminal: true, external: true }), /data-term=/);
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
  assert.doesNotMatch(summary, /<script>|Earlier prompt|class="desc"|Title/);
  const empty = context.card({ sid: 'empty', prompts: [] });
  assert.match(empty, /No conversation preview available/);
  assert.doesNotMatch(empty, /undefined|class="desc"/);
  const codex = context.card({ sid: 'codex', prompts: ['<img src=x onerror=alert(1)>', 'Add tests'] });
  assert.match(codex, /&lt;img src=x onerror=alert\(1\)&gt; · Add tests/);
  assert.doesNotMatch(codex, /<img/);
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

function openCodePicker() {
  const source = readFileSync(new URL('../public/app.js', import.meta.url), 'utf8');
  const code = source.slice(source.indexOf('function pickState('), source.indexOf('function resumableCard('));
  const context = {
    MODELS: [{ value: 'sonnet', label: 'Claude Sonnet' }], EFFORTS: ['low', 'medium', 'high'],
    modelChoices: [{ value: 'codex:gpt', label: 'Codex GPT', efforts: ['high'] }],
    modelCatalogs: new Map(), picks: {}, esc: value => String(value ?? ''), effortLabel: String,
    optionsHtml: (items, selected) => items.map(item => `<option value="${item.value}"${item.value === selected ? ' selected' : ''}>${item.label}</option>`).join(''),
  };
  runInNewContext(`${code}; this.state = pickState; this.html = pickHtml; this.payload = typeof resumePayload === 'function' ? resumePayload : undefined;`, context);
  return context;
}

test('OpenCode resume controls preserve saved variants and send only applicable fields', () => {
  const h = openCodePicker();
  const model = { value: 'opencode:fixture/model', label: 'OpenCode Fixture', variants: ['fast', 'deep'] };
  h.modelCatalogs.set('/project', { models: [model] });
  const session = { sid: 'ses_saved', dir: '/project', modelId: model.value, model: model.label, variant: 'deep' };
  assert.equal(h.state(session).isOpenCode, true);
  assert.match(h.html(session), /Variant/);
  assert.doesNotMatch(h.html(session), /Effort/);
  assert.equal(h.payload(session).variant, undefined, 'unchanged resume preserves the native variant');
  h.picks.ses_saved = { variant: 'fast' };
  assert.equal(h.payload(session).variant, 'fast');
  assert.equal('effort' in h.payload(session), false);
  h.picks.ses_saved.variant = '';
  assert.equal(h.payload(session).variant, '', 'the default is an explicit selection');
  const claude = { sid: 'uuid', modelId: 'sonnet', effort: 'high' };
  assert.equal(h.payload(claude).effort, 'high');
  assert.equal('variant' in h.payload(claude), false);
  h.modelCatalogs.set('/project', { models: [{ value: 'opencode:fixture/other', label: 'Other', variants: [] }] });
  h.picks.ses_saved = { model: 'opencode:fixture/other' };
  assert.doesNotMatch(h.html(session), /Effort|Variant/);
  assert.equal(h.payload(session).variant, '', 'changing models clears an unsupported old variant');
  h.picks.ses_saved = {};
  assert.equal(h.state(session).model, model.value, 'a missing saved model remains selectable');
  assert.equal(h.payload(session).variant, undefined, 'a missing saved variant remains unchanged');
});

test('project model loads do not let stale responses replace the active project or host', async () => {
  const source = readFileSync(new URL('../public/app.js', import.meta.url), 'utf8');
  const code = source.slice(source.indexOf('let modelChoices = MODELS;'), source.indexOf('function toast('));
  const dir = { value: '/a' }, calls = [], pending = [];
  const context = {
    MODELS: [{ value: 'sonnet', label: 'Claude' }, { value: 'opus', label: 'Claude Opus' }], EFFORTS: ['medium'], effortLabel: String, gen: 1,
    $: selector => selector === '#dir' ? dir : {},
    buildDropdown: (_el, _label, _items, initial) => ({ value: initial, setItems() {}, setLabel() {} }),
    api: path => { calls.push(path); return new Promise((resolve, reject) => pending.push({ resolve, reject })); },
    toast() {}, renderResumable() {},
  };
  runInNewContext(`${code}; this.refresh = refreshModels; this.catalogs = modelCatalogs; this.values = () => modelChoices.map(model => model.value);`, context);
  const first = context.refresh(1, '/a');
  dir.value = '/b';
  const second = context.refresh(1, '/b');
  assert.equal(calls.length, 2, 'a different project starts its own request');
  assert.match(calls[0], /dir=%2Fa/);
  assert.match(calls[1], /dir=%2Fb/);
  pending[1].resolve([{ value: 'codex:gpt', label: 'Codex', efforts: ['medium'] }, { value: 'opencode:b/model', label: 'B', variants: [] }]);
  await second;
  pending[0].resolve([{ value: 'opencode:a/model', label: 'A', variants: [] }]);
  await first;
  assert.ok(context.values().includes('opencode:b/model'));
  assert.ok(!context.values().includes('opencode:a/model'));
  dir.value = '/c';
  const oldHost = context.refresh(1, '/c');
  assert.equal(context.values().filter(value => value === 'opus').length, 1, 'project changes do not duplicate Claude models');
  assert.ok(context.values().includes('codex:gpt'), 'Codex choices survive a project change');
  context.gen = 2; dir.value = '/d';
  const newHost = context.refresh(2, '/d');
  pending[3].resolve([{ value: 'opencode:new/model', label: 'New', variants: [] }]);
  await newHost;
  pending[2].resolve([{ value: 'opencode:old/model', label: 'Old', variants: [] }]);
  await oldHost;
  assert.ok(context.values().includes('opencode:new/model'));
  assert.ok(!context.values().includes('opencode:old/model'));
  const cardCatalog = context.refresh(2, '/saved-project');
  assert.match(calls.at(-1), /dir=%2Fsaved-project/, 'a card loads its own project catalog');
  pending.at(-1).reject(new Error('discovery failed'));
  await cardCatalog;
  assert.ok(context.values().includes('opencode:new/model'), 'a different project failure does not replace launcher choices');
  assert.equal(context.catalogs.get('/saved-project').models, null);
});


test('changing from effort to an OpenCode model resets the secondary picker', () => {
  const source = readFileSync(new URL('../public/app.js', import.meta.url), 'utf8');
  const code = source.slice(source.indexOf('function updateEfforts() {'), source.indexOf('let modelsGeneration'));
  const captured = [], control = { hidden: false }, modelDD = { value: 'opencode:fixture/model' };
  const context = { modelChoices: [{ value: modelDD.value, variants: ['high', 'deep'] }], modelDD,
    effortDD: { setLabel: label => captured.push({ label }), setItems: (items, fallback, reset) => captured.push({ items, fallback, reset }) },
    EFFORTS: ['high'], effortLabel: String, effortModel: 'sonnet', $: () => control };
  runInNewContext(`${code}; updateEfforts();`, context);
  assert.equal(captured[0].label, 'Variant');
  assert.equal(captured[1].reset, true, 'an old effort must not silently become an OpenCode variant');
  assert.equal(captured[1].items[0].value, '');
  assert.equal(control.hidden, false);
});


test('opening an OpenCode card loads that card project', async () => {
  const source = readFileSync(new URL('../public/app.js', import.meta.url), 'utf8');
  const code = source.slice(source.indexOf('function ddOpen('), source.indexOf('function ddPick('));
  const calls = [], menu = { hidden: true, querySelector: () => null }, trigger = { setAttribute() {} };
  const dd = { dataset: { sid: 'ses_saved', pick: 'model' }, isConnected: true };
  const context = { lastRes: [{ sid: 'ses_saved', modelId: 'opencode:fixture/model', dir: '/saved' }], gen: 1,
    refreshModels: async (...args) => calls.push(args), ddMenu: () => menu, ddTrigger: () => trigger,
    ddActive() {}, document: { querySelectorAll: () => [] }, pickState: () => ({ models: [], model: '' }), optionsHtml: () => '' };
  runInNewContext(`${code}; this.open = ddOpen;`, context);
  context.open(dd);
  await Promise.resolve();
  assert.deepEqual(calls[0], [1, '/saved']);
  assert.equal(menu.hidden, false);
});

test('refreshing a project catalog preserves its selected OpenCode model', async () => {
  const source = readFileSync(new URL('../public/app.js', import.meta.url), 'utf8');
  const code = source.slice(source.indexOf('let modelChoices = MODELS;'), source.indexOf('function toast('));
  let now = 1000;
  const pending = [], dropdowns = [], dir = { value: '/project' };
  const context = {
    MODELS: [{ value: 'sonnet', label: 'Claude' }], EFFORTS: ['medium'], effortLabel: String, gen: 1,
    Date: { now: () => now }, $: selector => selector === '#dir' ? dir : {}, toast() {},
    buildDropdown: (_el, _label, _items, initial) => {
      const dd = { value: initial, setLabel() {}, setItems(items, fallback) { if (!items.some(item => item.value === this.value)) this.value = fallback; } };
      dropdowns.push(dd); return dd;
    },
    api: () => new Promise(resolve => pending.push(resolve)),
  };
  runInNewContext(`${code}; this.refresh = refreshModels;`, context);
  const first = context.refresh(1);
  const catalog = [{ value: 'opencode:fixture/model', variants: [] }];
  pending.shift()(catalog); await first;
  dropdowns[0].value = catalog[0].value;
  now += 61000;
  const second = context.refresh(1);
  assert.equal(dropdowns[0].value, catalog[0].value, 'a refresh must not silently change the model during loading');
  pending.shift()(catalog); await second;
  assert.equal(dropdowns[0].value, catalog[0].value);
});

test('changing between Claude models preserves a compatible effort', () => {
  const source = readFileSync(new URL('../public/app.js', import.meta.url), 'utf8');
  const code = source.slice(source.indexOf('function updateEfforts() {'), source.indexOf('let modelsGeneration'));
  let reset;
  const context = { modelChoices: [{ value: 'opus', efforts: ['high'] }], modelDD: { value: 'opus' },
    effortDD: { setLabel() {}, setItems(_items, _fallback, value) { reset = value; } },
    EFFORTS: ['high'], effortLabel: String, effortModel: 'sonnet', $: () => ({}) };
  runInNewContext(`${code}; updateEfforts();`, context);
  assert.equal(reset, false);
});
