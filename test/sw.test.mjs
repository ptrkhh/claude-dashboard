import test from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { runInNewContext } from 'node:vm';

// Runs public/sw.js against a fake cache and network and returns what one
// fetch event answers.
async function answer(path, { cached, network }) {
  const store = new Map(cached ? [[path, cached]] : []);
  const handlers = {};
  const cache = { put: async (k, r) => store.set(typeof k === 'string' ? k : new URL(k.url).pathname, r) };
  const self = { addEventListener: (type, fn) => { handlers[type] = fn; } };
  runInNewContext(readFileSync(new URL('../public/sw.js', import.meta.url), 'utf8'), {
    self, URL,
    caches: { open: async () => cache, keys: async () => [], match: async k => store.get(typeof k === 'string' ? k : new URL(k.url).pathname) },
    fetch: async () => { if (!network) throw new TypeError('offline'); return network; },
  });
  let responded;
  handlers.fetch({ request: { url: 'https://dash.example' + path, mode: 'no-cors' }, respondWith: p => { responded = p; } });
  return responded;
}
const res = body => ({ ok: true, status: 200, redirected: false, text: async () => body, clone: () => res(body) });

test('assets come from the network, so a page never pairs new HTML with an old stylesheet', async () => {
  const r = await answer('/style.css', { cached: res('old'), network: res('new') });
  assert.equal(await r.text(), 'new');
});
test('offline, the cached asset still answers', async () => {
  const r = await answer('/style.css', { cached: res('old'), network: null });
  assert.equal(await r.text(), 'old');
});
test('a new service worker takes over at once, so old browsers pick up the fix on their next load', () => {
  const handlers = {};
  let skipped = false;
  runInNewContext(readFileSync(new URL('../public/sw.js', import.meta.url), 'utf8'), {
    self: { addEventListener: (type, fn) => { handlers[type] = fn; }, skipWaiting: () => { skipped = true; } },
    URL, caches: {}, fetch: async () => {},
  });
  handlers.install?.({ waitUntil() {} });
  assert.ok(skipped);
});
