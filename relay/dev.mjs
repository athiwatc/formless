// Local stand-in for the Cloudflare Worker, for developing the Web frontend
// without deploying: serves web/ with the production security headers and
// implements the same relay room semantics (one daemon per room, per-client
// routing for ?mux=1 daemons, daemon_online/offline notices). No dependencies.
//
//   node relay/dev.mjs [--port 8787] [--mock-provider]
//
// --mock-provider also serves a scripted OpenAI-compatible endpoint at
// /mock/v1 that streams reasoning, creates and calls a Tool (so the
// approval flow shows up), then answers in Markdown. A message containing
// "markdown demo" instead streams a showcase of every rendered block
// (tables, alerts, code, Mermaid). Point a throwaway
// profile at it: provider.base_url = "http://127.0.0.1:8787/mock/v1",
// relay.url = "ws://127.0.0.1:8787".
import { createServer } from "node:http";
import { createHash, randomUUID } from "node:crypto";
import { readFile } from "node:fs/promises";
import { extname, join, normalize } from "node:path";
import { fileURLToPath } from "node:url";
import { SECURITY_HEADERS } from "./worker.js";

const args = process.argv.slice(2);
const port = Number(args[args.indexOf("--port") + 1]) || 8787;
const mock = args.includes("--mock-provider");
const webRoot = fileURLToPath(new URL("./web/", import.meta.url));
const TYPES = { ".html": "text/html; charset=utf-8", ".js": "text/javascript; charset=utf-8", ".css": "text/css; charset=utf-8" };

// ------------------------------------------------------------ websockets
function wsAccept(req, socket) {
  const accept = createHash("sha1").update(req.headers["sec-websocket-key"] + "258EAFA5-E914-47DA-95CA-C5AB0DC85B11").digest("base64");
  socket.write("HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: " + accept + "\r\n\r\n");
  const conn = { socket, open: true, onText: () => {}, onClose: () => {} };
  let buf = Buffer.alloc(0);
  let fragments = [];
  socket.on("data", (chunk) => {
    buf = Buffer.concat([buf, chunk]);
    for (;;) {
      if (buf.length < 2) return;
      const fin = buf[0] & 0x80, op = buf[0] & 0x0f, masked = buf[1] & 0x80;
      let len = buf[1] & 0x7f, off = 2;
      if (len === 126) { if (buf.length < 4) return; len = buf.readUInt16BE(2); off = 4; }
      else if (len === 127) { if (buf.length < 10) return; len = Number(buf.readBigUInt64BE(2)); off = 10; }
      const maskOff = off;
      if (masked) off += 4;
      if (buf.length < off + len) return;
      const payload = Buffer.from(buf.subarray(off, off + len));
      if (masked) for (let i = 0; i < len; i++) payload[i] ^= buf[maskOff + (i % 4)];
      buf = buf.subarray(off + len);
      if (op === 0x8) return close(conn);
      if (op === 0x9) { sendFrame(conn, 0xa, payload); continue; }
      if (op === 0x1 || op === 0x0) {
        fragments.push(payload);
        if (fin) { const text = Buffer.concat(fragments).toString("utf8"); fragments = []; conn.onText(text); }
      }
    }
  });
  socket.on("end", () => close(conn)); // peer went away (e.g. a killed daemon)
  socket.on("close", () => close(conn));
  socket.on("error", () => close(conn));
  return conn;
}
function sendFrame(conn, op, payload) {
  if (!conn.open) return;
  const len = payload.length;
  const head = len < 126 ? Buffer.from([0x80 | op, len])
    : len < 65536 ? Buffer.from([0x80 | op, 126, len >> 8, len & 255])
    : Buffer.concat([Buffer.from([0x80 | op, 127]), (() => { const b = Buffer.alloc(8); b.writeBigUInt64BE(BigInt(len)); return b; })()]);
  conn.socket.write(Buffer.concat([head, payload]));
}
const sendText = (conn, text) => sendFrame(conn, 0x1, Buffer.from(text, "utf8"));
function close(conn) {
  if (!conn.open) return;
  conn.open = false;
  try { conn.socket.end(); } catch (_) {}
  conn.onClose();
}

// ----------------------------------------------------------------- rooms
const rooms = new Map(); // room -> { daemon, mux, clients:Map<cid, conn> }
function joinRoom(room, role, conn, reject, mux) {
  const r = rooms.get(room) || { daemon: null, mux: false, clients: new Map() };
  rooms.set(room, r);
  const notifyDaemon = (kind, cid) => { if (r.daemon && r.mux) sendText(r.daemon, JSON.stringify({ relay: kind, cid })); };
  if (role === "daemon") {
    if (r.daemon) return reject();
    r.daemon = conn;
    r.mux = mux;
    for (const cid of r.clients.keys()) notifyDaemon("client_open", cid);
    for (const c of r.clients.values()) sendText(c, JSON.stringify({ relay: "daemon_online" }));
    conn.onText = (t) => {
      let routed = null;
      if (r.mux) { try { routed = JSON.parse(t); } catch (_) {} }
      if (routed && typeof routed.cid === "string" && typeof routed.f === "string") {
        const c = r.clients.get(routed.cid);
        if (c) sendText(c, routed.f);
        return;
      }
      for (const c of r.clients.values()) sendText(c, t);
    };
    conn.onClose = () => {
      r.daemon = null;
      for (const c of r.clients.values()) sendText(c, JSON.stringify({ relay: "daemon_offline" }));
    };
  } else {
    const cid = randomUUID();
    r.clients.set(cid, conn);
    notifyDaemon("client_open", cid);
    sendText(conn, JSON.stringify({ relay: r.daemon ? "daemon_online" : "daemon_offline" }));
    conn.onText = (t) => { if (r.daemon) sendText(r.daemon, r.mux ? JSON.stringify({ cid, f: t }) : t); };
    conn.onClose = () => { r.clients.delete(cid); notifyDaemon("client_close", cid); };
  }
  console.log("relay: " + role + " joined " + room.slice(0, 8) + "… (" + r.clients.size + " client(s), daemon " + (r.daemon ? "online" : "offline") + ")");
}

// --------------------------------------------------------- mock provider
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const HOME_TOOL = {
  name: "home_dir",
  description: "Return the user's home directory from $HOME",
  input_schema: { type: "object", properties: {} },
  output_schema: { type: "object" },
  implementation_js: "function run(input) {\n  return { home: env.get(\"HOME\") };\n}\n",
  capabilities: ["env:HOME"],
};
const MARKDOWN_DEMO = [
  "## Markdown demo", "",
  "Inline: **bold**, *italic*, ~~struck~~, `code`, [a link](https://example.com), https://example.org and $E = mc^2$.", "",
  "1. Ordered item", "2. With a nested list", "   - [x] done task", "   - [ ] open task", "", "---", "",
  "> [!TIP]", "> Tools persist between Runs.", "", "> A plain quote.", "",
  "| Tool | Calls | Rate |", "|:-----|------:|:----:|", "| page_title | 12 | 92% |", "| grep_logs | 3 | 67% |", "",
  "```rust", "fn main() {", "    // say hi", "    println!(\"hi {}\", 42);", "}", "```", "",
  "```diff", "--- a/x", "+++ b/x", "@@ -1 +1 @@", "-old", "+new", "```", "",
  "```mermaid", "flowchart TD", "    U[User] --> A{Tool exists?}", "    A -->|yes| R(Run tool)", "    A -->|no| C[create_tool]", "    C --> R", "    R --> M[(Metrics)]", "```", "",
  "```mermaid", "sequenceDiagram", "    participant U as User", "    participant D as Daemon", "    U->>D: run task", "    D->>D: plan", "    D-->>U: done", "```", "",
  "```mermaid", "pie title Outcomes", "    \"ok\" : 42", "    \"failed\" : 7", "```",
].join("\n");
async function mockChat(body, res) {
  if (body.tool_choice === "required") {
    res.writeHead(200, { "content-type": "application/json" });
    return res.end(JSON.stringify({ choices: [{ message: { tool_calls: [{ id: "p", type: "function", function: { name: "probe_ping", arguments: "{}" } }] } }] }));
  }
  if (!body.stream) {
    res.writeHead(200, { "content-type": "application/json" });
    // Non-streaming calls are optimiser/compaction work; a proposal-shaped
    // reply lets the Proposals view be exercised end to end.
    const proposal = { kind: "prompt", content: "Reuse an existing Tool before creating one; pass task values through input.", rationale: "mock: two Tools overlapped" };
    return res.end(JSON.stringify({ choices: [{ message: { content: JSON.stringify(proposal) } }] }));
  }
  const msgs = body.messages || [];
  let lastUser = msgs.length - 1;
  while (lastUser >= 0 && msgs[lastUser].role !== "user") lastUser--;
  const toolResults = msgs.slice(lastUser + 1).filter((m) => m.role === "tool");
  const haveTool = (body.tools || []).some((t) => (t.function || {}).name === HOME_TOOL.name);
  const called = (name) => msgs.slice(lastUser + 1).some((m) => (m.tool_calls || []).some((c) => (c.function || {}).name === name));
  res.writeHead(200, { "content-type": "text/event-stream" });
  const send = (delta, finish) => res.write("data: " + JSON.stringify({ choices: [{ delta, finish_reason: finish || null }] }) + "\n\n");
  const stream = async (field, text) => { for (const w of text.match(/\S+\s*/g) || []) { send({ [field]: w }); await sleep(45); } };
  const call = async (name, argsObj) => {
    send({ tool_calls: [{ index: 0, id: "call_" + Date.now(), type: "function", function: { name, arguments: JSON.stringify(argsObj) } }] }, "tool_calls");
  };
  const asked = String((msgs[lastUser] || {}).content || "");
  if (/markdown demo/i.test(asked)) {
    await stream("reasoning_content", "Showing every block the frontends render.");
    await stream("content", MARKDOWN_DEMO);
  } else if (!haveTool && !called("create_tool")) {
    await stream("reasoning_content", "No Tool reads the home directory yet. I will create a small one that only needs env:HOME, then call it.");
    await stream("content", "I'll build a small Tool for that.");
    await call("create_tool", HOME_TOOL);
  } else if (!called(HOME_TOOL.name)) {
    await stream("reasoning_content", "The Tool exists now; calling it.");
    await call(HOME_TOOL.name, {});
  } else {
    const result = String(toolResults[toolResults.length - 1].content || "");
    await stream("reasoning_content", "Summarising the result for the user.");
    await stream("content", "## Done\n\nYour home directory came back from **home_dir** (approved for `env:HOME` only).\n\n- Tool: `home_dir` v1\n- Capability: `env:HOME`\n\n```json\n" + result + "\n```\n\nAsk again and I will reuse the Tool without rebuilding it.");
  }
  res.end("data: [DONE]\n\n");
}

// ---------------------------------------------------------------- server
const server = createServer(async (req, res) => {
  const url = new URL(req.url, "http://localhost");
  if (mock && url.pathname.startsWith("/mock/v1/")) {
    if (url.pathname.endsWith("/models")) {
      res.writeHead(200, { "content-type": "application/json" });
      return res.end(JSON.stringify({ data: [{ id: "mock-1" }] }));
    }
    let raw = "";
    for await (const chunk of req) raw += chunk;
    return mockChat(JSON.parse(raw || "{}"), res);
  }
  if (url.pathname === "/health") {
    res.writeHead(200, { "content-type": "application/json" });
    return res.end(JSON.stringify({ ok: true, service: "formless-relay-dev", version: 2 }));
  }
  const rel = url.pathname === "/" ? "index.html" : normalize(url.pathname).replace(/^[/\\]+/, "");
  const file = join(webRoot, rel);
  if (!file.startsWith(webRoot)) { res.writeHead(404); return res.end("not found"); }
  try {
    const data = await readFile(file);
    res.writeHead(200, { "content-type": TYPES[extname(file)] || "application/octet-stream", ...SECURITY_HEADERS });
    res.end(data);
  } catch (_) {
    res.writeHead(404);
    res.end("not found");
  }
});
server.on("upgrade", (req, socket) => {
  const m = new URL(req.url, "http://localhost").pathname.match(/^\/relay\/([a-f0-9]{32})$/);
  if (!m) return socket.destroy();
  const params = new URL(req.url, "http://localhost").searchParams;
  const role = params.get("role") === "daemon" ? "daemon" : "client";
  const r = rooms.get(m[1]);
  if (role === "daemon" && r && r.daemon) {
    socket.end("HTTP/1.1 409 Conflict\r\n\r\n");
    return;
  }
  joinRoom(m[1], role, wsAccept(req, socket), () => socket.destroy(), params.get("mux") === "1");
});
server.listen(port, "127.0.0.1", () => {
  console.log("formless dev relay on http://127.0.0.1:" + port + (mock ? " (mock provider at /mock/v1)" : ""));
});
