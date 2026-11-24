import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { runInNewContext } from "node:vm";
import { webcrypto } from "node:crypto";

const source = readFileSync(new URL("./web/sw.js", import.meta.url), "utf8");
const manifest = JSON.parse(readFileSync(new URL("./web/manifest.webmanifest", import.meta.url), "utf8"));
function workerListeners({ self: worker = {}, ...globals } = {}) {
  const listeners = {};
  runInNewContext(source, {
    self: { ...worker, addEventListener: (type, handler) => { listeners[type] = handler; } },
    location: { origin: "https://formless.example" }, URL, Response, ...globals,
  });
  return listeners;
}
function fetchEvent({ fetch, caches, path = "/app.js", method = "GET", request, globals = {} }) {
  const listeners = {};
  runInNewContext(source, {
    self: { addEventListener: (type, handler) => { listeners[type] = handler; } },
    location: { origin: "https://formless.example" }, URL, Response, fetch, caches,
    ...globals,
  });
  const pending = [];
  const event = {
    request: request || new Request(new URL(path, "https://formless.example"), { method }),
    respondWith(response) { this.response = response; },
    waitUntil(work) { pending.push(work); },
  };
  listeners.fetch(event);
  return { event, pending };
}

function shareDatabase(records) {
  const store = {
    put(share) {
      records.set(share.token, share);
      queueMicrotask(() => transaction.oncomplete?.());
    },
  };
  let transaction;
  const db = {
    objectStoreNames: { contains: () => true },
    createObjectStore() {},
    transaction() {
      transaction = { objectStore: () => store };
      return transaction;
    },
    close() {},
  };
  return {
    open() {
      const request = {};
      queueMicrotask(() => {
        request.result = db;
        request.onsuccess?.();
      });
      return request;
    },
  };
}

test("network response is immediate while waitUntil retains the shell cache write", async () => {
  let completeWrite, storedPath, storedResponse, finished = false;
  const write = new Promise(resolve => { completeWrite = resolve; });
  const response = new Response("updated shell");
  const { event, pending } = fetchEvent({
    fetch: async () => response,
    caches: { open: async () => ({ put: (path, copy) => {
      storedPath = path; storedResponse = copy; return write;
    } }) },
  });
  assert.equal(await event.response, response);
  assert.equal(pending.length, 1);
  const lifetime = pending[0].then(() => { finished = true; });
  await Promise.resolve();
  assert.equal(finished, false);
  assert.equal(storedPath, "/app.js");
  assert.equal(await storedResponse.text(), "updated shell");
  assert.equal(await response.text(), "updated shell");
  completeWrite();
  await lifetime;
  assert.equal(finished, true);
});

test("cache open/write failures do not reject the event lifetime or replace the network response", async () => {
  for (const step of ["open", "put"]) {
    const response = new Response("online");
    const fail = async () => { throw new Error("quota exceeded"); };
    const { event, pending } = fetchEvent({
      fetch: async () => response,
      caches: { open: step === "open" ? fail : async () => ({ put: fail }) },
    });
    assert.equal(await event.response, response);
    assert.equal(pending.length, 1);
    await Promise.all(pending);
  }
});

test("offline index alias still falls back to the cached root", async () => {
  const cached = new Response("offline shell");
  const { event, pending } = fetchEvent({
    path: "/index.html",
    fetch: async () => { throw new Error("offline"); },
    caches: { match: async path => { assert.equal(path, "/"); return cached; } },
  });
  assert.equal(await event.response, cached);
  assert.equal(pending.length, 0);
});

test("unsuccessful responses and non-shell requests do not write to the cache", async () => {
  const response = new Response("not found", { status: 404 });
  for (const path of ["/app.js", "/auth", "https://other.example/app.js"]) {
    const { event, pending } = fetchEvent({ path, fetch: async () => response, caches: {} });
    assert.equal(await event.response, path === "/app.js" ? response : undefined);
    assert.equal(pending.length, 0);
  }
});

test("installed app shortcuts route to Main Chat and approvals", () => {
  assert.deepEqual(manifest.shortcuts.map(shortcut => [shortcut.name, shortcut.url, shortcut.description]), [
    ["Main Chat", "/?action=main-chat", "Open Main Chat in Formless."],
    ["Approvals", "/?view=approvals", "Manage tool grants in Formless."],
  ]);
});

test("generic waiting notifications focus the pending item or launch its focus route", async () => {
  const messages = [];
  let focused = 0, opened;
  const win = {
    type: "window",
    url: "https://formless.example/",
    async focus() { focused++; },
    postMessage(message) { messages.push(message); },
  };
  const click = (listeners, data) => {
    let lifetime;
    listeners.notificationclick({
      notification: { data, close() {} },
      waitUntil(promise) { lifetime = promise; },
    });
    return lifetime;
  };
  const listeners = workerListeners({ self: { clients: {
    get: async () => null,
    matchAll: async () => [win],
    openWindow: async url => { opened = url; },
  } } });
  await click(listeners, null);
  assert.equal(focused, 1);
  assert.equal(JSON.stringify(messages), JSON.stringify([{ type: "formless-focus-pending" }]));
  assert.equal(opened, undefined);

  const closedListeners = workerListeners({ self: { clients: {
    get: async () => null,
    matchAll: async () => [],
    openWindow: async url => { opened = url; },
  } } });
  await click(closedListeners, null);
  assert.equal(opened, "/?action=focus-pending");
});

test("Session-specific notification clicks still open their Session", async () => {
  const messages = [];
  let focused = 0;
  const win = {
    type: "window",
    url: "https://formless.example/",
    async focus() { focused++; },
    postMessage(message) { messages.push(message); },
  };
  const listeners = workerListeners({ self: { clients: {
    get: async () => win,
    matchAll: async () => [],
    openWindow: async () => assert.fail("existing client should be used"),
  } } });
  let lifetime;
  listeners.notificationclick({
    notification: { data: { client: "client-1", session: "topic-chat" }, close() {} },
    waitUntil(promise) { lifetime = promise; },
  });
  await lifetime;
  assert.equal(focused, 1);
  assert.equal(JSON.stringify(messages), JSON.stringify([{ type: "formless-open-notification-session", session: "topic-chat" }]));
});

test("share target rejects explicit cross-site request metadata before reading or storing the body", async () => {
  for (const headers of [
    new Headers({ Origin: "https://attacker.example" }),
    new Headers({ Origin: "https://attacker.example", "Sec-Fetch-Site": "none" }),
    new Headers({ Origin: "null" }),
    new Headers({ Origin: "null", "Sec-Fetch-Site": "cross-site" }),
    new Headers({ Origin: "null", "Sec-Fetch-Site": "same-site" }),
    new Headers({ "Sec-Fetch-Site": "cross-site" }),
    new Headers({ "Sec-Fetch-Site": "same-site" }),
    new Headers(),
  ]) {
    const records = new Map();
    let formReads = 0;
    const request = {
      url: "https://formless.example/share",
      method: "POST",
      mode: "navigate",
      headers,
      formData: async () => { formReads++; return new FormData(); },
    };
    const { event } = fetchEvent({
      request,
      fetch: async () => new Response("unexpected"),
      caches: {},
      globals: { indexedDB: shareDatabase(records), crypto: webcrypto },
    });
    assert.equal((await event.response).status, 403);
    assert.equal(formReads, 0);
    assert.equal(records.size, 0);
  }
});

test("share target accepts native opaque Origin:null with Sec-Fetch-Site:none", async () => {
  const records = new Map();
  const file = new File(["photo"], "photo.png", { type: "image/png" });
  const form = new FormData();
  form.set("title", "Useful page");
  form.set("text", "Look at this");
  form.set("url", "https://example.test/page");
  form.set("files", file, file.name);
  const request = {
    url: "https://formless.example/share",
    method: "POST",
    mode: "navigate",
    headers: new Headers({ Origin: "null", "Sec-Fetch-Site": "none" }),
    formData: async () => form,
  };
  const { event } = fetchEvent({
    request,
    fetch: async () => new Response("unexpected"),
    caches: {},
    globals: { indexedDB: shareDatabase(records), crypto: webcrypto },
  });
  const response = await event.response;
  assert.equal(response.status, 303);
  const destination = new URL(response.headers.get("location"));
  assert.equal(destination.pathname, "/");
  const token = destination.searchParams.get("share");
  assert.ok(token);
  assert.equal(records.get(token).title, "Useful page");
  assert.equal(records.get(token).text, "Look at this");
  assert.equal(records.get(token).url, "https://example.test/page");
  assert.equal(records.get(token).files[0].name, "photo.png");
  assert.equal(records.get(token).draftApplied, false);
});
