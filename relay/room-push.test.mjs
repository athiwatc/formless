import assert from "node:assert/strict";
import test from "node:test";
import { b64u } from "./push.mjs";
import { ackControls, controls, roomFixture } from "./test-room-state.mjs";

const pair = await crypto.subtle.generateKey({ name: "ECDSA", namedCurve: "P-256" }, true, ["sign", "verify"]);
const env = {
  VAPID_PUBLIC_KEY: b64u(await crypto.subtle.exportKey("raw", pair.publicKey)),
  VAPID_PRIVATE_JWK: JSON.stringify(await crypto.subtle.exportKey("jwk", pair.privateKey)),
};
const subscription = name => ({
  endpoint: `https://fcm.googleapis.com/fixture/${name}`,
  keys: { p256dh: env.VAPID_PUBLIC_KEY, auth: b64u(new Uint8Array(16)) },
});

async function setup(t, names) {
  const fixture = roomFixture(env);
  t.after(() => fixture.close());
  const { room, socket, storage } = fixture;
  for (const [index, name] of names.entries()) {
    await storage.put(`push:${index}`, subscription(name));
  }
  const daemon = socket();
  room.admit(daemon, "daemon", true, "", true);
  return { storage, push: kind => room.webSocketMessage(daemon, JSON.stringify({ relay: "push", kind })) };
}

test("ready push refusals preserve the connection and keep storage within capacity", async t => {
  const fixture = roomFixture(env);
  t.after(() => fixture.close());
  const { room, socket, storage } = fixture;
  const daemon = socket(), browser = socket();
  room.admit(daemon, "daemon", true, "", true);
  await ackControls(room, daemon);
  room.admit(browser, "client", true, "fixture");
  await ackControls(room, daemon);
  await ackControls(room, browser);
  const send = (ws, packet) => room.webSocketMessage(ws, JSON.stringify(packet));
  await send(daemon, { relay: "ready", cid: browser.att.cid, g: browser.att.g });
  await ackControls(room, daemon);
  await ackControls(room, browser);
  const subscribe = sub => send(browser, { relay: "push_subscribe", sub });
  const last = () => controls(browser).at(-1);

  await subscribe({ ...subscription("invalid"), endpoint: "https://fixture.invalid/push" });
  assert.deepEqual(last(), { relay: "push_refused", reason: "invalid", operation: "subscribe" });
  await ackControls(room, browser);

  for (let i = 0; i < 10; i++) {
    await subscribe(subscription(i));
    assert.deepEqual(last(), { relay: "push_subscribed" });
    await ackControls(room, browser);
  }
  await subscribe(subscription(10));
  assert.deepEqual(last(), { relay: "push_refused", reason: "capacity", operation: "subscribe" });
  await ackControls(room, browser);
  assert.equal((await storage.list({ prefix: "push:" })).size, 10);

  const id = crypto.randomUUID();
  const remove = () => send(browser, { relay: "push_unsubscribe", endpoint: subscription(0).endpoint, id });
  room.pushJobs.add(browser.att.sid);
  await remove();
  assert.deepEqual(last(), { relay: "push_refused", reason: "busy", operation: "unsubscribe", id });
  await ackControls(room, browser);
  room.pushJobs.delete(browser.att.sid);
  await remove();
  assert.deepEqual(last(), { relay: "push_unsubscribed", id });
  await ackControls(room, browser);
  await subscribe(subscription(10));
  assert.deepEqual(last(), { relay: "push_subscribed" });
  assert.equal(browser.closed, null);
  assert.equal(daemon.closed, null);
});

test("pushes arriving during a fan-out trigger one trailing pass with current subscriptions", { timeout: 3_000 }, async t => {
  const { push, storage } = await setup(t, ["pending", "healthy", "expired"]);
  const gate = Promise.withResolvers();
  const started = Promise.withResolvers();
  const calls = [];
  t.mock.method(globalThis, "fetch", endpoint => {
    const name = new URL(endpoint).pathname.split("/").at(-1);
    calls.push(name);
    if (calls.length === 3) started.resolve();
    if (name === "pending" && calls.filter(item => item === name).length === 1) return gate.promise;
    return Promise.resolve(new Response(null, { status: name === "expired" ? 410 : 201 }));
  });

  const delivery = push("approval");
  await started.promise;
  await push("question");
  await push("approval");
  await storage.put("push:3", subscription("joined"));
  gate.resolve(new Response(null, { status: 201 }));
  await delivery;

  assert.equal(calls.filter(name => name === "healthy").length, 2);
  assert.equal(calls.filter(name => name === "pending").length, 2);
  assert.equal(calls.filter(name => name === "expired").length, 1);
  assert.equal(calls.filter(name => name === "joined").length, 1);
  assert.equal((await storage.list({ prefix: "push:" })).has("push:2"), false);
});

test("a hung subscription times out while healthy deliveries use bounded slots", { timeout: 3_000 }, async t => {
  const names = ["unresponsive", ...Array.from({ length: 9 }, (_, index) => `healthy-${index}`)];
  const { push, storage } = await setup(t, names);
  t.mock.timers.enable({ apis: ["setTimeout"] });
  const errors = [];
  t.mock.method(console, "error", message => errors.push(message));
  const hung = Promise.withResolvers();
  const firstHealthy = Promise.withResolvers();
  const calls = new Map();
  let active = 0;
  let peak = 0;
  let closed = 0;
  t.mock.method(globalThis, "fetch", async (endpoint, { signal }) => {
    const name = new URL(endpoint).pathname.split("/").at(-1);
    calls.set(name, (calls.get(name) || 0) + 1);
    active++;
    peak = Math.max(peak, active);
    if (name === "unresponsive") {
      if (calls.get(name) === 1) {
        const pending = Promise.withResolvers();
        signal?.addEventListener("abort", () => {
          active--;
          pending.reject(signal.reason);
        }, { once: true });
        hung.resolve(signal);
        return pending.promise;
      }
      active--;
      return new Response(null, { status: 201 });
    }
    return new Response(new ReadableStream({
      cancel() {
        active--;
        if (++closed === 9) firstHealthy.resolve();
      },
    }), { status: 201 });
  });

  const delivery = push("approval");
  const signal = await hung.promise;
  assert.ok(signal, "the push request needs a cancellation signal");
  await firstHealthy.promise;
  await push("question");
  // Let the healthy cleanup continuations clear their timers before ticking.
  await new Promise(resolve => setImmediate(resolve));
  t.mock.timers.tick(9_999);
  assert.equal(signal.aborted, false);
  t.mock.timers.tick(1);
  await delivery;

  assert.equal(signal.aborted, true);
  assert.equal(active, 0);
  assert.ok(peak <= 4, `peak push connections ${peak}`);
  assert.equal(closed, 18);
  for (const name of names) assert.equal(calls.get(name), 2, name);
  assert.deepEqual(errors, ["relay: push timed out"]);
  assert.deepEqual((await storage.list({ prefix: "push:" })).get("push:0"), subscription("unresponsive"));
});

test("stalled body cancellation releases the fan-out and preserves an expired status", { timeout: 3_000 }, async t => {
  const { push, storage } = await setup(t, ["expired", "healthy"]);
  t.mock.timers.enable({ apis: ["setTimeout"] });
  const errors = [];
  t.mock.method(console, "error", message => errors.push(message));
  const cleanup = Promise.withResolvers();
  const calls = [];
  t.mock.method(globalThis, "fetch", async (endpoint, { signal }) => {
    const name = new URL(endpoint).pathname.split("/").at(-1);
    calls.push(name);
    if (name === "healthy") return new Response(null, { status: 201 });
    return new Response(new ReadableStream({
      cancel() {
        cleanup.resolve(signal);
        return new Promise(() => {});
      },
    }), { status: 410 });
  });

  const delivery = push("approval");
  const signal = await cleanup.promise;
  await push("question");
  t.mock.timers.tick(10_000);
  await delivery;

  assert.equal(signal.aborted, true);
  assert.equal(calls.filter(name => name === "expired").length, 1);
  assert.equal(calls.filter(name => name === "healthy").length, 2);
  assert.equal((await storage.list({ prefix: "push:" })).has("push:0"), false);
  assert.deepEqual(errors, ["relay: push response cleanup timed out"]);
});

test("a queued push still runs when the previous pass cannot list subscriptions", { timeout: 3_000 }, async t => {
  const { push, storage } = await setup(t, ["healthy"]);
  const started = Promise.withResolvers();
  const blocked = Promise.withResolvers();
  const originalList = storage.list.bind(storage);
  let reads = 0;
  t.mock.method(storage, "list", options => {
    if (++reads === 1) {
      started.resolve();
      return blocked.promise;
    }
    return originalList(options);
  });
  const errors = [];
  t.mock.method(console, "error", message => errors.push(message));
  const fetch = t.mock.method(globalThis, "fetch", async () => new Response(null, { status: 201 }));
  const delivery = push("approval");
  await started.promise;
  await push("question");
  blocked.reject(new Error("private fixture storage path"));
  await delivery;
  assert.equal(reads, 2);
  assert.equal(fetch.mock.callCount(), 1);
  assert.deepEqual(errors, ["relay: push fan-out failed"]);
});
