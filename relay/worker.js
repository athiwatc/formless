// Formless relay (SPEC §17 v2): a Cloudflare Worker + Durable Object room.
// The daemon dials out to /relay/<room>?role=daemon and browsers to
// ?role=client. Frames between them are AES-256-GCM ciphertext envelopes;
// this worker only forwards opaque text and can never read session data.
// The room id is a truncated SHA-256 of the pairing key, so knowing a
// room does not reveal the key. Deployed at formless.athiwat.xyz.

export class RelayRoom {
  constructor(state, env) {
    this.state = state;
    this.env = env;
  }

  sockets(role) {
    return this.state.getWebSockets(role).filter((ws) => ws.readyState === 1);
  }

  notifyClients(kind) {
    const frame = JSON.stringify({ relay: kind });
    for (const ws of this.sockets("client")) {
      try { ws.send(frame); } catch (_) {}
    }
  }

  async fetch(request) {
    if (request.headers.get("Upgrade") !== "websocket") {
      return new Response("expected websocket", { status: 426 });
    }
    const url = new URL(request.url);
    const role = url.searchParams.get("role") === "daemon" ? "daemon" : "client";
    if (role === "daemon" && this.sockets("daemon").length > 0) {
      return new Response("a daemon is already connected to this room", { status: 409 });
    }
    const pair = new WebSocketPair();
    const client = pair[0];
    const server = pair[1];
    this.state.acceptWebSocket(server, [role]);
    server.serializeAttachment({ role: role });
    if (role === "daemon") this.notifyClients("daemon_online");
    if (role === "client") {
      const online = this.sockets("daemon").length > 0;
      try { server.send(JSON.stringify({ relay: online ? "daemon_online" : "daemon_offline" })); } catch (_) {}
    }
    return new Response(null, { status: 101, webSocket: client });
  }

  async webSocketMessage(ws, message) {
    if (typeof message !== "string") return;
    let role = "client";
    try { role = (ws.deserializeAttachment() || {}).role || "client"; } catch (_) {}
    const targets = this.sockets(role === "daemon" ? "client" : "daemon");
    for (const target of targets) {
      try { target.send(message); } catch (_) {}
    }
  }

  async webSocketClose(ws) {
    let role = "client";
    try { role = (ws.deserializeAttachment() || {}).role || "client"; } catch (_) {}
    if (role === "daemon") this.notifyClients("daemon_offline");
  }

  async webSocketError(ws) {
    try { ws.close(1011, "error"); } catch (_) {}
  }
}

export default {
  async fetch(request, env) {
    const url = new URL(request.url);
    if (url.pathname === "/health") {
      return Response.json({ ok: true, service: "formless-relay", version: 1 });
    }
    const m = url.pathname.match(/^\/relay\/([a-f0-9]{32})$/);
    if (m) {
      const stub = env.ROOMS.get(env.ROOMS.idFromName(m[1]));
      return stub.fetch(request);
    }
    if (url.pathname === "/" || url.pathname === "/index.html") {
      return new Response(INDEX_HTML, {
        headers: {
          "content-type": "text/html; charset=utf-8",
          "cache-control": "no-cache",
          "content-security-policy": "default-src 'self'; script-src 'unsafe-inline'; style-src 'unsafe-inline'; connect-src 'self' wss:",
          "x-content-type-options": "nosniff",
          "referrer-policy": "no-referrer",
        },
      });
    }
    return new Response("not found", { status: 404 });
  },
};

const INDEX_HTML = `<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>Formless</title>
<style>
  :root { color-scheme: dark; --bg:#101212; --panel:#171a1a; --line:#2a2f2e; --text:#e8ecea; --dim:#9aa5a0; --accent:#57c785; --warn:#e0b341; --err:#e0604f; --think:#c58ad9; }
  * { box-sizing: border-box; }
  body { margin:0; font:15px/1.5 ui-sans-system, -apple-system, "Segoe UI", sans-serif; background:var(--bg); color:var(--text); }
  header { display:flex; gap:12px; align-items:center; padding:10px 16px; border-bottom:1px solid var(--line); }
  header b { font-size:17px; letter-spacing:.2px; }
  #status { color:var(--dim); font-size:13px; }
  #status .on { color:var(--accent); } #status .off { color:var(--err); }
  main { display:grid; grid-template-columns:240px 1fr; height:calc(100dvh - 49px); }
  aside { border-right:1px solid var(--line); padding:10px; overflow:auto; background:var(--panel); }
  aside button, .session { width:100%; text-align:left; background:none; border:1px solid transparent; color:var(--text); padding:7px 9px; border-radius:6px; cursor:pointer; font:inherit; font-size:14px; }
  .session:hover, aside button:hover { border-color:var(--line); }
  .session.active { background:#1e2a24; border-color:#2c4638; }
  #chatwrap { display:flex; flex-direction:column; min-width:0; }
  #chat { flex:1; overflow:auto; padding:16px 20px; }
  .msg { margin:0 0 14px; white-space:pre-wrap; word-break:break-word; }
  .who { font-weight:700; font-size:13px; margin-bottom:2px; }
  .who.user { color:#62b6d9; } .who.assistant { color:var(--accent); } .who.tool { color:var(--warn); }
  .who.system, .who.system-note { color:#7d9cc4; }
  .toolcall { color:var(--warn); font-family:ui-monospace, Menlo, monospace; font-size:13px; white-space:pre-wrap; word-break:break-all; margin:2px 0 8px; }
  #thinking { color:var(--think); font-style:italic; opacity:.85; white-space:pre-wrap; margin:0 0 14px; display:none; }
  #activity { color:var(--warn); font-size:13px; padding:0 20px 6px; min-height:20px; }
  form { display:flex; gap:8px; padding:10px 16px 14px; border-top:1px solid var(--line); }
  textarea { flex:1; resize:none; height:44px; padding:10px; border-radius:8px; border:1px solid var(--line); background:#0c0e0e; color:var(--text); font:inherit; }
  button.send { background:var(--accent); color:#08130c; border:0; border-radius:8px; padding:0 18px; font-weight:700; cursor:pointer; }
  button.stop { background:#2a2f2e; color:var(--text); border:1px solid var(--line); border-radius:8px; padding:0 12px; cursor:pointer; }
  #pair { max-width:520px; margin:12vh auto; padding:24px; border:1px solid var(--line); border-radius:10px; background:var(--panel); }
  #pair input { width:100%; padding:10px; margin:10px 0; border-radius:8px; border:1px solid var(--line); background:#0c0e0e; color:var(--text); font:inherit; }
  #approval { display:none; margin:0 16px 10px; padding:12px; border:1px solid var(--err); border-radius:8px; }
  #approval button { margin-right:8px; padding:6px 14px; border-radius:6px; cursor:pointer; font:inherit; }
  .hidden { display:none !important; }
  @media (max-width:720px) { main { grid-template-columns:1fr; } aside { display:none; } }
</style>
</head>
<body>
<header><b>Formless</b><span id="status">not connected</span><span style="flex:1"></span><button class="stop" id="disconnect" type="button">Disconnect</button></header>
<div id="pair">
  <h2 style="margin-top:0">Connect to your daemon</h2>
  <p style="color:var(--dim)">Paste the pairing secret from <code>formless relay status</code>, or open the full browser link it prints. Traffic is end-to-end encrypted; this relay only forwards ciphertext.</p>
  <input id="secret" type="password" placeholder="Pairing secret" autocomplete="off">
  <button class="send" id="connect" style="height:42px" type="button">Connect</button>
  <p id="pairerr" style="color:var(--err)"></p>
</div>
<main id="app" class="hidden">
  <aside>
    <button id="newsession" type="button">+ New session</button>
    <div id="sessions"></div>
  </aside>
  <section id="chatwrap">
    <div id="chat"></div>
    <div id="thinking"></div>
    <div id="activity"></div>
    <div id="approval"></div>
    <form id="composer">
      <textarea id="input" placeholder="Type a task, Enter to send"></textarea>
      <button class="stop" id="stopbtn" type="button">Stop</button>
      <button class="send" type="submit">Send</button>
    </form>
  </section>
</main>

<script>
"use strict";
var ws = null, cryptoKey = null, current = null, sessions = [], messages = [];
var streaming = "", thinkingText = "", running = false, approval = null;
var nextId = Math.floor(Math.random() * 1000000000) + 1;
var pending = {};
var el = function (id) { return document.getElementById(id); };
function b64urlToBytes(s) {
  s = s.trim().replace(/-/g, "+").replace(/_/g, "/");
  while (s.length % 4) s += "=";
  var bin = atob(s); var out = new Uint8Array(bin.length);
  for (var i = 0; i < bin.length; i++) out[i] = bin.charCodeAt(i);
  return out;
}
function bytesToB64(bytes) {
  var bin = ""; var arr = new Uint8Array(bytes);
  for (var i = 0; i < arr.length; i += 8192) bin += String.fromCharCode.apply(null, arr.subarray(i, i + 8192));
  return btoa(bin);
}
function hex(bytes) { return Array.from(new Uint8Array(bytes)).map(function (b) { return b.toString(16).padStart(2, "0"); }).join(""); }
async function roomFor(keyBytes) { var h = await crypto.subtle.digest("SHA-256", keyBytes); return hex(h).slice(0, 32); }
async function encryptLine(text) {
  var iv = crypto.getRandomValues(new Uint8Array(12));
  var ct = await crypto.subtle.encrypt({ name: "AES-GCM", iv: iv }, cryptoKey, new TextEncoder().encode(text));
  return JSON.stringify({ v: 1, iv: bytesToB64(iv), ct: bytesToB64(ct) });
}
async function decryptLine(frame) {
  var v = JSON.parse(frame);
  var pt = await crypto.subtle.decrypt({ name: "AES-GCM", iv: b64urlToBytes(v.iv.replace(/\+/g, "-").replace(/\//g, "_").replace(/=+$/, "")) }, cryptoKey, b64urlToBytes(v.ct.replace(/\+/g, "-").replace(/\//g, "_").replace(/=+$/, "")));
  return new TextDecoder().decode(pt);
}
function setStatus(html) { el("status").innerHTML = html; }
function setActivity(text) { el("activity").textContent = text || ""; }
function request(req) {
  return new Promise(async function (resolve, reject) {
    var id = nextId++;
    pending[id] = { resolve: resolve, reject: reject };
    var line = JSON.stringify({ id: id, protocol: 1, request: req });
    ws.send(await encryptLine(line));
  });
}
async function fire(req) {
  var id = nextId++;
  ws.send(await encryptLine(JSON.stringify({ id: id, protocol: 1, request: req })));
}
