// The setup command is copy-pasted by hand into a shell we never see — Termux
// on Android, WSL on Windows — so it is tested as a shell script. The three
// functions that build it are evaluated as written in app.js, not re-typed
// here: a second copy of the text could drift from the one that ships.
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync, writeFileSync, mkdtempSync, chmodSync, copyFileSync, existsSync, statSync } from 'node:fs';
import { execFile, spawn } from 'node:child_process';
import { once } from 'node:events';
import vm from 'node:vm';
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

const shipped = (() => {
  const js = src();
  const from = js.indexOf('const setupScript =');
  const to = js.indexOf('async function showSetup');
  assert.ok(from > 0 && to > from, 'app.js still defines the setup helpers in one block');
  return vm.runInNewContext(`${js.slice(from, to)}; ({ setupScript, fetchLine, keepAwakeLine })`);
})();

/** The shipped command for one platform, assembled the way showSetup does. */
const setupScript = (kind, source) =>
  shipped.setupScript(shipped.fetchLine(kind, source), PORT, shipped.keepAwakeLine(kind));

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
  assert.ok(!existsSync(`${agent}.new`), 'no staging file left behind');

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
    const script = setupScript('curl', url);
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

  const script = setupScript('copy', source);
  assert.doesNotMatch(script, /termux-wake-lock/, 'nothing termux-shaped in a WSL .bashrc');
  await checkCommon(script, HOME);
});

test('pasting again replaces an agent that is running', async () => {
  const { url, server } = await handoff();
  const HOME = mkdtempSync(join(tmpdir(), 'cdash-home-'));
  const agent = join(HOME, 'cdash-agent');
  // A real executable: Linux refuses to open a *running* binary for writing
  // ("Text file busy"), which a shell-script stub would never show.
  copyFileSync('/bin/sleep', agent);
  chmodSync(agent, 0o755);
  const running = spawn(agent, ['30'], { stdio: 'ignore' });
  try {
    await once(running, 'spawn');
    const file = join(mkdtempSync(join(tmpdir(), 'cdash-paste-')), 'setup.sh');
    writeFileSync(file, setupScript('curl', url));
    await run('bash', [file], { env: { ...process.env, HOME } });
    assert.equal(readFileSync(agent, 'utf8'), AGENT, 'the new agent is in place');
    assert.ok(!existsSync(`${agent}.new`), 'no staging file left behind');
  } finally {
    running.kill();
    server.close();
    await run('pkill', ['-f', agent]).catch(() => {});
  }
});

test('a failed download leaves the agent that was already there', async () => {
  const HOME = mkdtempSync(join(tmpdir(), 'cdash-home-'));
  const agent = join(HOME, 'cdash-agent');
  writeFileSync(agent, '#!/bin/sh\nexit 0\n');
  chmodSync(agent, 0o755);
  const before = readFileSync(agent, 'utf8');
  const file = join(mkdtempSync(join(tmpdir(), 'cdash-paste-')), 'setup.sh');
  // Nothing listens on port 1: curl fails, and the rename must not happen.
  writeFileSync(file, setupScript('curl', 'http://127.0.0.1:1/cdash-agent'));
  await run('bash', [file], { env: { ...process.env, HOME } }).catch(() => {});
  assert.equal(readFileSync(agent, 'utf8'), before, 'the old agent is untouched');
  assert.ok(!existsSync(`${agent}.new`), 'no partial download left behind');
  await run('pkill', ['-f', agent]).catch(() => {});
});

test('a truncated download leaves the agent that was already there', async () => {
  // Promises 1000 bytes, sends 10, hangs up: curl exits non-zero but has already
  // written a partial file, which is the case the && chain exists for. (A refused
  // connection, as above, writes nothing, so it cannot tell a chain from no chain.)
  const server = createServer((_, res) => {
    res.writeHead(200, { 'content-length': '1000' });
    res.write('0123456789');
    setTimeout(() => res.destroy(), 50);
  });
  await new Promise(r => server.listen(0, '127.0.0.1', r));
  const HOME = mkdtempSync(join(tmpdir(), 'cdash-home-'));
  const agent = join(HOME, 'cdash-agent');
  writeFileSync(agent, '#!/bin/sh\nexit 0\n');
  chmodSync(agent, 0o755);
  const before = readFileSync(agent, 'utf8');
  try {
    const file = join(mkdtempSync(join(tmpdir(), 'cdash-paste-')), 'setup.sh');
    writeFileSync(file, setupScript('curl', `http://127.0.0.1:${server.address().port}/cdash-agent`));
    await run('bash', [file], { env: { ...process.env, HOME } }).catch(() => {});
    assert.equal(readFileSync(agent, 'utf8'), before, 'a 10-byte fragment did not replace the agent');
    assert.ok(!existsSync(`${agent}.new`), 'and the partial download was cleaned up');
  } finally {
    server.close();
    await run('pkill', ['-f', agent]).catch(() => {});
  }
});

test('the WSL command survives a Windows path full of shell metacharacters', async () => {
  const HOME = mkdtempSync(join(tmpdir(), 'cdash-home-'));
  // $, backtick, double quote and backslash all stay live inside double quotes.
  const winDir = mkdtempSync(join(tmpdir(), 'cdash-mnt-')) + '/A $HOME "q" `x` \\y';
  await run('mkdir', ['-p', winDir]);
  const source = join(winDir, 'cdash-agent');
  writeFileSync(source, AGENT);
  await checkCommon(setupScript('copy', source), HOME);
});
