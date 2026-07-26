import { execFile } from 'node:child_process';
import { promisify } from 'node:util';
import fs from 'node:fs/promises';
import path from 'node:path';
import * as S from './sessions.js';
import { procTreeUsage, parseDf, machineStats } from './stats.js';

const run = promisify(execFile);
const sh = async (cmd, args) => { try { return (await run(cmd, args)).stdout; } catch { return ''; } };

export const logBuffer = [];
export function log(msg) {
  const line = `${new Date().toTimeString().slice(0, 8)} ${msg}`;
  logBuffer.push(line);
  if (logBuffer.length > 200) logBuffer.shift();
  console.log(line);
}

async function readIf(file) { try { return await fs.readFile(file, 'utf8'); } catch { return null; } }

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
  await fs.writeFile(file, JSON.stringify(cfg, null, 2));
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
      const link = await rcLinkFor(ctx.claudeDir, pid);
      if (link) {
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

export async function resumeSession(ctx, sid) {
  const hist = await fs.readFile(path.join(ctx.claudeDir, 'history.jsonl'), 'utf8');
  const g = S.groupHistory(hist).find(x => x.sid === sid);
  if (!g?.cwd) throw Object.assign(new Error(`unknown session: ${sid}`), { status: 400 });
  ctx.purged.delete(sid);
  return spawnClaude(ctx, g.cwd, ['--resume', sid], { model: null, effort: null });
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
    const [rcLink, gitOut, tr] = await Promise.all([
      meta.rcLink ? meta.rcLink : rcLinkFor(ctx.claudeDir, p.pid),
      sh('git', ['-C', p.path, 'status', '--porcelain=v1', '-b']),
      transcriptFor(ctx.claudeDir, p.path, p.created),
    ]);
    if (rcLink && !meta.rcLink) ctx.meta.set(p.name, { ...meta, rcLink });
    let working = false, lastMessage = null, sid = null;
    if (tr) {
      working = now - tr.mtimeMs < 10_000;
      sid = path.basename(tr.file, '.jsonl');
      const txt = await readIf(tr.file);
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

  const runningSids = new Set(running.map(r => r.sid).filter(Boolean));
  const histText = (await readIf(path.join(ctx.claudeDir, 'history.jsonl'))) || '';
  const resumable = [];
  for (const g of S.groupHistory(histText)) {
    if (resumable.length >= 20) break;
    if (runningSids.has(g.sid) || ctx.purged.has(g.sid)) continue;
    const txt = await readIf(path.join(ctx.claudeDir, 'projects', S.projectDirName(g.cwd || ''), `${g.sid}.jsonl`));
    if (!txt) continue;
    const t = S.parseTranscript(txt);
    if (t.assistantCount < 3) continue;
    resumable.push({ sid: g.sid, dir: g.cwd, ts: g.ts, branch: t.branch, title: t.title || g.prompts[0] || '(untitled)', prompts: g.prompts });
  }

  return { running, resumable, stats: { ...machineStats(), disks: parseDf(dfOut) } };
}
