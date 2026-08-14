#!/usr/bin/env node
// Spec step 5. Runs the Node and Rust agents against one synthetic ~/.claude
// and compares /api/sessions field-by-field over the closed exemption list in
// docs/superpowers/specs/2026-07-30-tauri-multi-host-design.md.
//
// This script is deleted with the Node tree: it cannot run without server.js.
import { spawn } from 'node:child_process';
import fs from 'node:fs/promises';
import os from 'node:os';
import path from 'node:path';

const fail = [];
const check = (ok, msg) => { if (!ok) fail.push(msg); };
const sleep = ms => new Promise(r => setTimeout(r, ms));

// ---------- fixture ----------
const root = await fs.mkdtemp(path.join(os.tmpdir(), 'cdash-parity-'));
const claudeDir = path.join(root, '.claude');
await fs.mkdir(path.join(claudeDir, 'sessions'), { recursive: true });

const munge = p => p.replace(/[^a-zA-Z0-9]/g, '-');
const transcript = turns => Array.from({ length: turns }, (_, i) =>
  JSON.stringify({ type: 'assistant', message: { content: [{ type: 'text', text: `turn ${i}` }] } })
).join('\n') + '\n';

async function writeTranscript(cwd, sid, turns, extra = '') {
  const dir = path.join(claudeDir, 'projects', munge(cwd));
  await fs.mkdir(dir, { recursive: true });
  await fs.writeFile(path.join(dir, `${sid}.jsonl`), extra + transcript(turns));
}

// Two live external sessions in two distinct non-repository directories.
const dirs = [path.join(root, 'proj-a'), path.join(root, 'proj-b')];
const kids = [];
for (const [i, cwd] of dirs.entries()) {
  await fs.mkdir(cwd, { recursive: true });
  const kid = spawn('sleep', ['300'], { stdio: 'ignore' });
  kids.push(kid);
  const sid = `1111111${i}-2222-4333-8444-555555555555`;
  await fs.writeFile(
    path.join(claudeDir, 'sessions', `${kid.pid}.json`),
    JSON.stringify({
      sessionId: sid, cwd, name: `proj-${i}`, entrypoint: 'cli',
      startedAt: Date.now() - 60_000, bridgeSessionId: `session_ext_${i}`,
    })
  );
  await writeTranscript(cwd, sid, 4, JSON.stringify({ type: 'user', gitBranch: 'main' }) + '\n');
}

// A real tmux session, so the pane branch — the main path, and the one whose
// format string changed — is actually compared. Its name must satisfy the kill
// guard's `^cdash-[\w-]+$`, because that is the shape both agents filter on.
const paneDir = path.join(root, 'pane-proj');
await fs.mkdir(paneDir, { recursive: true });
const TMUX_SESSION = 'cdash-parity-1200-abc';
let tmuxOk = false;
try {
  await new Promise((res, rej) => {
    const p = spawn('tmux', ['new-session', '-d', '-s', TMUX_SESSION, '-c', paneDir, 'sleep', '300'],
      { stdio: 'ignore' });
    p.on('exit', c => (c === 0 ? res() : rej(new Error(`tmux exited ${c}`))));
    p.on('error', rej);
  });
  tmuxOk = true;
} catch (e) {
  check(false, `could not create a tmux session, so the PANE PATH WAS NOT COMPARED: ${e.message}`);
}

// An sdk-cli session that BOTH agents must exclude.
const observer = spawn('sleep', ['300'], { stdio: 'ignore' });
kids.push(observer);
await fs.writeFile(
  path.join(claudeDir, 'sessions', `${observer.pid}.json`),
  JSON.stringify({ sessionId: 'aaaaaaaa-0000-4000-8000-000000000000', cwd: dirs[0], entrypoint: 'sdk-cli' })
);

// Resumable history: one with enough turns, one without.
const hist = [];
for (const [i, turns] of [[0, 5], [1, 2]]) {
  const sid = `9999999${i}-2222-4333-8444-555555555555`;
  const cwd = dirs[i % dirs.length];
  await writeTranscript(cwd, sid, turns, JSON.stringify({ type: 'ai-title', aiTitle: `Title ${i}` }) + '\n');
  hist.push(JSON.stringify({ sessionId: sid, project: cwd, timestamp: 1_700_000_000 + i, display: `prompt ${i}` }));
}
await fs.writeFile(path.join(claudeDir, 'history.jsonl'), hist.join('\n') + '\n');
await fs.writeFile(path.join(root, '.claude.json'), JSON.stringify({ projects: {} }));

// ---------- servers ----------
const env = { ...process.env, CLAUDE_DIR: claudeDir, HOME: root };
const nodeSrv = spawn('node', ['server.js'], { env: { ...env, PORT: '8791' }, stdio: 'ignore' });
const rustSrv = spawn('./target/debug/cdash-agent', [], {
  env: { ...env, PORT: '8792', CDASH_BIND: '127.0.0.1', CDASH_PUBLIC: 'public' },
  stdio: 'ignore',
});

const get = async (port, route) => {
  const r = await fetch(`http://127.0.0.1:${port}${route}`);
  return { status: r.status, body: await r.json() };
};

async function waitFor(port) {
  for (let i = 0; i < 100; i++) {
    try { if ((await get(port, '/api/health')).status === 200) return; } catch { /* not up */ }
    await sleep(100);
  }
  throw new Error(`agent on ${port} never became healthy`);
}

try {
  await waitFor(8791);
  await waitFor(8792);

  // Warm both: the git cache returns null cold, and the CPU sampler needs two
  // refreshes 200ms apart. The exemption list assumes a warm comparison.
  for (const p of [8791, 8792]) await get(p, '/api/sessions');
  await sleep(1500);
  for (const p of [8791, 8792]) await get(p, '/api/sessions');
  await sleep(500);

  const [n, r] = [(await get(8791, '/api/sessions')).body, (await get(8792, '/api/sessions')).body];

  // What was actually compared. A gate that silently compared two empty lists
  // passes just as loudly as one that compared everything.
  console.log(`compared: running node=${n.running.length} rust=${r.running.length}` +
    ` | resumable node=${n.resumable.length} rust=${r.resumable.length}` +
    ` | panes ${tmuxOk ? 'yes' : 'NO'}` +
    ` | external node=${n.running.filter(s => s.external).length}`);
  check(n.running.length > 0, 'the fixture produced no running sessions — nothing was compared');
  check(n.resumable.length > 0, 'the fixture produced no resumable sessions — nothing was compared');

  // ---------- /api/sessions ----------
  const key = s => s.sid ?? s.name;
  const byKey = list => Object.fromEntries(list.map(s => [key(s), s]));
  const [nr, rr] = [byKey(n.running), byKey(r.running)];

  check(
    JSON.stringify(Object.keys(nr).sort()) === JSON.stringify(Object.keys(rr).sort()),
    `running sets differ:\n  node=${Object.keys(nr).sort()}\n  rust=${Object.keys(rr).sort()}`
  );

  if (tmuxOk) {
    const pane = k => Object.values(k).find(s => s.name === TMUX_SESSION);
    const [np, rp] = [pane(nr), pane(rr)];
    check(np !== undefined, 'node did not report the tmux pane');
    check(rp !== undefined, 'rust did not report the tmux pane');
    if (np && rp) {
      // The format-string change lives or dies here: Node read the path third
      // of four, Rust reads it last.
      check(np.dir === rp.dir, `pane dir: node=${np.dir} rust=${rp.dir}`);
      check(np.pid === rp.pid, `pane pid: node=${np.pid} rust=${rp.pid}`);
      check(Math.abs(np.uptimeSec - rp.uptimeSec) <= 5, 'pane uptimeSec drifted by more than 5s');
    }
  }

  // Exempt by name, per the spec's closed list (plus the two sampled `stats`
  // fields this port's derivation found missing from it).
  const EXACT = ['name', 'dir', 'pid', 'model', 'effort', 'rcLink', 'sid', 'lastMessage', 'external'];

  for (const k of Object.keys(nr)) {
    const a = nr[k], b = rr[k];
    if (!a || !b) continue;
    for (const f of EXACT) {
      check(
        JSON.stringify(a[f] ?? null) === JSON.stringify(b[f] ?? null),
        `running[${k}].${f}: node=${JSON.stringify(a[f])} rust=${JSON.stringify(b[f])}`
      );
    }
    check(Math.abs(a.uptimeSec - b.uptimeSec) <= 5, `running[${k}].uptimeSec drifted by more than 5s`);
    check(a.uptimeSec >= 0 && b.uptimeSec >= 0, `running[${k}].uptimeSec negative`);
    check(JSON.stringify(a.git) === JSON.stringify(b.git), `running[${k}].git: ${JSON.stringify(a.git)} vs ${JSON.stringify(b.git)}`);
    check(a.working === b.working, `running[${k}].working differs`);
    check(b.cpu === null || typeof b.cpu === 'number', `running[${k}].cpu is neither null nor a number`);
    check(a.rssKb > 0 && b.rssKb > 0, `running[${k}].rssKb must be positive (node=${a.rssKb} rust=${b.rssKb})`);
    check(Math.abs(a.rssKb - b.rssKb) / Math.max(a.rssKb, 1) < 0.10, `running[${k}].rssKb differs by more than 10% (node=${a.rssKb} rust=${b.rssKb})`);
    check(a.cpuSampleAgeMs === undefined, `node must not have cpuSampleAgeMs`);
    check(b.cpuSampleAgeMs !== undefined, `rust must have cpuSampleAgeMs`);
  }

  check(
    JSON.stringify(n.resumable) === JSON.stringify(r.resumable),
    `resumable differs:\n  node=${JSON.stringify(n.resumable)}\n  rust=${JSON.stringify(r.resumable)}`
  );

  check(n.stats.ramTotalKb === r.stats.ramTotalKb, `stats.ramTotalKb: ${n.stats.ramTotalKb} vs ${r.stats.ramTotalKb}`);
  check(
    JSON.stringify(n.stats.disks.map(d => [d.mount, d.totalKb])) ===
    JSON.stringify(r.stats.disks.map(d => [d.mount, d.totalKb])),
    `stats.disks mounts/totals differ:\n  node=${JSON.stringify(n.stats.disks)}\n  rust=${JSON.stringify(r.stats.disks)}`
  );
  // cpuPct and ramUsedKb are sampled per request and cannot be compared for
  // equality — the exemption this port's derivation added to the spec's list.
  check(typeof r.stats.cpuPct === 'number' && r.stats.cpuPct <= 100, 'stats.cpuPct out of range');
  check(r.stats.ramUsedKb > 0, 'stats.ramUsedKb must be positive');

  // ---------- /api/logs: invariants, not equality ----------
  const logs = (await get(8792, '/api/logs')).body;
  check(Array.isArray(logs.lines), '/api/logs must return a lines array');
  check(logs.lines.every(l => /^\d\d:\d\d:\d\d /.test(l)), 'every log line needs an HH:MM:SS prefix');

  // The two-sided dedupe property. Both project dirs are non-repositories, so
  // `git status` failed in each; the keys are per-directory.
  const gitLines = logs.lines.filter(l => l.includes('sh failed: git '));
  const gitDirs = new Set(gitLines.map(l => l.split('sh failed: git ')[1].split(':')[0]));
  check(gitDirs.size >= 2, `two different failing directories must log separately, got ${gitDirs.size} (lines: ${JSON.stringify(gitLines)})`);
  for (const d of gitDirs) {
    const count = gitLines.filter(l => l.includes(`sh failed: git ${d}:`)).length;
    check(count === 1, `one directory failing repeatedly must log once, got ${count} for ${d}`);
  }
} finally {
  for (const k of [nodeSrv, rustSrv, ...kids]) k.kill('SIGKILL');
  if (tmuxOk) spawn('tmux', ['kill-session', '-t', TMUX_SESSION], { stdio: 'ignore' });
  await fs.rm(root, { recursive: true, force: true });
}

if (fail.length) {
  console.error(`PARITY GATE FAILED (${fail.length}):`);
  for (const f of fail) console.error(`  - ${f}`);
  process.exit(1);
}
console.log('PARITY GATE PASSED');
