# UI Step 7 Implementation Plan — backoff, status propagation, service worker

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Ship spec step 7 — the graduated-ladder poll with auth-halt, `api()` status propagation and the Tauri transport branch, and the runtime-caching service worker replacing the precache manifest.

**Architecture:** All decision logic lives in one new pure-function file, `public/transport/backoff.js`, written in the `(function (g) { … })(globalThis)` form so it is simultaneously a classic script (browser `<script>` tag) and a side-effect ES module (`node --test` imports it directly). `app.js` keeps only DOM wiring: `poll()` branches on the propagated HTTP status through `next()`. `sw.js` drops its precache manifest entirely for network-first navigations and stale-while-revalidate sub-resources.

**Tech Stack:** Vanilla JS, no build step, no bundler, no framework. `node --test` for the backoff unit tests. Existing `cargo test` suite must stay green (static files are served by the Rust agent).

**Spec:** `docs/superpowers/specs/2026-07-30-tauri-multi-host-design.md` — sections [UI — `public/`](#2-ui--public), [Service worker](#service-worker), [Error handling](#error-handling) (the two explicit behaviors), [Testing → UI logic tests](#testing).

## Global Constraints

- No build step, no bundler, no `import`/`export` in any browser-loaded `public/*.js`. Classic scripts only.
- `public/transport/backoff.js` assigns to `globalThis` via `(function (g) { … })(globalThis)` — top-level `this` is `undefined` in a module and `globalThis` in a classic script; this form is load-bearing, not stylistic.
- Backoff ladder exactly `4s → 8s → 15s → 30s` (30s cap); reset to 4s on success, on `visibilitychange` to visible, and on any user-initiated action.
- Auth failures (HTTP 401 or 403) are terminal: halt the poll. Throttling (429/503) is transient: back off, never halt.
- Service worker cache key is `'cdash'` — a namespace, never bumped. Navigations are network-first; only a response with `status === 200 && !redirected && pathname === '/'` may be cached under `/`. Every `cache.put` requires `r.ok`.
- Service-worker registration gated to web mode on the same `isTauri` predicate as the transport branch, with `.catch()` on `register()`.
- The Tauri detection predicate is **unconfirmed** — the spec requires confirming it during step 8. Use `'__TAURI_INTERNALS__' in window || '__TAURI__' in window` and mark it for verification in step 8.

---

### Task 1: `backoff.js` — the pure decision function

**Files:**
- Create: `public/transport/backoff.js`
- Create: `test/backoff.test.mjs`
- Modify: `package.json` (add a `test` script)

**Interfaces:**
- Consumes: nothing.
- Produces: `globalThis.cdashBackoff` with:
  - `initial()` → `{ i: 0, halted: false }`
  - `next(state, outcome)` → new state; `outcome ∈ {'ok', 'fail', 'auth'}`. `ok` resets; `fail` climbs the ladder one rung (capped at the last); `auth` returns `{ i: 0, halted: true }` and stays halted regardless of later `fail`s.
  - `delay(state)` → ms, from `[4000, 8000, 15000, 30000]`.

- [ ] **Step 1: Write the failing tests**

Create `test/backoff.test.mjs`:

```js
import { test } from 'node:test';
import assert from 'node:assert/strict';

await import('../public/transport/backoff.js');
const b = globalThis.cdashBackoff;

test('a fresh state polls every 4 seconds', () => {
  const s = b.initial();
  assert.deepEqual(s, { i: 0, halted: false });
  assert.equal(b.delay(s), 4000);
});

test('failure climbs the ladder 8, 15, 30 and caps at 30', () => {
  let s = b.initial();
  s = b.next(s, 'fail'); assert.equal(b.delay(s), 8000);
  s = b.next(s, 'fail'); assert.equal(b.delay(s), 15000);
  s = b.next(s, 'fail'); assert.equal(b.delay(s), 30000);
  s = b.next(s, 'fail'); assert.equal(b.delay(s), 30000);
  s = b.next(s, 'fail'); assert.equal(b.delay(s), 30000);
});

test('success resets to 4 seconds', () => {
  let s = b.initial();
  s = b.next(s, 'fail');
  s = b.next(s, 'fail');
  s = b.next(s, 'ok');
  assert.equal(b.delay(s), 4000);
  assert.equal(s.halted, false);
});

test('an auth failure halts and stays halted', () => {
  let s = b.next(b.initial(), 'auth');
  assert.equal(s.halted, true);
  s = b.next(s, 'fail');
  assert.equal(s.halted, true);
});

test('next never mutates the state it is given', () => {
  const s = b.initial();
  b.next(s, 'fail');
  assert.equal(b.delay(s), 4000);
});
```

Modify `package.json` to:

```json
{
  "name": "claude-dashboard",
  "private": true,
  "type": "module",
  "scripts": {
    "test": "node --test test/"
  }
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `npm test`
Expected: FAIL — `Cannot read properties of undefined (reading 'initial')` (backoff.js does not exist yet).

- [ ] **Step 3: Write the implementation**

Create `public/transport/backoff.js`:

```js
/* Poll-interval decision logic, deliberately free of DOM so node --test can
   exercise it. Loaded as a classic script by index.html; the IIFE form makes
   the same file a valid side-effect ES module under node --test. */
(function (g) {
  const LADDER = [4000, 8000, 15000, 30000];
  g.cdashBackoff = {
    initial: () => ({ i: 0, halted: false }),
    next(state, outcome) {
      if (outcome === 'ok') return { i: 0, halted: false };
      if (state.halted) return { ...state };
      if (outcome === 'auth') return { i: 0, halted: true };
      return { i: Math.min(state.i + 1, LADDER.length - 1), halted: false };
    },
    delay: state => LADDER[state.i],
  };
})(typeof globalThis === 'undefined' ? this : globalThis);
```

(The `typeof globalThis === 'undefined' ? this : globalThis` fallback is belt-and-braces for ancient engines; in practice both environments have `globalThis`.)

- [ ] **Step 4: Run the tests to verify they pass**

Run: `npm test`
Expected: PASS — 5 tests.

- [ ] **Step 5: Commit**

```bash
git add public/transport/backoff.js test/backoff.test.mjs package.json
git commit -m "feat: pure backoff ladder with reset-on-success and the auth halt"
```

---

### Task 2: `api()` propagates the HTTP status and gains the transport branch

**Files:**
- Modify: `public/app.js:111-116` (the `api()` function)

**Interfaces:**
- Consumes: nothing new.
- Produces: thrown errors from `api()` carry `.status` (the HTTP status code, absent on transport errors). `poll()` in Task 3 reads it. The `isTauri` constant is defined here and re-declared in Task 4's registration snippet.

- [ ] **Step 1: Replace `api()`**

Replace the current `api()` at `public/app.js:111-116` with:

```js
const isTauri = typeof window !== 'undefined' &&
  ('__TAURI_INTERNALS__' in window || '__TAURI__' in window); // ponytail: predicate unconfirmed — spec requires verifying which global the Tauri runtime injects, in step 8

async function api(path, body) {
  const opts = body
    ? { method: 'POST', headers: { 'content-type': 'application/json' }, body: JSON.stringify(body) }
    : undefined;
  const res = isTauri
    ? await window.__TAURI_INTERNALS__.invoke('api', { path, body })
    : await fetch(path, opts);
  const data = await res.json().catch(() => ({}));
  if (!res.ok) {
    const err = new Error(data.error || res.statusText);
    err.status = res.status;
    throw err;
  }
  return data;
}
```

The `invoke('api', …)` command name is provisional — step 8 names the real Tauri command; the shape (path + parsed-or-null body in, Response-like out) is what matters here.

- [ ] **Step 2: Commit**

```bash
git add public/app.js
git commit -m "feat: api() propagates the HTTP status and branches on the Tauri transport"
```

---

### Task 3: `poll()` applies `next()` — the graduated ladder and the auth halt

**Files:**
- Modify: `public/index.html:105` (script tag for backoff.js)
- Modify: `public/app.js:371-388` (poll, interval, visibilitychange)

**Interfaces:**
- Consumes: `cdashBackoff` (Task 1), errors with `.status` (Task 2).
- Produces: none — this is the top of the UI.

- [ ] **Step 1: Load backoff.js before app.js**

In `public/index.html`, immediately above the existing `<script src="app.js"></script>`, add:

```html
<script src="transport/backoff.js"></script>
```

- [ ] **Step 2: Replace the polling tail**

Replace `public/app.js` lines 371–388 (from `async function poll() {` to the end of the file) with:

```js
let bk = cdashBackoff.initial();
let timer = null;

function arm() {
  clearTimeout(timer);
  timer = setTimeout(tick, cdashBackoff.delay(bk));
}

async function tick() {
  if (document.hidden) { arm(); return; }
  let outcome;
  try {
    render(await api('/api/sessions'));
    $('#health').className = 'dot ok';
    $('#health-label').textContent = 'Connected';
    if ($('#logbox').open) $('#logs').textContent = (await api('/api/logs')).lines.join('\n');
    outcome = 'ok';
  } catch (err) {
    // Only transport errors and 401/403 reach here as distinct things;
    // everything non-auth backs off. A halt is cleared only by poll(),
    // which is user-initiated.
    outcome = err.status === 401 || err.status === 403 ? 'auth' : 'fail';
    $('#health').className = 'dot bad';
    $('#health-label').textContent = 'Disconnected';
  }
  bk = cdashBackoff.next(bk, outcome);
  if (!bk.halted) arm();
}

// Any user-initiated action (launch, kill, resume, purge) calls poll():
// reset the ladder and try immediately.
function poll() {
  clearTimeout(timer);
  bk = cdashBackoff.initial();
  tick();
}

// Give the launch button its resting icon + label.
$('#launch').innerHTML = `${ICONS.play}<span>Launch</span>`;

poll();
document.addEventListener('visibilitychange', () => {
  if (!document.hidden) { clearTimeout(timer); bk = cdashBackoff.initial(); tick(); }
});
```

Behaviour notes for the reviewer:
- The single `#health` dot/label is the "persistent disconnected indicator"; no error toasts are raised from polling.
- While hidden, `tick` reschedules at the current delay without fetching — the tab wakes on `visibilitychange`, which resets to 4s and polls immediately.
- An auth halt leaves the timer disarmed; the next click of Launch/Kill/etc. calls `poll()`, which resets and retries once — matching "requires user action".

- [ ] **Step 3: Run the suites**

Run: `npm test && cargo test --all --locked -- --test-threads=1`
Expected: PASS — the Rust suite is untouched but proves the static files still serve.

- [ ] **Step 4: Commit**

```bash
git add public/app.js public/index.html
git commit -m "feat: poll applies the backoff ladder, halts on 401/403, resets on visibility"
```

---

### Task 4: Service worker — runtime caching, registration gated to web mode

**Files:**
- Modify: `public/sw.js` (full rewrite, 9 lines → ~20)
- Modify: `public/index.html:106` (registration)

**Interfaces:**
- Consumes: the `isTauri` predicate definition (Task 2).
- Produces: none.

- [ ] **Step 1: Rewrite sw.js**

Replace the entire contents of `public/sw.js` with the spec's code verbatim:

```js
const CACHE = 'cdash';   // namespace, not a version — never bumped
self.addEventListener('activate', e => e.waitUntil(caches.keys().then(ks => Promise.all(ks.filter(k => k !== CACHE).map(k => caches.delete(k))))));
self.addEventListener('fetch', e => {
  const url = new URL(e.request.url);
  if (url.pathname.startsWith('/api/')) return;                  // network only
  if (e.request.mode === 'navigate') {
    e.respondWith((async () => {
      try {
        const r = await fetch(e.request);
        if (r.status === 200 && !r.redirected &&
            new URL(e.request.url).pathname === '/')               // only '/' writes '/'
          (await caches.open(CACHE)).put('/', r.clone());
        return r;
      } catch { return (await caches.match('/')) || Response.error(); }
    })());
    return;
  }
  e.respondWith(caches.match(e.request).then(hit => {             // stale-while-revalidate
    const fresh = fetch(e.request)
      .then(r => { if (r.ok) caches.open(CACHE).then(c => c.put(e.request, r.clone())); return r; })
      .catch(() => hit);
    return hit || fresh;
  }));
});
```

Why each guard exists (from the spec, restated so the reviewer does not have to reopen it):
- **No precache manifest**: the hand-maintained `SHELL` array drifted four times during design review; runtime caching has nothing to drift. Cost: offline works from the second visit onward.
- **pathname check on the navigation write**: after `302 → /login`, the followed navigation responds `200` unredirected — without the check, the login page would be cached as the application shell.
- **`r.ok` on every put**: `cache.put` happily stores a 401 body (static assets sit behind the guard) and an opaqueredirect; `addAll` used to reject both. This restores fail-closed.
- **No `skipWaiting`/`clients.claim`**: unchanged behaviour, accepted cost.

- [ ] **Step 2: Gate registration to web mode**

In `public/index.html`, replace line 106:

```html
<script>if ('serviceWorker' in navigator) navigator.serviceWorker.register('sw.js');</script>
```

with:

```html
<script>
  // Same predicate as api()'s transport branch. Both of sw.js's assumptions
  // (same-origin /api/, http-cache semantics) break in a Tauri webview.
  if (!('__TAURI_INTERNALS__' in window || '__TAURI__' in window) && 'serviceWorker' in navigator)
    navigator.serviceWorker.register('sw.js').catch(() => {});
</script>
```

- [ ] **Step 3: Run the suites**

Run: `npm test && cargo test --all --locked -- --test-threads=1`
Expected: PASS.

- [ ] **Step 4: Commit**

```bash
git add public/sw.js public/index.html
git commit -m "feat: network-first navigations and SWR sub-resources; registration web-mode only"
```

---

### Task 5: README and closing gate

**Files:**
- Modify: `README.md` (one paragraph)

- [ ] **Step 1: Update the README**

Add a short paragraph after the intro describing the offline/polling model:

```markdown
The dashboard polls `/api/sessions` on a graduated ladder — 4s, 8s, 15s, capped
at 30s — resetting on any successful poll, tab refocus, or button press. A 401
or 403 halts the poll until you act; throttling never does. The service worker
caches at runtime (network-first navigations, stale-while-revalidate assets), so
offline works from the second visit; there is no precache manifest to maintain.
```

- [ ] **Step 2: Full gate**

Run: `npm test && cargo clippy --all-targets --locked -- -D warnings -D clippy::disallowed_types && cargo test --all --locked -- --test-threads=1`
Expected: all PASS / exit 0.

- [ ] **Step 3: Commit**

```bash
git add README.md
git commit -m "docs: describe the polling ladder and runtime-caching service worker"
```

---

## What this plan does not cover

- **Confirming the Tauri predicate** — step 8's job; the `ponytail:` comment in Task 2 marks the site.
- **Steps 8–11** — the Tauri clients themselves; `invokeTauri`'s command name is provisional until step 8.
- **DOM-wiring tests** — per the spec, DOM wiring stays untested, as today.
