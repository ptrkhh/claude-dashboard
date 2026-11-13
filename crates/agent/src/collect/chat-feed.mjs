// The supervisor's side of dashboard chat: a numbered log of conversation
// messages (Agent SDK shapes) served as NDJSON on a private unix socket, plus
// send and stop routed back to the supervisor. The agent is the only client.
// Never writes to stdout or stderr: stdout is the native TUI's terminal.
import { createServer } from 'node:net';
import { chmod, rm } from 'node:fs/promises';
import { join } from 'node:path';
import { randomBytes } from 'node:crypto';

const LINE_MAX = 64 * 1024;

export class ChatFeed {
  #send; #stop;
  #log = [];
  #uuids = new Set();
  #approvals = new Set();
  #state = { working: false, waiting: false };
  #ready = false;
  #clients = new Set();
  #server = null;
  epoch = randomBytes(8).toString('hex');

  constructor({ send, stop }) {
    this.#send = send;
    this.#stop = stop;
  }

  get head() { return this.#log.length; }
  get log() { return this.#log; }
  get state() { return { working: this.#state.working, waiting: this.#state.waiting || this.#approvals.size > 0 }; }

  accept(m) {
    if (!m || typeof m !== 'object') return;
    if (m.type === 'control_request' && m.request?.subtype === 'can_use_tool') {
      this.#approvals.add(m.request_id);
      return this.#publishState();
    }
    if (m.type === 'control_cancel_request' || m.type === 'control_response') {
      if (this.#approvals.delete(m.request_id ?? m.response?.request_id)) this.#publishState();
      return;
    }
    if (!['user', 'assistant', 'result'].includes(m.type)) return;
    if (m.uuid) {
      if (this.#uuids.has(m.uuid)) return;
      this.#uuids.add(m.uuid);
    }
    const ev = m.type === 'result' ? { type: 'result', subtype: m.subtype } : m;
    const seq = this.#log.length + 1;
    this.#log.push({ seq, ev });
    this.#broadcast({ t: 'ev', seq, ev });
  }

  delta(id, text) {
    if (id && text) this.#broadcast({ t: 'delta', id, text });
  }

  setState(next) {
    const before = JSON.stringify(this.state);
    Object.assign(this.#state, next);
    if (JSON.stringify(this.state) !== before) this.#publishState();
  }

  ready() {
    if (this.#ready) return;
    this.#ready = true;
    this.#broadcast({ t: 'ready' });
  }

  async listen(dir) {
    const path = join(dir, 'chat.sock');
    await rm(path, { force: true });
    this.#server = createServer(socket => this.#serve(socket));
    await new Promise((resolve, reject) => {
      this.#server.once('error', reject);
      this.#server.listen(path, resolve);
    });
    this.#server.on('error', () => {});
    await chmod(path, 0o600);
    return path;
  }

  close() {
    for (const s of this.#clients) s.destroy();
    this.#clients.clear();
    this.#server?.close();
  }

  #publishState() {
    this.#broadcast({ t: 'state', ...this.state });
  }

  #broadcast(frame) {
    const line = JSON.stringify(frame) + '\n';
    for (const s of this.#clients) s.write(line);
  }

  #serve(socket) {
    socket.on('error', () => {});
    socket.on('close', () => this.#clients.delete(socket));
    let buf = '';
    let hello = false;
    let chain = Promise.resolve(); // one request at a time, acks in order
    socket.on('data', chunk => {
      buf += chunk;
      let i;
      while ((i = buf.indexOf('\n')) >= 0) {
        const line = buf.slice(0, i);
        buf = buf.slice(i + 1);
        let m;
        try { m = JSON.parse(line); } catch { m = null; }
        if (!hello) {
          hello = true;
          this.#attach(socket, m && typeof m === 'object' ? m : {});
        } else if (m && typeof m === 'object') {
          chain = chain.then(() => this.#request(socket, m));
        }
      }
      if (buf.length > LINE_MAX) socket.destroy();
    });
  }

  #attach(socket, { after, epoch }) {
    const from = epoch === this.epoch && Number.isInteger(after) && after >= 0 ? after : 0;
    const lines = [{ t: 'hello', epoch: this.epoch, head: this.head }];
    for (const e of this.#log.slice(from)) lines.push({ t: 'ev', seq: e.seq, ev: e.ev });
    lines.push({ t: 'state', ...this.state });
    if (this.#ready) lines.push({ t: 'ready' });
    socket.write(lines.map(l => JSON.stringify(l) + '\n').join(''));
    this.#clients.add(socket);
  }

  async #request(socket, m) {
    if (m.t === 'stop') {
      try { await this.#stop(); } catch { /* nothing to stop */ }
      return;
    }
    if (m.t !== 'send' || typeof m.text !== 'string') return;
    let ack;
    try {
      await this.#send(m.text, { steer: !!m.steer, cid: m.cid });
      ack = { t: 'ack', cid: m.cid, ok: true };
    } catch (e) {
      ack = { t: 'ack', cid: m.cid, ok: false, error: String(e?.message ?? e) };
    }
    if (!socket.destroyed) socket.write(JSON.stringify(ack) + '\n');
  }
}
