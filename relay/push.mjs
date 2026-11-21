// Web Push for the relay (SPEC §12): payload-free pushes signed with VAPID
// (RFC 8292). A push carries no content at all, so neither the relay nor the
// push service learns anything about a Session; the service worker shows a
// generic "something is waiting" notification. Runs on Workers and Node
// (WebCrypto only).

const enc = new TextEncoder();
const PUSH_TIMEOUT_MS = 10_000;

export function b64u(bytes) {
  let s = "";
  for (const b of new Uint8Array(bytes)) s += String.fromCharCode(b);
  return btoa(s).replace(/\+/g, "-").replace(/\//g, "_").replace(/=+$/, "");
}

/** `Authorization` for a push to `endpoint`: a 12-hour ES256 JWT for the
 * push service's origin, with the server's public key. */
export async function vapidAuth(endpoint, publicKey, privateKey, subject) {
  const json = (o) => b64u(enc.encode(JSON.stringify(o)));
  const exp = Math.floor(Date.now() / 1000) + 12 * 3600;
  const unsigned = json({ typ: "JWT", alg: "ES256" }) + "." + json({ aud: new URL(endpoint).origin, exp, sub: subject });
  // A CryptoKey (imported once by loadVapid) or a private JWK.
  const key = privateKey instanceof CryptoKey ? privateKey
    : await crypto.subtle.importKey("jwk", privateKey, { name: "ECDSA", namedCurve: "P-256" }, false, ["sign"]);
  // WebCrypto signs as raw r||s, which is what JWS ES256 wants.
  const sig = await crypto.subtle.sign({ name: "ECDSA", hash: "SHA-256" }, key, enc.encode(unsigned));
  return "vapid t=" + unsigned + "." + b64u(sig) + ", k=" + publicKey;
}

/** Send one payload-free push; resolves to the HTTP status (404 and 410
 * mean the subscription is gone; 400, 401 and 403 mean the push service
 * refused our VAPID signature). `vapid` is from `loadVapid`. */
export async function sendPush(sub, vapid) {
  const auth = await vapidAuth(sub.endpoint, vapid.publicKey, vapid.key, vapid.subject);
  const controller = new AbortController();
  const timeout = new DOMException("push timed out", "TimeoutError");
  let timer;
  const deadline = new Promise(resolve => {
    timer = setTimeout(() => {
      resolve(null);
      controller.abort(timeout);
    }, PUSH_TIMEOUT_MS);
  });
  try {
    const request = fetch(sub.endpoint, {
      method: "POST",
      headers: { authorization: auth, ttl: "3600", urgency: "high", "content-length": "0" },
      signal: controller.signal,
    });
    const res = await Promise.race([request, deadline]);
    if (!res) throw timeout;
    if (res.body) {
      // Cleanup shares the request deadline. A received 404 or 410 still
      // identifies an expired subscription if cleanup fails or stalls.
      const cleaned = await Promise.race([res.body.cancel().catch(() => controller.abort()), deadline]);
      if (cleaned === null) console.error("relay: push response cleanup timed out");
    }
    return res.status;
  } catch (error) {
    if (controller.signal.reason === timeout) throw timeout;
    throw error;
  } finally {
    clearTimeout(timer);
  }
}

/** The browsers' push services (#837). A self-hosted relay adds its own
 * with PUSH_HOSTS (comma-separated host names; `.example.com` matches
 * subdomains). */
export const PUSH_HOSTS = [
  "fcm.googleapis.com", // Chrome, Edge (Chromium), Opera, Brave
  "updates.push.services.mozilla.com", // Firefox
  ".push.apple.com", // Safari
  ".notify.windows.com", // Edge (legacy WNS)
];

function b64uBytes(s) {
  if (typeof s !== "string" || !/^[A-Za-z0-9_-]+={0,2}$/.test(s)) return null;
  try {
    const bin = atob(s.replace(/=+$/, "").replace(/-/g, "+").replace(/_/g, "/"));
    return Uint8Array.from(bin, (c) => c.charCodeAt(0));
  } catch (_) {
    return null;
  }
}

function hostAllowed(host, extra) {
  const list = PUSH_HOSTS.concat(String(extra || "").split(",").map((h) => h.trim().toLowerCase()).filter(Boolean));
  return list.some((h) => (h.startsWith(".") ? host.endsWith(h) : host === h));
}

/** A real PushSubscription: an https endpoint on a known push service (or,
 * for the local dev relay, http on 127.0.0.1), its p256dh (65-byte P-256
 * point) and auth (16 bytes) keys, at most 1 KB. Anything else would make
 * the relay POST to whatever URL a client chose (#837). */
export function validSubscription(sub, allowLocalHttp, extraHosts) {
  try {
    if (!sub || typeof sub.endpoint !== "string" || JSON.stringify(sub).length > 1024) return false;
    const url = new URL(sub.endpoint);
    const local = allowLocalHttp && url.protocol === "http:" && url.hostname === "127.0.0.1";
    if (!local && !(url.protocol === "https:" && hostAllowed(url.hostname.toLowerCase(), extraHosts))) return false;
    const p256dh = b64uBytes(sub.keys && sub.keys.p256dh);
    const auth = b64uBytes(sub.keys && sub.keys.auth);
    return !!p256dh && p256dh.length === 65 && p256dh[0] === 4 && !!auth && auth.length === 16;
  } catch (_) {
    return false;
  }
}

/** Push configuration from the environment, checked once (#838):
 * {state: "off"} with no keys, {state: "misconfigured", reason} when the
 * JWK does not parse, is not the private half of VAPID_PUBLIC_KEY, fails
 * a sign/verify round trip, or the subject is not mailto:/https:, else
 * {state: "ok", vapid: {publicKey, key, subject}}. Never throws. */
export async function loadVapid(env) {
  if (!env.VAPID_PUBLIC_KEY && !env.VAPID_PRIVATE_JWK) return { state: "off" };
  const bad = (reason) => ({ state: "misconfigured", reason });
  for (const name of ["VAPID_PUBLIC_KEY", "VAPID_PRIVATE_JWK"]) {
    if (typeof env[name] === "string" && env[name].startsWith(name + "=")) {
      return bad(`${name} includes its '${name}=' prefix; store only the value`);
    }
  }
  if (!env.VAPID_PUBLIC_KEY || !env.VAPID_PRIVATE_JWK) return bad("set both VAPID_PUBLIC_KEY and VAPID_PRIVATE_JWK");
  let jwk;
  try { jwk = JSON.parse(env.VAPID_PRIVATE_JWK); } catch (_) { return bad("VAPID_PRIVATE_JWK is not JSON"); }
  if (!jwk || jwk.kty !== "EC" || jwk.crv !== "P-256" || typeof jwk.d !== "string") return bad("VAPID_PRIVATE_JWK is not a P-256 private key");
  const point = b64uBytes(env.VAPID_PUBLIC_KEY);
  if (!point || point.length !== 65 || point[0] !== 4) return bad("VAPID_PUBLIC_KEY is not a 65-byte P-256 point");
  if (jwk.x !== b64u(point.slice(1, 33)) || jwk.y !== b64u(point.slice(33))) return bad("VAPID_PRIVATE_JWK does not match VAPID_PUBLIC_KEY (rotate both together)");
  const subject = env.VAPID_SUBJECT || "mailto:admin@formless.invalid";
  if (!/^(mailto:[^\s@]+@[^\s@]+|https:\/\/\S+)$/.test(subject)) return bad("VAPID_SUBJECT must be a mailto: or https: URL");
  try {
    const curve = { name: "ECDSA", namedCurve: "P-256" };
    const key = await crypto.subtle.importKey("jwk", { kty: "EC", crv: "P-256", x: jwk.x, y: jwk.y, d: jwk.d }, curve, false, ["sign"]);
    const pub = await crypto.subtle.importKey("raw", point, curve, false, ["verify"]);
    const probe = enc.encode("formless vapid self-test");
    const sig = await crypto.subtle.sign({ name: "ECDSA", hash: "SHA-256" }, key, probe);
    if (!(await crypto.subtle.verify({ name: "ECDSA", hash: "SHA-256" }, pub, sig, probe))) return bad("the VAPID key pair failed a sign/verify self-test");
    return { state: "ok", vapid: { publicKey: env.VAPID_PUBLIC_KEY, key, subject } };
  } catch (e) {
    return bad("the VAPID keys could not be imported (" + (e && e.message) + ")");
  }
}

/** Push service answers that mean our VAPID setup is wrong, not that one
 * subscription is gone. */
export const isConfigFailure = (status) => status === 400 || status === 401 || status === 403;
