// One behaviour matrix for any relay server (#836): HTTP statuses and
// headers, and the room's control frames.
//   node relay/conformance.mjs                 (starts dev.mjs and checks it)
//   node relay/conformance.mjs http://127.0.0.1:8787   (e.g. `wrangler dev`)
import assert from "node:assert/strict";
import { spawn } from "node:child_process";

let base = process.argv[2];
let dev = null;
if (!base) {
  const port = 19000 + Math.floor(Math.random() * 1000);
  const env = { ...process.env };
  delete env.VAPID_PUBLIC_KEY;
  delete env.VAPID_PRIVATE_JWK;
  dev = spawn(process.execPath, [new URL("./dev.mjs", import.meta.url).pathname, "--port", String(port)], { env, stdio: "ignore" });
  base = "http://127.0.0.1:" + port;
  await new Promise((r) => setTimeout(r, 500));
}
const wsBase = base.replace(/^http/, "ws");
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

try {
  // HTTP: health, push key without VAPID, a missing asset.
  const health = await (await fetch(base + "/health")).json();
  assert.deepEqual([health.ok, health.service, health.version], [true, "formless-relay", 2]);
  const key = await fetch(base + "/push/key");
  assert.equal(key.status, 404);
  assert.match(key.headers.get("content-type") || "", /^text\/plain/);
  const missing = await fetch(base + "/no-such-file.js");
  assert.equal(missing.status, 404);
  assert.ok(missing.headers.get("content-security-policy"), "404s carry the CSP");
  assert.equal(missing.headers.get("x-content-type-options"), "nosniff");

  // Room: a mux daemon and one browser.
  const room = Array.from({ length: 32 }, () => "0123456789abcdef"[Math.floor(Math.random() * 16)]).join("");
  const open = (q) => new Promise((resolve, reject) => {
    const ws = new WebSocket(wsBase + "/relay/" + room + q);
    ws.got = [];
    ws.onmessage = (e) => ws.got.push(String(e.data));
    ws.onopen = () => resolve(ws);
    ws.onerror = reject;
  });
  const daemon = await open("?role=daemon&mux=1");
  const client = await open("?role=client");
  const closed = new Promise((r) => (client.onclose = (e) => r(e.code)));
  await sleep(150);
  const opened = daemon.got.map((t) => JSON.parse(t)).find((m) => m.relay === "client_open");
  assert.ok(opened, daemon.got.join("\n"));
  assert.ok(client.got.includes('{"relay":"daemon_online"}'), client.got.join("\n"));
  client.got.length = 0;

  // Spaced control JSON from the daemon is a control frame, not a broadcast.
  daemon.send('{"relay": "push"}');
  await sleep(200);
  assert.deepEqual(client.got, [], "a spaced push is not forwarded to browsers");

  // A malformed browser control frame reaches the daemon as data.
  daemon.got.length = 0;
  client.send('{"relay"');
  await sleep(200);
  const routed = daemon.got.map((t) => JSON.parse(t)).find((m) => m.f === '{"relay"');
  assert.ok(routed && routed.cid === opened.cid, daemon.got.join("\n"));

  // A routed reply reaches that browser only.
  daemon.send(JSON.stringify({ cid: opened.cid, f: "hello" }));
  await sleep(150);
  assert.deepEqual(client.got, ["hello"]);

  // Disconnect codes: 4002 for a broken channel.
  daemon.send(JSON.stringify({ relay: "disconnect", cid: opened.cid, code: 4002 }));
  assert.equal(await Promise.race([closed, sleep(2000).then(() => "timeout")]), 4002);

  // A second daemon is refused while one is connected.
  await assert.rejects(open("?role=daemon&mux=1"));
  daemon.close();
  console.log("conformance ok: " + base);
} finally {
  if (dev) dev.kill();
}
