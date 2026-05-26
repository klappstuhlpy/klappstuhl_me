/* ── Service worker ───────────────────────────────────────────
   Strategy:
     - /static/* → cache-first (the shell rarely changes; immediate
       second-load + works on flaky mobile networks).
     - Everything else (HTML, /admin/*/data, /ws, etc.) → network-only.
       Admin data is too dynamic to be useful when stale.
   Bump SHELL_CACHE on every static-asset change to bust old caches.
   ───────────────────────────────────────────────────────────── */

const SHELL_CACHE = "klappstuhl-shell-v1";

// Resources we want available offline. Includes the page entry-points
// (well, just /login since that's the only useful no-auth page) plus
// the bare-essentials assets every page pulls in.
const SHELL_ASSETS = [
  "/static/base.css",
  "/static/base.js",
  "/static/live.js",
  "/static/img/logo.jpg",
  "/static/img/favicon.ico",
  "/static/img/visibility.svg",
  "/static/img/visibility_off.svg",
  "/static/site.webmanifest",
];

self.addEventListener("install", (event) => {
  // Pre-populate the shell cache. failure on any single asset shouldn't
  // block install — pages can still work, the asset just won't be cached.
  event.waitUntil(
    caches.open(SHELL_CACHE).then((cache) =>
      Promise.allSettled(SHELL_ASSETS.map((url) => cache.add(url)))
    )
  );
  self.skipWaiting();
});

self.addEventListener("activate", (event) => {
  // Drop old versions of the shell cache so we don't accumulate stale
  // entries across deploys.
  event.waitUntil(
    caches.keys().then((keys) =>
      Promise.all(keys
        .filter((k) => k.startsWith("klappstuhl-shell-") && k !== SHELL_CACHE)
        .map((k) => caches.delete(k))
      )
    )
  );
  self.clients.claim();
});

self.addEventListener("fetch", (event) => {
  const req = event.request;
  if (req.method !== "GET") return;            // POSTs/PUTs etc. — skip
  const url = new URL(req.url);
  if (url.origin !== location.origin) return;  // cross-origin (CDN) — skip

  // Cache-first for /static/*. Everything else goes straight to the
  // network so admin data is always fresh.
  if (url.pathname.startsWith("/static/")) {
    event.respondWith(cacheFirst(req));
  }
});

async function cacheFirst(req) {
  const cache = await caches.open(SHELL_CACHE);
  const cached = await cache.match(req);
  if (cached) return cached;
  try {
    const resp = await fetch(req);
    // Stash a copy if the response is OK. Opaque/error responses are
    // left alone so the next request can retry.
    if (resp && resp.ok) {
      cache.put(req, resp.clone());
    }
    return resp;
  } catch (e) {
    // Fully offline + nothing in cache → propagate the error so the
    // browser shows its own "no connection" UI.
    throw e;
  }
}
