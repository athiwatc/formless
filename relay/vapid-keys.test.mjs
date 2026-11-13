import test from "node:test";
import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { mkdtempSync, writeFileSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { fileURLToPath } from "node:url";
import { loadVapid } from "./push.mjs";

const script = fileURLToPath(new URL("./vapid-keys.mjs", import.meta.url));
function run(...args) {
  const result = spawnSync(process.execPath, [script, ...args], { encoding: "utf8", timeout: 5000 });
  assert.ifError(result.error);
  return result;
}

test("saved pair selectors print bare, matching values without regenerating keys", async t => {
  const dir = mkdtempSync(join(tmpdir(), "formless-vapid-"));
  t.after(() => rmSync(dir, { recursive: true, force: true }));
  const file = join(dir, "pair.json");
  const generated = run("--json");
  assert.equal(generated.status, 0);
  const pair = JSON.parse(generated.stdout);
  writeFileSync(file, generated.stdout, { mode: 0o600 });
  const publicValue = run("--public", file), privateValue = run("--private", file);
  assert.equal(publicValue.status, 0);
  assert.equal(privateValue.status, 0);
  assert.equal(publicValue.stdout.trim(), pair.VAPID_PUBLIC_KEY);
  assert.equal(privateValue.stdout.trim(), pair.VAPID_PRIVATE_JWK);
  assert.equal((await loadVapid({
    VAPID_PUBLIC_KEY: publicValue.stdout.trim(), VAPID_PRIVATE_JWK: privateValue.stdout.trim(),
  })).state, "ok");
  assert.equal(run("--public", file).stdout, publicValue.stdout);
});

test("default output retains NAME=value lines containing one valid pair", async () => {
  const result = run();
  assert.equal(result.status, 0);
  const env = Object.fromEntries(result.stdout.trim().split("\n").map(line => {
    const at = line.indexOf("="); return [line.slice(0, at), line.slice(at + 1)];
  }));
  assert.deepEqual(Object.keys(env), ["VAPID_PUBLIC_KEY", "VAPID_PRIVATE_JWK"]);
  assert.equal((await loadVapid(env)).state, "ok");
});

test("invalid selector invocations fail with usage and no secret output", t => {
  const dir = mkdtempSync(join(tmpdir(), "formless-vapid-bad-"));
  t.after(() => rmSync(dir, { recursive: true, force: true }));
  const file = join(dir, "pair.json");
  writeFileSync(file, '{"VAPID_PRIVATE_JWK":"synthetic-secret"', { mode: 0o600 });
  for (const args of [["--public"], ["--private", file], ["--public", join(dir, "missing")],
    ["--json", "--public", file], ["--public", file, "--private", file], ["--unknown"]]) {
    const result = run(...args);
    assert.equal(result.status, 2);
    assert.equal(result.stdout, "");
    assert.match(result.stderr, /Usage:/);
    assert.doesNotMatch(result.stderr, /synthetic-secret|\n\s+at /);
  }
});
