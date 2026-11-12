import test from 'node:test';
import assert from 'node:assert/strict';
await import('../public/chat/store.js');
const S = globalThis.cdashChatStore;
const ev = (seq, e) => ({ t: 'ev', seq, ev: e });
const prompt = (uuid, text, extra = {}) => ({ type: 'user', uuid, message: { role: 'user', content: text }, src: 'typed', ...extra });
const say = (uuid, id, text) => ({ type: 'assistant', uuid, message: { id, role: 'assistant', content: [{ type: 'text', text }] } });
const use = (uuid, id, name, input) => ({ type: 'assistant', uuid, message: { id: 'm' + id, role: 'assistant', content: [{ type: 'tool_use', id, name, input }] } });
const done = (uuid, id, text, is_error = false) => ({ type: 'user', uuid, message: { role: 'user', content: [{ type: 'tool_result', tool_use_id: id, content: text, is_error }] } });
const hello = (o = {}) => ({ t: 'hello', epoch: 'e1', tool: 'claude', first: 1, head: 0, reset: true, ...o });
const feed = frames => frames.reduce(S.apply, S.initial());

test('events become rows; repeats and gaps are not shown twice', () => {
  const s = feed([hello(), ev(1, prompt('u1', 'hi')), ev(2, say('k1', 'm1', 'hello')), ev(2, say('k1', 'm1', 'hello')), ev(1, prompt('u1', 'hi'))]);
  assert.deepEqual(S.rows(s).map(r => [r.kind, r.text]), [['user', 'hi'], ['text', 'hello']]);
  assert.equal(s.head, 2);
});
test('tool calls pair with results, group by kind, and show a running state', () => {
  const s = feed([hello(), ev(1, use('a', 't1', 'Read', { file_path: '/p/README.md' })), ev(2, done('b', 't1', 'ok')),
    ev(3, use('c', 't2', 'Read', { file_path: '/p/a.js' })), ev(4, done('d', 't2', 'ok')), ev(5, use('e', 't3', 'Bash', { command: 'npm test' }))]);
  const rows = S.rows(s);
  assert.deepEqual(rows.map(r => [r.kind, r.label, r.running]), [['tools', 'Read 2 files', false], ['tools', 'Running `npm test`', true]]);
  assert.equal(rows[0].calls[1].result, 'ok');
  assert.equal(S.toolLabel('Bash', { command: 'npm test' }, true), 'Ran `npm test`');
  assert.equal(S.toolLabel('Edit', { file_path: '/p/README.md' }, true), 'Edited README.md');
  assert.equal(S.toolLabel('mcp__beeper__search', {}, true), 'mcp__beeper__search');
});
test('my message goes sending, sent by its client id, and is not doubled', () => {
  let s = feed([hello()]);
  S.addMine(s, 'c1', 'do it');
  assert.deepEqual(S.rows(s).map(r => [r.text, r.status]), [['do it', 'sending']]);
  s = S.apply(s, { t: 'ack', cid: 'c1', status: 'sent' });
  s = S.apply(s, ev(1, prompt('u1', 'do it', { cid: 'c1' })));
  assert.deepEqual(S.rows(s).map(r => [r.text, r.status]), [['do it', 'sent']]);
});
test('held, returned, refused and unconfirmed messages keep their text', () => {
  let s = feed([hello()]);
  for (const [cid, status] of [['c1', 'held'], ['c2', 'returned'], ['c3', 'unconfirmed']]) { S.addMine(s, cid, cid); s = S.apply(s, { t: 'ack', cid, status, id: 'h1' }); }
  S.addMine(s, 'c4', 'c4'); s = S.apply(s, { t: 'ack', cid: 'c4', status: 'refused', error: 'session ended' });
  const mine = S.rows(s).filter(r => r.kind === 'user');
  assert.deepEqual(mine.map(r => r.status), ['held', 'returned', 'unconfirmed', 'refused']);
  assert.equal(mine[3].error, 'session ended');
});
test('a withdrawn prompt disappears and a queued one shows until Claude takes it', () => {
  let s = feed([hello(), ev(1, prompt('u4', 'handed back')), ev(2, { type: 'cdash', subtype: 'retract', uuid: 'u4' }),
    ev(3, { type: 'cdash', subtype: 'queued', text: 'use -j4' })]);
  assert.deepEqual(S.rows(s).map(r => [r.text, r.status]), [['use -j4', 'queued']]);
  s = S.apply(s, ev(4, prompt('q1', 'use -j4', { src: 'steer' })));
  assert.deepEqual(S.rows(s).map(r => [r.text, r.status]), [['use -j4', 'sent']]);
});
test('a reset drops what came before, and my held message the agent forgot is marked not sent', () => {
  let s = feed([hello(), ev(1, prompt('u1', 'old'))]);
  S.addMine(s, 'c1', 'waiting'); s = S.apply(s, { t: 'ack', cid: 'c1', status: 'held', id: 'h1' });
  s = S.apply(s, hello({ epoch: 'e2', head: 0 }));
  s = S.apply(s, { t: 'state', working: false, waiting: false, ended: false, held: [] });
  assert.deepEqual(S.rows(s).map(r => [r.text, r.status]), [['waiting', 'refused']]);
  s = S.apply(feed([hello(), ev(1, prompt('u1', 'old'))]), { t: 'reset', epoch: 'e3' });
  assert.equal(S.rows(s).length, 0);
  assert.equal(s.head, 0);
});
test('streamed text shows until the finished block replaces it; older pages go on top', () => {
  let s = feed([hello({ first: 3, head: 2 }), { t: 'delta', id: 'm9', text: 'Hel' }, { t: 'delta', id: 'm9', text: 'lo' }]);
  assert.deepEqual(S.rows(s).map(r => [r.text, r.streaming]), [['Hello', true]]);
  s = S.apply(s, ev(3, say('k9', 'm9', 'Hello.')));
  assert.deepEqual(S.rows(s).map(r => [r.text, r.streaming]), [['Hello.', false]]);
  s = S.apply(s, { t: 'page', events: [{ seq: 1, ev: prompt('u0', 'first') }, { seq: 2, ev: say('k0', 'm0', 'reply') }], more: false });
  assert.deepEqual(S.rows(s).map(r => r.text), ['first', 'reply', 'Hello.']);
  assert.equal(s.more, false);
});
test('turn ends, shell commands and both prompt content shapes are understood', () => {
  const s = feed([hello(), ev(1, { type: 'user', uuid: 'u', message: { role: 'user', content: [{ type: 'text', text: 'block form' }] } }),
    ev(2, { type: 'cdash', subtype: 'shell', uuid: 'b1', command: 'echo hi' }), ev(3, { type: 'cdash', subtype: 'shell_output', uuid: 'b2', stdout: 'hi', stderr: '' }),
    ev(4, { type: 'result', subtype: 'interrupted' }), ev(5, { type: 'result', subtype: 'success' }), ev(6, { type: 'something-new' })]);
  assert.deepEqual(S.rows(s).map(r => [r.kind, r.text ?? r.command]), [['user', 'block form'], ['shell', 'echo hi'], ['note', 'Stopped']]);
  assert.equal(S.rows(s)[1].stdout, 'hi');
});
