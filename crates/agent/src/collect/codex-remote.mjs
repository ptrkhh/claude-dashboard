// Claude 2.1.296 worker transport. Authentication/registration stays in Claude.
// ponytail: one worker + one Codex thread per tmux pane; no daemon or packages.
import { spawn } from 'node:child_process';
import { randomUUID, createHash } from 'node:crypto';
import { createServer, createConnection } from 'node:net';
import { createInterface } from 'node:readline';
import { readFile, writeFile, rename, mkdir, chmod, rm } from 'node:fs/promises';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { tmpdir } from 'node:os';

const sleep = ms => new Promise(r => setTimeout(r, ms));
const args = process.argv.slice(2);
const option = key => args[args.indexOf(key) + 1];
let rotateToken = () => {};
const atomic = async (path, data) => {
  const tmp = `${path}.${process.pid}.tmp`;
  await writeFile(tmp, data, { mode: 0o600 });
  await rename(tmp, path);
};
const socketDir = state => join(tmpdir(), `cdash-codex-${process.getuid()}-${createHash('sha256').update(state).digest('hex').slice(0, 16)}`);
const socketPath = state => join(socketDir(state), 'input.sock');

class Codex {
  pending = new Map();
  id = 0;
  constructor(onEvent) {
    // Session ingress credentials must never enter Codex's tool environment.
    const env = Object.fromEntries(Object.entries(process.env).filter(([k]) => !k.startsWith('CLAUDE_') && !k.startsWith('CDASH_CODEX_')));
    this.child = spawn('codex', ['app-server', '--listen', 'stdio://'], { env, stdio: ['pipe', 'pipe', 'inherit'] });
    let notifications = Promise.resolve();
    createInterface({ input: this.child.stdout }).on('line', line => {
      let m;
      try { m = JSON.parse(line); } catch { return; }
      const p = this.pending.get(m.id);
      if (!m.method && p) {
        if (p.method === 'turn/start' && m.result?.turn?.status === 'inProgress') this.turnId = m.result.turn.id;
        clearTimeout(p.timer);
        this.pending.delete(m.id);
        if (m.error) p.reject(Object.assign(Error(m.error.message), { rpc: true })); else p.resolve(m.result);
      } else {
        // Track turn state before uploads: a slow HTTP reply must not leave a
        // stale turn ID steering the next prompt into an already completed turn.
        if (m.method === 'turn/started') this.turnId = m.params.turn.id;
        if (m.method === 'turn/completed' && this.turnId === m.params.turn.id) this.turnId = undefined;
        notifications = notifications.then(() => onEvent(m));
        notifications.catch(fatal);
      }
    });
    this.child.on('error', fatal);
    this.child.on('exit', () => {
      for (const p of this.pending.values()) { clearTimeout(p.timer); p.reject(Error('Codex app-server exited')); }
      this.pending.clear();
      if (!this.closing) fatal(Error('Codex app-server exited'));
    });
  }
  send(m) { this.child.stdin.write(`${JSON.stringify(m)}\n`); }
  call(method, params) {
    const id = ++this.id;
    return new Promise((resolve, reject) => {
      const timer = setTimeout(() => { this.pending.delete(id); reject(Object.assign(Error(`Codex ${method} timed out`), { rpc: true })); }, 120_000);
      this.pending.set(id, { resolve, reject, timer, method });
      this.send({ id, method, params });
    });
  }
  close() { this.closing = true; this.child.kill('SIGTERM'); }
}

class Remote {
  sequence = 0;
  sent = new Set();
  stopped = new AbortController();
  constructor(url, token, epoch, sid) {
    const u = new URL(url);
    if (u.protocol !== 'https:' || u.username || u.password || !/^\/v1\/code\/sessions\/[^/]+$/.test(u.pathname)) throw Error('Unexpected Claude worker URL');
    this.url = url.replace(/\/$/, '');
    this.token = token;
    this.epoch = epoch;
    this.sid = sid;
  }
  async request(path, method, body, stream = false) {
    const signal = stream ? this.stopped.signal : AbortSignal.any([this.stopped.signal, AbortSignal.timeout(15_000)]);
    const response = await fetch(`${this.url}${path}`, {
      method, signal,
      redirect: 'error',
      headers: { Authorization: `Bearer ${this.token}`, 'Content-Type': 'application/json', 'anthropic-version': '2023-06-01' },
      ...(body === undefined ? {} : { body: JSON.stringify(body) }),
    });
    // Do not quote response bodies: a transport failure must not leak tokens.
    if (!response.ok) { await response.body?.cancel(); throw Object.assign(Error(`Claude worker ${path.split('?')[0]}: HTTP ${response.status}`), { status: response.status }); }
    if (stream) return response;
    await response.body?.cancel();
  }
  async retry(path, method, body) {
    for (let attempt = 0; ; attempt++) {
      try { return await this.request(path, method, body); }
      catch (e) {
        if (this.stopped.signal.aborted || (e.status >= 400 && e.status < 500 && e.status !== 429) || attempt >= 5) throw e;
        await sleep(Math.min(1000 * 2 ** attempt, 30_000));
      }
    }
  }
  state(worker_status, details = null) { return this.retry('/worker', 'PUT', { worker_epoch: this.epoch, worker_status, requires_action_details: details, external_metadata: { model: 'codex', pending_action: details, pending_actions: details ? [details] : [] } }); }
  event(payload) {
    payload = { session_id: this.sid, uuid: randomUUID(), ...payload };
    this.sent.add(payload.uuid);
    if (this.sent.size > 200) this.sent.delete(this.sent.values().next().value);
    // Retry the same UUID so the service can deduplicate a lost HTTP response.
    return this.retry('/worker/events', 'POST', { worker_epoch: this.epoch, events: [{ payload }] });
  }
  ack(id, status) { return this.retry('/worker/events/delivery', 'POST', { worker_epoch: this.epoch, updates: [{ event_id: id, status }] }); }
  async listen(onEvent, onSequence) {
    let delay = 1000;
    while (!this.stopped.signal.aborted) {
      try {
        const response = await this.request(`/worker/events/stream?from_sequence_num=${this.sequence}`, 'GET', undefined, true);
        let frame = '', kind = '', data = [];
        for await (const chunk of response.body.pipeThrough(new TextDecoderStream())) {
          frame += chunk;
          if (frame.length > 16 * 1024 * 1024) throw Error('Claude SSE frame too large');
          let pos;
          while ((pos = frame.indexOf('\n')) >= 0) {
            const line = frame.slice(0, pos).replace(/\r$/, '');
            frame = frame.slice(pos + 1);
            if (line.startsWith('event:')) kind = line.slice(6).trim();
            else if (line.startsWith('data:')) data.push(line.slice(5).trimStart());
            else if (!line) {
              if (kind === 'client_event' && data.length) {
                const e = JSON.parse(data.join('\n'));
                const sequence = Number(e.sequence_num);
                if (!Number.isSafeInteger(sequence) || sequence < 0) throw Error('Invalid Claude SSE sequence');
                if (sequence > this.sequence) {
                  if (e.source !== 'worker' && !this.sent.has(e.payload?.uuid)) await onEvent(e);
                  this.sequence = sequence;
                  await onSequence(sequence);
                }
              }
              kind = ''; data = []; delay = 1000;
            }
          }
        }
      } catch (e) {
        if (this.stopped.signal.aborted) break;
        if ([401, 403, 404, 409, 410].includes(e.status)) throw e;
        console.error(`${e.message}; reconnecting`);
      }
      await sleep(delay);
      delay = Math.min(delay * 2, 30_000);
    }
  }
}

let stop = () => {};
function fatal(e) { console.error(`Codex bridge: ${e.message}`); stop(); setTimeout(() => process.exit(1), 5000).unref(); process.exitCode = 1; }

async function worker() {
  const statePath = process.env.CDASH_CODEX_STATE;
  const state = JSON.parse(await readFile(statePath, 'utf8'));
  const sid = option('--session-id');
  const remote = new Remote(option('--sdk-url'), process.env.CLAUDE_CODE_SESSION_ACCESS_TOKEN, Number(process.env.CLAUDE_CODE_WORKER_EPOCH), sid);
  rotateToken = token => { remote.token = token; };
  if (!remote.token || !Number.isSafeInteger(remote.epoch)) throw Error('Missing Claude worker credential or epoch');
  let closed = false, heartbeat, local;
  const approvals = new Map();
  const items = new Map();
  let outgoing = Promise.resolve(), saved = Promise.resolve(), queue = Promise.resolve();
  const emit = payload => { outgoing = outgoing.then(() => remote.event(payload)); outgoing.catch(fatal); return outgoing; };
  const save = () => {
    const json = JSON.stringify(state);
    const history = state.sid && join(dirname(statePath), 'history', `${state.sid}.json`);
    saved = saved.then(async () => { await atomic(statePath, json); if (history) await atomic(history, json); });
    saved.catch(fatal);
    return saved;
  };
  const codex = new Codex(async m => {
    const p = m.params || {};
    if (m.method && m.id !== undefined) {
      if (['item/commandExecution/requestApproval', 'item/fileChange/requestApproval'].includes(m.method)) {
        const request_id = randomUUID(), item = items.get(p.itemId);
        approvals.set(request_id, m.id);
        await remote.state('requires_action', { tool_name: m.method.includes('commandExecution') ? 'Bash' : 'Edit', action_description: p.reason || 'Approve Codex tool execution', raw_command: p.command ?? item?.command, request_id, tool_use_id: p.itemId });
        await emit({ type: 'control_request', request_id, request: {
          subtype: 'can_use_tool', tool_name: m.method.includes('commandExecution') ? 'Bash' : 'Edit',
          input: { command: p.command ?? item?.command, changes: item?.changes, reason: p.reason, network: p.networkApprovalContext },
          tool_use_id: p.itemId,
        } });
      } else {
        // Fail closed for unsupported input/permissions/elicitation requests.
        codex.send({ id: m.id, error: { code: -32601, message: `Unsupported bridge request: ${m.method}` } });
        await emit({ type: 'assistant', message: { role: 'assistant', content: [{ type: 'text', text: `Codex requested ${m.method}; this bridge cannot present it yet.` }] } });
      }
      return;
    }
    if (m.method === 'item/started') items.set(p.item.id, p.item);
    if (m.method === 'turn/started') { state.working = true; await save(); await remote.state('running'); }
    if (m.method === 'item/completed') {
      const item = p.item;
      items.delete(item.id);
      if (item.type === 'agentMessage') {
        state.last_message = item.text;
        await emit({ type: 'assistant', message: { id: item.id, role: 'assistant', model: 'codex', content: [{ type: 'text', text: item.text }], stop_reason: 'end_turn' } });
        console.log(item.text);
        await save();
      }
    }
    if (m.method === 'turn/completed') {
      state.working = false; state.ts = Date.now(); approvals.clear();
      await save();
      const failed = p.turn.status !== 'completed';
      await emit({ type: 'result', subtype: failed ? 'error_during_execution' : 'success', is_error: failed, result: state.last_message || '', errors: failed ? [p.turn.error?.message || `Codex turn ${p.turn.status}`] : [], num_turns: 1, total_cost_usd: 0, duration_ms: 0, duration_api_ms: 0 });
      await remote.state('idle');
    }
  });
  stop = () => {
    if (closed) return;
    closed = true; clearInterval(heartbeat); remote.stopped.abort(); codex.close(); local?.close();
    rm(socketDir(statePath), { recursive: true, force: true }).catch(() => {});
  };
  const respond = (request_id, response, error) => emit({ type: 'control_response', response: { subtype: error ? 'error' : 'success', request_id, ...(error ? { error } : { response }) } });
  const reportError = async text => {
    console.error(text);
    await emit({ type: 'assistant', message: { role: 'assistant', content: [{ type: 'text', text }] } });
    await emit({ type: 'result', subtype: 'error_during_execution', is_error: true, errors: [text], result: text });
  };
  const handle = async payload => {
    if (payload.type === 'control_response') {
      const r = payload.response, id = approvals.get(r?.request_id);
      if (id !== undefined) { approvals.delete(r.request_id); codex.send({ id, result: { decision: r.response?.behavior === 'allow' ? 'accept' : 'decline' } }); await remote.state('running'); }
    } else if (payload.type === 'control_request') {
      const r = payload.request;
      if (r.subtype === 'interrupt') {
        if (codex.turnId) await codex.call('turn/interrupt', { threadId: state.sid, turnId: codex.turnId });
        await respond(payload.request_id, {});
      } else if (r.subtype === 'initialize') await respond(payload.request_id, { commands: [], models: [{ value: 'codex', displayName: 'Codex', description: 'Configured Codex model' }] });
      else if (r.subtype === 'set_env') await respond(payload.request_id, {}, 'Environment changes are unsupported');
      else await respond(payload.request_id, {}, `Unsupported Codex bridge control: ${r.subtype}`);
    } else if (payload.type === 'user') {
      const content = payload.message?.content;
      const blocks = typeof content === 'string' ? [{ type: 'text', text: content }] : content;
      if (!Array.isArray(blocks) || blocks.some(b => b?.type !== 'text' || typeof b.text !== 'string')) {
        await reportError('Codex bridge currently accepts text prompts only.');
        return;
      }
      const input = blocks.map(b => ({ type: 'text', text: b.text, text_elements: [] }));
      if (!input.some(b => b.text.trim())) return;
      state.title ||= input.map(b => b.text).join(' ').slice(0, 200);
      state.ts = Date.now(); await save();
      if (codex.turnId) await codex.call('turn/steer', { threadId: state.sid, expectedTurnId: codex.turnId, input });
      else await codex.call('turn/start', { threadId: state.sid, input, effort: state.effort });
    }
  };
  const dispatch = async payload => {
    try { await handle(payload); }
    catch (e) {
      if (!e.rpc) throw e;
      await reportError(`Codex request failed: ${e.message}`);
    }
  };
  await codex.call('initialize', { clientInfo: { name: 'claude_dashboard', title: 'Claude Dashboard Codex Bridge', version: '0.3.0' } });
  codex.send({ method: 'initialized' });
  const result = await codex.call(state.sid ? 'thread/resume' : 'thread/start', { ...(state.sid ? { threadId: state.sid, excludeTurns: true } : {}), cwd: state.dir });
  state.sid = result.thread.id; state.working = false; await save();
  if (state.remote_sid === sid) remote.sequence = state.remote_sequence || 0;
  else { state.remote_sid = sid; state.remote_sequence = 0; await save(); }
  await remote.state('idle');
  await emit({ type: 'system', subtype: 'init', cwd: state.dir, model: 'codex', tools: [], mcp_servers: [], permissionMode: 'default', slash_commands: [] });
  const rcId = sid.replace(/^cse_/, 'session_');
  await atomic(`${statePath.replace(/\.json$/, '')}.link`, `https://claude.ai/code/${rcId}`);
  const dir = socketDir(statePath);
  await mkdir(dir, { recursive: true, mode: 0o700 }); await chmod(dir, 0o700);
  await rm(socketPath(statePath), { force: true });
  local = createServer(socket => {
    socket.setEncoding('utf8');
    let text = '';
    socket.on('data', b => { text += b; if (text.length > 16_384) socket.destroy(); });
    socket.on('end', () => {
      if (!text.trim()) return;
      queue = queue.then(async () => {
        const payload = { type: 'user', uuid: randomUUID(), message: { role: 'user', content: text.trim() } };
        await emit(payload); await dispatch(payload);
      });
      queue.catch(fatal);
    });
  });
  await new Promise((r, j) => { local.once('error', j); local.listen(socketPath(statePath), r); });
  await chmod(socketPath(statePath), 0o600);
  heartbeat = setInterval(() => remote.retry('/worker/heartbeat', 'POST', { session_id: sid, worker_epoch: remote.epoch }).catch(fatal), 30_000);
  console.log(`Codex thread ${state.sid} ready`);
  await remote.listen(e => {
    queue = queue.then(async () => {
      await remote.ack(e.event_id, 'received');
      // Mark before dispatch: a restarted worker must not re-run old prompts.
      // ponytail: crash during dispatch can lose this prompt; resend it explicitly.
      state.remote_sequence = Number(e.sequence_num); await save();
      await dispatch(e.payload);
      await remote.ack(e.event_id, 'processed');
    });
    return queue;
  }, sequence => { state.remote_sequence = sequence; return save(); });
}

async function launcher() {
  const statePath = resolve(option('--state'));
  const state = JSON.parse(await readFile(statePath, 'utf8'));
  state.pid = process.pid;
  await atomic(statePath, JSON.stringify(state));
  process.env.CDASH_CODEX_STATE = statePath;
  const script = fileURLToPath(import.meta.url);
  const runner = spawn('claude', ['remote-control', '--spawn=session', '--name', `Codex: ${option('--name')}`, '--verbose'], {
    cwd: join(dirname(statePath), 'controls', option('--name')),
    env: { ...process.env, CLAUDE_CODE_PROCESS_WRAPPER: JSON.stringify([process.execPath, script, '--worker']) },
    stdio: ['pipe', 'pipe', 'pipe'],
  });
  stop = () => { runner.kill('SIGTERM'); };
  runner.on('error', fatal);
  runner.on('exit', code => { process.exit(code || 0); });
  for (const stream of [runner.stdout, runner.stderr]) createInterface({ input: stream }).on('line', line => {
    const cleaned = line.replace(/\x1b\[[0-?]*[ -/]*[@-~]/g, '');
    if (cleaned) console.log(cleaned);
  });
  createInterface({ input: process.stdin }).on('line', text => {
    if (!text.trim()) return;
    if (text === '/quit') { stop(); return; }
    const socket = createConnection(socketPath(statePath));
    socket.on('connect', () => socket.end(text));
    socket.on('error', () => console.error('Codex worker not ready; retry prompt after ready message.'));
  });
  console.log('Codex bridge: type prompts here or open Claude Remote Control. /quit stops session.');
}

for (const signal of ['SIGTERM', 'SIGINT', 'SIGHUP']) process.on(signal, () => { stop(); setTimeout(() => process.exit(0), 5000).unref(); });
if (args[0] === '--worker') {
  // Claude may wrap other self-spawns too; only replace its authenticated RC child.
  if (args.includes('--sdk-url') && process.env.CLAUDE_CODE_SESSION_ACCESS_TOKEN) {
    createInterface({ input: process.stdin }).on('line', line => {
      try {
        const m = JSON.parse(line);
        if (m.type === 'update_environment_variables' && typeof m.variables?.CLAUDE_CODE_SESSION_ACCESS_TOKEN === 'string') {
          process.env.CLAUDE_CODE_SESSION_ACCESS_TOKEN = m.variables.CLAUDE_CODE_SESSION_ACCESS_TOKEN;
          rotateToken(m.variables.CLAUDE_CODE_SESSION_ACCESS_TOKEN);
        }
      } catch {}
    });
    worker().catch(fatal);
  } else {
    const child = spawn(args[1], args.slice(2), { stdio: 'inherit' });
    stop = () => child.kill('SIGTERM');
    child.on('error', fatal);
    child.on('exit', code => process.exit(code || 0));
  }
} else launcher().catch(fatal);
