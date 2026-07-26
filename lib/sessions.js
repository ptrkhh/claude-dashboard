const STOPWORDS = new Set(['continue', 'resume', 'exit', 'usage', 'ok', 'yes', 'no', 'quit', 'y', 'n']);

export function usablePrompts(displays) {
  return displays.filter(d => {
    const t = (d || '').trim();
    return t && !t.startsWith('/') && !STOPWORDS.has(t.toLowerCase());
  });
}

function parseLines(text) {
  const out = [];
  for (const line of text.split('\n')) {
    if (!line.trim()) continue;
    try { out.push(JSON.parse(line)); } catch { /* skip malformed */ }
  }
  return out;
}

export function groupHistory(jsonlText) {
  const bySid = new Map();
  for (const e of parseLines(jsonlText)) {
    if (!e.sessionId) continue;
    const g = bySid.get(e.sessionId) || { sid: e.sessionId, cwd: e.project, ts: 0, displays: [] };
    g.cwd = e.project ?? g.cwd;
    g.ts = Math.max(g.ts, e.timestamp || 0);
    if (typeof e.display === 'string') g.displays.push(e.display);
    bySid.set(e.sessionId, g);
  }
  return [...bySid.values()]
    .sort((a, b) => b.ts - a.ts)
    .slice(0, 60)
    .map(({ displays, ...g }) => ({ ...g, prompts: usablePrompts(displays).slice(-3) }));
}

export function parseTranscript(jsonlText) {
  let branch = null, title = null, assistantCount = 0, lastAssistantText = null;
  for (const e of parseLines(jsonlText)) {
    if (branch === null && e.gitBranch && e.gitBranch !== 'HEAD') branch = e.gitBranch;
    if (title === null && e.type === 'ai-title' && e.aiTitle) title = e.aiTitle;
    if (e.type === 'assistant') {
      assistantCount++;
      const txt = (e.message?.content || []).find(c => c.type === 'text')?.text;
      if (txt) lastAssistantText = txt;
    }
  }
  return { branch, title, assistantCount, lastAssistantText };
}

export function parseRcFile(jsonText) {
  try { return JSON.parse(jsonText).bridgeSessionId ?? null; } catch { return null; }
}

export function parseTmuxPanes(out) {
  return out.split('\n').filter(Boolean).map(l => {
    const [name, pid, path, created] = l.split('|');
    return { name, pid: Number(pid), path, created: Number(created) };
  }).filter(p => p.name?.startsWith('cdash-'));
}

export function parseGitStatus(out) {
  const lines = out.split('\n').filter(Boolean);
  const head = lines[0] || '';
  const branch = head.replace(/^## /, '').split('...')[0].trim();
  return {
    branch,
    dirty: lines.length - 1,
    ahead: Number(head.match(/ahead (\d+)/)?.[1] || 0),
    behind: Number(head.match(/behind (\d+)/)?.[1] || 0),
  };
}

export function projectDirName(cwd) {
  return cwd.replace(/[^a-zA-Z0-9]/g, '-');
}
