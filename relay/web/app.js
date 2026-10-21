// Formless Web frontend (SPEC §12, v2). Talks the normal daemon protocol
// through the encrypted relay: every line is AES-256-GCM encrypted here
// under a per-connection session key derived (HKDF) from the pairing
// secret, which never leaves this browser, and both sides' handshake
// nonces. Frames carry a sequence number bound into the authenticated
// data, so the relay cannot replay or reflect them. Feature parity
// with the TUI: Sessions (new/fork/rename/delete/search/import/export),
// streaming chat with thinking + Tool calls, steer/queue/stop/edit,
// approvals and promotions, Tools (detail, edit code, optimise, retire,
// rollback, purge, import/export), Metrics, Proposals, and Settings.
//
// Untrusted text (messages, Tool names, code) is only ever inserted as DOM
// text nodes, never as HTML.
"use strict";

(function () {
  const PROTOCOL = 1;
  const $ = (id) => document.getElementById(id);

  /** Tiny DOM builder: h("div", {class, onclick, ...attrs}, ...children). */
  function h(tag, props, ...kids) {
    const node = document.createElement(tag);
    for (const [k, v] of Object.entries(props || {})) {
      if (v == null || v === false) continue;
      if (k === "class") node.className = v;
      else if (k === "text") node.textContent = v;
      // CSSOM, not a style attribute: the CSP has no 'unsafe-inline'.
      else if (k === "style") node.style.cssText = v;
      else if (k.startsWith("on")) node.addEventListener(k.slice(2), v);
      else node.setAttribute(k, v === true ? "" : String(v));
    }
    for (const kid of kids.flat(Infinity)) {
      if (kid == null || kid === false) continue;
      node.append(kid instanceof Node ? kid : String(kid));
    }
    return node;
  }

  const prefs = {
    get(k, d) {
      try {
        const v = localStorage.getItem("formless." + k);
        return v === null ? d : JSON.parse(v);
      } catch (_) {
        return d;
      }
    },
    set(k, v) {
      try { localStorage.setItem("formless." + k, JSON.stringify(v)); } catch (_) {}
    },
  };

  // ---------------------------------------------------------------- state
  const S = {
    ws: null,
    key: null, // HKDF root key from the pairing secret
    hello: null, // this connection's handshake nonce
    channel: null, // { key, sent, received } once the handshake is done
    sendChain: Promise.resolve(),
    recvChain: Promise.resolve(),
    secret: "",
    userClosed: false,
    backoff: 1000,
    daemonOnline: false,
    view: prefs.get("view", "chat"),
    sessions: [],
    current: null,
    messages: [],
    streaming: "",
    thinking: "",
    running: new Set(),
    activity: null, // { label, since }
    liveTools: [],
    approvals: [],
    tools: [],
    modules: [],
    selectedTool: null,
    toolDetail: null,
    editingCode: null, // { toolId, code }
    metricTool: null,
    modelStats: [],
    proposals: [],
    settings: null,
    status: null,
    notes: [],
    unread: 0,
    showThinking: prefs.get("thinking", true),
    follow: true,
    editMessageId: null,
    nextId: Math.floor(Math.random() * 1e9) + 1,
    pending: new Map(),
    subagents: [], // last /agents listing
  };

  // --------------------------------------------------------------- crypto
  function b64ToBytes(s) {
    s = s.trim().replace(/-/g, "+").replace(/_/g, "/");
    while (s.length % 4) s += "=";
    const bin = atob(s);
    const out = new Uint8Array(bin.length);
    for (let i = 0; i < bin.length; i++) out[i] = bin.charCodeAt(i);
    return out;
  }
  function bytesToB64(bytes) {
    const arr = new Uint8Array(bytes);
    let bin = "";
    for (let i = 0; i < arr.length; i += 8192) bin += String.fromCharCode.apply(null, arr.subarray(i, i + 8192));
    return btoa(bin);
  }
  async function roomFor(keyBytes) {
    const digest = new Uint8Array(await crypto.subtle.digest("SHA-256", keyBytes));
    return Array.from(digest.slice(0, 16), (b) => b.toString(16).padStart(2, "0")).join("");
  }
  // Relay frame protocol v2 (must match crates/formless-core/src/relay.rs).
  const FRAME_VERSION = 2;
  const enc = (s) => new TextEncoder().encode(s);
  const aad = (direction, seq) => enc("formless-relay/2 " + direction + " " + seq);
  function startHandshake() {
    S.channel = null;
    S.hello = crypto.getRandomValues(new Uint8Array(16));
    S.ws.send(JSON.stringify({ v: FRAME_VERSION, hello: bytesToB64(S.hello) }));
  }
  async function finishHandshake(daemonHello) {
    const salt = new Uint8Array(32);
    salt.set(S.hello, 0);
    salt.set(daemonHello, 16);
    const key = await crypto.subtle.deriveKey(
      { name: "HKDF", hash: "SHA-256", salt, info: enc("formless-relay/2") },
      S.key, { name: "AES-GCM", length: 256 }, false, ["encrypt", "decrypt"]);
    S.channel = { key, sent: 0, received: 0 };
  }
  async function encrypt(text) {
    const ch = S.channel;
    const seq = ++ch.sent;
    const iv = crypto.getRandomValues(new Uint8Array(12));
    const ct = await crypto.subtle.encrypt({ name: "AES-GCM", iv, additionalData: aad("b2d", seq) }, ch.key, enc(text));
    return JSON.stringify({ v: FRAME_VERSION, seq, iv: bytesToB64(iv), ct: bytesToB64(ct) });
  }
  async function decrypt(frame) {
    const ch = S.channel;
    const v = JSON.parse(frame);
    if (!ch || v.v !== FRAME_VERSION || !(v.seq > ch.received)) throw new Error("rejected frame");
    const pt = await crypto.subtle.decrypt(
      { name: "AES-GCM", iv: b64ToBytes(v.iv), additionalData: aad("d2b", v.seq) }, ch.key, b64ToBytes(v.ct));
    ch.received = v.seq;
    return new TextDecoder().decode(pt);
  }

  // ------------------------------------------------------------ transport
  /** Request with a response (rejects on daemon error or timeout). */
  function request(req, timeoutMs) {
    return new Promise((resolve, reject) => {
      if (!S.ws || S.ws.readyState !== 1) return reject(new Error("not connected to the relay"));
      if (!S.channel) return reject(new Error("still connecting to the daemon"));
      const id = S.nextId++;
      const timer = setTimeout(() => {
        S.pending.delete(id);
        reject(new Error("the daemon did not answer (is it online?)"));
      }, timeoutMs || 30000);
      S.pending.set(id, {
        resolve: (v) => { clearTimeout(timer); resolve(v); },
        reject: (e) => { clearTimeout(timer); reject(e); },
      });
      // Serialised so frames leave in sequence-number order.
      const ws = S.ws;
      S.sendChain = S.sendChain
        .then(() => {
          const text = JSON.stringify({ id, protocol: PROTOCOL, request: req });
          return S.local ? text : encrypt(text);
        })
        .then((frame) => ws.send(frame))
        .catch((e) => { S.pending.delete(id); clearTimeout(timer); reject(e); });
    });
  }
  /** Request that reports failures as a toast; resolves null on error. */
  async function act(req, okText, timeoutMs) {
    try {
      const r = await request(req, timeoutMs);
      if (okText) toast(okText);
      return r;
    } catch (e) {
      toast(e.message, true);
      note(e.message, true);
      return null;
    }
  }

  // -------------------------------------------------------------- helpers
  function relTime(ts) {
    const t = Date.parse(ts);
    if (isNaN(t)) return ts || "";
    const s = Math.max(0, (Date.now() - t) / 1000);
    if (s < 60) return "just now";
    if (s < 3600) return Math.floor(s / 60) + "m ago";
    if (s < 86400) return Math.floor(s / 3600) + "h ago";
    if (s < 604800) return Math.floor(s / 86400) + "d ago";
    return new Date(t).toLocaleDateString();
  }
  function clock(ts) {
    const t = Date.parse(ts);
    return isNaN(t) ? "" : new Date(t).toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" });
  }
  function elapsed(since) {
    const s = Math.floor((Date.now() - since) / 1000);
    return s < 60 ? s + "s" : Math.floor(s / 60) + "m" + String(s % 60).padStart(2, "0") + "s";
  }
  const pct = (r) => (r == null ? "-" : Math.round(r * 100) + "%");
  // Same thresholds as the TUI: green at the optimiser trigger (90%).
  const rateClass = (r) => (r == null ? "muted" : r >= 0.9 ? "rate-good" : r >= 0.7 ? "rate-mid" : "rate-bad");
  const truncate = (s, n) => {
    s = String(s || "").replace(/\s+/g, " ").trim();
    return s.length > n ? s.slice(0, n - 1) + "…" : s;
  };
  function rateCell(r) {
    return h("span", { class: rateClass(r) }, pct(r));
  }
  function bar(r) {
    return h("span", { class: "bar " + rateClass(r) }, h("i", { style: "width:" + Math.round((r || 0) * 100) + "%" }));
  }
  function summarizeArgs(args) {
    let obj;
    try { obj = JSON.parse(args); } catch (_) { return truncate(args, 160); }
    if (!obj || typeof obj !== "object" || Array.isArray(obj)) return truncate(args, 160);
    const keys = Object.keys(obj).sort((a, b) => (a === "name" ? -1 : b === "name" ? 1 : 0));
    return truncate(keys.map((k) => {
      const v = obj[k];
      if (typeof v === "string") return k + '="' + truncate(v, 40) + '"';
      if (Array.isArray(v)) return k + "=[" + v.length + "]";
      if (v && typeof v === "object") return k + "={…}";
      return k + "=" + v;
    }).join("  "), 200);
  }
  /** <pre> for a Tool payload: JSON is pretty-printed and highlighted. */
  function payload(text) {
    try {
      const p = JSON.stringify(JSON.parse(text), null, 2);
      return h("pre", null, highlight(p, "json"));
    } catch (_) {
      return h("pre", null, text);
    }
  }
  function isBroad(cap) { return cap === "net" || cap === "process"; }
  function capTags(caps) {
    if (!caps || !caps.length) return h("span", { class: "muted" }, "none");
    return caps.map((c) => h("span", { class: "tag" + (isBroad(c) ? " broad" : ""), title: isBroad(c) ? "unscoped — broad access" : null }, c));
  }
  function download(name, value) {
    downloadBlob(name, new Blob([JSON.stringify(value, null, 2)], { type: "application/json" }));
  }
  function downloadBlob(name, blob) {
    const a = h("a", { href: URL.createObjectURL(blob), download: name });
    document.body.append(a);
    a.click();
    a.remove();
    setTimeout(() => URL.revokeObjectURL(a.href), 2000);
  }
  function pickJson() {
    return new Promise((resolve) => {
      const input = $("file-input");
      input.value = "";
      input.onchange = async () => {
        const file = input.files && input.files[0];
        if (!file) return resolve(null);
        try { resolve(JSON.parse(await file.text())); } catch (_) { toast("That file is not valid JSON.", true); resolve(null); }
      };
      input.click();
    });
  }
  const slug = (s) => String(s || "export").toLowerCase().replace(/[^a-z0-9]+/g, "-").replace(/^-|-$/g, "") || "export";

  // ------------------------------------------------------ notes + toasts
  function note(text, err) {
    S.notes.push({ text, err: !!err, at: Date.now() });
    if (S.notes.length > 200) S.notes.shift();
    if ($("notes-panel").classList.contains("hidden")) S.unread++;
    renderNotes();
    renderStatus();
  }
  function toast(text, err) {
    const t = h("div", { class: "toast" + (err ? " err" : ""), role: err ? "alert" : "status" }, text);
    $("toasts").append(t);
    setTimeout(() => t.remove(), err ? 6000 : 3500);
  }

  // -------------------------------------------------------- ask dialogs
  let askResolve = null;
  /** Promise-based prompt/confirm: resolves the text (input) or true, or null. */
  function ask(opts) {
    $("ask-title").textContent = opts.title;
    $("ask-text").textContent = opts.text || "";
    const input = $("ask-input");
    input.classList.toggle("hidden", !opts.input);
    input.value = opts.value || "";
    input.placeholder = opts.placeholder || "";
    const ok = $("ask-ok");
    ok.textContent = opts.ok || "OK";
    ok.className = "btn " + (opts.danger ? "danger" : "primary");
    $("ask-backdrop").classList.remove("hidden");
    setTimeout(() => (opts.input ? input.select() : ok.focus()), 0);
    return new Promise((resolve) => (askResolve = resolve));
  }
  function closeAsk(value) {
    $("ask-backdrop").classList.add("hidden");
    if (askResolve) askResolve(value);
    askResolve = null;
  }
  $("ask-form").addEventListener("submit", (e) => {
    e.preventDefault();
    closeAsk($("ask-input").classList.contains("hidden") ? true : $("ask-input").value);
  });
  $("ask-cancel").onclick = () => closeAsk(null);

  // ------------------------------------------------------------ activity
  function setActivity(label) {
    if (!label) S.activity = null;
    else if (!S.activity || S.activity.label !== label) S.activity = { label, since: Date.now() };
    renderStatus();
    renderLive();
  }
  const isRunning = () => S.current != null && S.running.has(S.current);

  // ============================================================ rendering
  function renderAll() {
    renderChrome();
    renderSessions();
    renderHistory();
    renderLive();
    renderComposer();
    renderView();
    renderStatus();
    renderApproval();
    renderNotes();
  }

  function renderChrome() {
    document.querySelectorAll("#tabs button").forEach((b) => {
      if (b.dataset.view === S.view) b.setAttribute("aria-current", "page");
      else b.removeAttribute("aria-current");
    });
    for (const v of ["chat", "tools", "metrics", "proposals", "settings"]) {
      $("view-" + v).classList.toggle("hidden", v !== S.view);
    }
    $("sidebar").classList.toggle("hidden", S.view !== "chat");
    $("menu-btn").classList.toggle("hidden", S.view !== "chat");
    const conn = $("conn");
    if (!S.ws || S.ws.readyState !== 1) {
      conn.className = "pill off";
      conn.textContent = "relay disconnected";
    } else if (!S.daemonOnline) {
      conn.className = "pill off";
      conn.textContent = "daemon offline";
    } else {
      conn.className = "pill on";
      conn.textContent = "● connected";
    }
  }

  function renderView() {
    if (S.view === "tools") renderTools();
    else if (S.view === "metrics") renderMetrics();
    else if (S.view === "proposals") renderProposals();
    else if (S.view === "settings") renderSettings();
  }

  function setView(view) {
    S.view = view;
    prefs.set("view", view);
    if (view === "tools") { refreshTools(); refreshModules(); }
    if (view === "metrics") { refreshTools(); refreshModelStats(); }
    if (view === "proposals") refreshProposals();
    if (view === "settings") refreshSettings();
    renderChrome();
    renderView();
    if (view === "chat") scrollToBottom(true);
  }

  // ------------------------------------------------------------- sidebar
  let popover = null;
  function closePopover() {
    if (popover) { popover.remove(); popover = null; }
    document.querySelectorAll(".menu[aria-expanded]").forEach((m) => m.removeAttribute("aria-expanded"));
  }
  function sessionMenu(btn, s) {
    closePopover();
    btn.setAttribute("aria-expanded", "true");
    const r = btn.getBoundingClientRect();
    popover = h("div", { class: "popover", role: "menu" },
      h("button", { type: "button", onclick: () => { closePopover(); renameSession(s); } }, "Rename"),
      h("button", { type: "button", onclick: () => { closePopover(); forkSession(s.id); } }, "Fork"),
      h("button", { type: "button", onclick: () => { closePopover(); exportSession(s.id); } }, "Export"),
      h("button", { type: "button", class: "danger", onclick: () => { closePopover(); deleteSession(s); } }, "Delete"));
    popover.style.top = Math.min(r.bottom + 4, innerHeight - 170) + "px";
    popover.style.left = Math.min(r.left, innerWidth - 170) + "px";
    document.body.append(popover);
    popover.querySelector("button").focus();
  }

  // Running Invocations across Sessions (SPEC §6.3): cancel or abandon.
  async function refreshRunning() {
    const list = (await request({ type: "list_invocations" }).catch(() => null)) || [];
    S.invocations = list;
    const box = $("running");
    box.classList.toggle("hidden", !list.length);
    box.replaceChildren(...(list.length ? [h("h4", null, "Running tools")] : []), ...list.map((i) => {
      const s = S.sessions.find((x) => x.id === i.session_id);
      return h("div", { class: "running-item" + (i.stalled ? " stalled" : "") },
        h("button", { class: "linklike", type: "button", title: "Open its session", onclick: () => openSession(i.session_id) },
          h("strong", null, i.tool_name), " · " + Math.round(i.elapsed_ms / 1000) + "s" + (s ? " · " + s.name : "") +
          (i.stalled ? " · silent " + Math.round(i.silent_ms / 1000) + "s" : "") + (i.abandoned ? " · abandoned" : "")),
        h("button", { class: "btn ghost small", type: "button", title: "Cancel (user-cancelled)", onclick: () => act({ type: "cancel_invocation", handle: i.handle }, "Cancelled " + i.tool_name).then(refreshRunning) }, "Cancel"),
        i.abandoned ? null : h("button", { class: "btn ghost small", type: "button", title: "Stop waiting for it", onclick: () => act({ type: "abandon_invocation", handle: i.handle }, "Abandoned " + i.tool_name).then(refreshRunning) }, "Abandon"));
    }));
  }
  setInterval(() => { if (S.channel && S.invocations && S.invocations.length) refreshRunning(); }, 2000);

  function renderSessions() {
    const list = $("sessions");
    list.replaceChildren();
    if (!S.sessions.length) {
      list.append(h("li", { class: "muted", style: "padding:8px" }, "No sessions yet."));
      return;
    }
    for (const s of S.sessions) {
      const menu = h("button", {
        class: "icon-btn menu", type: "button", "aria-label": "Session actions",
        onclick: (e) => { e.stopPropagation(); sessionMenu(menu, s); },
      }, "⋯");
      list.append(h("li", {
        class: "session" + (s.id === S.current ? " active" : ""),
        tabindex: 0,
        onclick: () => openSession(s.id),
        onkeydown: (e) => { if (e.key === "Enter") openSession(s.id); },
      },
      S.running.has(s.id) ? h("span", { class: "running", title: "running" }) : null,
      h("span", { class: "name", title: s.name }, s.name),
      h("span", { class: "when" }, relTime(s.updated_at)),
      menu));
    }
  }

  function renderSearch(hits, query) {
    const box = $("search-results");
    box.replaceChildren();
    if (hits == null) { box.classList.add("hidden"); return; }
    box.classList.remove("hidden");
    box.append(h("div", { class: "muted", style: "padding:4px 8px;font-size:12px" },
      hits.length + " hit" + (hits.length === 1 ? "" : "s") + " for “" + query + "”"));
    const lower = query.toLowerCase();
    for (const hit of hits) {
      const snippet = h("div");
      const text = hit.snippet || "";
      const at = text.toLowerCase().indexOf(lower);
      if (at >= 0 && lower) {
        snippet.append(text.slice(0, at), h("mark", null, text.slice(at, at + query.length)), text.slice(at + query.length));
      } else snippet.append(text);
      box.append(h("div", {
        class: "hit", tabindex: 0,
        onclick: () => openSession(hit.session_id),
        onkeydown: (e) => { if (e.key === "Enter") openSession(hit.session_id); },
      }, h("div", { class: "meta" }, hit.session_name + " · #" + hit.seq + " " + hit.role), snippet));
    }
  }

  // ---------------------------------------------------------- markdown
  // CommonMark-ish + GitHub extensions (tables, task lists, strikethrough,
  // alerts, autolinks), built as DOM nodes — never innerHTML — so model
  // output cannot inject markup. Mermaid diagrams render via a vendored,
  // lazily loaded mermaid.js into <img> data URLs (see renderMermaid).
  const SAFE_URL = /^(https?:|mailto:)/i;
  const INLINE = new RegExp([
    /(`+)([^`]|[^`][\s\S]*?[^`])\1(?!`)/.source, // 1,2 code
    /\*\*(?=\S)([\s\S]*?\S)\*\*/.source, // 3 strong
    /__(?=\S)([\s\S]*?\S)__(?!\w)/.source, // 4 strong
    /~~(?=\S)([\s\S]*?\S)~~/.source, // 5 strike
    /\*(?=[^\s*])((?:[^*]|\*\*[^*]+\*\*)*?[^\s*])\*/.source, // 6 em
    /(?<![\w\\])_(?=[^\s_])([^_]*?[^\s_])_(?!\w)/.source, // 7 em
    /!\[([^\]]*)\]\(([^)\s]+)(?:\s+"[^"]*")?\)/.source, // 8,9 image
    /\[((?:[^\]\\]|\\.)+)\]\(([^)\s]+)(?:\s+"[^"]*")?\)/.source, // 10,11 link
    /<((?:https?:\/\/|mailto:)[^>\s]+)>/.source, // 12 autolink
    /(https?:\/\/[^\s<]*[^\s<.,:;"')\]!?*_~])/.source, // 13 bare url
    /\$(?=\S)([^$\n]*?\S)\$(?!\d)/.source, // 14 math
    /\\([\\`*_{}\[\]()#+\-.!~|$<>])/.source, // 15 escape
  ].join("|"), "g");
  function link(url, kids) {
    if (!SAFE_URL.test(url)) return kids;
    return h("a", { href: url, target: "_blank", rel: "noopener noreferrer" }, kids);
  }
  function inline(text) {
    const out = [];
    const re = new RegExp(INLINE.source, "g");
    let last = 0;
    let m;
    while ((m = re.exec(text))) {
      if (m.index > last) out.push(text.slice(last, m.index));
      if (m[2] != null) out.push(h("code", null, m[1].length > 1 ? m[2].trim() : m[2]));
      else if (m[3] != null || m[4] != null) out.push(h("strong", null, inline(m[3] != null ? m[3] : m[4])));
      else if (m[5] != null) out.push(h("del", null, inline(m[5])));
      else if (m[6] != null || m[7] != null) out.push(h("em", null, inline(m[6] != null ? m[6] : m[7])));
      // Remote images are blocked by the CSP (img-src 'self' data:), so an
      // image is shown as a labelled link instead.
      else if (m[9] != null) out.push(link(m[9], h("span", { class: "img-link" }, "🖼 " + (m[8] || "image"))));
      else if (m[11] != null) out.push(link(m[11], inline(m[10])));
      else if (m[12] != null) out.push(link(m[12], m[12].replace(/^mailto:/i, "")));
      else if (m[13] != null) out.push(link(m[13], m[13]));
      else if (m[14] != null) out.push(h("span", { class: "math" }, m[14]));
      else if (m[15] != null) out.push(m[15]);
      last = re.lastIndex;
    }
    if (last < text.length) out.push(text.slice(last));
    return out;
  }

  // Tiny tokenizer-highlighter, mirroring the TUI's (crates/formless-tui/src/highlight.rs).
  const W = (s) => new Set(s.split(" "));
  const HL = {
    rust: { lc: ["//"], bc: ["/*", "*/"], q: "\"", kw: W("as async await break const continue crate dyn else enum extern fn for if impl in let loop match mod move mut pub ref return static struct trait type unsafe use where while"), lit: W("true false self Self None Some Ok Err super") },
    js: { lc: ["//"], bc: ["/*", "*/"], q: "\"'`", kw: W("as async await break case catch class const continue debugger default delete do else enum export extends finally for from function if implements import in instanceof interface let new of private protected public readonly return static switch throw try type typeof var void while with yield"), lit: W("true false null undefined NaN this super") },
    py: { lc: ["#"], q: "\"'", kw: W("and as assert async await break case class continue def del elif else except finally for from global if import in is lambda match nonlocal not or pass raise return try while with yield"), lit: W("True False None self cls") },
    go: { lc: ["//"], bc: ["/*", "*/"], q: "\"`'", kw: W("break case chan const continue default defer else fallthrough for func go goto if import interface map package range return select struct switch type var"), lit: W("true false nil iota") },
    sh: { lc: ["#"], q: "\"'", kw: W("alias case cd do done echo elif else end esac exit export fi for function if in local return set source then unset while"), lit: W("true false") },
    c: { lc: ["//"], bc: ["/*", "*/"], q: "\"'", kw: W("abstract auto break case catch class const continue default do else enum extends extern final for fun func goto if implements import include interface let namespace new override package private protected public return sizeof static struct switch template throw throws try typedef union using val var virtual void volatile while"), lit: W("true false null nullptr nil this self super") },
    json: { q: "\"", kw: W(""), lit: W("true false null") },
    conf: { lc: ["#", ";"], q: "\"'", kw: W(""), lit: W("true false null yes no on off"), key: true },
    sql: { lc: ["--"], bc: ["/*", "*/"], q: "'", ci: true, kw: W("add all alter and as asc begin by case commit create default delete desc distinct drop else end exists foreign from group having in index inner insert into is join key left like limit not offset on or order outer primary references returning right select set table then union update values when where with"), lit: W("null true false") },
    html: { bc: ["<!--", "-->"], q: "\"'", kw: W(""), lit: W(""), tags: true },
    css: { bc: ["/*", "*/"], q: "\"'", kw: W("important media import supports keyframes"), lit: W("") },
  };
  const HL_ALIAS = {
    rs: "rust", javascript: "js", jsx: "js", mjs: "js", cjs: "js", ts: "js", typescript: "js", tsx: "js",
    python: "py", python3: "py", golang: "go", bash: "sh", zsh: "sh", fish: "sh", shell: "sh", console: "sh", powershell: "sh", ps: "sh",
    h: "c", cpp: "c", "c++": "c", cc: "c", hpp: "c", java: "c", kotlin: "c", kt: "c", swift: "c", cs: "c", csharp: "c", scala: "c", dart: "c", php: "c", zig: "c",
    jsonc: "json", json5: "json", yaml: "conf", yml: "conf", toml: "conf", ini: "conf", env: "conf", dotenv: "conf", properties: "conf",
    sqlite: "sql", postgres: "sql", postgresql: "sql", mysql: "sql", xml: "html", svg: "html", vue: "html", svelte: "html", scss: "css", less: "css",
  };
  function highlight(code, lang) {
    lang = (lang || "").toLowerCase();
    if (lang === "diff" || lang === "patch") {
      return code.split("\n").flatMap((l, i) => {
        const cls = /^(\+\+\+|---)/.test(l) ? "tok-meta" : l[0] === "+" ? "tok-add" : l[0] === "-" ? "tok-del" : l.startsWith("@@") ? "tok-hunk" : null;
        return [i ? "\n" : "", cls ? h("span", { class: cls }, l) : l];
      });
    }
    const L = HL[HL_ALIAS[lang] || lang];
    if (!L) return null;
    const out = [];
    let plain = "";
    const tok = (cls, s) => {
      if (plain) { out.push(plain); plain = ""; }
      out.push(h("span", { class: cls }, s));
    };
    const word = /[\p{L}\p{N}_$]/u;
    let i = 0;
    let lineStart = true;
    while (i < code.length) {
      const c = code[i];
      if (lineStart && L.key) {
        const m = code.slice(i).match(/^([ \t]*)([\w.\-"'\[\] ]+?)(?=\s*[:=])/);
        if (m && m[2].trim() && !(L.lc || []).some((p) => m[2].trim().startsWith(p))) {
          plain += m[1];
          tok("tok-key", m[2]);
          i += m[0].length;
          lineStart = false;
          continue;
        }
      }
      lineStart = c === "\n";
      if (L.bc && code.startsWith(L.bc[0], i)) {
        const end = code.indexOf(L.bc[1], i + L.bc[0].length);
        const stop = end < 0 ? code.length : end + L.bc[1].length;
        tok("tok-com", code.slice(i, stop));
        i = stop;
        continue;
      }
      if ((L.lc || []).some((p) => code.startsWith(p, i)) && (c !== "#" || i === 0 || /\s/.test(code[i - 1]))) {
        const end = code.indexOf("\n", i);
        const stop = end < 0 ? code.length : end;
        tok("tok-com", code.slice(i, stop));
        i = stop;
        continue;
      }
      if (L.q.includes(c)) {
        let j = i + 1;
        while (j < code.length && code[j] !== c && (code[j] !== "\n" || c === "`")) j += code[j] === "\\" ? 2 : 1;
        const stop = Math.min(j + 1, code.length);
        tok("tok-str", code.slice(i, stop));
        i = stop;
        continue;
      }
      const prevWord = i > 0 && word.test(code[i - 1]);
      if (/\d/.test(c) && !prevWord) {
        const m = code.slice(i).match(/^[\d][\w.]*/);
        tok("tok-num", m[0]);
        i += m[0].length;
        continue;
      }
      if (L.tags && c === "<") {
        const m = code.slice(i).match(/^<\/?[\w:-]+/);
        if (m) { tok("tok-kw", m[0]); i += m[0].length; continue; }
      }
      if (/[\p{L}_$]/u.test(c) && !prevWord) {
        const m = code.slice(i).match(/^[\p{L}\p{N}_$]+/u);
        const w = m[0];
        const k = L.ci ? w.toLowerCase() : w;
        const cls = L.kw.has(k) ? "tok-kw" : L.lit.has(k) ? "tok-lit" : code[i + w.length] === "(" ? "tok-fn"
          : !L.ci && !L.tags && /^\p{Lu}/u.test(w) && /\p{Ll}/u.test(w) ? "tok-type" : null;
        if (cls) tok(cls, w); else plain += w;
        i += w.length;
        continue;
      }
      plain += c;
      i++;
    }
    if (plain) out.push(plain);
    return out;
  }

  function copyButton(text) {
    return h("button", {
      class: "btn ghost small copy", type: "button",
      onclick: () => navigator.clipboard.writeText(text).then(() => toast("Copied"), () => toast("Copy failed", true)),
    }, "Copy");
  }
  function codeBlock(code, lang) {
    const body = highlight(code, lang) || [code];
    return h("div", { class: "codeblock" }, lang ? h("div", { class: "lang" }, lang) : null, copyButton(code), h("pre", null, h("code", null, body)));
  }

  // Mermaid: loaded on first use, rendered one diagram at a time, cached by
  // source + theme. securityLevel "strict" sanitizes labels; the SVG is shown
  // through an <img> data URL, so it can neither run script nor touch the
  // page, and its own <style> applies without loosening the page CSP.
  // Mermaid measures text in a live off-screen copy whose inline styles the
  // CSP blocks (console noise only); .mermaid-measure supplies the same font
  // from app.css so the measured boxes match the final image.
  const MERMAID_SRC = "vendor/mermaid.min.js";
  const MERMAID_FONT = "ui-sans-serif, system-ui, -apple-system, \"Segoe UI\", sans-serif"; // = .mermaid-measure
  const mermaidCache = new Map();
  let mermaidReady = null;
  let mermaidQueue = Promise.resolve();
  let mermaidSeq = 0;
  const darkScheme = window.matchMedia("(prefers-color-scheme: dark)");
  function loadMermaid() {
    if (!mermaidReady) {
      mermaidReady = new Promise((resolve, reject) => {
        const s = h("script", { src: MERMAID_SRC });
        s.onload = () => (window.mermaid ? resolve(window.mermaid) : reject(new Error("mermaid missing")));
        s.onerror = () => { mermaidReady = null; reject(new Error("could not load " + MERMAID_SRC)); };
        document.head.append(s);
      });
    }
    return mermaidReady;
  }
  /** Pie slices in the app palette (mermaid's dark pie is near-black). */
  function pieColors() {
    const css = getComputedStyle(document.documentElement);
    const v = (n) => css.getPropertyValue("--" + n).trim();
    const vars = { pieStrokeColor: v("bg"), pieOuterStrokeColor: v("line"), pieSectionTextColor: v("bg"), pieTitleTextColor: v("text"), pieLegendTextColor: v("text") };
    ["accent", "user", "warn", "think", "err", "muted"].forEach((n, i) => { vars["pie" + (i + 1)] = v(n); });
    return vars;
  }
  function renderMermaid(src) {
    const theme = darkScheme.matches ? "dark" : "default";
    const key = theme + "\n" + src;
    if (!mermaidCache.has(key)) {
      const job = mermaidQueue.then(async () => {
        const mermaid = await loadMermaid();
        mermaid.initialize({
          startOnLoad: false, securityLevel: "strict", theme, htmlLabels: false,
          flowchart: { htmlLabels: false }, fontFamily: MERMAID_FONT, fontSize: 16,
          themeVariables: pieColors(),
        });
        if (!(await mermaid.parse(src, { suppressErrors: true }))) throw new Error("not a valid Mermaid diagram");
        const id = "mmd-" + ++mermaidSeq;
        let measure = document.querySelector(".mermaid-measure");
        if (!measure) {
          measure = h("div", { class: "mermaid-measure", "aria-hidden": "true" });
          document.body.append(measure);
        }
        try {
          const { svg } = await mermaid.render(id, src, measure);
          const vb = svg.match(/viewBox="[-\d.]+ [-\d.]+ ([\d.]+) ([\d.]+)"/);
          return { url: "data:image/svg+xml;charset=utf-8," + encodeURIComponent(svg), svg, width: vb ? Math.ceil(+vb[1]) : 0 };
        } finally {
          measure.replaceChildren();
        }
      });
      mermaidQueue = job.catch(() => {});
      mermaidCache.set(key, job);
      if (mermaidCache.size > 200) mermaidCache.delete(mermaidCache.keys().next().value);
    }
    return mermaidCache.get(key);
  }
  function diagramBlock(src, closed) {
    const code = h("pre", { class: "diagram-src" }, h("code", null, src));
    const view = h("div", { class: "diagram-view" }, h("span", { class: "muted" }, closed ? "Rendering diagram…" : "Waiting for the diagram to finish…"));
    const box = h("div", { class: "codeblock diagram" });
    const toggle = h("button", { class: "btn ghost small", type: "button", onclick: () => setSource(!box.classList.contains("show-src")) }, "Source");
    const save = h("button", { class: "btn ghost small", type: "button", hidden: true }, "SVG");
    const setSource = (on) => { box.classList.toggle("show-src", on); toggle.textContent = on ? "Diagram" : "Source"; };
    box.append(h("div", { class: "lang" }, h("span", null, "mermaid"), h("span", { class: "spacer" }), toggle, save, copyButton(src)), view, code);
    if (!closed) { setSource(true); return box; }
    renderMermaid(src).then((d) => {
      const img = h("img", { src: d.url, alt: "Mermaid diagram", class: "diagram-img", title: "Click to zoom" });
      if (d.width) img.style.width = d.width + "px";
      img.addEventListener("click", () => box.classList.toggle("zoom"));
      img.addEventListener("load", () => scrollToBottom(), { once: true });
      view.replaceChildren(img);
      save.hidden = false;
      save.onclick = () => downloadBlob("diagram.svg", new Blob([d.svg], { type: "image/svg+xml" }));
    }, (e) => {
      view.replaceChildren(h("span", { class: "error" }, "Diagram not rendered: " + (e && e.message ? e.message : e)));
      setSource(true);
    });
    return box;
  }

  const FENCE = /^(\s*)(`{3,}|~{3,})\s*([^\s`{,]*)/;
  const HEADING = /^\s{0,3}(#{1,6})\s+(.*?)(?:\s+#+\s*)?$/;
  const RULE = /^\s{0,3}([-*_])(?:\s*\1){2,}\s*$/;
  const ITEM = /^(\s*)([-*+]|\d{1,9}[.)])(\s+|$)(.*)$/;
  const QUOTE = /^\s{0,3}>/;
  const TABLE_SEP = /^\s*\|?\s*:?-+:?\s*(\|\s*:?-+:?\s*)*\|?\s*$/;
  const ALERTS = { NOTE: "ℹ Note", TIP: "★ Tip", IMPORTANT: "‼ Important", WARNING: "⚠ Warning", CAUTION: "✖ Caution" };
  const indentOf = (l) => l.match(/^\s*/)[0].replace(/\t/g, "    ").length;
  const startsBlock = (lines, i) => {
    const l = lines[i];
    return FENCE.test(l) || HEADING.test(l) || RULE.test(l) || ITEM.test(l) || QUOTE.test(l) || /^\s*\$\$/.test(l)
      || (l.includes("|") && i + 1 < lines.length && TABLE_SEP.test(lines[i + 1]) && lines[i + 1].includes("-"));
  };
  function splitRow(line) {
    let s = line.trim();
    if (s.startsWith("|")) s = s.slice(1);
    if (s.endsWith("|") && !s.endsWith("\\|")) s = s.slice(0, -1);
    return s.split(/(?<!\\)\|/).map((c) => c.trim().replace(/\\\|/g, "|"));
  }

  /** Append the block structure of `lines` to `root`. */
  function blocks(lines, root) {
    let i = 0;
    while (i < lines.length) {
      const line = lines[i];
      if (!line.trim()) { i++; continue; }
      const fence = line.match(FENCE);
      if (fence) {
        const marker = fence[2];
        const lead = fence[1].length;
        const buf = [];
        i++;
        while (i < lines.length && !new RegExp("^\\s*" + marker[0] + "{" + marker.length + ",}\\s*$").test(lines[i])) {
          buf.push(lines[i].slice(Math.min(lead, indentOf(lines[i]))));
          i++;
        }
        const closed = i < lines.length;
        i++;
        const lang = fence[3];
        root.append(lang.toLowerCase() === "mermaid" ? diagramBlock(buf.join("\n"), closed) : codeBlock(buf.join("\n"), lang));
        continue;
      }
      if (/^\s*\$\$/.test(line)) {
        const buf = [line.replace(/^\s*\$\$/, "")];
        i++;
        while (i < lines.length && !buf[buf.length - 1].includes("$$")) buf.push(lines[i++]);
        root.append(h("pre", { class: "math block" }, buf.join("\n").replace(/\$\$\s*$/, "").trim()));
        continue;
      }
      const head = line.match(HEADING);
      if (head) { root.append(h("h" + Math.min(6, head[1].length + 1), null, inline(head[2]))); i++; continue; }
      if (RULE.test(line)) { root.append(h("hr")); i++; continue; }
      if (line.includes("|") && i + 1 < lines.length && TABLE_SEP.test(lines[i + 1]) && lines[i + 1].includes("-")) {
        const header = splitRow(line);
        const aligns = splitRow(lines[i + 1]).map((c) => (/^:-+:$/.test(c) ? "center" : /-:$/.test(c) ? "right" : null));
        const cell = (tag, text, k) => h(tag, { class: aligns[k] ? "al-" + aligns[k] : null }, inline(text));
        const tbody = h("tbody");
        i += 2;
        while (i < lines.length && lines[i].trim() && lines[i].includes("|")) {
          const cells = splitRow(lines[i++]);
          tbody.append(h("tr", null, header.map((_, k) => cell("td", cells[k] || "", k))));
        }
        root.append(h("div", { class: "md-table" }, h("table", null, h("thead", null, h("tr", null, header.map((c, k) => cell("th", c, k)))), tbody)));
        continue;
      }
      if (QUOTE.test(line)) {
        const buf = [];
        while (i < lines.length && (QUOTE.test(lines[i]) || (lines[i].trim() && buf.length && !startsBlock(lines, i)))) {
          buf.push(lines[i++].replace(/^\s{0,3}>\s?/, ""));
        }
        const alert = buf[0] && buf[0].trim().match(/^\[!(NOTE|TIP|IMPORTANT|WARNING|CAUTION)\]$/i);
        const q = h("blockquote", { class: alert ? "callout " + alert[1].toLowerCase() : null });
        if (alert) { buf.shift(); q.append(h("div", { class: "callout-title" }, ALERTS[alert[1].toUpperCase()])); }
        blocks(buf, q);
        root.append(q);
        continue;
      }
      if (ITEM.test(line)) { i = list(lines, i, root); continue; }
      const p = h("p");
      p.append(...inline(line.trim()));
      i++;
      while (i < lines.length && lines[i].trim() && !startsBlock(lines, i)) { p.append(h("br"), ...inline(lines[i++].trim())); }
      root.append(p);
    }
    return root;
  }

  /** A (possibly nested) list starting at lines[i]; returns the next index. */
  function list(lines, i, root) {
    const first = lines[i].match(ITEM);
    const base = indentOf(first[1]);
    const ordered = /\d/.test(first[2]);
    const el = h(ordered ? "ol" : "ul");
    if (ordered && parseInt(first[2], 10) !== 1) el.setAttribute("start", String(parseInt(first[2], 10)));
    let loose = false;
    while (i < lines.length) {
      const m = lines[i].match(ITEM);
      if (!m || indentOf(m[1]) !== base || /\d/.test(m[2]) !== ordered) break;
      const contentIndent = base + m[2].length + Math.max(1, Math.min(4, m[3].length));
      const body = [m[4]];
      i++;
      while (i < lines.length) {
        const l = lines[i];
        if (!l.trim()) {
          let j = i + 1;
          while (j < lines.length && !lines[j].trim()) j++;
          if (j < lines.length && indentOf(lines[j]) > base) { body.push(""); loose = loose || !ITEM.test(lines[j]); i++; continue; }
          break;
        }
        const ind = indentOf(l);
        if (ind > base) { body.push(" ".repeat(Math.max(0, ind - contentIndent)) + l.trim()); i++; continue; }
        if (ITEM.test(l) || startsBlock(lines, i) || !body[body.length - 1].trim()) break;
        body.push(l.trim()); // lazy continuation
        i++;
      }
      const li = h("li");
      const task = body[0].match(/^\[([ xX])\]\s+(.*)$/);
      if (task) {
        body[0] = task[2];
        li.className = "task";
        li.append(h("input", { type: "checkbox", disabled: true, checked: task[1] !== " ", "aria-label": task[1] !== " " ? "done" : "not done" }));
      }
      blocks(body, li);
      el.append(li);
      // Blank line before a sibling item makes the list loose.
      if (i > 0 && !lines[i - 1].trim() && i < lines.length && ITEM.test(lines[i])) loose = true;
    }
    if (loose) el.classList.add("loose");
    root.append(el);
    return i;
  }

  function markdown(text) {
    return blocks(String(text || "").replace(/\r\n?/g, "\n").split("\n"), h("div", { class: "md bubble" }));
  }

  // ------------------------------------------------------------- chat
  const speaker = (role) => (role === "user" ? "user" : role === "system-note" ? "system-note" : "assistant");
  const WHO = { user: "You", assistant: "Formless", "system-note": "System" };

  function toolCallChip(call) {
    const f = call.function || {};
    return h("details", { class: "chip" },
      h("summary", null, h("span", { class: "tname" }, "⚙ " + (f.name || "tool")), h("span", { class: "targs" }, summarizeArgs(f.arguments || ""))),
      payload(f.arguments || ""));
  }
  function toolResultChip(content) {
    const failed = /"ok"\s*:\s*false/.test(content) || /^\s*error/i.test(content);
    const lines = content.split("\n").length;
    return h("details", { class: "chip result" + (failed ? " failed" : "") },
      h("summary", null,
        h("span", { class: "tname" }, failed ? "↳ failed" : "↳ result"),
        h("span", { class: "targs" }, truncate(content, 160)),
        lines > 1 ? h("span", { class: "tstat" }, lines + " lines") : null),
      payload(content));
  }

  // Rendered message bodies by message id: the turn layout is rebuilt on
  // every change, but Markdown, highlighting, Mermaid and tool chips are
  // built once per message (and keep their state, e.g. open <details>).
  const rendered = new Map();
  function messageBody(m, build) {
    let node = rendered.get(m.id);
    if (!node) {
      node = h("div", { class: "msg" });
      build(node);
      rendered.set(m.id, node);
    }
    return node;
  }

  function renderHistory() {
    const box = $("transcript");
    box.replaceChildren();
    if (!S.current) {
      box.append(welcome());
      return;
    }
    if (!S.daemonOnline) box.append(h("div", { class: "banner" }, "The daemon is offline. Start formless on your machine; this page reconnects automatically."));
    if (!S.messages.length && !isRunning()) { box.append(welcome()); box.append(h("div", { id: "live" })); return; }
    const lastUser = [...S.messages].reverse().find((m) => m.role === "user");
    let turn = null;
    let group = null;
    const open = (role, when, msg) => {
      const g = speaker(role);
      if (g === group && turn) return turn.body;
      group = g;
      const who = h("div", { class: "who " + g }, WHO[g], when ? h("time", null, clock(when)) : null);
      if (msg && msg === lastUser && !isRunning()) {
        who.append(h("button", { class: "btn ghost small edit", type: "button", title: "Edit and re-run (/edit)", onclick: () => beginEdit(msg) }, "Edit"));
      }
      const body = h("div");
      turn = { el: h("div", { class: "turn " + g }, who, body), body };
      box.append(turn.el);
      return body;
    };
    for (const m of S.messages) {
      if (m.role === "user") {
        // Each user message starts a fresh turn so its Edit button is exact.
        group = null;
        open("user", m.created_at, m).append(messageBody(m, (n) => n.append(h("div", { class: "bubble" }, m.content))));
      } else if (m.role === "tool") {
        open("assistant", m.created_at).append(messageBody(m, (n) => n.append(toolResultChip(m.content || ""))));
      } else if (m.role === "system-note") {
        open("system-note", m.created_at).append(messageBody(m, (n) => n.append(h("div", { class: "bubble" }, m.content))));
      } else {
        open("assistant", m.created_at).append(messageBody(m, (n) => {
          if (m.content) n.append(markdown(m.content));
          let calls = [];
          try { calls = (JSON.parse(m.meta || "{}").tool_calls) || []; } catch (_) {}
          calls.forEach((c) => n.append(toolCallChip(c)));
        }));
      }
    }
    box.append(h("div", { id: "live", class: "turn" }));
    renderLive();
    scrollToBottom();
  }

  function welcome() {
    const examples = [
      "Fetch the title of https://example.com",
      "Summarise the largest files in ~/Downloads",
      "Watch a log file and flag new errors",
    ];
    return h("div", { class: "welcome" },
      h("h2", null, h("span", { class: "mark" }, "◇ "), "Formless"),
      h("p", { class: "muted" }, "An agent that builds, measures, and improves its own Tools."),
      h("div", { class: "examples" }, examples.map((ex) => h("button", { type: "button", onclick: () => { $("input").value = ex; $("input").focus(); autosize(); } }, ex))),
      h("p", { class: "muted" }, "Enter sends · Ctrl/⌘+Enter queues · /help lists commands"));
  }

  function runningTool(name) {
    return [...S.liveTools].reverse().find((x) => x.name === name && x.status === "running");
  }
  function cancelTool(handle) {
    if (handle) act({ type: "cancel_invocation", handle }, "Cancelling the tool");
    else note("no pending or stalled tool to cancel (Esc stops the whole run)", true);
  }

  function renderLive() {
    const live = $("live");
    if (!live) return;
    live.replaceChildren();
    const running = isRunning();
    const tools = S.liveTools.filter((t) => t.status === "running");
    if (!running && !S.streaming && !tools.length) return;
    live.append(h("div", { class: "who assistant" }, "Formless"));
    if (S.showThinking && S.thinking) {
      // Latest lines only, like the TUI: reasoning never floods the view.
      const lines = S.thinking.split("\n");
      live.append(h("div", { class: "thinking" }, lines.length > 6 ? "… " + lines.slice(-6).join("\n") : S.thinking));
    }
    for (const t of tools) {
      live.append(h("details", { class: "chip live" },
        h("summary", null,
          h("span", { class: "spinner", style: "color:var(--warn)" }),
          h("span", { class: "tname" }, t.name + (t.version > 0 ? " v" + t.version : "")),
          h("span", { class: "targs" }, summarizeArgs(t.arguments || "")),
          t.stalledSecs != null ? h("span", { class: "tag broad" }, "stalled " + t.stalledSecs + "s") : null,
          t.handle ? h("button", { type: "button", class: "btn danger small", onclick: (e) => { e.preventDefault(); cancelTool(t.handle); } }, "Cancel") : null,
          h("span", { class: "tstat" }, elapsed(t.started))),
        payload(t.arguments || "")));
    }
    if (S.streaming) live.append(markdown(S.streaming));
    else if (running && S.activity) live.append(h("div", { class: "activity" }, h("span", { class: "spinner" }), S.activity.label + "… " + elapsed(S.activity.since)));
    scrollToBottom();
  }

  function scrollToBottom(force) {
    const box = $("transcript");
    if (force) S.follow = true;
    if (S.follow) {
      box.scrollTop = box.scrollHeight;
      $("jump").classList.add("hidden");
    } else if (isRunning() || S.streaming) $("jump").classList.remove("hidden");
  }
  $("transcript").addEventListener("scroll", () => {
    const box = $("transcript");
    S.follow = box.scrollHeight - box.scrollTop - box.clientHeight < 80;
    if (S.follow) $("jump").classList.add("hidden");
  });
  $("jump").onclick = () => scrollToBottom(true);

  // An interrupted/paused last Run (SPEC §5.4): why, and what to do.
  function renderRunBanner() {
    const box = $("run-banner");
    const u = S.unfinished;
    box.classList.toggle("hidden", !u || isRunning());
    if (!u) return box.replaceChildren();
    box.replaceChildren(
      h("span", null, "⏸ The last Run was " + u.status + (u.error ? ": " + truncate(u.error, 200) : "")),
      h("button", { class: "btn small primary", type: "button", onclick: async () => {
        if (await act({ type: "resume_run", session_id: S.current })) { S.unfinished = null; renderRunBanner(); }
      } }, "Resume"),
      h("button", { class: "btn small", type: "button", onclick: async () => {
        if (await act({ type: "abandon_run", session_id: S.current }, "Abandoned")) { S.unfinished = null; renderRunBanner(); }
      } }, "Abandon"));
  }

  function renderComposer() {
    const running = isRunning();
    $("composer").classList.toggle("running", running);
    $("stop-btn").hidden = !running;
    $("send-btn").textContent = S.editMessageId ? "Re-run" : running ? "Steer" : "Send";
    $("input").placeholder = running ? "Steer, or Ctrl/⌘+Enter to queue…" : "Message Formless…";
    const mode = $("compose-mode");
    mode.replaceChildren();
    if (S.editMessageId) {
      mode.classList.remove("hidden");
      mode.append("Editing your last message — sending re-runs from there.",
        h("button", { class: "btn ghost small", type: "button", onclick: cancelEdit }, "Cancel"));
    } else mode.classList.add("hidden");
    const s = S.sessions.find((x) => x.id === S.current);
    $("session-title").textContent = s ? s.name : "No session";
    $("show-thinking").checked = S.showThinking;
  }

  function renderStatus() {
    const state = $("state");
    state.replaceChildren();
    if (isRunning()) {
      state.className = "state busy";
      state.append(h("span", { class: "spinner" }), S.activity ? S.activity.label + "… " + elapsed(S.activity.since) : "running");
    } else {
      state.className = "state";
      state.append("● ready");
    }
    const s = S.sessions.find((x) => x.id === S.current);
    $("status-session").textContent = s ? s.name : "no session";
    $("status-model").textContent = (s && s.model) || (S.settings && S.settings.provider.model) || "";
    const u = S.usage && S.usage[S.current];
    $("status-usage").textContent = u && u.steps ? " │ " + usageText(u) : "";
    $("status-usage").title = S.sessionUsage && S.sessionUsage.id === S.current ? "Session (" + S.sessionUsage.runs + " Runs): " + usageText(S.sessionUsage.total) : "";
    const waiting = S.approvals.length;
    $("status-approvals").classList.toggle("hidden", !waiting);
    $("status-approvals").textContent = "⚠ " + waiting + " awaiting approval";
    const last = S.notes[S.notes.length - 1];
    const n = $("status-note");
    n.textContent = last ? last.text : "";
    n.className = "muted note" + (last && last.err ? " err" : "");
    const badge = $("notes-badge");
    badge.textContent = S.unread > 99 ? "99+" : String(S.unread);
    badge.classList.toggle("hidden", !S.unread);
  }

  function renderNotes() {
    const list = $("notes");
    list.replaceChildren();
    if (!S.notes.length) list.append(h("li", { class: "muted" }, h("time"), h("span", null, "Nothing yet.")));
    for (const n of [...S.notes].reverse()) {
      list.append(h("li", { class: n.err ? "err" : "" },
        h("time", null, new Date(n.at).toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" })),
        h("span", null, n.text)));
    }
  }

  // -------------------------------------------------------- approvals
  function renderApproval() {
    const req = S.approvals[0];
    $("modal-backdrop").classList.toggle("hidden", !req);
    if (!req) return;
    const promo = req.kind === "promotion";
    $("modal-title").textContent = promo ? "Promotion approval" : "Approval required";
    document.querySelector("#modal-backdrop .modal").classList.add("alert");
    const body = $("modal-body");
    body.replaceChildren(h("div", null,
      req.detail
        ? h("p", { class: "detail-text" }, req.detail)
        : h("p", null, "Tool ", h("strong", null, req.tool_name), req.version ? " v" + req.version : "",
          promo ? " is ready to be promoted." : " wants sensitive capabilities:"),
      promo && req.reason ? h("p", { class: "muted" }, req.reason) : null,
      h("ul", null,
        (req.capabilities.length ? req.capabilities : ["(no capabilities)"]).map((c) =>
          (req.already || []).includes(c)
            ? h("li", { class: "muted" }, h("span", { class: "tag" }, c), " already allowed")
            : h("li", null, h("span", { class: "tag" + (isBroad(c) ? " broad" : "") }, c), isBroad(c) ? h("span", { class: "error" }, " unscoped — broad access") : (req.already || []).length ? h("span", null, " new") : null)),
        req.detail ? null : h("li", null, h("span", { class: "tag" }, "memory " + req.memory_mb + " MB"))),
      req.code ? h("details", { class: "chip" }, h("summary", null, "Code (first lines)"), h("pre", { class: "diff" }, req.code)) : null,
      h("p", { class: "muted" }, promo
        ? "The new version replaces the current one; rollback stays available."
        : "Approve lasts for this Tool until its capabilities widen. Allow once covers this call only.")));
    $("modal-once").classList.toggle("hidden", promo);
    $("modal-approve").replaceChildren(h("kbd", null, "y"), promo ? " Approve" : " Always");
    $("modal-more").textContent = S.approvals.length > 1 ? "+" + (S.approvals.length - 1) + " more waiting" : "";
    setTimeout(() => $("modal-approve").focus(), 0);
  }
  function decide(approved, scope) {
    const req = S.approvals.shift();
    if (!req) return;
    if (req.kind === "promotion") act({ type: "promotion_decision", promotion_id: req.id, approved });
    else act({ type: "approval_decision", approval_id: req.id, approved, scope: scope || "always" });
    note((approved ? "approved " : "denied ") + (req.kind === "promotion" ? "promotion of " : "") + req.tool_name + (scope === "once" ? " once" : ""));
    if (!S.approvals.length && isRunning()) setActivity("waiting for model");
    renderApproval();
    renderStatus();
  }
  $("modal-approve").onclick = () => decide(true);
  $("modal-once").onclick = () => decide(true, "once");
  $("modal-deny").onclick = () => decide(false);

  // ------------------------------------------------------------- tools
  function toolStatusDot(t) {
    return h("span", { class: "dot" + (t.needs_attention ? " attention" : t.retired ? " retired" : ""), title: t.needs_attention ? "needs attention" : t.retired ? "retired" : "active" });
  }
  function toolsTable(table, tools, opts) {
    table.replaceChildren();
    const cols = ["Tool", "Ver", "Calls", "Success", opts.bars ? "" : "Capabilities"];
    table.append(h("thead", null, h("tr", null, cols.map((c, i) => h("th", { class: i === 2 || i === 3 ? "num" : null }, c)))));
    const body = h("tbody");
    if (!tools.length) body.append(h("tr", null, h("td", { class: "empty", colspan: 5 }, "No Tools yet — the agent builds them with create_tool.")));
    for (const t of tools) {
      body.append(h("tr", {
        class: opts.selected === t.id ? "selected" : null, tabindex: 0,
        onclick: () => opts.onselect(t.id),
        onkeydown: (e) => { if (e.key === "Enter") opts.onselect(t.id); },
      },
      h("td", null, toolStatusDot(t), t.name, t.retired ? h("span", { class: "muted" }, " (retired)") : null),
      h("td", null, "v" + t.current_version),
      h("td", { class: "num" }, t.invocations),
      h("td", { class: "num" }, rateCell(t.success_rate)),
      opts.bars ? h("td", null, bar(t.success_rate)) : h("td", { class: "caps" }, (t.capabilities || []).join(", ") || "—")));
    }
    table.append(body);
  }

  function renderTools() {
    $("tools-count").textContent = "(" + S.tools.length + ")";
    toolsTable($("tools-table"), S.tools, { selected: S.selectedTool, onselect: selectTool });
    const box = $("tool-detail");
    box.replaceChildren();
    const d = S.toolDetail;
    if (!d || d.tool.id !== S.selectedTool) {
      box.append(h("p", { class: "placeholder" }, S.tools.length ? "Select a Tool for its versions, metrics, optimiser history, and code." : "Tools appear here once the agent creates them."));
    } else {
      renderToolDetail(box, d);
    }
    if (S.modules.length) {
      box.append(h("h3", null, "Shared modules ", h("span", { class: "muted" }, "(requireModule)")));
      box.append(h("dl", { class: "kv" }, S.modules.map((m) => [h("dt", null, m.name + " v" + m.current_version), h("dd", null, m.description)])));
    }
  }

  function metricsBlock(m) {
    return h("dl", { class: "kv" },
      h("dt", null, "Success"), h("dd", null, bar(m.success_rate), " ", rateCell(m.success_rate), h("span", { class: "muted" }, "  retry " + pct(m.retry_rate))),
      h("dt", null, "Invocations"), h("dd", null, m.total + " total · " + m.success + " ok · " + m.failure + " failed · " + m.denied + " denied · " + m.cancelled + " cancelled · " + m.validation_failures + " invalid"),
      h("dt", null, "Latency"), h("dd", null, "p50 " + (m.p50_ms == null ? "-" : Math.round(m.p50_ms) + "ms") + " · p95 " + (m.p95_ms == null ? "-" : Math.round(m.p95_ms) + "ms")),
      h("dt", null, "Cost per success"), h("dd", null, m.ms_per_success == null ? "-" : (m.ms_per_success / 1000).toFixed(1) + "s · " + Math.round(m.tokens_per_success || 0) + " tokens"));
  }

  function renderToolDetail(box, d) {
    const t = d.tool;
    const current = d.versions.find((v) => v.version === t.current_version) || d.versions[d.versions.length - 1];
    box.append(h("h3", null, t.name), h("p", { class: "muted", style: "margin:0" }, t.description));
    box.append(h("dl", { class: "kv" },
      h("dt", null, "Status"), h("dd", null, h("span", { class: "status-pill " + (t.needs_attention ? "bad" : t.retired ? "" : "good") }, t.needs_attention ? "needs attention" : t.retired ? "retired" : "active")),
      h("dt", null, "Versions"), h("dd", null, h("div", { class: "versions" }, d.versions.map((v) =>
        v.version === t.current_version
          ? h("span", { class: "version current", title: "current" }, "v" + v.version + " " + v.author)
          : h("button", { class: "version", type: "button", title: "Roll back to v" + v.version, onclick: () => rollbackTool(t, v.version) }, "v" + v.version + " " + v.author)))),
      h("dt", null, "Capabilities"), h("dd", null, capTags(t.capabilities)),
      h("dt", null, "Memory"), h("dd", null, t.memory_mb + " MB"),
      h("dt", null, "Approved"), h("dd", null, d.approval
        ? h("div", null,
            d.approval.capabilities.map((c) => h("span", { class: "tag" }, c,
              h("button", { class: "btn ghost small", type: "button", title: "Revoke " + c, onclick: () => revokeApproval(t, c) }, "×"))),
            h("button", { class: "btn small", type: "button", onclick: () => revokeApproval(t, null) }, "Revoke all"))
        : h("span", { class: "muted" }, "nothing yet (asks on first sensitive call)"))));
    box.append(metricsBlock(d.metrics));
    const idx = d.versions.findIndex((v) => v.version === t.current_version);
    if (idx > 0) {
      // What the current version changed (optimiser promotion or agent edit).
      const from = d.versions[idx - 1].version;
      const pre = h("pre", { class: "diff" }, "loading…");
      box.append(h("details", { class: "chip" }, h("summary", null, "Changes v" + from + " → v" + t.current_version), pre));
      request({ type: "diff_tool_versions", id: t.id, from, to: t.current_version }).then((r) => {
        pre.replaceChildren(...String(r.diff).split("\n").map((l) =>
          h("div", { class: l[0] === "+" ? "add" : l[0] === "-" ? "del" : l[0] === "@" ? "muted" : null }, l)));
      }).catch((e) => { pre.textContent = String(e.message || e); });
    }
    const editing = S.editingCode && S.editingCode.toolId === t.id;
    box.append(h("div", { class: "detail-actions" },
      editing ? null : h("button", { class: "btn small", type: "button", onclick: () => { S.editingCode = { toolId: t.id, code: current ? current.code : "" }; renderTools(); } }, "Edit code"),
      h("button", { class: "btn small", type: "button", onclick: () => act({ type: "run_optimiser", tool_id: t.id }, "Optimiser started for " + t.name) }, "Optimise"),
      h("button", { class: "btn small", type: "button", onclick: () => act({ type: t.retired ? "restore_tool" : "retire_tool", id: t.id }, (t.retired ? "Restored " : "Retired ") + t.name) }, t.retired ? "Restore" : "Retire"),
      t.current_version > 1 ? h("button", { class: "btn small", type: "button", onclick: () => rollbackTool(t, t.current_version - 1) }, "Roll back") : null,
      h("button", { class: "btn small", type: "button", onclick: () => exportTool(t) }, "Export"),
      h("button", { class: "btn small danger", type: "button", onclick: () => purgeTool(t) }, "Purge")));
    if (d.optimiser_attempts && d.optimiser_attempts.length) {
      box.append(h("h3", null, "Optimiser"));
      for (const a of d.optimiser_attempts.slice(0, 8)) {
        const cls = a.status === "promoted" ? "good" : a.status === "discarded" || a.status === "failed" ? "bad" : "mid";
        box.append(h("div", { class: "attempt" },
          h("span", null, h("span", { class: "status-pill " + cls }, a.status)),
          h("span", null, "cand " + pct(a.candidate_score) + " vs base " + pct(a.baseline_score) + " · " + a.replay_total + " replays · ",
            h("span", { class: "muted" }, relTime(a.created_at) + (a.detail ? " · " + a.detail : "")))));
      }
    }
    if (editing) {
      box.append(h("h3", null, "Edit v" + t.current_version + " ", h("span", { class: "muted" }, "→ saves as a new version through the smoke gate")));
      const area = h("textarea", { class: "editor", spellcheck: "false", "aria-label": "Tool code" });
      area.value = S.editingCode.code;
      area.addEventListener("input", () => { S.editingCode.code = area.value; });
      area.addEventListener("keydown", (e) => {
        if (e.key === "Tab") {
          e.preventDefault();
          const p = area.selectionStart;
          area.setRangeText("  ", p, area.selectionEnd, "end");
          S.editingCode.code = area.value;
        } else if ((e.ctrlKey || e.metaKey) && e.key.toLowerCase() === "s") {
          e.preventDefault();
          saveCode(t);
        } else if (e.key === "Escape") {
          e.stopPropagation();
          S.editingCode = null;
          renderTools();
        }
      });
      box.append(area, h("div", { class: "detail-actions" },
        h("button", { class: "btn primary small", type: "button", onclick: () => saveCode(t) }, "Save new version"),
        h("button", { class: "btn ghost small", type: "button", onclick: () => { S.editingCode = null; renderTools(); } }, "Cancel"),
        h("span", { class: "muted", style: "font-size:12px" }, "Ctrl/⌘+S save · Esc cancel")));
      setTimeout(() => area.focus(), 0);
    } else if (current) {
      box.append(h("h3", null, "Code v" + current.version));
      const rows = current.code.split("\n").map((l, i) => h("tr", null, h("td", { class: "ln" }, i + 1), h("td", { class: "src" }, l)));
      box.append(h("div", { class: "code" }, h("table", null, h("tbody", null, rows))));
    }
  }

  function renderMetrics() {
    const tools = S.tools;
    const total = tools.reduce((a, t) => a + t.invocations, 0);
    const rated = tools.filter((t) => t.success_rate != null && t.invocations > 0);
    const weighted = rated.length ? rated.reduce((a, t) => a + t.success_rate * t.invocations, 0) / rated.reduce((a, t) => a + t.invocations, 0) : null;
    const card = (label, value, cls) => h("div", { class: "card" }, h("div", { class: "label" }, label), h("div", { class: "value " + (cls || "") }, value));
    $("metric-cards").replaceChildren(
      card("Tools", tools.length),
      card("Invocations", total),
      card("Success (weighted)", pct(weighted), rateClass(weighted)),
      card("Need attention", tools.filter((t) => t.needs_attention).length, tools.some((t) => t.needs_attention) ? "rate-bad" : ""),
      card("Retired", tools.filter((t) => t.retired).length));
    const sorted = [...tools].sort((a, b) => b.invocations - a.invocations);
    toolsTable($("metrics-table"), sorted, {
      bars: true, selected: S.metricTool,
      onselect: (id) => { S.metricTool = id; selectTool(id, true); renderMetrics(); },
    });
    const box = $("metric-detail");
    box.replaceChildren();
    const d = S.toolDetail;
    if (d && d.tool.id === S.metricTool) {
      box.append(h("div", { class: "detail" }, h("h3", null, d.tool.name), metricsBlock(d.metrics)));
    }
    const rt = $("routing-table");
    rt.replaceChildren(h("thead", null, h("tr", null, ["Model", "Calls", "Success", "Avg latency", "Prompt tokens", "Cached"].map((c, i) => h("th", { class: i ? "num" : null }, c)))));
    const body = h("tbody");
    if (!S.modelStats.length) body.append(h("tr", null, h("td", { class: "empty", colspan: 6 }, "No provider calls measured yet.")));
    for (const st of S.modelStats) {
      const n = st.successes + st.failures;
      const r = n ? st.successes / n : null;
      body.append(h("tr", null, h("td", null, st.model), h("td", { class: "num" }, n), h("td", { class: "num" }, rateCell(r)),
        h("td", { class: "num" }, n ? Math.round(st.total_ms / n) + "ms" : "-"),
        h("td", { class: "num" }, st.prompt_tokens ? st.prompt_tokens.toLocaleString() : "-"),
        h("td", { class: "num" }, st.prompt_tokens ? Math.round(100 * st.cached_tokens / st.prompt_tokens) + "%" : "-")));
    }
    rt.append(body);
  }

  function renderProposals() {
    const box = $("proposals");
    box.replaceChildren();
    if (!S.proposals.length) {
      box.append(h("div", { class: "detail placeholder" }, "No proposals yet. Generate one to have the optimiser draft and measure a prompt or memory addendum."));
      return;
    }
    for (const p of S.proposals) {
      const cls = p.status === "applied" ? "good" : p.status === "measured" ? "mid" : p.status === "proposed" ? "" : "bad";
      const scored = p.baseline_score != null && p.candidate_score != null;
      const actions = [];
      if (p.status === "measured") {
        actions.push(h("button", { class: "btn primary small", type: "button", onclick: () => decideProposal(p, true) }, "Apply"));
      }
      if (p.status === "measured" || p.status === "proposed") {
        actions.push(h("button", { class: "btn ghost small", type: "button", onclick: () => decideProposal(p, false) }, "Reject"));
      }
      if (p.status === "applied") {
        actions.push(h("button", { class: "btn small", type: "button", onclick: () => act({ type: "rollback_proposal", id: p.id }, "Rolled back") }, "Roll back"));
      }
      box.append(h("article", { class: "proposal" },
        h("div", { class: "proposal-head" },
          h("span", { class: "tag" }, p.kind),
          h("span", { class: "status-pill " + cls }, p.status),
          scored ? h("span", { class: "score" },
            h("span", { class: p.candidate_score > p.baseline_score ? "rate-good" : "rate-bad" }, pct(p.candidate_score)),
            h("span", { class: "muted" }, " vs " + pct(p.baseline_score) + " baseline · " + p.eval_total + " evals")) : h("span", { class: "muted score" }, "unmeasured"),
          h("span", { class: "spacer" }),
          h("span", { class: "muted score" }, "~" + p.est_tokens + " tokens · " + relTime(p.created_at))),
        h("div", { class: "content" }, p.content),
        p.rationale ? h("p", { class: "muted", style: "margin:8px 0 0" }, p.rationale) : null,
        actions.length ? h("div", { class: "actions" }, actions) : null));
    }
  }

  function renderSettings() {
    const box = $("settings");
    box.replaceChildren();
    // A row is [label, value] or [label, value, settingKey, kind]: the
    // latter gets an Edit control (SetSetting, validated by the daemon).
    const section = (title, rows) => h("section", null, h("h3", null, title), h("dl", { class: "kv" },
      rows.map(([k, v, key, kind]) => [h("dt", null, k), h("dd", null,
        v === "" || v == null ? h("span", { class: "muted" }, "(not set)") : String(v),
        key ? h("button", { class: "btn ghost small", type: "button", onclick: () => editSetting(key, v, kind) }, "Edit") : null)])));
    const on = (b) => (b ? "on" : "off");
    const grid = h("div", { class: "settings-grid" });
    const c = S.settings;
    if (c && !c.error) {
      grid.append(
        section("Provider", [["base_url", c.provider.base_url], ["model", c.provider.model, "provider.model", "text"],
          ["reasoning_effort", c.provider.reasoning_effort || "default", "provider.reasoning_effort", "optional"],
          ["api_style", c.provider.api_style], ["tool_mode", c.provider.tool_mode], ["auth", c.provider.auth_mode],
          ["context_tokens", c.provider.context_tokens, "provider.context_tokens", "number"],
          ["fallback_models", (c.provider.fallback_models || []).join(", "), "provider.fallback_models", "list"]]),
        section("Interface (TUI)", [["multi_pane", on(c.tui.multi_pane), "tui.multi_pane", "bool"], ["queue_key", c.tui.queue_key]]),
        section("Relay", [["enabled", on(c.relay.enabled)], ["url", c.relay.url]]),
        section("Paths", [["config", c.paths.config], ["data", c.paths.data]]));
    } else if (c && c.error) {
      grid.append(h("section", null, h("h3", null, "Daemon settings"), h("p", { class: "muted" }, c.error)));
    }
    const st = S.status;
    if (st) {
      const auth = st.auth || {};
      grid.append(section("Daemon", [["protocol", st.protocol], ["pid", st.pid], ["sessions", st.sessions],
        ["credential", auth.credential_present ? "present" : "missing"], ["auth mode", auth.mode],
        ["token expires", auth.expires_at ? new Date(auth.expires_at * 1000).toLocaleString() : ""]]));
    }
    const mcp = S.mcpServers || [];
    grid.append(h("section", null, h("h3", null, "MCP servers"),
      mcp.length ? h("table", { class: "table" },
        h("tbody", null, mcp.map((m) => h("tr", null,
          h("td", null, h("strong", null, m.name), h("div", { class: "muted" }, m.transport + " · " + m.target),
            m.last_error ? h("div", { class: "error" }, m.last_error) : null),
          h("td", null, m.disabled ? "disabled" : m.running ? "running" : "idle"),
          h("td", { class: "num" }, m.tool_count + " tools"),
          h("td", null,
            h("button", { class: "btn small", type: "button", onclick: () => act({ type: "restart_mcp_server", name: m.name }, "Restarted " + m.name).then(refreshSettings) }, "Restart"),
            h("button", { class: "btn small danger", type: "button", onclick: async () => {
              const ok = await ask({ title: "Remove MCP server " + m.name + "?", text: "Its tools are retired (metrics kept).", ok: "Remove", danger: true });
              if (ok) { await act({ type: "remove_mcp_server", name: m.name }, "Removed " + m.name); refreshSettings(); }
            } }, "Remove"))))))
        : h("p", { class: "muted" }, "None. Add one with formless mcp add NAME -- COMMAND, or --url URL.")));
    const skills = S.skills || [];
    grid.append(h("section", null, h("h3", null, "Skills"),
      skills.length ? h("table", { class: "table" },
        h("tbody", null, skills.map((sk) => h("tr", null,
          h("td", null, h("strong", null, sk.name + " v" + sk.version), h("div", { class: "muted" }, sk.description),
            sk.has_scripts ? h("div", { class: "muted" }, "bundles scripts (never run on the host)") : null,
            sk.author === "auto" ? h("div", { class: "muted" }, sk.retired ? "auto-created · retired (enable to restore)" : "auto-created from a successful Run") : null),
          h("td", { class: "num" }, sk.loads + " loads" + (sk.runs ? " · " + Math.round(100 * sk.runs_completed / sk.runs) + "% of " + sk.runs + " Runs completed" : "")),
          h("td", null,
            h("button", { class: "btn small", type: "button", onclick: () => reviewSkill(sk.name) }, "Review"),
            h("button", { class: "btn small" + (sk.enabled ? "" : " primary"), type: "button", onclick: () => act({ type: "set_skill_enabled", name: sk.name, enabled: !sk.enabled }, (sk.enabled ? "Disabled " : "Enabled ") + sk.name).then(refreshSettings) }, sk.enabled ? "Disable" : "Enable"),
            h("button", { class: "btn small danger", type: "button", onclick: async () => {
              if (await ask({ title: "Remove skill " + sk.name + "?", text: "All its versions are deleted.", ok: "Remove", danger: true })) { await act({ type: "remove_skill", name: sk.name }, "Removed " + sk.name); refreshSettings(); }
            } }, "Remove"))))))
        : h("p", { class: "muted" }, "None. Install one with formless skills add FOLDER | GIT_URL | FILE.zip")));
    const audit = S.audit || [];
    const auditFilter = h("input", { type: "search", placeholder: "Filter: tool.approve, skill, a Tool name…", value: S.auditFilter || "" });
    auditFilter.addEventListener("change", () => { S.auditFilter = auditFilter.value; loadAudit(false); });
    grid.append(h("section", null, h("h3", null, "Audit log"),
      auditFilter,
      audit.length ? h("table", { class: "table" },
        h("tbody", null, audit.map((e) => h("tr", null,
          h("td", { class: "muted" }, relTime(e.created_at)),
          h("td", null, h("strong", null, e.action), h("div", { class: "muted" }, e.actor + (e.detail ? " · " + e.detail : "")))))))
        : h("p", { class: "muted" }, "No entries."),
      audit.length >= 50 * (S.auditPages || 1) ? h("div", { class: "detail-actions" },
        h("button", { class: "btn small", type: "button", onclick: () => loadAudit(true) }, "Older")) : null));
    const jobs = S.jobs || [];
    grid.append(h("section", null, h("h3", null, "Jobs"),
      jobs.length ? h("table", { class: "table" },
        h("tbody", null, jobs.map((j) => h("tr", null,
          h("td", null, h("div", null, j.prompt),
            h("div", { class: "muted" }, (j.cron || "once") + " · next " + (j.next_run ? new Date(j.next_run).toLocaleString() : "-") + (j.last_status ? " · last: " + j.last_status : "") + " · " + j.author)),
          h("td", null,
            h("button", { class: "btn small" + (j.enabled && j.approved ? "" : " primary"), type: "button", onclick: () => act({ type: "set_job_enabled", id: j.id, enabled: !(j.enabled && j.approved) }, "Updated").then(refreshSettings) },
              !j.approved ? "Approve" : j.enabled ? "Disable" : "Enable"),
            h("button", { class: "btn small danger", type: "button", onclick: async () => {
              if (await ask({ title: "Delete this job?", text: j.prompt, ok: "Delete", danger: true })) { await act({ type: "delete_job", id: j.id }, "Deleted"); refreshSettings(); }
            } }, "Delete"))))))
        : h("p", { class: "muted" }, "None. The agent can schedule_run, or: formless jobs add PROMPT --cron \"0 3 * * *\"")));
    const memories = S.memories || [];
    grid.append(h("section", null, h("h3", null, "Memory"),
      h("p", { class: "muted" }, "Notes the agent keeps about your preferences; they go into new sessions' prompts."),
      memories.length ? h("table", { class: "table" },
        h("tbody", null, memories.map((m) => h("tr", null,
          h("td", null, h("div", { class: m.enabled ? "" : "muted" }, m.text),
            h("div", { class: "muted" }, m.scope + " · " + (m.session_id ? "from session " + m.session_id.slice(0, 8) : m.author) + (m.enabled ? "" : " · off"))),
          h("td", null,
            h("button", { class: "btn small", type: "button", onclick: async () => {
              const text = await ask({ title: "Edit memory", input: true, value: m.text, ok: "Save" });
              if (text && text !== m.text) { await act({ type: "upsert_memory", id: m.id, text }, "Saved"); refreshSettings(); }
            } }, "Edit"),
            h("button", { class: "btn small", type: "button", onclick: () => act({ type: "upsert_memory", id: m.id, enabled: !m.enabled }, m.enabled ? "Disabled" : "Enabled").then(refreshSettings) }, m.enabled ? "Disable" : "Enable"),
            h("button", { class: "btn small danger", type: "button", onclick: async () => {
              if (await ask({ title: "Delete this memory?", text: m.text, ok: "Delete", danger: true })) { await act({ type: "delete_memory", id: m.id }, "Deleted"); refreshSettings(); }
            } }, "Delete"))))))
        : h("p", { class: "muted" }, "None yet."),
      h("div", { class: "detail-actions" },
        h("button", { class: "btn small", type: "button", onclick: async () => {
          const text = await ask({ title: "Add a memory", input: true, placeholder: "e.g. Use pnpm, not npm", ok: "Add" });
          if (text) { await act({ type: "upsert_memory", text }, "Added"); refreshSettings(); }
        } }, "Add"))));
    const thinking = h("input", { type: "checkbox", onchange: (e) => setThinking(e.target.checked) });
    thinking.checked = S.showThinking;
    grid.append(h("section", null, h("h3", null, "This browser"),
      h("dl", { class: "kv" },
        h("dt", null, "thinking"), h("dd", null, h("label", { class: "toggle" }, thinking, "show live reasoning")),
        h("dt", null, "encryption"), h("dd", null, "AES-256-GCM, key held only in this tab")),
      h("div", { class: "detail-actions" },
        h("button", { class: "btn danger small", type: "button", onclick: forget }, "Disconnect and forget this device"))));
    box.append(grid, h("p", { class: "muted" }, "Edit applies through the daemon (next Run). Credentials, the endpoint, the relay and MCP servers are changed in config.toml on the daemon's machine."));
  }

  // ============================================================ actions
  async function refreshSessions() {
    const list = await act({ type: "list_sessions" });
    if (list) S.sessions = list;
    renderSessions();
    renderComposer();
    renderStatus();
  }
  async function refreshTools() {
    const list = await act({ type: "list_tools" });
    if (list) S.tools = list;
    renderView();
  }
  async function refreshModules() {
    const list = await act({ type: "list_modules" });
    if (list) S.modules = list;
    renderView();
  }
  async function refreshModelStats() {
    const list = await act({ type: "list_model_stats" });
    if (list) S.modelStats = list;
    renderView();
  }
  async function refreshProposals() {
    const list = await act({ type: "list_proposals" });
    if (list) S.proposals = list;
    renderView();
  }
  async function reviewSkill(name) {
    const r = await act({ type: "get_skill", name });
    if (!r) return;
    await ask({ title: r.skill.name + " v" + r.skill.version, text: r.body + "\n\nFiles: " + r.files.join(", "), ok: "Close" });
  }
  async function editSetting(key, current, kind) {
    let value;
    if (kind === "bool") value = !(current === "on" || current === true);
    else {
      const text = await ask({ title: "Set " + key, input: true, value: current == null || current === "default" ? "" : String(current), ok: "Save",
        text: kind === "list" ? "Comma-separated." : kind === "optional" ? "Leave empty for the default." : "" });
      if (text == null) return;
      if (kind === "number") { value = Number(text); if (!Number.isFinite(value)) { toast("not a number", true); return; } }
      else if (kind === "list") value = text.split(",").map((s) => s.trim()).filter(Boolean);
      else if (kind === "optional") value = text.trim() ? text.trim() : null;
      else value = text.trim();
    }
    if (await act({ type: "set_setting", key, value }, "Saved " + key)) refreshSettings();
  }
  async function loadAudit(older) {
    const before = older && S.audit && S.audit.length ? S.audit[S.audit.length - 1].created_at : null;
    const page = (await request({ type: "list_audit", filter: S.auditFilter || null, limit: 50, before }).catch(() => null)) || [];
    S.audit = older ? (S.audit || []).concat(page) : page;
    S.auditPages = older ? (S.auditPages || 1) + 1 : 1;
    renderView();
  }
  async function refreshSettings() {
    S.mcpServers = (await request({ type: "list_mcp_servers" }).catch(() => null)) || [];
    S.skills = (await request({ type: "list_skills" }).catch(() => null)) || [];
    S.memories = (await request({ type: "list_memories" }).catch(() => null)) || [];
    S.jobs = (await request({ type: "list_jobs" }).catch(() => null)) || [];
    if (!S.auditPages || S.auditPages === 1) S.audit = (await request({ type: "list_audit", filter: S.auditFilter || null, limit: 50 }).catch(() => null)) || [];
    try {
      S.settings = await request({ type: "get_settings" });
    } catch (e) {
      S.settings = { error: "This daemon does not expose settings remotely (" + e.message + "). Update formless to see them here." };
    }
    S.status = await act({ type: "status" });
    renderView();
    renderStatus();
  }
  // "3 steps · 12.3k in / 1.2k out · 40% cached · $0.021" (SPEC §10.6).
  function usageText(u) {
    const k = (n) => (n >= 1000 ? (n / 1000).toFixed(1) + "k" : String(n));
    let s = u.steps + " step" + (u.steps === 1 ? "" : "s") + " · " + k(u.prompt_tokens) + " in / " + k(u.completion_tokens) + " out";
    if (u.prompt_tokens > 0 && u.cached_tokens > 0) s += " · " + Math.round((100 * u.cached_tokens) / u.prompt_tokens) + "% cached";
    if (u.cost != null) s += " · $" + u.cost.toFixed(3);
    return s;
  }
  async function revokeApproval(t, capability) {
    if (!(await ask({ title: "Revoke " + (capability || "every approval") + " for " + t.name + "?", text: "The next call that needs it asks again.", ok: "Revoke", danger: true }))) return;
    if (await act({ type: "revoke_approval", tool_id: t.id, capability }, "Revoked")) selectTool(t.id, true);
  }
  async function selectTool(id, quiet) {
    S.selectedTool = id;
    if (!quiet && S.editingCode && S.editingCode.toolId !== id) S.editingCode = null;
    const d = await act({ type: "get_tool", id });
    if (d) S.toolDetail = d;
    renderView();
  }

  /** Open a session; `stay` keeps the current view (initial load). */
  async function openSession(id, stay) {
    closeSidebar();
    if (!stay && S.view !== "chat") setView("chat");
    S.current = id;
    request({ type: "session_usage", session_id: id }).then((r) => {
      S.sessionUsage = { id, runs: r.runs, total: r.total };
      if (r.last_run && r.last_run.steps) (S.usage = S.usage || {})[id] = r.last_run;
      renderStatus();
    }).catch(() => {});
    rendered.clear(); // a reload may carry edited messages
    // Live traffic only for the Session on screen: the relay then carries
    // (and bills) one Session's tokens, not every Session's.
    request({ type: "subscribe", sessions: [id], deltas: true }).catch(() => {});
    prefs.set("session", id);
    S.streaming = "";
    S.thinking = "";
    S.liveTools = [];
    S.editMessageId = null;
    S.follow = true;
    const r = await act({ type: "get_session", id });
    if (r && S.current === id) {
      S.messages = r.messages || [];
      S.unfinished = r.unfinished_run || null;
      renderRunBanner();
      if (r.queued && r.queued.length) note(r.queued.length + " queued message(s) start after the current run");
      const i = S.sessions.findIndex((s) => s.id === id);
      if (i >= 0 && r.session) S.sessions[i] = r.session;
    }
    if (!isRunning()) S.activity = null;
    renderAll();
  }
  async function newSession() {
    const s = await act({ type: "create_session", name: null });
    if (!s) return;
    S.sessions = [s, ...S.sessions.filter((x) => x.id !== s.id)];
    await openSession(s.id);
    $("input").focus();
  }
  async function renameSession(s) {
    const name = await ask({ title: "Rename session", input: true, value: s.name, ok: "Rename" });
    if (name && name.trim() && name.trim() !== s.name) await act({ type: "rename_session", id: s.id, name: name.trim() });
  }
  async function forkSession(id) {
    if (!id) return;
    const s = await act({ type: "fork_session", id, name: null, up_to_seq: null }, "Forked session");
    if (s && s.id) { S.sessions = [s, ...S.sessions.filter((x) => x.id !== s.id)]; await openSession(s.id); }
  }
  async function exportSession(id) {
    if (!id) return;
    const bundle = await act({ type: "export_session", id });
    if (bundle) download("formless-session-" + slug(bundle.name) + ".json", bundle);
  }
  async function importSession() {
    const bundle = await pickJson();
    if (!bundle) return;
    const s = await act({ type: "import_session", bundle }, "Session imported");
    await refreshSessions();
    if (s && s.id) openSession(s.id);
  }
  async function deleteSession(s) {
    const ok = await ask({ title: "Delete “" + s.name + "”?", text: "This permanently deletes the session and its transcript.", ok: "Delete", danger: true });
    if (ok) await act({ type: "delete_session", id: s.id }, "Session deleted");
  }

  function beginEdit(msg) {
    S.editMessageId = msg.id;
    $("input").value = msg.content;
    autosize();
    renderComposer();
    $("input").focus();
  }
  function cancelEdit() {
    S.editMessageId = null;
    $("input").value = "";
    autosize();
    renderComposer();
  }

  function rollbackTool(t, version) {
    ask({ title: "Roll back " + t.name + " to v" + version + "?", text: "Later Invocations use v" + version + "; every version is kept.", ok: "Roll back" })
      .then((ok) => { if (ok) act({ type: "rollback_tool", id: t.id, version }, t.name + " rolled back to v" + version).then(() => selectTool(t.id, true)); });
  }
  async function purgeTool(t) {
    const typed = await ask({ title: "Purge " + t.name + " permanently?", text: "All versions, metrics, and history are deleted. Type the Tool name to confirm.", input: true, placeholder: t.name, ok: "Purge", danger: true });
    if (typed == null) return;
    if (typed.trim() !== t.name) { toast("Name did not match; nothing was purged.", true); return; }
    const r = await act({ type: "purge_tool", id: t.id }, "Purged " + t.name);
    if (r) { S.selectedTool = null; S.toolDetail = null; refreshTools(); }
  }
  async function exportTool(t) {
    const bundle = await act({ type: "export_tool", id: t.id });
    if (bundle) download("formless-tool-" + slug(t.name) + ".json", bundle);
  }
  async function saveCode(t) {
    if (!S.editingCode) return;
    const r = await act({ type: "update_tool_code", id: t.id, code: S.editingCode.code }, null, 120000);
    if (!r) return;
    toast("Saved " + t.name + " v" + r.version);
    S.editingCode = null;
    selectTool(t.id, true);
    refreshTools();
  }
  async function decideProposal(p, approved) {
    await act({ type: "proposal_decision", id: p.id, approved }, approved ? "Proposal applied" : "Proposal rejected");
  }

  function setThinking(on) {
    S.showThinking = on;
    prefs.set("thinking", on);
    renderLive();
    renderComposer();
    if (S.view === "settings") renderSettings();
  }

  const HELP = "Enter send/steer · Ctrl/⌘+Enter queue · Esc stop · /new /sessions /fork /search <q> /edit /tools /metrics /proposals /propose /settings /thinking /stop /cancel /resend /resume /abandon /agents /prompts /help";
  async function slash(cmd) {
    const [name, ...rest] = cmd.split(/\s+/);
    const arg = rest.join(" ");
    switch (name) {
      case "new": return newSession();
      case "sessions": setView("chat"); openSidebar(); return $("search").focus();
      case "fork": return forkSession(S.current);
      case "search": setView("chat"); openSidebar(); $("search").value = arg; return arg ? search(arg) : $("search").focus();
      case "edit": case "retry": {
        const m = [...S.messages].reverse().find((x) => x.role === "user");
        return m ? beginEdit(m) : toast("No user message to edit.", true);
      }
      case "tools": case "metrics": case "proposals": case "settings": return setView(name);
      case "propose": act({ type: "generate_proposal" }); return note("generating a measured proposal…");
      case "thinking": setThinking(!S.showThinking); return note("thinking display " + (S.showThinking ? "on" : "off"));
      case "usage": {
        if (!S.current) return note("open a session first", true);
        const r = await act({ type: "session_usage", session_id: S.current });
        if (!r) return;
        if (r.last_run) note("last Run: " + usageText(r.last_run));
        return note("session (" + r.runs + " Runs): " + usageText(r.total));
      }
      case "stop": return stop();
      case "prompts": {
        const list = (await act({ type: "list_mcp_prompts" })) || [];
        if (!list.length) return note("no MCP prompts available");
        for (const p of list) note("/prompt " + p.server + ":" + p.name + " " + (p.arguments || []).map((a) => a.name + "=…").join(" ") + " — " + (p.description || ""));
        return;
      }
      case "prompt": {
        const [target, ...pairs] = arg.split(/\s+/);
        const [server, name] = (target || "").split(":");
        if (!server || !name) return note("usage: /prompt SERVER:NAME [key=value …] (see /prompts)", true);
        const r = await act({ type: "get_mcp_prompt", server, name, arguments: Object.fromEntries(pairs.filter((p) => p.includes("=")).map((p) => p.split(/=(.*)/s).slice(0, 2))) });
        if (r) { $("input").value = r.text; autosize(); $("input").focus(); note("prompt loaded into the composer; edit it, then send"); }
        return;
      }
      case "agents": {
        if (!S.current) return;
        if (!arg) {
          S.subagents = (await act({ type: "list_subagents", session_id: S.current })) || [];
          if (!S.subagents.length) return note("no sub-agents in this session");
          S.subagents.forEach((a, i) => note("#" + (i + 1) + " " + a.status + " · " + a.steps + " step(s) · " + truncate(a.task, 70)));
          return note("/agents <n> shows that sub-agent's transcript");
        }
        const a = (S.subagents || [])[Number(arg) - 1];
        if (!a) return note("usage: /agents to list, then /agents <n>", true);
        const r = await act({ type: "get_subagent", id: a.id });
        for (const m of (r && r.messages) || []) {
          const text = m.tool_calls ? "calls " + m.tool_calls.map((c) => c.function.name).join(", ") : String(m.content || "");
          note("[" + m.role + "] " + truncate(text.replace(/\n/g, " "), 120));
        }
        return;
      }
      case "cancel": {
        const t = [...S.liveTools].reverse().find((x) => x.status === "running" && x.handle);
        return cancelTool(t && t.handle);
      }
      case "resend":
        if (S.current) await act({ type: "retry_provider_request", session_id: S.current }, "Re-sending the request");
        return;
      case "resume":
        if (S.current) await act({ type: "resume_run", session_id: S.current });
        return;
      case "abandon": {
        if (!S.current) return;
        const r = await act({ type: "abandon_run", session_id: S.current });
        if (r) note(r.abandoned ? "abandoned the interrupted run" : "nothing to abandon");
        return;
      }
      case "panes": return note("The web layout always shows Sessions beside the chat.");
      case "model": {
        const s = S.sessions.find((x) => x.id === S.current);
        return note("model: " + ((s && s.model) || (S.settings && S.settings.provider.model) || "default") + " (see Settings)");
      }
      case "help": case "": toast(HELP); return note(HELP);
      case "quit": return forget();
      default: return note("unknown command: /" + name, true);
    }
  }

  async function submit(queued) {
    const input = $("input");
    const text = input.value.trim();
    if (!text) return;
    if (text.startsWith("/") && !S.editMessageId) {
      input.value = "";
      autosize();
      return slash(text.slice(1).trim());
    }
    if (!S.current) await newSession();
    if (!S.current) return;
    input.value = "";
    autosize();
    S.follow = true;
    if (S.editMessageId) {
      const id = S.editMessageId;
      S.editMessageId = null;
      renderComposer();
      await act({ type: "edit_message", session_id: S.current, message_id: id, content: text });
      return;
    }
    const mode = queued ? "queue" : isRunning() ? "steer" : "send";
    const r = await act({ type: "send_message", session_id: S.current, content: text, mode });
    if (r && mode !== "send") note(mode === "queue" ? "queued for after this run" : "steering the current run");
  }
  function stop() {
    if (S.current && isRunning()) act({ type: "interrupt_run", session_id: S.current });
  }
  async function search(q) {
    q = q.trim();
    if (!q) { renderSearch(null); return; }
    const hits = await act({ type: "search_sessions", query: q, limit: 50 });
    if (hits) renderSearch(hits, q);
  }

  function autosize() {
    const t = $("input");
    t.style.height = "auto";
    t.style.height = Math.min(t.scrollHeight, 220) + "px";
  }

  function openSidebar() { $("sidebar").classList.add("open"); }
  function closeSidebar() { $("sidebar").classList.remove("open"); }

  // ============================================================== events
  let toolRefresh = null;
  function onEvent(ev) {
    const here = ev.session_id === S.current;
    switch (ev.type) {
      case "run_started":
        S.running.add(ev.session_id);
        if (here) { S.unfinished = null; renderRunBanner(); }
        if (here) { S.streaming = ""; S.thinking = ""; S.liveTools = []; setActivity("waiting for model"); }
        break;
      case "message_appended":
      case "message_complete":
        // A persisted assistant step (e.g. text before a Tool call) replaces
        // the live stream, so the next step starts fresh.
        if (here && (ev.type === "message_complete" || ev.message.role === "assistant")) { S.streaming = ""; S.thinking = ""; }
        if (here && !S.messages.some((m) => m.id === ev.message.id)) { S.messages.push(ev.message); renderHistory(); }
        break;
      case "message_delta":
        if (here) { S.streaming += ev.delta; if (!S.activity || S.activity.label !== "responding") setActivity("responding"); else renderLive(); }
        break;
      case "provider_stalled":
        if (here) { setActivity("provider stalled"); note("no data from the provider for " + ev.idle_secs + "s; /resend re-sends the request", true); }
        break;
      case "stream_restarted":
        if (here) { S.streaming = ""; S.thinking = ""; note("provider stream dropped; retrying"); renderLive(); }
        break;
      case "reasoning_delta":
        if (here) { S.thinking += ev.delta; if (!S.activity || S.activity.label !== "thinking") setActivity("thinking"); else renderLive(); }
        break;
      case "run_finished":
      case "run_paused":
        S.running.delete(ev.session_id);
        if (here) { S.unfinished = { status: "paused", error: ev.error }; renderRunBanner(); }
        if (here) {
          S.streaming = "";
          S.thinking = "";
          S.liveTools = [];
          setActivity(null);
          if (ev.type === "run_paused") note("run paused: " + ev.error, true);
          else if (ev.status !== "completed") note("run " + ev.status, ev.status !== "interrupted");
          renderHistory();
        }
        break;
      case "tool_call_started":
        if (here) {
          S.liveTools.push({ name: ev.tool_name, version: ev.version, arguments: ev.arguments, status: "running", started: Date.now() });
          setActivity("running " + ev.tool_name);
        }
        break;
      case "tool_call_finished":
        if (S.invocations && S.invocations.length) refreshRunning();
        if (here) {
          const t = S.liveTools.find((x) => x.name === ev.tool_name && x.status === "running");
          if (t) t.status = ev.ok ? "ok" : "failed";
          if (!ev.ok) note(ev.tool_name + " failed after " + ev.elapsed_ms + "ms", true);
          setActivity(isRunning() ? "waiting for model" : null);
        }
        break;
      case "user_question": {
        if (here) setActivity("awaiting your answer");
        S.questionId = ev.question_id;
        const opts = (ev.options || []).map((o, i) => (i + 1) + ") " + o).join("\n");
        ask({ title: "The agent asks", text: ev.question + (opts ? "\n\n" + opts : ""), input: true, placeholder: opts ? "Your answer, or an option number" : "Your answer", ok: "Answer" }).then((text) => {
          if (S.questionId !== ev.question_id || text == null || text === "") return;
          S.questionId = null;
          const n = parseInt(text, 10);
          const answer = String(n) === text.trim() && ev.options && ev.options[n - 1] ? ev.options[n - 1] : text;
          act({ type: "answer_question", question_id: ev.question_id, answer });
        });
        break;
      }
      case "run_usage":
        (S.usage = S.usage || {})[ev.session_id] = ev.usage;
        if (here) renderStatus();
        break;
      case "user_question_closed":
        // Answered elsewhere (or the Run stopped): close our prompt.
        if (S.questionId === ev.question_id) { S.questionId = null; closeAsk(null); }
        break;
      case "approval_requested":
        S.approvals.push({ kind: "tool", id: ev.approval_id, tool_name: ev.tool_name, version: ev.version, capabilities: ev.capabilities || [], memory_mb: ev.memory_mb, detail: ev.detail || null,
          already: ev.already_allowed || [], code: ev.code_preview || null });
        if (here) setActivity("awaiting your approval");
        renderApproval();
        break;
      case "promotion_approval_requested":
        S.approvals.push({ kind: "promotion", id: ev.promotion_id, tool_name: ev.tool_name, reason: ev.reason, capabilities: ev.capabilities || [], memory_mb: ev.memory_mb });
        renderApproval();
        break;
      case "session_updated": {
        const i = S.sessions.findIndex((s) => s.id === ev.session.id);
        if (i >= 0) S.sessions[i] = ev.session; else S.sessions.unshift(ev.session);
        S.sessions.sort((a, b) => String(b.updated_at).localeCompare(String(a.updated_at)));
        renderSessions();
        renderComposer();
        break;
      }
      case "session_deleted":
        S.sessions = S.sessions.filter((s) => s.id !== ev.id);
        if (S.current === ev.id) {
          S.current = null;
          S.messages = [];
          if (S.sessions.length) openSession(S.sessions[0].id);
        }
        renderAll();
        break;
      case "messages_edited":
        if (here) openSession(S.current);
        break;
      case "tool_updated": {
        const i = S.tools.findIndex((t) => t.id === ev.tool.id);
        if (i >= 0) S.tools[i] = ev.tool; else S.tools.push(ev.tool);
        if (ev.tool.id === S.selectedTool || ev.tool.id === S.metricTool) {
          clearTimeout(toolRefresh);
          toolRefresh = setTimeout(() => selectTool(ev.tool.id, true), 300);
        }
        renderView();
        break;
      }
      case "tool_deleted":
        S.tools = S.tools.filter((t) => t.id !== ev.id);
        if (S.selectedTool === ev.id) { S.selectedTool = null; S.toolDetail = null; }
        renderView();
        break;
      case "module_updated": {
        const i = S.modules.findIndex((m) => m.id === ev.module.id);
        if (i >= 0) S.modules[i] = ev.module; else S.modules.push(ev.module);
        renderView();
        break;
      }
      case "proposal_updated": {
        const i = S.proposals.findIndex((p) => p.id === ev.proposal.id);
        if (i >= 0) S.proposals[i] = ev.proposal; else S.proposals.unshift(ev.proposal);
        note("proposal " + ev.proposal.status + ": " + truncate(ev.proposal.content, 60));
        renderView();
        break;
      }
      case "note":
        note(ev.text, /fail|error|denied|paused/i.test(ev.text));
        break;
      case "compaction_notice":
        if (here) note("compacted earlier context: " + truncate(ev.summary_preview, 80));
        break;
      case "invocation_pending": {
        refreshRunning();
        const t = here && runningTool(ev.tool_name);
        if (t) t.handle = ev.handle;
        note(ev.tool_name + " continues in the background (" + ev.handle + ")");
        break;
      }
      case "invocation_stalled": {
        refreshRunning();
        const t = here && runningTool(ev.tool_name);
        if (t) {
          t.handle = ev.handle;
          t.stalledSecs = ev.silent_secs;
          note(ev.tool_name + ": no activity for " + ev.silent_secs + "s; /cancel stops just this tool", true);
          renderLive();
        }
        break;
      }
      case "settings_changed":
      case "mcp_servers_changed":
      case "skills_changed":
      case "memory_updated":
      case "jobs_changed":
        refreshSettings();
        break;
      case "subagent_started":
        if (here) note("sub-agent started: " + truncate(ev.subagent.task, 80));
        break;
      case "subagent_step":
        if (here && isRunning()) setActivity("sub-agent step " + ev.step + ": " + (ev.tool_calls.length ? ev.tool_calls.join(", ") : "answering"));
        break;
      case "subagent_finished":
        if (here) note("sub-agent " + ev.subagent.status + " after " + ev.subagent.steps + " step(s) · /agents to inspect", ev.subagent.status !== "completed");
        break;
      case "invocation_activity":
        for (const t of S.liveTools) if (t.handle === ev.handle) t.stalledSecs = null;
        if (here) renderLive();
        break;
      case "invocation_completed":
        note(ev.tool_name + " background invocation " + (ev.ok ? "finished" : "failed"), !ev.ok);
        break;
      case "optimiser_activity":
        note("optimiser · " + ev.tool_name + ": " + ev.text);
        break;
    }
    renderStatus();
    renderComposer();
  }

  // =========================================================== connection
  async function loadAll() {
    await refreshSessions();
    const saved = prefs.get("session", null);
    const target = S.sessions.find((s) => s.id === S.current) || S.sessions.find((s) => s.id === saved) || S.sessions[0];
    if (target) await openSession(target.id, true);
    else renderAll();
    if (S.view !== "chat") setView(S.view);
    refreshTools();
    refreshModules();
    refreshProposals();
    refreshModelStats();
    refreshSettings();
  }

  async function connect(secret) {
    $("pair-error").textContent = "";
    let keyBytes;
    try {
      keyBytes = b64ToBytes(secret);
      if (keyBytes.length !== 32) throw new Error("length");
    } catch (_) {
      $("pair-error").textContent = "That pairing secret does not look right. Run formless relay status to see it again.";
      showPair();
      return;
    }
    S.secret = secret;
    S.userClosed = false;
    S.key = await crypto.subtle.importKey("raw", keyBytes, "HKDF", false, ["deriveKey"]);
    S.channel = null;
    const room = await roomFor(keyBytes);
    const proto = location.protocol === "https:" ? "wss:" : "ws:";
    const ws = new WebSocket(proto + "//" + location.host + "/relay/" + room + "?role=client");
    S.ws = ws;
    ws.onopen = () => {
      S.backoff = 1000;
      try { sessionStorage.setItem("formless_secret", secret); } catch (_) {}
      // Keep the key out of the address bar and history.
      if (location.hash) history.replaceState(null, "", location.pathname);
      $("pair").classList.add("hidden");
      $("app").classList.remove("hidden");
      renderAll();
    };
    // Frames are handled one at a time, in arrival order.
    ws.onmessage = (e) => {
      S.recvChain = S.recvChain.then(() => onFrame(ws, String(e.data))).catch(() => {});
    };
    async function onFrame(ws, frame) {
      if (S.ws !== ws) return;
      if (frame.startsWith('{"relay":')) {
        const online = JSON.parse(frame).relay === "daemon_online";
        const was = S.daemonOnline;
        S.daemonOnline = online;
        if (online && !was) startHandshake();
        if (!online && was) { S.channel = null; note("daemon went offline", true); }
        renderChrome();
        renderHistory();
        return;
      }
      if (!S.channel) {
        let hello = null;
        try {
          const v = JSON.parse(frame);
          if (v.v === FRAME_VERSION && typeof v.hello === "string") hello = b64ToBytes(v.hello);
        } catch (_) {}
        if (!hello || hello.length !== 16 || !S.hello) return;
        await finishHandshake(hello);
        note("daemon online");
        loadAll();
        return;
      }
      let msg;
      try { msg = JSON.parse(await decrypt(frame)); } catch (_) { return; }
      onMessage(msg);
    }
    ws.onclose = () => {
      if (S.ws !== ws) return;
      S.daemonOnline = false;
      S.channel = null;
      S.running.clear();
      S.activity = null;
      for (const p of S.pending.values()) p.reject(new Error("relay connection closed"));
      S.pending.clear();
      if (!$("app").classList.contains("hidden")) renderAll();
      if (S.userClosed) return;
      // Reconnect with backoff; the relay and daemon are both stateless here.
      setTimeout(() => { if (!S.userClosed && S.ws === ws) connect(S.secret); }, S.backoff);
      S.backoff = Math.min(S.backoff * 2, 15000);
    };
  }

  function onMessage(msg) {
    if (msg.kind === "response") {
      const p = S.pending.get(msg.id);
      if (!p) return; // timed out already
      S.pending.delete(msg.id);
      if (msg.ok) p.resolve(msg.result); else p.reject(new Error(msg.error || "daemon error"));
    } else if (msg.kind === "event") onEvent(msg);
  }

  // Local mode (SPEC §12): `formless web` serves this page on 127.0.0.1
  // and the protocol runs over a same-origin WebSocket, no relay crypto.
  async function connectLocal(token) {
    S.local = true;
    S.userClosed = false;
    if (token) {
      history.replaceState(null, "", location.pathname);
      const r = await fetch("auth", { method: "POST", body: token, credentials: "same-origin" }).catch(() => null);
      if (!r || !r.ok) {
        $("pair-error").textContent = r ? await r.text() : "Could not reach the local daemon.";
        showPair();
        return;
      }
    }
    const ws = new WebSocket((location.protocol === "https:" ? "wss:" : "ws:") + "//" + location.host + "/ws");
    S.ws = ws;
    ws.onopen = () => {
      S.backoff = 1000;
      S.channel = { local: true };
      S.daemonOnline = true;
      $("pair").classList.add("hidden");
      $("app").classList.remove("hidden");
      renderAll();
      note("connected to the local daemon");
      loadAll();
    };
    ws.onmessage = (e) => {
      let msg;
      try { msg = JSON.parse(String(e.data)); } catch (_) { return; }
      onMessage(msg);
    };
    ws.onclose = () => {
      if (S.ws !== ws) return;
      const wasOpen = S.channel != null;
      S.daemonOnline = false;
      S.channel = null;
      S.running.clear();
      for (const p of S.pending.values()) p.reject(new Error("local daemon connection closed"));
      S.pending.clear();
      if (S.userClosed) return;
      if (!wasOpen) {
        $("pair-error").textContent = "Not signed in to the local daemon: run formless web for a new link.";
        showPair();
        return;
      }
      renderAll();
      setTimeout(() => { if (!S.userClosed && S.ws === ws) connectLocal(null); }, S.backoff);
      S.backoff = Math.min(S.backoff * 2, 15000);
    };
  }

  function showPair() {
    $("app").classList.add("hidden");
    $("pair").classList.remove("hidden");
    $("secret").focus();
  }
  function forget() {
    S.userClosed = true;
    try { sessionStorage.removeItem("formless_secret"); } catch (_) {}
    if (S.ws) S.ws.close();
    S.ws = null;
    history.replaceState(null, "", location.pathname);
    location.reload();
  }

  // ============================================================== wiring
  $("pair-form").addEventListener("submit", (e) => { e.preventDefault(); connect($("secret").value.trim()); });
  $("disconnect").onclick = forget;
  document.querySelectorAll("#tabs button").forEach((b) => (b.onclick = () => setView(b.dataset.view)));
  $("new-session").onclick = newSession;
  $("import-session").onclick = importSession;
  $("fork-btn").onclick = () => forkSession(S.current);
  $("export-session").onclick = () => exportSession(S.current);
  $("show-thinking").onchange = (e) => setThinking(e.target.checked);
  $("search-form").addEventListener("submit", (e) => { e.preventDefault(); search($("search").value); });
  $("search").addEventListener("search", () => { if (!$("search").value) renderSearch(null); });
  $("search").addEventListener("input", () => { if (!$("search").value) renderSearch(null); });
  $("import-tool").onclick = async () => {
    const bundle = await pickJson();
    if (bundle && (await act({ type: "import_tool", bundle }, "Imported — the Tool needs first-run approval"))) refreshTools();
  };
  $("export-tools").onclick = async () => {
    const bundle = await act({ type: "export_tools" });
    if (bundle) download("formless-tools.json", bundle);
  };
  $("optimise-all").onclick = () => act({ type: "run_optimiser", tool_id: null }, "Optimiser started");
  $("generate-proposal").onclick = () => { act({ type: "generate_proposal" }, "Generating and measuring a proposal…"); };
  $("stop-btn").onclick = stop;
  $("menu-btn").onclick = () => $("sidebar").classList.toggle("open");
  $("notes-btn").onclick = () => {
    const panel = $("notes-panel");
    panel.classList.toggle("hidden");
    if (!panel.classList.contains("hidden")) { S.unread = 0; renderStatus(); }
  };
  $("notes-close").onclick = () => $("notes-panel").classList.add("hidden");
  // Attachments (SPEC §5.6): stored by the daemon, sent with the next message.
  async function attachFiles(files) {
    if (!S.current) { note("Open a session first", true); return; }
    for (const file of files) {
      const buf = new Uint8Array(await file.arrayBuffer());
      let bin = "";
      for (let i = 0; i < buf.length; i += 0x8000) bin += String.fromCharCode.apply(null, buf.subarray(i, i + 0x8000));
      const r = await act({ type: "attach_file", session_id: S.current, name: file.name || "pasted-image.png", data_b64: btoa(bin), mime: file.type || null });
      if (r) note("attached " + r.name + " (" + Math.round(r.bytes / 1024) + " KB); it goes with your next message");
    }
  }
  $("attach-input").addEventListener("change", (e) => { attachFiles([...e.target.files]); e.target.value = ""; });
  $("input").addEventListener("paste", (e) => {
    const files = [...((e.clipboardData && e.clipboardData.files) || [])];
    if (files.length) { e.preventDefault(); attachFiles(files); }
  });
  $("composer").addEventListener("submit", (e) => { e.preventDefault(); submit(false); });
  $("input").addEventListener("input", autosize);
  $("input").addEventListener("keydown", (e) => {
    if (e.key === "Enter" && (e.ctrlKey || e.metaKey)) { e.preventDefault(); submit(true); }
    else if (e.key === "Enter" && !e.shiftKey && !e.isComposing) { e.preventDefault(); submit(false); }
    else if (e.key === "Escape") {
      if (S.editMessageId) cancelEdit();
      else stop();
    }
  });
  document.addEventListener("click", (e) => {
    if (popover && !popover.contains(e.target)) closePopover();
    const sb = $("sidebar");
    if (sb.classList.contains("open") && !sb.contains(e.target) && e.target !== $("menu-btn")) closeSidebar();
  });
  // Keyboard shortcuts (SPEC §10.5): one table drives both the handler
  // and the `?` overlay. Keys mirror the TUI where a browser allows: Tab
  // stays focus navigation, so views move with 1–5 and [ ].
  const VIEWS = ["chat", "tools", "metrics", "proposals", "settings"];
  const selectedTool = () => S.tools.find((t) => t.id === S.selectedTool);
  function moveSelection(delta) {
    if (S.view === "tools") {
      const list = S.tools;
      if (!list.length) return;
      const i = list.findIndex((t) => t.id === S.selectedTool);
      selectTool(list[Math.max(0, Math.min(list.length - 1, (i < 0 ? 0 : i + delta)))].id);
    } else {
      const list = S.sessions;
      if (!list.length) return;
      const i = list.findIndex((s) => s.id === S.current);
      openSession(list[Math.max(0, Math.min(list.length - 1, (i < 0 ? 0 : i + delta)))].id);
    }
  }
  const SHORTCUTS = [
    { keys: ["/"], help: "focus the message box", run: () => { if (S.view !== "chat") setView("chat"); $("input").focus(); } },
    { keys: ["?"], help: "this help", run: () => showShortcuts() },
    { keys: ["1", "2", "3", "4", "5"], help: "Chat · Tools · Metrics · Proposals · Settings", run: (k) => setView(VIEWS[Number(k) - 1]) },
    { keys: ["[", "]"], help: "previous / next view (TUI Shift+Tab / Tab)", run: (k) => setView(VIEWS[(VIEWS.indexOf(S.view) + (k === "]" ? 1 : VIEWS.length - 1)) % VIEWS.length]) },
    { keys: ["j", "ArrowDown"], help: "next Session (Chat) or Tool (Tools)", run: () => moveSelection(1) },
    { keys: ["k", "ArrowUp"], help: "previous Session or Tool", run: () => moveSelection(-1) },
    { keys: ["n"], help: "new Session", run: () => newSession() },
    { keys: ["x"], view: "tools", help: "retire / restore the selected Tool", run: () => { const t = selectedTool(); if (t) act({ type: t.retired ? "restore_tool" : "retire_tool", id: t.id }, (t.retired ? "Restored " : "Retired ") + t.name); } },
    { keys: ["b"], view: "tools", help: "roll the selected Tool back one version", run: () => { const t = selectedTool(); if (t && t.current_version > 1) rollbackTool(t, t.current_version - 1); } },
    { keys: ["e"], view: "tools", help: "export the selected Tool", run: () => { const t = selectedTool(); if (t) exportTool(t); } },
    { keys: ["o"], view: "tools", help: "optimise the selected Tool", run: () => { const t = selectedTool(); if (t) act({ type: "run_optimiser", tool_id: t.id }, "Optimiser started for " + t.name); } },
    { keys: ["p"], view: "tools", help: "purge the selected Tool (typed confirmation)", run: () => { const t = selectedTool(); if (t) purgeTool(t); } },
  ];
  function showShortcuts() {
    const keyName = (k) => (k === "ArrowDown" ? "↓" : k === "ArrowUp" ? "↑" : k);
    const lines = SHORTCUTS.map((s) => s.keys.map(keyName).join(" ") + "  —  " + (s.view ? "(" + s.view + ") " : "") + s.help);
    lines.push("", "Enter send · Shift+Enter newline · Ctrl/⌘+Enter queue · Esc close or stop", "Approvals: y always · o once · n deny");
    ask({ title: "Keyboard shortcuts", text: lines.join("\n"), ok: "Close" });
  }
  const typingIn = (el) => el && (el.tagName === "INPUT" || el.tagName === "TEXTAREA" || el.tagName === "SELECT" || el.isContentEditable);

  document.addEventListener("keydown", (e) => {
    const modalOpen = !$("modal-backdrop").classList.contains("hidden");
    if (modalOpen && !e.ctrlKey && !e.metaKey && !e.altKey) {
      if (e.key === "y") { e.preventDefault(); decide(true); return; }
      if (e.key === "o" && S.approvals[0] && S.approvals[0].kind !== "promotion") { e.preventDefault(); decide(true, "once"); return; }
      if (e.key === "n") { e.preventDefault(); decide(false); return; }
    }
    const dialogOpen = modalOpen || !$("ask-backdrop").classList.contains("hidden");
    if (!dialogOpen && !e.ctrlKey && !e.metaKey && !e.altKey && !typingIn(document.activeElement) && S.channel) {
      const sc = SHORTCUTS.find((s) => s.keys.includes(e.key) && (!s.view || s.view === S.view));
      if (sc) { e.preventDefault(); sc.run(e.key); return; }
    }
    if (e.key === "Escape") {
      if (!$("ask-backdrop").classList.contains("hidden")) closeAsk(null);
      else if (popover) closePopover();
      else if (!$("notes-panel").classList.contains("hidden")) $("notes-panel").classList.add("hidden");
      else closeSidebar();
    }
  });
  // Tick elapsed timers (activity + running Tools) once a second.
  setInterval(() => {
    if (isRunning() || S.liveTools.some((t) => t.status === "running")) { renderStatus(); renderLive(); }
  }, 1000);
  // Relative times drift; refresh the sidebar each minute.
  setInterval(renderSessions, 60000);

  (function init() {
    if (document.querySelector('meta[name="formless-local"]')) {
      connectLocal(location.hash.startsWith("#local=") ? decodeURIComponent(location.hash.slice(7)) : null);
      return;
    }
    let fromHash = "";
    if (location.hash.startsWith("#k=")) fromHash = decodeURIComponent(location.hash.slice(3));
    let saved = "";
    try { saved = sessionStorage.getItem("formless_secret") || ""; } catch (_) {}
    const secret = fromHash || saved;
    if (secret) { $("secret").value = secret; connect(secret); }
    else showPair();
  })();
})();
