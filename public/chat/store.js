/* Chat frames to view rows. Pure: no DOM, no socket, so node --test can drive
   it frame by frame. Events are kept by position and rows are rebuilt on
   demand; the view diffs rows by key. Same IIFE shape as term/keys.js. */
(function (g) {
  const initial = () => ({
    epoch: null, tool: null, head: 0, first: 1, more: false,
    working: false, waiting: false, ended: false, unreadable: false, held: [],
    events: new Map(),   // seq -> event
    streams: new Map(),  // message id -> text so far (deltas, never stored server-side)
    mine: [],            // {cid, text, status, error, seq}; seq set once an event showed it
    forgetHeld: false,   // after a reset, the next state frame says which held ones survived
  });

  const isPrompt = ev => ev.type === 'user' && !(Array.isArray(ev.message?.content) && ev.message.content.some(b => b?.type === 'tool_result'));
  const promptText = ev => {
    const c = ev.message?.content;
    return typeof c === 'string' ? c : Array.isArray(c) ? c.filter(b => b?.type === 'text').map(b => b.text).join('') : '';
  };
  const flatten = c => typeof c === 'string' ? c : Array.isArray(c) ? c.map(b => b?.text ?? '').join('\n') : '';

  function clearConversation(s) {
    s.events.clear();
    s.streams.clear();
    s.head = 0;
    s.first = 1;
    s.more = false;
    // Messages the new conversation never showed stay, so their text is not lost.
    s.mine = s.mine.filter(m => m.seq == null);
    s.forgetHeld = true;
  }

  function store(s, seq, ev) {
    if (s.events.has(seq)) return;
    s.events.set(seq, ev);
    if (isPrompt(ev)) {
      const text = promptText(ev);
      const m = ev.cid ? s.mine.find(x => x.cid === ev.cid)
        : s.mine.find(x => x.seq == null && x.text === text);  // the ack may have been lost across a reconnect
      if (m) { m.seq = seq; m.status = 'sent'; }
    }
    if (ev.type === 'assistant' && ev.message?.id) s.streams.delete(ev.message.id);
    if (ev.type === 'result') s.streams.clear();
  }

  function apply(s, f) {
    switch (f?.t) {
      case 'hello':
        if (f.reset || f.epoch !== s.epoch) { clearConversation(s); s.first = f.first; }
        s.epoch = f.epoch;
        s.tool = f.tool;
        s.more = s.first > 1;
        break;
      case 'reset':
        clearConversation(s);
        s.forgetHeld = false;  // the agent keeps its queue across a conversation change
        s.epoch = f.epoch;
        break;
      case 'ev':
        if (f.seq <= s.head) break;
        store(s, f.seq, f.ev);
        s.head = f.seq;
        break;
      case 'page':
        for (const e of f.events || []) { store(s, e.seq, e.ev); s.first = Math.min(s.first, e.seq); }
        s.more = !!f.more;
        break;
      case 'delta':
        s.streams.set(f.id, (s.streams.get(f.id) || '') + f.text);
        break;
      case 'state':
        Object.assign(s, { working: !!f.working, waiting: !!f.waiting, ended: !!f.ended, unreadable: !!f.unreadable, held: f.held || [] });
        if (s.forgetHeld) {
          // An agent that restarted lost its queue; say so instead of waiting forever.
          const ids = new Set(s.held.map(h => h.id));
          for (const m of s.mine) if (m.status === 'held' && !ids.has(m.heldId)) { m.status = 'refused'; m.error = 'not sent'; }
          s.forgetHeld = false;
        }
        break;
      case 'ack': {
        const m = s.mine.find(x => x.cid === f.cid);
        if (!m) break;
        m.status = f.status;
        if (f.id) m.heldId = f.id;
        if (f.error) m.error = f.error;
        // Handed back or never shown: it is the owner's text again.
        if (['returned', 'refused', 'unconfirmed'].includes(f.status)) m.seq = null;
        break;
      }
    }
    return s;
  }

  function addMine(s, cid, text) {
    s.mine.push({ cid, text, status: 'sending', seq: null });
    return s;
  }

  const base = p => String(p || '').split('/').filter(Boolean).pop() || '';
  const VERBS = { Bash: ['Running', 'Ran'], Read: ['Reading', 'Read'], Edit: ['Editing', 'Edited'], Write: ['Editing', 'Edited'], Grep: ['Searching', 'Searched'], Glob: ['Searching', 'Searched'] };
  function toolLabel(name, input, done) {
    const v = VERBS[name];
    if (!v) return name;
    const verb = v[done ? 1 : 0];
    if (name === 'Bash') {
      const cmd = String(input?.command ?? '');
      return verb + ' `' + (cmd.length > 60 ? cmd.slice(0, 59) + '…' : cmd) + '`';
    }
    return verb + ' ' + base(input?.file_path ?? input?.path ?? input?.pattern);
  }
  const groupLabel = (name, calls, done) => calls.length === 1 ? toolLabel(name, calls[0].input, done)
    : VERBS[name] && name !== 'Bash' ? VERBS[name][done ? 1 : 0] + ' ' + calls.length + ' files'
    : name + ' ×' + calls.length;

  function rows(s) {
    const seqs = [...s.events.keys()].sort((a, b) => a - b);
    const gone = new Set();
    for (const q of seqs) { const e = s.events.get(q); if (e.type === 'cdash' && e.subtype === 'retract') gone.add(e.uuid); }
    const out = [];
    const calls = new Map();  // tool_use id -> call
    let lastResult = -1;
    for (const q of seqs) {
      const e = s.events.get(q);
      const last = out[out.length - 1];
      if (e.type === 'user' && isPrompt(e)) {
        if (gone.has(e.uuid)) continue;
        const text = promptText(e);
        const queued = out.findIndex(r => r.kind === 'user' && r.status === 'queued' && r.text === text);
        if (queued >= 0) out.splice(queued, 1);
        out.push({ kind: 'user', key: 'e' + q, text, status: 'sent' });
      } else if (e.type === 'user') {
        for (const b of e.message.content) {
          const c = calls.get(b?.tool_use_id);
          if (c) Object.assign(c, { result: flatten(b.content), error: !!b.is_error, done: true });
        }
      } else if (e.type === 'assistant') {
        for (const b of e.message?.content || []) {
          if (b.type === 'text') out.push({ kind: 'text', key: 'e' + q, text: b.text, streaming: false });
          if (b.type === 'tool_use') {
            const c = { id: b.id, name: b.name, input: b.input, result: null, error: false, done: false, at: q };
            calls.set(b.id, c);
            if (last?.kind === 'tools' && last.name === b.name) last.calls.push(c);
            else out.push({ kind: 'tools', key: 'e' + q, name: b.name, calls: [c] });
          }
        }
      } else if (e.type === 'result') {
        lastResult = q;
        if (e.subtype === 'interrupted') out.push({ kind: 'note', key: 'e' + q, text: 'Stopped' });
        if (e.subtype === 'error_during_execution') out.push({ kind: 'note', key: 'e' + q, text: 'Turn ended with an error' });
      } else if (e.type === 'cdash' && e.subtype === 'shell') {
        out.push({ kind: 'shell', key: 'e' + q, command: e.command, stdout: '', stderr: '' });
      } else if (e.type === 'cdash' && e.subtype === 'shell_output') {
        const sh = [...out].reverse().find(r => r.kind === 'shell');
        if (sh) Object.assign(sh, { stdout: e.stdout ?? '', stderr: e.stderr ?? '' });
      } else if (e.type === 'cdash' && e.subtype === 'queued') {
        out.push({ kind: 'user', key: 'e' + q, text: e.text, status: 'queued' });
      }
    }
    for (const r of out) {
      if (r.kind !== 'tools') continue;
      r.running = r.calls.some(c => !c.done && c.at > lastResult);
      r.label = groupLabel(r.name, r.calls, !r.running);
    }
    for (const [id, text] of s.streams) out.push({ kind: 'text', key: 's' + id, text, streaming: true });
    for (const m of s.mine) {
      if (m.seq != null) continue;
      const r = { kind: 'user', key: 'm' + m.cid, cid: m.cid, text: m.text, status: m.status };
      if (m.error) r.error = m.error;
      out.push(r);
    }
    return out;
  }

  g.cdashChatStore = { initial, apply, rows, addMine, toolLabel };
})(typeof globalThis === 'undefined' ? this : globalThis);
