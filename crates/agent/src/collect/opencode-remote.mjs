import { randomBytes, createHash } from 'node:crypto';
import { spawn } from 'node:child_process';
import { stat, readFile, writeFile, rename, readdir, mkdir, open, rm } from 'node:fs/promises';
import { isAbsolute, resolve, join, dirname } from 'node:path';
import { pathToFileURL } from 'node:url';
import { OpenCode, startServer, normalizeModels } from './opencode-api.mjs';

function inputID() {
  const chars = '0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz';
  const time = (BigInt(Date.now()) * 4096n).toString(16).slice(-12).padStart(12, '0');
  return `msg_${time}${Array.from(randomBytes(14), byte => chars[byte % chars.length]).join('')}`;
}

export async function submitPrompt(client, state, remoteId, text, save) {
  if (typeof remoteId !== 'string' || !remoteId || typeof text !== 'string' || !state.sid) throw new Error('Invalid OpenCode prompt association');
  state.remote_inputs ||= {};
  let entry = Object.hasOwn(state.remote_inputs, remoteId) ? state.remote_inputs[remoteId] : undefined;
  if (!entry) {
    entry = { id: inputID(), sessionID: state.sid, text };
    Object.defineProperty(state.remote_inputs, remoteId, { value: entry, enumerable: true, configurable: true, writable: true });
    await save();
  }
  if (entry.sessionID !== state.sid || entry.text !== text) throw new Error('OpenCode prompt association conflict');
  const path = `/api/session/${encodeURIComponent(state.sid)}`;
  try {
    const result = await client.request(`${path}/prompt`, { method: 'POST', body: { id: entry.id, text, delivery: 'steer' } });
    if (result?.data?.id !== entry.id || result.data.sessionID !== state.sid || result.data.type !== 'user') throw new Error('OpenCode returned an unmatched prompt');
  } catch (error) {
    if (error.status !== 409) throw error;
    const inbox = (await client.request(`${path}/inbox`))?.data;
    const accepted = Array.isArray(inbox) && inbox.some(item => item.id === entry.id && item.sessionID === state.sid && item.type === 'user');
    if (!accepted && !(await client.messages(state.sid)).some(item => item.id === entry.id && item.type === 'user')) throw new Error('OpenCode prompt ID conflict');
  }
}

function messageUuid(remoteSid, nativeSid, messageId, part) {
  const hash = createHash('sha256').update(JSON.stringify([remoteSid, nativeSid, messageId, part])).digest('hex');
  return `${hash.slice(0, 8)}-${hash.slice(8, 12)}-8${hash.slice(13, 16)}-8${hash.slice(17, 20)}-${hash.slice(20, 32)}`;
}

export function historyMessages(row, nativeSid, remoteSid, submitted = new Set()) {
  const messages = [];
  const emit = (part, payload) => messages.push({ ...payload, uuid: messageUuid(remoteSid, nativeSid, row.id, part) });
  if (row.type === 'user' && typeof row.text === 'string' && !submitted.has(row.id)) {
    emit('user', { type: 'user', message: { id: row.id, role: 'user', content: [{ type: 'text', text: row.text }] } });
  }
  if (row.type !== 'assistant' || !Array.isArray(row.content)) return messages;
  for (const [index, part] of row.content.entries()) {
    if (part.type === 'text' && typeof part.text === 'string' && part.text) {
      emit(`text:${index}`, { type: 'assistant', message: { id: `${row.id}-text-${index}`, role: 'assistant', content: [{ type: 'text', text: part.text }], stop_reason: null } });
    }
    if (part.type !== 'tool' || !part.state || !part.id) continue;
    const tool = part.state, name = ['shell', 'bash'].includes(part.name) ? 'Bash' : part.name;
    const input = tool.input && typeof tool.input === 'object' && !Array.isArray(tool.input) ? tool.input : {};
    emit(`tool:${part.id}:start`, { type: 'assistant', parent_tool_use_id: null, message: { id: `${part.id}-tool-use`, role: 'assistant', stop_reason: 'tool_use', content: [{ type: 'tool_use', id: part.id, name, input }] } });
    if (!['completed', 'error'].includes(tool.status)) continue;
    // shortcut: tool results are capped at 65,536 characters; use Terminal for larger output.
    const output = (tool.content || []).filter(item => item.type === 'text' && typeof item.text === 'string').map(item => item.text).join('\n').slice(0, 65536);
    emit(`tool:${part.id}:result`, { type: 'user', parent_tool_use_id: null, message: { id: `${part.id}-tool-result`, role: 'user', content: [{ type: 'tool_result', tool_use_id: part.id, content: output || (tool.status === 'error' ? 'Tool failed' : ''), is_error: tool.status === 'error' }] } });
  }
  return messages;
}

export class RemoteBridge {
  constructor(client, remote, state, save) {
    this.client = client; this.remote = remote; this.state = state; this.save = save;
    this.approvals = new Map(); this.forms = new Set(); this.sent = new Set(); this.activity = false; this.completedSinceStart = false;
    this.path = `/api/session/${encodeURIComponent(state.sid)}`;
  }
  connect(signal, onError = () => {}, beforeReplay = Promise.resolve()) {
    let queue = Promise.resolve(beforeReplay), first = true;
    return new Promise((resolve, reject) => {
      const enqueue = operation => {
        queue = queue.then(operation);
        queue.catch(error => { reject(error); onError(error); });
      };
      const abort = () => reject(new Error('OpenCode event stream stopped'));
      if (signal.aborted) return abort();
      signal.addEventListener('abort', abort, { once: true });
      void this.client.subscribe(event => { if (event.type !== 'server.connected') enqueue(() => this.nativeEvent(event)); }, () => {
        enqueue(async () => {
          await this.reconcile();
          if (first) { first = false; signal.removeEventListener('abort', abort); resolve(); }
        });
      }, signal).catch(error => { reject(error); onError(error); });
    });
  }
  async emitHistory(row, onlyTools = false) {
    const prefix = `${this.remote.sid || ''}:`;
    const submitted = new Set(Object.entries(this.state.remote_inputs || {}).filter(([key]) => key.startsWith(prefix)).map(([, value]) => value.id));
    for (const payload of historyMessages(row, this.state.sid, this.remote.sid || '', submitted)) {
      if (onlyTools && !['tool_use', 'tool_result'].includes(payload.message?.content?.[0]?.type)) continue;
      if (this.sent.has(payload.uuid)) continue;
      await this.remote.event(payload);
      if (payload.type === 'assistant' && payload.message?.content?.[0]?.type === 'text') this.state.last_message = payload.message.content[0].text;
      this.sent.add(payload.uuid);
    }
  }
  async reconcile() {
    for (const row of await this.client.messages(this.state.sid)) {
      await this.emitHistory(row, row.type === 'assistant' && !row.time?.completed);
    }
    const session = (await this.client.request(this.path)).data;
    if (session?.model) {
      this.state.model = `${session.model.providerID}/${session.model.id}`;
      this.state.variant = session.model.variant || null;
      this.remote.model = this.state.model;
    }
    const active = (await this.client.request('/api/session/active')).data;
    this.state.working = Boolean(active?.[this.state.sid]);
    await this.syncForms();
    await this.syncPermissions();
    if (this.state.remote_sid !== this.remote.sid) this.state.remote_sequence = 0;
    this.state.remote_sid = this.remote.sid;
    await this.save();
  }
  async nativeEvent(event) {
    const data = event.data || {};
    if (event.type === 'form.created' && data.form?.sessionID === this.state.sid) { await this.showForm(data.form.id); return; }
    if (data.sessionID !== this.state.sid) return;
    if (event.type === 'form.replied' || event.type === 'form.cancelled') { this.forms.delete(data.id); await this.syncPermissions(); return; }
    if (event.type === 'permission.asked' || event.type === 'permission.replied') { await this.syncPermissions(); return; }
    if (event.type === 'session.execution.started') {
      this.state.working = true; this.completedSinceStart = false;
      await this.remote.state('running');
      if (!this.activity) await this.startActivity();
      await this.save();
      return;
    }
    if (event.type === 'session.inbox.delivered') {
      const row = (await this.client.request(`${this.path}/message/${encodeURIComponent(data.inboxID)}`)).data;
      await this.emitHistory(row);
      return;
    }
    if (['session.tool.called', 'session.tool.success', 'session.tool.failed', 'session.step.ended', 'session.step.failed', 'session.text.ended'].includes(event.type) && data.assistantMessageID) {
      const row = (await this.client.request(`${this.path}/message/${encodeURIComponent(data.assistantMessageID)}`)).data;
      await this.emitHistory(row, !['session.step.ended', 'session.step.failed', 'session.text.ended'].includes(event.type));
      return;
    }
    if (['session.execution.succeeded', 'session.execution.failed', 'session.execution.interrupted'].includes(event.type)) {
      if (this.completedSinceStart) return;
      await this.reconcile();
      if (!this.activity) await this.startActivity();
      await this.remote.event({ type: 'stream_event', parent_tool_use_id: null, event: { type: 'message_stop' } });
      const failed = event.type !== 'session.execution.succeeded';
      await this.remote.event({ type: 'result', subtype: failed ? 'error_during_execution' : 'success', is_error: failed, result: this.state.last_message || '', errors: failed ? ['OpenCode execution did not complete'] : [], num_turns: 1, total_cost_usd: 0, duration_ms: 0, duration_api_ms: 0 });
      this.state.working = false; this.state.ts = Date.now(); this.activity = false; this.completedSinceStart = true;
      await this.syncPermissions(); await this.save();
    }
  }
  async startActivity() {
    this.activity = true;
    await this.remote.event({ type: 'stream_event', parent_tool_use_id: null, event: { type: 'message_start', message: { id: `opencode-${this.state.sid}`, type: 'message', role: 'assistant', model: this.state.model, content: [], stop_reason: null, stop_sequence: null, usage: { input_tokens: 0, output_tokens: 0 } } } });
  }
  async showForm(id) {
    if (!id || this.forms.has(id)) return;
    this.forms.add(id);
    await this.remote.event({ type: 'assistant', message: { role: 'assistant', content: [{ type: 'text', text: 'OpenCode needs input. Use Terminal to answer the pending form.' }] } });
    await this.remote.state('requires_action', { tool_name: 'Terminal', action_description: 'Answer the pending form in Terminal.' });
  }
  async syncForms() {
    const forms = (await this.client.request(`${this.path}/form`)).data;
    if (!Array.isArray(forms)) throw new Error('Invalid OpenCode form list');
    const ids = new Set(forms.filter(form => form.sessionID === this.state.sid).map(form => form.id));
    for (const id of this.forms) if (!ids.has(id)) this.forms.delete(id);
    for (const id of ids) await this.showForm(id);
  }
  async syncPermissions() {
    const pending = (await this.client.request(`${this.path}/permission`)).data;
    if (!Array.isArray(pending)) throw new Error('Invalid OpenCode permission list');
    const ids = new Set(pending.filter(request => request.sessionID === this.state.sid).map(request => request.id));
    for (const [requestId, request] of this.approvals) if (!ids.has(request.id)) {
      this.approvals.delete(requestId);
      await this.remote.event({ type: 'control_cancel_request', request_id: requestId });
    }
    for (const request of pending) {
      if (request.sessionID !== this.state.sid || !/^per[A-Za-z0-9_-]*$/.test(request.id || '')) continue;
      const requestId = messageUuid(this.remote.sid || '', this.state.sid, request.id, 'permission');
      if (this.approvals.has(requestId)) continue;
      const name = ['shell', 'bash'].includes(request.action) ? 'Bash' : request.action;
      const details = { tool_name: name, action_description: request.message || `Approve OpenCode ${request.action}`, request_id: requestId, tool_use_id: request.source?.id || request.id };
      this.approvals.set(requestId, { ...request, details });
      await this.remote.state('requires_action', details);
      await this.remote.event({ type: 'control_request', request_id: requestId, request: { subtype: 'can_use_tool', tool_name: name, input: { resources: request.resources }, tool_use_id: details.tool_use_id } });
    }
    if (this.forms.size) await this.remote.state('requires_action', { tool_name: 'Terminal', action_description: 'Answer the pending form in Terminal.' });
    else if (this.approvals.size) await this.remote.state('requires_action', [...this.approvals.values()].at(-1).details);
    else await this.remote.state(this.state.working ? 'running' : 'idle');
  }
  async handle(payload, eventId) {
    if (payload.type === 'control_response') {
      const response = payload.response, request = this.approvals.get(response?.request_id);
      if (!request) return;
      const pending = (await this.client.request(`${this.path}/permission`)).data;
      this.approvals.delete(response.request_id);
      if (!Array.isArray(pending) || !pending.some(item => item.id === request.id && item.sessionID === this.state.sid)) return;
      const decision = response.response?.behavior === 'allow' ? 'once' : 'reject';
      try { await this.client.request(`${this.path}/permission/${encodeURIComponent(request.id)}/reply`, { method: 'POST', body: { decision } }); }
      catch (error) { if (error.status !== 404 && error.status !== 409) throw error; }
      await this.syncPermissions();
      return;
    }
    if (payload.type === 'control_request') {
      const request = payload.request || {};
      let result = {}, error;
      if (request.subtype === 'interrupt') await this.client.request(`${this.path}/interrupt`, { method: 'POST', body: {} });
      else if (request.subtype === 'initialize') result = { commands: [], models: [{ value: this.state.model, displayName: this.state.model_label || this.state.model, description: 'Current session model' }] };
      else error = `Unsupported OpenCode control: ${request.subtype}`;
      await this.remote.event({ type: 'control_response', response: { subtype: error ? 'error' : 'success', request_id: payload.request_id, ...(error ? { error } : { response: result }) } });
      return;
    }
    if (payload.type !== 'user') return;
    const content = payload.message?.content;
    const blocks = typeof content === 'string' ? [{ type: 'text', text: content }] : content;
    if (!Array.isArray(blocks) || blocks.some(block => block?.type !== 'text' || typeof block.text !== 'string')) {
      await this.remote.event({ type: 'assistant', message: { role: 'assistant', content: [{ type: 'text', text: 'Use Terminal for attachments or interactive forms.' }] } });
      return;
    }
    const text = blocks.map(block => block.text).join('\n');
    if (!text.trim()) return;
    const sourceId = payload.uuid || eventId;
    if (typeof sourceId !== 'string' || !sourceId) throw new Error('Missing Claude prompt identity');
    await submitPrompt(this.client, this.state, `${this.remote.sid || ''}:${sourceId}`, text, this.save);
  }
}

const nativeEnv = () => Object.fromEntries(Object.entries(process.env).filter(([key]) => !key.startsWith('CLAUDE_') && !key.startsWith('CDASH_OPENCODE_')));

async function models(dir) {
  if (!dir || !isAbsolute(dir) || !(await stat(dir)).isDirectory()) throw new Error('OpenCode project directory must exist and be absolute');
  const client = await startServer({ cwd: dir, env: nativeEnv(), metadataOnly: true });
  try { return normalizeModels(await client.request('/api/model')); }
  finally { await client.close(); }
}

async function atomic(path, value) {
  const tmp = `${path}.${process.pid}.tmp`;
  await writeFile(tmp, value, { mode: 0o600 });
  await rename(tmp, path);
}

async function threads(root, limit) {
  if (!root || !isAbsolute(root)) throw new Error('OpenCode state root must be absolute');
  const files = (await readdir(root).catch(error => { if (error.code === 'ENOENT') return []; throw error; }))
    .filter(name => /^ses_[A-Za-z0-9_-]+\.owned\.json$/.test(name)).slice(0, 1000);
  if (!files.length || limit < 1) return [];
  const client = await startServer({ cwd: process.cwd(), env: nativeEnv(), metadataOnly: true });
  try {
    const rows = [];
    for (const file of files) {
      try {
        const owned = JSON.parse(await readFile(join(root, file), 'utf8'));
        if (`${owned.sid}.owned.json` !== file) continue;
        const session = (await client.request(`/api/session/${encodeURIComponent(owned.sid)}`)).data;
        if (!session || session.id !== owned.sid || !session.metadata?.cdash_owner || session.parentID || session.time?.archived || !isAbsolute(session.location?.directory || '')) continue;
        const messages = await client.messages(owned.sid);
        const text = messages.flatMap(message => {
          if (message.type === 'user' && typeof message.text === 'string') return [{ type: 'user', message: { content: message.text } }];
          if (message.type !== 'assistant') return [];
          const content = (message.content || []).filter(part => part.type === 'text' && typeof part.text === 'string').map(part => part.text).join('\n');
          return content ? [{ type: 'assistant', message: { content } }] : [];
        });
        const snapshot = join(root, `${owned.sid}.jsonl`), data = text.map(row => JSON.stringify(row)).join('\n') + (text.length ? '\n' : '');
        if (await readFile(snapshot, 'utf8').catch(() => null) !== data) await atomic(snapshot, data);
        const model = session.model ? `${session.model.providerID}/${session.model.id}` : null;
        rows.push({ sid: owned.sid, dir: session.location.directory, ts: session.time?.updated || 0, display_name: owned.display_name || session.title || 'OpenCode', model, model_label: model, variant: session.model?.variant || null, prompts: text.filter(row => row.type === 'user').map(row => row.message.content).slice(0, 3), source_file: snapshot });
      } catch { /* preserve existing snapshot when discovery fails */ }
    }
    return rows.sort((a, b) => b.ts - a.ts).slice(0, limit);
  } finally { await client.close(); }
}

async function worker(args) {
  const { Remote } = await import('./claude-remote.mjs');
  const option = name => args[args.indexOf(name) + 1];
  const statePath = process.env.CDASH_OPENCODE_STATE;
  const state = JSON.parse(await readFile(statePath, 'utf8'));
  if (!/^ses_[A-Za-z0-9_-]+$/.test(state.sid || '')) throw new Error('Invalid OpenCode session ID');
  const sid = option('--session-id'), token = process.env.CLAUDE_CODE_SESSION_ACCESS_TOKEN;
  const epoch = Number(process.env.CLAUDE_CODE_WORKER_EPOCH);
  if (!token || !Number.isSafeInteger(epoch)) throw new Error('Missing Claude worker credential or epoch');
  const remote = new Remote(option('--sdk-url'), token, epoch, sid);
  const client = new OpenCode({ url: process.env.CDASH_OPENCODE_URL, password: process.env.CDASH_OPENCODE_PASSWORD });
  remote.model = state.model;
  if (state.remote_sid === sid) remote.sequence = state.remote_sequence || 0;
  let saved = Promise.resolve(), stopped = false;
  const save = () => { const json = JSON.stringify(state); saved = saved.then(() => atomic(statePath, json)); return saved; };
  const bridge = new RemoteBridge(client, remote, state, save);
  const nativeStop = new AbortController();
  let heartbeat;
  const stop = () => { if (stopped) return; stopped = true; clearInterval(heartbeat); nativeStop.abort(); remote.stopped.abort(); process.stdin.pause(); };
  const fatal = error => { if (stopped) return; console.error(`OpenCode worker: ${error.message}`); stop(); process.exitCode = 1; };
  for (const signal of ['SIGINT', 'SIGTERM', 'SIGHUP']) process.once(signal, () => { stop(); process.exit(0); });
  let input = '';
  process.stdin.setEncoding('utf8');
  process.stdin.on('data', chunk => {
    input += chunk;
    let end;
    while ((end = input.indexOf('\n')) !== -1) {
      const line = input.slice(0, end); input = input.slice(end + 1);
      try { const update = JSON.parse(line); const next = update.variables?.CLAUDE_CODE_SESSION_ACCESS_TOKEN; if (update.type === 'update_environment_variables' && typeof next === 'string') remote.token = next; } catch {}
    }
    if (input.length > 65536) input = '';
  });
  const init = remote.event({ type: 'system', subtype: 'init', cwd: state.dir, model: state.model, tools: [], mcp_servers: [], permissionMode: 'default', slash_commands: [] });
  init.catch(fatal);
  heartbeat = setInterval(() => remote.retry('/worker/heartbeat', 'POST', { session_id: sid, worker_epoch: epoch }).catch(fatal), 30000);
  try {
    await bridge.connect(nativeStop.signal, fatal, init);
    const rcId = sid.replace(/^cse_/, 'session_');
    await atomic(statePath.replace(/\.json$/, '.link'), `https://claude.ai/code/${rcId}`);
    await remote.listen(async event => {
      await remote.ack(event.event_id, 'received');
      await bridge.handle(event.payload, event.event_id);
      await remote.ack(event.event_id, 'processed');
    }, sequence => { state.remote_sequence = sequence; return save(); });
  } finally { stop(); await saved.catch(() => {}); await client.close(); }
}

function modelRef(model, variant) {
  const split = typeof model === 'string' ? model.indexOf('/') : -1;
  if (split < 1 || split === model.length - 1) throw new Error('Invalid OpenCode model');
  return { providerID: model.slice(0, split), id: model.slice(split + 1), ...(variant ? { variant } : {}) };
}

function stopGroup(child) {
  if (!child?.pid) return;
  const kill = signal => { try { process.kill(-child.pid, signal); } catch {} };
  kill('SIGTERM');
  setTimeout(() => kill('SIGKILL'), 250).unref();
}

async function supervise(statePath, name) {
  if (!isAbsolute(statePath) || !/^cdash-[A-Za-z0-9_-]+$/.test(name || '')) throw new Error('Invalid OpenCode state or pane name');
  const state = JSON.parse(await readFile(statePath, 'utf8'));
  if (!isAbsolute(state.dir || '') || !(await stat(state.dir)).isDirectory()) throw new Error('Invalid OpenCode project directory');
  const root = dirname(statePath), nativePath = statePath.replace(/\.json$/, '.native');
  const controlDir = join(root, 'controls', name);
  await mkdir(controlDir, { recursive: true, mode: 0o700 });
  const log = await open(join(controlDir, 'native.log'), 'a', 0o600);
  let client, tui, runner, retry, closing = false, cleanup;
  let saved = Promise.resolve();
  const saveNative = () => { const data = JSON.stringify(state); saved = saved.then(() => atomic(nativePath, data)); return saved; };
  const events = new AbortController();
  const stop = () => {
    if (cleanup) return cleanup;
    closing = true; clearTimeout(retry); events.abort();
    cleanup = (async () => {
      if (client && state.sid) await client.request(`/api/session/${encodeURIComponent(state.sid)}/interrupt`, { method: 'POST', body: {} }).catch(() => {});
      tui?.kill('SIGTERM');
      stopGroup(runner);
      await client?.close();
      await saved.catch(() => {});
      await rm(statePath.replace(/\.json$/, '.link'), { force: true });
      await log.close();
    })();
    return cleanup;
  };
  const signal = () => { stop().then(() => process.exit(0), () => process.exit(1)); };
  for (const event of ['SIGTERM', 'SIGINT', 'SIGHUP']) process.once(event, signal);
  try {
    client = await startServer({ cwd: state.dir, env: nativeEnv(), onError: () => { if (!closing) stop().then(() => process.exit(1)); } });
    const selected = modelRef(state.model, state.variant);
    if (state.sid) {
      if (!/^ses_[A-Za-z0-9_-]+$/.test(state.sid)) throw new Error('Invalid OpenCode session ID');
      const owned = JSON.parse(await readFile(join(root, `${state.sid}.owned.json`), 'utf8'));
      const previous = (await client.request(`/api/session/${encodeURIComponent(state.sid)}`)).data;
      if (owned.sid !== state.sid || !previous?.metadata?.cdash_owner || previous.parentID || previous.time?.archived) throw new Error('OpenCode session is not owned by this dashboard');
      if (previous.model?.providerID !== selected.providerID || previous.model?.id !== selected.id || (previous.model?.variant || null) !== (selected.variant || null)) await client.request(`/api/session/${encodeURIComponent(state.sid)}/model`, { method: 'POST', body: { model: selected } });
    } else {
      const session = (await client.request('/api/session', { method: 'POST', body: { title: state.display_name, model: selected, metadata: { cdash_owner: true } } })).data;
      if (!/^ses_[A-Za-z0-9_-]+$/.test(session?.id || '')) throw new Error('Invalid OpenCode session response');
      state.sid = session.id;
      await atomic(join(root, `${state.sid}.owned.json`), JSON.stringify({ sid: state.sid, dir: state.dir, display_name: state.display_name }));
    }
    state.pid = process.pid; state.native_terminal = true; state.working = false; state.rc_unavailable = false;
    await atomic(statePath, JSON.stringify(state)); await saveNative();
    let updates = Promise.resolve();
    const enqueue = operation => { updates = updates.then(operation).catch(() => {}); };
    void client.subscribe(event => enqueue(async () => {
      const data = event.data || {};
      if (data.sessionID !== state.sid) return;
      if (event.type === 'session.execution.started') state.working = true;
      else if (['session.execution.succeeded', 'session.execution.failed', 'session.execution.interrupted'].includes(event.type)) { state.working = false; state.ts = Date.now(); }
      else if (event.type === 'session.text.ended') state.last_message = data.text;
      else if (event.type === 'session.model.selected') { state.model = `${data.model.providerID}/${data.model.id}`; state.variant = data.model.variant || null; }
      else return;
      await saveNative();
    }), () => enqueue(async () => {
      const active = (await client.request('/api/session/active')).data;
      const session = (await client.request(`/api/session/${encodeURIComponent(state.sid)}`)).data;
      state.working = Boolean(active?.[state.sid]);
      if (session?.model) { state.model = `${session.model.providerID}/${session.model.id}`; state.variant = session.model.variant || null; }
      await saveNative();
    }), events.signal);
    tui = spawn('opencode', ['--server', client.url, '--session', state.sid], { cwd: state.dir, env: { ...nativeEnv(), OPENCODE_PASSWORD: client.password }, stdio: 'inherit' });
    tui.once('error', () => stop().then(() => process.exit(1)));
    tui.once('exit', code => { if (!closing) stop().then(() => process.exit(code || 0)); });
    let failures = 0, reuseRemote = false;
    const startRemote = () => {
      if (closing) return;
      const child = runner = spawn('claude', ['remote-control', ...(reuseRemote ? ['--continue'] : ['--spawn=session']), '--name', state.display_name, '--debug-file', join(controlDir, 'remote-debug.log')], {
        cwd: controlDir, detached: true, stdio: ['ignore', log.fd, log.fd],
        env: { ...process.env, CLAUDE_CODE_PROCESS_WRAPPER: JSON.stringify([process.execPath, resolve(process.argv[1]), '--worker']), CDASH_OPENCODE_STATE: statePath, CDASH_OPENCODE_URL: client.url, CDASH_OPENCODE_PASSWORD: client.password },
      });
      child.on('error', () => {});
      child.once('close', () => {
        stopGroup(child);
        if (closing) return;
        void (async () => {
          const link = statePath.replace(/\.json$/, '.link');
          const connected = await stat(link).then(() => true, () => false);
          reuseRemote ||= connected;
          await rm(link, { force: true });
          if (connected) failures = 0;
          const delay = Math.min(1000 * 2 ** failures++, 60000);
          state.rc_unavailable = failures > 3; await saveNative();
          if (!closing) retry = setTimeout(startRemote, delay);
        })().catch(() => stop().then(() => process.exit(1)));
      });
    };
    startRemote();
  } catch (error) { await stop(); throw error; }
}

if (process.argv[1] && pathToFileURL(resolve(process.argv[1])).href === import.meta.url) {
  const args = process.argv.slice(2);
  const option = name => args[args.indexOf(name) + 1];
  if (args[0] === '--models') {
    models(option('--dir')).then(value => console.log(JSON.stringify(value)), error => { console.error(`OpenCode models: ${error.message}`); process.exitCode = 1; });
  } else if (args[0] === '--worker' && args.includes('--sdk-url') && process.env.CLAUDE_CODE_SESSION_ACCESS_TOKEN) {
    worker(args).catch(error => { console.error(`OpenCode worker: ${error.message}`); process.exitCode = 1; });
  } else if (args[0] === '--worker') {
    const child = spawn(args[1], args.slice(2), { stdio: 'inherit' });
    child.once('error', () => { process.exitCode = 1; });
    child.once('exit', code => { process.exitCode = code || 0; });
  } else if (args[0] === '--state') {
    supervise(option('--state'), option('--name')).catch(error => { console.error(`OpenCode supervisor: ${error.message}`); process.exitCode = 1; });
  } else if (args[0] === '--threads') {
    const limit = Math.min(100, Math.max(0, Number.parseInt(option('--limit'), 10) || 0));
    threads(option('--root'), limit).then(value => console.log(JSON.stringify(value)), error => { console.error(`OpenCode history: ${error.message}`); process.exitCode = 1; });
  } else {
    console.error('Unsupported OpenCode command');
    process.exitCode = 1;
  }
}
