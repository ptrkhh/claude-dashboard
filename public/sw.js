const CACHE = 'cdash';   // namespace, not a version — never bumped
// Network-first is safe to take over mid-session; waiting for every tab to
// close kept browsers on the old cache-first worker.
self.addEventListener('install', () => self.skipWaiting());
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
  // Network first, like the page itself: a cached asset answering first is
  // how a new page ended up running with an old stylesheet. The cache is the
  // offline copy; the agent's ETags keep the network round trip a 304.
  e.respondWith(fetch(e.request).then(r => {
    // !r.redirected is load-bearing: the guard answers an unauthenticated
    // sub-resource with 302 -> /login, not 401, and a followed redirect is a
    // 200 login page that would be cached as app.js.
    if (r.ok && !r.redirected) { const copy = r.clone(); caches.open(CACHE).then(c => c.put(e.request, copy)).catch(() => {}); }
    return r;
  }, async () => (await caches.match(e.request)) || Response.error()));
});
