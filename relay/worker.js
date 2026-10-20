// Formless relay (SPEC §12, v2): a Cloudflare Worker + Durable Object room.
// The daemon dials out to /relay/<room>?role=daemon and browsers to
// ?role=client. Frames between them are AES-256-GCM ciphertext envelopes;
// this worker only forwards opaque text and can never read session data.
// The Web frontend lives in web/ and is served as static assets with the
// headers below (the page URL carries the key, so no inline script runs).
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

/** Security headers for every frontend response (also used by dev.mjs). */
export const SECURITY_HEADERS = {
  "content-security-policy":
    "default-src 'none'; script-src 'self'; style-src 'self'; img-src 'self' data:; " +
    "connect-src 'self' wss: ws:; base-uri 'none'; form-action 'none'; frame-ancestors 'none'",
  "x-content-type-options": "nosniff",
  "referrer-policy": "no-referrer",
  "cache-control": "no-cache",
};

export default {
  async fetch(request, env) {
    const url = new URL(request.url);
    if (url.pathname === "/health") {
      return Response.json({ ok: true, service: "formless-relay", version: 2 });
    }
    const m = url.pathname.match(/^\/relay\/([a-f0-9]{32})$/);
    if (m) {
      const stub = env.ROOMS.get(env.ROOMS.idFromName(m[1]));
      return stub.fetch(request);
    }
    const asset = await env.ASSETS.fetch(request);
    const res = new Response(asset.body, asset);
    for (const [k, v] of Object.entries(SECURITY_HEADERS)) res.headers.set(k, v);
    return res;
  },
};
