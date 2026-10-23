// Web Push for the relay (SPEC §12): payload-free pushes signed with VAPID
// (RFC 8292). A push carries no content at all, so neither the relay nor the
// push service learns anything about a Session; the service worker shows a
// generic "something is waiting" notification. Runs on Workers and Node
// (WebCrypto only).

const enc = new TextEncoder();

export function b64u(bytes) {
  let s = "";
  for (const b of new Uint8Array(bytes)) s += String.fromCharCode(b);
  return btoa(s).replace(/\+/g, "-").replace(/\//g, "_").replace(/=+$/, "");
}

/** `Authorization` for a push to `endpoint`: a 12-hour ES256 JWT for the
 * push service's origin, with the server's public key. */
export async function vapidAuth(endpoint, publicKey, privateJwk, subject) {
  const json = (o) => b64u(enc.encode(JSON.stringify(o)));
  const exp = Math.floor(Date.now() / 1000) + 12 * 3600;
  const unsigned = json({ typ: "JWT", alg: "ES256" }) + "." + json({ aud: new URL(endpoint).origin, exp, sub: subject });
  const key = await crypto.subtle.importKey("jwk", privateJwk, { name: "ECDSA", namedCurve: "P-256" }, false, ["sign"]);
  // WebCrypto signs as raw r||s, which is what JWS ES256 wants.
  const sig = await crypto.subtle.sign({ name: "ECDSA", hash: "SHA-256" }, key, enc.encode(unsigned));
  return "vapid t=" + unsigned + "." + b64u(sig) + ", k=" + publicKey;
}

/** Send one payload-free push; resolves to the HTTP status (404 and 410
 * mean the subscription is gone). `vapid` is {publicKey, privateJwk,
 * subject}. */
export async function sendPush(sub, vapid) {
  const auth = await vapidAuth(sub.endpoint, vapid.publicKey, vapid.privateJwk, vapid.subject);
  const res = await fetch(sub.endpoint, {
    method: "POST",
    headers: { authorization: auth, ttl: "3600", urgency: "high", "content-length": "0" },
  });
  return res.status;
}

/** A subscription worth keeping: an https endpoint (or, for the local dev
 * relay, http on 127.0.0.1), at most 1 KB. */
export function validSubscription(sub, allowLocalHttp) {
  try {
    const url = new URL(sub.endpoint);
    const local = allowLocalHttp && url.protocol === "http:" && url.hostname === "127.0.0.1";
    return typeof sub.endpoint === "string" && JSON.stringify(sub).length <= 1024
      && (url.protocol === "https:" || local);
  } catch (_) {
    return false;
  }
}

/** The VAPID settings from the environment, or null when push is off. */
export function vapidFrom(env) {
  if (!env.VAPID_PUBLIC_KEY || !env.VAPID_PRIVATE_JWK) return null;
  return {
    publicKey: env.VAPID_PUBLIC_KEY,
    privateJwk: JSON.parse(env.VAPID_PRIVATE_JWK),
    subject: env.VAPID_SUBJECT || "mailto:admin@formless.invalid",
  };
}
