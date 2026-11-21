import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";

const source = readFileSync(new URL("./pi_extension.ts", import.meta.url), "utf8");
const { default: register } = await import(`data:text/javascript,${encodeURIComponent(source)}`);

function inputHandler(restoreLeadingAt) {
  const previous = process.env.FORMLESS_PI_MCP_CONFIG;
  process.env.FORMLESS_PI_MCP_CONFIG = JSON.stringify({
    name: "formless_test",
    command: "/fake/mcp",
    cwd: "/fake/cwd",
    args: [],
    envVars: [],
    restoreLeadingAt,
  });
  const handlers = new Map();
  try {
    register({ registerMcpServer() {}, on: (name, handler) => handlers.set(name, handler) });
    return handlers.get("input");
  } finally {
    if (previous === undefined) delete process.env.FORMLESS_PI_MCP_CONFIG;
    else process.env.FORMLESS_PI_MCP_CONFIG = previous;
  }
}

test("restores leading @ input byte-for-byte only on the first input", () => {
  for (const text of ["@alice please review the parser", "@@literal $HOME\nnext line", "@file.txt"]) {
    const handler = inputHandler(true);
    assert.deepEqual(handler({ text: `\n${text}` }), { action: "transform", text });
    assert.equal(handler({ text: `\n${text}` }), undefined);
  }
});

test("does not strip original whitespace from unescaped prompts", () => {
  for (const flag of [false, undefined]) {
    for (const text of ["\n@alice keep this newline", "@literal", "ordinary prompt", ""]) {
      assert.equal(inputHandler(flag)({ text }), undefined);
    }
  }
});

test("does not rewrite an input without the transport prefix", () => {
  assert.equal(inputHandler(true)({ text: "ordinary prompt" }), undefined);
});

test("rejects a malformed prompt restoration flag", () => {
  assert.throws(() => inputHandler("true"), /invalid FORMLESS_PI_MCP_CONFIG/);
});
