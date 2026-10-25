import test from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { runInNewContext } from 'node:vm';

const source = readFileSync(new URL('../crates/agent/src/collect/codex-remote.mjs', import.meta.url), 'utf8');
const transport = source.slice(source.indexOf('class Remote {'), source.indexOf('\nfunction messageUuid'));

for (const stalled of ['headers', 'body']) test(`Codex reconnects after stalled SSE ${stalled}`, { timeout: 3000 }, async t => {
  const requests = [], received = [], saved = [], errors = [];
  let remote;
  const context = {
    URL, AbortController, AbortSignal, TextDecoderStream,
    console: { error: text => errors.push(text) },
    // Exercise real timers and streams with a shorter inactivity window.
    setTimeout: (fn, ms) => setTimeout(fn, ms / 600), clearTimeout,
    sleep: () => Promise.resolve(),
    fetch: async (url, { signal }) => {
      requests.push(url);
      if (requests.length === 1 && stalled === 'headers') {
        return new Promise((resolve, reject) => signal.addEventListener('abort', () => reject(signal.reason), { once: true }));
      }
      return new Response(new ReadableStream({ start(controller) {
        const send = text => controller.enqueue(new TextEncoder().encode(text));
        send(':keepalive\n\n');
        let heartbeat, message;
        if (requests.length > 1) {
          heartbeat = setInterval(() => send(':keepalive\n\n'), 20);
          message = setTimeout(() => {
            for (const sequence_num of ['7', '8']) send(`event: client_event\ndata: ${JSON.stringify({
              sequence_num, source: 'client', payload: { type: 'user', message: { content: 'pending prompt' } },
            })}\n\n`);
          }, 250);
        }
        signal.addEventListener('abort', () => {
          clearInterval(heartbeat); clearTimeout(message);
          controller.error(signal.reason);
        }, { once: true });
      } }));
    },
  };
  runInNewContext(`${transport}\nthis.Remote = Remote;`, context);
  remote = new context.Remote('https://api.anthropic.com/v1/code/sessions/cse_check', 'test-token', 1, 'cse_check');
  remote.sequence = 7;
  t.after(() => remote.stopped.abort());
  await remote.listen(event => {
    received.push(event);
    remote.stopped.abort();
  }, sequence => saved.push(sequence));
  assert.deepEqual(requests.map(url => new URL(url).searchParams.get('from_sequence_num')), ['7', '7'],
    'reconnect from the last processed event; keepalives prevent further reconnects');
  assert.deepEqual(received.map(event => event.sequence_num), ['8'], 'pending prompt is forwarded once');
  assert.deepEqual(saved, [8]);
  assert.equal(errors.length, 1, 'only the stalled connection is reported');
});
