// Claude 2.1.296 worker transport. Authentication/registration stays in Claude.
// ponytail: one worker + one Codex thread per tmux pane; no daemon or packages.
import { spawn } from 'node:child_process';
import { randomUUID, createHash } from 'node:crypto';
import { createServer, createConnection } from 'node:net';
import { createInterface } from 'node:readline';
import { readFile, writeFile, rename, mkdir, chmod, rm, readdir, mkdtemp, open, access } from 'node:fs/promises';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { tmpdir, homedir } from 'node:os';

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
const codexEnv = () => Object.fromEntries(Object.entries(process.env).filter(([k]) => !k.startsWith('CLAUDE_') && !k.startsWith('CDASH_CODEX_')));

function killGroup(child) {
  if (!child?.pid) return;
  const kill = signal => {
    try { process.kill(-child.pid, signal); }
    catch (e) { if (e.code !== 'ESRCH') console.error(`Codex cleanup: ${e.message}`); }
  };
  kill('SIGTERM');
  setTimeout(() => kill('SIGKILL'), 250);
}

class Codex {
  pending = new Map();
  id = 0;
  turnEpoch = 0;
  constructor(onEvent, options = {}) {
    // Session ingress credentials must never enter Codex's tool environment.
    const onError = options.onError || fatal;
    const backend = process.env.CDASH_CODEX_BACKEND;
    const executable = backend ? process.env.CDASH_CODEX_AGENT : 'codex';
    if (!executable) throw Error('Native Codex transport executable is missing');
    this.child = spawn(executable, backend ? ['codex-proxy', backend] : ['app-server', '--listen', 'stdio://'], {
      env: options.env || codexEnv(), cwd: options.cwd, detached: true, stdio: ['pipe', 'pipe', options.quiet ? 'ignore' : 'inherit'],
    });
    let notifications = Promise.resolve();
    createInterface({ input: this.child.stdout }).on('line', line => {
      let m;
      try { m = JSON.parse(line); } catch { return; }
      const p = this.pending.get(m.id);
      if (!m.method && p) {
        if (this.turnEpoch === p.turnEpoch) {
          if (p.method === 'turn/start' && m.result?.turn?.status === 'inProgress') this.turnId = m.result.turn.id;
          if (p.method === 'thread/resume' && m.result?.initialTurnsPage) {
            this.turnId = m.result.thread.status?.type === 'active'
              ? m.result.initialTurnsPage.data.find(t => t.status === 'inProgress')?.id : undefined;
          }
        }
        clearTimeout(p.timer);
        this.pending.delete(m.id);
        if (m.error) p.reject(Object.assign(Error(m.error.message), { rpc: true })); else p.resolve(m.result);
      } else {
        // Track turn state before uploads: a slow HTTP reply must not leave a
        // stale turn ID steering the next prompt into an already completed turn.
        if (m.method === 'turn/started') { this.turnEpoch++; this.turnId = m.params.turn.id; }
        if (m.method === 'turn/completed') { this.turnEpoch++; if (this.turnId === m.params.turn.id) this.turnId = undefined; }
        notifications = notifications.then(() => onEvent(m));
        notifications.catch(onError);
      }
    });
    this.child.on('error', onError);
    this.child.stdin.on('error', onError);
    this.child.on('exit', () => {
      for (const p of this.pending.values()) { clearTimeout(p.timer); p.reject(Error('Codex app-server exited')); }
      this.pending.clear();
      if (!this.closing) onError(Error('Codex app-server exited'));
    });
  }
  send(m) { this.child.stdin.write(`${JSON.stringify(m)}\n`); }
  call(method, params, timeout = 120_000) {
    const id = ++this.id;
    return new Promise((resolve, reject) => {
      const timer = setTimeout(() => { this.pending.delete(id); reject(Object.assign(Error(`Codex ${method} timed out`), { rpc: true })); }, timeout);
      this.pending.set(id, { resolve, reject, timer, method, turnEpoch: this.turnEpoch });
      this.send({ id, method, params });
    });
  }
  async models() {
    const models = [], seen = new Set();
    let cursor;
    do {
      const page = await this.call('model/list', { limit: 100, includeHidden: false, ...(cursor ? { cursor } : {}) }, 10_000);
      models.push(...page.data.filter(m => !m.hidden).map(m => ({
        value: `codex:${m.model}`, model: m.model, label: m.displayName || m.model,
        efforts: m.supportedReasoningEfforts.map(e => e.reasoningEffort),
        defaultEffort: m.defaultReasoningEffort,
      })));
      cursor = page.nextCursor;
      if (cursor && seen.has(cursor)) throw Error('Repeated Codex model cursor');
      seen.add(cursor);
    } while (cursor);
    return models;
  }
  async ready() {
    const auth = await this.call('account/read', { refreshToken: false }, 10_000);
    return !auth.requiresOpenaiAuth || auth.account != null;
  }
  close() {
    if (this.closing) return;
    this.closing = true;
    killGroup(this.child); // npm's launcher can own a native child
  }
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
  state(worker_status, details = null) { return this.retry('/worker', 'PUT', { worker_epoch: this.epoch, worker_status, requires_action_details: details, external_metadata: { model: this.model || 'codex', pending_action: details, pending_actions: details ? [details] : [] } }); }
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

function messageUuid(remoteSid, threadId, turnId, itemId) {
  const hash = createHash('sha256').update(JSON.stringify([remoteSid, threadId, turnId, itemId])).digest('hex');
  return `${hash.slice(0, 8)}-${hash.slice(8, 12)}-8${hash.slice(13, 16)}-8${hash.slice(17, 20)}-${hash.slice(20, 32)}`;
}

async function replayHistory(codex, threadId, remoteSid, emit, submitted = new Set()) {
  let cursor;
  const seen = new Set();
  do {
    const page = await codex.call('thread/turns/list', {
      threadId, limit: 50, sortDirection: 'asc', itemsView: 'full', ...(cursor ? { cursor } : {}),
    });
    for (const turn of page.data) for (const item of turn.items) {
      const role = item.type === 'userMessage' ? 'user' : item.type === 'agentMessage' ? 'assistant' : null;
      if (!role || (role === 'user' && submitted.has(item.clientId))) continue;
      const content = role === 'user'
        ? item.content.filter(b => b.type === 'text').map(b => ({ type: 'text', text: b.text }))
        : [{ type: 'text', text: item.text }];
      if (!content.length) continue;
      // Stable UUIDs let the remote deduplicate a replay interrupted by a crash.
      const uuid = messageUuid(remoteSid, threadId, turn.id, item.id);
      await emit({ uuid, type: role, message: { id: item.id, role, content, ...(role === 'assistant' ? { stop_reason: 'end_turn' } : {}) } });
    }
    cursor = page.nextCursor;
    if (cursor && seen.has(cursor)) throw Error('Repeated Codex history cursor');
    seen.add(cursor);
  } while (cursor);
}

let stop = () => {};
function fatal(e) { console.error(`Codex bridge: ${e.message}${e.cause ? ` (${e.cause.code || e.cause.message})` : ''}`); stop(); setTimeout(() => process.exit(1), 5000).unref(); process.exitCode = 1; }

async function worker() {
  const statePath = process.env.CDASH_CODEX_STATE;
  const state = JSON.parse(await readFile(statePath, 'utf8'));
  const sid = option('--session-id');
  const remote = new Remote(option('--sdk-url'), process.env.CLAUDE_CODE_SESSION_ACCESS_TOKEN, Number(process.env.CLAUDE_CODE_WORKER_EPOCH), sid);
  rotateToken = token => { remote.token = token; };
  if (!remote.token || !Number.isSafeInteger(remote.epoch)) throw Error('Missing Claude worker credential or epoch');
  let closed = false, heartbeat, local;
  const approvals = new Map();
  const delegated = new Map();
  const submitted = new Set(state.remote_sid === sid ? state.remote_user_ids || [] : []);
  const items = new Map();
  let models = [];
  let outgoing = Promise.resolve(), saved = Promise.resolve(), queue = Promise.resolve(), savedNamePath;
  const emit = payload => { outgoing = outgoing.then(() => remote.event(payload)); outgoing.catch(fatal); return outgoing; };
  const approvalState = () => {
    const details = [...delegated.values(), ...[...approvals.values()].map(a => a.details)].at(-1);
    return remote.state(details ? 'requires_action' : state.working ? 'running' : 'idle', details);
  };
  const save = () => {
    const json = JSON.stringify(state);
    const namePath = /^[0-9a-f-]{36}$/i.test(state.sid || '') && state.display_name && join(dirname(statePath), `${state.sid}.name`);
    saved = saved.then(async () => {
      await atomic(statePath, json);
      if (namePath && namePath !== savedNamePath) {
        await atomic(namePath, state.display_name);
        savedNamePath = namePath;
      }
    });
    saved.catch(fatal);
    return saved;
  };
  const codex = new Codex(async m => {
    const p = m.params || {};
    if (m.method && m.id !== undefined) {
      if (['item/commandExecution/requestApproval', 'item/fileChange/requestApproval'].includes(m.method)) {
        const request_id = randomUUID(), item = items.get(p.itemId);
        const details = { tool_name: m.method.includes('commandExecution') ? 'Bash' : 'Edit', action_description: p.reason || 'Approve Codex tool execution', raw_command: p.command ?? item?.command, request_id, tool_use_id: p.itemId };
        approvals.set(request_id, { id: m.id, details });
        await approvalState();
        await emit({ type: 'control_request', request_id, request: {
          subtype: 'can_use_tool', tool_name: m.method.includes('commandExecution') ? 'Bash' : 'Edit',
          input: { command: p.command ?? item?.command, changes: item?.changes, reason: p.reason, network: p.networkApprovalContext },
          tool_use_id: p.itemId,
        } });
      } else if (state.native_terminal) {
        delegated.set(m.id, { tool_name: 'Terminal', action_description: 'Answer the pending question in Terminal.' });
        await approvalState();
        await emit({ type: 'assistant', message: { role: 'assistant', content: [{ type: 'text', text: 'Codex needs input in the native terminal.' }] } });
      } else {
        // Fail closed for unsupported input/permissions/elicitation requests.
        codex.send({ id: m.id, error: { code: -32601, message: `Unsupported bridge request: ${m.method}` } });
        await emit({ type: 'assistant', message: { role: 'assistant', content: [{ type: 'text', text: `Codex requested ${m.method}; this bridge cannot present it yet.` }] } });
      }
      return;
    }
    if (m.method === 'serverRequest/resolved') {
      delegated.delete(p.requestId);
      for (const [request_id, request] of approvals) if (request.id === p.requestId) {
        approvals.delete(request_id);
        await emit({ type: 'control_cancel_request', request_id });
      }
      await approvalState();
    }
    if (m.method === 'item/started') items.set(p.item.id, p.item);
    if (m.method === 'thread/settings/updated') {
      state.model = p.threadSettings.model;
      state.effort = p.threadSettings.effort;
      state.model_label = models.find(m => m.model === state.model)?.label || state.model;
      remote.model = state.model;
      await save();
    }
    if (m.method === 'turn/started') { state.working = true; await save(); await remote.state('running'); }
    if (m.method === 'item/completed') {
      const item = p.item;
      items.delete(item.id);
      if (item.type === 'userMessage') {
        if (!(state.native_terminal ? submitted.has(item.clientId) : submitted.delete(item.clientId))) {
          const content = item.content.filter(b => b.type === 'text').map(b => ({ type: 'text', text: b.text }));
          if (content.length) await emit({ uuid: messageUuid(sid, state.sid, p.turnId, item.id), type: 'user', message: { id: item.id, role: 'user', content } });
        }
      }
      if (item.type === 'agentMessage') {
        state.last_message = item.text;
        await emit({ uuid: messageUuid(sid, state.sid, p.turnId, item.id), type: 'assistant', message: { id: item.id, role: 'assistant', model: state.model, content: [{ type: 'text', text: item.text }], stop_reason: 'end_turn' } });
        if (!state.native_terminal) console.log(item.text);
        await save();
      }
    }
    if (m.method === 'turn/completed') {
      state.working = false; state.ts = Date.now();
      for (const request_id of approvals.keys()) await emit({ type: 'control_cancel_request', request_id });
      approvals.clear(); delegated.clear();
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
      const r = payload.response, request = approvals.get(r?.request_id);
      if (request) { approvals.delete(r.request_id); codex.send({ id: request.id, result: { decision: r.response?.behavior === 'allow' ? 'accept' : 'decline' } }); await approvalState(); }
    } else if (payload.type === 'control_request') {
      const r = payload.request;
      if (r.subtype === 'interrupt') {
        if (codex.turnId) await codex.call('turn/interrupt', { threadId: state.sid, turnId: codex.turnId });
        await respond(payload.request_id, {});
      } else if (r.subtype === 'initialize') await respond(payload.request_id, { commands: [], models: [{ value: state.model, displayName: state.model_label || state.model, description: 'Current session model' }] });
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
      state.ts = Date.now(); await save();
      const clientUserMessageId = payload.uuid || randomUUID();
      submitted.add(clientUserMessageId);
      if (state.native_terminal) {
        // UUIDs only: original RC user bubbles already exist in this remote.
        // Retain them while full history can be replayed after a reconnect.
        state.remote_user_ids = [...submitted];
        await save();
      } else if (submitted.size > 200) submitted.delete(submitted.values().next().value);
      if (codex.turnId) await codex.call('turn/steer', { threadId: state.sid, expectedTurnId: codex.turnId, input, clientUserMessageId });
      else await codex.call('turn/start', { threadId: state.sid, input, effort: state.effort, clientUserMessageId });
    }
  };
  const dispatch = async payload => {
    try { await handle(payload); }
    catch (e) {
      if (!e.rpc) throw e;
      await reportError(`Codex request failed: ${e.message}`);
    }
  };
  await codex.call('initialize', { clientInfo: { name: 'claude_dashboard', title: 'Claude Dashboard Codex Bridge', version: '0.5.1' }, ...(state.native_terminal ? { capabilities: { experimentalApi: true } } : {}) });
  codex.send({ method: 'initialized' });
  if (!await codex.ready()) throw Error('Codex is signed out; run codex login on the agent host.');
  const savedModel = state.model, savedLabel = state.model_label, resuming = !!state.sid;
  const result = await codex.call(state.sid ? 'thread/resume' : 'thread/start', {
    ...(state.sid ? { threadId: state.sid, excludeTurns: true,
      ...(state.native_terminal ? { initialTurnsPage: { limit: 1, itemsView: 'summary' } } : {}) } : {}), cwd: state.dir,
    ...(!state.native_terminal ? { ...(state.model ? { model: state.model } : {}), config: { model_reasoning_effort: state.effort } } : {}),
  });
  state.sid = result.thread.id; state.working = Boolean(codex.turnId);
  state.model = result.model;
  state.effort = result.reasoningEffort;
  try { models = await codex.models(); }
  catch { console.error('Model catalog unavailable; using model ID.'); }
  state.model_label = models.find(m => m.model === state.model)?.label || (state.model === savedModel && savedLabel) || state.model;
  remote.model = state.model;
  await save();
  if (state.remote_sid === sid) remote.sequence = state.remote_sequence || 0;
  await approvalState();
  heartbeat = setInterval(() => remote.retry('/worker/heartbeat', 'POST', { session_id: sid, worker_epoch: remote.epoch }).catch(fatal), 30_000);
  await emit({ type: 'system', subtype: 'init', cwd: state.dir, model: state.model, tools: [], mcp_servers: [], permissionMode: 'default', slash_commands: [] });
  if (resuming && (state.native_terminal || state.remote_sid !== sid)) await replayHistory(codex, state.sid, sid, emit, submitted);
  state.remote_user_ids = [...submitted];
  // Commit the remote identity only after every history upload succeeds.
  state.remote_sid = sid; state.remote_sequence = remote.sequence; await save();
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

async function nativeLauncher(statePath, state, script) {
  const dir = await mkdtemp(join(tmpdir(), 'cdash-codex-native-'));
  const endpoint = join(dir, 'backend.sock');
  let backend, runner, tui, control, log, cleanup, remoteRetry;
  let closing = false;
  stop = () => {
    if (cleanup) return cleanup;
    closing = true;
    clearTimeout(remoteRetry);
    cleanup = (async () => {
      // Interrupt tools through their owner before terminating the backend.
      if (control?.turnId) {
        const turnId = control.turnId;
        try {
          await control.call('turn/interrupt', { threadId: state.sid, turnId }, 2000);
          for (let i = 0; i < 20 && control.turnId === turnId; i++) await sleep(50);
        }
        catch { /* the backend may already have exited */ }
      }
      tui?.kill('SIGTERM');
      killGroup(runner);
      control?.close();
      killGroup(backend);
      process.stdin.pause();
      try { if (process.stdin.isTTY) process.stdin.setRawMode(false); } catch { /* tmux may have closed the PTY */ }
      await rm(dir, { recursive: true, force: true });
      await log?.close();
    })();
    cleanup.catch(e => { console.error(`Codex cleanup: ${e.message}`); process.exitCode = 1; });
    return cleanup;
  };
  const controlDir = join(dirname(statePath), 'controls', option('--name'));
  await mkdir(controlDir, { recursive: true });
  log = await open(join(controlDir, 'native.log'), 'a', 0o600);
  // Buffer input without echo while the native TUI is starting.
  if (process.stdin.isTTY) process.stdin.setRawMode(true);
  backend = spawn('codex', ['app-server', '--listen', `unix://${endpoint}`], {
    cwd: state.dir, env: codexEnv(), detached: true, stdio: ['ignore', log.fd, log.fd],
  });
  backend.on('error', fatal);
  backend.on('exit', code => { if (!closing) fatal(Error(`Codex backend exited (${code})`)); });
  let ready = false;
  for (let i = 0; i < 100 && !closing; i++) {
    ready = await access(endpoint).then(() => true, () => false);
    if (ready) break;
    await sleep(100);
  }
  if (!ready || closing) throw Error('Codex backend did not start');
  process.env.CDASH_CODEX_BACKEND = endpoint;
  // The supervisor owns native metadata independently of RC's worker state.
  // Separate files avoid two processes overwriting each other's updates.
  let nativeSaved = Promise.resolve();
  const saveNative = () => {
    const json = JSON.stringify(state);
    nativeSaved = nativeSaved.then(() => atomic(statePath.replace(/\.json$/, '.native'), json));
    return nativeSaved;
  };
  control = new Codex(async m => {
    const p = m.params || {};
    if (m.method === 'thread/settings/updated') {
      state.model = p.threadSettings.model;
      state.effort = p.threadSettings.effort;
      state.model_label = state.model;
    } else if (m.method === 'turn/started') state.working = true;
    else if (m.method === 'turn/completed') { state.working = false; state.ts = Date.now(); }
    else if (m.method === 'item/completed' && p.item.type === 'agentMessage') state.last_message = p.item.text;
    else if (m.method === 'item/completed' && p.item.type === 'userMessage') {
      state.ts = Date.now();
    } else return;
    await saveNative();
  }); // also tracks the active turn for shutdown; never answers approvals
  await control.call('initialize', { clientInfo: { name: 'claude_dashboard', version: '0.5.1' }, capabilities: { experimentalApi: true } });
  control.send({ method: 'initialized' });
  if (!await control.ready()) throw Error('Codex is signed out; run codex login on the agent host.');
  const result = await control.call(state.sid ? 'thread/resume' : 'thread/start', {
    ...(state.sid ? { threadId: state.sid, excludeTurns: true, initialTurnsPage: { limit: 1, itemsView: 'summary' } } : {}), cwd: state.dir,
    ...(state.model ? { model: state.model } : {}), config: { model_reasoning_effort: state.effort },
  });
  state.sid = result.thread.id;
  state.model = result.model;
  state.effort = result.reasoningEffort;
  state.working = Boolean(control.turnId);
  // Naming persists an empty thread, so both clients can immediately resume it.
  await control.call('thread/name/set', { threadId: state.sid, name: state.display_name });
  await atomic(statePath, JSON.stringify(state));
  await saveNative();
  tui = spawn('codex', ['resume', '--remote', `unix://${endpoint}`, '-c', 'tui.fullscreen_transcript=true', '-c', 'tui.alternate_screen="always"', state.sid], {
    cwd: state.dir, env: codexEnv(), stdio: 'inherit',
  });
  tui.on('error', fatal);
  tui.on('exit', code => { if (!closing) { process.exitCode = code ?? 1; stop(); } });
  const debugPath = join(controlDir, 'remote-debug.log');
  await writeFile(debugPath, '', { flag: 'a', mode: 0o600 });
  await chmod(debugPath, 0o600);
  let failures = 0, reuseRemote = false;
  const startRemote = () => {
    if (closing) return;
    const child = runner = spawn('claude', ['remote-control', ...(reuseRemote ? ['--continue'] : ['--spawn=session']), '--name', state.display_name,
      '--debug-file', debugPath], {
      cwd: controlDir, detached: true, stdio: ['ignore', log.fd, log.fd],
      env: { ...process.env, CLAUDE_CODE_PROCESS_WRAPPER: JSON.stringify([process.execPath, script, '--worker']) },
    });
    let failure;
    child.once('error', e => { failure = e.code || 'spawn failed'; });
    child.once('close', (code, signal) => {
      if (closing) return;
      void (async () => {
        killGroup(child);
        const link = statePath.replace(/\.json$/, '') + '.link';
        const connected = await access(link).then(() => true, () => false);
        reuseRemote ||= connected;
        await rm(link, { force: true });
        // Remote Control can exit 0 after a worker failure; only shutdown is intentional.
        // Retry for as long as the native session lives: an outage can outlast any
        // fixed budget, and an established connection earns a fresh backoff.
        // ponytail: a permanent failure (signed out, no claude binary) respawns once a minute.
        if (connected) failures = 0;
        const delay = Math.min(1000 * 2 ** failures, 60_000);
        failures++;
        state.rc_unavailable = failures > 3;
        await saveNative();
        await log.write(`${new Date().toISOString()} Remote Control failure ${failures} (${failure ? `spawn error ${failure}; ` : ''}exit ${code}, signal ${signal || 'none'}); retrying in ${delay}ms; native terminal remains available.\n`);
        if (!closing) remoteRetry = setTimeout(startRemote, delay);
      })().catch(e => { if (!closing) fatal(e); });
    });
  };
  startRemote();
}

async function launcher() {
  const statePath = resolve(option('--state'));
  const state = JSON.parse(await readFile(statePath, 'utf8'));
  state.pid = process.pid;
  state.rc_unavailable = false;
  await atomic(statePath, JSON.stringify(state));
  process.env.CDASH_CODEX_STATE = statePath;
  const script = fileURLToPath(import.meta.url);
  if (state.native_terminal) return nativeLauncher(statePath, state, script);
  const runner = spawn('claude', ['remote-control', '--spawn=session', '--name', state.display_name || `[Codex] ${state.dir.split('/').filter(Boolean).pop() || state.dir}`, '--verbose'], {
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
const queryMode = ['--models', '--usage'].find(mode => process.argv.includes(mode));
if (args[0] === '--summary') {
  import('./summary.mjs').then(({ summarize }) => summarize(JSON.parse(args[1]), Codex, fn => { stop = fn; }))
    .then(value => console.log(JSON.stringify(value)), () => { console.log(JSON.stringify({ error: 'Session summary unavailable' })); });
} else if (process.argv.includes('--threads')) {
  // Resumable dashboard threads, read from Codex's own state database: it is
  // what Codex itself records per thread, so a thread continued elsewhere
  // shows its real model. Read-only; Codex owns the file.
  // ponytail: `node:sqlite` keeps the agent free of a sqlite dependency; if
  // Codex's schema moves, this reports an error and the list simply has no Codex rows.
  (async () => {
    const home = process.env.CODEX_HOME || join(homedir(), '.codex');
    // The number is Codex's schema generation; the newest one is the live one.
    const files = (await readdir(home).catch(() => [])).map(f => /^state_(\d+)\.sqlite$/.exec(f)).filter(Boolean);
    if (!files.length) return [];
    const { DatabaseSync } = await import('node:sqlite');
    const db = new DatabaseSync(join(home, files.sort((a, b) => b[1] - a[1])[0][0]), { readOnly: true });
    try {
      return db.prepare(`select id as sid, cwd as dir,
          updated_at_ms as ts, model, reasoning_effort as effort, rollout_path
        from threads
        where originator = 'claude_dashboard' and archived = 0 and coalesce(nullif(name, ''), title) <> ''
        order by updated_at_ms desc`).all();
    } finally { db.close(); }
  })().then(rows => console.log(JSON.stringify(rows)), () => console.log(JSON.stringify({ error: 'Codex threads unavailable' })));
} else if (queryMode) {
  const codex = new Codex(async () => {});
  stop = () => codex.close();
  let timedOut = false;
  // Startup, account lookup, and the quota/catalog RPC share this budget.
  const deadline = setTimeout(() => { timedOut = true; codex.close(); }, 12_000);
  (async () => {
    try {
      await codex.call('initialize', { clientInfo: { name: 'claude_dashboard', version: '0.5.1' } }, 10_000);
      codex.send({ method: 'initialized' });
      let result;
      if (queryMode === '--usage') {
        const auth = await codex.call('account/read', { refreshToken: false }, 10_000);
        // API keys and local/third-party providers have no ChatGPT quota windows.
        result = auth.account?.type === 'chatgpt' ? await codex.call('account/rateLimits/read', {}, 10_000) : null;
      } else result = await codex.ready() ? await codex.models() : [];
      console.log(JSON.stringify(result));
    } finally { clearTimeout(deadline); codex.close(); }
  })().catch(e => {
    // Report only our own timeout text; provider error bodies may contain credentials.
    const error = timedOut ? `Codex metadata ${queryMode} timed out after 12000ms`
      : /^Codex [\w/]+ timed out$/.test(e.message) ? e.message : `Codex metadata ${queryMode} failed`;
    console.log(JSON.stringify({ error }));
    process.exitCode = 0; // Structured errors are checked by the agent.
  });
} else if (args[0] === '--worker') {
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
