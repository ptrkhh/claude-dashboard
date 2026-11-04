import { test } from 'node:test';
import assert from 'node:assert/strict';
import { createServer } from 'node:http';
import { execFile } from 'node:child_process';
import { promisify } from 'node:util';
import { mkdtemp, mkdir, writeFile, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { setTimeout as delay } from 'node:timers/promises';
import { startServer } from '../crates/agent/src/collect/opencode-api.mjs';
import { RemoteBridge } from '../crates/agent/src/collect/opencode-remote.mjs';

test('OpenCode API qualification: stranded input retries execute once', { skip: process.env.CDASH_OPENCODE_INTEGRATION !== '1', timeout: 45000 }, async t => {
  const { stdout } = await promisify(execFile)('opencode', ['--version']);
  t.diagnostic(`Installed ${stdout.trim()}`);
  const root = await mkdtemp(join(tmpdir(), 'cdash-oc-live-'));
  const project = join(root, 'project');
  for (const dir of [project, join(root, 'home'), join(root, 'config'), join(root, 'data'), join(root, 'cache')]) await mkdir(dir, { recursive: true });
  let requests = 0;
  const provider = createServer(async (req, res) => {
    for await (const _ of req) { /* drain fixture request */ }
    requests++;
    res.writeHead(200, { 'content-type': 'text/event-stream' });
    if (requests >= 4) { res.write(': awaiting interruption\n\n'); return; }
    for (const delta of [{ role: 'assistant', content: 'Fixture reply' }, {}]) res.write(`data: ${JSON.stringify({ id: 'fixture', object: 'chat.completion.chunk', created: 1, model: 'test', choices: [{ index: 0, delta, finish_reason: delta.content ? null : 'stop' }] })}\n\n`);
    res.end('data: [DONE]\n\n');
  });
  await new Promise(resolve => provider.listen(0, '127.0.0.1', resolve));
  const config = { model: 'fixture/test', providers: { fixture: { name: 'Fixture', package: '@opencode/ai/providers/openai-compatible', settings: { baseURL: `http://127.0.0.1:${provider.address().port}/v1`, apiKey: 'fixture' }, models: { test: { name: 'Fixture', limit: { context: 100000, output: 4096 } } } } } };
  await writeFile(join(project, 'opencode.json'), JSON.stringify(config));
  const env = { PATH: process.env.PATH, HOME: join(root, 'home'), XDG_CONFIG_HOME: join(root, 'config'), XDG_DATA_HOME: join(root, 'data'), XDG_CACHE_HOME: join(root, 'cache'), NO_PROXY: '*', no_proxy: '*' };
  const tmux = promisify(execFile);
  const socket = `cdash-opencode-test-${process.pid}`;
  let client;
  try {
    client = await startServer({ cwd: project, env });
    const info = await client.request('/api/info');
    assert.ok(info);
    const catalog = await client.request('/api/model');
    assert.ok(catalog.data.some(model => model.providerID === 'fixture' && model.id === 'test'));
    assert.equal(requests, 0);
    const controller = new AbortController();
    const events = [];
    let connected;
    const connectedPromise = new Promise(resolve => { connected = resolve; });
    const stream = client.subscribe(event => events.push(event), connected, controller.signal);
    await Promise.race([connectedPromise, delay(5000).then(() => { throw Error('event stream did not connect'); })]);
    const session = (await client.request('/api/session', { method: 'POST', body: { title: 'Fixture', permissions: [{ action: 'bash', resource: 'fixture-restricted-command', effect: 'ask' }] } })).data;
    for (let i = 0; i < 30 && !events.some(event => event.type === 'session.created'); i++) await delay(100);
    controller.abort();
    await stream;
    assert.ok(events.some(event => event.type === 'session.created'));
    const path = `/api/session/${session.id}`;
    const variantModel = catalog.data.find(model => model.variants?.length);
    assert.ok(variantModel);
    const selected = { providerID: variantModel.providerID, id: variantModel.id, variant: variantModel.variants[0].id };
    await client.request(`${path}/model`, { method: 'POST', body: { model: selected } });
    assert.deepEqual((await client.request(path)).data.model, selected);
    const permission = (await client.request(`${path}/permission`, { method: 'POST', body: { action: 'bash', resources: ['fixture-restricted-command'] } })).data;
    assert.ok(permission.id);
    assert.equal(permission.effect, 'ask');
    {
      assert.ok((await client.request(`${path}/permission`)).data.some(request => request.id === permission.id));
      await client.request(`${path}/permission/${permission.id}/reply`, { method: 'POST', body: { decision: 'once' } });
      assert.equal((await client.request(`${path}/permission`)).data.length, 0);
    }
    await client.request(`${path}/model`, { method: 'POST', body: { model: { providerID: 'fixture', id: 'test' } } });
    assert.equal((await client.request(path)).data.model.id, 'test');
    await tmux('tmux', ['-L', socket, 'new-session', '-d', '-s', 'fixture', 'opencode', '--server', client.url, '--session', session.id], { cwd: project, env: { ...env, TERM: 'xterm-256color', OPENCODE_PASSWORD: client.password } });
    await delay(1000);
    await tmux('tmux', ['-L', socket, 'has-session', '-t', 'fixture'], { env });
    await tmux('tmux', ['-L', socket, 'kill-server'], { env });
    assert.equal(requests, 0);
    const shell = (await client.request('/api/shell', { method: 'POST', body: { command: 'if [ -n "${OPENCODE_PASSWORD+x}" ]; then echo exposed; else echo clean; fi', cwd: project } })).data;
    let output = '';
    for (let i = 0; i < 50; i++) {
      output = (await client.request(`/api/shell/${shell.id}/output`)).data.output;
      if (output.includes('clean') || output.includes('exposed')) break;
      await delay(100);
    }
    assert.match(output, /clean/);
    assert.doesNotMatch(output, /exposed/);
    const item = (await client.request(`${path}/prompt`, { method: 'POST', body: { text: 'Reply exactly once', resume: false } })).data;
    assert.match(item.id, /^msg_[A-Za-z0-9]+$/);
    assert.equal((await client.request(`${path}/inbox`)).data.length, 1);
    const other = (await client.request('/api/session', { method: 'POST', body: { title: 'Other fixture' } })).data;
    await assert.rejects(client.request(`/api/session/${other.id}/prompt`, { method: 'POST', body: { id: item.id, text: 'Other session', resume: false } }), error => error.status === 409);
    await client.close();
    client = await startServer({ cwd: project, env });
    assert.equal((await client.request(`${path}/inbox`)).data[0].id, item.id);
    const retry = { id: item.id, text: 'Reply exactly once' };
    assert.equal((await client.request(`${path}/prompt`, { method: 'POST', body: retry })).data.id, item.id);
    let messages;
    for (let i = 0; i < 100; i++) {
      messages = (await client.request(`${path}/message`)).data;
      if (messages.some(message => message.type === 'idle')) break;
      await delay(100);
    }
    assert.ok(messages.some(message => message.type === 'assistant' && message.content?.some(part => part.text === 'Fixture reply')));
    assert.equal(messages.filter(message => message.type === 'user' && message.id === item.id).length, 1);
    assert.equal((await client.messages(session.id)).filter(message => message.type === 'user' && message.id === item.id).length, 1);
    assert.equal((await client.request(`${path}/inbox`)).data.length, 0);
    await client.request(`${path}/prompt`, { method: 'POST', body: retry });
    await delay(500);
    assert.equal(requests, 1);

    // Exercise the bridge against the real server with a local stand-in for Claude.
    await tmux('tmux', ['-L', socket, 'new-session', '-d', '-s', 'fixture', 'opencode', '--server', client.url, '--session', session.id], { cwd: project, env: { ...env, TERM: 'xterm-256color', OPENCODE_PASSWORD: client.password } });
    const forwarded = [], remote = { sid: 'fixture-remote', event: async event => forwarded.push(event), state: async () => {} };
    const bridgeState = { sid: session.id, remote_inputs: {} };
    const bridge = new RemoteBridge(client, remote, bridgeState, async () => {});
    const bridgeStop = new AbortController();
    await bridge.connect(bridgeStop.signal);
    await bridge.handle({ type: 'user', uuid: 'remote-prompt', message: { content: 'Prompt from Claude fixture' } }, 'remote-event');
    for (let i = 0; i < 100 && requests < 2; i++) await delay(100);
    assert.equal(requests, 2);
    await client.request(`${path}/prompt`, { method: 'POST', body: { text: 'Prompt from native fixture' } });
    for (let i = 0; i < 100 && requests < 3; i++) await delay(100);
    assert.equal(requests, 3);
    for (let i = 0; i < 100 && !forwarded.some(event => event.message?.content?.some(part => part.text === 'Prompt from native fixture')); i++) await delay(100);
    assert.ok(forwarded.some(event => event.message?.content?.some(part => part.text === 'Prompt from native fixture')));
    const finalMessages = await client.messages(session.id);
    assert.equal(finalMessages.filter(message => message.type === 'user' && message.text === 'Prompt from Claude fixture').length, 1);
    assert.equal(finalMessages.filter(message => message.type === 'user' && message.text === 'Prompt from native fixture').length, 1);
    const waitFor = async condition => {
      for (let i = 0; i < 100; i++) { if (await condition()) return; await delay(100); }
      throw Error('Timed out waiting for OpenCode execution');
    };
    const active = async () => Boolean((await client.request('/api/session/active')).data?.[session.id]);
    await waitFor(async () => !await active());
    await client.request(`${path}/prompt`, { method: 'POST', body: { text: 'Interrupt from Claude fixture' } });
    await waitFor(async () => requests === 4 && await active());
    await bridge.handle({ type: 'control_request', request_id: 'stop-fixture', request: { subtype: 'interrupt' } });
    await waitFor(async () => !await active());
    assert.ok(forwarded.some(event => event.type === 'control_response' && event.response?.request_id === 'stop-fixture'));
    await client.request(`${path}/prompt`, { method: 'POST', body: { text: 'Interrupt from native fixture' } });
    await waitFor(async () => requests === 5 && await active());
    await client.request(`${path}/interrupt`, { method: 'POST', body: {} });
    await waitFor(async () => !await active());
    await tmux('tmux', ['-L', socket, 'has-session', '-t', 'fixture'], { env });
    bridgeStop.abort();
    await tmux('tmux', ['-L', socket, 'has-session', '-t', 'fixture'], { env });
  } finally {
    await tmux('tmux', ['-L', socket, 'kill-server'], { env }).catch(() => {});
    await client?.close();
    provider.closeAllConnections();
    await new Promise(resolve => provider.close(resolve));
    await rm(root, { recursive: true, force: true });
  }
});

test('OpenCode metadata-only lease does not start configured MCP servers', { skip: process.env.CDASH_OPENCODE_INTEGRATION !== '1', timeout: 15000 }, async () => {
  const root = await mkdtemp(join(tmpdir(), 'cdash-oc-mcp-'));
  const project = join(root, 'project');
  for (const dir of [project, join(root, 'home'), join(root, 'config'), join(root, 'data'), join(root, 'cache')]) await mkdir(dir, { recursive: true });
  const marker = join(root, 'mcp-started');
  const script = join(root, 'mcp.cjs');
  await writeFile(script, `require('fs').writeFileSync(${JSON.stringify(marker)}, process.env.OPENCODE_PASSWORD ? 'credential-present' : 'credential-absent'); setInterval(() => {}, 1000);`);
  await writeFile(join(project, 'opencode.json'), JSON.stringify({ model: 'fixture/test', providers: { fixture: { name: 'Fixture', package: '@opencode/ai/providers/openai-compatible', settings: { baseURL: 'http://127.0.0.1:1/v1', apiKey: 'fixture' }, models: { test: { name: 'Project model' } } } }, mcp: { servers: { fixture: { type: 'local', command: ['node', script] } } } }));
  const env = { PATH: process.env.PATH, HOME: join(root, 'home'), XDG_CONFIG_HOME: join(root, 'config'), XDG_DATA_HOME: join(root, 'data'), XDG_CACHE_HOME: join(root, 'cache'), NO_PROXY: '*', no_proxy: '*' };
  env.OPENCODE_CONFIG_CONTENT = JSON.stringify({ model: 'fixture/test', plugins: ['-opencode.provider.openai-context-caps'] });
  let client;
  try {
    client = await startServer({ cwd: project, env, metadataOnly: true });
    const models = await client.request('/api/model');
    assert.ok(models.data.some(model => model.providerID === 'fixture' && model.id === 'test'));
    const selected = await client.request('/api/model/default');
    assert.equal(selected.data.id, 'test');
    await delay(2000);
    const { readFile } = await import('node:fs/promises');
    const result = await readFile(marker, 'utf8').catch(error => error.code === 'ENOENT' ? 'not-started' : Promise.reject(error));
    assert.equal(result, 'not-started');
  } finally {
    await client?.close();
    await rm(root, { recursive: true, force: true });
  }
});
