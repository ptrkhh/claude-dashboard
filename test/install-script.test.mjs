// The setup command is copy-pasted by hand into a shell we never see — Termux
// on Android, WSL on Windows — so it is tested as a shell script, against the
// real templates read out of app.js. A second copy of the text here could
// drift from the one that ships.
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
// A port nothing else here is using: "down" must be decided from the agent's
// own port, not from whatever else happens to be listening.
const PORT = 23274;

const src = () => readFileSync(new URL('../public/app.js', import.meta.url), 'utf8');

function template(name, re, subs) {
  const m = src().match(re);
  assert.ok(m, `app.js still defines ${name} as a template literal`);
  return Object.entries(subs).reduce((t, [k, v]) => t.replaceAll('${' + k + '}', v), m[1]);
}

/** The shipped command for one platform, assembled the way showSetup does. */
function setupScript(fetch, keepAwake) {
  return template('setupScript', /const setupScript = \(fetch, port, keepAwake\) => `([\s\S]*?)`;\n/, {
    fetch, port: String(PORT), keepAwake,
  });
}

/** Stands in for the app's loopback handoff on Android. */
async function handoff() {
  const server = createServer((_, res) => {
    res.writeHead(200, { 'content-type': 'application/octet-stream' });
    res.end(AGENT);
  });
  await new Promise(r => server.listen(0, '127.0.0.1', r));
  return { url: `http://127.0.0.1:${server.address().port}/cdash-agent`, server };
}

/** Everything after the fetch is identical on both platforms, so assert it once. */
async function checkCommon(script, HOME) {
  const dir = mkdtempSync(join(tmpdir(), 'cdash-paste-'));
  const file = join(dir, 'setup.sh');
  writeFileSync(file, script);
  const paste = () => run('bash', [file], { env: { ...process.env, HOME } });

  // A syntax error would surface in the user's shell, one paste too late.
  await run('bash', ['-n', file]);
  await paste();

  const agent = join(HOME, 'cdash-agent');
  assert.equal(readFileSync(agent, 'utf8'), AGENT, 'delivered byte for byte');
  assert.ok(statSync(agent).mode & 0o111, 'executable');

  const bashrc = () => readFileSync(join(HOME, '.bashrc'), 'utf8');
  const blocks = () => bashrc().split('claude-dashboard: start the agent').length - 1;
  assert.equal(blocks(), 1, 'one startup block');
  assert.match(bashrc(), new RegExp(`127\\.0\\.0\\.1:${PORT}/api/health`),
    'the guard checks the agent on its own port');

  // Pasting again — the "reinstall" case the dialog offers — must not stack a
  // second copy into .bashrc.
  await paste();
  assert.equal(blocks(), 1, 'still one startup block after a second paste');

  // Opening the shell with the agent down must start it. curl is stubbed to
  // fail so "down" does not depend on that port being free on this machine.
  const bin = mkdtempSync(join(tmpdir(), 'cdash-bin-'));
  writeFileSync(join(bin, 'curl'), '#!/bin/sh\nexit 1\n');
  chmodSync(join(bin, 'curl'), 0o755);
  await run('bash', ['-c', '. "$HOME/.bashrc"; sleep 0.5'], {
    env: { ...process.env, HOME, PATH: `${bin}:${process.env.PATH}` },
  });
  assert.ok(existsSync(join(HOME, 'agent-started')), 'the startup block launched the agent');
  await run('pkill', ['-f', agent]).catch(() => {});
}

test('the Termux command curls the agent out of the app and arms the shell', async () => {
  const { url, server } = await handoff();
  const HOME = mkdtempSync(join(tmpdir(), 'cdash-home-'));
  try {
    const fetch = `curl -fsS -o "$HOME/cdash-agent" ${url}`;
    const script = setupScript(fetch, '\n  termux-wake-lock 2>/dev/null');
    assert.match(script, /termux-wake-lock/, 'Android holds a wake lock');
    await checkCommon(script, HOME);
  } finally {
    server.close();
  }
});

test('the WSL command copies the agent off the Windows filesystem', async () => {
  const HOME = mkdtempSync(join(tmpdir(), 'cdash-home-'));
  // Stands in for /mnt/c/…, with the space a real Windows username can have.
  const winDir = mkdtempSync(join(tmpdir(), 'cdash-mnt-')) + '/Ada Lovelace';
  await run('mkdir', ['-p', winDir]);
  const source = join(winDir, 'cdash-agent');
  writeFileSync(source, AGENT);

  const script = setupScript(`cp "${source}" "$HOME/cdash-agent"`, '');
  assert.doesNotMatch(script, /termux-wake-lock/, 'nothing termux-shaped in a WSL .bashrc');
  await checkCommon(script, HOME);
});
