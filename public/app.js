const $ = s => document.querySelector(s);
const MODELS = ['sonnet', 'opus', 'haiku', 'fable'];
const EFFORTS = ['low', 'medium', 'high', 'xhigh', 'max'];
let model = 'sonnet', effort = 'medium';
let armedKill = null; // ponytail: survives render() replacing #running.innerHTML

function seg(el, items, sel, onpick) {
  el.innerHTML = items.map(i => `<button data-v="${i}" class="${i === sel ? 'on' : ''}">${i}</button>`).join('');
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
  return `<span class="branch">(${s})</span>`;
}

function render(d) {
  $('#nrun').textContent = d.running.length;
  $('#nres').textContent = d.resumable.length;
  const st = d.stats;
  $('#stats').innerHTML = [
    `CPU<br><b>${st.cpuPct}%</b>`,
    `RAM<br><b>${fmtKb(st.ramUsedKb)}/${fmtKb(st.ramTotalKb)}</b>`,
    ...st.disks.map(x => `${esc(x.mount)}<br><b>${fmtKb(x.freeKb)} free</b>`),
  ].map(h => `<div>${h}</div>`).join('');

  $('#running').innerHTML = d.running.map(r => `
    <div class="card run ${r.working ? 'working' : 'idle'} ${r.external ? 'ext' : ''}">
      <div class="row"><b>${esc(r.dir.split('/').pop())}</b> ${gitBadge(r.git)}
        ${r.external ? '<span class="tag">external</span>' : ''}
        <span class="badge">${r.working ? '● working' : '○ waiting'}</span><span class="dim">${fmtUp(r.uptimeSec)}</span></div>
      <div class="dim">${[r.model, r.effort].filter(Boolean).map(esc).join(' · ') || (r.external ? 'external' : 'resumed')} · cpu ${r.cpu}% · ${fmtKb(r.rssKb)}</div>
      ${r.lastMessage ? `<div class="peek" onclick="this.classList.toggle('open')">${esc(r.lastMessage)}</div>` : ''}
      <div class="row">
        ${r.rcLink
          ? `<a class="btn rc" href="${esc(r.rcLink)}" target="_blank" rel="noopener">Open Remote Control ↗</a>`
          : `<span class="btn wait">⏳ waiting for RC link…</span>`}
        ${r.external ? '' : `<button class="btn kill" data-kill="${esc(r.name)}" ${r.name === armedKill ? 'data-arm="1"' : ''}>${r.name === armedKill ? 'sure?' : '✕'}</button>`}
      </div>
    </div>`).join('') || '<p class="dim">none</p>';

  $('#resumable').innerHTML = d.resumable.map(s => `
    <div class="card res">
      <div class="row"><b>${esc(s.dir?.split('/').pop() || '?')}</b> ${gitBadge({ branch: s.branch })}<span class="dim">${fmtTs(s.ts)}</span></div>
      <div>${esc(s.title)}</div>
      <div class="dim prompts">↳ ${esc(s.prompts.join(' · '))}</div>
      <div class="row">
        <button class="btn" data-resume="${esc(s.sid)}">↻ Resume</button>
        <button class="btn dim" data-purge="${esc(s.sid)}">Purge</button>
      </div>
    </div>`).join('') || '<p class="dim">none</p>';

  const dirs = [...new Set(d.resumable.map(s => s.dir).filter(Boolean))];
  $('#dirs').innerHTML = dirs.map(x => `<option value="${esc(x)}">`).join('');
}

document.body.addEventListener('click', async e => {
  const b = e.target;
  try {
    if (b.dataset.kill) { if (b.dataset.arm) { armedKill = null; await api('/api/kill', { name: b.dataset.kill }); poll(); } else { armedKill = b.dataset.kill; b.dataset.arm = '1'; b.textContent = 'sure?'; setTimeout(() => { if (armedKill === b.dataset.kill) armedKill = null; delete b.dataset.arm; b.textContent = '✕'; }, 3000); } }
    else if (b.dataset.resume) { b.disabled = true; try { await api('/api/resume', { sid: b.dataset.resume }); poll(); } finally { b.disabled = false; } }
    else if (b.dataset.purge) { await api('/api/purge', { sid: b.dataset.purge }); poll(); }
  } catch (err) { toast(err.message); }
});

$('#launch').onclick = async () => {
  const btn = $('#launch');
  const dir = $('#dir').value.trim();
  if (!dir) { toast('Enter a project directory first'); $('#dir').focus(); return; }
  btn.disabled = true; btn.textContent = '⏳ launching…';
  try { await api('/api/launch', { dir, model, effort }); $('#dir').value = ''; poll(); }
  catch (err) { toast(err.message); }
  finally { btn.disabled = false; btn.textContent = '▶ Launch Session'; }
};

async function poll() {
  try {
    render(await api('/api/sessions'));
    $('#health').className = 'dot ok';
    if ($('#logbox').open) $('#logs').textContent = (await api('/api/logs')).lines.join('\n');
  } catch { $('#health').className = 'dot bad'; }
}
poll();
setInterval(() => { if (!document.hidden) poll(); }, 4000);
document.addEventListener('visibilitychange', () => { if (!document.hidden) poll(); });
