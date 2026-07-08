/* ── Service worker ───────────────────────────────────────────
   Strategy:
     - SHELL_ASSETS (explicit allow-list of shell files) → cache-first.
       Immediate second-load + works on flaky mobile networks. The list
       is small on purpose; it's the bare-bones shared assets every
       page uses, nothing per-page.
     - Everything else (HTML, /admin/'*'/data, /ws, per-page admin_*.js
       and admin_*.css) → network-only. Per-page admin JS in particular
       MUST stay network-only — caching it forced users to do a hard
       reload (Ctrl+Shift+R) to pick up fixes, and the only signal
       something was stale was the page misbehaving.
   Bump SHELL_CACHE on every static-asset change to bust old caches.
   ───────────────────────────────────────────────────────────── */

// Bump on every static-asset change AND on every change to the cache
// policy below — the activate handler deletes old shell caches so a
// version bump guarantees stale entries (e.g. an old admin_*.js that
// got opportunistically cached under a previous, broader fetch rule)
// get purged on the user's next reload.
const SHELL_CACHE = "klappstuhl-shell-v4";

// Set of paths we'll serve cache-first. Anything not in this set goes
// straight to the network even if it lives under /static/, so admin
// pages always get the freshest JS/CSS without needing a hard reload.
const SHELL_ASSET_SET = new Set();
// Populated below from SHELL_ASSETS so the two lists can't drift.

// Resources we want available offline. Includes the page entry-points
// (well, just /login since that's the only useful no-auth page) plus
// the bare-essentials assets every page pulls in.
const SHELL_ASSETS = [
  "/kls/base.css",
  "/kls/base.js",
  "/static/js/live.js",
  "/static/img/logo.png",
  "/static/img/favicon.ico",
  "/static/img/discord.svg",
  "/static/img/visibility.svg",
  "/static/img/visibility_off.svg",
  "/static/site.webmanifest",
];
for (const a of SHELL_ASSETS) SHELL_ASSET_SET.add(a);

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

  // Cache-first ONLY for the explicit shell allow-list (base.css,
  // base.js, live.js, favicon, etc.) Everything else under /static/ —
  // notably per-page admin_*.js / admin_*.css — falls through to the
  // browser's default network fetch so a regular reload always picks
  // up the latest version. Previously the handler cache-first'd all of
  // /static/, which silently bound users to whichever admin JS got
  // opportunistically cached on first visit; the only escape was a
  // hard reload (Ctrl+Shift+R) until the SW version was bumped.
  if (SHELL_ASSET_SET.has(url.pathname)) {
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
