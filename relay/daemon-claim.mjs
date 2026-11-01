// Daemon authentication is separate from the pairing/E2E key. Room v3 is
// self-certifying: no first-writer registration can pin an attacker's verifier.
export const DAEMON_CLAIM_INFO = "formless-relay/3 daemon-claim";
const hex = (bytes) => [...new Uint8Array(bytes)].map((b) => b.toString(16).padStart(2, "0")).join("");

export async function daemonClaim(keyBytes) {
  if (!(keyBytes instanceof Uint8Array) || keyBytes.length !== 32) throw new Error("expected 32-byte pairing key");
  const key = await crypto.subtle.importKey("raw", keyBytes, "HKDF", false, ["deriveBits"]);
  return new Uint8Array(await crypto.subtle.deriveBits({ name: "HKDF", hash: "SHA-256", salt: new Uint8Array(), info: new TextEncoder().encode(DAEMON_CLAIM_INFO) }, key, 256));
}
export async function claimRoom(claim) {
  return hex(await crypto.subtle.digest("SHA-256", claim)).slice(0, 32);
}
export function claimAuthorization(claim) {
  if (!(claim instanceof Uint8Array) || claim.length !== 32) throw new Error("expected 32-byte daemon claim");
  return "Bearer " + hex(claim);
}
export async function verifyDaemonClaim(room, authorization) {
  const match = typeof authorization === "string" && /^Bearer ([a-f0-9]{64})$/.exec(authorization);
  if (!/^[a-f0-9]{32}$/.test(room) || !match) return false;
  const claim = Uint8Array.from(match[1].match(/../g), (pair) => parseInt(pair, 16));
  let actual;
  try { actual = await claimRoom(claim); } finally { claim.fill(0); }
  // Compare the public commitment without an early differing-character exit.
  let difference = 0;
  for (let i = 0; i < room.length; i++) difference |= room.charCodeAt(i) ^ actual.charCodeAt(i);
  return difference === 0;
}
