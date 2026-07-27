import os from 'node:os';

export function procTreeUsage(psOut, rootPid) {
  const rows = psOut.split('\n').filter(Boolean).map(l => {
    const [pid, ppid, cpu, rss] = l.trim().split(/\s+/);
    return { pid: Number(pid), ppid: Number(ppid), cpu: Number(cpu), rss: Number(rss) };
  });
  const children = new Map();
  for (const r of rows) {
    if (!children.has(r.ppid)) children.set(r.ppid, []);
    children.get(r.ppid).push(r);
  }
  const byPid = new Map(rows.map(r => [r.pid, r]));
  let cpu = 0, rssKb = 0;
  const stack = byPid.has(rootPid) ? [rootPid] : [];
  while (stack.length) {
    const pid = stack.pop();
    const r = byPid.get(pid);
    if (r) { cpu += r.cpu; rssKb += r.rss; }
    for (const c of children.get(pid) || []) stack.push(c.pid);
  }
  return { cpu: Math.round(cpu * 10) / 10, rssKb };
}

export function parseDf(dfOut) {
  return dfOut.split('\n').slice(1).filter(Boolean).map(l => {
    const [mount, avail, size] = l.trim().split(/\s+/);
    return { mount, freeKb: Number(avail), totalKb: Number(size) };
  });
}

export function machineStats() {
  const cpuPct = Math.min(100, Math.round((os.loadavg()[0] / os.cpus().length) * 100));
  return { cpuPct, ramUsedKb: Math.round((os.totalmem() - os.freemem()) / 1024), ramTotalKb: Math.round(os.totalmem() / 1024) };
}
