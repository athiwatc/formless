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
import { extname, join, normalize, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { DevStorage } from "./dev-storage.mjs";
import { httpBoundary, upgradeBoundary } from "./dev-errors.mjs";
import { LIMITS } from "./web/transport.mjs";
import { verifyDaemonClaim } from "./daemon-claim.mjs";
import worker, { RelayRoom, SECURITY_HEADERS } from "./worker.js";

// Plain ws:// to this machine is allowed here only: production pages may
// connect to their own origin alone (#907).
const DEV_CSP = SECURITY_HEADERS["content-security-policy"]
  .replace("connect-src 'self'", "connect-src 'self' ws://127.0.0.1:* ws://localhost:*");

const args = process.argv.slice(2);
const portIndex = args.indexOf("--port");
const port = portIndex < 0 ? 8787 : Number(args[portIndex + 1]);
if (!Number.isInteger(port) || port < 0 || port > 65535) throw new Error("invalid port");
const mock = args.includes("--mock-provider");
const webRoot = fileURLToPath(new URL("./web/", import.meta.url));
const TYPES = { ".html": "text/html; charset=utf-8", ".js": "text/javascript; charset=utf-8", ".mjs": "text/javascript; charset=utf-8", ".css": "text/css; charset=utf-8", ".png": "image/png", ".webmanifest": "application/manifest+json" };

// The Worker's environment: the process's (VAPID keys, PUSH_HOSTS), a
// push service on 127.0.0.1 allowed for the tests, and the static assets.
const env = {
  ...process.env,
  ALLOW_LOCAL_PUSH: "1",
  ASSETS: {
    async fetch(request) {
      const path = new URL(request.url).pathname;
      let decoded;
      try {
        decoded = decodeURIComponent(path);
      } catch (error) {
        if (error instanceof URIError) return new Response("bad request", { status: 400 });
        throw error;
      }
      const rel = path === "/" ? "index.html" : normalize(decoded).replace(/^[/\\]+/, "");
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
/** Parse at most one bounded native frame's retained bytes, even if a TCP
 * read coalesces many messages. The application still carries 32MiB logically
 * through 64KiB opaque fragments; native WS messages are not that envelope. */
const NATIVE_CLOSE_GRACE_MS=1000;
function wsAccept(req,socket){
 const accept=createHash("sha1").update(req.headers["sec-websocket-key"]+"258EAFA5-E914-47DA-95CA-C5AB0DC85B11").digest("base64");
 socket.write("HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: "+accept+"\r\n\r\n");
 const conn={socket,open:true,closed:false,maxMessage:LIMITS.preReady,nativeControlBytes:0,nativeRate:[Date.now(),0],closeTimer:null,onText:()=>{},onClose:()=>{}};
 let buf=Buffer.alloc(0),parts=[],pending=0,fragmented=false;
 const parse=()=>{
  for(;;){
   if(buf.length<2)return;
   const fin=!!(buf[0]&128),op=buf[0]&15,masked=!!(buf[1]&128);
   if((buf[0]&112)||!masked)return closeWith(conn,1002,"invalid native frame");
   let len=buf[1]&127,off=2;
   if(len===126){if(buf.length<4)return;len=buf.readUInt16BE(2);off=4;}
   else if(len===127){if(buf.length<10)return;const big=buf.readBigUInt64BE(2);if(big>BigInt(conn.maxMessage))return closeWith(conn,1009,"frame too big");len=Number(big);off=10;}
   if(len>conn.maxMessage||(op<8&&pending+len>conn.maxMessage))return closeWith(conn,1009,"message too big");
   if(op>=8&&(!fin||len>125))return closeWith(conn,1002,"invalid control frame");
   const maskOff=off;off+=4;if(buf.length<off+len)return;
   if(Date.now()-conn.nativeRate[0]>=1000)conn.nativeRate=[Date.now(),0];
   if(++conn.nativeRate[1]>8192)return closeWith(conn,1008,"native frame rate exceeded");
   const payload=Buffer.from(buf.subarray(off,off+len));for(let i=0;i<len;i++)payload[i]^=buf[maskOff+i%4];buf=buf.subarray(off+len);
   if(op===8){sendFrame(conn,8,payload.subarray(0,2));return close(conn);}
   if(op===9){sendFrame(conn,10,payload);continue;}if(op===10)continue;
   if(op!==1&&op!==0)return closeWith(conn,1003,"text frames required");
   if((op===0&&!fragmented)||(op===1&&fragmented))return closeWith(conn,1002,"invalid continuation");
   parts.push(payload);pending+=len;if(parts.length>128)return closeWith(conn,1009,"too many native fragments");
   if(fin){let text;try{text=new TextDecoder('utf-8',{fatal:true}).decode(Buffer.concat(parts,pending));}catch(_){return closeWith(conn,1007,"invalid text encoding");}parts=[];pending=0;fragmented=false;conn.onText(text);}else fragmented=true;
   if(!conn.open)return;
  }
 };
 socket.on("data",chunk=>{
  let offset=0;
  while(conn.open&&offset<chunk.length){
   const take=Math.min(chunk.length-offset,LIMITS.wire+14-buf.length);
   if(take<=0)return closeWith(conn,1009,"native buffer limit");
   buf=Buffer.concat([buf,chunk.subarray(offset,offset+take)]);offset+=take;parse();
  }
 });
 socket.on("end",()=>close(conn));
 socket.on("error",()=>close(conn));
 socket.on("close",()=>{if(conn.closed)return;if(conn.closeTimer)clearTimeout(conn.closeTimer);conn.closeTimer=null;conn.closed=true;conn.open=false;buf=Buffer.alloc(0);parts=[];conn.onClose();});
 return conn;
}
function sendFrame(conn,op,payload){
 if(!conn.open)return;const len=payload.length;
 if(len>LIMITS.wire)return closeWith(conn,1009,"outbound wire limit");
 const head=len<126?Buffer.from([128|op,len]):len<65536?Buffer.from([128|op,126,len>>8,len&255]):Buffer.concat([Buffer.from([128|op,127]),(()=>{const b=Buffer.alloc(8);b.writeBigUInt64BE(BigInt(len));return b;})()]);
 const nativeControl=op>=8?head.length+payload.length:0;
 if(nativeControl&&conn.nativeControlBytes+nativeControl>4096){conn.open=false;conn.socket.destroy();return;}
 conn.nativeControlBytes+=nativeControl;
 conn.socket.write(Buffer.concat([head,payload]),()=>{conn.nativeControlBytes-=nativeControl;});
}
function closeWith(conn,code,reason){if(!conn.open)return;sendFrame(conn,8,Buffer.concat([Buffer.from([code>>8,code&255]),Buffer.from(reason,'utf8')]));close(conn);}
function close(conn){if(!conn.open)return;conn.open=false;try{conn.socket.end();}catch(_){}
 // A hostile half-open peer cannot retain a local native socket forever.
 // Destruction merely requests eviction; only actual 'close' proves teardown.
 conn.closeTimer=setTimeout(()=>conn.socket.destroy(),NATIVE_CLOSE_GRACE_MS);conn.closeTimer.unref();
}

// ----------------------------------------------------------------- rooms
/** The Durable Object state RelayRoom uses: tagged sockets and storage. */
class DevState {
 constructor(){this.webSockets=[];this.storage=new DevStorage(()=>this.room?.alarm());this.data=this.storage.data;}
 acceptWebSocket(ws,tags){ws.tags=tags;this.webSockets.push(ws);}
 getWebSockets(tag){return this.webSockets.filter(ws=>ws.readyState!==3&&(!tag||ws.tags.includes(tag)));}
 waitUntil(promise){promise.catch(()=>console.error("relay: background storage failed"));}
}

/** A Node connection as the WebSocket a Durable Object hands RelayRoom. */
function roomSocket(conn) {
  let att = null;
  return {
    get readyState() { return conn.closed ? 3 : conn.open ? 1 : 2; },
    send(text) { sendFrame(conn, 0x1, Buffer.from(text, "utf8")); },
    close(code, reason) { closeWith(conn, code || 1000, reason || ""); },
    setMessageLimit(bytes) { conn.maxMessage=bytes; },
    serializeAttachment(a) { att = structuredClone(a); },
    deserializeAttachment() { return att; },
  };
}

const rooms = new Map(); // room id -> { state, room }
function joinRoom(id, role, mux, ua, req, socket) {
  let r = rooms.get(id);
  if (!r) {
    const state = new DevState();
    r = { state, room: new RelayRoom(state, env) };state.room=r.room;
    rooms.set(id, r);
  }
  if(!r.room.canAdmit(role)){
    const timer=setTimeout(()=>socket.destroy(),NATIVE_CLOSE_GRACE_MS);timer.unref();socket.once("close",()=>clearTimeout(timer));
    socket.end("HTTP/1.1 429 Too Many Requests\r\nConnection: close\r\nContent-Length: 0\r\n\r\n",()=>socket.destroy());return;
  }
  const conn = wsAccept(req, socket);
  const ws = roomSocket(conn);
  try { r.room.admit(ws, role, mux, ua, role === "daemon"); } catch (_) {
    if(r.state.webSockets.includes(ws))ws.close(1008,"room capacity exhausted");
    else{conn.open=false;socket.destroy();} // Never retain an untracked 101 socket.
  }
  conn.onText = (text) => r.room.webSocketMessage(ws, text).catch((e) => console.log("relay: " + e.message));
  conn.onClose = () => {
    r.state.webSockets = r.state.webSockets.filter((w) => w !== ws);
    r.room.webSocketClose(ws);
    // An empty room with nothing stored is forgotten.
    if (!r.state.webSockets.length && !r.state.data.size) { r.state.storage.close();rooms.delete(id); }
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
async function handleHttp(req, res) {
  const url = new URL(req.url, "http://localhost");
  if (mock && url.pathname.startsWith("/mock/v1/")) {
    if (url.pathname.endsWith("/models")) {
      res.writeHead(200, { "content-type": "application/json" });
      return res.end(JSON.stringify({ data: [{ id: "mock-1" }] }));
    }
    let raw = "";
    for await (const chunk of req) raw += chunk;
    let body;
    try {
      body = JSON.parse(raw || "{}");
    } catch (error) {
      if (!(error instanceof SyntaxError)) throw error;
      return mockBadRequest(res, "Invalid JSON request body", "invalid_json");
    }
    if (body === null || typeof body !== "object" || Array.isArray(body)) {
      return mockBadRequest(res, "Request body must be a JSON object", "invalid_request");
    }
    // Keep asynchronous mock failures within the awaited request handler.
    return await mockChat(body, res);
  }
  // Everything else is the Worker's: /health, /push/key and the assets.
  const response = await worker.fetch(new Request("http://localhost" + req.url, { method: req.method, headers: req.headers }), env);
  const headers = Object.fromEntries(response.headers);
  if (headers["content-security-policy"]) headers["content-security-policy"] = DEV_CSP;
  res.writeHead(response.status, headers);
  res.end(Buffer.from(await response.arrayBuffer()));
}
function mockBadRequest(res, message, code) {
  res.writeHead(400, { "content-type": "application/json; charset=utf-8" });
  res.end(JSON.stringify({ error: { message, type: "invalid_request_error", param: null, code } }));
}
async function handleUpgrade(req, socket) {
  const url = new URL(req.url, "http://localhost");
  const m = url.pathname.match(/^\/relay\/([a-f0-9]{32})$/);
  if (!m) return socket.destroy();
  const role = url.searchParams.get("role") === "daemon" ? "daemon" : "client";
  if (role === "daemon" && !await verifyDaemonClaim(m[1], req.headers.authorization)) {
    socket.end("HTTP/1.1 401 Unauthorized\r\nConnection: close\r\nContent-Length: 0\r\n\r\n");
    return;
  }
  if (url.searchParams.get("transport")!=="1"||(role==="daemon"&&url.searchParams.get("mux")!=="1")) {socket.end("HTTP/1.1 426 Upgrade Required\r\nConnection: close\r\nContent-Length: 0\r\n\r\n");return;}
  if (socket.destroyed) return;
  joinRoom(m[1], role, url.searchParams.get("mux") === "1", req.headers["user-agent"] || "", req, socket);
}
export const server = createServer((req, res) => { void httpBoundary(req, res, handleHttp); });
server.on("upgrade", (req, socket) => { void upgradeBoundary(req, socket, handleUpgrade); });
if(process.argv[1]&&resolve(process.argv[1])===fileURLToPath(import.meta.url))server.listen(port, "127.0.0.1", () => {
  console.log("formless dev relay on http://127.0.0.1:" + server.address().port + (mock ? " (mock provider at /mock/v1)" : ""));
});
