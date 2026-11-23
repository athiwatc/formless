import test from "node:test";
import assert from "node:assert/strict";
import vm from "node:vm";
import { readFileSync } from "node:fs";

const source = readFileSync(new URL("./web/app.js", import.meta.url), "utf8");
const functionStart = source.indexOf("  function resolveDocsTopic(");
assert.ok(functionStart >= 0, "docs topic resolver exists");
const functionEnd = source.indexOf("\n  }", functionStart) + 4;
assert.ok(functionEnd > functionStart, "docs topic resolver is complete");
const context = vm.createContext({});
vm.runInContext(source.slice(functionStart, functionEnd), context);

const topicsLine = source.split("\n").find((line) => line.includes("const topics = {") && line.includes("docs/architecture.md"));
assert.ok(topicsLine, "web docs topic table exists");
const topics = vm.runInNewContext(`(${topicsLine.trim().replace(/^const topics = /, "").replace(/;$/, "")})`);

test("web /docs topic resolution matches exact, unique-prefix, and ambiguity rules", () => {
  const resolve = (input) => context.resolveDocsTopic(topics, input);
  assert.equal(resolve("").key, "gallery");
  assert.equal(resolve("cli").key, "cli");
  assert.equal(resolve(" ARCh ").key, "architecture");
  assert.equal(resolve("us").key, "usage");
  assert.equal(resolve("a").error, 'ambiguous docs topic "a": matches approvals, architecture');
  assert.equal(resolve("c").error, 'ambiguous docs topic "c": matches capabilities, cli');
  assert.equal(resolve("not-a-topic").error, 'unknown docs topic "not-a-topic"');
});
