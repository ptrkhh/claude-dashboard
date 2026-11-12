/* The chat screen: one WebSocket to the agent, frames through cdashChatStore,
   rows to DOM by key. Conversation text reaches markup only through
   cdashMd.render; everything else is textContent. Touches the DOM only inside
   functions, so node --test can import it for its pure helpers. */
(function (g) {
  const $ = s => document.querySelector(s);
  const RETRY = [1000, 2000, 5000, 10000];
  const NEAR = 80; // px from the bottom that still counts as following
  let c = null;    // the open chat
  let wired = false;

  /* What the composer offers: 'send' while idle, 'stop' while a turn runs or
     a message is still on its way, 'off' when typing would go nowhere useful. */
  function composerMode(state, hasPending) {
    if (state.waiting || state.ended || state.unreadable) return 'off';
    return state.working || hasPending ? 'stop' : 'send';
  }
  const pending = s => s.mine.some(m => m.seq == null && (m.status === 'sending' || m.status === 'sent'));

  function el(tag, cls, text) {
    const n = document.createElement(tag);
    if (cls) n.className = cls;
    if (text != null) n.textContent = text;
    return n;
  }

  function wireOnce() {
    if (wired) return;
    wired = true;
    const log = $('#chat-log');
    log.addEventListener('scroll', () => {
      if (!c) return;
      c.follow = log.scrollHeight - log.scrollTop - log.clientHeight < NEAR;
      $('#chat-jump').hidden = c.follow;
      if (log.scrollTop < 40 && c.state.more && !c.paging && c.ws?.readyState === WebSocket.OPEN) {
        c.paging = true;
        c.ws.send(JSON.stringify({ t: 'page', before: c.state.first }));
      }
    });
    $('#chat-jump').addEventListener('click', () => {
      if (!c) return;
      c.follow = true;
      log.scrollTop = log.scrollHeight;
      $('#chat-jump').hidden = true;
    });
    log.addEventListener('click', e => {
      const b = e.target.closest('[data-tools]');
      if (b && c) showTools(b.dataset.tools);
    });
    $('#chat-sheet-close').addEventListener('click', () => $('#chat-sheet').close());
    log.addEventListener('click', e => {
      const b = e.target.closest('[data-edit],[data-again]');
      if (!b || !c) return;
      const cid = b.dataset.edit ?? b.dataset.again;
      const m = c.state.mine.find(x => x.cid === cid);
      if (!m) return;
      c.state.mine = c.state.mine.filter(x => x !== m);
      if (b.dataset.edit !== undefined) toBox(m.text);
      else submitText(m.text);
      schedule();
    });
    $('#chat-held').addEventListener('click', e => {
      const b = e.target.closest('[data-op]');
      if (!b || !c) return;
      const h = c.state.held.find(x => x.id === b.dataset.id);
      if (!h || !sendFrame({ t: 'held', op: b.dataset.op === 'now' ? 'now' : 'remove', id: h.id })) return;
      if (b.dataset.op === 'edit') toBox(h.text);
    });
    $('#chat-banner').addEventListener('click', e => {
      if (e.target.closest('[data-open-term]') && c) c.opts.openTerm(c.opts.name, c.opts.title);
    });
    const input = $('#chat-input');
    $('#chat-form').addEventListener('submit', e => {
      e.preventDefault();
      if (!c) return;
      const text = input.value.trim();
      const mode = composerMode(c.state, pending(c.state));
      if (mode === 'off') return;
      if (!text) { if (mode === 'stop') sendFrame({ t: 'stop' }); return; }
      if (submitText(text)) { input.value = ''; grow(); renderComposer(); }
    });
    input.addEventListener('keydown', e => {
      // Enter inserts a newline on touch keyboards; a hardware keyboard sends.
      if (e.key === 'Enter' && !e.shiftKey && !e.isComposing && matchMedia('(pointer: fine)').matches) {
        e.preventDefault();
        $('#chat-form').requestSubmit();
      }
    });
    input.addEventListener('input', () => { grow(); if (c) renderComposer(); });
    document.addEventListener('visibilitychange', () => {
      if (!document.hidden && c && !c.ws && !c.done) { clearTimeout(c.retryTimer); connect(); }
    });
    addEventListener('online', () => { if (c && !c.ws && !c.done) { clearTimeout(c.retryTimer); connect(); } });
    addEventListener('offline', () => { if (c) { c.link = 'Offline'; schedule(); } });
  }

  function open(opts) {
    if (c) close();
    wireOnce();
    c = {
      opts, state: cdashChatStore.initial(), ws: null, nodes: new Map(), raf: 0,
      attempt: 0, retryTimer: 0, done: false, follow: true, paging: false, paged: false,
      link: 'Connecting…', since: 0, clock: 0,
    };
    $('#chat-title').textContent = opts.title || opts.name;
    const rc = $('#chat-rc');
    rc.hidden = !opts.rcLink;
    if (opts.rcLink) rc.href = opts.rcLink;
    $('#chat-term').onclick = () => opts.openTerm(opts.name, opts.title);
    $('#chat-log').textContent = '';
    $('#chat-jump').hidden = true;
    $('#chat').hidden = false;
    c.clock = setInterval(() => { if (c?.state.working) renderStatus(); }, 1000);
    schedule();
    connect();
  }

  function close() {
    if (!c) return;
    const x = c;
    c = null;
    clearTimeout(x.retryTimer);
    clearInterval(x.clock);
    if (x.raf) cancelAnimationFrame(x.raf);
    x.ws?.close();
    $('#chat').hidden = true;
    $('#chat-log').textContent = '';
    $('#chat-held').textContent = '';
    if ($('#chat-sheet').open) $('#chat-sheet').close();
  }

  async function connect() {
    const x = c;
    const s = x.state;
    const q = '/api/chat?name=' + encodeURIComponent(x.opts.name)
      + (s.epoch ? '&after=' + s.head + '&epoch=' + encodeURIComponent(s.epoch) : '');
    let url;
    try { url = await x.opts.wsUrl(q); }
    catch { if (c === x) retry(x); return; }
    if (c !== x || x.ws) return; // closed, or another attempt won, while resolving the URL
    const ws = new WebSocket(url);
    x.ws = ws;
    ws.onmessage = e => {
      if (c !== x || x.ws !== ws) return;
      let f;
      try { f = JSON.parse(e.data); } catch { return; }
      if (f.t === 'hello') { x.attempt = 0; x.link = ''; }
      if (f.t === 'page') { x.paging = false; x.paged = true; }
      const wasWorking = x.state.working;
      cdashChatStore.apply(x.state, f);
      if (x.state.working && !wasWorking) x.since = Date.now();
      schedule();
    };
    ws.onclose = e => {
      if (c !== x || x.ws !== ws) return; // the UI closed it, or a newer socket replaced it
      x.ws = null;
      x.paging = false;
      if (e.code === 4000) { x.done = true; x.link = ''; schedule(); return; }
      if (e.code === 4002) { connect(); return; } // fell behind: catch up from the last position now
      retry(x);
    };
  }

  function sendFrame(f) {
    if (c?.ws?.readyState !== WebSocket.OPEN) { c?.opts.toast?.('Not connected'); return false; }
    c.ws.send(JSON.stringify(f));
    return true;
  }

  function submitText(text) {
    const cid = crypto.randomUUID();
    if (!sendFrame({ t: 'send', cid, text })) return false;
    cdashChatStore.addMine(c.state, cid, text);
    c.follow = true;
    schedule();
    return true;
  }

  function toBox(text) {
    const input = $('#chat-input');
    input.value = input.value ? input.value + '\n' + text : text;
    grow();
    input.focus();
    renderComposer();
  }

  function grow() {
    const input = $('#chat-input');
    input.style.height = 'auto';
    input.style.height = Math.min(input.scrollHeight, innerHeight * 0.4) + 'px';
  }

  function retry(x) {
    x.link = navigator.onLine === false ? 'Offline' : 'Reconnecting…';
    schedule();
    x.retryTimer = setTimeout(() => { if (c === x && !x.ws) connect(); }, RETRY[Math.min(x.attempt++, RETRY.length - 1)]);
  }

  function schedule() {
    if (c && !c.raf) c.raf = requestAnimationFrame(render);
  }

  const STATUS = { sending: 'Sending…', queued: 'Waiting for Claude', held: 'Held', unconfirmed: 'Not confirmed', returned: 'Not sent', refused: 'Not sent' };

  function build(r) {
    if (r.kind === 'text') {
      const n = el('div', 'chat-text' + (r.streaming ? ' streaming' : ''));
      n.innerHTML = cdashMd.render(r.text);
      return n;
    }
    if (r.kind === 'user') {
      const n = el('div', 'chat-user chat-' + r.status);
      n.append(el('div', 'chat-bubble', r.text));
      if (r.status !== 'sent') {
        const meta = el('div', 'chat-meta', STATUS[r.status] + (r.error ? ': ' + r.error : ''));
        if (r.cid && ['unconfirmed', 'returned', 'refused'].includes(r.status)) {
          // Nothing is ever resent on its own: the owner decides.
          const edit = el('button', '', 'Edit');
          edit.type = 'button';
          edit.dataset.edit = r.cid;
          meta.append(edit);
          if (r.status === 'unconfirmed') {
            const again = el('button', '', 'Send again');
            again.type = 'button';
            again.dataset.again = r.cid;
            meta.append(again);
          }
        }
        n.append(meta);
      }
      return n;
    }
    if (r.kind === 'tools') {
      const b = el('button', 'chat-tool' + (r.running ? ' running' : ''));
      b.type = 'button';
      b.dataset.tools = r.key;
      b.append(el('span', 'chat-tool-label', r.label));
      if (r.running) b.append(el('span', 'chat-spinner'));
      return b;
    }
    if (r.kind === 'shell') {
      const n = el('div', 'chat-shell');
      n.append(el('pre', 'chat-shell-cmd', '! ' + r.command));
      const out = [r.stdout, r.stderr].filter(Boolean).join('\n');
      if (out) n.append(el('pre', 'chat-shell-out', out));
      return n;
    }
    return el('div', 'chat-note', r.text);
  }

  function render() {
    const x = c;
    if (!x) return;
    x.raf = 0;
    const log = $('#chat-log');
    const height = log.scrollHeight;
    const top = log.scrollTop;
    // Held messages live in the list above the composer, with their controls.
    const rows = cdashChatStore.rows(x.state).filter(r => !(r.kind === 'user' && r.status === 'held'));
    x.rows = rows;
    const keep = new Set();
    let prev = null;
    for (const r of rows) {
      keep.add(r.key);
      const sig = JSON.stringify(r);
      let n = x.nodes.get(r.key);
      if (!n) {
        n = { el: build(r), sig };
        x.nodes.set(r.key, n);
      } else if (n.sig !== sig) {
        const fresh = build(r);
        n.el.replaceWith(fresh);
        Object.assign(n, { el: fresh, sig });
      }
      const want = prev ? prev.nextSibling : log.firstChild;
      if (n.el !== want) log.insertBefore(n.el, want);
      prev = n.el;
    }
    for (const [k, n] of x.nodes) if (!keep.has(k)) { n.el.remove(); x.nodes.delete(k); }
    if (x.paged) {
      x.paged = false;
      log.scrollTop = top + (log.scrollHeight - height); // older rows went on top; keep the view still
    } else if (x.follow) {
      log.scrollTop = log.scrollHeight;
    }
    $('#chat-jump').hidden = x.follow;
    renderStatus();
    renderBanner();
    renderHeld();
    renderComposer();
  }

  function renderHeld() {
    const box = $('#chat-held');
    box.textContent = '';
    for (const h of c.state.held) {
      const item = el('div', 'chat-held-item');
      item.append(el('span', 'chat-held-text', h.text));
      for (const [op, label] of [['now', 'Send now'], ['edit', 'Edit'], ['remove', 'Remove']]) {
        const b = el('button', '', label);
        b.type = 'button';
        b.dataset.op = op;
        b.dataset.id = h.id;
        item.append(b);
      }
      box.append(item);
    }
  }

  function renderComposer() {
    const s = c.state;
    const input = $('#chat-input');
    const go = $('#chat-go');
    const mode = c.done ? 'off' : composerMode(s, pending(s));
    const stop = mode === 'stop' && !input.value.trim();
    input.disabled = go.disabled = mode === 'off';
    input.placeholder = s.working ? 'Queue a message…' : 'Message…';
    go.textContent = stop ? 'Stop' : 'Send';
    go.classList.toggle('stop', stop);
  }

  function renderStatus() {
    const s = c.state;
    $('#chat-state').textContent = s.ended || c.done ? 'Ended'
      : s.waiting ? 'Needs an answer'
      : s.working ? 'Working… ' + Math.max(0, Math.round((Date.now() - c.since) / 1000)) + 's'
      : '';
  }

  function renderBanner() {
    const b = $('#chat-banner');
    const s = c.state;
    const [text, term] = c.link ? [c.link, false]
      : s.ended || c.done ? ['Session ended.', false]
      : s.unreadable ? ['Chat cannot read this session. Use Terminal.', true]
      : s.waiting ? ['This session is waiting for an answer.', true]
      : ['', false];
    const sig = text + term;
    if (b.dataset.sig === sig) return;
    b.dataset.sig = sig;
    b.textContent = '';
    b.hidden = !text;
    if (!text) return;
    b.append(el('span', '', text));
    if (term) {
      const open = el('button', 'chat-bar-btn', 'Open Terminal');
      open.type = 'button';
      open.dataset.openTerm = '';
      b.append(open);
    }
  }

  function showTools(key) {
    const row = (c.rows || []).find(r => r.key === key);
    if (!row) return;
    const body = $('#chat-sheet-body');
    body.textContent = '';
    for (const call of row.calls) {
      const sec = el('section', 'chat-call');
      sec.append(el('h3', 'chat-call-name', call.name));
      sec.append(el('pre', 'chat-call-input', JSON.stringify(call.input, null, 2)));
      if (call.done) sec.append(el('pre', 'chat-call-result' + (call.error ? ' error' : ''), call.result || '(no output)'));
      else sec.append(el('p', 'chat-call-wait', 'Running…'));
      body.append(sec);
    }
    $('#chat-sheet').showModal();
  }

  g.cdashChat = {
    composerMode,
    open,
    close,
    isOpen: () => !!c,
    name: () => c?.opts.name ?? null,
  };
})(typeof globalThis === 'undefined' ? this : globalThis);
