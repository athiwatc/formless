// node relay/push.test.mjs
import assert from "node:assert/strict";
import { b64u, isConfigFailure, loadVapid, vapidAuth, validSubscription } from "./push.mjs";

const pair = await crypto.subtle.generateKey({ name: "ECDSA", namedCurve: "P-256" }, true, ["sign", "verify"]);
const pub = b64u(await crypto.subtle.exportKey("raw", pair.publicKey));
const jwk = await crypto.subtle.exportKey("jwk", pair.privateKey);

const auth = await vapidAuth("https://push.example.com/send/abc", pub, jwk, "mailto:me@example.com");
const m = auth.match(/^vapid t=([\w-]+)\.([\w-]+)\.([\w-]+), k=([\w-]+)$/);
assert.ok(m, auth);
assert.equal(m[4], pub);
const dec = (s) => JSON.parse(Buffer.from(s, "base64url").toString());
assert.deepEqual(dec(m[1]), { typ: "JWT", alg: "ES256" });
const claims = dec(m[2]);
assert.equal(claims.aud, "https://push.example.com");
assert.equal(claims.sub, "mailto:me@example.com");
assert.ok(claims.exp > Date.now() / 1000 && claims.exp <= Date.now() / 1000 + 24 * 3600);
// The signature verifies with the public key.
const ok = await crypto.subtle.verify({ name: "ECDSA", hash: "SHA-256" }, pair.publicKey,
  Buffer.from(m[3], "base64url"), new TextEncoder().encode(m[1] + "." + m[2]));
assert.ok(ok, "signature verifies");

// Subscriptions: a known push service and real-shaped keys (#837).
const keys = { p256dh: pub, auth: b64u(crypto.getRandomValues(new Uint8Array(16))) };
assert.ok(validSubscription({ endpoint: "https://fcm.googleapis.com/fcm/send/abc", keys }));
assert.ok(validSubscription({ endpoint: "https://web.push.apple.com/QX", keys }));
assert.ok(validSubscription({ endpoint: "https://updates.push.services.mozilla.com/wpush/v2/x", keys }));
assert.ok(!validSubscription({ endpoint: "https://push.example.com/x", keys }), "not a push service");
assert.ok(!validSubscription({ endpoint: "https://hooks.example.com/webhook", keys }));
assert.ok(validSubscription({ endpoint: "https://push.example.com/x", keys }, false, "push.example.com"), "PUSH_HOSTS override");
assert.ok(!validSubscription({ endpoint: "https://fcm.googleapis.com/x", keys: {} }), "keyless");
assert.ok(!validSubscription({ endpoint: "https://fcm.googleapis.com/x", keys: { p256dh: "abc", auth: keys.auth } }), "short p256dh");
assert.ok(!validSubscription({ endpoint: "http://fcm.googleapis.com/x", keys }), "plain http");
assert.ok(validSubscription({ endpoint: "http://127.0.0.1:9/x", keys }, true), "dev relay");
assert.ok(!validSubscription({ endpoint: "https://fcm.googleapis.com/" + "a".repeat(2000), keys }));

// VAPID config is checked once and never throws (#838).
const env = { VAPID_PUBLIC_KEY: pub, VAPID_PRIVATE_JWK: JSON.stringify(jwk) };
assert.equal((await loadVapid({})).state, "off");
const loaded = await loadVapid(env);
assert.equal(loaded.state, "ok", loaded.reason);
assert.ok(loaded.vapid.key instanceof CryptoKey);
assert.match(await vapidAuth("https://fcm.googleapis.com/x", pub, loaded.vapid.key, loaded.vapid.subject), /^vapid t=/);
const bad = async (e) => (await loadVapid(e)).state;
assert.equal(await bad({ ...env, VAPID_PRIVATE_JWK: "{not json" }), "misconfigured");
assert.equal(await bad({ VAPID_PUBLIC_KEY: pub }), "misconfigured");
for (const name of ["VAPID_PUBLIC_KEY", "VAPID_PRIVATE_JWK"]) {
  const prefixed = { ...env, [name]: name + "=" + env[name] };
  const result = await loadVapid(prefixed);
  assert.equal(result.state, "misconfigured");
  assert.equal(result.reason, `${name} includes its '${name}=' prefix; store only the value`);
  assert.ok(!result.reason.includes(env[name]), "diagnostic must not echo the secret");
}
const other = await crypto.subtle.generateKey({ name: "ECDSA", namedCurve: "P-256" }, true, ["sign", "verify"]);
const mismatched = { ...env, VAPID_PRIVATE_JWK: JSON.stringify(await crypto.subtle.exportKey("jwk", other.privateKey)) };
assert.match((await loadVapid(mismatched)).reason, /does not match/);
assert.equal(await bad({ ...env, VAPID_SUBJECT: "admin" }), "misconfigured");
assert.ok(isConfigFailure(401) && isConfigFailure(403) && isConfigFailure(400) && !isConfigFailure(410));
console.log("push ok");
