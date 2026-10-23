// End to end against the dev relay: a browser subscribes, the daemon asks
// for a push, and the (fake, local) push service gets a payload-free POST
// signed with VAPID.   node relay/push.dev-test.mjs
import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import { createServer } from "node:http";
import { b64u } from "./push.mjs";

const pair = await crypto.subtle.generateKey({ name: "ECDSA", namedCurve: "P-256" }, true, ["sign", "verify"]);
const env = {
  ...process.env,
  VAPID_PUBLIC_KEY: b64u(await crypto.subtle.exportKey("raw", pair.publicKey)),
  VAPID_PRIVATE_JWK: JSON.stringify(await crypto.subtle.exportKey("jwk", pair.privateKey)),
};
const got = new Promise((resolve) => {
  const svc = createServer((req, res) => {
    let body = "";
    req.on("data", (c) => (body += c));
    req.on("end", () => { res.writeHead(201); res.end(); svc.close(); resolve({ method: req.method, auth: req.headers.authorization, body, ttl: req.headers.ttl }); });
  }).listen(0, "127.0.0.1");
  svc.on("listening", () => (got.port = svc.address().port));
});
await new Promise((r) => setTimeout(r, 50));
const relayPort = 18000 + Math.floor(Math.random() * 1000);
const relay = spawn(process.execPath, [new URL("./dev.mjs", import.meta.url).pathname, "--port", String(relayPort)], { env, stdio: "ignore" });
try {
  await new Promise((r) => setTimeout(r, 500));
  const key = await (await fetch(`http://127.0.0.1:${relayPort}/push/key`)).json();
  assert.equal(key.key, env.VAPID_PUBLIC_KEY);
  const room = "a".repeat(32);
  const open = (role) => new Promise((resolve) => {
    const ws = new WebSocket(`ws://127.0.0.1:${relayPort}/relay/${room}?role=${role}&mux=1`);
    ws.onopen = () => resolve(ws);
  });
  const client = await open("client");
  const port = await new Promise((r) => { const t = setInterval(() => { if (got.port) { clearInterval(t); r(got.port); } }, 10); });
  client.send(JSON.stringify({ relay: "push_subscribe", sub: { endpoint: `http://127.0.0.1:${port}/push/abc`, keys: {} } }));
  await new Promise((r) => setTimeout(r, 100));
  const daemon = await open("daemon");
  daemon.send(JSON.stringify({ relay: "push", kind: "approval" }));
  const req = await Promise.race([got, new Promise((_, rej) => setTimeout(() => rej(new Error("no push arrived")), 5000))]);
  assert.equal(req.method, "POST");
  assert.equal(req.body, "", "no payload");
  assert.match(req.auth, /^vapid t=[\w-]+\.[\w-]+\.[\w-]+, k=[\w-]+$/);
  assert.equal(req.ttl, "3600");
  client.close();
  daemon.close();
  console.log("dev relay push ok");
} finally {
  relay.kill();
}
