const CACHE = 'cdash-v3';
const SHELL = ['/', '/style.css', '/app.js', '/manifest.json', '/icon.svg'];
self.addEventListener('install', e => e.waitUntil(caches.open(CACHE).then(c => c.addAll(SHELL))));
self.addEventListener('activate', e => e.waitUntil(caches.keys().then(ks => Promise.all(ks.filter(k => k !== CACHE).map(k => caches.delete(k))))));
self.addEventListener('fetch', e => {
  const url = new URL(e.request.url);
  if (url.pathname.startsWith('/api/')) return; // network only
  e.respondWith(caches.match(e.request).then(hit => hit || fetch(e.request)));
});
