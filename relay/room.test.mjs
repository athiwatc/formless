// node relay/room.test.mjs
// Room teardown (#910): an error alone, or an error then a close, tells
// the other side exactly once; the daemon's disconnect codes reach the
// browser (#905); production pages connect to their own origin only (#907).
import assert from "node:assert/strict";
import { RelayRoom, SECURITY_HEADERS } from "./worker.js";

function socket(att) {
  return {
    att, readyState: 1, sent: [], closed: null,
    deserializeAttachment() { return this.att; },
    send(t) { this.sent.push(t); },
    close(code, reason) { this.closed = { code, reason }; this.readyState = 3; },
  };
}
const daemon = socket({ role: "daemon", mux: true });
const browser = socket({ role: "client", cid: "c1" });
const tags = { daemon: [daemon], client: [browser], c1: [browser] };
const room = new RelayRoom({ getWebSockets: (t) => tags[t] || [] }, {});

// A browser whose socket errors: the daemon hears client_close once.
await room.webSocketError(browser);
await room.webSocketClose(browser);
const closes = daemon.sent.filter((t) => JSON.parse(t).relay === "client_close");
assert.equal(closes.length, 1, daemon.sent.join("\n"));
assert.equal(JSON.parse(closes[0]).cid, "c1");

// A daemon whose socket errors: browsers hear daemon_offline once.
const b2 = socket({ role: "client", cid: "c2" });
tags.client = [b2];
await room.webSocketError(daemon);
await room.webSocketClose(daemon);
assert.equal(b2.sent.filter((t) => JSON.parse(t).relay === "daemon_offline").length, 1);

// Disconnect codes: 4002 for a broken channel, else 4001.
const d2 = socket({ role: "daemon", mux: true });
const b3 = socket({ role: "client", cid: "c3" });
const b4 = socket({ role: "client", cid: "c4" });
tags.c3 = [b3];
tags.c4 = [b4];
await room.webSocketMessage(d2, JSON.stringify({ relay: "disconnect", cid: "c3", code: 4002 }));
await room.webSocketMessage(d2, JSON.stringify({ relay: "disconnect", cid: "c4" }));
assert.equal(b3.closed.code, 4002);
assert.equal(b4.closed.code, 4001);

assert.match(SECURITY_HEADERS["content-security-policy"], /connect-src 'self';/);
console.log("room ok");

// /health says whether push works, without secret material (#838).
const worker = (await import("./worker.js")).default;
const health = await (await worker.fetch(new Request("https://relay.test/health"), { VAPID_PUBLIC_KEY: "x", VAPID_PRIVATE_JWK: "{bad" })).json();
assert.equal(health.push, "misconfigured");
assert.ok(!JSON.stringify(health).includes("{bad"));
console.log("health ok");
