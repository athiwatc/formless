// Make a VAPID key pair for the relay's Web Push (SPEC §12):
//   node relay/vapid-keys.mjs
// then `wrangler secret put VAPID_PUBLIC_KEY` and `VAPID_PRIVATE_JWK` with
// the two lines it prints (and optionally VAPID_SUBJECT, a mailto: URL).
import { b64u } from "./push.mjs";

const pair = await crypto.subtle.generateKey({ name: "ECDSA", namedCurve: "P-256" }, true, ["sign", "verify"]);
const raw = await crypto.subtle.exportKey("raw", pair.publicKey);
const jwk = await crypto.subtle.exportKey("jwk", pair.privateKey);
console.log("VAPID_PUBLIC_KEY=" + b64u(raw));
console.log("VAPID_PRIVATE_JWK=" + JSON.stringify(jwk));
