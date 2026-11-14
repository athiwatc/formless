// Make one VAPID key pair for the relay's Web Push (SPEC §12).
// Default output is NAME=value for dev environments. Use --json to save a
// pair, then --public FILE / --private FILE to print bare secret values.
import { readFile } from "node:fs/promises";
import { parseArgs } from "node:util";
import { b64u, loadVapid } from "./push.mjs";

try {
  const { values } = parseArgs({
    strict: true,
    options: { json: { type: "boolean" }, public: { type: "string" }, private: { type: "string" } },
  });
  if ([values.json, values.public !== undefined, values.private !== undefined].filter(Boolean).length > 1) {
    throw new Error("choose only one of --json, --public FILE or --private FILE");
  }
  let env;
  const file = values.public ?? values.private;
  if (file !== undefined) {
    try { env = JSON.parse(await readFile(file, "utf8")); }
    catch (_) { throw new Error("could not read saved VAPID pair JSON"); }
    const loaded = env && typeof env === "object" ? await loadVapid(env) : { state: "off" };
    if (loaded.state !== "ok") throw new Error("invalid saved VAPID pair: " + (loaded.reason || "missing keys"));
  } else {
    const pair = await crypto.subtle.generateKey({ name: "ECDSA", namedCurve: "P-256" }, true, ["sign", "verify"]);
    const raw = await crypto.subtle.exportKey("raw", pair.publicKey);
    const jwk = await crypto.subtle.exportKey("jwk", pair.privateKey);
    env = { VAPID_PUBLIC_KEY: b64u(raw), VAPID_PRIVATE_JWK: JSON.stringify(jwk) };
  }
  if (values.public !== undefined) console.log(env.VAPID_PUBLIC_KEY);
  else if (values.private !== undefined) console.log(env.VAPID_PRIVATE_JWK);
  else if (values.json) console.log(JSON.stringify(env));
  else {
    console.log("VAPID_PUBLIC_KEY=" + env.VAPID_PUBLIC_KEY);
    console.log("VAPID_PRIVATE_JWK=" + env.VAPID_PRIVATE_JWK);
  }
} catch (error) {
  console.error("vapid-keys: " + error.message);
  console.error("Usage: node relay/vapid-keys.mjs [--json | --public FILE | --private FILE]");
  process.exitCode = 2;
}
