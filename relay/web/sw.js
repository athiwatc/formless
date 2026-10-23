// Formless app shell (SPEC §12): network first, so updates arrive at once;
// the cached shell only when the network fails, so the page still opens
// offline and says the daemon is unreachable. Only the static shell is
// cached: never /auth, /ws or /relay/ (and URL fragments, which carry the
// key, never reach a service worker).
const CACHE = "formless-shell-v1";
const SHELL = ["/", "/app.js", "/app.css", "/manifest.webmanifest", "/icon-192.png", "/icon-512.png"];

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
          caches.open(CACHE).then((c) => c.put(path, copy));
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

self.addEventListener("notificationclick", (e) => {
  e.notification.close();
  e.waitUntil(self.clients.matchAll({ type: "window", includeUncontrolled: true }).then((wins) => {
    const win = wins.find((w) => new URL(w.url).origin === location.origin);
    return win ? win.focus() : self.clients.openWindow("/");
  }));
});
