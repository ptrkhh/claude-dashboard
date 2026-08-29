// The Termux setup command is copy-pasted by hand into a shell we never see,
// so it is tested as a shell script — against the real template read out of
// app.js, since a second copy of the text here could drift from what ships.
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync, writeFileSync, mkdtempSync, chmodSync, existsSync, statSync } from 'node:fs';
import { execFile } from 'node:child_process';
import { createServer } from 'node:http';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { promisify } from 'node:util';

// Async, not execFileSync: the handoff server below runs in this process, and a
// synchronous child would block the event loop that has to answer its curl.
const run = promisify(execFile);

const AGENT = '#!/bin/sh\ntouch "$HOME/agent-started"\nsleep 2\n';

/** The shipped template with `${url}` filled in — never a second copy of it. */
function installScript(url) {
  const src = readFileSync(new URL('../public/app.js', import.meta.url), 'utf8');
  const m = src.match(/const installScript = url => `([\s\S]*?)`;\n/);
  assert.ok(m, 'app.js still defines installScript as a template literal');
  return m[1].replaceAll('${url}', url);
}

/** Stands in for the app's loopback handoff of the bundled agent. */
async function handoff() {
  const server = createServer((_, res) => {
    res.writeHead(200, { 'content-type': 'application/octet-stream' });
    res.end(AGENT);
  });
  await new Promise(r => server.listen(0, '127.0.0.1', r));
  return { url: `http://127.0.0.1:${server.address().port}/cdash-agent`, server };
}

test('the pasted command installs the agent and arms Termux to start it', async () => {
  const { url, server } = await handoff();
  const HOME = mkdtempSync(join(tmpdir(), 'cdash-home-'));
  const dir = mkdtempSync(join(tmpdir(), 'cdash-paste-'));
  const script = join(dir, 'install.sh');
  writeFileSync(script, installScript(url));
  const paste = () => run('bash', [script], { env: { ...process.env, HOME } });

  try {
    // A syntax error would surface in the user's Termux, one paste too late.
    await run('bash', ['-n', script]);
    await paste();

    const agent = join(HOME, 'cdash-agent');
    assert.equal(readFileSync(agent, 'utf8'), AGENT, 'downloaded byte for byte');
    assert.ok(statSync(agent).mode & 0o111, 'executable');

    const blocks = () =>
      readFileSync(join(HOME, '.bashrc'), 'utf8').split('claude-dashboard: start the agent').length - 1;
    assert.equal(blocks(), 1, 'one startup block');

    // Pasting again — the "reinstall" case the dialog offers — must not stack a
    // second copy into .bashrc.
    await paste();
    assert.equal(blocks(), 1, 'still one startup block after a second paste');

    // Opening Termux with the agent down must start it. curl is stubbed to fail
    // so "down" does not depend on port 8080 being free on this machine.
    const bin = mkdtempSync(join(tmpdir(), 'cdash-bin-'));
    writeFileSync(join(bin, 'curl'), '#!/bin/sh\nexit 1\n');
    chmodSync(join(bin, 'curl'), 0o755);
    await run('bash', ['-c', '. "$HOME/.bashrc"; sleep 0.5'], {
      env: { ...process.env, HOME, PATH: `${bin}:${process.env.PATH}` },
    });
    assert.ok(existsSync(join(HOME, 'agent-started')), 'the startup block launched the agent');
  } finally {
    server.close();
    await run('pkill', ['-f', join(HOME, 'cdash-agent')]).catch(() => {});
  }
});
