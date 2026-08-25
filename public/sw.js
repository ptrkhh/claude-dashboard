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
