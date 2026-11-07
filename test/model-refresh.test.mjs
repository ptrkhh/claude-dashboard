import test from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { runInNewContext } from 'node:vm';

const source = readFileSync(new URL('../public/app.js', import.meta.url), 'utf8');
const catalog = [
  { value: 'sonnet', label: 'Claude Sonnet 5.5' },
  { value: 'opus', label: 'Claude Opus 5.5' },
  { value: 'codex:gpt-6.1-sol', model: 'gpt-6.1-sol', label: 'GPT-6.1 Sol', efforts: ['low', 'high'] },
  { value: 'opencode:provider/model', label: 'OpenCode model', variants: ['fast', 'deep'] },
];

// Exercise the shipped dropdown and refresh together; only DOM, storage and HTTP are stand-ins.
function launcher(storage = new Map(), dir = '') {
  const node = () => ({ events: {}, attributes: {}, addEventListener(type, fn) { this.events[type] = fn; },
    setAttribute(key, value) { this.attributes[key] = value; }, focus() {} });
  function dropdown() {
    const trigger = node(), current = node();
    const menu = { ...node(), hidden: true, scrollTop: 0, previousElementSibling: trigger,
      set innerHTML(html) {
        this.options = [...html.matchAll(/data-value="([^"]*)"/g)].map(([, value]) => ({
          dataset: { value }, attributes: {}, classes: new Set(),
          classList: { toggle(name, on) { if (on) this.owner.classes.add(name); else this.owner.classes.delete(name); } },
          setAttribute(key, value) { this.attributes[key] = value; }, scrollIntoView() {},
        }));
        this.options.forEach(option => { option.classList.owner = option; });
      }, querySelectorAll() { return this.options; } };
    return Object.assign(node(), { trigger, menu, current, contains: () => false,
      querySelector: selector => ({ '.cb-trigger': trigger, '.cb-menu': menu, '.cb-current': current, '.cb-lead': {} })[selector] });
  }
  const model = dropdown(), effort = dropdown(), directory = { value: dir }, pending = [], notices = [];
  const document = { ...node(), querySelectorAll: () => [model.menu, effort.menu].filter(menu => !menu.hidden) };
  const context = { gen: 1, document, EFFORTS: ['low', 'medium', 'high'], cap: s => s[0].toUpperCase() + s.slice(1),
    esc: s => s, effortLabel: String, $: selector => ({ '#model': model, '#effort': effort, '#dir': directory })[selector],
    localStorage: { getItem: key => storage.get(key) ?? null, setItem: (key, value) => storage.set(key, value) },
    api: path => new Promise((resolve, reject) => pending.push({ path, resolve, reject })), toast: text => notices.push(text) };
  const definitions = source.slice(source.indexOf('const MODELS ='), source.indexOf('const EFFORTS ='));
  const store = source.slice(source.indexOf('const store ='), source.indexOf('const MODES ='));
  const controls = source.slice(source.indexOf('const optionsHtml ='), source.indexOf('// navigator.clipboard'));
  runInNewContext(`${definitions}\n${store}\n${controls}\nthis.refresh = refreshModels; this.models = modelDD; this.efforts = effortDD; this.catalogs = modelCatalogs;`, context);
  return { context, model, effort, directory, pending, notices,
    open: () => model.trigger.events.click({ stopPropagation() {} }),
    key: key => model.events.keydown({ key, preventDefault() {} }),
    values: () => model.menu.options.map(option => option.dataset.value),
    active: () => model.menu.options.find(option => option.classes.has('active'))?.dataset.value };
}

test('model discovery keeps an open dropdown, keyboard position and scroll position', async () => {
  const ui = launcher();
  const loading = ui.context.refresh(1);
  ui.open(); ui.key('ArrowDown');
  ui.model.menu.scrollTop = 70;
  assert.equal(ui.active(), 'opus');
  ui.pending[0].resolve(catalog); await loading;
  assert.equal(ui.model.menu.hidden, false);
  assert.equal(ui.model.trigger.attributes['aria-expanded'], 'true');
  assert.equal(ui.active(), 'opus');
  assert.equal(ui.model.menu.scrollTop, 70);
  assert.ok(ui.values().includes('opencode:provider/model'));
  ui.key('Enter');
  assert.equal(ui.context.models.value, 'opus');
  assert.equal(ui.model.current.textContent, 'Claude Opus 5.5');
  assert.equal(ui.model.menu.hidden, true, 'choosing a model still closes the menu');
  ui.open(); ui.key('Escape');
  assert.equal(ui.model.menu.hidden, true, 'Escape still closes the menu');
});

test('updated dropdown falls back when the highlighted item disappears and never opens a closed menu', () => {
  const ui = launcher();
  ui.open(); ui.key('ArrowDown');
  ui.context.models.setItems([{ value: 'sonnet', label: 'Sonnet' }], 'sonnet');
  assert.equal(ui.model.menu.hidden, false);
  assert.equal(ui.active(), 'sonnet');
  ui.key('Escape');
  ui.context.models.setItems(catalog, 'sonnet');
  assert.equal(ui.model.menu.hidden, true);
});

test('a new page shows the stored project catalog while revalidating and retains it after failure', async () => {
  const storage = new Map();
  const first = launcher(storage, '/project');
  const loading = first.context.refresh(1);
  first.pending[0].resolve(catalog); await loading;
  const reopened = launcher(storage, '/project');
  assert.ok(reopened.values().includes('opencode:provider/model'), 'stored catalog is ready before the first sessions response');
  assert.equal(reopened.model.current.textContent, 'Claude Sonnet 5.5');
  const refreshing = reopened.context.refresh(1);
  assert.ok(reopened.values().includes('opencode:provider/model'), 'cached OpenCode models appear before discovery finishes');
  assert.equal(reopened.model.current.textContent, 'Claude Sonnet 5.5');
  assert.equal(reopened.pending.length, 1, 'stored catalogs still revalidate');
  reopened.open();
  reopened.pending[0].reject(new Error('offline')); await refreshing;
  assert.equal(reopened.model.menu.hidden, false);
  assert.ok(reopened.values().includes('opencode:provider/model'));
  assert.deepEqual(reopened.notices, []);
  for (let i = 0; i < 5; i++) reopened.key('ArrowDown');
  reopened.key('Enter');
  assert.equal(reopened.context.models.value, 'opencode:provider/model');
  const effortKey = key => reopened.effort.events.keydown({ key, preventDefault() {} });
  effortKey('ArrowDown'); effortKey('ArrowDown'); effortKey('Enter');
  assert.equal(reopened.context.efforts.value, 'fast');
  reopened.context.gen++;
  const nextPoll = reopened.context.refresh(2);
  assert.equal(reopened.context.models.value, 'opencode:provider/model');
  assert.equal(reopened.context.efforts.value, 'fast');
  reopened.pending.at(-1).resolve(catalog); await nextPoll;
});

test('stored catalogs keep project-specific OpenCode models separate', async () => {
  const storage = new Map();
  const first = launcher(storage, '/a');
  const loading = first.context.refresh(1);
  first.pending[0].resolve(catalog); await loading;
  const other = launcher(storage, '/b');
  const refreshing = other.context.refresh(1);
  assert.ok(!other.values().includes('opencode:provider/model'));
  other.pending[0].resolve([{ value: 'opencode:b/model', label: 'B' }]); await refreshing;
  const reopened = launcher(storage, '/a');
  const again = reopened.context.refresh(1);
  assert.ok(reopened.values().includes('opencode:provider/model'));
  assert.ok(!reopened.values().includes('opencode:b/model'));
  reopened.pending[0].resolve(catalog); await again;
});

test('a superseded poll cannot replace the catalog or clear a newer pending request', async () => {
  const ui = launcher(new Map(), '/project');
  const old = ui.context.refresh(1);
  ui.context.gen++;
  const current = ui.context.refresh(2);
  ui.pending[0].resolve([{ value: 'sonnet', label: 'Stale Sonnet' }]); await old;
  assert.ok(ui.context.catalogs.get('/project').loading);
  ui.pending[1].resolve(catalog); await current;
  assert.equal(ui.model.current.textContent, 'Claude Sonnet 5.5');
});

test('broken or unavailable storage cannot prevent model discovery', async () => {
  const storage = new Map();
  const first = launcher(storage);
  const loading = first.context.refresh(1);
  first.pending[0].resolve(catalog); await loading;
  assert.equal(storage.size, 1, 'catalog is persisted');
  const key = [...storage.keys()][0];
  for (const broken of ['{', '{}', JSON.stringify([['', null]]), JSON.stringify([['', [{ value: 4, label: 'Bad' }]]]),
    JSON.stringify([['', [{ value: 'sonnet', label: 'Bad', efforts: 'high', variants: 3 }]]])]) {
    const ui = launcher(new Map([[key, broken]]));
    const refreshed = ui.context.refresh(1);
    ui.pending[0].resolve(catalog); await refreshed;
    assert.equal(ui.model.current.textContent, 'Claude Sonnet 5.5');
  }
  const denied = launcher({ get() { throw new Error('denied'); }, set() { throw new Error('quota'); } });
  const refreshed = denied.context.refresh(1);
  denied.pending[0].resolve(catalog); await refreshed;
  assert.ok(denied.values().includes('opencode:provider/model'));
});
