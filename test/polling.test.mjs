import test from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { runInNewContext } from 'node:vm';

await import('../public/transport/backoff.js');
const source = readFileSync(new URL('../public/app.js', import.meta.url), 'utf8');
const polling = source.slice(source.indexOf('let bk = cdashBackoff.initial();'), source.indexOf('/* ---------- Agent setup'));
const closeTerm = source.slice(source.indexOf('function closeTerm()'), source.indexOf("$('#term-close').innerHTML"));

function dashboard(session = {}) {
  let now = 0, id = 0, calls = 0, error;
  const timers = new Map(), rendered = [], nodes = {};
  const running = [{ name: 'cdash-new', external: false, ...session }];
  const context = {
    cdashBackoff: globalThis.cdashBackoff, term: null, resLimit: 20, isTauri: false,
    document: { hidden: false },
    $: selector => nodes[selector] ||= {},
    api: async () => {
      calls++;
      if (error) throw error;
      return { running: running.map(s => ({ ...s })) };
    },
    render: data => rendered.push(data),
    markConnected() {}, refreshModels() {}, checkAgent() {},
    clearTimeout: key => timers.delete(key),
    setTimeout: (fn, ms) => { timers.set(++id, { fn, at: now + ms }); return id; },
  };
  runInNewContext(`${polling}\n${closeTerm}`, context);
  return {
    context, running, rendered, calls: () => calls,
    fail: () => { error = Error('offline'); },
    poll: async () => { context.poll(); await new Promise(setImmediate); },
    advance: async ms => {
      const end = now + ms;
      while (timers.size) {
        const [key, timer] = [...timers].sort((a, b) => a[1].at - b[1].at)[0];
        if (timer.at > end) break;
        now = timer.at; timers.delete(key); await timer.fn();
      }
      now = end;
    },
  };
}

test('a ready link appears without opening Terminal, then polling returns to 60 seconds', async () => {
  const d = dashboard();
  await d.poll();
  d.running[0].rcLink = 'https://claude.ai/code/session_ready';
  await d.advance(2000);
  assert.equal(d.rendered.at(-1).running[0].rcLink, 'https://claude.ai/code/session_ready');
  assert.equal(d.calls(), 2);
  await d.advance(59999);
  assert.equal(d.calls(), 2);
  await d.advance(1);
  assert.equal(d.calls(), 3);
});

test('closing Terminal immediately refreshes a link that is already ready', async () => {
  const d = dashboard();
  await d.poll();
  d.running[0].rcLink = 'https://claude.ai/code/session_ready';
  d.context.term = { ro: { disconnect() {} }, xterm: { dispose() {} } };
  d.context.closeTerm();
  await new Promise(setImmediate);
  assert.equal(d.rendered.at(-1).running[0].rcLink, 'https://claude.ai/code/session_ready');
  assert.equal(d.calls(), 2);
});

test('external sessions and unavailable links retain the normal polling interval', async () => {
  for (const session of [{ external: true }, { nativeTerminal: true, rcUnavailable: true }]) {
    const d = dashboard(session);
    await d.poll();
    await d.advance(59999);
    assert.equal(d.calls(), 1);
    await d.advance(1);
    assert.equal(d.calls(), 2);
  }
});

test('a pending link retains failure backoff and pauses polling under Terminal', async () => {
  const d = dashboard();
  await d.poll();
  d.context.term = {};
  await d.advance(2000);
  assert.equal(d.calls(), 1);
  d.context.term = null;
  d.fail();
  await d.poll();
  await d.advance(119999);
  assert.equal(d.calls(), 2);
  await d.advance(1);
  assert.equal(d.calls(), 3);
});
