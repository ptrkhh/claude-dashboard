import { randomUUID } from 'node:crypto';

const sleep = ms => new Promise(r => setTimeout(r, ms));

export class Remote {
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
  async request(path, method, body, streamSignal) {
    const signal = AbortSignal.any([this.stopped.signal, streamSignal || AbortSignal.timeout(15_000)]);
    const response = await fetch(`${this.url}${path}`, {
      method, signal,
      redirect: 'error',
      headers: { Authorization: `Bearer ${this.token}`, 'Content-Type': 'application/json', 'anthropic-version': '2023-06-01' },
      ...(body === undefined ? {} : { body: JSON.stringify(body) }),
    });
    // Do not quote response bodies: a transport failure must not leak tokens.
    if (!response.ok) { await response.body?.cancel(); throw Object.assign(Error(`Claude worker ${path.split('?')[0]}: HTTP ${response.status}`), { status: response.status }); }
    if (streamSignal) return response;
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
      const connection = new AbortController();
      // Network handovers can leave an open stream that never delivers again.
      const idle = setTimeout(() => connection.abort(Error('Claude worker event stream stalled')), 60_000);
      try {
        const response = await this.request(`/worker/events/stream?from_sequence_num=${this.sequence}`, 'GET', undefined, connection.signal);
        let frame = '', kind = '', data = [];
        for await (const chunk of response.body.pipeThrough(new TextDecoderStream())) {
          idle.refresh();
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
      } finally {
        clearTimeout(idle);
        connection.abort();
      }
      await sleep(delay);
      delay = Math.min(delay * 2, 30_000);
    }
  }
}

