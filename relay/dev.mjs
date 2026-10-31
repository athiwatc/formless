// Local stand-in for the Cloudflare Worker, for developing the Web frontend
// without deploying. It runs the Worker's own code (#836): every relay room
// is worker.js's RelayRoom over a small in-memory stand-in for the Durable
// Object, and every HTTP request goes through worker.js's fetch handler
// (/health, /push/key, the static web/ assets with their security
// headers). Only the Node shell lives here: the WebSocket framing, the
// static file reader and the mock provider. No dependencies.
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
import { createHash } from "node:crypto";
import { readFile } from "node:fs/promises";
import { extname, join, normalize } from "node:path";
import { fileURLToPath } from "node:url";
import worker, { RelayRoom, SECURITY_HEADERS } from "./worker.js";

// Plain ws:// to this machine is allowed here only: production pages may
// connect to their own origin alone (#907).
const DEV_CSP = SECURITY_HEADERS["content-security-policy"]
  .replace("connect-src 'self'", "connect-src 'self' ws://127.0.0.1:* ws://localhost:*");

const args = process.argv.slice(2);
const port = Number(args[args.indexOf("--port") + 1]) || 8787;
const mock = args.includes("--mock-provider");
const webRoot = fileURLToPath(new URL("./web/", import.meta.url));
const TYPES = { ".html": "text/html; charset=utf-8", ".js": "text/javascript; charset=utf-8", ".css": "text/css; charset=utf-8", ".png": "image/png", ".webmanifest": "application/manifest+json" };

// The Worker's environment: the process's (VAPID keys, PUSH_HOSTS), a
// push service on 127.0.0.1 allowed for the tests, and the static assets.
const env = {
  ...process.env,
  ALLOW_LOCAL_PUSH: "1",
  ASSETS: {
    async fetch(request) {
      const path = new URL(request.url).pathname;
      const rel = path === "/" ? "index.html" : normalize(decodeURIComponent(path)).replace(/^[/\\]+/, "");
      const file = join(webRoot, rel);
      if (!file.startsWith(webRoot)) return new Response("not found", { status: 404 });
      try {
        return new Response(await readFile(file), { headers: { "content-type": TYPES[extname(file)] || "application/octet-stream" } });
      } catch (_) {
        return new Response("not found", { status: 404 });
      }
    },
  },
};

// ------------------------------------------------------------ websockets
/** Biggest message accepted from a peer. */
const MAX_MESSAGE = 64 * 1024 * 1024;

function wsAccept(req, socket) {
  const accept = createHash("sha1").update(req.headers["sec-websocket-key"] + "258EAFA5-E914-47DA-95CA-C5AB0DC85B11").digest("base64");
  socket.write("HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: " + accept + "\r\n\r\n");
  const conn = { socket, open: true, onText: () => {}, onClose: () => {} };
  let buf = Buffer.alloc(0);
  let fragments = [];
  let pending = 0;
  socket.on("data", (chunk) => {
    buf = Buffer.concat([buf, chunk]);
    for (;;) {
      if (buf.length < 2) return;
      const fin = buf[0] & 0x80, op = buf[0] & 0x0f, masked = buf[1] & 0x80;
      // RFC 6455: a client's frames are masked; anything else is a
      // protocol error.
      if (!masked) return closeWith(conn, 1002, "unmasked frame");
      let len = buf[1] & 0x7f, off = 2;
      if (len === 126) { if (buf.length < 4) return; len = buf.readUInt16BE(2); off = 4; }
      else if (len === 127) {
        if (buf.length < 10) return;
        const big = buf.readBigUInt64BE(2);
        if (big > BigInt(MAX_MESSAGE)) return closeWith(conn, 1009, "message too big");
        len = Number(big); off = 10;
      }
      if (pending + len > MAX_MESSAGE) return closeWith(conn, 1009, "message too big");
      const maskOff = off;
      off += 4;
      if (buf.length < off + len) return;
      const payload = Buffer.from(buf.subarray(off, off + len));
      for (let i = 0; i < len; i++) payload[i] ^= buf[maskOff + (i % 4)];
      buf = buf.subarray(off + len);
      // A close is answered with a close before the socket ends.
      if (op === 0x8) { sendFrame(conn, 0x8, payload.subarray(0, 2)); return close(conn); }
      if (op === 0x9) { sendFrame(conn, 0xa, payload); continue; }
      if (op === 0x1 || op === 0x0) {
        fragments.push(payload);
        pending += len;
        if (fin) { const text = Buffer.concat(fragments).toString("utf8"); fragments = []; pending = 0; conn.onText(text); }
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
function closeWith(conn, code, reason) {
  const body = Buffer.concat([Buffer.from([code >> 8, code & 255]), Buffer.from(reason, "utf8")]);
  sendFrame(conn, 0x8, body);
  close(conn);
}
function close(conn) {
  if (!conn.open) return;
  conn.open = false;
  try { conn.socket.end(); } catch (_) {}
  conn.onClose();
}

// ----------------------------------------------------------------- rooms
/** The Durable Object state RelayRoom uses: tagged sockets and storage. */
class DevState {
  constructor() {
    this.webSockets = [];
    this.data = new Map();
    const data = this.data;
    this.storage = {
      async list({ prefix }) { return new Map([...data].filter(([k]) => k.startsWith(prefix))); },
      async put(k, v) { data.set(k, v); },
      async delete(k) { data.delete(k); },
    };
  }
  acceptWebSocket(ws, tags) { ws.tags = tags; this.webSockets.push(ws); }
  getWebSockets(tag) { return this.webSockets.filter((ws) => !tag || ws.tags.includes(tag)); }
}

/** A Node connection as the WebSocket a Durable Object hands RelayRoom. */
function roomSocket(conn) {
  let att = null;
  return {
    get readyState() { return conn.open ? 1 : 3; },
    send(text) { sendFrame(conn, 0x1, Buffer.from(text, "utf8")); },
    close(code, reason) { closeWith(conn, code || 1000, reason || ""); },
    serializeAttachment(a) { att = a; },
    deserializeAttachment() { return att; },
  };
}

const rooms = new Map(); // room id -> { state, room }
function joinRoom(id, role, mux, ua, req, socket) {
  let r = rooms.get(id);
  if (!r) {
    const state = new DevState();
    r = { state, room: new RelayRoom(state, env) };
    rooms.set(id, r);
  }
  const refused = r.room.refuse(role);
  if (refused) {
    socket.end("HTTP/1.1 " + refused[0] + " Conflict\r\n\r\n");
    return;
  }
  const conn = wsAccept(req, socket);
  const ws = roomSocket(conn);
  r.room.admit(ws, role, mux, ua);
  conn.onText = (text) => r.room.webSocketMessage(ws, text).catch((e) => console.log("relay: " + e.message));
  conn.onClose = () => {
    r.state.webSockets = r.state.webSockets.filter((w) => w !== ws);
    r.room.webSocketClose(ws);
    // An empty room with nothing stored is forgotten.
    if (!r.state.webSockets.length && !r.state.data.size) rooms.delete(id);
  };
  console.log("relay: " + role + " joined " + id.slice(0, 8) + "… (" + r.state.getWebSockets("client").length + " client(s))");
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
  // Everything else is the Worker's: /health, /push/key and the assets.
  const response = await worker.fetch(new Request("http://localhost" + req.url, { method: req.method, headers: req.headers }), env);
  const headers = Object.fromEntries(response.headers);
  if (headers["content-security-policy"]) headers["content-security-policy"] = DEV_CSP;
  res.writeHead(response.status, headers);
  res.end(Buffer.from(await response.arrayBuffer()));
});
server.on("upgrade", (req, socket) => {
  const url = new URL(req.url, "http://localhost");
  const m = url.pathname.match(/^\/relay\/([a-f0-9]{32})$/);
  if (!m) return socket.destroy();
  const role = url.searchParams.get("role") === "daemon" ? "daemon" : "client";
  joinRoom(m[1], role, url.searchParams.get("mux") === "1", req.headers["user-agent"] || "", req, socket);
});
server.listen(port, "127.0.0.1", () => {
  console.log("formless dev relay on http://127.0.0.1:" + port + (mock ? " (mock provider at /mock/v1)" : ""));
});
