const $ = s => document.querySelector(s);
const MODELS = ['sonnet', 'opus', 'haiku', 'fable'];
const EFFORTS = ['low', 'medium', 'high', 'xhigh', 'max'];
let armedKill = null; // ponytail: survives render() replacing #running.innerHTML
const cap = s => s.charAt(0).toUpperCase() + s.slice(1);

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
  system: svg('<rect x="3" y="4" width="18" height="13" rx="2"/><path d="M8 21h8M12 17v4"/>'),
  folder: svg('<path d="M3 7a2 2 0 0 1 2-2h4l2 2h8a2 2 0 0 1 2 2v8a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2z"/>'),
  chevronRight: svg('<path d="M9 6l6 6-6 6"/>'),
  clock: svg('<circle cx="12" cy="12" r="9"/><path d="M12 7v5l3 2"/>'),
  star: svg('<path d="M12 3.5l2.6 5.3 5.9.9-4.3 4.1 1 5.8-5.2-2.7-5.2 2.7 1-5.8L3.5 9.7l5.9-.9z"/>'),
  starFilled: `<svg viewBox="0 0 24 24" fill="currentColor" stroke="currentColor" stroke-width="1.5" stroke-linejoin="round" aria-hidden="true"><path d="M12 3.5l2.6 5.3 5.9.9-4.3 4.1 1 5.8-5.2-2.7-5.2 2.7 1-5.8L3.5 9.7l5.9-.9z"/></svg>`,
};

/* ---------- Theme ----------
   Three modes cycled by the toggle: system → light → dark → system.
   'system' (the default, no stored value) follows the OS via prefers-color-scheme
   and stays live if the OS setting changes. */
const THEME_KEY = 'cdash-theme';

/* An Android WebView can have DOM storage disabled, where *reading*
   `window.localStorage` throws rather than returning null. Unguarded, that
   killed this file on its first line of theme code — before the launcher, the
   dropdowns or the poll loop existed — and the app came up as a dead shell on
   "Connecting…". A theme that does not persist is the acceptable cost. */
const store = {
  get(k) { try { return localStorage.getItem(k); } catch { return null; } },
  set(k, v) { try { localStorage.setItem(k, v); } catch { /* not persisted */ } },
  remove(k) { try { localStorage.removeItem(k); } catch { /* nothing to clear */ } },
};
const MODES = ['system', 'light', 'dark'];
const systemDark = () => matchMedia('(prefers-color-scheme: dark)').matches;
const themeMode = () => { const s = store.get(THEME_KEY); return s === 'light' || s === 'dark' ? s : 'system'; };
const resolved = mode => mode === 'system' ? (systemDark() ? 'dark' : 'light') : mode;

function applyMode(mode) {
  const dark = resolved(mode) === 'dark';
  document.documentElement.dataset.theme = dark ? 'dark' : 'light';
  const tt = $('#theme-toggle');
  tt.innerHTML = mode === 'system' ? ICONS.system : dark ? ICONS.moon : ICONS.sun;
  const label = mode === 'system' ? 'Theme: system — click to override' : `Theme: ${mode} — click to change`;
  tt.setAttribute('aria-label', label);
  tt.title = label;
  document.querySelectorAll('meta[name="theme-color"]').forEach(m => m.remove());
  const m = document.createElement('meta');
  m.name = 'theme-color';
  m.content = dark ? '#1a1917' : '#f2f0e9';
  document.head.appendChild(m);
}
$('#theme-toggle').onclick = () => {
  const next = MODES[(MODES.indexOf(themeMode()) + 1) % MODES.length];
  if (next === 'system') store.remove(THEME_KEY);
  else store.set(THEME_KEY, next);
  applyMode(next);
};
matchMedia('(prefers-color-scheme: dark)').addEventListener('change', () => {
  if (themeMode() === 'system') applyMode('system');
});
applyMode(themeMode());

/* ---------- Launcher controls ----------
   Custom dropdown: a native <select> can't be themed and double-glowed inside
   its wrapper. Fully keyboard-accessible; closes on outside click / Escape. */
function buildDropdown(el, label, items, initial) {
  let value = initial;
  el.innerHTML = `
    <div class="cb-trigger" role="button" tabindex="0" aria-haspopup="listbox" aria-expanded="false" aria-label="${label}">
      <span class="cb-lead">${label}</span><span class="cb-current">${cap(value)}</span><span class="cb-caret">▾</span>
    </div>
    <ul class="cb-menu" role="listbox" hidden>
      ${items.map(i => `<li role="option" data-value="${i}" class="${i === value ? 'on' : ''}" aria-selected="${i === value}">${cap(i)}</li>`).join('')}
    </ul>`;
  const trigger = el.querySelector('.cb-trigger');
  const menu = el.querySelector('.cb-menu');
  const current = el.querySelector('.cb-current');
  const opts = [...menu.querySelectorAll('li')];
  let active = -1;
  const isOpen = () => !menu.hidden;
  const setActive = i => { active = i; opts.forEach((o, idx) => o.classList.toggle('active', idx === i)); if (opts[i]) opts[i].scrollIntoView({ block: 'nearest' }); };
  const open = () => {
    document.querySelectorAll('.cb-menu:not([hidden])').forEach(m => { if (m !== menu) { m.hidden = true; m.previousElementSibling.setAttribute('aria-expanded', 'false'); } });
    menu.hidden = false; trigger.setAttribute('aria-expanded', 'true');
    setActive(Math.max(0, opts.findIndex(o => o.dataset.value === value)));
  };
  const close = () => { menu.hidden = true; trigger.setAttribute('aria-expanded', 'false'); setActive(-1); };
  const pick = li => {
    value = li.dataset.value; current.textContent = cap(value);
    opts.forEach(o => { const on = o === li; o.classList.toggle('on', on); o.setAttribute('aria-selected', on); });
    close(); trigger.focus();
  };
  trigger.addEventListener('click', e => { e.stopPropagation(); isOpen() ? close() : open(); });
  menu.addEventListener('click', e => { const li = e.target.closest('[data-value]'); if (li) pick(li); });
  el.addEventListener('keydown', e => {
    if (e.key === 'Escape') { if (isOpen()) { close(); trigger.focus(); } return; }
    if (e.key === 'ArrowDown') { e.preventDefault(); isOpen() ? setActive(Math.min(opts.length - 1, active + 1)) : open(); return; }
    if (e.key === 'ArrowUp') { e.preventDefault(); if (isOpen()) setActive(Math.max(0, active - 1)); return; }
    if (e.key === 'Enter' || e.key === ' ') { e.preventDefault(); if (!isOpen()) open(); else if (opts[active]) pick(opts[active]); }
  });
  document.addEventListener('click', e => { if (isOpen() && !el.contains(e.target)) close(); });
  return { get value() { return value; } };
}
const modelDD = buildDropdown($('#model'), 'Model', MODELS, 'sonnet');
const effortDD = buildDropdown($('#effort'), 'Effort', EFFORTS, 'medium');

function toast(msg) {
  const t = $('#toast');
  t.textContent = msg; t.classList.add('show');
  setTimeout(() => t.classList.remove('show'), 4000);
}

// Confirmed: tauri.conf.json sets withGlobalTauri, so the runtime injects
// window.__TAURI__ (and __TAURI_INTERNALS__ unconditionally) in the webview
// and neither on the web.
const isTauri = typeof window !== 'undefined' &&
  ('__TAURI_INTERNALS__' in window || '__TAURI__' in window);

const invoke = (...a) => {
  const f = window.__TAURI__?.core?.invoke;
  if (!f) throw new Error('Tauri IPC unavailable (withGlobalTauri off?)');
  return f(...a);
};

async function api(path, body) {
  if (isTauri) {
    let res;
    try {
      res = await invoke('api_request', { method: body ? 'POST' : 'GET', path, body });
    } catch (e) {
      // invoke rejects with the command's Err(String), not an Error.
      throw new Error(String(e?.message ?? e));
    }
    // api_request returns { status, body } without throwing on non-2xx;
    // re-raise as an Error to keep api()'s fetch-style contract.
    if (res.status >= 400) {
      const err = new Error(res.body?.error || res.body?.message || `HTTP ${res.status}`);
      err.status = res.status;
      throw err;
    }
    return res.body;
  }
  const opts = body
    ? { method: 'POST', headers: { 'content-type': 'application/json' }, body: JSON.stringify(body) }
    : undefined;
  const res = await fetch(path, opts);
  const data = await res.json().catch(() => ({}));
  if (!res.ok) {
    const err = new Error(data.error || res.statusText);
    err.status = res.status;
    throw err;
  }
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
  return `<div class="stat"><div class="stat-top"><span class="stat-label">${label}</span><span class="stat-value">${value}</span></div>${meter}</div>`;
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
        <div class="session-headline">
          <span class="session-title">${esc(r.dir.split('/').pop())}</span>
          ${gitBadge(r.git)}
        </div>
        <span class="status ${r.working ? 'on' : 'off'}"><span class="dot"></span>${r.working ? 'Working' : 'Waiting'}</span>
      </div>
      <div class="session-meta">
        ${chipHtml}<span>cpu ${r.cpu ?? '—'}%</span><span class="sep">·</span><span>${fmtKb(r.rssKb)}</span>
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
        <div class="session-headline">
          <span class="session-title">${esc(s.dir?.split('/').pop() || '?')}</span>
          ${gitBadge({ branch: s.branch })}
        </div>
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
      if (armedKill === el.dataset.kill) { armedKill = null; await api('/api/kill', { name: el.dataset.kill }); poll(); }
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
  } catch (err) { toast(err.message); poll(); } // re-arm even when the action failed
});

// Launch: submitting the command bar (button or Enter in the directory field).
$('#launcher').addEventListener('submit', async e => {
  e.preventDefault();
  const btn = $('#launch');
  const dir = $('#dir').value.trim();
  if (!dir) { toast('Enter a project directory first'); $('#dir').focus(); return; }
  const model = modelDD.value, effort = effortDD.value;
  btn.disabled = true; btn.innerHTML = `${ICONS.play}<span>Launching…</span>`;
  try { await api('/api/launch', { dir, model, effort }); $('#dir').value = ''; poll(); }
  catch (err) { toast(err.message); poll(); } // re-arm even when the launch failed
  finally { btn.disabled = false; btn.innerHTML = `${ICONS.play}<span>Launch</span>`; }
});

/* ---------- Folder picker ----------
   A touch-friendly modal that browses server directories from "/", plus
   server-backed Recents and Favorites. Folders only; dotfolders hidden by
   default. Selecting a folder fills the directory field. */
$('#browse').innerHTML = ICONS.folder;
$('#picker-close').innerHTML = ICONS.x;

const picker = $('#picker');
const pkList = $('#picker-list');
const pkCrumbs = $('#picker-crumbs');
const pkFoot = document.querySelector('.picker-foot');
const pkHiddenInput = $('#picker-hidden');
let pkTab = 'browse';
let pkPath = null;      // current browse directory
let favorites = [];     // cached from /api/places, for the star state

function openPicker() {
  const typed = $('#dir').value.trim();
  pkPath = typed.startsWith('/') ? typed : null; // seed from field, else server default (home)
  api('/api/places').then(p => { favorites = p.favorites; }).catch(() => {}).finally(() => setTab('browse'));
  picker.showModal();
}
function closePicker() { if (picker.open) picker.close(); }

$('#browse').onclick = openPicker;
$('#picker-close').onclick = closePicker;
picker.addEventListener('click', e => { if (e.target === picker) closePicker(); }); // backdrop
pkHiddenInput.addEventListener('change', () => { if (pkTab === 'browse') browseTo(pkPath); });
$('#picker-select').onclick = () => { if (pkPath) selectFolder(pkPath); };
$('#picker-tabs').addEventListener('click', e => { const t = e.target.dataset.tab; if (t) setTab(t); });

function setTab(tab) {
  pkTab = tab;
  [...$('#picker-tabs').children].forEach(b => b.classList.toggle('on', b.dataset.tab === tab));
  const browse = tab === 'browse';
  pkCrumbs.hidden = !browse;
  pkFoot.hidden = !browse;
  if (browse) browseTo(pkPath); else renderPlaces(tab);
}

function starBtn(path) {
  const fav = favorites.includes(path);
  return `<button class="pk-star ${fav ? 'on' : ''}" type="button" data-fav="${esc(path)}" aria-label="${fav ? 'Remove favorite' : 'Add favorite'}">${fav ? ICONS.starFilled : ICONS.star}</button>`;
}
function folderRow(name, path) {
  return `<div class="pk-row">
    <button class="pk-main" type="button" data-nav="${esc(path)}">
      <span class="pk-icon">${ICONS.folder}</span><span class="pk-name">${esc(name)}</span>
      <span class="pk-chevron">${ICONS.chevronRight}</span>
    </button>${starBtn(path)}</div>`;
}
function placeRow(path, icon) {
  const name = path.split('/').filter(Boolean).pop() || path;
  return `<div class="pk-row">
    <button class="pk-main" type="button" data-pick="${esc(path)}">
      <span class="pk-icon">${icon}</span>
      <span class="pk-col"><span class="pk-name">${esc(name)}</span><span class="pk-sub">${esc(path)}</span></span>
    </button>${starBtn(path)}</div>`;
}

async function browseTo(path) {
  pkList.innerHTML = '<div class="picker-empty">Loading…</div>';
  try {
    const q = new URLSearchParams();
    if (path) q.set('path', path);
    if (pkHiddenInput.checked) q.set('hidden', '1');
    const d = await api('/api/browse?' + q);
    pkPath = d.path;
    $('#picker-current').textContent = d.path;
    renderCrumbs(d.path);
    pkList.innerHTML = d.entries.length
      ? d.entries.map(e => folderRow(e.name, e.path)).join('') + (d.truncated ? '<div class="picker-empty">…more folders not shown</div>' : '')
      : '<div class="picker-empty">No subfolders here — use “Use this folder” to pick it.</div>';
  } catch (err) {
    if (path) { pkPath = null; return browseTo(null); } // dead-end guard: fall back to home
    pkList.innerHTML = `<div class="picker-empty">${esc(err.message)}</div>`;
  }
}

async function renderPlaces(tab) {
  pkList.innerHTML = '<div class="picker-empty">Loading…</div>';
  try {
    const p = await api('/api/places');
    favorites = p.favorites;
    const list = tab === 'recents' ? p.recents : p.favorites;
    const icon = tab === 'recents' ? ICONS.clock : ICONS.starFilled;
    pkList.innerHTML = list.length
      ? list.map(path => placeRow(path, icon)).join('')
      : `<div class="picker-empty">No ${tab} yet</div>`;
  } catch (err) { pkList.innerHTML = `<div class="picker-empty">${esc(err.message)}</div>`; }
}

function renderCrumbs(path) {
  const parts = path.split('/').filter(Boolean);
  let acc = '';
  const out = [`<button class="picker-crumb" type="button" data-nav="/">/</button>`];
  for (const part of parts) {
    acc += '/' + part;
    out.push('<span class="picker-crumb-sep">›</span>');
    out.push(`<button class="picker-crumb" type="button" data-nav="${esc(acc)}">${esc(part)}</button>`);
  }
  pkCrumbs.innerHTML = out.join('');
  pkCrumbs.scrollLeft = pkCrumbs.scrollWidth; // keep the deepest crumb in view
}

// starBtn is the one place that decides how a star looks; querySelectorAll
// returns a static list, so replacing during iteration is safe.
function refreshStars() {
  pkList.querySelectorAll('.pk-star').forEach(b => { b.outerHTML = starBtn(b.dataset.fav); });
}
async function toggleFav(path) {
  try {
    favorites = (await api('/api/favorites', { path })).favorites;
    if (pkTab === 'favorites') renderPlaces('favorites'); else refreshStars();
  } catch (err) { toast(err.message); }
}
function selectFolder(path) { $('#dir').value = path; closePicker(); $('#dir').focus(); }

pkList.addEventListener('click', e => {
  const fav = e.target.closest('[data-fav]');
  if (fav) { e.stopPropagation(); toggleFav(fav.dataset.fav); return; }
  const nav = e.target.closest('[data-nav]');
  if (nav) { pkPath = nav.dataset.nav; browseTo(pkPath); return; }
  const pick = e.target.closest('[data-pick]');
  if (pick) selectFolder(pick.dataset.pick);
});
pkCrumbs.addEventListener('click', e => { const c = e.target.closest('[data-nav]'); if (c) { pkPath = c.dataset.nav; browseTo(pkPath); } });

let bk = cdashBackoff.initial();
let timer = null;
let gen = 0; // stale-tick guard: a superseded tick's result is dropped
let polled = false; // has any tick actually run to completion?

function arm() {
  clearTimeout(timer);
  timer = setTimeout(tick, cdashBackoff.delay(bk));
}

async function tick() {
  const g = gen;
  // Skip background polls, but never the first one. An Android WebView is
  // hidden until its activity resumes, and `visibilitychange` can fire before
  // the listener below is attached — which left the app arming forever on
  // "Connecting…", with no request ever sent.
  if (document.hidden && polled) { arm(); return; }
  let outcome;
  try {
    const data = await api('/api/sessions');
    if (g !== gen) return; // superseded mid-flight: never paint the old snapshot
    render(data);
    $('#health').className = 'dot ok';
    $('#health-label').textContent = 'Connected';
    markConnected();
    outcome = 'ok';
  } catch (err) {
    if (g !== gen) return;
    // Only transport errors and 401/403 reach here as distinct things;
    // everything non-auth backs off. A halt is cleared only by poll(),
    // which is user-initiated.
    outcome = cdashBackoff.outcomeFor(err.status);
    $('#health').className = 'dot bad';
    $('#health-label').textContent = 'Disconnected';
    // Only Android has a story the user can act on from here; every other
    // platform's agent is either in-process or somewhere this app cannot reach.
    if (isTauri) hostPlatform().then(p => { if (p === 'android') showSetup(); });
  }
  // Logs are secondary: their failure must not flip the indicator or
  // advance the ladder once sessions have rendered.
  if ($('#logbox').open && outcome === 'ok') {
    try { $('#logs').textContent = (await api('/api/logs')).lines.join('\n'); } catch {}
  }
  if (g !== gen) return;
  polled = true;
  bk = cdashBackoff.next(bk, outcome);
  if (!bk.halted) arm();
}

// Any user-initiated action (launch, kill, resume, purge) calls poll():
// reset the ladder and try immediately.
function poll() {
  clearTimeout(timer);
  gen++;
  bk = cdashBackoff.initial();
  tick();
}


/* ---------- Termux setup (the Android app only) ----------
   The app carries its own UI, so — unlike the PWA, which the agent itself
   serves — it can say how to install the agent before one exists. The bundled
   binary is handed over loopback, which Android does not isolate between apps,
   so Termux curls it straight out of this process.

   The pasted block also appends a guard to ~/.bashrc, so the agent comes back
   by itself every time Termux opens. That is what keeps the reminder down to
   "open Termux" instead of "run this again". */
let host = null;          // 'android' | 'windows' | 'linux' | … once resolved
let setupDismissed = false;

/// Resolved on first use rather than at load: a throw at the top level of this
/// file would take `poll()` down with it, and the whole UI with that.
async function hostPlatform() {
  if (host === null) {
    try { host = await invoke('host_platform'); } catch { host = 'unknown'; }
  }
  return host;
}

const installScript = url => `curl -fsS -o "$HOME/cdash-agent" ${url}
chmod +x "$HOME/cdash-agent"
grep -q cdash-agent "$HOME/.bashrc" 2>/dev/null || cat >> "$HOME/.bashrc" <<'CDASH'

# claude-dashboard: start the agent whenever Termux opens, unless it is up
if ! curl -fsS -m 1 http://127.0.0.1:8080/api/health >/dev/null 2>&1; then
  termux-wake-lock 2>/dev/null
  nohup "$HOME/cdash-agent" >"$HOME/cdash-agent.log" 2>&1 </dev/null &
fi
CDASH
. "$HOME/.bashrc"`;

async function showSetup() {
  const dlg = $('#setup');
  if (dlg.open || setupDismissed) return;
  try {
    const { url } = await invoke('agent_handoff');
    $('#setup-script').textContent = installScript(url);
  } catch (e) {
    // No bundled agent (a build without CDASH_AGENT_BIN): say so rather than
    // showing a curl of nothing.
    $('#setup-script').textContent = String(e?.message ?? e);
  }
  if (!dlg.open) dlg.showModal();
}

/// A reconnect retires the dialog and re-arms it for the next outage.
function markConnected() {
  setupDismissed = false;
  if ($('#setup').open) $('#setup').close();
}

$('#setup-close').innerHTML = ICONS.x;
$('#setup-close').onclick = () => { setupDismissed = true; $('#setup').close(); };
$('#setup-retry').onclick = () => { $('#setup').close(); poll(); };
$('#setup-copy').onclick = async () => {
  const text = $('#setup-script').textContent;
  try {
    await navigator.clipboard.writeText(text);
    toast('Copied \u2014 paste it into Termux');
  } catch {
    // Android WebView can refuse the async clipboard; selecting the block at
    // least makes a long-press copy one gesture.
    const r = document.createRange();
    r.selectNodeContents($('#setup-script'));
    getSelection().removeAllRanges();
    getSelection().addRange(r);
    toast('Long-press the command to copy');
  }
};

// Give the launch button its resting icon + label.
$('#launch').innerHTML = `${ICONS.play}<span>Launch</span>`;

poll();
document.addEventListener('visibilitychange', () => { if (!document.hidden) poll(); });

// Registration lives here, not in an inline script, so it shares one isTauri.
// Both of sw.js's assumptions (same-origin /api/, http-cache semantics) break
// in the Tauri webview.
if (!isTauri && 'serviceWorker' in navigator) navigator.serviceWorker.register('sw.js').catch(() => {});
