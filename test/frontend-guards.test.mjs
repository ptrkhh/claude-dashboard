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
    document: { activeElement: null, querySelectorAll: () => [] },
    esc: s => s, fmtKb: String, fmtReset: String,
    runningCard: () => '', resumableCard: () => '',
    resLimit: 20, lastRes: [], lastPinned: [],
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
  const code = app.slice(app.indexOf('const copyIdBtn ='), app.indexOf('let lastRes ='));
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

test('cards offer the session ID for copying once one exists', () => {
  const app = readFileSync(new URL('../public/app.js', import.meta.url), 'utf8');
  const code = app.slice(app.indexOf('const copyIdBtn ='), app.indexOf('let lastRes ='));
  const context = { esc: s => String(s || ''), ICONS: {}, gitBadge: () => '', fmtUp: () => '', armedKill: null };
  runInNewContext(`${code}; this.card = runningCard;`, context);
  const session = { name: 'cdash-test', dir: '/tmp/project' };
  assert.doesNotMatch(context.card(session), /data-copy=/, 'no sid yet, no button');
  assert.match(context.card({ ...session, sid: '3c7254a1-54ff' }), /data-copy="3c7254a1-54ff"/);
  assert.match(context.card({ ...session, sid: 'ext', external: true }), /data-copy="ext"/);
});

test('copy reports success and shows the ID when the clipboard APIs fail', async () => {
  const app = readFileSync(new URL('../public/app.js', import.meta.url), 'utf8');
  const code = app.slice(app.indexOf('async function copyText('), app.indexOf('function toast('));
  const run = async (write, exec) => {
    let toastMsg;
    const field = { style: {}, select() {}, remove() {} };
    const context = {
      navigator: { clipboard: { writeText: write } },
      document: { createElement: () => field, body: { append() {} }, execCommand: () => exec },
      toast: msg => { toastMsg = msg; },
    };
    runInNewContext(`${code}; this.copy = copyText;`, context);
    await context.copy('sid-123');
    return toastMsg;
  };
  assert.equal(await run(async () => {}, false), 'Session ID copied');
  assert.equal(await run(async () => { throw Error('denied'); }, true), 'Session ID copied');
  assert.equal(await run(async () => { throw Error('denied'); }, false), 'Session ID: sid-123');
});

test('external sessions without a link explain how to enable Remote Control instead of waiting forever', () => {
  const app = readFileSync(new URL('../public/app.js', import.meta.url), 'utf8');
  const code = app.slice(app.indexOf('const copyIdBtn ='), app.indexOf('let lastRes ='));
  const context = {
    esc: s => String(s || ''), ICONS: { spinner: '<svg class="spinner"></svg>' },
    gitBadge: () => '', fmtUp: () => '', armedKill: null,
  };
  runInNewContext(`${code}; this.card = runningCard;`, context);
  const session = { name: 'external-test', dir: '/tmp/project', external: true, rcLink: null };
  for (const uptimeSec of [0, 180, 8940]) {
    const card = context.card({ ...session, uptimeSec });
    assert.match(card, /\/remote-control/);
    assert.match(card, /original terminal/);
    assert.doesNotMatch(card, /Waiting for link|class="spinner"|data-term=|data-kill=/);
  }
  const linked = context.card({ ...session, rcLink: 'https://claude.ai/code/session_external' });
  assert.match(linked, /href="https:\/\/claude.ai\/code\/session_external"[^>]*>.*Open in Claude/);
  assert.doesNotMatch(linked, /\/remote-control|Waiting for link/);
  assert.match(context.card({ ...session, external: false }), /Waiting for link/);
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
    ICONS: { refresh: '', trash: '', pin: '' }, copyIdBtn: sid => `data-copy="${sid}"`,
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
  assert.match(summary, /data-copy="one"/);
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
    $: id => id === '#pinned' ? { querySelectorAll: () => [] } : container,
    lastRes: [{ sid: 'one' }, { sid: 'two' }], lastPinned: [], resLimit: 20,
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
  const code = source.slice(source.indexOf('function latestModels('), source.indexOf('function toast('));
  const dir = { value: '/a' }, calls = [], pending = [];
  const context = {
    TERMINAL: { value: 'terminal', label: 'Terminal' }, MODELS: [{ value: 'sonnet', label: 'Claude' }, { value: 'opus', label: 'Claude Opus' }], EFFORTS: ['medium'], effortLabel: String, gen: 1,
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
    TERMINAL: { value: 'terminal', label: 'Terminal' }, EFFORTS: ['high'], effortLabel: String, effortModel: 'sonnet', $: () => control };
  runInNewContext(`${code}; updateEfforts();`, context);
  assert.equal(captured[0].label, 'Variant');
  assert.equal(captured[1].reset, true, 'an old effort must not silently become an OpenCode variant');
  assert.equal(captured[1].items[0].value, '');
  assert.equal(control.hidden, false);
  modelDD.value = 'terminal';
  runInNewContext('updateEfforts();', context);
  assert.equal(control.hidden, true, 'a bare terminal takes no effort');
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
  const code = source.slice(source.indexOf('function latestModels('), source.indexOf('function toast('));
  let now = 1000;
  const pending = [], dropdowns = [], dir = { value: '/project' };
  const context = {
    TERMINAL: { value: 'terminal', label: 'Terminal' }, MODELS: [{ value: 'sonnet', label: 'Claude' }], EFFORTS: ['medium'], effortLabel: String, gen: 1,
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
    TERMINAL: { value: 'terminal', label: 'Terminal' }, EFFORTS: ['high'], effortLabel: String, effortModel: 'sonnet', $: () => ({}) };
  runInNewContext(`${code}; updateEfforts();`, context);
  assert.equal(reset, false);
});

test('inactive cards offer pin and unpin without changing Resume', () => {
  const src = readFileSync(new URL('../public/app.js', import.meta.url), 'utf8');
  const card = src.slice(src.indexOf('function resumableCard('), src.indexOf('function renderResumable('));
  const context = { esc: String, fmtTs: () => '', gitBadge: () => '', pickHtml: () => '', ICONS: {}, copyIdBtn: () => '' };
  runInNewContext(`${card}; this.card = resumableCard;`, context);
  const s = { sid: 'one', prompts: [] };
  assert.match(context.card(s), /data-pin="one"[^>]*aria-pressed="false"[^>]*>.*Pin/s);
  assert.match(context.card(s, true), /data-pin="one"[^>]*aria-pressed="true"[^>]*>.*Unpin/s);
  assert.match(context.card(s, true), /data-resume="one"/);
});

test('pinned summaries retain expansion and keyboard focus during polling', () => {
  const src = readFileSync(new URL('../public/app.js', import.meta.url), 'utf8');
  const render = src.slice(src.indexOf('function renderResumable('), src.indexOf('function render(d) {'));
  let replaced = false, focused = false;
  const detail = { dataset: { summary: 'pinned' }, open: false,
    querySelector: () => ({ focus: () => { focused = true; } }) };
  const pinned = { querySelectorAll: () => replaced ? [detail] : [{ dataset: { summary: 'pinned' } }],
    set innerHTML(value) { this.html = value; replaced = true; } };
  const resumable = { querySelectorAll: () => [] };
  const context = {
    $: id => id === '#pinned' ? pinned : resumable,
    lastPinned: [{ sid: 'pinned' }], lastRes: [{ sid: 'history' }], resLimit: 1,
    resumableCard: (s, pin) => `${s.sid}:${pin}`,
    document: { activeElement: { closest: () => detail } },
  };
  runInNewContext(`${render}; renderResumable();`, context);
  assert.equal(detail.open, true);
  assert.equal(focused, true);
  assert.match(pinned.html, /pinned:true/);
  assert.doesNotMatch(pinned.html, /data-more|history/);
  assert.match(resumable.innerHTML, /history:false.*data-more/);
});

test('a newly running conversation leaves inactive sections without disturbing another picker', () => {
  const src = readFileSync(new URL('../public/app.js', import.meta.url), 'utf8');
  const code = src.slice(src.indexOf('function render(d) {'), src.indexOf("document.body.addEventListener('click'", src.indexOf('function render(d) {')));
  let painted = false;
  const nodes = {};
  const context = {
    $: id => nodes[id] ||= {},
    document: { activeElement: { closest: () => ({ dataset: { sid: 'two' } }) },
      querySelectorAll: () => [{ dataset: { resume: 'one' }, closest: () => card }] },
    lastPinned: [{ sid: 'one' }], lastRes: [],
    esc: String, fmtKb: String, runningCard: () => '',
    statTile: () => '', renderResumable: () => { painted = true; },
  };
  let removed = false;
  const card = { parentElement: { id: 'pinned', querySelector: () => null }, remove: () => { removed = true; } };
  runInNewContext(`${code}; this.render = render;`, context);
  context.render({ running: [{ sid: 'one' }], pinned: [], resumable: [],
    stats: { cpuPct: 1, ramUsedKb: 1, ramTotalKb: 2, disks: [] } });
  assert.equal(removed, true, 'a stale pinned card must not remain beside its running card');
  assert.equal(painted, false, 'the unrelated open picker keeps its DOM and focus');
});

test('summary expansion follows a conversation when it moves between inactive sections', () => {
  const src = readFileSync(new URL('../public/app.js', import.meta.url), 'utf8');
  const render = src.slice(src.indexOf('function renderResumable('), src.indexOf('function render(d) {'));
  for (const destination of ['#pinned', '#resumable']) {
    const detail = { dataset: { summary: 'one' }, open: false };
    const nodes = Object.fromEntries(['#pinned', '#resumable'].map(id => {
      let replaced = false;
      return [id, { querySelectorAll: () => replaced ? (id === destination ? [detail] : [])
        : (id !== destination ? [{ dataset: { summary: 'one' } }] : []),
        set innerHTML(_) { replaced = true; } }];
    }));
    runInNewContext(`${render}; renderResumable();`, {
      $: id => nodes[id], document: { activeElement: null },
      lastPinned: destination === '#pinned' ? [{ sid: 'one' }] : [],
      lastRes: destination === '#resumable' ? [{ sid: 'one' }] : [],
      resLimit: 20, resumableCard: () => '',
    });
    assert.equal(detail.open, true, `summary stays expanded after moving to ${destination}`);
  }
});

test('pin keeps keyboard focus unless the user focuses something else while saving', async () => {
  const src = readFileSync(new URL('../public/app.js', import.meta.url), 'utf8');
  const start = src.indexOf("document.body.addEventListener('click'");
  const handler = src.slice(start, src.indexOf('// Resume-card dropdowns:', start));
  for (const moveFocus of [false, true]) {
    const body = {};
    const other = {};
    const document = { body, activeElement: null };
    const el = {
      dataset: { pin: 'one' }, getAttribute: () => 'false',
      set disabled(value) { if (value && document.activeElement === el) document.activeElement = body; },
      focus: () => { document.activeElement = el; },
    };
    document.activeElement = el;
    let click, polledFocus;
    body.addEventListener = (_, callback) => { click = callback; };
    runInNewContext(handler, { document,
      api: async (path, request) => {
        assert.equal(path, '/api/pin');
        assert.equal(request.sid, 'one');
        assert.equal(request.pinned, true);
        if (moveFocus) document.activeElement = other;
      },
      poll: () => { polledFocus = document.activeElement; },
      toast: message => { assert.fail(message); },
    });
    await click({ target: { closest: selector => selector === '[data-term]' || selector === '[data-copy]' ? null : el } });
    assert.equal(polledFocus, moveFocus ? other : el);
  }});
test('chat scripts load before app.js and the chat markup exists', () => {
  const html = readFileSync(new URL('../public/index.html', import.meta.url), 'utf8');
  const at = s => html.indexOf(s);
  for (const f of ['chat/md.js', 'chat/store.js', 'chat/view.js']) {
    assert.ok(at(`<script src="${f}"></script>`) > 0 && at(`<script src="${f}"></script>`) < at('<script src="app.js"></script>'), f);
  }
  for (const id of ['chat', 'chat-log', 'chat-held', 'chat-form', 'chat-input', 'chat-go', 'chat-banner', 'chat-jump', 'chat-sheet'])
    assert.ok(html.includes(`id="${id}"`), id);
  assert.ok(/id="chat"[^>]*\shidden/.test(html), 'chat starts hidden');
});
test('the chat view never builds markup from conversation text', () => {
  const view = readFileSync(new URL('../public/chat/view.js', import.meta.url), 'utf8');
  const uses = view.match(/\.innerHTML\s*=[^;]*;/g) || [];
  assert.ok(uses.length > 0 && uses.every(u => /cdashMd\.render\(|=\s*''/.test(u)), uses.join('\n'));
  assert.ok(!/insertAdjacentHTML|document\.write|eval\(/.test(view));
});
