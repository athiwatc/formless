// Formless app shell (SPEC §12): network first, so updates arrive at once;
// the cached shell only when the network fails, so the page still opens
// offline and says the daemon is unreachable. Only the static shell is
// cached: never /auth, /ws or /relay/ (and URL fragments, which carry the
// key, never reach a service worker).
const CACHE = "formless-shell-v2";
const SHELL = ["/", "/app.js", "/transport.mjs", "/app.css", "/manifest.webmanifest", "/icon-192.png", "/icon-512.png"];

self.addEventListener("install", (e) => {
  e.waitUntil(caches.open(CACHE).then((c) => c.addAll(SHELL)).then(() => self.skipWaiting()));
});

self.addEventListener("activate", (e) => {
  e.waitUntil(
    caches.keys()
      .then((keys) => Promise.all(keys.filter((k) => k !== CACHE).map((k) => caches.delete(k))))
      .then(() => self.clients.claim()),
  );
});

self.addEventListener("fetch", (e) => {
  const url = new URL(e.request.url);
  const path = url.pathname === "/index.html" ? "/" : url.pathname;
  if (e.request.method !== "GET" || url.origin !== location.origin || !SHELL.includes(path)) return;
  e.respondWith(
    fetch(e.request)
      .then((res) => {
        if (res.ok) {
          const copy = res.clone();
          e.waitUntil(caches.open(CACHE).then((c) => c.put(path, copy)).catch(() => {}));
        }
        return res;
      })
      .catch(() => caches.match(path).then((hit) => hit || Response.error())),
  );
});

// Web Push (opt-in, SPEC §12): pushes carry no content, so the notice is
// generic; opening it brings the app to the front.
self.addEventListener("push", (e) => {
  e.waitUntil(self.registration.showNotification("Formless", {
    body: "An approval or a question is waiting for you.",
    icon: "icon-192.png",
    tag: "formless-waiting",
    renotify: true,
  }));
});

// Chat notices carry the same local data as the page notification and
// keep the tab ID so a click cannot accidentally switch a different tab.
self.addEventListener("message", (e) => {
  const message = e.data;
  const source = e.source;
  if (message?.type !== "formless-chat-notification" || source?.type !== "window"
    || new URL(source.url).origin !== location.origin) return;
  const { title, options } = message;
  if (typeof title !== "string" || typeof options?.tag !== "string") return;
  const session = options.data?.session;
  e.waitUntil(self.registration.showNotification(title, {
    body: options.body,
    icon: "icon-192.png",
    tag: options.tag,
    renotify: true,
    data: { session: typeof session === "string" ? session : null, client: source.id },
  }));
});

self.addEventListener("notificationclick", (e) => {
  e.notification.close();
  e.waitUntil((async () => {
    const { session, client } = e.notification.data || {};
    if (client) {
      const win = await self.clients.get(client);
      if (win?.type === "window" && new URL(win.url).origin === location.origin) {
        await win.focus();
        if (session) win.postMessage({ type: "formless-open-notification-session", session });
        return;
      }
      return self.clients.openWindow(session ? "/#s=" + encodeURIComponent(session) : "/");
    }
    const wins = await self.clients.matchAll({ type: "window", includeUncontrolled: true });
    const win = wins.find((w) => new URL(w.url).origin === location.origin);
    return win ? win.focus() : self.clients.openWindow("/");
  })());
});
