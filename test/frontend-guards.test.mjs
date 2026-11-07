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
    await click({ target: { closest: selector => selector === '[data-term]' || selector === '[data-browser]' || selector === '[data-copy]' ? null : el } });
    assert.equal(polledFocus, moveFocus ? other : el);
  }});

test('only sessions with a live browser get the Browser button', () => {
  const src = readFileSync(new URL('../public/app.js', import.meta.url), 'utf8');
  const card = src.slice(src.indexOf('function runningCard('), src.indexOf('\n}\n', src.indexOf('function runningCard(')) + 2);
  const context = { esc: s => s, effortLabel: s => s, gitBadge: () => '', copyIdBtn: () => '', fmtUp: () => '', armedKill: null, ICONS: new Proxy({}, { get: () => '' }) };
  runInNewContext(`${card}; this.runningCard = runningCard;`, context);
  const row = { name: 'cdash-a', dir: '/x/JOBS', working: false };
  assert.match(context.runningCard({ ...row, browser: true }), /data-browser="cdash-a"/);
  assert.doesNotMatch(context.runningCard(row), /data-browser/);
});

test('the browser typing field stays focusable and its scripts load before app.js', () => {
  const html = readFileSync(new URL('../public/index.html', import.meta.url), 'utf8');
  const css = readFileSync(new URL('../public/style.css', import.meta.url), 'utf8');
  const input = html.match(/<input id="browser-input"[^>]*>/)?.[0] ?? '';
  assert.match(input, /type="password"/);
  assert.match(input, /autocomplete="off"/);
  assert.match(input, /autocapitalize="off"/);
  assert.match(input, /spellcheck="false"/);
  assert.match(input, /aria-label="Remote browser typing"/);
  assert.doesNotMatch(input, /\bhidden\b/, 'a hidden input cannot take focus');
  const rule = css.match(/\.browser-input\s*\{[^}]*\}/)?.[0] ?? '';
  assert.ok(rule, 'the field is concealed by a .browser-input rule');
  assert.doesNotMatch(rule, /display\s*:\s*none|visibility\s*:\s*hidden/);
  assert.match(css, /\.browser \.term-keys\s*\{[^}]*repeat\(7,/);
  assert.match(html, /<div id="browser" class="term browser" role="dialog" aria-modal="true" aria-labelledby="browser-title" hidden>/);
  assert.ok(html.indexOf('browser/view.js') !== -1 && html.indexOf('browser/view.js') < html.indexOf('"app.js"'));
});

/* A fake DOM, WebSocket, Image and history around the real Terminal and Browser
   sections of app.js, so the two overlays run against each other. */
function overlayHarness({ protocol = 'http:', tauri = false } = {}) {
  const src = readFileSync(new URL('../public/app.js', import.meta.url), 'utf8');
  const start = src.indexOf('/* ---------- Terminal: ');
  const end = src.indexOf('let modelDirTimer;');
  assert.ok(start !== -1 && end > start, 'the Terminal and Browser sections are found');
  const region = src.slice(start, end);
  const listeners = {};
  const sockets = [], images = [], urls = [], revoked = [];
  const doc = { hidden: false, activeElement: null, documentElement: {}, addEventListener: (t, f) => (listeners[`doc:${t}`] ||= []).push(f) };
  function makeEl(tag) {
    const handlers = {};
    let text = '';
    const e = {
      tag, children: [], dataset: {}, style: {}, attrs: {}, hidden: false, disabled: false, value: '', title: '', htmlWrites: 0,
      rect: { left: 0, top: 0, width: 0, height: 0 }, clientWidth: 0, clientHeight: 0, offsetWidth: 0, offsetHeight: 0,
      classList: { toggle() {}, add() {}, remove() {} },
      addEventListener(t, f) { (handlers[t] ||= []).push(f); },
      dispatch(t, extra = {}) {
        const ev = { type: t, target: e, defaultPrevented: false, preventDefault() { this.defaultPrevented = true; }, ...extra };
        for (const f of handlers[t] || []) f(ev);
        return ev;
      },
      appendChild(c) { e.children.push(c); return c; },
      setAttribute(k, v) { e.attrs[k] = String(v); },
      removeAttribute(k) { delete e.attrs[k]; if (k === 'src') e.src = undefined; },
      focus() { doc.activeElement = e; },
      blur() { if (doc.activeElement === e) doc.activeElement = null; },
      closest: () => null,
      getBoundingClientRect: () => e.rect,
      setPointerCapture() {},
    };
    Object.defineProperty(e, 'textContent', { get: () => text, set: v => { text = String(v); if (text === '') e.children = []; } });
    Object.defineProperty(e, 'innerHTML', { get: () => '', set: () => { e.htmlWrites++; } });
    return e;
  }
  doc.createElement = makeEl;
  const els = {};
  const $ = sel => (els[sel] ||= makeEl('div'));
  class FakeWS {
    static OPEN = 1;
    constructor(url) { this.url = url; this.readyState = 0; this.sent = []; sockets.push(this); }
    send(d) { this.sent.push(d); }
    close() { this.readyState = 3; this.closedByClient = true; }
    open() { this.readyState = 1; this.onopen?.(); }
    recv(data) { this.onmessage?.({ data }); }
    serverClose(code, reason = '') { this.readyState = 3; this.onclose?.({ code, reason }); }
  }
  class FakeImage {
    constructor() { images.push(this); this.done = new Promise((res, rej) => { this.resolve = res; this.reject = rej; }); }
    decode() { return this.done; }
  }
  class FakeTerminal {
    cols = 80; rows = 24; textarea = { setAttribute() {} };
    loadAddon() {} open() {} onData() {} onResize() {} focus() {} reset() {} dispose() {} write() {}
    attachCustomKeyEventHandler() {} hasSelection() { return false; }
  }
  let polls = 0;
  const history = {
    entries: [], pushes: 0, replaces: 0,
    pushState(s) { this.pushes++; this.entries.push(s); },
    replaceState(s) { this.replaces++; this.entries[this.entries.length - 1] = s; },
    back() { this.entries.pop(); for (const f of listeners.popstate || []) f({}); },
  };
  const context = {
    $, document: doc, history, isTauri: tauri, invoke: async () => 'http://127.0.0.1:23274',
    location: { protocol, host: 'dash.test' }, poll: () => { polls++; },
    ICONS: new Proxy({}, { get: () => '' }),
    WebSocket: FakeWS, Image: FakeImage, Blob: class { constructor(parts, opts) { this.parts = parts; this.type = opts?.type; } },
    URL: { createObjectURL: () => { const u = `blob:${urls.length + 1}`; urls.push(u); return u; }, revokeObjectURL: u => revoked.push(u) },
    Terminal: FakeTerminal, FitAddon: { FitAddon: class { fit() {} } },
    ResizeObserver: class { observe() {} disconnect() {} },
    getComputedStyle: () => ({ getPropertyValue: () => 'monospace' }),
    TextEncoder, JSON, Map, Promise, setTimeout,
    addEventListener: (t, f) => (listeners[t] ||= []).push(f),
  };
  for (const file of ['term/keys.js', 'browser/view.js']) runInNewContext(readFileSync(new URL(`../public/${file}`, import.meta.url), 'utf8'), context);
  runInNewContext(`${region}\nthis.api = { openTerm, closeTerm, openBrowser, closeBrowser, isOpen: () => !!browserView, termOpen: () => !!term };`, context);
  const flush = () => new Promise(r => setImmediate(r));
  const live = () => sockets.filter(s => s.readyState === 0 || s.readyState === 1).length;
  const meta = (w, h) => JSON.stringify({ frame: { deviceWidth: w, deviceHeight: h, offsetTop: 0, pageScaleFactor: 1 } });
  /* Open the overlay, connect, list one tab and display one frame. */
  async function ready(w = 1000, h = 500) {
    context.api.openBrowser('cdash-a', 'JOBS');
    await flush();
    const ws = sockets.at(-1);
    ws.open();
    ws.recv(JSON.stringify({ tabs: [{ id: 'T1', title: 'A', origin: 'https://a.example' }], active: 'T1' }));
    ws.recv(meta(w, h));
    ws.recv(new ArrayBuffer(4));
    images.at(-1).resolve();
    await flush();
    return ws;
  }
  const sent = ws => ws.sent.map(s => JSON.parse(s));
  return { api: context.api, $, els, sockets, images, urls, revoked, history, doc, listeners, flush, live, meta, ready, sent, polls: () => polls };
}

test('composition_is_forwarded_once_on_commit', async () => {
  const h = overlayHarness();
  const ws = await h.ready();
  const input = h.$('#browser-input');
  assert.equal(input.disabled, false, 'a displayed frame pair enables typing');

  // Compose, commit, and a browser's trailing duplicate commit event.
  input.dispatch('compositionstart');
  input.value = 'に';
  input.dispatch('input', { inputType: 'insertCompositionText', data: 'に', isComposing: true });
  assert.equal(input.value, 'に', 'the composing text is not reset');
  input.dispatch('compositionend', { data: '日本' });
  input.dispatch('input', { inputType: 'insertCompositionText', data: '日本', isComposing: false });
  input.dispatch('input', { inputType: 'insertText', data: '日本' });
  assert.deepEqual(h.sent(ws), [{ text: '日本' }]);
  assert.notEqual(input.value, '日本', 'cleared after forwarding');

  // A canceled composition sends nothing and clears the field.
  input.dispatch('compositionstart');
  input.value = 'x';
  input.dispatch('input', { inputType: 'insertCompositionText', data: 'x', isComposing: true });
  input.dispatch('compositionend', { data: '' });
  assert.deepEqual(h.sent(ws), [{ text: '日本' }]);
  assert.notEqual(input.value, 'x');

  // After the duplicate window, the same text typed again is real input.
  await new Promise(r => setTimeout(r, 5));
  input.dispatch('input', { inputType: 'insertText', data: '日本' });
  assert.deepEqual(h.sent(ws).at(-1), { text: '日本' });

  // Enter is one key, prevented, and its line-break input is not a second one.
  const before = ws.sent.length;
  const enter = input.dispatch('keydown', { key: 'Enter', keyCode: 13, isComposing: false });
  input.dispatch('input', { inputType: 'insertLineBreak', data: null });
  assert.equal(enter.defaultPrevented, true);
  assert.deepEqual(h.sent(ws).slice(before), [{ key: 'Enter' }]);
  input.dispatch('keydown', { key: 'Enter', keyCode: 229, isComposing: true });
  assert.equal(ws.sent.length, before + 1, 'Enter inside a composition is not forwarded');
  await new Promise(r => setTimeout(r, 5));
  input.dispatch('input', { inputType: 'deleteContentBackward', data: null });
  assert.deepEqual(h.sent(ws).at(-1), { key: 'Backspace' });

  // Closing mid-composition clears the field and the composition.
  input.dispatch('compositionstart');
  input.value = 'に';
  h.api.closeBrowser();
  assert.equal(input.value, '');
  assert.equal(h.api.isOpen(), false);
});

test('tab_labels_are_text', async () => {
  const h = overlayHarness();
  h.api.openBrowser('cdash-a', 'JOBS');
  await h.flush();
  const ws = h.sockets.at(-1);
  ws.open();
  const evil = '<img src=x onerror=URL_CANARY>';
  ws.recv(JSON.stringify({ tabs: [
    { id: 'T1', title: evil, origin: 'https://a.example' },
    { id: 'T2', title: 'No origin', origin: null },
  ], active: 'T1' }));
  const sel = h.$('#browser-tabs');
  const [placeholder, one, two] = sel.children;
  assert.equal(sel.children.length, 3);
  assert.deepEqual([placeholder.textContent, placeholder.disabled], ['Pick a tab', true]);
  assert.deepEqual([one.tag, one.textContent, one.title, one.value], ['option', 'https://a.example', evil, 'T1']);
  assert.deepEqual([two.textContent, two.value], ['No web origin', 'T2']);
  for (const o of sel.children) assert.equal(o.htmlWrites, 0, 'labels are assigned as text, never as HTML');
  assert.equal(sel.value, 'T1');

  ws.recv(JSON.stringify({ tabs: [{ id: 'T2', title: 'No origin', origin: null }], active: null }));
  assert.equal(sel.value, '', 'the placeholder is selected');
  assert.equal(h.$('#browser-status').textContent, 'Tab closed. Pick a tab.');
  assert.equal(h.$('#browser-input').disabled, true);

  // Picking a tab asks the agent; input stays off until its frame pair is shown.
  sel.value = 'T2';
  sel.dispatch('change');
  assert.deepEqual(h.sent(ws), [{ tab: 'T2' }]);
});

test('frame_pairing_survives_switch_and_reconnect', async () => {
  const h = overlayHarness();
  h.api.openBrowser('cdash-a', 'JOBS');
  await h.flush();
  const ws = h.sockets.at(-1);
  ws.open();
  const list = active => JSON.stringify({ tabs: [{ id: 'T1', title: 'A', origin: 'https://a.example' }, { id: 'T2', title: 'B', origin: 'https://b.example' }], active });
  const input = h.$('#browser-input'), screen = h.$('#browser-screen');
  ws.recv(list('T1'));

  // Frame A is still decoding when the target switches; finishing it must not restore input.
  ws.recv(h.meta(800, 600));
  ws.recv(new ArrayBuffer(4));
  assert.equal(input.disabled, true, 'no pair is displayed yet');
  ws.recv(list('T2'));
  h.images[0].resolve();
  await h.flush();
  assert.equal(input.disabled, true, 'a stale decode never enables input');
  assert.equal(screen.src, undefined);
  assert.ok(h.revoked.includes('blob:1'), 'the stale frame URL is revoked');

  // Frame B has other dimensions; taps use B's metadata and the image rect, not the stage.
  ws.recv(h.meta(1000, 500));
  ws.recv(new ArrayBuffer(4));
  h.images[1].resolve();
  await h.flush();
  assert.equal(input.disabled, false);
  assert.equal(screen.src, 'blob:2');
  screen.rect = { left: 10, top: 20, width: 500, height: 250 };
  h.$('#browser-stage').rect = { left: 0, top: 0, width: 600, height: 900 };
  const stage = h.$('#browser-stage');
  const tap = (x, y) => { stage.dispatch('pointerdown', { pointerId: 1, clientX: x, clientY: y }); stage.dispatch('pointerup', { pointerId: 1, clientX: x, clientY: y }); };
  tap(260, 145);
  assert.deepEqual(h.sent(ws).at(-1), { tap: [500, 250] });
  const taps = () => h.sent(ws).filter(m => m.tap).length;
  tap(5, 145);
  tap(300, 400);
  assert.equal(taps(), 1, 'a tap outside the image (letterbox, stage padding) is dropped');

  // A new frame replaces the old one and revokes its URL.
  ws.recv(h.meta(1000, 500));
  ws.recv(new ArrayBuffer(4));
  h.images[2].resolve();
  await h.flush();
  assert.ok(h.revoked.includes('blob:2'));

  // Reconnect: the old socket's late frames are ignored, input waits for a new pair.
  ws.serverClose(1006);
  assert.equal(input.disabled, true);
  assert.equal(h.$('#browser-reconnect').hidden, false);
  assert.ok(h.revoked.includes('blob:3'), 'closing revokes the displayed frame URL');
  h.$('#browser-reconnect').dispatch('click');
  await h.flush();
  const ws2 = h.sockets.at(-1);
  assert.notEqual(ws2, ws);
  ws2.open();
  ws2.recv(list('T1'));
  ws.recv(h.meta(1000, 500));
  ws.recv(new ArrayBuffer(4));
  assert.equal(h.images.length, 3, 'a late frame on the old socket never starts a decode');
  ws2.recv(h.meta(400, 300));
  ws2.recv(new ArrayBuffer(4));
  assert.equal(input.disabled, true);
  h.images[3].resolve();
  await h.flush();
  assert.equal(input.disabled, false);
  tap(260, 145);
  assert.deepEqual(h.sent(ws2).at(-1), { tap: [200, 150] }, 'taps use the new pair, 400x300 over a 500x250 image');
  h.api.closeBrowser();
  assert.ok(h.revoked.includes('blob:4'));
});

test('browser_and_terminal_lifecycles_do_not_conflict', async () => {
  for (const [label, opts, base] of [
    ['http', {}, 'ws://dash.test'],
    ['https', { protocol: 'https:' }, 'wss://dash.test'],
    ['tauri', { tauri: true }, 'ws://127.0.0.1:23274'],
  ]) {
    const h = overlayHarness(opts);
    const opener = h.$('#opener');
    h.doc.activeElement = opener;

    h.api.openBrowser('cdash-a', 'JOBS');
    await h.flush();
    assert.equal(h.sockets.at(-1).url, `${base}/api/browser?name=cdash-a`, label);
    assert.equal(h.live(), 1);
    h.sockets.at(-1).open();
    h.history.back(); // Android Back
    assert.equal(h.api.isOpen(), false, label);
    assert.equal(h.live(), 0);
    assert.equal(h.doc.activeElement, opener, 'focus returns to where the overlay was opened');
    assert.equal(h.$('#browser').hidden, true);

    h.api.openTerm('cdash-a', 'JOBS');
    await h.flush();
    assert.equal(h.sockets.at(-1).url, `${base}/api/term?name=cdash-a&cols=80&rows=24`, label);
    assert.equal(h.live(), 1);

    // Opening the browser while the terminal is open closes it and reuses the history entry.
    const entries = h.history.entries.length;
    h.api.openBrowser('cdash-a', 'JOBS');
    await h.flush();
    assert.equal(h.api.termOpen(), false, label);
    assert.equal(h.live(), 1, 'one live socket at a time');
    assert.equal(h.history.entries.length, entries, 'one history entry for the one overlay');

    // A network drop retries once, when the page is next visible.
    const ws = h.sockets.at(-1);
    ws.open();
    ws.serverClose(1006);
    assert.equal(h.live(), 0);
    h.doc.hidden = false;
    for (const f of h.listeners['doc:visibilitychange'] || []) f();
    await h.flush();
    assert.equal(h.live(), 1, 'visibility retry reconnects once');
    for (const f of h.listeners['doc:visibilitychange'] || []) f();
    await h.flush();
    assert.equal(h.live(), 1, 'and does not stack a second socket');

    // 4004 offers no reconnect and no retry.
    const ws4 = h.sockets.at(-1);
    ws4.open();
    ws4.serverClose(4004, 'raw server text');
    assert.equal(h.$('#browser-status').textContent, 'Browser view unsupported');
    assert.equal(h.$('#browser-reconnect').hidden, true);
    for (const f of h.listeners['doc:visibilitychange'] || []) f();
    await h.flush();
    assert.equal(h.live(), 0, 'no automatic retry after 4004');

    // Fixed texts, never the server's own words.
    for (const [code, text] of [[4002, 'Browser not running'], [4003, 'Browser closed'], [1011, 'Browser connection failed']]) {
      h.$('#browser-reconnect').dispatch('click');
      await h.flush();
      const s = h.sockets.at(-1);
      s.open();
      s.serverClose(code, 'URL_CANARY');
      assert.equal(h.$('#browser-status').textContent, text);
      assert.equal(h.$('#browser-reconnect').hidden, false);
    }
    h.history.back();
    assert.equal(h.api.isOpen(), false);
    assert.equal(h.live(), 0);
  }
});
