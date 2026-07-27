import { execFile } from 'node:child_process';
import { promisify } from 'node:util';
import fs from 'node:fs/promises';
import path from 'node:path';
import * as S from './sessions.js';
import { procTreeUsage, parseDf, machineStats } from './stats.js';

const run = promisify(execFile);
const shFailed = new Set(); // ponytail: log a given failing command once, not every 4s poll
// Every subprocess is time-boxed: a slow repo (git status on a big tree over a
// network mount can take minutes) must never stall a 4s poll.
const sh = async (cmd, args, timeout = 5000) => {
  try { return (await run(cmd, args, { timeout, killSignal: 'SIGKILL' })).stdout; }
  catch (e) {
    const key = `${cmd} ${args[0] || ''}`;
    if (!shFailed.has(key)) { shFailed.add(key); log(`sh failed: ${key}: ${e.message}`); }
    return '';
  }
};

export const logBuffer = [];
export function log(msg) {
  const line = `${new Date().toTimeString().slice(0, 8)} ${msg}`;
  logBuffer.push(line);
  if (logBuffer.length > 200) logBuffer.shift();
  console.log(line);
}

async function readIf(file) { try { return await fs.readFile(file, 'utf8'); } catch { return null; } }

// git status per directory, refreshed in the background. A poll never waits on
// git: it gets the last known answer (or null the first time) and moves on.
const gitCache = new Map(); // dir -> { out, ts, busy }
const GIT_TTL_MS = 15_000;
function gitStatusFor(dir, now) {
  const hit = gitCache.get(dir) || { out: null, ts: 0, busy: false };
  if (!hit.busy && now - hit.ts > GIT_TTL_MS) {
    hit.busy = true;
    gitCache.set(dir, hit);
    // 20s ceiling: slower than that and this repo simply has no git badge.
    sh('git', ['-C', dir, 'status', '--porcelain=v1', '-b'], 20_000)
      .then(out => gitCache.set(dir, { out: out || null, ts: Date.now(), busy: false }))
      .catch(() => gitCache.set(dir, { ...hit, ts: Date.now(), busy: false }));
  }
  return hit.out;
}

// memoized transcript parse, keyed by file path, revalidated by mtimeMs
const transcriptCache = new Map();
const TRANSCRIPT_CACHE_MAX = 200;
export async function parseTranscriptCached(file) {
  let st;
  try { st = await fs.stat(file); } catch { return null; }
  const hit = transcriptCache.get(file);
  if (hit && hit.mtimeMs === st.mtimeMs) return hit.result;
  const txt = await readIf(file);
  if (txt === null) return null;
  const result = S.parseTranscript(txt);
  if (transcriptCache.size >= TRANSCRIPT_CACHE_MAX) transcriptCache.clear(); // ponytail: crude cap, LRU if this ever matters
  transcriptCache.set(file, { mtimeMs: st.mtimeMs, result });
  return result;
}

const TAIL_BYTES = 128 * 1024;
async function readTail(file) {
  let fh;
  try {
    fh = await fs.open(file, 'r');
    const st = await fh.stat();
    const start = Math.max(0, st.size - TAIL_BYTES);
    const len = st.size - start;
    const buf = Buffer.alloc(len);
    await fh.read(buf, 0, len, start);
    return buf.toString('utf8');
  } catch { return null; }
  finally { if (fh) await fh.close(); }
}

// ~/.claude/sessions/<pid>.json — authoritative link between a pane's pid and
// its session id, so we never have to guess which transcript belongs to a pane.
async function sessionFileFor(claudeDir, pid) {
  const txt = await readIf(path.join(claudeDir, 'sessions', `${pid}.json`));
  if (!txt) return null;
  try { return JSON.parse(txt); } catch { return null; }
}

export async function rcLinkFor(claudeDir, pid) {
  const txt = await readIf(path.join(claudeDir, 'sessions', `${pid}.json`));
  const id = txt && S.parseRcFile(txt);
  return id ? `https://claude.ai/code/${id}` : null;
}

// newest transcript in the project dir modified at/after session start
async function transcriptFor(claudeDir, cwd, createdSec) {
  const dir = path.join(claudeDir, 'projects', S.projectDirName(cwd));
  let best = null;
  try {
    for (const f of await fs.readdir(dir)) {
      if (!f.endsWith('.jsonl')) continue;
      const st = await fs.stat(path.join(dir, f));
      if (st.mtimeMs / 1000 < createdSec - 5) continue;
      if (!best || st.mtimeMs > best.mtimeMs) best = { file: path.join(dir, f), mtimeMs: st.mtimeMs };
    }
  } catch { /* no project dir yet */ }
  return best;
}

const MODELS = new Set(['sonnet', 'opus', 'haiku', 'fable']);
const EFFORTS = new Set(['low', 'medium', 'high', 'xhigh', 'max']);

async function trustDir(claudeDir, dir) {
  // ~/.claude.json lives in $HOME even when CLAUDE_DIR is overridden, but for
  // testability we derive it from claudeDir's parent when CLAUDE_DIR is set.
  const file = process.env.CLAUDE_DIR
    ? path.join(path.dirname(claudeDir), '.claude.json')
    : path.join(process.env.HOME, '.claude.json');
  let cfg = {};
  try { cfg = JSON.parse(await fs.readFile(file, 'utf8')); } catch { /* fresh */ }
  cfg.projects = cfg.projects || {};
  cfg.projects[dir] = { ...cfg.projects[dir], hasTrustDialogAccepted: true };
  const tmp = `${file}.cdash.tmp`;
  await fs.writeFile(tmp, JSON.stringify(cfg, null, 2));
  await fs.rename(tmp, file);
}

function tmuxName(dir) {
  const base = path.basename(dir).replace(/[^a-zA-Z0-9_-]/g, '-').slice(0, 30);
  const hhmm = new Date().toTimeString().slice(0, 5).replace(':', '');
  return `cdash-${base}-${hhmm}-${Math.random().toString(36).slice(2, 5)}`;
}

async function spawnClaude(ctx, dir, claudeArgs, metaEntry) {
  await trustDir(ctx.claudeDir, dir);
  const name = tmuxName(dir);
  await run('tmux', ['new-session', '-d', '-s', name, '-c', dir,
    'claude', ...claudeArgs, '--dangerously-skip-permissions', '--remote-control', name]);
  ctx.meta.set(name, metaEntry);
  log(`launch ${path.basename(dir)} → ${name}`);
  // background RC-link poll: 60 × 500ms
  (async () => {
    const { stdout } = await run('tmux', ['display-message', '-p', '-t', name, '#{pane_pid}']);
    const pid = stdout.trim();
    for (let i = 0; i < 60; i++) {
      await new Promise(r => setTimeout(r, 500));
      if (!ctx.meta.has(name)) return; // killed while polling
      const link = await rcLinkFor(ctx.claudeDir, pid);
      if (link) {
        if (!ctx.meta.has(name)) return; // killed between check and write
        ctx.meta.set(name, { ...ctx.meta.get(name), rcLink: link });
        log(`rc-link captured ${name} (${(i + 1) / 2}s)`);
        return;
      }
    }
    log(`rc-link timeout ${name} (30s)`);
  })().catch(e => log(`rc-poll error ${name}: ${e.message}`));
  return name;
}

export async function launchSession(ctx, { dir, model = 'sonnet', effort = 'medium' }) {
  if (!MODELS.has(model)) throw Object.assign(new Error(`bad model: ${model}`), { status: 400 });
  if (!EFFORTS.has(effort)) throw Object.assign(new Error(`bad effort: ${effort}`), { status: 400 });
  if (!dir || !(await fs.stat(dir).catch(() => null))?.isDirectory())
    throw Object.assign(new Error(`not a directory: ${dir}`), { status: 400 });
  return spawnClaude(ctx, dir, ['--model', model, '--effort', effort], { model, effort });
}

const SID_RE = /^[0-9a-f-]{36}$/i;
export function assertValidSid(sid) {
  if (!SID_RE.test(sid || '')) throw Object.assign(new Error(`bad sid: ${sid}`), { status: 400 });
}

export async function resumeSession(ctx, sid) {
  assertValidSid(sid);
  const hist = await fs.readFile(path.join(ctx.claudeDir, 'history.jsonl'), 'utf8');
  const g = S.groupHistory(hist).find(x => x.sid === sid);
  if (!g?.cwd) throw Object.assign(new Error(`unknown session: ${sid}`), { status: 400 });
  ctx.purged.delete(sid);
  return spawnClaude(ctx, g.cwd, ['--resume', sid], { model: null, effort: null });
}

// Claude sessions this dashboard did not launch: every ~/.claude/sessions/<pid>.json
// whose pid is still alive. Read-only — they live in terminals we don't own.
async function externalSessions(ctx, psOut, panePids, now) {
  const alive = new Set(psOut.split('\n').filter(Boolean).map(l => Number(l.trim().split(/\s+/)[0])));
  let files = [];
  try { files = await fs.readdir(path.join(ctx.claudeDir, 'sessions')); } catch { return []; }

  const out = await Promise.all(files.map(async f => {
    const pid = Number(path.basename(f, '.json'));
    if (!f.endsWith('.json') || !pid || panePids.has(pid) || !alive.has(pid)) return null;
    const sess = await sessionFileFor(ctx.claudeDir, pid);
    if (!sess?.sessionId || !sess.cwd) return null;
    // entrypoint 'cli' is a session someone is sitting in front of; 'sdk-cli'
    // is programmatic (claude-mem observers, SDK runs) and not ours to show.
    if (sess.entrypoint !== 'cli') return null;

    const file = path.join(ctx.claudeDir, 'projects', S.projectDirName(sess.cwd), `${sess.sessionId}.jsonl`);
    const st = await fs.stat(file).catch(() => null);
    const gitOut = gitStatusFor(sess.cwd, now);
    let lastMessage = null;
    if (st) {
      const txt = await readTail(file);
      if (txt) lastMessage = S.parseTranscript(txt).lastAssistantText;
    }
    return {
      name: sess.name || path.basename(sess.cwd), dir: sess.cwd, pid,
      uptimeSec: sess.startedAt ? Math.max(0, Math.round((now - sess.startedAt) / 1000)) : 0,
      model: null, effort: null, external: true,
      rcLink: sess.bridgeSessionId ? `https://claude.ai/code/${sess.bridgeSessionId}` : null,
      git: gitOut ? S.parseGitStatus(gitOut) : null,
      working: st ? now - st.mtimeMs < 10_000 : false,
      lastMessage, sid: sess.sessionId,
      ...procTreeUsage(psOut, pid),
    };
  }));
  return out.filter(Boolean);
}

export async function collectSessions(ctx) {
  const [panesOut, psOut, dfOut] = await Promise.all([
    sh('tmux', ['list-panes', '-a', '-F', '#{session_name}|#{pane_pid}|#{pane_current_path}|#{session_created}']),
    sh('ps', ['-eo', 'pid=,ppid=,%cpu=,rss=']),
    sh('df', ['-k', '--output=target,avail,size', '/', ...(ctx.diskExtra ? [ctx.diskExtra] : [])]),
  ]);
  const panes = S.parseTmuxPanes(panesOut);
  const now = Date.now();

  const running = await Promise.all(panes.map(async p => {
    const meta = ctx.meta.get(p.name) || {};
    const sess = await sessionFileFor(ctx.claudeDir, p.pid);
    const gitOut = gitStatusFor(p.path, now);
    const rcLink = meta.rcLink
      || (sess?.bridgeSessionId ? `https://claude.ai/code/${sess.bridgeSessionId}` : null);
    if (rcLink && !meta.rcLink) ctx.meta.set(p.name, { ...meta, rcLink });

    // Prefer the pane's own session id; only guess from mtime if the session
    // file has no id yet (several panes can share one cwd).
    let tr = null;
    if (sess?.sessionId) {
      const file = path.join(ctx.claudeDir, 'projects', S.projectDirName(sess.cwd || p.path), `${sess.sessionId}.jsonl`);
      const st = await fs.stat(file).catch(() => null);
      if (st) tr = { file, mtimeMs: st.mtimeMs };
    }
    if (!tr) tr = await transcriptFor(ctx.claudeDir, p.path, p.created);

    let working = false, lastMessage = null, sid = null;
    if (tr) {
      working = now - tr.mtimeMs < 10_000;
      sid = path.basename(tr.file, '.jsonl');
      const txt = await readTail(tr.file);
      if (txt) lastMessage = S.parseTranscript(txt).lastAssistantText;
    }
    return {
      name: p.name, dir: p.path, pid: p.pid,
      uptimeSec: Math.max(0, Math.round(now / 1000 - p.created)),
      model: meta.model || null, effort: meta.effort || null, rcLink,
      git: gitOut ? S.parseGitStatus(gitOut) : null,
      working, lastMessage, sid,
      ...procTreeUsage(psOut, p.pid),
    };
  }));

  running.push(...await externalSessions(ctx, psOut, new Set(panes.map(p => p.pid)), now));

  const runningSids = new Set(running.map(r => r.sid).filter(Boolean));
  const histText = (await readIf(path.join(ctx.claudeDir, 'history.jsonl'))) || '';
  const resumable = [];
  for (const g of S.groupHistory(histText)) {
    if (resumable.length >= 20) break;
    if (runningSids.has(g.sid) || ctx.purged.has(g.sid)) continue;
    const t = await parseTranscriptCached(path.join(ctx.claudeDir, 'projects', S.projectDirName(g.cwd || ''), `${g.sid}.jsonl`));
    if (!t) continue;
    if (t.assistantCount < 3) continue;
    resumable.push({ sid: g.sid, dir: g.cwd, ts: g.ts, branch: t.branch, title: t.title || g.prompts[0] || '(untitled)', prompts: g.prompts });
  }

  return { running, resumable, stats: { ...machineStats(), disks: parseDf(dfOut) } };
}
