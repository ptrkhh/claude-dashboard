import { test } from 'node:test';
import assert from 'node:assert/strict';
import { createServer } from 'node:http';
import { mkdtemp, writeFile, readFile, chmod, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { OpenCode, normalizeModels, startServer } from '../crates/agent/src/collect/opencode-api.mjs';

test('model catalog preserves provider and slash-containing model IDs and variants', () => {
  const [model] = normalizeModels({ data: [{ providerID: 'nvidia', id: 'deepseek-ai/deepseek-v4.1-flash', name: 'DeepSeek', variants: [{ id: 'fast' }, { id: 'deep' }] }] });
  assert.equal(model.value, 'opencode:nvidia/deepseek-ai/deepseek-v4.1-flash');
  assert.equal(model.model, 'nvidia/deepseek-ai/deepseek-v4.1-flash');
  assert.deepEqual(model.variants, ['fast', 'deep']);
});

test('private lease uses environment credential and ends on close', async () => {
  const dir = await mkdtemp(join(tmpdir(), 'cdash-oc-api-'));
  try {
    const script = join(dir, 'opencode');
    await writeFile(script, `#!/usr/bin/env node\nrequire('fs').writeFileSync(process.env.ARGS_FILE, JSON.stringify(process.argv.slice(2))); console.log(JSON.stringify({url:'http://127.0.0.1:12345'}));process.stdin.resume();`);
    await chmod(script, 0o755);
    const client = await startServer({ cwd: dir, env: { ...process.env, PATH: `${dir}:${process.env.PATH}`, ARGS_FILE: join(dir, 'args') } });
    const args = JSON.parse(await readFile(join(dir, 'args'), 'utf8'));
    assert.equal(args.includes('--service'), false);
    assert.equal(args.some(arg => arg.includes(client.password)), false);
    assert.equal(client.url, 'http://127.0.0.1:12345');
    assert.ok(client.password);
    const child = client.child;
    await client.close();
    assert.equal(child.exitCode, 0);
  } finally { await rm(dir, { recursive: true, force: true }); }
});

test('lease rejects non-loopback readiness and early exit', async () => {
  const dir = await mkdtemp(join(tmpdir(), 'cdash-oc-api-'));
  try {
    const script = join(dir, 'opencode');
    await writeFile(script, `#!/usr/bin/env node\nconsole.log(JSON.stringify({url:'http://example.com:12345'}));process.stdin.resume();`);
    await chmod(script, 0o755);
    const options = { cwd: dir, env: { ...process.env, PATH: `${dir}:${process.env.PATH}` } };
    await assert.rejects(startServer(options), /loopback/i);
    await writeFile(script, '#!/usr/bin/env node\nprocess.exit(1);');
    await assert.rejects(startServer(options), /exit/i);
  } finally { await rm(dir, { recursive: true, force: true }); }
});

test('HTTP errors never include credentials and attached close leaves server running', async () => {
  const server = createServer((req, res) => { res.writeHead(401); res.end('secret-password'); });
  await new Promise(resolve => server.listen(0, '127.0.0.1', resolve));
  try {
    const client = new OpenCode({ url: `http://127.0.0.1:${server.address().port}`, password: 'secret-password' });
    await assert.rejects(client.request('/api/info'), e => e.status === 401 && !e.message.includes('secret-password'));
    await client.close();
    assert.equal(server.listening, true);
  } finally { await new Promise(resolve => server.close(resolve)); }
});

test('event stream parses events and reconnects without exposing credentials', async () => {
  let connections = 0;
  const server = createServer((req, res) => {
    connections++;
    res.writeHead(200, { 'content-type': 'text/event-stream' });
    res.write('data: {"type":"one"}\n\ndata: malformed\n\ndata: {"type":"two"}\n\n');
  });
  await new Promise(resolve => server.listen(0, '127.0.0.1', resolve));
  try {
    const client = new OpenCode({ url: `http://127.0.0.1:${server.address().port}`, password: 'secret' });
    const controller = new AbortController();
    const events = [];
    await client.subscribe(event => { events.push(event.type); if (events.length === 2) controller.abort(); }, () => {}, controller.signal);
    assert.deepEqual(events, ['one', 'two']);
    assert.equal(connections, 1);
  } finally { server.closeAllConnections(); await new Promise(resolve => server.close(resolve)); }
});

test('requests reject redirects and malformed JSON without leaking response contents', async () => {
  const server = createServer((req, res) => {
    if (req.url.endsWith('/redirect')) { res.writeHead(302, { location: 'https://example.com' }); res.end(); }
    else { res.writeHead(200, { 'content-type': 'application/json' }); res.end('secret-password'); }
  });
  await new Promise(resolve => server.listen(0, '127.0.0.1', resolve));
  try {
    const client = new OpenCode({ url: `http://127.0.0.1:${server.address().port}`, password: 'secret-password' });
    await assert.rejects(client.request('/api/redirect'), /request failed/i);
    await assert.rejects(client.request('/api/json'), e => /invalid.*json/i.test(e.message) && !e.message.includes('secret-password'));
  } finally { server.closeAllConnections(); await new Promise(resolve => server.close(resolve)); }
});

test('message pagination follows opaque cursors in server order and rejects loops', async () => {
  const seen = [];
  const server = createServer((req, res) => {
    const url = new URL(req.url, 'http://localhost');
    seen.push(url.searchParams.toString());
    const cursor = url.searchParams.get('cursor');
    const page = cursor === null ? { data: [{ id: 'msg_z' }], cursor: { next: 'opaque+1' } } : cursor === 'opaque+1' ? { data: [{ id: 'msg_a' }], cursor: { next: 'opaque+2' } } : { data: [], cursor: { next: null } };
    if (url.pathname.includes('/loop/')) { page.data = [{ id: 'msg_loop' }]; page.cursor.next = 'repeat'; }
    res.writeHead(200, { 'content-type': 'application/json' }); res.end(JSON.stringify(page));
  });
  await new Promise(resolve => server.listen(0, '127.0.0.1', resolve));
  try {
    const client = new OpenCode({ url: `http://127.0.0.1:${server.address().port}`, password: 'fixture' });
    assert.deepEqual((await client.messages('ses_test')).map(message => message.id), ['msg_z', 'msg_a']);
    assert.match(seen[0], /order=asc/);
    assert.doesNotMatch(seen[1], /order=/);
    await assert.rejects(client.messages('loop', { maxPages: 10 }), /repeated.*cursor/i);
  } finally { server.closeAllConnections(); await new Promise(resolve => server.close(resolve)); }
});

test('lease escalates cleanup when its server ignores stdin and SIGTERM', { timeout: 10000 }, async () => {
  const dir = await mkdtemp(join(tmpdir(), 'cdash-oc-api-'));
  try {
    const script = join(dir, 'opencode');
    await writeFile(script, `#!/usr/bin/env node\nprocess.on('SIGTERM', () => {});process.stdin.resume();setInterval(() => {}, 1000);console.log(JSON.stringify({url:'http://127.0.0.1:12345'}));`);
    await chmod(script, 0o755);
    const client = await startServer({ cwd: dir, env: { ...process.env, PATH: `${dir}:${process.env.PATH}` } });
    const child = client.child;
    await client.close();
    assert.equal(child.signalCode, 'SIGKILL');
  } finally { await rm(dir, { recursive: true, force: true }); }
});

test('server without readiness times out and is terminated', { timeout: 15000 }, async () => {
  const dir = await mkdtemp(join(tmpdir(), 'cdash-oc-api-'));
  try {
    const script = join(dir, 'opencode');
    await writeFile(script, `#!/usr/bin/env node\nrequire('fs').writeFileSync(process.env.PID_FILE, String(process.pid));setInterval(() => {}, 1000);`);
    await chmod(script, 0o755);
    await assert.rejects(startServer({ cwd: dir, env: { ...process.env, PATH: `${dir}:${process.env.PATH}`, PID_FILE: join(dir, 'pid') } }), /readiness timeout/i);
    const pid = Number(await readFile(join(dir, 'pid'), 'utf8'));
    await new Promise(resolve => setTimeout(resolve, 100));
    assert.throws(() => process.kill(pid, 0), { code: 'ESRCH' });
  } finally { await rm(dir, { recursive: true, force: true }); }
});

test('invalid readiness also reaps a child that ignores SIGTERM', { timeout: 10000 }, async () => {
  const dir = await mkdtemp(join(tmpdir(), 'cdash-oc-api-'));
  try {
    const script = join(dir, 'opencode');
    await writeFile(script, `#!/usr/bin/env node\nrequire('fs').writeFileSync(process.env.PID_FILE, String(process.pid));process.on('SIGTERM', () => {});setInterval(() => {}, 1000);console.log(JSON.stringify({url:'http://example.com:12345'}));`);
    await chmod(script, 0o755);
    await assert.rejects(startServer({ cwd: dir, env: { ...process.env, PATH: `${dir}:${process.env.PATH}`, PID_FILE: join(dir, 'pid') } }), /loopback/i);
    const pid = Number(await readFile(join(dir, 'pid'), 'utf8'));
    try { assert.throws(() => process.kill(pid, 0), { code: 'ESRCH' }); }
    finally { try { process.kill(-pid, 'SIGKILL'); } catch {} }
  } finally { await rm(dir, { recursive: true, force: true }); }
});

test('rejects oversized API responses', async () => {
  const server = createServer((req, res) => { res.writeHead(200, { 'content-type': 'application/json' }); res.end('x'.repeat(9 * 1024 * 1024)); });
  await new Promise(resolve => server.listen(0, '127.0.0.1', resolve));
  try {
    const client = new OpenCode({ url: `http://127.0.0.1:${server.address().port}`, password: 'fixture' });
    await assert.rejects(client.request('/api/model'), /too large/i);
  } finally { server.closeAllConnections(); await new Promise(resolve => server.close(resolve)); }
});

test('event stream reconnects and reports each connection', async () => {
  let requests = 0, connections = 0;
  const server = createServer((req, res) => {
    requests++;
    res.writeHead(200, { 'content-type': 'text/event-stream' });
    res.end(`data: {"number":${requests}}\n\n`);
  });
  await new Promise(resolve => server.listen(0, '127.0.0.1', resolve));
  try {
    const client = new OpenCode({ url: `http://127.0.0.1:${server.address().port}`, password: 'fixture' });
    const controller = new AbortController();
    const numbers = [];
    await client.subscribe(event => { numbers.push(event.number); if (event.number === 2) controller.abort(); }, () => connections++, controller.signal);
    assert.deepEqual(numbers, [1, 2]);
    assert.equal(connections, 2);
  } finally { server.closeAllConnections(); await new Promise(resolve => server.close(resolve)); }
});

test('unexpected server exit notifies the owner without exposing credentials', { timeout: 3000 }, async () => {
  const dir = await mkdtemp(join(tmpdir(), 'cdash-oc-api-'));
  try {
    const script = join(dir, 'opencode');
    await writeFile(script, `#!/usr/bin/env node\nconsole.log(JSON.stringify({url:'http://127.0.0.1:12345'}));setTimeout(() => process.exit(1), 100);`);
    await chmod(script, 0o755);
    let notify;
    const notified = new Promise(resolve => { notify = resolve; });
    const client = await startServer({ cwd: dir, env: { ...process.env, PATH: `${dir}:${process.env.PATH}` }, onError: notify });
    const error = await notified;
    assert.doesNotMatch(error.message, new RegExp(client.password));
    await client.close();
  } finally { await rm(dir, { recursive: true, force: true }); }
});

test('metadata discovery refuses an unparseable inline config before spawning', async () => {
  await assert.rejects(startServer({ cwd: tmpdir(), metadataOnly: true, env: { PATH: '/nonexistent', OPENCODE_CONFIG_CONTENT: '{invalid' } }), /inline configuration|requires.*JSON/i);
});

test('event frames parse CRLF boundaries split across network chunks', { timeout: 3000 }, async () => {
  const server = createServer((req, res) => {
    res.writeHead(200, { 'content-type': 'text/event-stream' });
    res.write('data: {"type":"split"}\r');
    setTimeout(() => res.write('\n\r'), 20);
    setTimeout(() => res.write('\n'), 40);
  });
  await new Promise(resolve => server.listen(0, '127.0.0.1', resolve));
  try {
    const client = new OpenCode({ url: `http://127.0.0.1:${server.address().port}`, password: 'fixture' });
    const controller = new AbortController(), events = [];
    const timeout = setTimeout(() => controller.abort(), 300);
    await client.subscribe(event => { events.push(event.type); controller.abort(); }, () => {}, controller.signal);
    clearTimeout(timeout);
    assert.deepEqual(events, ['split']);
  } finally { server.closeAllConnections(); await new Promise(resolve => server.close(resolve)); }
});
