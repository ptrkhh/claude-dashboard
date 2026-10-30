import { test } from 'node:test';
import assert from 'node:assert/strict';
import { createServer } from 'node:http';
import { spawn } from 'node:child_process';
import { mkdtemp, writeFile, readFile, rm } from 'node:fs/promises';
import { join, resolve } from 'node:path';
import { tmpdir } from 'node:os';
import { OpenCode } from '../crates/agent/src/collect/opencode-api.mjs';
import { submitPrompt } from '../crates/agent/src/collect/opencode-remote.mjs';

async function fixture(handler) {
  const server = createServer(async (request, response) => {
    let body = '';
    for await (const chunk of request) body += chunk;
    const result = await handler(request, body ? JSON.parse(body) : undefined);
    response.writeHead(result.status || 200, { 'content-type': 'application/json' });
    response.end(JSON.stringify(result.body));
  });
  await new Promise(resolve => server.listen(0, '127.0.0.1', resolve));
  return { client: new OpenCode({ url: `http://127.0.0.1:${server.address().port}`, password: 'fixture' }), close: () => new Promise(resolve => server.close(resolve)) };
}

test('prompt ID is saved before submission and reused after a worker restart', async () => {
  let persisted, firstId, posts = 0;
  const server = await fixture((request, body) => {
    assert.equal(request.url, '/api/session/ses_test/prompt');
    assert.equal(body.delivery, 'steer');
    assert.equal(body.id, persisted.remote_inputs['remote-1'].id);
    firstId ||= body.id;
    assert.equal(body.id, firstId);
    posts++;
    return { body: { data: { id: body.id, sessionID: 'ses_test', type: 'user' } } };
  });
  try {
    let state = { sid: 'ses_test' };
    const save = async () => { persisted = structuredClone(state); };
    await submitPrompt(server.client, state, 'remote-1', 'hello', save);
    state = structuredClone(persisted);
    await submitPrompt(server.client, state, 'remote-1', 'hello', save);
    assert.match(firstId, /^msg_[a-zA-Z0-9]+$/);
    assert.equal(posts, 2);
  } finally { await server.close(); }
});

test('a conflicting ID is accepted only for the intended session user record', async () => {
  let matching = false, posts = 0;
  const state = { sid: 'ses_test', remote_inputs: { 'remote-1': { id: 'msg_abc', sessionID: 'ses_test', text: 'hello' } } };
  const server = await fixture((request, body) => {
    if (request.url.endsWith('/prompt')) { posts++; assert.equal(body.id, 'msg_abc'); return { status: 409, body: {} }; }
    if (request.url.endsWith('/inbox')) return { body: { data: [{ id: 'msg_abc', sessionID: matching ? 'ses_test' : 'ses_other', type: 'user' }] } };
    return { body: { data: matching ? [] : [{ id: 'msg_abc', type: 'assistant' }], cursor: {} } };
  });
  try {
    await assert.rejects(submitPrompt(server.client, state, 'remote-1', 'hello', async () => {}), /conflict/i);
    matching = true;
    await submitPrompt(server.client, state, 'remote-1', 'hello', async () => {});
    assert.equal(posts, 2);
    assert.equal(state.remote_inputs['remote-1'].id, 'msg_abc');
  } finally { await server.close(); }
});

test('history forwards text and tool lifecycle while omitting private content', async () => {
  const { historyMessages } = await import('../crates/agent/src/collect/opencode-remote.mjs');
  const row = { id: 'msg_answer', type: 'assistant', content: [
    { type: 'reasoning', text: 'private reasoning' },
    { type: 'text', text: 'Checking' },
    { type: 'tool', id: 'call_1', name: 'shell', state: { status: 'completed', input: { command: 'pwd' }, content: [{ type: 'text', text: 'x'.repeat(70000) }], metadata: { secret: 'do not include' } } },
  ] };
  const messages = historyMessages(row, 'ses_test', 'remote_session');
  assert.equal(messages.length, 3);
  assert.equal(messages[0].message.content[0].text, 'Checking');
  assert.equal(messages[1].message.content[0].type, 'tool_use');
  assert.equal(messages[2].message.content[0].content.length, 65536);
  assert.doesNotMatch(JSON.stringify(messages), /private reasoning|do not include/);
  assert.deepEqual(messages.map(message => message.uuid), historyMessages(row, 'ses_test', 'remote_session').map(message => message.uuid));
  assert.notEqual(messages[0].uuid, historyMessages(row, 'ses_test', 'other_remote')[0].uuid);
});

test('Claude text steers the shared session, and Stop interrupts it', async () => {
  const { RemoteBridge } = await import('../crates/agent/src/collect/opencode-remote.mjs');
  const calls = [], sent = [], state = { sid: 'ses_test', model: 'fixture/model' };
  const server = await fixture((request, body) => {
    calls.push({ path: request.url, body });
    if (request.url.endsWith('/prompt')) return { body: { data: { id: body.id, sessionID: state.sid, type: 'user' } } };
    return { status: 204, body: null };
  });
  try {
    const remote = { event: async payload => sent.push(payload), state: async () => {} };
    const bridge = new RemoteBridge(server.client, remote, state, async () => {});
    await bridge.handle({ type: 'user', uuid: 'remote-one', message: { content: 'Fix it' } });
    await bridge.handle({ type: 'control_request', request_id: 'stop-one', request: { subtype: 'interrupt' } });
    await bridge.handle({ type: 'user', uuid: 'attachment', message: { content: [{ type: 'image', data: 'binary' }] } });
    assert.equal(calls.filter(call => call.path.endsWith('/prompt')).length, 1);
    assert.equal(calls[0].body.delivery, 'steer');
    assert.equal(calls[1].path, '/api/session/ses_test/interrupt');
    assert.ok(sent.some(payload => JSON.stringify(payload).includes('Terminal')));
  } finally { await server.close(); }
});

test('permission replies are one-time and stale approvals have no effect', async () => {
  const { RemoteBridge } = await import('../crates/agent/src/collect/opencode-remote.mjs');
  const sent = [], replies = [];
  let pending = [{ id: 'per_one', sessionID: 'ses_test', action: 'bash', resources: ['pwd'], source: { type: 'tool', id: 'call_one', messageID: 'msg_one' } }];
  const server = await fixture((request, body) => {
    if (request.url.endsWith('/permission')) return { body: { data: pending } };
    replies.push(body); pending = []; return { status: 204, body: null };
  });
  try {
    const remote = { event: async payload => sent.push(payload), state: async () => {} };
    const bridge = new RemoteBridge(server.client, remote, { sid: 'ses_test', model: 'fixture/model' }, async () => {});
    await bridge.syncPermissions();
    const id = sent.find(payload => payload.type === 'control_request').request_id;
    await bridge.handle({ type: 'control_response', response: { request_id: id, response: { behavior: 'allow' } } });
    assert.deepEqual(replies, [{ decision: 'once' }]);
    await bridge.handle({ type: 'control_response', response: { request_id: id, response: { behavior: 'allow' } } });
    assert.equal(replies.length, 1);
    pending = [{ id: 'per_two', sessionID: 'ses_test', action: 'bash', resources: ['ls'] }];
    await bridge.syncPermissions();
    const stale = sent.filter(payload => payload.type === 'control_request').at(-1).request_id;
    pending = [];
    await bridge.syncPermissions();
    assert.ok(sent.some(payload => payload.type === 'control_cancel_request' && payload.request_id === stale));
    await bridge.handle({ type: 'control_response', response: { request_id: stale, response: { behavior: 'allow' } } });
    assert.equal(replies.length, 1);
    pending = [{ id: 'per_three', sessionID: 'ses_test', action: 'bash', resources: ['id'] }];
    await bridge.syncPermissions();
    const denied = sent.filter(payload => payload.type === 'control_request').at(-1).request_id;
    await bridge.handle({ type: 'control_response', response: { request_id: denied, response: { behavior: 'deny' } } });
    assert.deepEqual(replies, [{ decision: 'once' }, { decision: 'reject' }]);
  } finally { await server.close(); }
});

test('reconciliation preserves server order and does not duplicate replayed tools', async () => {
  const { RemoteBridge } = await import('../crates/agent/src/collect/opencode-remote.mjs');
  const sent = [], states = [];
  const rows = [
    { id: 'msg_z', type: 'user', text: 'First' },
    { id: 'msg_a', type: 'assistant', time: { completed: 1 }, content: [{ type: 'tool', id: 'call_1', name: 'shell', state: { status: 'completed', input: { command: 'pwd' }, content: [{ type: 'text', text: '/work' }] } }, { type: 'text', text: 'Done' }] },
  ];
  const server = await fixture(request => {
    if (request.url.includes('/message')) return { body: { data: rows, cursor: {} } };
    if (request.url.endsWith('/permission') || request.url.endsWith('/form')) return { body: { data: [] } };
    if (request.url.endsWith('/active')) return { body: { data: {} } };
    return { body: { data: { id: 'ses_test', model: { providerID: 'fixture', id: 'model' } } } };
  });
  try {
    const remote = { sid: 'remote_session', sequence: 0, event: async payload => sent.push(payload), state: async status => states.push(status) };
    const state = { sid: 'ses_test', model: 'fixture/model', remote_sid: 'old_remote', remote_sequence: 900 };
    const bridge = new RemoteBridge(server.client, remote, state, async () => {});
    await bridge.reconcile();
    assert.equal(state.remote_sid, 'remote_session');
    assert.equal(state.remote_sequence, 0, 'a new Claude identity must not inherit the previous sequence');
    await bridge.reconcile();
    assert.equal(sent.filter(item => item.message?.content?.[0]?.type === 'tool_use').length, 1);
    assert.equal(sent.filter(item => item.message?.content?.[0]?.type === 'tool_result').length, 1);
    assert.equal(sent[0].message.content[0].text, 'First');
    assert.equal(states.at(-1), 'idle');
  } finally { await server.close(); }
});

test('live native execution emits tool starts, completion, and idle exactly once', async () => {
  const { RemoteBridge } = await import('../crates/agent/src/collect/opencode-remote.mjs');
  const sent = [], statuses = [];
  let completed = false;
  const row = () => ({ id: 'msg_answer', type: 'assistant', time: completed ? { completed: 1 } : {}, content: [
    { type: 'tool', id: 'tool_1', name: 'shell', state: { status: completed ? 'completed' : 'running', input: { command: 'pwd' }, ...(completed ? { content: [{ type: 'text', text: '/work' }] } : { metadata: {} }) } },
    ...(completed ? [{ type: 'text', text: 'Done' }] : []),
  ] });
  const server = await fixture(request => {
    if (request.url.includes('/message/msg_answer')) return { body: { data: row() } };
    if (request.url.includes('/message')) return { body: { data: [row()], cursor: {} } };
    if (request.url.endsWith('/permission') || request.url.endsWith('/form')) return { body: { data: [] } };
    if (request.url.endsWith('/active')) return { body: { data: completed ? {} : { ses_test: { type: 'running' } } } };
    return { body: { data: { id: 'ses_test', model: { providerID: 'fixture', id: 'model' } } } };
  });
  try {
    const remote = { sid: 'remote', event: async event => sent.push(event), state: async value => statuses.push(value) };
    const bridge = new RemoteBridge(server.client, remote, { sid: 'ses_test', model: 'fixture/model' }, async () => {});
    await bridge.nativeEvent({ type: 'session.execution.started', data: { sessionID: 'ses_test' } });
    await bridge.nativeEvent({ type: 'session.tool.called', data: { sessionID: 'ses_test', assistantMessageID: 'msg_answer' } });
    assert.equal(sent.filter(event => event.message?.content?.[0]?.type === 'tool_use').length, 1);
    completed = true;
    await bridge.nativeEvent({ type: 'session.step.ended', data: { sessionID: 'ses_test', assistantMessageID: 'msg_answer' } });
    await bridge.nativeEvent({ type: 'session.execution.succeeded', data: { sessionID: 'ses_test' } });
    await bridge.reconcile();
    assert.equal(sent.filter(event => event.message?.content?.[0]?.type === 'tool_use').length, 1);
    assert.equal(sent.filter(event => event.message?.content?.[0]?.type === 'tool_result').length, 1);
    assert.equal(sent.filter(event => event.type === 'result').length, 1);
    assert.equal(statuses.at(-1), 'idle');
  } finally { await server.close(); }
});

test('worker bridges a real native event stream and Claude input through one session', { timeout: 15000 }, async t => {
  const dir = await mkdtemp(join(tmpdir(), 'cdash-oc-worker-'));
  t.after(() => rm(dir, { recursive: true, force: true }));
  const stateFile = join(dir, 'pane.json'), wire = join(dir, 'remote.jsonl'), preload = join(dir, 'preload.mjs');
  await writeFile(stateFile, JSON.stringify({ sid: 'ses_test', dir, model: 'fixture/model', display_name: 'Fixture' }));
  await writeFile(wire, '');
  await writeFile(preload, `
import { appendFileSync } from 'node:fs';
const native=globalThis.fetch;
globalThis.fetch=async (url, options={}) => {
 if(!String(url).startsWith('https://api.anthropic.com/')) return native(url,options);
 const path=new URL(url).pathname;
 appendFileSync(process.env.CDASH_TEST_WIRE,JSON.stringify({path,body:options.body?JSON.parse(options.body):null})+'\\n');
 if(path.endsWith('/stream')) return new Response(new ReadableStream({start(controller){
  const events=[{sequence_num:1,event_id:'prompt-event',payload:{type:'user',uuid:'prompt-one',message:{content:'Do it'}}},{sequence_num:2,event_id:'stop-event',payload:{type:'control_request',request_id:'stop-one',request:{subtype:'interrupt'}}}];
  for(const event of events)controller.enqueue(new TextEncoder().encode('event: client_event\\ndata: '+JSON.stringify(event)+'\\n\\n'));
  options.signal.addEventListener('abort',()=>controller.close(),{once:true});
 }}));
 return new Response('{}');
};
`);
  let stream, stage = 0, prompted = 0, interrupted = 0;
  const row = () => ({ id: 'msg_answer', type: 'assistant', time: stage > 1 ? { completed: 1 } : {}, content: [{ type: 'tool', id: 'tool_1', name: 'shell', state: { status: stage > 1 ? 'completed' : 'running', input: { command: 'pwd' }, ...(stage > 1 ? { content: [{ type: 'text', text: '/work' }] } : { metadata: {} }) } }, ...(stage > 1 ? [{ type: 'text', text: 'Done' }] : [])] });
  const native = createServer(async (request, response) => {
    const path = request.url.split('?')[0]; let raw = '';
    for await (const chunk of request) raw += chunk;
    const body = raw ? JSON.parse(raw) : {};
    const json = data => { response.writeHead(200, { 'content-type': 'application/json' }); response.end(JSON.stringify({ data })); };
    if (path === '/api/event') { response.writeHead(200, { 'content-type': 'text/event-stream' }); stream = response; response.write('data: {"type":"server.connected","data":{}}\n\n'); return; }
    if (path.endsWith('/prompt')) {
      prompted++;
      json({ id: body.id, sessionID: 'ses_test', type: 'user' });
      setTimeout(() => {
        stage = 1;
        for (const type of ['session.execution.started', 'session.tool.called']) stream?.write(`data: ${JSON.stringify({ type, data: { sessionID: 'ses_test', assistantMessageID: 'msg_answer' } })}\n\n`);
        setTimeout(() => {
          stage = 2;
          for (const type of ['session.tool.success', 'session.step.ended', 'session.execution.succeeded']) stream?.write(`data: ${JSON.stringify({ type, data: { sessionID: 'ses_test', assistantMessageID: 'msg_answer' } })}\n\n`);
        }, 100);
      }, 100);
      return;
    }
    if (path.endsWith('/interrupt')) { interrupted++; response.writeHead(204); response.end(); return; }
    if (path.endsWith('/permission') || path.endsWith('/form') || path.endsWith('/inbox')) return json([]);
    if (path.endsWith('/active')) return json(stage === 1 ? { ses_test: { type: 'running' } } : {});
    if (path.endsWith('/message/msg_answer')) return json(row());
    if (path.endsWith('/message')) return json(stage ? [row()] : []);
    return json({ id: 'ses_test', model: { providerID: 'fixture', id: 'model' } });
  });
  await new Promise(resolve => native.listen(0, '127.0.0.1', resolve));
  const child = spawn(process.execPath, ['--import', preload, resolve('crates/agent/src/collect/opencode-remote.mjs'), '--worker', '--sdk-url', 'https://api.anthropic.com/v1/code/sessions/cse_fixture', '--session-id', 'cse_fixture'], {
    env: { ...process.env, CDASH_TEST_WIRE: wire, CDASH_OPENCODE_STATE: stateFile, CDASH_OPENCODE_URL: `http://127.0.0.1:${native.address().port}`, CDASH_OPENCODE_PASSWORD: 'fixture-password', CLAUDE_CODE_SESSION_ACCESS_TOKEN: 'fixture-token', CLAUDE_CODE_WORKER_EPOCH: '1' }, stdio: ['pipe', 'pipe', 'pipe'],
  });
  let stderr = ''; child.stderr.on('data', value => stderr += value);
  try {
    let events = [];
    for (let i = 0; i < 200; i++) {
      events = (await readFile(wire, 'utf8')).trim().split('\n').filter(Boolean).map(JSON.parse);
      if (events.some(event => event.body?.events?.some(item => item.payload.type === 'result')) && interrupted) break;
      if (child.exitCode !== null) throw Error(`worker exited: ${stderr}`);
      await new Promise(resolve => setTimeout(resolve, 50));
    }
    const payloads = events.flatMap(event => event.body?.events || []).map(item => item.payload);
    assert.equal(prompted, 1);
    assert.equal(interrupted, 1);
    assert.ok(payloads.some(payload => payload.message?.content?.[0]?.type === 'tool_use'));
    assert.ok(payloads.some(payload => payload.message?.content?.[0]?.type === 'tool_result'));
    assert.ok(payloads.some(payload => payload.type === 'result'));
    assert.doesNotMatch(await readFile(stateFile, 'utf8'), /fixture-token|fixture-password/);
  } finally {
    if (child.exitCode === null) { const exited = new Promise(resolve => child.once('exit', resolve)); child.kill('SIGTERM'); await exited; }
    native.closeAllConnections(); await new Promise(resolve => native.close(resolve));
  }
});

test('native turn ending during history upload leaves one completion and idle state', { timeout: 5000 }, async () => {
  const { RemoteBridge } = await import('../crates/agent/src/collect/opencode-remote.mjs');
  const sent = [], statuses = [];
  let stream, triggered = false;
  const server = createServer((request, response) => {
    const path = request.url.split('?')[0];
    const json = data => { response.writeHead(200, { 'content-type': 'application/json' }); response.end(JSON.stringify({ data })); };
    if (path === '/api/event') { response.writeHead(200, { 'content-type': 'text/event-stream' }); stream = response; response.write('data: {"type":"server.connected","data":{}}\n\n'); return; }
    if (path.endsWith('/message')) {
      json([{ id: 'msg_old', type: 'user', text: 'Earlier' }]);
      if (!triggered) {
        triggered = true;
        setTimeout(() => { for (const type of ['session.execution.started', 'session.execution.succeeded']) stream.write(`data: ${JSON.stringify({ type, data: { sessionID: 'ses_test' } })}\n\n`); }, 20);
      }
      return;
    }
    if (path.endsWith('/permission') || path.endsWith('/form')) return json([]);
    if (path.endsWith('/active')) return json({});
    return json({ id: 'ses_test', model: { providerID: 'fixture', id: 'model' } });
  });
  await new Promise(resolve => server.listen(0, '127.0.0.1', resolve));
  const controller = new AbortController();
  try {
    const remote = { sid: 'remote', event: async payload => { sent.push(payload); if (payload.type === 'user') await new Promise(resolve => setTimeout(resolve, 100)); }, state: async status => statuses.push(status) };
    const client = new OpenCode({ url: `http://127.0.0.1:${server.address().port}`, password: 'fixture' });
    const bridge = new RemoteBridge(client, remote, { sid: 'ses_test', model: 'fixture/model' }, async () => {});
    await bridge.connect(controller.signal);
    for (let i = 0; i < 100 && !sent.some(item => item.type === 'result'); i++) await new Promise(resolve => setTimeout(resolve, 20));
    assert.equal(sent.filter(item => item.type === 'result').length, 1);
    assert.equal(statuses.at(-1), 'idle');
  } finally { controller.abort(); server.closeAllConnections(); await new Promise(resolve => server.close(resolve)); }
});

test('OpenCode stream reconnect reconciles missed history without duplicates', { timeout: 5000 }, async () => {
  const { RemoteBridge } = await import('../crates/agent/src/collect/opencode-remote.mjs');
  const sent = []; let connections = 0;
  const server = createServer((request, response) => {
    const path = request.url.split('?')[0];
    const json = data => { response.writeHead(200, { 'content-type': 'application/json' }); response.end(JSON.stringify({ data })); };
    if (path === '/api/event') {
      connections++; response.writeHead(200, { 'content-type': 'text/event-stream' }); response.write('data: {"type":"server.connected","data":{}}\n\n');
      if (connections === 1) setTimeout(() => response.end(), 80);
      return;
    }
    if (path.endsWith('/message')) return json([{ id: 'msg_z', type: 'user', text: 'Before disconnect' }, ...(connections > 1 ? [{ id: 'msg_a', type: 'user', text: 'While offline' }] : [])]);
    if (path.endsWith('/permission') || path.endsWith('/form')) return json([]);
    if (path.endsWith('/active')) return json({});
    return json({ id: 'ses_test', model: { providerID: 'fixture', id: 'model' } });
  });
  await new Promise(resolve => server.listen(0, '127.0.0.1', resolve));
  const controller = new AbortController();
  try {
    const remote = { sid: 'remote', event: async payload => sent.push(payload), state: async () => {} };
    const client = new OpenCode({ url: `http://127.0.0.1:${server.address().port}`, password: 'fixture' });
    const bridge = new RemoteBridge(client, remote, { sid: 'ses_test', model: 'fixture/model' }, async () => {});
    await bridge.connect(controller.signal);
    for (let i = 0; i < 150 && !sent.some(item => item.message?.content?.[0]?.text === 'While offline'); i++) await new Promise(resolve => setTimeout(resolve, 20));
    assert.ok(connections >= 2);
    assert.equal(sent.filter(item => item.message?.content?.[0]?.text === 'Before disconnect').length, 1);
    assert.equal(sent.filter(item => item.message?.content?.[0]?.text === 'While offline').length, 1);
  } finally { controller.abort(); server.closeAllConnections(); await new Promise(resolve => server.close(resolve)); }
});

test('interactive OpenCode forms direct the Claude user to Terminal', async () => {
  const { RemoteBridge } = await import('../crates/agent/src/collect/opencode-remote.mjs');
  const sent = [], statuses = [];
  const remote = { sid: 'remote', event: async payload => sent.push(payload), state: async (...args) => statuses.push(args) };
  const bridge = new RemoteBridge({}, remote, { sid: 'ses_test', model: 'fixture/model' }, async () => {});
  await bridge.nativeEvent({ type: 'form.created', data: { form: { id: 'form_one', sessionID: 'ses_test' } } });
  assert.ok(sent.some(payload => JSON.stringify(payload).includes('Terminal')));
  assert.equal(statuses.at(-1)[0], 'requires_action');
});

test('a lost prompt response retries the saved native ID', async () => {
  let attempts = 0, original, saved;
  const server = createServer(async (request, response) => {
    let raw = ''; for await (const chunk of request) raw += chunk;
    const body = JSON.parse(raw);
    attempts++; original ||= body.id;
    assert.equal(body.id, original);
    if (attempts === 1) { request.socket.destroy(); return; }
    response.end(JSON.stringify({ data: { id: body.id, sessionID: 'ses_test', type: 'user' } }));
  });
  await new Promise(resolve => server.listen(0, '127.0.0.1', resolve));
  try {
    const client = new OpenCode({ url: `http://127.0.0.1:${server.address().port}`, password: 'fixture' });
    let state = { sid: 'ses_test' };
    const save = async () => { saved = structuredClone(state); };
    await assert.rejects(submitPrompt(client, state, 'remote-lost', 'hello', save));
    state = structuredClone(saved);
    await submitPrompt(client, state, 'remote-lost', 'hello', save);
    assert.equal(attempts, 2);
  } finally { server.closeAllConnections(); await new Promise(resolve => server.close(resolve)); }
});

test('native events are buffered while Claude initialization uploads', { timeout: 5000 }, async () => {
  const { RemoteBridge } = await import('../crates/agent/src/collect/opencode-remote.mjs');
  const sent = [], statuses = []; let release;
  const gate = new Promise(resolve => { release = resolve; });
  const server = createServer((request, response) => {
    const path = request.url.split('?')[0];
    const json = data => { response.writeHead(200, { 'content-type': 'application/json' }); response.end(JSON.stringify({ data })); };
    if (path === '/api/event') {
      response.writeHead(200, { 'content-type': 'text/event-stream' });
      for (const type of ['server.connected', 'session.execution.started', 'session.execution.succeeded']) response.write(`data: ${JSON.stringify({ type, data: { sessionID: 'ses_test' } })}\n\n`);
      return;
    }
    if (path.endsWith('/message') || path.endsWith('/permission') || path.endsWith('/form')) return json([]);
    if (path.endsWith('/active')) return json({});
    return json({ id: 'ses_test', model: { providerID: 'fixture', id: 'model' } });
  });
  await new Promise(resolve => server.listen(0, '127.0.0.1', resolve));
  const controller = new AbortController();
  try {
    const remote = { sid: 'remote', event: async payload => sent.push(payload), state: async status => statuses.push(status) };
    const client = new OpenCode({ url: `http://127.0.0.1:${server.address().port}`, password: 'fixture' });
    const bridge = new RemoteBridge(client, remote, { sid: 'ses_test', model: 'fixture/model' }, async () => {});
    const ready = bridge.connect(controller.signal, () => {}, gate);
    await new Promise(resolve => setTimeout(resolve, 100));
    assert.equal(sent.length, 0);
    assert.equal(statuses.length, 0);
    release(); await ready;
    for (let i = 0; i < 100 && !sent.some(item => item.type === 'result'); i++) await new Promise(resolve => setTimeout(resolve, 20));
    assert.equal(sent.filter(item => item.type === 'result').length, 1);
    assert.equal(statuses.at(-1), 'idle');
  } finally { release(); controller.abort(); server.closeAllConnections(); await new Promise(resolve => server.close(resolve)); }
});

test('resolving one approval keeps the remaining approval visible', async () => {
  const { RemoteBridge } = await import('../crates/agent/src/collect/opencode-remote.mjs');
  const sent = [], statuses = [];
  let pending = ['per_a', 'per_b'].map(id => ({ id, sessionID: 'ses_test', action: id, resources: [] }));
  const server = await fixture((request, body) => {
    if (request.url.endsWith('/permission')) return { body: { data: pending } };
    pending = pending.filter(item => !request.url.includes(item.id)); return { status: 204, body: null };
  });
  try {
    const remote = { sid: 'remote', event: async payload => sent.push(payload), state: async (...args) => statuses.push(args) };
    const bridge = new RemoteBridge(server.client, remote, { sid: 'ses_test' }, async () => {});
    await bridge.syncPermissions();
    const last = sent.filter(payload => payload.type === 'control_request').at(-1).request_id;
    await bridge.handle({ type: 'control_response', response: { request_id: last, response: { behavior: 'deny' } } });
    assert.equal(statuses.at(-1)[0], 'requires_action');
    assert.equal(statuses.at(-1)[1].tool_name, 'per_a');
  } finally { await server.close(); }
});
