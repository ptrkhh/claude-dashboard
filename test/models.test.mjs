import test from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { runInNewContext } from 'node:vm';

const src = readFileSync(new URL('../public/app.js', import.meta.url), 'utf8');
function picker(catalog = []) {
  const context = { picks: {}, EFFORTS: ['low', 'medium', 'high'] };
  const models = src.slice(src.indexOf('const MODELS ='), src.indexOf('const EFFORTS ='));
  const filter = src.includes('function latestModels(')
    ? src.slice(src.indexOf('function latestModels('), src.indexOf('let modelChoices =')) : '';
  const state = src.slice(src.indexOf('function pickState('), src.indexOf('const pickDropdown ='));
  runInNewContext(`${models}\n${filter}\nthis.latest = typeof latestModels === 'function' ? latestModels : x => x;
    let modelChoices = [...MODELS.filter(m => !catalog.some(c => c.value === m.value)), ...catalog];
    ${state}\nthis.pick = pickState;`, Object.assign(context, { catalog }));
  return context;
}
const choice = model => ({ value: `codex:${model}`, model, label: model, efforts: ['low', 'high'], defaultEffort: 'low' });
const plain = value => JSON.parse(JSON.stringify(value));

test('picker keeps the numerically latest available model in each family without assuming catalog order', () => {
  const input = ['gpt-5.6-sol', 'gpt-6-luna', 'gpt-6-sol', 'gpt-6.9-terra', 'gpt-6-astra', 'gpt-6.1-sol', 'gpt-5.6-luna', 'gpt-6.10-terra'].map(choice);
  const result = picker().latest(input);
  assert.deepEqual(plain(result.map(m => m.model)), ['gpt-6.1-sol', 'gpt-6-luna', 'gpt-6.10-terra', 'gpt-6-astra']);
  assert.strictEqual(result[0], input[5], 'keep metadata belonging to the latest model');
  assert.equal(input.length, 8, 'leave full catalog intact');
});

test('picker retains unknown models and distinct model variants', () => {
  const input = ['custom-model', 'gpt-6-sol', 'gpt-6.1-sol', 'gpt-6-sol-mini', 'gpt-6', 'gpt-6.1'].map(choice);
  assert.deepEqual(plain(picker().latest(input).map(m => m.model)), ['custom-model', 'gpt-6.1-sol', 'gpt-6-sol-mini', 'gpt-6.1']);
});

test('resume uses versioned Claude labels from discovery and stays within its provider', () => {
  const p = picker([{ value: 'opus', model: 'claude-opus-5-5', label: 'Claude Opus 5.5' }, choice('gpt-6-astra')]);
  const state = p.pick({ sid: 'claude', modelId: 'opus' });
  assert.equal(state.models.find(m => m.value === 'opus').label, 'Claude Opus 5.5');
  assert.ok(state.models.every(m => !m.value.startsWith('codex:')));
});

test('resume preserves an older saved Codex model while offering only latest alternatives', () => {
  const p = picker(['gpt-6-sol', 'gpt-6.1-sol', 'gpt-5.6-luna', 'gpt-6-luna'].map(choice));
  const state = p.pick({ sid: 'old', modelId: 'codex:gpt-6-sol', model: 'GPT-6-Sol', effort: 'high' });
  assert.equal(state.model, 'codex:gpt-6-sol');
  assert.equal(state.effort, 'high');
  assert.deepEqual(plain(state.models.map(m => m.value)), ['codex:gpt-6-sol', 'codex:gpt-6.1-sol', 'codex:gpt-6-luna']);
});

test('launch refresh merges discovered Claude labels and latest GPT families with alias fallbacks', async () => {
  const catalog = [{ value: 'opus', model: 'claude-opus-5-5', label: 'Claude Opus 5.5' },
    ...['gpt-6-sol', 'gpt-6.1-sol', 'gpt-6-luna'].map(choice)];
  const context = { gen: 1, api: async () => catalog, updateEfforts() {},
    modelDD: { setItems(items) { context.items = plain(items); } } };
  const definitions = src.slice(src.indexOf('const MODELS ='), src.indexOf('const EFFORTS ='));
  const filter = src.slice(src.indexOf('function latestModels('), src.indexOf('const modelDD ='));
  const refresh = src.slice(src.indexOf('let modelsAt ='), src.indexOf('function toast('));
  runInNewContext(`${definitions}\n${filter}\n${refresh}\nthis.refresh = refreshModels;`, context);
  await context.refresh(1);
  assert.deepEqual(context.items.map(m => m.value), ['sonnet', 'opus', 'haiku', 'fable', 'codex:gpt-6.1-sol', 'codex:gpt-6-luna']);
  assert.equal(context.items[1].label, 'Claude Opus 5.5');
  assert.equal(context.items[0].label, 'Claude Sonnet', 'unavailable Claude metadata retains a usable alias');
});
