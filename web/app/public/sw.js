// Public app shell only. Hashed /assets/* files are cache-first (immutable); the shell document is
// network-first with an offline fallback. /api/*, POSTs and cross-origin requests are never touched.
const CACHE = "nebula-shell-v3";

self.addEventListener("install", (event) => {
  event.waitUntil(caches.open(CACHE).then((cache) => cache.addAll(["/", "/manifest.webmanifest", "/icon.svg"])).then(() => self.skipWaiting()));
});

self.addEventListener("activate", (event) => {
  event.waitUntil(caches.keys().then((keys) => Promise.all(keys.filter((k) => k.startsWith("nebula-shell-") && k !== CACHE).map((k) => caches.delete(k)))).then(() => self.clients.claim()));
});

self.addEventListener("fetch", (event) => {
  const request = event.request;
  const url = new URL(request.url);
  if (request.method !== "GET" || url.origin !== self.location.origin || url.pathname.startsWith("/api/") || url.search) return;
  if (url.pathname.startsWith("/assets/")) {
    event.respondWith(caches.open(CACHE).then(async (cache) => {
      const hit = await cache.match(request);
      if (hit) return hit;
      const response = await fetch(request);
      if (response.ok) cache.put(request, response.clone());
      return response;
    }));
    return;
  }
  if (request.mode === "navigate" || ["/manifest.webmanifest", "/icon.svg", "/icon-192.png", "/icon-512.png"].includes(url.pathname)) {
    const shell = request.mode === "navigate" ? new Request("/", { cache: "no-cache" }) : request;
    event.respondWith(caches.open(CACHE).then(async (cache) => {
      try {
        const response = await fetch(shell);
        if (response.ok) cache.put(shell, response.clone());
        return response;
      } catch {
        return (await cache.match(shell)) || Response.error();
      }
    }));
  }
});
