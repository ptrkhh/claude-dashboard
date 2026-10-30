import { spawn } from 'node:child_process';
import { randomBytes } from 'node:crypto';
import { once } from 'node:events';

function validURL(value) {
  const url = new URL(value);
  if (url.protocol !== 'http:' || !['127.0.0.1', 'localhost', '[::1]'].includes(url.hostname) || !url.port || Number(url.port) < 1 || url.username || url.password || url.pathname !== '/' || url.search || url.hash) throw new Error('Invalid loopback server URL');
  return url.origin;
}

export class OpenCode {
  constructor({ url, password }) {
    this.url = validURL(url);
    this.password = password;
  }
  async request(path, { method = 'GET', body, signal } = {}) {
    if (!path.startsWith('/api/') || path.startsWith('//')) throw new Error('Invalid API path');
    let response;
    try {
      response = await fetch(this.url + path, { method, redirect: 'error', signal: signal ? AbortSignal.any([signal, AbortSignal.timeout(15000)]) : AbortSignal.timeout(15000), headers: { authorization: `Basic ${Buffer.from(`opencode:${this.password}`).toString('base64')}`, ...(body === undefined ? {} : { 'content-type': 'application/json' }) }, ...(body === undefined ? {} : { body: JSON.stringify(body) }) });
    } catch (cause) {
      if (signal?.aborted) throw cause;
      throw new Error('OpenCode API request failed');
    }
    if (!response.ok) { const error = new Error(`OpenCode API returned HTTP ${response.status}`); error.status = response.status; throw error; }
    if (response.status === 204) return;
    const chunks = []; let size = 0;
    try {
      for await (const chunk of response.body) {
        size += chunk.length;
        if (size > 8 * 1024 * 1024) throw new Error('OpenCode API response too large');
        chunks.push(chunk);
      }
    } catch (error) {
      if (error.message === 'OpenCode API response too large' || signal?.aborted) throw error;
      throw new Error('OpenCode API response failed');
    }
    try { return JSON.parse(Buffer.concat(chunks).toString()); } catch { throw new Error('Invalid OpenCode API JSON response'); }
  }
  async messages(sid, { maxPages = 100 } = {}) {
    const messages = [], cursors = new Set(); let cursor;
    for (let pageNumber = 0; pageNumber < maxPages; pageNumber++) {
      const query = new URLSearchParams({ limit: '100', ...(cursor ? { cursor } : { order: 'asc' }) });
      const page = await this.request(`/api/session/${encodeURIComponent(sid)}/message?${query}`);
      if (!Array.isArray(page?.data)) throw new Error('Invalid OpenCode message page');
      messages.push(...page.data);
      cursor = page.cursor?.next;
      if (!cursor || !page.data.length) return messages;
      if (cursors.has(cursor)) throw new Error('Repeated OpenCode message cursor');
      cursors.add(cursor);
    }
    throw new Error('OpenCode message page limit exceeded');
  }
  async subscribe(onEvent, onConnected, signal) {
    while (!signal?.aborted) {
      try {
        const response = await fetch(this.url + '/api/event', { signal, redirect: 'error', headers: { authorization: `Basic ${Buffer.from(`opencode:${this.password}`).toString('base64')}` } });
        if (!response.ok) throw new Error(`OpenCode event stream HTTP ${response.status}`);
        onConnected();
        const decoder = new TextDecoder(); let buffer = '';
        for await (const chunk of response.body) {
          buffer = (buffer + decoder.decode(chunk, { stream: true })).replace(/\r\n/g, '\n');
          if (buffer.length > 1024 * 1024) throw new Error('OpenCode event too large');
          let end;
          while ((end = buffer.indexOf('\n\n')) !== -1) {
            const frame = buffer.slice(0, end); buffer = buffer.slice(end + 2);
            const data = frame.split('\n').filter(line => line.startsWith('data:')).map(line => line.slice(5).trimStart()).join('\n');
            if (!data) continue;
            let event; try { event = JSON.parse(data); } catch { continue; }
            onEvent(event);
          }
        }
      } catch (error) { if (signal?.aborted) return; }
      if (!signal?.aborted) await new Promise(resolve => { const timer = setTimeout(resolve, 1000); signal?.addEventListener('abort', () => { clearTimeout(timer); resolve(); }, { once: true }); });
    }
  }
  async close() {
    if (!this.child) return;
    const child = this.child; this.child = null;
    if (child.exitCode !== null || child.signalCode !== null) return;
    const exited = once(child, 'exit'); child.stdin.end();
    const terminate = setTimeout(() => { try { process.kill(-child.pid, 'SIGTERM'); } catch {} }, 2000);
    const kill = setTimeout(() => { try { process.kill(-child.pid, 'SIGKILL'); } catch {} }, 4000);
    terminate.unref(); kill.unref();
    try { await exited; } finally { clearTimeout(terminate); clearTimeout(kill); }
  }
}

export async function startServer({ cwd, env = process.env, onError, metadataOnly = false } = {}) {
  let serverEnv = env;
  if (metadataOnly) {
    let config;
    try { config = env.OPENCODE_CONFIG_CONTENT ? JSON.parse(env.OPENCODE_CONFIG_CONTENT) : {}; }
    catch { throw new Error('Metadata discovery requires OPENCODE_CONFIG_CONTENT to be JSON'); }
    if (!config || typeof config !== 'object' || Array.isArray(config) || (config.plugins !== undefined && !Array.isArray(config.plugins))) throw new Error('Invalid OpenCode inline configuration');
    // shortcut: inline JSONC is unsupported for metadata queries, upgrade when OpenCode provides a config parser or MCP-off flag.
    config.plugins = [...(config.plugins || []), '-opencode.config.mcp', '-opencode.warming'];
    serverEnv = { ...env, OPENCODE_CONFIG_CONTENT: JSON.stringify(config) };
  }
  const password = randomBytes(32).toString('hex');
  const child = spawn('opencode', ['serve', '--stdio', '--hostname', '127.0.0.1', '--port', '0'], { cwd, env: { ...serverEnv, OPENCODE_PASSWORD: password }, detached: true, stdio: ['pipe', 'pipe', 'pipe'] });
  child.stderr.on('data', () => {});
  let buffer = '';
  try {
    const url = await new Promise((resolve, reject) => {
      const timer = setTimeout(() => finish(new Error('OpenCode readiness timeout')), 10000);
      const finish = (error, value) => { clearTimeout(timer); child.off('error', failed); child.off('exit', exited); child.stdout.off('data', data); error ? reject(error) : resolve(value); };
      const failed = () => finish(new Error('Unable to start OpenCode'));
      const exited = () => finish(new Error('OpenCode exited before readiness'));
      const data = chunk => { buffer += chunk; if (buffer.length > 65536) return finish(new Error('Invalid OpenCode readiness')); const end = buffer.indexOf('\n'); if (end < 0) return; try { finish(null, validURL(JSON.parse(buffer.slice(0, end)).url)); } catch { finish(new Error('Invalid loopback OpenCode readiness')); } };
      child.once('error', failed); child.once('exit', exited); child.stdout.on('data', data);
    });
    child.stdout.resume();
    const client = new OpenCode({ url, password }); client.child = child;
    child.on('error', () => onError?.(new Error('OpenCode process failed')));
    child.on('exit', () => { if (client.child) onError?.(new Error('OpenCode process exited')); });
    return client;
  } catch (error) {
    if (child.pid) {
      const cleanup = new OpenCode({ url: 'http://127.0.0.1:1', password });
      cleanup.child = child;
      try { process.kill(-child.pid, 'SIGTERM'); } catch {}
      await cleanup.close();
    }
    throw error;
  }
}

export function normalizeModels(payload) {
  if (!Array.isArray(payload?.data)) return [];
  return payload.data.filter(model => model.enabled !== false && typeof model.providerID === 'string' && typeof model.id === 'string').map(model => {
    const canonical = `${model.providerID}/${model.id}`;
    return { value: `opencode:${canonical}`, model: canonical, label: `OpenCode: ${model.name || canonical}`, provider: 'opencode', variants: Array.isArray(model.variants) ? model.variants.map(variant => variant.id).filter(id => typeof id === 'string') : [], defaultVariant: typeof model.defaultVariant === 'string' ? model.defaultVariant : null };
  });
}
