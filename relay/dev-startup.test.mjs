import test from "node:test";
import assert from "node:assert/strict";
import { spawn, spawnSync } from "node:child_process";
import { once } from "node:events";
import { createServer } from "node:net";
import { fileURLToPath } from "node:url";

const script = fileURLToPath(new URL("./dev.mjs", import.meta.url));
function run(args) {
  const result = spawnSync(process.execPath, [script, ...args], { encoding: "utf8", timeout: 5000 });
  assert.ifError(result.error);
  assert.equal(result.signal, null, result.stderr);
  assert.doesNotMatch(result.stderr, /\n\s+at |Unhandled|node:events/);
  return result;
}

test("bad dev relay arguments exit 2 with usage and no stack trace", () => {
  for (const args of [
    ["--port"], ["--port=oops"], ["--port=-1"], ["--port=65536"],
    ["--port="], ["--port=1.5"], ["--port=0x10"], ["--port= "],
    ["--mock"], ["--mock-providers"], ["unexpected"],
  ]) {
    const result = run(args);
    assert.equal(result.status, 2, JSON.stringify(args));
    assert.match(result.stderr, /Usage: node relay\/dev.mjs \[--port 8787\] \[--mock-provider\]/);
    assert.equal(result.stdout, "");
  }
});

test("both port syntaxes start on an ephemeral port with the optional mock provider", { timeout: 15000 }, async t => {
  for (const args of [["--port=0"], ["--port", "0", "--mock-provider"]]) {
    const child = spawn(process.execPath, [script, ...args], { stdio: ["ignore", "pipe", "pipe"] });
    let errors = "";
    child.stderr.on("data", chunk => { errors = (errors + chunk).slice(-8192); });
    const exited = once(child, "exit");
    const cleanup = async () => {
      if (child.exitCode !== null || child.signalCode !== null) return;
      child.kill("SIGKILL");
      await exited;
    };
    t.after(cleanup);
    try {
      const output = await new Promise((resolve, reject) => {
        const timer = setTimeout(() => reject(new Error("relay startup deadline")), 4000);
        let text = "";
        child.stdout.on("data", chunk => {
          text = (text + chunk).slice(-8192);
          if (/relay on http:\/\/127\.0\.0\.1:\d+/.test(text)) { clearTimeout(timer); resolve(text); }
        });
        child.once("error", error => { clearTimeout(timer); reject(error); });
        child.once("exit", () => { clearTimeout(timer); reject(new Error("relay exited before listening: " + errors)); });
      });
      const port = Number(output.match(/127\.0\.0\.1:(\d+)/)[1]);
      assert.ok(port > 0 && port <= 65535, output);
      assert.equal(output.includes("mock provider at /mock/v1"), args.includes("--mock-provider"));
      assert.equal(errors, "");
    } finally { await cleanup(); }
  }
});

test("an occupied port exits 1 with an actionable message and no stack trace", async t => {
  const blocker = createServer();
  t.after(() => new Promise(resolve => blocker.close(resolve)));
  blocker.listen(0, "127.0.0.1");
  await once(blocker, "listening");
  const port = blocker.address().port;
  const result = run([`--port=${port}`]);
  assert.equal(result.status, 1);
  assert.match(result.stderr, new RegExp(`port ${port} is already in use; pass --port <other>`));
});
