const $ = s => document.querySelector(s);
const MODELS = ['sonnet', 'opus', 'haiku', 'fable'];
const EFFORTS = ['low', 'medium', 'high', 'xhigh', 'max'];
let model = 'sonnet', effort = 'medium';
let armedKill = null; // ponytail: survives render() replacing #running.innerHTML

/* ---------- Inline icons (stroke = currentColor, sized via CSS) ---------- */
const svg = body => `<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">${body}</svg>`;
const ICONS = {
  play: `<svg viewBox="0 0 24 24" fill="currentColor" aria-hidden="true"><path d="M7 4.5v15a1 1 0 0 0 1.5.87l12-7.5a1 1 0 0 0 0-1.74l-12-7.5A1 1 0 0 0 7 4.5z"/></svg>`,
  external: svg('<path d="M14 4h6v6"/><path d="M20 4l-9 9"/><path d="M18 14v4a2 2 0 0 1-2 2H6a2 2 0 0 1-2-2V8a2 2 0 0 1 2-2h4"/>'),
  x: svg('<path d="M18 6 6 18"/><path d="M6 6l12 12"/>'),
  refresh: svg('<path d="M21 12a9 9 0 1 1-3-6.7"/><path d="M21 4v5h-5"/>'),
  trash: svg('<path d="M4 7h16"/><path d="M9 7V5a1 1 0 0 1 1-1h4a1 1 0 0 1 1 1v2"/><path d="M18 7l-.8 12a2 2 0 0 1-2 1.9H8.8a2 2 0 0 1-2-1.9L6 7"/>'),
  spinner: svg('<path d="M12 3a9 9 0 1 0 9 9"/>'),
  sun: svg('<circle cx="12" cy="12" r="4"/><path d="M12 2v2M12 20v2M4.9 4.9l1.4 1.4M17.7 17.7l1.4 1.4M2 12h2M20 12h2M4.9 19.1l1.4-1.4M17.7 6.3l1.4-1.4"/>'),
  moon: svg('<path d="M21 12.8A9 9 0 1 1 11.2 3a7 7 0 0 0 9.8 9.8z"/>'),
};

/* ---------- Theme ---------- */
const THEME_KEY = 'cdash-theme';
const curTheme = () => document.documentElement.dataset.theme === 'dark' ? 'dark' : 'light';
function applyTheme(theme) {
  document.documentElement.dataset.theme = theme;
  const dark = theme === 'dark';
  const tt = $('#theme-toggle');
  tt.innerHTML = dark ? ICONS.sun : ICONS.moon;
  tt.setAttribute('aria-label', dark ? 'Switch to light theme' : 'Switch to dark theme');
  document.querySelectorAll('meta[name="theme-color"]').forEach(m => m.remove());
  const m = document.createElement('meta');
  m.name = 'theme-color';
  m.content = dark ? '#1a1917' : '#f2f0e9';
  document.head.appendChild(m);
}
$('#theme-toggle').onclick = () => {
  const next = curTheme() === 'dark' ? 'light' : 'dark';
  localStorage.setItem(THEME_KEY, next);
  applyTheme(next);
};
matchMedia('(prefers-color-scheme: dark)').addEventListener('change', e => {
  if (!localStorage.getItem(THEME_KEY)) applyTheme(e.matches ? 'dark' : 'light');
});
applyTheme(curTheme());

/* ---------- Segmented controls ---------- */
function seg(el, items, sel, onpick) {
  el.innerHTML = items.map(i => `<button type="button" data-v="${i}" class="${i === sel ? 'on' : ''}">${i}</button>`).join('');
  el.onclick = e => { const v = e.target.dataset.v; if (v) { onpick(v); seg(el, items, v, onpick); } };
}
seg($('#model'), MODELS, model, v => model = v);
seg($('#effort'), EFFORTS, effort, v => effort = v);

function toast(msg) {
  const t = $('#toast');
  t.textContent = msg; t.classList.add('show');
  setTimeout(() => t.classList.remove('show'), 4000);
}

async function api(path, body) {
  const res = await fetch(path, body ? { method: 'POST', headers: { 'content-type': 'application/json' }, body: JSON.stringify(body) } : undefined);
  const data = await res.json().catch(() => ({}));
  if (!res.ok) throw new Error(data.error || res.statusText);
  return data;
}

const esc = s => (s || '').replace(/[&<>"]/g, c => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;' }[c]));
const fmtUp = s => s >= 3600 ? `${Math.floor(s / 3600)}h ${Math.floor(s % 3600 / 60)}m` : s >= 60 ? `${Math.floor(s / 60)}m` : `${s}s`;
const fmtKb = k => k >= 1048576 ? `${(k / 1048576).toFixed(1)}G` : `${Math.round(k / 1024)}M`;
const fmtTs = t => new Date(t < 1e12 ? t * 1000 : t).toISOString().slice(5, 16).replace('T', ' '); // history.jsonl timestamps may be seconds or ms

function gitBadge(g) {
  if (!g?.branch) return '';
  let s = esc(g.branch);
  if (g.dirty) s += ` ●${g.dirty}`;
  if (g.ahead) s += ` ↑${g.ahead}`;
  if (g.behind) s += ` ↓${g.behind}`;
  return `<span class="branch">${s}</span>`;
}

function statTile({ label, value, pct }) {
  const cls = pct == null ? '' : pct >= 90 ? 'crit' : pct >= 75 ? 'warn' : '';
  const meter = pct == null ? ''
    : `<div class="meter"><div class="meter-fill ${cls}" style="width:${Math.min(100, Math.max(0, pct))}%"></div></div>`;
  return `<div class="stat"><div class="stat-label">${label}</div><div class="stat-value">${value}</div>${meter}</div>`;
}

function runningCard(r) {
  const status = r.working ? 'working' : 'idle';
  const chips = [r.model, r.effort].filter(Boolean);
  const chipHtml = chips.length
    ? chips.map(x => `<span class="chip">${esc(x)}</span>`).join('')
    : `<span class="chip">${r.external ? 'external' : 'resumed'}</span>`;
  const peek = r.lastMessage
    ? `<div class="peek" onclick="this.classList.toggle('open')"><span class="peek-text">${esc(r.lastMessage)}</span></div>` : '';
  const rc = r.rcLink
    ? `<a class="action primary" href="${esc(r.rcLink)}" target="_blank" rel="noopener">${ICONS.external}<span>Open in Claude</span></a>`
    : `<span class="action pending">${ICONS.spinner}<span>Waiting for link…</span></span>`;
  const kill = r.external ? ''
    : `<button class="action danger" type="button" data-kill="${esc(r.name)}" ${r.name === armedKill ? 'data-arm="1"' : ''} aria-label="Kill session">${r.name === armedKill ? 'Sure?' : ICONS.x}</button>`;
  return `
    <div class="session ${status} ${r.external ? 'external' : ''}">
      <div class="session-head">
        <span class="session-title">${esc(r.dir.split('/').pop())}</span>
        ${gitBadge(r.git)}
        ${r.external ? '<span class="tag">external</span>' : ''}
        <span class="status ${r.working ? 'on' : 'off'}"><span class="dot"></span>${r.working ? 'Working' : 'Waiting'}</span>
      </div>
      <div class="session-meta">
        ${chipHtml}<span>cpu ${r.cpu}%</span><span class="sep">·</span><span>${fmtKb(r.rssKb)}</span>
        <span class="time">${fmtUp(r.uptimeSec)}</span>
      </div>
      ${peek}
      <div class="actions">${rc}${kill}</div>
    </div>`;
}

function resumableCard(s) {
  return `
    <div class="session resumable">
      <div class="session-head">
        <span class="session-title">${esc(s.dir?.split('/').pop() || '?')}</span>
        ${gitBadge({ branch: s.branch })}
        <span class="time">${fmtTs(s.ts)}</span>
      </div>
      <div class="desc">${esc(s.title)}</div>
      <div class="prompts">↳ ${esc(s.prompts.join(' · '))}</div>
      <div class="actions">
        <button class="action" type="button" data-resume="${esc(s.sid)}">${ICONS.refresh}<span>Resume</span></button>
        <button class="action ghost" type="button" data-purge="${esc(s.sid)}" aria-label="Purge from list">${ICONS.trash}</button>
      </div>
    </div>`;
}

function render(d) {
  $('#nrun').textContent = d.running.length;
  $('#nres').textContent = d.resumable.length;

  const st = d.stats;
  const tiles = [
    { label: 'CPU', value: `${st.cpuPct}<span class="unit">%</span>`, pct: st.cpuPct },
    { label: 'RAM', value: `${fmtKb(st.ramUsedKb)}<span class="unit"> / ${fmtKb(st.ramTotalKb)}</span>`, pct: st.ramTotalKb ? Math.round(st.ramUsedKb / st.ramTotalKb * 100) : null },
    ...st.disks.map(x => ({ label: esc(x.mount), value: `${fmtKb(x.freeKb)}<span class="unit"> free</span>`, pct: x.totalKb ? Math.round((x.totalKb - x.freeKb) / x.totalKb * 100) : null })),
  ];
  $('#stats').innerHTML = tiles.map(statTile).join('');

  $('#running').innerHTML = d.running.map(runningCard).join('') || '<div class="empty">No running sessions</div>';
  $('#resumable').innerHTML = d.resumable.map(resumableCard).join('') || '<div class="empty">No resumable sessions</div>';

  const dirs = [...new Set(d.resumable.map(s => s.dir).filter(Boolean))];
  $('#dirs').innerHTML = dirs.map(x => `<option value="${esc(x)}">`).join('');
}

document.body.addEventListener('click', async e => {
  const el = e.target.closest('[data-kill],[data-resume],[data-purge]');
  if (!el) return;
  try {
    if (el.dataset.kill) {
      if (el.dataset.arm) { armedKill = null; await api('/api/kill', { name: el.dataset.kill }); poll(); }
      else {
        armedKill = el.dataset.kill; el.dataset.arm = '1'; el.textContent = 'Sure?';
        setTimeout(() => { if (armedKill === el.dataset.kill) armedKill = null; delete el.dataset.arm; el.innerHTML = ICONS.x; }, 3000);
      }
    } else if (el.dataset.resume) {
      el.disabled = true;
      try { await api('/api/resume', { sid: el.dataset.resume }); poll(); } finally { el.disabled = false; }
    } else if (el.dataset.purge) {
      await api('/api/purge', { sid: el.dataset.purge }); poll();
    }
  } catch (err) { toast(err.message); }
});

$('#launch').onclick = async () => {
  const btn = $('#launch');
  const dir = $('#dir').value.trim();
  if (!dir) { toast('Enter a project directory first'); $('#dir').focus(); return; }
  btn.disabled = true; btn.innerHTML = `${ICONS.play}<span>Launching…</span>`;
  try { await api('/api/launch', { dir, model, effort }); $('#dir').value = ''; poll(); }
  catch (err) { toast(err.message); }
  finally { btn.disabled = false; btn.innerHTML = `${ICONS.play}<span>Launch session</span>`; }
};

async function poll() {
  try {
    render(await api('/api/sessions'));
    $('#health').className = 'dot ok';
    $('#health-label').textContent = 'Connected';
    if ($('#logbox').open) $('#logs').textContent = (await api('/api/logs')).lines.join('\n');
  } catch {
    $('#health').className = 'dot bad';
    $('#health-label').textContent = 'Offline';
  }
}

// Give the launch button its resting icon + label.
$('#launch').innerHTML = `${ICONS.play}<span>Launch session</span>`;

poll();
setInterval(() => { if (!document.hidden) poll(); }, 4000);
document.addEventListener('visibilitychange', () => { if (!document.hidden) poll(); });
