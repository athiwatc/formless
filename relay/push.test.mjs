// node relay/push.test.mjs
import assert from "node:assert/strict";
import { b64u, vapidAuth, validSubscription, vapidFrom } from "./push.mjs";

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

assert.ok(validSubscription({ endpoint: "https://push.example.com/x", keys: {} }));
assert.ok(!validSubscription({ endpoint: "http://push.example.com/x" }));
assert.ok(!validSubscription({ endpoint: "https://x/" + "a".repeat(2000) }));
assert.equal(vapidFrom({}), null);
console.log("push ok");
