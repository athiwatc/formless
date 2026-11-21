import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";

const source = readFileSync(new URL("./pi_extension.ts", import.meta.url), "utf8");
const { default: register } = await import(`data:text/javascript,${encodeURIComponent(source)}`);

function handlers() {
  const previous = process.env.FORMLESS_PI_MCP_CONFIG;
  process.env.FORMLESS_PI_MCP_CONFIG = JSON.stringify({
    name: "formless_test",
    command: "/fake/mcp",
    cwd: "/fake/cwd",
    args: [],
    envVars: [],
  });
  const handlers = new Map();
  try {
    register({ registerMcpServer() {}, on: (name, handler) => handlers.set(name, handler) });
    return handlers;
  } finally {
    if (previous === undefined) delete process.env.FORMLESS_PI_MCP_CONFIG;
    else process.env.FORMLESS_PI_MCP_CONFIG = previous;
  }
}

test("does not install an input rewrite hook for RPC prompts", () => {
  assert.equal(handlers().has("input"), false);
  assert.equal(typeof handlers().get("session_start"), "function");
});

test("requires the official MCP API", () => {
  assert.throws(() => register({}), /requires Pi/);
});
