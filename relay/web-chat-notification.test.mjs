import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import { runInNewContext } from "node:vm";

const app = readFileSync(new URL("./web/app.js", import.meta.url), "utf8");
const worker = readFileSync(new URL("./web/sw.js", import.meta.url), "utf8");
const origin = "https://formless.example";
const plain = value => JSON.parse(JSON.stringify(value));

function appFunction(name) {
  const start = app.indexOf("  async function " + name + "(");
  assert.ok(start >= 0, name);
  return app.slice(start, app.indexOf("\n  }", start) + 4);
}

test("page chooses a worker first and keeps Session and event tags distinct", async () => {
  const messages = [];
  const c = { navigator: { serviceWorker: { getRegistration: async () => ({
    active: { postMessage: data => messages.push(plain(data)) },
  }) } }, Notification: () => assert.fail("the worker should handle the notification") };
  runInNewContext(appFunction("showChatNotification"), c);
  for (const [session, kind] of [["a", "approval_requested"], ["b", "approval_requested"], ["a", "user_question"]]) {
    await c.showChatNotification("Test", "synthetic body", kind, session);
  }
  assert.deepEqual(messages.map(m => m.options.tag), [
    "formless:a:approval_requested", "formless:b:approval_requested", "formless:a:user_question",
  ]);
  assert.ok(messages.every(m => m.type === "formless-chat-notification" && m.options.renotify));
  assert.deepEqual(messages.map(m => m.options.data.session), ["a", "b", "a"]);
});

test("page fallback focuses and opens a known Session", async () => {
  const events = [];
  let notification;
  class Notification {
    constructor(title, options) { notification = this; this.title = title; this.options = plain(options); }
    close() { events.push("close"); }
  }
  const S = { daemonOnline: true, local: true, sessions: [{ id: "known" }] };
  const c = {
    S, navigator: {}, Notification, window: { focus: () => events.push("focus") },
    openSession: async id => events.push(id), refreshSessions: async () => {},
    note: () => assert.fail("the target exists"),
  };
  runInNewContext(appFunction("showChatNotification") + appFunction("openNotificationSession"), c);
  await c.showChatNotification("Question", "synthetic body", "user_question", "known");
  assert.equal(notification.options.tag, "formless:known:user_question");
  notification.onclick();
  assert.deepEqual(events, ["close", "focus", "known"]);
  S.daemonOnline = false;
  await c.openNotificationSession("deferred");
  assert.equal(S.linkedSession, "deferred");
});

test("worker notification opens the sending tab or a Session link if it was closed", async () => {
  const handlers = {};
  const calls = [];
  const win = {
    id: "source-client", type: "window", url: origin + "/",
    focus: async () => calls.push("focus"),
    postMessage: data => calls.push(plain(data)),
  };
  let isOpen = true;
  runInNewContext(worker, {
    self: {
      addEventListener: (type, fn) => { handlers[type] = fn; },
      registration: { showNotification: async (title, options) => calls.push([title, plain(options)]) },
      clients: {
        get: async id => isOpen && id === win.id ? win : null,
        matchAll: async () => [win],
        openWindow: async path => calls.push(path),
      },
    },
    URL, location: { origin },
  });
  let pending;
  handlers.message({
    source: win,
    data: { type: "formless-chat-notification", title: "Approval", options: {
      body: "synthetic body", tag: "formless:test:approval", data: { session: "test session" },
    } },
    waitUntil: promise => { pending = promise; },
  });
  await pending;
  const [[title, notice]] = calls;
  assert.equal(title, "Approval");
  assert.equal(notice.tag, "formless:test:approval");
  assert.deepEqual(notice.data, { session: "test session", client: win.id });
  const click = async data => {
    handlers.notificationclick({
      notification: { data, close: () => calls.push("close") },
      waitUntil: promise => { pending = promise; },
    });
    await pending;
  };
  await click(notice.data);
  assert.deepEqual(calls.slice(-3), ["close", "focus", {
    type: "formless-open-notification-session", session: "test session",
  }]);
  isOpen = false;
  await click(notice.data);
  assert.deepEqual(calls.slice(-2), ["close", "/#s=test%20session"]);
  await click(undefined);
  assert.deepEqual(calls.slice(-3), ["close", "focus", { type: "formless-focus-pending" }]);
});
