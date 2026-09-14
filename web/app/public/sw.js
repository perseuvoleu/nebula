// Public app shell only. Hashed /assets/* files are cache-first (immutable); the shell document is
// network-first with an offline fallback. /api/*, POSTs and cross-origin requests are never touched.
const CACHE = "nebula-shell-v6";

self.addEventListener("install", (event) => {
  event.waitUntil(caches.open(CACHE).then((cache) => cache.addAll(["/", "/manifest.webmanifest", "/icon.svg"])).then(() => self.skipWaiting()));
});

self.addEventListener("activate", (event) => {
  event.waitUntil(caches.keys().then((keys) => Promise.all(keys.filter((k) => k.startsWith("nebula-shell-") && k !== CACHE).map((k) => caches.delete(k)))).then(() => self.clients.claim()));
});

/** Restrict notification navigation to this app, including persisted notifications. */
function notificationUrl(value) {
  if (typeof value !== "string" || !value.startsWith("/")) return "/";
  try {
    const url = new URL(value, self.location.origin);
    return url.origin === self.location.origin ? url.pathname + url.search + url.hash : "/";
  } catch { return "/"; }
}

self.addEventListener("push", (event) => {
  let data = {};
  try { data = event.data?.json() ?? {}; } catch { /* Always show a visible fallback notification. */ }
  const title = typeof data.title === "string" ? data.title : "Nebula";
  const body = typeof data.body === "string" ? data.body : "Un agent are nevoie de atenție.";
  const url = notificationUrl(data.url);
  const tag = typeof data.tag === "string" ? data.tag : "nebula-agent";
  const tasks = [self.registration.showNotification(title, {
    body, tag, icon: "/icon-192.png", badge: "/icon-192.png", data: { url },
  })];
  if ("setAppBadge" in self.navigator) tasks.push(self.navigator.setAppBadge(1).catch(() => {}));
  event.waitUntil(Promise.all(tasks));
});

self.addEventListener("notificationclick", (event) => {
  event.notification.close();
  const url = notificationUrl(event.notification.data?.url);
  event.waitUntil((async () => {
    if ("clearAppBadge" in self.navigator) await self.navigator.clearAppBadge().catch(() => {});
    const windows = await self.clients.matchAll({ type: "window", includeUncontrolled: true });
    if (windows[0]) {
      await windows[0].navigate(url);
      return windows[0].focus();
    }
    return self.clients.openWindow(url);
  })());
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
