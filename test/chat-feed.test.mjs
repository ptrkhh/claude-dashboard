import test from 'node:test';
import assert from 'node:assert/strict';
import { createConnection } from 'node:net';
import { mkdtemp, stat, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { ChatFeed } from '../crates/agent/src/collect/chat-feed.mjs';

const user = (uuid, text) => ({ type: 'user', uuid, message: { role: 'user', content: [{ type: 'text', text }] } });
async function client(path, first) {                              // returns { lines, write, close }
  const socket = createConnection(path); const lines = []; let buf = '';
  socket.on('data', d => { buf += d; let i; while ((i = buf.indexOf('\n')) >= 0) { lines.push(JSON.parse(buf.slice(0, i))); buf = buf.slice(i + 1); } });
  await new Promise(r => socket.once('connect', r));
  socket.write(JSON.stringify(first) + '\n');
  return { lines, write: o => socket.write(JSON.stringify(o) + '\n'), close: () => socket.destroy() };
}
const until = async (f, ms = 2000) => { const t = Date.now(); while (!f()) { if (Date.now() - t > ms) throw Error('timed out'); await new Promise(r => setTimeout(r, 10)); } };

test('a client gets the log, state, ready and then live events; reconnecting resumes', async t => {
  const dir = await mkdtemp(join(tmpdir(), 'cdash-feed-')); t.after(() => rm(dir, { recursive: true, force: true }));
  const feed = new ChatFeed({ send: async () => {}, stop: async () => {} }); t.after(() => feed.close());
  const path = await feed.listen(dir);
  assert.equal((await stat(path)).mode & 0o777, 0o600);
  feed.accept(user('u1', 'one')); feed.accept(user('u1', 'one')); feed.accept(user('u2', 'two'));
  const a = await client(path, { after: 0, epoch: null });
  await until(() => a.lines.length >= 4);
  assert.deepEqual(a.lines.map(l => l.t), ['hello', 'ev', 'ev', 'state'], 'duplicates by uuid are dropped; not ready yet');
  feed.ready(); feed.setState({ working: true, waiting: false }); feed.delta('m1', 'Hel'); feed.accept(user('u3', 'three'));
  await until(() => a.lines.length >= 8);
  assert.deepEqual(a.lines.slice(4).map(l => l.t), ['ready', 'state', 'delta', 'ev']);
  assert.equal(a.lines.at(-1).seq, 3);
  const b = await client(path, { after: 2, epoch: a.lines[0].epoch });
  await until(() => b.lines.some(l => l.t === 'ready'));
  assert.deepEqual(b.lines.filter(l => l.t === 'ev').map(l => l.seq), [3]);
  const c = await client(path, { after: 2, epoch: 'someone-else' });
  await until(() => c.lines.some(l => l.t === 'ready'));
  assert.deepEqual(c.lines.filter(l => l.t === 'ev').map(l => l.seq), [1, 2, 3]);
  for (const x of [a, b, c]) x.close();
});
test('send and stop reach the supervisor and failures come back in the ack', async t => {
  const dir = await mkdtemp(join(tmpdir(), 'cdash-feed-')); t.after(() => rm(dir, { recursive: true, force: true }));
  const calls = [];
  const feed = new ChatFeed({ send: async (text, o) => { calls.push(['send', text, o.steer, o.cid]); if (text === 'bad') throw Error('no turn'); }, stop: async () => { calls.push(['stop']); } });
  t.after(() => feed.close());
  const a = await client(await feed.listen(dir), { after: 0, epoch: null });
  a.write({ t: 'send', cid: 'c1', text: 'go', steer: true }); a.write({ t: 'send', cid: 'c2', text: 'bad', steer: false }); a.write({ t: 'stop' }); a.write('garbage');
  await until(() => a.lines.filter(l => l.t === 'ack').length === 2 && calls.length === 3);
  assert.deepEqual(calls, [['send', 'go', true, 'c1'], ['send', 'bad', false, 'c2'], ['stop']]);
  assert.deepEqual(a.lines.filter(l => l.t === 'ack'), [{ t: 'ack', cid: 'c1', ok: true }, { t: 'ack', cid: 'c2', ok: false, error: 'no turn' }]);
  a.close();
});
test('only conversation messages are kept and approval requests set waiting', async () => {
  const feed = new ChatFeed({ send: async () => {}, stop: async () => {} });
  feed.accept({ type: 'stream_event', event: { type: 'message_start' } });
  feed.accept({ type: 'system', subtype: 'init' });
  feed.accept({ type: 'control_request', request_id: 'r1', request: { subtype: 'can_use_tool' } });
  assert.deepEqual([feed.head, feed.state.waiting], [0, true]);
  feed.accept({ type: 'control_cancel_request', request_id: 'r1' });
  feed.accept({ type: 'result', subtype: 'success', is_error: false, result: 'x', num_turns: 1 });
  assert.deepEqual([feed.head, feed.state.waiting], [1, false]);
  assert.deepEqual(feed.log[0].ev, { type: 'result', subtype: 'success' });
});
