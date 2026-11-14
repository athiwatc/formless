import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { runInNewContext } from "node:vm";

const source = readFileSync(new URL("./web/sw.js", import.meta.url), "utf8");
function fetchEvent({ fetch, caches, path = "/app.js", method = "GET" }) {
  const listeners = {};
  runInNewContext(source, {
    self: { addEventListener: (type, handler) => { listeners[type] = handler; } },
    location: { origin: "https://formless.example" }, URL, Response, fetch, caches,
  });
  const pending = [];
  const event = {
    request: new Request(new URL(path, "https://formless.example"), { method }),
    respondWith(response) { this.response = response; },
    waitUntil(work) { pending.push(work); },
  };
  listeners.fetch(event);
  return { event, pending };
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
