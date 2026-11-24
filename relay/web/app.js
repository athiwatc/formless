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
      else if (k === "text") node.textContent = typeof v === "number" ? uiFormat(v) : v;
      // CSSOM, not a style attribute: the CSP has no 'unsafe-inline'.
      else if (k === "style") node.style.cssText = v;
      else if (k.startsWith("on")) node.addEventListener(k.slice(2), v);
      else node.setAttribute(k, v === true ? "" : String(v));
    }
    configureTextInput(node);
    for (const kid of kids.flat(Infinity)) {
      if (kid == null || kid === false) continue;
      node.append(kid instanceof Node ? kid : typeof kid === "number" ? uiFormat(kid) : String(kid));
    }
    return node;
  }

  /** Machine-readable fields must retain exactly what the user types. */
  function configureTextInput(node) {
    if (node.tagName !== "INPUT" && node.tagName !== "TEXTAREA") return;
    const machine = ["password", "search", "url", "email"].includes(node.type)
      || node.getAttribute("spellcheck") === "false" || node.classList.contains("editor");
    if (!machine) return;
    if (!node.hasAttribute("autocapitalize")) node.setAttribute("autocapitalize", "none");
    if (!node.hasAttribute("autocorrect")) node.setAttribute("autocorrect", "off");
    if (!node.hasAttribute("inputmode")) node.setAttribute("inputmode", ["url", "email", "search"].includes(node.type) ? node.type : "text");
    if (node.tagName === "TEXTAREA" && !node.hasAttribute("enterkeyhint")) node.setAttribute("enterkeyhint", "enter");
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
      try { localStorage.setItem("formless." + k, JSON.stringify(v)); return true; } catch (_) { return false; }
    },
    remove(k) {
      try { localStorage.removeItem("formless." + k); return true; } catch (_) { return false; }
    },
  };

  function outboxStorageKey(scope) { return "outbox:" + encodeURIComponent(scope); }
  function draftStorageKey(scope, sessionId) { return "draft:" + encodeURIComponent(scope) + ":" + encodeURIComponent(sessionId); }
  function attachmentStorageKey(scope, sessionId) { return "attachment-inventory:" + encodeURIComponent(scope) + ":" + encodeURIComponent(sessionId); }
  function sessionCacheKey(scope, sessionId) {
    return (scope || "") + "\u0000" + sessionId;
  }
  function outboxLockName(scope) { return "formless-outbox:" + encodeURIComponent(scope); }
  function sessionStorageKey(scope) { return scope ? "session:" + encodeURIComponent(scope) : "session"; }
  function loadDraft(scope, sessionId) {
    if (!scope || !sessionId) return "";
    const fallbackKey = sessionCacheKey(scope, sessionId);
    if (S.unpersistedDrafts && S.unpersistedDrafts.has(fallbackKey)) return S.unpersistedDrafts.get(fallbackKey);
    const value = prefs.get(draftStorageKey(scope, sessionId), "");
    return typeof value === "string" ? value : "";
  }
  function normalizeTextareaValue(value) {
    return typeof value === "string" ? value.replace(/\r\n?/g, "\n") : value;
  }
  function sharedSlashDraftKey(scope, sessionId) {
    return scope && sessionId ? "shared-slash-draft:" + encodeURIComponent(scope) + ":" + encodeURIComponent(sessionId) : null;
  }
  function sharedSlashDraftValue(scope, sessionId) {
    const key = sharedSlashDraftKey(scope, sessionId);
    if (!key) return null;
    if (S.sharedSlashDrafts?.has(key)) return S.sharedSlashDrafts.get(key);
    const value = prefs.get(key, null);
    return typeof value === "string" ? value : null;
  }
  function rememberSharedSlashDraft(scope, sessionId, value) {
    const key = sharedSlashDraftKey(scope, sessionId);
    if (!key) return false;
    const normalized = normalizeTextareaValue(value);
    (S.sharedSlashDrafts || (S.sharedSlashDrafts = new Map())).set(key, normalized);
    return prefs.set(key, normalized);
  }
  function clearSharedSlashDraft(scope, sessionId) {
    const key = sharedSlashDraftKey(scope, sessionId);
    if (!key) return false;
    if (S.sharedSlashDrafts) S.sharedSlashDrafts.delete(key);
    return prefs.remove(key);
  }
  function clearEditedSharedSlashDraft(scope, sessionId, value) {
    const marked = sharedSlashDraftValue(scope, sessionId);
    if (marked !== null && normalizeTextareaValue(marked) !== normalizeTextareaValue(value)) {
      clearSharedSlashDraft(scope, sessionId);
    }
  }
  function isSharedSlashDraft(scope, sessionId, value) {
    const marked = sharedSlashDraftValue(scope, sessionId);
    return marked !== null && normalizeTextareaValue(marked) === normalizeTextareaValue(value);
  }
  function clearSubmittedDraft(scope, sessionId, snapshot) {
    clearSharedSlashDraft(scope, sessionId);
    const fallbackKey = sessionCacheKey(scope, sessionId);
    const fallbacks = S.unpersistedDrafts || (S.unpersistedDrafts = new Map());
    const sharedValues = S.unpersistedDraftSharedValues || (S.unpersistedDraftSharedValues = new Map());
    const hasFallback = fallbacks.has(fallbackKey);
    const fallback = hasFallback ? fallbacks.get(fallbackKey) : null;
    const fallbackIsSubmitted = hasFallback && normalizeTextareaValue(fallback) === snapshot;
    const hasSharedBaseline = hasFallback && sharedValues.has(fallbackKey);
    const sharedBaseline = hasSharedBaseline ? sharedValues.get(fallbackKey) : null;
    const key = draftStorageKey(scope, sessionId);
    const shared = prefs.get(key, null);
    if (fallbackIsSubmitted) {
      fallbacks.delete(fallbackKey);
      sharedValues.delete(fallbackKey);
    }
    const sharedMatchesSnapshot = typeof shared === "string" && normalizeTextareaValue(shared) === snapshot;
    const sharedIsUnchanged = hasSharedBaseline && typeof shared === "string" && Object.is(shared, sharedBaseline);
    const shouldRemoveShared = typeof shared === "string" && (fallbackIsSubmitted && hasSharedBaseline ? sharedIsUnchanged : sharedMatchesSnapshot);
    if (shouldRemoveShared) {
      const removed = typeof prefs.remove === "function" ? prefs.remove(key) : prefs.set(key, "");
      if (!removed && !fallbacks.has(fallbackKey)) {
        fallbacks.set(fallbackKey, "");
        sharedValues.set(fallbackKey, shared);
      }
    }
    const latest = loadDraft(scope, sessionId);
    if (scope === S.storageScope) {
      if (latest) S.drafts[sessionId] = latest;
      else delete S.drafts[sessionId];
    }
    return latest;
  }
  function loadOutbox(scope) {
    if (!scope) return [];
    const saved = prefs.get(outboxStorageKey(scope), []);
    if (!Array.isArray(saved)) return [];
    return saved.filter((item) => item && item.scope === scope && typeof item.session_id === "string"
      && typeof item.content === "string" && typeof item.client_msg_id === "string"
      && item.client_msg_id && ["send", "steer", "queue"].includes(item.mode))
      .map((item) => {
        const validInventory = Array.isArray(item.expected_attachments);
        let status = ["queued", "sending", "failed"].includes(item.status) ? item.status : "queued";
        let error = typeof item.error === "string" ? item.error : "";
        if (!validInventory) { status = "failed"; error = "Attachment inventory is unavailable; review this message before sending."; }
        if (item.mode === "steer" && (typeof item.expected_run_id !== "string" || !item.expected_run_id)) {
          status = "failed"; error = "The original Run could not be identified; this message was not sent.";
        }
        return {
          scope,
          order: Number.isFinite(item.order) ? item.order : 0,
          session_id: item.session_id,
          content: item.content,
          mode: item.mode,
          expected_attachments: validInventory ? item.expected_attachments : null,
          expected_run_id: typeof item.expected_run_id === "string" ? item.expected_run_id : null,
          forecast_pending: item.forecast_pending === true,
          client_msg_id: item.client_msg_id,
          attempted: item.attempted !== false,
          dispatch_kind: item.dispatch_kind === "durable" ? "durable" : null,
          status,
          error,
        };
      })
      .sort((a, b) => a.order - b.order || a.client_msg_id.localeCompare(b.client_msg_id));
  }

  // While an IME is composing (Japanese, Chinese, Korean…), Enter and
  // other keys confirm candidates: no shortcut, palette, mention or form
  // handler may act on them (#792). One capture-phase guard on window
  // runs before every other keydown handler.
  window.addEventListener("keydown", (e) => {
    if (e.isComposing || e.keyCode === 229) e.stopImmediatePropagation();
  }, true);

  // ---------------------------------------------------------------- state
  const S = {
    ws: null,
    key: null, // HKDF root key from the pairing secret
    hello: null, // this connection's handshake nonce
    channel: null, // { key, sent, received } once the handshake is done
    sendChain: Promise.resolve(),
    recvChain: Promise.resolve(),
    relayModule: null,
    relayMemory: null, // shared across reconnects; old async consumers stay charged
    relayLink: null,
    relayLocalProof: false,
    relayRoomReady: false,
    relayReady: false,
    secret: "",
    userClosed: false,
    backoff: 1000,
    daemonOnline: false,
    /** Pending attachments by Session (shown as chips until sent). */
    chips: {},
    view: prefs.get("view", "chat"),
    sessions: [],
    topics: [],
    mainChat: null,
    topicsAvailable: false,
    topicsUnsupported: false,
    topicRevision: 0,
    topicFocus: null,
    current: null,
    pullId: null,
    drafts: {},
    unpersistedDrafts: new Map(),
    unpersistedDraftSharedValues: new Map(),
    attachmentInventories: {},
    outbox: [],
    outboxFlushing: false,
    outboxFlushRequested: false,
    messages: [],
    sessionLoad: null, // { id, status: pending | failed | loaded }
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
    toolDetailError: null, // id of the selected Tool whose detail fetch failed
    editingCode: null, // { toolId, code }
    metricTool: null,
    modelStats: [],
    proposals: [],
    settings: null,
    status: null,
    approvalHelpSeen: undefined,
    notes: [],
    unread: 0,
    showThinking: prefs.get("thinking", true),
    enterSends: prefs.get("enterSends", null),
    follow: true,
    editMessageId: null,
    storageScope: null,
    nextId: Math.floor(Math.random() * 1e9) + 1,
    submitting: new Set(),
    shareApplyPending: false,
    pending: new Map(),
    subagents: [], // last /agents listing
  };

  function saveDraft(sessionId, value, scope = S.storageScope) {
    if (!sessionId) return true;
    S.drafts = S.drafts || {};
    if (scope === S.storageScope) {
      if (value) S.drafts[sessionId] = value;
      else delete S.drafts[sessionId];
    }
    if (!scope) return true;
    const key = draftStorageKey(scope, sessionId);
    S.unpersistedDrafts = S.unpersistedDrafts || new Map();
    S.unpersistedDraftSharedValues = S.unpersistedDraftSharedValues || new Map();
    const fallbackKey = sessionCacheKey(scope, sessionId);
    const hasFallback = S.unpersistedDrafts.has(fallbackKey);
    const sharedValue = hasFallback ? null : prefs.get(key, null);
    const saved = value ? prefs.set(key, value) : typeof prefs.remove === "function" ? prefs.remove(key) : prefs.set(key, "");
    if (saved) {
      S.unpersistedDrafts.delete(fallbackKey);
      S.unpersistedDraftSharedValues.delete(fallbackKey);
    } else {
      S.unpersistedDrafts.set(fallbackKey, value);
      if (!hasFallback) S.unpersistedDraftSharedValues.set(fallbackKey, sharedValue);
    }
    return saved;
  }
  function restoreDraft(sessionId) {
    const fallbackKey = sessionCacheKey(S.storageScope, sessionId);
    const value = S.unpersistedDrafts && S.unpersistedDrafts.has(fallbackKey)
      ? S.unpersistedDrafts.get(fallbackKey)
      : loadDraft(S.storageScope, sessionId);
    S.drafts[sessionId] = value;
    $("input").value = value;
    autosize();
  }
  function persistOutbox(scope, entries) {
    return !!scope && prefs.set(outboxStorageKey(scope), entries);
  }
  function hasOutboxLock() {
    return typeof navigator !== "undefined" && !!navigator.locks && typeof navigator.locks.request === "function";
  }
  function withOutboxLock(scope, action) {
    if (!scope || !hasOutboxLock()) return Promise.resolve(false);
    return navigator.locks.request(outboxLockName(scope), action);
  }
  function refreshOutbox(scope = S.storageScope) {
    if (!scope) return;
    const entries = loadOutbox(scope);
    if (scope === S.storageScope) {
      S.outbox = entries;
      renderOutbox();
    }
    return entries;
  }
  function activateStorageScope() {
    const scope = S.local ? "local:" + location.origin : S.room ? "relay:" + S.room : null;
    if (!scope) return false;
    if (scope === S.storageScope) { refreshOutbox(scope); return true; }
    if (S.current) saveDraft(S.current, $("input").value);
    S.storageScope = scope;
    S.drafts = {};
    S.attachmentInventories = {};
    S.chips = {};
    S.outbox = loadOutbox(scope);
    if (S.current) {
      S.current = null;
      $("input").value = "";
      autosize();
      renderChips();
    }
    renderOutbox();
    return true;
  }
  function loadAttachmentInventory(scope, sessionId) {
    const value = scope && prefs.get(attachmentStorageKey(scope, sessionId), null);
    return Array.isArray(value) ? value : null;
  }
  function saveAttachmentInventory(scope, sessionId, attachments) {
    if (!scope || !sessionId || !Array.isArray(attachments)) return false;
    S.attachmentInventories[sessionId] = attachments;
    S.chips[sessionId] = attachments;
    return prefs.set(attachmentStorageKey(scope, sessionId), attachments);
  }
  async function refreshAttachmentInventory(sessionId, scope = S.storageScope, requireFresh = false) {
    if (!scope || !sessionId) return null;
    if (scope !== S.storageScope) return requireFresh ? null : loadAttachmentInventory(scope, sessionId);
    if (S.daemonOnline && S.channel) {
      try {
        const result = await request({ type: "list_pending_attachments", session_id: sessionId });
        if (scope !== S.storageScope) return null;
        const attachments = Array.isArray(result && result.attachments) ? result.attachments : null;
        if (attachments) {
          saveAttachmentInventory(scope, sessionId, attachments);
          if (S.current === sessionId) renderChips();
          return attachments;
        }
      } catch (_) {}
    }
    if (requireFresh) return null;
    const cached = S.attachmentInventories[sessionId] || loadAttachmentInventory(scope, sessionId);
    if (cached) {
      S.attachmentInventories[sessionId] = cached;
      S.chips[sessionId] = cached;
      if (S.current === sessionId) renderChips();
    }
    return cached;
  }
  async function fetchFreshAttachmentInventory(sessionId, scope = S.storageScope) {
    if (!scope || !sessionId) return null;
    const result = await request({ type: "list_pending_attachments", session_id: sessionId });
    if (scope !== S.storageScope) return null;
    if (!Array.isArray(result && result.attachments)) {
      const error = new Error("The daemon did not return a pending attachment inventory.");
      error.daemonResponse = true;
      throw error;
    }
    const attachments = result.attachments;
    saveAttachmentInventory(scope, sessionId, attachments);
    if (S.current === sessionId) renderChips();
    return attachments;
  }
  function renderOutbox() {
    const box = $("outbox");
    if (!box) return;
    const entries = S.outbox || [];
    const hidden = entries.length === 0;
    box.hidden = hidden;
    box.classList.toggle("hidden", hidden);
    box.replaceChildren(...entries.map((item) => {
      const status = item.status === "sending" ? "Sending" : item.status === "failed" ? "Failed" : "Queued";
      return h("div", { class: "outbox-item", "data-status": item.status },
        h("div", { class: "outbox-entry" },
          h("strong", { class: "outbox-status" }, status),
          h("div", { class: "outbox-content" }, item.content),
          item.error ? h("div", { class: "outbox-error" }, item.error) : null),
          h("button", { type: "button", class: "btn ghost", "aria-label": "Discard " + status.toLowerCase() + " message", onclick: () => discardOutbox(item) }, "Discard"));
    }));
  }
  async function enqueueOutbox(sessionId, content, mode, { expectedAttachments, expectedRunId, forecastPending = false, blockedReason = "", queuedReason = "", storageScope = S.storageScope } = {}) {
    if (!sessionId || !content || !["send", "steer", "queue"].includes(mode) || !storageScope || !hasOutboxLock()) return null;
    const scope = storageScope;
    const clientMsgId = crypto.randomUUID ? crypto.randomUUID() : String(Date.now()) + Math.random().toString(16).slice(2);
    const inventory = Array.isArray(expectedAttachments) ? expectedAttachments : null;
    const error = blockedReason || (!inventory ? "Attachment inventory could not be verified; reconnect and review before sending." : "");
    const item = {
      scope, order: 0, session_id: sessionId, content, mode,
      expected_attachments: inventory,
      expected_run_id: expectedRunId || null,
      forecast_pending: !!forecastPending,
      client_msg_id: clientMsgId,
      attempted: false,
      dispatch_kind: null,
      status: error ? "failed" : "queued",
      error: error || queuedReason,
    };
    let saved = false;
    await withOutboxLock(scope, async () => {
      const entries = loadOutbox(scope);
      item.order = entries.reduce((max, entry) => Math.max(max, entry.order), 0) + 1;
      saved = persistOutbox(scope, [...entries, item]);
      if (saved && scope === S.storageScope) refreshOutbox(scope);
    });
    return saved ? item : null;
  }
  function canFlushOutbox(scope = S.storageScope) {
    return !!(scope && scope === S.storageScope && hasOutboxLock() && S.daemonOnline && S.channel && (S.local || S.relayReady));
  }
  async function discardOutbox(item) {
    const scope = item && item.scope;
    if (!scope) return false;
    let discarded = false;
    try {
      await withOutboxLock(scope, async () => {
        const entries = loadOutbox(scope);
        const next = entries.filter((entry) => entry.client_msg_id !== item.client_msg_id);
        if (next.length === entries.length) return;
        discarded = persistOutbox(scope, next);
        if (discarded) refreshOutbox(scope);
      });
    } catch (_) {}
    if (!discarded) {
      note("Could not save the outbox change; the message was not discarded.", true);
      return false;
    }
    if (scope === S.storageScope) void flushOutbox();
    return true;
  }
  async function flushOutbox() {
    const scope = S.storageScope;
    if (S.outboxFlushing) { S.outboxFlushRequested = true; return; }
    if (!canFlushOutbox(scope)) return;
    S.outboxFlushing = true;
    try {
      S.outboxFlushRequested = false;
      await withOutboxLock(scope, async () => {
        if (!canFlushOutbox(scope)) return;
        let entries = loadOutbox(scope);
        let recovered = false;
        for (const item of entries) if (item.status === "sending") { item.status = "queued"; item.attempted = true; recovered = true; }
        if (recovered && !persistOutbox(scope, entries)) return;
        refreshOutbox(scope);
        let durableOutboxSupported = null;
        while (canFlushOutbox(scope)) {
          entries = loadOutbox(scope);
          const item = entries[0];
          if (!item || item.status === "failed") break;
          if (item.attempted && item.dispatch_kind !== "durable") {
            item.status = "failed";
            item.error = "A previous send attempt cannot be classified as durably protected. Check the Session before resending to avoid a duplicate.";
            persistOutbox(scope, entries); refreshOutbox(scope); break;
          }
          if (durableOutboxSupported === null) {
            try {
              const ping = await request({ type: "ping" });
              durableOutboxSupported = Array.isArray(ping && ping.features) && ping.features.includes("durable_outbox_v1");
            } catch (error) {
              if (scope !== S.storageScope) break;
              item.status = error.daemonResponse ? "failed" : "queued";
              item.error = "Could not verify durable outbox support: " + error.message;
              persistOutbox(scope, entries); refreshOutbox(scope); break;
            }
          }
          if (!durableOutboxSupported) {
            item.status = "failed";
            item.error = item.attempted
              ? "This daemon does not support durable retries. The message may already have been sent; check the Session before resending."
              : "This daemon does not support the durable outbox; upgrade it, manually copy this message text, discard the saved item, and resend it; it was not sent.";
            persistOutbox(scope, entries); refreshOutbox(scope); break;
          }
          if (item.forecast_pending) {
            if (S.current !== item.session_id) {
              item.error = "Open this Session to review and approve its forecast before sending.";
              persistOutbox(scope, entries); refreshOutbox(scope); break;
            }
            let currentAttachments;
            try {
              currentAttachments = await fetchFreshAttachmentInventory(item.session_id, scope);
            } catch (error) {
              if (scope !== S.storageScope) break;
              item.status = error.daemonResponse ? "failed" : "queued";
              item.error = error.daemonResponse
                ? "Pending attachment inventory failed; this message was not sent: " + error.message
                : "Pending attachment inventory is unavailable; this message remains queued: " + error.message;
              persistOutbox(scope, entries); refreshOutbox(scope); break;
            }
            if (scope !== S.storageScope) break;
            if (!Array.isArray(currentAttachments)) break;
            if (JSON.stringify(currentAttachments) !== JSON.stringify(item.expected_attachments)) {
              item.status = "failed";
              item.error = "Pending attachments changed since this forecast was composed; review the message again before sending.";
              persistOutbox(scope, entries); refreshOutbox(scope); break;
            }
            try {
              if (!(await confirmForecast(item.session_id, item.content))) {
                item.status = S.daemonOnline && S.current === item.session_id ? "failed" : "queued";
                item.error = item.status === "failed" ? "Forecast approval was declined; this message was not sent." : "Forecast approval is pending; reconnect and review before sending.";
                persistOutbox(scope, entries); refreshOutbox(scope); break;
              }
              item.forecast_pending = false;
              item.error = "";
              if (!persistOutbox(scope, entries)) break;
            } catch (error) {
              item.status = error.daemonResponse ? "failed" : "queued";
              item.error = "Forecast approval is pending: " + error.message;
              persistOutbox(scope, entries); refreshOutbox(scope); break;
            }
          }
          if (!Array.isArray(item.expected_attachments)) {
            item.status = "failed";
            item.error = "Attachment inventory is unavailable; this message was not sent.";
            persistOutbox(scope, entries); refreshOutbox(scope); break;
          }
          item.status = "sending";
          item.error = "";
          item.attempted = true;
          item.dispatch_kind = "durable";
          if (!persistOutbox(scope, entries)) break;
          refreshOutbox(scope);
          try {
            await request({
              type: "send_outbox_message",
              session_id: item.session_id,
              content: item.content,
              mode: item.mode,
              expected_attachments: item.expected_attachments,
              expected_run_id: item.expected_run_id,
              client_msg_id: item.client_msg_id,
            });
          } catch (error) {
            const latest = loadOutbox(scope);
            const current = latest.find((entry) => entry.client_msg_id === item.client_msg_id);
            if (!current) continue;
            current.status = error.daemonResponse ? "failed" : "queued";
            current.error = error.message;
            persistOutbox(scope, latest); refreshOutbox(scope);
            if (error.daemonResponse) { toast(error.message, true); note(error.message, true); }
            break;
          }
          const remaining = loadOutbox(scope).filter((entry) => entry.client_msg_id !== item.client_msg_id);
          if (!persistOutbox(scope, remaining)) break;
          refreshOutbox(scope);
          if (scope === S.storageScope) {
            const attachments = await refreshAttachmentInventory(item.session_id, scope, true);
            const mentioned = mentionAttached[sessionCacheKey(scope, item.session_id)];
            if (Array.isArray(attachments) && mentioned) {
              const pendingPaths = new Set(attachments.map((attachment) => attachment.path).filter((path) => typeof path === "string"));
              for (const path of [...mentioned]) if (!pendingPaths.has(path)) mentioned.delete(path);
              if (!mentioned.size) delete mentionAttached[sessionCacheKey(scope, item.session_id)];
            }
          }
          if (item.mode === "steer") note("steering the original run");
        }
      });
    } finally {
      S.outboxFlushing = false;
      if (S.outboxFlushRequested && canFlushOutbox()) {
        S.outboxFlushRequested = false;
        queueMicrotask(() => { void flushOutbox(); });
      }
    }
  }

  function onStorage(event) {
    const scope = S.storageScope;
    if (!scope || !event.key) return;
    if (event.key === "formless." + outboxStorageKey(scope)) {
      refreshOutbox(scope);
      if (canFlushOutbox(scope)) void flushOutbox();
    }
    const prefix = "formless.draft:" + encodeURIComponent(scope) + ":";
    if (event.key.startsWith(prefix)) {
      const sessionId = decodeURIComponent(event.key.slice(prefix.length));
      const storedValue = typeof event.newValue === "string" ? (() => { try { return JSON.parse(event.newValue); } catch (_) { return ""; } })() : "";
      const fallbackKey = sessionCacheKey(scope, sessionId);
      let value = storedValue;
      if (S.unpersistedDrafts && S.unpersistedDrafts.has(fallbackKey)) {
        const fallback = S.unpersistedDrafts.get(fallbackKey);
        if (normalizeTextareaValue(fallback) === normalizeTextareaValue(storedValue)) {
          S.unpersistedDrafts.delete(fallbackKey);
          if (S.unpersistedDraftSharedValues) S.unpersistedDraftSharedValues.delete(fallbackKey);
        }
        else value = fallback;
      }
      const composer = $("input");
      const localDraft = S.drafts[sessionId];
      const preserveComposer = S.current === sessionId && (S.editMessageId || composer.value !== (localDraft || ""));
      if (typeof value === "string") S.drafts[sessionId] = value;
      if (S.current === sessionId && !preserveComposer && typeof value === "string") { composer.value = value; autosize(); }
    }
  }
  window.addEventListener("storage", onStorage);

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
    const key = await crypto.subtle.importKey("raw", keyBytes, "HKDF", false, ["deriveBits"]);
    const claim = new Uint8Array(await crypto.subtle.deriveBits({ name: "HKDF", hash: "SHA-256", salt: new Uint8Array(), info: new TextEncoder().encode("formless-relay/3 daemon-claim") }, key, 256));
    try {
      const digest = new Uint8Array(await crypto.subtle.digest("SHA-256", claim));
      return Array.from(digest.slice(0, 16), (b) => b.toString(16).padStart(2, "0")).join("");
    } finally { claim.fill(0); }
  }
  // Relay frame protocol v2 (must match crates/formless-core/src/relay.rs).
  const FRAME_VERSION = 2;
  const enc = (s) => new TextEncoder().encode(s);
  const aad = (direction, seq) => enc("formless-relay/2 " + direction + " " + seq);
  // The handshake (#904): each side proves it holds the pairing key with a
  // value bound to both nonces, before anything else is trusted. A lost
  // reply is asked for again (the same hello gets the same answer); a
  // handshake that never completes says so instead of looking connected.
  const HANDSHAKE_MS = 8000;
  const HANDSHAKE_TRIES = 3;
  function startHandshake(again) {
    S.channel = null;
    S.relayLocalProof = false;
    S.relayRoomReady = false;
    S.relayReady = false;
    clearTimeout(S.handshakeTimer);
    S.handshakeTries = again ? (S.handshakeTries || 0) + 1 : 1;
    S.handshakeFailed = false;
    S.hello = crypto.getRandomValues(new Uint8Array(16));
    sendHello();
  }
  function sendHello() {
    const ws = S.ws;
    if (!ws || ws.readyState !== 1 || !S.hello) return;
    ws.send(JSON.stringify({ v: FRAME_VERSION, hello: bytesToB64(S.hello) }));
    S.handshakeTimer = setTimeout(() => {
      if (S.channel || S.ws !== ws || !S.daemonOnline) return;
      if (S.handshakeTries < HANDSHAKE_TRIES) {
        S.handshakeTries++;
        sendHello();
      } else {
        S.handshakeFailed = true;
        note("Couldn't secure the connection: the daemon didn't answer the handshake. Reload to try again, or check that the pairing secret is current.", true);
        renderChrome();
      }
    }, HANDSHAKE_MS);
  }
  /** The proof `from` ("b2d" or "d2b") gives for this handshake. */
  async function confirmTag(daemonHello, from) {
    const salt = new Uint8Array(32);
    salt.set(S.hello, 0);
    salt.set(daemonHello, 16);
    return new Uint8Array(await crypto.subtle.deriveBits(
      { name: "HKDF", hash: "SHA-256", salt, info: enc("formless-relay/2 confirm " + from) }, S.key, 256));
  }
  async function finishHandshake(daemonHello) {
    const ws = S.ws, hello = S.hello, pairingKey = S.key;
    const salt = new Uint8Array(32);
    salt.set(hello, 0);
    salt.set(daemonHello, 16);
    const key = await crypto.subtle.deriveKey(
      { name: "HKDF", hash: "SHA-256", salt, info: enc("formless-relay/2") },
      pairingKey, { name: "AES-GCM", length: 256 }, false, ["encrypt", "decrypt"]);
    if (S.ws !== ws || S.hello !== hello || S.key !== pairingKey) return false;
    S.channel = { key, sent: 0, received: 0 };
    return true;
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
    // Exactly the next one: a gap means a frame went missing (#905).
    if (!ch || v.v !== FRAME_VERSION || v.seq !== ch.received + 1) throw new Error("rejected frame");
    const pt = await crypto.subtle.decrypt(
      { name: "AES-GCM", iv: b64ToBytes(v.iv), additionalData: aad("d2b", v.seq) }, ch.key, b64ToBytes(v.ct));
    if (pt.byteLength > (S.relayModule ? S.relayModule.LIMITS.plaintext : 32 * 1024 * 1024)) throw new Error("relay plaintext exceeds IPC limit");
    ch.received = v.seq;
    return new TextDecoder().decode(pt);
  }

  // ------------------------------------------------------------ transport
  /** Request with a response (rejects on daemon error or timeout). */
  function request(req, timeoutMs) {
    return new Promise((resolve, reject) => {
      if (!S.ws || S.ws.readyState !== 1) return reject(new Error("not connected to the relay"));
      if (!S.channel || (!S.local && !S.relayReady)) return reject(new Error("still connecting to the daemon"));
      if (S.pending.size >= 64) return reject(new Error("too many pending requests; wait for an answer"));
      const id = S.nextId++;
      let text, lease;
      try {
        text = JSON.stringify({ id, protocol: PROTOCOL, request: req });
        if (!S.local) {
          const bytes = S.relayModule.wireBytes(text, S.relayModule.LIMITS.plaintext);
          // Known serialization/UTF8/GCM/base64/string copies and bounded
          // fragment lookahead. This is not a DOM/application-state RSS cap.
          lease = S.relayMemory.reserve(12 * bytes + 1024 * 1024 + 4 * S.relayModule.LIMITS.windowFrames * S.relayModule.LIMITS.wire + 4096);
        }
      } catch (error) { return reject(error); }
      const timer = setTimeout(() => {
        S.pending.delete(id);
        reject(new Error("the daemon did not answer (is it online?)"));
      }, timeoutMs || 30000);
      S.pending.set(id, {
        resolve: (v) => { clearTimeout(timer); resolve(v); },
        reject: (e) => { clearTimeout(timer); reject(e); },
      });
      const ws = S.ws;
      S.sendChain = S.sendChain.then(async () => {
        // A timed-out queued intent must never be transmitted later.
        if (S.ws !== ws || !S.pending.has(id)) return;
        if (S.local) ws.send(text);
        else {
          const frame = await encrypt(text);
          // Encryption's transient copies are gone. Retain only the actual
          // encoded string plus bounded fragment/native-send lookahead; the
          // discarded plaintext must not keep the peak reservation alive
          // while a full-size response arrives before our final credit.
          text = null;
          lease.shrink(2 * frame.length + 4 * S.relayModule.LIMITS.windowFrames * S.relayModule.LIMITS.wire + 4096);
          if (S.ws !== ws || !S.relayLink || S.relayLink.ws !== ws) throw new Error("relay connection changed");
          await S.relayLink.endpoint.start(frame, lease);
        }
      }).catch((e) => {
        S.pending.delete(id); clearTimeout(timer); reject(e);
        if (!S.local && S.ws === ws && ws.readyState === 1) { try { ws.close(4002, "relay transport failed"); } catch (_) {} }
      }).finally(() => { if (lease) lease.release(); });
    });
  }
  /** Request that reports failures as a toast; resolves null on error. */
  async function act(req, okText, timeoutMs, retry) {
    try {
      const r = await request(req, timeoutMs);
      if (okText) toast(okText);
      return r;
    } catch (e) {
      toast(e.message, true, retry ? { label: "Retry", run: retry } : null);
      note(e.message, true);
      return null;
    }
  }

  // ----------------------------------------------------- strings (§10.8)
  // Keyed by the English text, like the TUI's catalog: anything missing
  // shows in English. `tui.language` picks it; `auto` uses the browser's.
  const I18N = {
    th: {
      "First approval? Tools run in a sandbox with no access at all; each Capability below (files, network, programs…) is something this Tool asks to use. Always lets it keep using them, Allow once covers just this call, This Session covers the current Session, and Deny says no. Change your mind any time: /approvals lists what you allowed, /revoke takes it back.": "อนุมัติครั้งแรกใช่ไหม เครื่องมือทำงานแยกและไม่มีสิทธิ์เข้าถึงอะไรโดยค่าเริ่มต้น รายการด้านล่างคือสิทธิ์ที่เครื่องมือนี้ขอ เช่น ไฟล์ เครือข่าย หรือโปรแกรม อนุญาตเสมอให้ใช้สิทธิ์ต่อไป อนุญาตครั้งเดียวใช้ได้สำหรับการเรียกนี้ เซสชันนี้ใช้ได้จนจบเซสชัน และปฏิเสธคือไม่อนุญาต ใช้ /approvals เพื่อดูสิทธิ์ที่อนุญาต และ /revoke เพื่อถอนอนุญาต",
      "Approval required": "ต้องอนุมัติ",
      "Promotion approval": "อนุมัติการเลื่อนเวอร์ชัน",
      "Approve": "อนุมัติ",
      "Always": "อนุญาตเสมอ",
      "Allow once": "อนุญาตครั้งเดียว",
      "This Session": "เซสชันนี้",
      "For 1 hour": "เป็นเวลา 1 ชั่วโมง",
      "Deny": "ปฏิเสธ",
      "Always deny": "ปฏิเสธเสมอ",
      "Later": "ไว้ทีหลัง",
      "Approve lasts for this Tool until its capabilities widen. Allow once covers this call only.": "การอนุญาตมีผลกับเครื่องมือนี้จนกว่าจะขอสิทธิ์เพิ่ม อนุญาตครั้งเดียวมีผลเฉพาะการเรียกนี้",
      "The new version replaces the current one; rollback stays available.": "เวอร์ชันใหม่จะแทนที่เวอร์ชันปัจจุบัน คุณยังย้อนกลับได้",
      "Send": "ส่ง", "Steer": "ปรับทิศ", "Re-run": "ส่งใหม่", "Stop": "หยุด",
      "Message Formless…": "พิมพ์ข้อความถึง Formless…",
      "Steer, or Ctrl/⌘+Enter to queue…": "ปรับทิศ หรือ Ctrl/⌘+Enter เพื่อรอส่ง…",
      "Recent retained calls": "การเรียกใช้ล่าสุดที่เก็บไว้", "Succeeded": "สำเร็จ", "Failed": "ล้มเหลว",
      "Recent sample trend": "แนวโน้มตัวอย่างล่าสุด", "up": "เพิ่มขึ้น", "down": "ลดลง", "no arrow": "ไม่มีลูกศร",
      "Resume": "ทำต่อ", "Abandon": "ทิ้ง", "Retry": "ลองใหม่", "Retry with…": "ลองใหม่กับโมเดล…",
      "Turn started": "เริ่มรอบการสนทนา",
      "Run": "ทำ", "Edit": "แก้", "Skip": "ข้าม", "Run the rest": "ทำที่เหลือทั้งหมด",
      "What I did": "สิ่งที่ทำ", "Plan": "แผน", "Plan done": "แผนเสร็จแล้ว",
      "Queued for after this Run": "รอส่งหลังงานนี้",
      "thinking": "กำลังคิด", "responding": "กำลังตอบ", "waiting for model": "รอโมเดล",
      "provider stalled": "ผู้ให้บริการไม่ตอบสนอง", "awaiting your approval": "รอการอนุมัติจากคุณ",
      "awaiting your answer": "รอคำตอบจากคุณ", "running": "กำลังทำงาน",
      "just now": "เมื่อกี้", "m ago": " นาทีที่แล้ว", "h ago": " ชม.ที่แล้ว", "d ago": " วันที่แล้ว",
      "Session": "เซสชัน", "Run control": "ควบคุมการทำงาน", "Tools": "เครื่องมือ",
      "Integrations": "การเชื่อมต่อ", "Settings": "การตั้งค่า", "App": "แอป",
      "start a new Session, optionally from a template": "เริ่มเซสชันใหม่ (เลือกจากเทมเพลตได้)",
      "stop the current Run (Esc)": "หยุดการทำงานปัจจุบัน (Esc)",
      "continue an interrupted or paused Run": "ทำงานต่อจากที่ถูกขัดจังหวะหรือหยุดไว้",
      "the files the last Run changed, with diffs": "ไฟล์ที่งานล่าสุดแก้ไข พร้อมส่วนที่เปลี่ยน",
      "step mode: pause before each Tool call": "โหมดทีละขั้น: หยุดก่อนเรียกเครื่องมือแต่ละครั้ง",
      "the same prompt on two models, side by side with time and cost": "ถามคำถามเดียวกันกับสองโมเดล เทียบคำตอบ เวลา และค่าใช้จ่าย",
      "clean up: shows what it frees first": "ล้างข้อมูล: แสดงก่อนว่าจะคืนพื้นที่เท่าไร",
    },
  };
  function uiLang() {
    const set = S.settings && S.settings.tui && S.settings.tui.language;
    const l = set && set !== "auto" ? set : (navigator.language || "en");
    return String(l).toLowerCase().startsWith("th") ? "th" : "en";
  }
  /** `s` in the UI language (English when there is no translation). */
  function tr(s) {
    const t = I18N[uiLang()];
    return (t && t[s]) || s;
  }
  /** Match the TUI/ISO calendar and digits, independently of browser defaults. */
  function uiLocale() {
    return uiLang() === "th" ? "th-TH-u-ca-gregory-nu-latn" : "en-US-u-ca-gregory-nu-latn";
  }
  /** The sole localized date/number formatter; machine values stay ASCII. */
  function uiFormat(value, options = {}) {
    const valid = value instanceof Date ? Number.isFinite(value.getTime()) : typeof value === "number" && Number.isFinite(value);
    return valid ? value.toLocaleString(uiLocale(), options) : "—";
  }
  function uiFixed(value, digits) {
    return uiFormat(value, { minimumFractionDigits: digits, maximumFractionDigits: digits, useGrouping: false });
  }
  function uiMoney(value, digits = 3) {
    return uiFormat(value, { style: "currency", currency: "USD", minimumFractionDigits: digits ?? 0, maximumFractionDigits: digits ?? 20 });
  }
  function applyLanguage() {
    const language = uiLang(), changed = document.documentElement.lang !== language;
    document.documentElement.lang = language;
    return changed;
  }
  applyLanguage();
  window.addEventListener("languagechange", () => { applyLanguage(); renderAll(); });

  // -------------------------------------------------------------- helpers
  function relTime(ts) {
    const t = Date.parse(ts);
    if (isNaN(t)) return ts || "";
    const s = Math.max(0, (Date.now() - t) / 1000);
    if (s < 60) return tr("just now");
    if (s < 3600) return uiFormat(Math.floor(s / 60)) + tr("m ago");
    if (s < 86400) return uiFormat(Math.floor(s / 3600)) + tr("h ago");
    if (s < 604800) return uiFormat(Math.floor(s / 86400)) + tr("d ago");
    return uiFormat(new Date(t), { year: "numeric", month: "numeric", day: "numeric" });
  }
  /** "in 42m": how long until a future time. */
  function untilTime(ts) {
    const s = (Date.parse(ts) - Date.now()) / 1000;
    if (isNaN(s)) return ts || "";
    if (s <= 0) return "ended";
    if (s < 60) return "in less than 1m";
    const minutes = Math.floor(s / 60);
    const hours = Math.floor(minutes / 60), remainder = minutes % 60;
    return "in " + (hours ? uiFormat(hours) + "h" + (remainder ? " " : "") : "")
      + (!hours || remainder ? uiFormat(remainder) + "m" : "");
  }
  function clock(ts) {
    const t = typeof ts === "number" ? ts : Date.parse(ts);
    return Number.isFinite(t) ? uiFormat(new Date(t), { hour: "2-digit", minute: "2-digit" }) : "";
  }
  function turnTimestamp(ts, today) {
    if (typeof ts !== "number") {
      // Stored times use RFC 3339. Guard against Date treating incomplete
      // strings as dates or rolling an impossible day into the next month.
      const parts = typeof ts === "string" && /^(\d{4})-(\d\d)-(\d\d)[T ](\d\d):(\d\d):(\d\d)(?:\.\d+)?(?:Z|[+-]\d\d:\d\d)$/i.exec(ts);
      if (!parts) return null;
      const [year, month, day, hour, minute, second] = parts.slice(1).map(Number);
      if (year < 1 || month < 1 || month > 12 || day < 1 || hour > 23 || minute > 59 || second > 59) return null;
      const monthEnd = new Date(0);
      monthEnd.setUTCFullYear(year, month, 0);
      if (day > monthEnd.getUTCDate()) return null;
    }
    const date = new Date(ts);
    const year = date.getUTCFullYear();
    if (!Number.isFinite(year) || year < 1 || year > 9999) return null;
    const clockOptions = { hour: "2-digit", minute: "2-digit" };
    const visible = uiFormat(date, date.toDateString() === today
      ? clockOptions : { ...clockOptions, year: "numeric", month: "short", day: "numeric" });
    const full = tr("Turn started") + " " + uiFormat(date, {
      ...clockOptions, year: "numeric", month: "long", day: "numeric", second: "2-digit", timeZoneName: "shortOffset",
    });
    return h("time", { datetime: date.toISOString() },
      h("span", { "aria-hidden": "true" }, visible), h("span", { class: "sr-only" }, full));
  }
  function elapsed(since) {
    const s = Math.floor((Date.now() - since) / 1000);
    if (s >= 3600) {
      const pad = { minimumIntegerDigits: 2, useGrouping: false };
      return uiFormat(Math.floor(s / 3600)) + "h" + uiFormat(Math.floor(s % 3600 / 60), pad) + "m" + uiFormat(s % 60, pad) + "s";
    }
    return s < 60 ? uiFormat(s) + "s" : uiFormat(Math.floor(s / 60)) + "m" + uiFormat(s % 60, { minimumIntegerDigits: 2, useGrouping: false }) + "s";
  }
  /** Durations recorded in ms: keep short calls visible and long calls readable. */
  function recordedDuration(ms) {
    if (!Number.isFinite(ms) || ms < 0) return "—";
    if (ms === 0) return "0s";
    if (ms < 1000) return uiFormat(Math.floor(ms)) + "ms";
    if (ms < 60000) return uiFormat(Math.floor(ms / 100) / 10) + "s";
    const seconds = Math.floor(ms / 1000);
    const padded = { minimumIntegerDigits: 2, useGrouping: false };
    if (seconds < 3600) return uiFormat(Math.floor(seconds / 60)) + "m" + uiFormat(seconds % 60, padded) + "s";
    return uiFormat(Math.floor(seconds / 3600)) + "h" + uiFormat(Math.floor(seconds % 3600 / 60), padded) + "m" + uiFormat(seconds % 60, padded) + "s";
  }
  const pct = (r) => (r == null ? "-" : uiFormat(r, { style: "percent", maximumFractionDigits: 0 }));
  // Same thresholds as the TUI: green at the optimiser trigger (90%).
  const rateClass = (r) => (r == null ? "muted" : r >= 0.9 ? "rate-good" : r >= 0.7 ? "rate-mid" : "rate-bad");
  // Keep the existing UTF-16 budget, including the ellipsis. Older engines
  // without Segmenter retain code points, but cannot guarantee whole graphemes.
  const summarySegments = typeof Intl.Segmenter === "function"
    ? new Intl.Segmenter(undefined, { granularity: "grapheme" }) : null;
  function truncate(s, n) {
    s = String(s || "").replace(/\s+/g, " ").trim();
    if (s.length <= n) return s;
    if (n <= 0) return "";
    let end = 0;
    const segments = summarySegments ? summarySegments.segment(s) : s;
    for (const part of segments) {
      const length = summarySegments ? part.segment.length : part.length;
      if (end + length > n - 1) break;
      end += length;
    }
    return s.slice(0, end) + "…";
  }
  // Rust str::lines(): a final LF ends a line instead of adding an empty one.
  // CRLF has the same count; lone CR is ordinary content.
  function logicalLineCount(text) {
    if (!text.length) return 0;
    let count = text.endsWith("\n") ? 0 : 1;
    for (let i = 0; i < text.length; i++) if (text[i] === "\n") count++;
    return count;
  }
  function rateCell(r) {
    return h("span", { class: rateClass(r) }, pct(r));
  }
  function bar(r) {
    return h("span", { class: "bar " + rateClass(r) }, h("i", { style: "width:" + Math.round((r || 0) * 100) + "%" }));
  }
  // Same two-window descriptive rule as the TUI; retained outcomes, not a forecast.
  function recentCalls(recent) {
    if (typeof recent !== "string" || !recent.length || /[^sf]/.test(recent)) return null;
    const outcomes = recent.slice(-40), now = outcomes.slice(-20), before = outcomes.slice(0, -20);
    const successes = text => [...text].filter(c => c === "s").length;
    const succeeded = successes(now), failed = now.length - succeeded;
    const delta = before.length >= 5 && now.length >= 5 ? succeeded / now.length - successes(before) / before.length : 0;
    return { now, succeeded, failed, trend: delta >= 0.1 ? "up" : delta <= -0.1 ? "down" : "none" };
  }
  function toolRecent(recent) {
    const sample = recentCalls(recent);
    if (!sample) return null;
    const { now, succeeded, failed, trend } = sample;
    const arrow = trend === "up" ? " ↑" : trend === "down" ? " ↓" : "";
    const label = tr("Recent retained calls") + ": " + uiFormat(now.length) + "; " +
      tr("Succeeded") + ": " + uiFormat(succeeded) + "; " + tr("Failed") + ": " + uiFormat(failed) +
      "; " + tr("Recent sample trend") + ": " + tr(trend === "up" ? "up" : trend === "down" ? "down" : "no arrow");
    return h("span", { class: "tool-recent", role: "img", "aria-label": label },
      h("span", { class: "tool-recent-bars", "aria-hidden": "true" }, [...now].map(c =>
        h("span", { class: c === "s" ? "rate-good success" : "rate-bad failure" }))),
      h("span", { class: "tool-recent-counts", "aria-hidden": "true" }, uiFormat(succeeded) + "/" + uiFormat(now.length) + " ✓" + arrow));
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
  /** Original Tool payload text, with syntax highlighting for valid JSON.
   * Parsing only classifies the text: stringify would round large integers,
   * discard repeated keys and change numeric spellings/whitespace. */
  function payload(text) {
    try {
      JSON.parse(text);
      return h("pre", null, highlight(text, "json"));
    } catch (_) {
      return h("pre", null, text);
    }
  }
  function normalizeApprovalPath(path) {
    const plain = path.replace(/^\\\\\?\\(?=[a-z]:[\\/])/i, "");
    const windows = /^(?:[a-z]:[\\/]|\\\\)/i.test(path);
    return { path: windows ? plain.replace(/\\/g, "/").replace(/[A-Z]/g, (letter) => letter.toLowerCase()) : path, windows };
  }
  /** Why a Capability is unusually broad, or null (SPEC §10.3; the same
   * rule as `broad_capability` in the protocol crate). */
  function broadWhy(cap) {
    if (cap === "net") return "any host on the internet";
    if (cap === "process") return "any program on this computer";
    if (cap.startsWith("fs:write:")) {
      const { path } = normalizeApprovalPath(cap.slice(9));
      const p = path.replace(/\/+$/, "");
      const parts = p.split("/").filter(Boolean);
      const home = parts.length === 0 || (["Users", "home"].includes(parts[0]) && parts.length <= 2) || (parts.length === 1 && parts[0] === "root");
      const windowsHome = /^[a-z]:\//.test(path) && (parts.length === 1 || (parts[1] === "users" && parts.length <= 3));
      if (p === "" || p === "~" || home || windowsHome) return "writing anywhere in your files";
    }
    if (cap.startsWith("process:")) {
      // Match the protocol's executable and first-argument split, including
      // Windows drive-letter paths and optional scopes.
      const grant = cap.slice(8);
      const separator = grant.lastIndexOf(":");
      const scope = grant.slice(separator + 1);
      const executable = separator > 1 && scope && !/[\\/]/.test(scope)
        ? grant.slice(0, separator) : grant;
      const base = executable.split(/[\\/]/).pop()
        .replace(/[A-Z]/g, (letter) => letter.toLowerCase()).replace(/(?:\.exe)+$/, "");
      const stem = base.replace(/[0-9.]+$/, "");
      if (["sh", "bash", "zsh", "fish", "dash", "ksh", "csh", "tcsh", "mksh", "ash", "cmd", "powershell", "pwsh", "osascript", "env", "xargs", "sudo", "doas", "pkexec", "su", "runuser", "nodejs"].includes(base)
          || ["python", "pythonw", "py", "pyw", "node", "deno", "bun", "perl", "ruby", "php", "lua", "tclsh"].includes(stem)) {
        return "an interpreter or shell: it can run any program, like bare process";
      }
      if (["curl", "wget", "ssh", "scp", "rsync", "nc", "ncat"].includes(base)) {
        return "it can reach any host and write files, like net plus fs:write";
      }
      if (base === "git") return "git runs hooks and settings from the repository it works in, even for read-only sub-commands";
      if (base === "find") return "find can run any program (-exec)";
      if (base === "tar") return "tar can run any program (--to-command) and write anywhere";
      if (["make", "cmake", "ninja"].includes(base)) return "a build tool runs whatever the build files say";
    }
    if (cap.startsWith("env:")) {
      const k = cap.slice(4).toUpperCase();
      const schemaRole = ["PRIMARY_KEY", "FOREIGN_KEY", "UNIQUE_KEY", "CANDIDATE_KEY", "SORT_KEY", "PARTITION_KEY"].includes(k);
      const credentialKey = k.endsWith("_PWD") || (!schemaRole && k.split("_").includes("KEY")) ||
        ["APIKEY", "ACCESSKEY", "ENCRYPTIONKEY", "SIGNINGKEY", "HMACKEY", "CLIENTKEY", "ACCOUNTKEY",
          "MASTERKEY", "SESSIONKEY", "SHAREDKEY"].some((ending) => k.endsWith(ending));
      const credentialPart = k !== "SSH_AUTH_SOCK" && k.split(/[^A-Z0-9]+/).some((part) =>
        ["TOKEN", "TOKENS", "SECRET", "SECRETS", "PASSWORD", "PASSWORDS", "PASSWD",
          "CREDENTIAL", "CREDENTIALS", "PRIVATE", "AUTH", "AUTHORIZATION"].includes(part));
      if (credentialKey || k.endsWith("_PAT") ||
          ["DATABASE_URL", "REDIS_URL", "MONGODB_URI"].includes(k) ||
          credentialPart) {
        return "a secret from your environment";
      }
    }
    return null;
  }
  function isBroad(cap) { return broadWhy(cap) !== null; }
  /** Where an fs: path lies, as [warn, note] or null (SPEC §10.3; the same
   * rule as `path_note` in the protocol crate). */
  function pathNote(cap, cwd) {
    if (cap.startsWith("fs:read-sensitive:")) return [true, "Credential read: exact approved path identity only, not descendants. Contents may flow to the model provider."];
    const m = /^fs:(read|write):(.*)$/.exec(cap);
    if (!m) return null;
    const { path, windows } = normalizeApprovalPath(m[2]);
    const seg = path.split("/").filter(Boolean);
    const windowsPath = /^[a-z]:[\\/]|^\\\\/i.test(path);
    const composer = (windowsPath ? path.split(/[\\/]/).filter(Boolean) : seg)
      .slice(-3).map(part => (windowsPath ? part.split(":")[0] : part).toLowerCase());
    if (composer.at(-1) === "auth.json" && (composer.at(-2) === ".composer" ||
        (composer.at(-2) === "composer" && composer.at(-3) === ".config"))) return [true, "holds Composer credentials"];
    const secret = [[".ssh", "SSH keys"], [".aws", "cloud credentials"], [".gnupg", "encryption keys"], [".kube", "cluster credentials"], [".docker", "registry credentials"], ["Keychains", "the keychain"], [".password-store", "saved passwords"]];
    for (const [dir, what] of secret) if (seg.includes(windows ? dir.toLowerCase() : dir)) return [true, "holds " + what];
    if (seg.some((part) => part.toLowerCase() === ".azure")) return [true, "holds cloud credentials"];
    for (let i = 0; i + 1 < seg.length; i++) if (seg[i] === ".config" && (seg[i + 1] === "gh" || seg[i + 1] === "gcloud")) return [true, "holds account credentials"];
    const pulumiWindowsPath = /^(?:[a-z]:|\\\\)/i.test(path);
    const pulumiPath = (pulumiWindowsPath ? path.split(/[\\/]+/) : seg)
      .filter(Boolean).map((part) => (pulumiWindowsPath ? part.split(":", 1)[0] : part).toLowerCase());
    if (pulumiPath.at(-1) === "credentials.json" && pulumiPath.slice(0, -1).includes(".pulumi")) return [true, "holds Pulumi credentials"];
    if (seg[0] === "etc") return [true, "system configuration"];
    if (!cwd) return null;
    const c = cwd.replace(/\/+$/, "");
    const compared = normalizeApprovalPath(cwd).path.replace(/\/+$/, "");
    return path === compared || path.startsWith(compared + "/") ? [false, "inside the working folder"] : [true, "outside the working folder " + c];
  }
  function capTags(caps) {
    if (!caps || !caps.length) return h("span", { class: "muted" }, "none");
    return caps.map((c) => h("span", { class: "tag" + (isBroad(c) ? " broad" : ""), title: isBroad(c) ? "broad access: " + broadWhy(c) : null }, c));
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
    if ($("notes-panel").classList.contains("hidden")) S.unread = Math.min(S.notes.length, S.unread + 1);
    renderNotes();
    renderStatus();
  }
  /** A toast; `action` ({label, run, ms}) adds a button; `ms` defaults to 3500. */
  function toast(text, err, action) {
    let timer = null, deadline = 0, hovered = false, focused = false;
    let remaining = err ? null : Number.isFinite(action?.ms) && action.ms >= 0 ? action.ms : 3500;
    const remove = () => { clearTimeout(timer); timer = null; remaining = null; t.remove(); };
    const pauseTimer = () => {
      if (remaining == null || timer == null) return;
      clearTimeout(timer);
      timer = null;
      remaining = Math.max(0, deadline - Date.now());
    };
    const resumeTimer = () => {
      if (remaining == null || timer != null) return;
      deadline = Date.now() + remaining;
      timer = setTimeout(remove, remaining);
    };
    const updatePause = (source, active) => {
      const wasPaused = hovered || focused;
      if (source === "hover") hovered = active;
      else focused = active;
      const isPaused = hovered || focused;
      if (!wasPaused && isPaused) pauseTimer();
      else if (wasPaused && !isPaused) resumeTimer();
    };
    const button = action && h("button", { class: "btn small", type: "button", onclick: () => {
      if (button.disabled) return;
      button.disabled = true;
      remove();
      action.run();
    } }, action.label);
    const dismiss = h("button", { class: "btn ghost small toast-dismiss", type: "button", "aria-label": "Dismiss notification", onclick: remove }, "×");
    const message = h("span", { class: "toast-message" });
    const t = h("div", { class: "toast" + (err ? " err" : ""), role: err ? "alert" : "status",
      onmouseenter: () => updatePause("hover", true), onmouseleave: () => updatePause("hover", false),
      onfocusin: () => updatePause("focus", true),
      onfocusout: (event) => { if (!t.contains(event.relatedTarget)) updatePause("focus", false); } },
    message, h("span", { class: "toast-actions" }, button, dismiss));
    $("toasts").append(t);
    setTimeout(() => { if (t.isConnected) message.textContent = text; }, 0);
    if (remaining != null) {
      deadline = Date.now() + remaining;
      timer = setTimeout(remove, remaining);
    }
  }

  const chatAnnouncement = { last: 0, timer: null, pending: null };
  function announceChat(text, session = S.current, immediate = false) {
    const announcer = $("chat-announcer");
    if (!announcer || !text || session !== S.current) return;
    if (immediate) {
      clearTimeout(chatAnnouncement.timer);
      chatAnnouncement.timer = null;
      chatAnnouncement.pending = null;
      announcer.textContent = text;
      chatAnnouncement.last = Date.now();
      return;
    }
    chatAnnouncement.pending = { text, session };
    if (chatAnnouncement.timer) return;
    chatAnnouncement.timer = setTimeout(() => {
      chatAnnouncement.timer = null;
      const next = chatAnnouncement.pending;
      chatAnnouncement.pending = null;
      if (!next || next.session !== S.current) return;
      announcer.textContent = next.text;
      chatAnnouncement.last = Date.now();
    }, Math.max(0, 500 - (Date.now() - chatAnnouncement.last)));
  }
  function completedRunAnswer(messages, afterSeq) {
    const message = [...messages].reverse().find((item) =>
      item.role === "assistant" && typeof item.content === "string" && item.seq > afterSeq);
    if (!message) return "";
    let meta = {};
    try { meta = JSON.parse(message.meta || "{}"); } catch (_) {}
    return Array.isArray(meta?.tool_calls) && meta.tool_calls.length ? "" : message.content;
  }
  let announcedRun = null;
  let lastAnnouncedFinishedRun = null;

  // -------------------------------------------------------- ask dialogs
  let askResolve = null;
  let askTag = null;
  let askOptions = null;
  let askSaving = false;
  const askQueue = [];
  /** Promise-based prompt/confirm: resolves the text (input) or true, or null. */
  function ask(opts) {
    return new Promise((resolve) => {
      askQueue.push({ opts, resolve });
      if (!askResolve) showNextAsk();
    });
  }
  function configureAskInput(input, opts) {
    const kind = opts.secret ? "secret" : ["prose", "machine", "url", "secret"].includes(opts.inputKind) ? opts.inputKind : "prose";
    const prose = kind === "prose";
    // URL uses its keyboard layout without adding new native form validation.
    input.type = kind === "secret" ? "password" : "text";
    for (const [key, value] of Object.entries({
      spellcheck: prose ? "true" : "false",
      autocapitalize: prose ? "sentences" : "none",
      autocorrect: prose ? "on" : "off",
      inputmode: kind === "url" ? "url" : "text",
      autocomplete: prose ? "on" : kind === "url" ? "url" : "off",
    })) input.setAttribute(key, value);
  }
  function showNextAsk() {
    if (!askQueue.length) return;
    const { opts, resolve } = askQueue.shift();
    askResolve = resolve;
    askTag = opts.tag || null;
    askOptions = opts;
    rememberFocus("ask-backdrop");
    $("ask-title").textContent = opts.title;
    $("ask-text").textContent = opts.text || "";
    $("ask-body").replaceChildren(...(opts.body ? [opts.body] : []));
    $("ask-form").setAttribute("autocomplete", opts.secretFields ? "off" : "on");
    const input = $("ask-input");
    configureAskInput(input, opts);
    input.classList.toggle("hidden", !opts.input);
    input.value = opts.input ? opts.value || "" : "";
    input.placeholder = opts.placeholder || "";
    const cancel = $("ask-cancel");
    cancel.textContent = opts.cancelLabel || "Cancel";
    cancel.classList.toggle("hidden", !!opts.hideCancel);
    const ok = $("ask-ok");
    ok.textContent = opts.ok || "OK";
    ok.className = "btn " + (opts.danger ? "danger" : "primary");
    $("ask-backdrop").classList.remove("hidden");
    const first = opts.body && opts.body.querySelector("input, select, textarea");
    // Long read-only content should start at its heading, not at footer actions.
    if (opts.focusTitle) $("ask-form").scrollTop = 0;
    openDialog($("ask-backdrop"), { trigger: focusBefore["ask-backdrop"],
      initialFocus: opts.focusTitle ? $("ask-title") : opts.input ? input : first || ok, onDismiss: () => closeAsk(null) });
    if (opts.input && activeDialog()?.node === $("ask-backdrop")) input.select();
  }
  function closeAsk(value) {
    if (askSaving) return;
    // Callers pass/capture the resolved value before the shared DOM is cleared.
    $("ask-input").value = "";
    for (const field of askOptions?.secretFields || []) field.value = "";
    $("ask-body").replaceChildren();
    showAskError("");
    $("ask-backdrop").classList.add("hidden");
    restoreFocus("ask-backdrop");
    const resolve = askResolve;
    askResolve = null;
    askTag = null;
    askOptions = null;
    if (resolve) resolve(value);
    showNextAsk();
  }
  function cancelAsk(tag) {
    for (let i = askQueue.length - 1; i >= 0; i--) {
      if (askQueue[i].opts.tag === tag) askQueue.splice(i, 1)[0].resolve(null);
    }
    if (askTag === tag) closeAsk(null);
  }
  function showAskError(message) {
    const error = $("ask-error");
    error.classList.toggle("hidden", !message);
    error.textContent = message;
  }
  function setAskSaving(saving) {
    askSaving = saving;
    $("ask-fields").disabled = $("ask-ok").disabled = $("ask-cancel").disabled = saving;
    $("ask-ok").textContent = saving ? "Saving…" : askOptions.ok || "OK";
    $("ask-saving").textContent = saving ? "Saving…" : "";
    const dialog = activeDialog();
    if (saving && dialog?.node === $("ask-backdrop")) focusDialog(dialog);
  }
  function recordValue(field, message) {
    const value = field.value.trim();
    if (!value) throw Object.assign(new Error(message), { field });
    return value;
  }
  $("ask-form").addEventListener("submit", async (e) => {
    e.preventDefault();
    if (!askOptions || askSaving) return;
    if (!askOptions.saveRequest) {
      return closeAsk($("ask-input").classList.contains("hidden") ? true : $("ask-input").value);
    }
    const opts = askOptions;
    showAskError("");
    let req;
    try { req = opts.saveRequest(); }
    catch (error) { showAskError(error.message); error.field?.focus(); return; }
    if (!req) return closeAsk(null);
    const focused = document.activeElement;
    setAskSaving(true);
    try {
      const result = await request(req, opts.timeoutMs);
      setAskSaving(false);
      closeAsk(result ?? true);
    } catch (error) {
      setAskSaving(false);
      // Daemon errors can echo request fields. Secret-bearing forms use
      // field-specific guidance without including the server's error text.
      showAskError(error.daemonResponse
        ? opts.saveError || error.message
        : "Save was not confirmed. Check the connection and whether the record was saved before trying again.");
      if (activeDialog()?.node === $("ask-backdrop")) focusDialog(activeDialog(), focused);
    }
  });
  $("ask-cancel").onclick = () => closeAsk(null);

  // ask_user (SPEC §5.5): one fieldset per question, options as radios or
  // checkboxes, and an "Other" box for the user's own answer.
  function questionForm(qs) {
    return h("div", { class: "questions" }, qs.map((q, i) => {
      const type = q.multi_select ? "checkbox" : "radio";
      const other = h("input", { type: "text", class: "q-other", spellcheck: "false",
        placeholder: (q.options || []).length ? "Other…" : "Your answer",
        // Typing your own answer replaces a single choice.
        oninput: (e) => { if (!q.multi_select && e.target.value) e.target.closest("fieldset").querySelectorAll("input[type=radio]").forEach((r) => (r.checked = false)); } });
      return h("fieldset", { class: "question" },
        h("legend", {}, q.header ? h("span", { class: "pill on", text: q.header }) : null, " ", q.question),
        (q.options || []).map((o) => h("label", { class: "q-option" },
          h("input", { type, name: "q" + i, value: o.label, onchange: () => { if (!q.multi_select) other.value = ""; } }),
          h("span", {}, h("span", { text: o.label }), o.description ? h("span", { class: "muted", text: o.description }) : null))),
        other);
    }));
  }
  function readQuestionForm(body, qs) {
    return qs.map((_, i) => {
      const fs = body.querySelectorAll("fieldset.question")[i];
      const selected = [...fs.querySelectorAll("input:checked")].map((x) => x.value);
      const text = fs.querySelector(".q-other").value.trim();
      return text ? { selected, text } : { selected };
    });
  }

  // ------------------------------------------------------------ activity
  // "thinking… 12s", or "rate limited · retrying in 8s (2/5) · Esc to stop".
  function activityText(a) {
    if (a.retryAt) {
      const left = Math.ceil((a.retryAt - Date.now()) / 1000);
      return a.label + " · " + (left > 0 ? "retrying in " + left + "s " : "retrying ") + a.count + " · Esc to stop";
    }
    return tr(a.label) + "… " + elapsed(a.since);
  }
  function setActivity(label) {
    if (!label) S.activity = null;
    else if (!S.activity || S.activity.label !== label) S.activity = { label, since: Date.now() };
    scheduleEventRender({ status: true, live: true });
  }
  const isRunning = () => S.current != null && S.running.has(S.current);
  const coarsePointer = window.matchMedia("(pointer: coarse)");
  function inputTouchMode() { return coarsePointer.matches; }
  function syncInputTouchMode() {
    document.documentElement.toggleAttribute("data-input-touch", inputTouchMode());
  }
  function composerEnterSends() {
    return typeof S.enterSends === "boolean" ? S.enterSends : !inputTouchMode();
  }
  function enterModeHint(enterSends) {
    return enterSends ? "Enter: sends" : "Enter: newline";
  }
  function updateEnterSendsControl() {
    const input = $("enter-sends");
    if (input) input.checked = composerEnterSends();
  }
  function setEnterSends(value) {
    S.enterSends = typeof value === "boolean" ? value : null;
    prefs.set("enterSends", S.enterSends);
    renderComposer();
    updateEnterSendsControl();
  }
  syncInputTouchMode();

  // UI work from a burst of protocol events is coalesced, while event state
  // itself is reduced immediately. Navigation invalidates rather than replays
  // a queued paint from an earlier Session/view generation.
  let eventPaint = null, eventPaintEpoch = 0;
  function cancelEventRender() {
    eventPaintEpoch++;
    if (eventPaint) cancelAnimationFrame(eventPaint.frame);
    eventPaint = null;
  }
  function scheduleEventRender(flags) {
    if (!Object.values(flags).some(Boolean)) return;
    if (eventPaint && (eventPaint.session !== S.current || eventPaint.view !== S.view || eventPaint.sequence !== sessionSeq)) cancelEventRender();
    if (!eventPaint) {
      const batch = { session: S.current, view: S.view, sequence: sessionSeq, epoch: eventPaintEpoch };
      eventPaint = batch;
      batch.frame = requestAnimationFrame(() => {
        if (eventPaint !== batch) return;
        eventPaint = null;
        if (batch.epoch !== eventPaintEpoch || batch.session !== S.current || batch.view !== S.view || batch.sequence !== sessionSeq) return;
        withTranscriptLayout(() => {
          if (batch.history) renderHistory();
          else if (batch.live) renderLive();
          if (batch.status) renderStatus();
          if (batch.composer) renderComposer();
        }, { newContent: !!batch.newContent });
      });
    }
    for (const [key, value] of Object.entries(flags)) if (value) eventPaint[key] = true;
  }

  // ============================================================ rendering
  function renderAll() {
    withTranscriptLayout(() => {
      renderChrome();
      renderSessions();
      renderHistory();
      renderOutbox();
      renderComposer();
      renderView();
      renderStatus();
      renderApproval();
      renderNotes();
    });
  }

  function renderChrome() {
    document.querySelectorAll("#tabs button").forEach((b) => {
      if (b.dataset.view === S.view) b.setAttribute("aria-current", "page");
      else b.removeAttribute("aria-current");
    });
    for (const v of ["chat", "tools", "metrics", "proposals", "approvals", "settings"]) {
      $("view-" + v).classList.toggle("hidden", v !== S.view);
    }
    $("sidebar").classList.toggle("hidden", S.view !== "chat");
    $("menu-btn").classList.toggle("hidden", S.view !== "chat");
    syncSidebar();
    const conn = $("conn");
    const retry = $("conn-retry");
    if (!S.reconnect && document.activeElement === retry) focusReturn($("notes-btn"));
    retry.hidden = !S.reconnect;
    const localConnected = S.local && S.ws?.readyState === 1 && S.daemonOnline;
    if (S.reconnect) {
      // Visible backoff with a manual retry (SPEC §12).
      const secs = Math.max(0, Math.ceil((S.reconnect.at - Date.now()) / 1000));
      conn.className = "pill off";
      conn.textContent = (S.local ? "daemon" : "relay") + " offline · retry in " + secs + "s";
    } else if (!navigator.onLine && !localConnected) {
      conn.className = "pill off";
      conn.textContent = "offline · daemon unreachable";
    } else if (!S.ws || S.ws.readyState !== 1) {
      conn.className = "pill off";
      conn.textContent = S.ws && S.ws.readyState === 0 ? "connecting…" : (S.local ? "daemon disconnected" : "relay disconnected");
    } else if (!S.daemonOnline) {
      conn.className = "pill off";
      conn.textContent = "daemon offline";
    } else if (!S.local && !S.channel) {
      // Online, but not yet proven both ways: not "connected" (#904).
      conn.className = "pill off";
      conn.textContent = S.handshakeFailed ? "couldn't secure the connection" : "securing the connection…";
    } else {
      conn.className = "pill on";
      conn.textContent = "● connected" + (!navigator.onLine ? " · browser offline" : "");
    }
  }

  // Installable app shell (SPEC §12): the page opens offline and says so.
  function handleServiceWorkerMessage(event) {
    if (event.data?.type === "formless-open-notification-session") {
      openNotificationSession(event.data.session).catch(() => {});
    } else if (event.data?.type === "formless-focus-pending") {
      focusPending();
    }
  }
  if ("serviceWorker" in navigator && window.isSecureContext) {
    navigator.serviceWorker.register("sw.js").catch(() => {});
    navigator.serviceWorker.addEventListener("message", handleServiceWorkerMessage);
  }
  window.addEventListener("offline", () => renderAll());
  window.addEventListener("online", () => { renderAll(); if (S.reconnect) retryNow(); });

  function renderView() {
    if (S.view === "tools") renderTools();
    else if (S.view === "metrics") renderMetrics();
    else if (S.view === "proposals") renderProposals();
    else if (S.view === "approvals") renderApprovals();
    else if (S.view === "settings") renderSettings();
  }

  function setView(view) {
    if (S.view !== view) {
      if (S.view === "chat") reading.anchor = captureReadingAnchor() || reading.anchor;
      if (eventPaint?.newContent) reading.pendingContent = true;
      cancelEventRender();
    }
    S.view = view;
    prefs.set("view", view);
    if (view === "tools") { refreshTools(); refreshModules(); }
    if (view === "metrics") { refreshTools(); refreshModelStats(); refreshForecastMetrics(); }
    if (view === "proposals") refreshProposals();
    if (view === "approvals") refreshApprovals();
    if (view === "settings") refreshSettings();
    renderChrome();
    renderView();
    if (view === "chat") { withTranscriptLayout(() => renderHistory()); scrollToBottom(); }
  }

  // ------------------------------------------------------------- sidebar
  let popover = null;
  let popoverTrigger = null;
  let popoverAlignment = false;
  function closePopover({ restore = !!popover && (popover.contains(document.activeElement) || document.activeElement === document.body) } = {}) {
    const trigger = popoverTrigger;
    if (popover) { popover.remove(); popover = null; }
    popoverTrigger = null;
    // Keep the expanded trigger visible until focus-within takes over.
    if (restore) {
      focusReturn(trigger);
      if (document.activeElement === trigger) revealSessionControl(trigger);
    }
    if (trigger) {
      trigger.removeAttribute("aria-expanded");
      trigger.removeAttribute("aria-controls");
    }
  }
  function menuTriggerKey(e, open) {
    if (["Enter", " ", "ArrowDown", "ArrowUp"].includes(e.key)) e.stopPropagation();
    if (e.key === "ArrowDown" || e.key === "ArrowUp") {
      e.preventDefault();
      open(e.key === "ArrowUp");
    }
  }
  function anchorPopover(btn) {
    if (popoverTrigger && popoverTrigger !== btn) {
      popoverTrigger.removeAttribute("aria-expanded");
      popoverTrigger.removeAttribute("aria-controls");
    }
    popoverTrigger = btn;
    btn.setAttribute("aria-haspopup", "menu");
    btn.setAttribute("aria-expanded", "true");
    btn.setAttribute("aria-controls", "action-menu");
    popover.id = "action-menu";
  }
  /** Measure after insertion: rem-based menus can be taller than the viewport. */
  function placePopover(btn, alignRight = false, focusLast = false) {
    anchorPopover(btn);
    const items = [...popover.querySelectorAll("button")];
    for (const item of items) { item.setAttribute("role", "menuitem"); item.tabIndex = -1; }
    const available = () => items.filter((item) => !item.disabled && !item.hidden);
    const focus = (item) => {
      if (!item) return;
      for (const other of items) other.tabIndex = other === item ? 0 : -1;
      item.focus({ preventScroll: true });
      item.scrollIntoView({ block: "nearest" });
    };
    popover.addEventListener("focusin", (e) => {
      if (items.includes(e.target)) for (const item of items) item.tabIndex = item === e.target ? 0 : -1;
    });
    // A menu action is not an outside click on its drawer or parent dialog.
    popover.addEventListener("click", (e) => e.stopPropagation());
    popover.addEventListener("keydown", (e) => {
      // A menu owns its keys: page shortcuts must not run behind it.
      e.stopPropagation();
      if (e.key === "Escape" || e.key === "Tab") {
        e.preventDefault();
        closePopover({ restore: true });
        return;
      }
      const enabled = available();
      if (!enabled.length) return;
      const current = enabled.indexOf(document.activeElement);
      let next;
      if (e.key === "ArrowDown") next = (current + 1) % enabled.length;
      else if (e.key === "ArrowUp") next = (current - 1 + enabled.length) % enabled.length;
      else if (e.key === "Home") next = 0;
      else if (e.key === "End") next = enabled.length - 1;
      else if (e.key.length === 1 && !e.ctrlKey && !e.metaKey && !e.altKey && e.key !== " ") {
        const ordered = enabled.slice(current + 1).concat(enabled.slice(0, current + 1));
        const match = ordered.find((item) => item.textContent.trim().toLocaleLowerCase(uiLocale()).startsWith(e.key.toLocaleLowerCase(uiLocale())));
        if (match) next = enabled.indexOf(match);
      }
      if (next != null) { e.preventDefault(); focus(enabled[next]); }
    });
    (activeDialog()?.node || document.body).append(popover);
    popoverAlignment = alignRight;
    positionPopover();
    const enabled = available();
    focus(focusLast ? enabled[enabled.length - 1] : enabled[0]);
  }
  /** Visual viewport coordinates are CSS pixels, including during pinch zoom.
   * Keep typography and the main layout width native; only the keyboard's
   * available height and floating controls follow the visible rectangle. */
  let viewportFrame = 0;
  let composerInputWidth = 0;
  let viewportPrinting = false;
  function viewportBounds() {
    const viewport = window.visualViewport;
    const finite = (value, fallback) => Number.isFinite(value) ? value : fallback;
    const left = Math.max(0, finite(viewport?.offsetLeft, 0));
    const top = Math.max(0, finite(viewport?.offsetTop, 0));
    const width = Math.max(1, finite(viewport?.width, innerWidth));
    const height = Math.max(1, finite(viewport?.height, innerHeight));
    return { left, top, width, height, right: left + width, bottom: top + height,
      scale: Math.max(.1, finite(viewport?.scale, 1)) };
  }
  function safeViewportBounds(gap = 0) {
    const viewport = viewportBounds(), css = getComputedStyle(document.documentElement);
    const inset = edge => parseFloat(css.getPropertyValue("--safe-" + edge)) || 0;
    return { ...viewport, left: viewport.left + inset("left") + gap,
      top: viewport.top + inset("top") + gap, right: viewport.right - inset("right") - gap,
      bottom: viewport.bottom - inset("bottom") - gap };
  }
  function viewportVariable(name, value) {
    const style = document.documentElement.style, text = Math.max(0, value).toFixed(3) + "px";
    if (style.getPropertyValue(name) === text) return false;
    style.setProperty(name, text);
    return true;
  }
  // inline-size containment on .app creates a fixed-position containing block.
  // Measure that origin instead of treating layout and visual coordinates alike.
  function positionFixedBox(node, left, top) {
    node.style.right = node.style.bottom = "auto";
    node.style.left = node.style.top = "0px";
    const origin = node.getBoundingClientRect();
    node.style.left = left - origin.left + "px";
    node.style.top = top - origin.top + "px";
  }
  function withMeasurableBox(node, measure) {
    const hidden = getComputedStyle(node).display === "none";
    const display = node.style.getPropertyValue("display"), priority = node.style.getPropertyPriority("display"), visibility = node.style.visibility;
    if (hidden) { node.style.setProperty("display", "block", "important"); node.style.visibility = "hidden"; }
    try { return measure(); }
    finally { if (hidden) { if (display) node.style.setProperty("display", display, priority); else node.style.removeProperty("display"); node.style.visibility = visibility; } }
  }
  function revealViewportControl(node, container, bounds, { partial = false } = {}) {
    if (!node || !container || !container.contains(node)) return;
    const rect = node.getBoundingClientRect(), box = container.getBoundingClientRect();
    const top = Math.max(bounds.top, box.top) + 1, bottom = Math.min(bounds.bottom, box.bottom) - 1;
    if (rect.height <= bottom - top) {
      if (rect.top < top) container.scrollTop -= top - rect.top;
      else if (rect.bottom > bottom) container.scrollTop += rect.bottom - bottom;
    } else if (partial) {
      if (rect.top > top) container.scrollTop += rect.top - top;
      else if (rect.bottom < bottom) container.scrollTop += rect.bottom - bottom;
    }
  }
  function revealSessionControl(control) {
    const list = $("sessions");
    if (list.contains(control) && focusVisible(control)) revealViewportControl(control, list, safeViewportBounds());
  }
  function positionPopover() {
    if (!popover) return;
    if (!focusVisible(popoverTrigger)) return closePopover();
    const gap = (parseFloat(getComputedStyle(document.documentElement).fontSize) || 16) * .5;
    const bounds = safeViewportBounds(gap);
    popover.style.minWidth = "0";
    popover.style.maxWidth = Math.max(0, bounds.right - bounds.left) + "px";
    popover.style.maxHeight = Math.max(0, bounds.bottom - bounds.top) + "px";
    // Reset the previous anchor before measuring shrink-to-fit width. An old
    // right-edge position must not squeeze a retained menu after resize.
    popover.style.right = popover.style.bottom = "auto";
    popover.style.left = popover.style.top = "0px";
    const rect = popoverTrigger.getBoundingClientRect(), width = popover.offsetWidth, height = popover.offsetHeight;
    const left = popoverAlignment ? rect.right - width : rect.left;
    const top = rect.bottom + gap + height <= bounds.bottom ? rect.bottom + gap : rect.top - gap - height;
    positionFixedBox(popover, Math.max(bounds.left, Math.min(left, bounds.right - width)), Math.max(bounds.top, Math.min(top, bounds.bottom - height)));
    revealViewportControl(document.activeElement, popover, bounds);
  }
  function positionComposePopups(bounds, fontSize) {
    const input = $("input"), anchor = input.getBoundingClientRect();
    for (const popup of document.querySelectorAll(".compose-popup")) {
      if (!popup.getClientRects().length) continue;
      const width = Math.min(35 * fontSize, Math.max(0, bounds.right - bounds.left), Math.max(anchor.width, 12 * fontSize));
      popup.style.width = width + "px";
      const above = Math.max(0, anchor.top - bounds.top - fontSize * .25), below = Math.max(0, bounds.bottom - anchor.bottom - fontSize * .25);
      const preferAbove = above >= below;
      popup.style.maxHeight = Math.min(20 * fontSize, preferAbove ? above : below) + "px";
      const rect = popup.getBoundingClientRect();
      const top = preferAbove ? anchor.top - fontSize * .25 - rect.height : anchor.bottom + fontSize * .25;
      positionFixedBox(popup, Math.max(bounds.left, Math.min(anchor.left, bounds.right - rect.width)), Math.max(bounds.top, Math.min(top, bounds.bottom - rect.height)));
      const selected = popup.querySelector(".sel");
      if (selected) revealViewportControl(selected, popup, bounds);
    }
  }
  function positionJump(bounds, fontSize) {
    const jump = $("jump"), composer = $("composer").getBoundingClientRect();
    withMeasurableBox(jump, () => {
      jump.style.maxWidth = Math.max(0, bounds.right - bounds.left) + "px";
      jump.style.maxHeight = Math.max(0, Math.min(bounds.bottom, composer.top) - bounds.top - fontSize * .5) + "px";
      const rect = jump.getBoundingClientRect();
      const top = Math.max(bounds.top, Math.min(bounds.bottom - rect.height, composer.top - rect.height - fontSize * .5));
      positionFixedBox(jump, bounds.left + Math.max(0, bounds.right - bounds.left - rect.width) / 2, top);
    });
  }
  function syncViewport() {
    viewportFrame = 0;
    if (viewportPrinting || document.hidden || matchMedia("print").matches) return;
    const viewport = viewportBounds(), root = document.documentElement;
    const fontSize = parseFloat(getComputedStyle(root).fontSize) || 16;
    const zoomed = Math.abs(viewport.scale - 1) > .01;
    const composing = $("composer").contains(document.activeElement);
    withTranscriptLayout(() => {
      root.setAttribute("data-visual-viewport", "");
      root.toggleAttribute("data-viewport-zoomed", zoomed);
      const keyboardShrankViewport = !zoomed && viewport.height < root.clientHeight - fontSize * 3;
      const shortViewport = viewport.height < fontSize * (zoomed ? 28 : 24);
      root.toggleAttribute("data-viewport-short", shortViewport || keyboardShrankViewport);
      root.toggleAttribute("data-viewport-composing", composing);
      viewportVariable("--visual-left", viewport.left);
      viewportVariable("--visual-top", viewport.top);
      viewportVariable("--visual-width", viewport.width);
      viewportVariable("--visual-height", viewport.height);
      viewportVariable("--visual-right", viewport.right);
      viewportVariable("--visual-bottom", viewport.bottom);
      viewportVariable("--app-viewport-left", zoomed ? 0 : viewport.left);
      viewportVariable("--app-viewport-top", zoomed ? 0 : viewport.top);
      viewportVariable("--app-viewport-width", zoomed ? root.clientWidth : viewport.width);
      viewportVariable("--app-viewport-height", zoomed ? viewport.bottom : viewport.height);
      const bounds = safeViewportBounds(fontSize * .5);
      const chat = $("view-chat"), composer = $("composer"), input = $("input");
      if (chat.getClientRects().length) {
        const rect = chat.getBoundingClientRect();
        const available = Math.max(1, Math.min(bounds.bottom, rect.bottom) - Math.max(bounds.top, rect.top));
        viewportVariable("--chat-visible-height", available);
        viewportVariable("--composer-limit", available * .65);
        const inputStyle = getComputedStyle(input);
        const oneLine = (parseFloat(inputStyle.lineHeight) || parseFloat(inputStyle.fontSize) * 1.45)
          + parseFloat(inputStyle.paddingTop) + parseFloat(inputStyle.paddingBottom) + parseFloat(inputStyle.borderTopWidth) + parseFloat(inputStyle.borderBottomWidth);
        const inputLimitChanged = viewportVariable("--composer-input-limit", Math.max(oneLine, Math.min(viewport.height * .35, available * .45)));
        // Measure after applying the width for the current viewport mode. The
        // existing observers batch another frame when height changes; comparing
        // only width prevents that notification from repeatedly autosizing.
        if (zoomed) composer.style.width = Math.max(0, bounds.right - bounds.left) + "px";
        else for (const property of ["left", "top", "right", "bottom", "width"]) composer.style.removeProperty(property);
        const inputWidth = input.getBoundingClientRect().width;
        const inputWidthChanged = inputWidth !== composerInputWidth;
        composerInputWidth = inputWidth;
        if (inputLimitChanged || inputWidthChanged) autosize();
        if (zoomed) {
          const height = composer.getBoundingClientRect().height;
          const bottom = Math.min(bounds.bottom, rect.bottom);
          positionFixedBox(composer, bounds.left, Math.max(bounds.top, bottom - height));
          viewportVariable("--floating-composer-height", height + fontSize * .5);
        } else {
          viewportVariable("--floating-composer-height", 0);
        }
        if (composing) revealViewportControl(document.activeElement, composer, bounds);
        positionComposePopups(bounds, fontSize);
        positionJump(bounds, fontSize);
      }
      positionPopover();
      const focused = document.activeElement;
      const focusScrollport = activeDialog()?.node.querySelector(".modal") || focused?.closest(".pair-card");
      if (focusScrollport) revealViewportControl(focused, focusScrollport, bounds);
      else {
        const chrome = focused?.closest(".topbar, .statusbar");
        if (chrome) revealViewportControl(focused, chrome, safeViewportBounds(), { partial: true });
      }
    }, { newContent: false });
    // A viewport pan changes which block the user is looking at. Adopt that
    // new visible block rather than compensating away the user's pinch/pan.
    if (!S.follow && readingVisible()) reading.anchor = captureReadingAnchor();
  }
  function scheduleViewportSync() {
    if (!viewportFrame) viewportFrame = requestAnimationFrame(syncViewport);
  }
  window.addEventListener("resize", scheduleViewportSync);
  window.visualViewport?.addEventListener("resize", scheduleViewportSync);
  window.visualViewport?.addEventListener("scroll", scheduleViewportSync);
  document.addEventListener("focusin", scheduleViewportSync);
  document.addEventListener("focusout", scheduleViewportSync);
  document.addEventListener("visibilitychange", scheduleViewportSync);
  window.addEventListener("beforeprint", () => { viewportPrinting = true; });
  window.addEventListener("afterprint", () => { viewportPrinting = false; scheduleViewportSync(); });
  $("view-chat").addEventListener("scroll", scheduleViewportSync, { passive: true });
  const viewportResize = new ResizeObserver(scheduleViewportSync);
  for (const node of [$("composer"), $("input"), $("app"), document.querySelector(".topbar"), document.querySelector(".statusbar")]) viewportResize.observe(node);
  new MutationObserver(records => {
    if (records.some(record => record.target === $("app") || record.target === $("jump") || record.target instanceof Element && record.target.closest("#composer")
      || [...record.addedNodes].some(node => node instanceof Element && node.matches(".backdrop, .compose-popup")))) scheduleViewportSync();
  }).observe(document.body, { childList: true, subtree: true, attributes: true, attributeFilter: ["class", "hidden"] });
  scheduleViewportSync();
  /** The URL fragment as `#k=…&s=…` pairs. Not URLSearchParams: it turns
   * "+" (in a base64 key) into a space. */
  function hashParams() {
    const out = {};
    for (const part of location.hash.replace(/^#/, "").split("&")) {
      const i = part.indexOf("=");
      if (i > 0) {
        try { out[part.slice(0, i)] = decodeURIComponent(part.slice(i + 1)); }
        catch (_) { out.malformed = true; }
      }
    }
    return out;
  }
  /** A link that opens Session `s` (SPEC §12). On the relay it carries the
   * pairing key, in the fragment only (never sent to a server); locally
   * the login token is single-use, so it opens in a browser already signed
   * in. */
  function sessionLink(s) {
    const base = location.origin + location.pathname;
    const secret = S.local ? "" : rememberedSecret();
    return secret ? base + "#k=" + encodeURIComponent(secret) + "&s=" + encodeURIComponent(s.id)
      : base + "#s=" + encodeURIComponent(s.id);
  }
  function copySessionLink(s) {
    const withKey = !S.local && !!rememberedSecret();
    navigator.clipboard.writeText(sessionLink(s)).then(
      () => toast(withKey ? "Copied: it holds the pairing key, so share it only with your own devices"
        : "Copied: opens “" + s.name + "” in a browser already connected to this daemon"),
      () => toast("Copy failed", true));
  }
  /** Archived Sessions are hidden but kept (SPEC §10.4). */
  async function archiveSession(s) {
    const r = await act({ type: "set_session_archived", id: s.id, archived: !s.archived }, s.archived ? "Restored " + s.name : "Archived " + s.name);
    if (!r) return;
    const i = S.sessions.findIndex((x) => x.id === r.id);
    if (i >= 0) S.sessions[i] = r;
    renderSessions();
  }
  /** Pinned Sessions list first (SPEC §10.4). */
  async function pinSession(s) {
    const r = await act({ type: "set_session_pinned", id: s.id, pinned: !s.pinned });
    if (!r) return;
    const i = S.sessions.findIndex((x) => x.id === r.id);
    if (i >= 0) S.sessions[i] = r;
    renderSessions();
  }
  function sessionMenu(btn, s, focusLast = false) {
    closePopover();
    const action = (label, run, attrs = {}) => h("button", { type: "button", ...attrs, onclick: () => {
      const current = S.sessions.find((session) => session.id === s.id);
      closePopover();
      if (current) run(current);
    } }, label);
    popover = h("div", { class: "popover", role: "menu", "aria-label": "Session actions" },
      action(s.pinned ? "Unpin" : "Pin", pinSession, { "data-session-action": "pin" }),
      action(s.archived ? "Unarchive" : "Archive", archiveSession, { "data-session-action": "archive" }),
      action("Rename", renameSession),
      action("Suggest name", suggestName),
      action("Tags…", tagSession),
      action("Instructions…", editInstructions),
      action("Fork", (session) => forkSession(session.id)),
      action(S.local ? "Copy link" : "Copy link (has the key)", copySessionLink,
        { title: S.local ? "" : "The link includes the pairing key" }),
      action("Export…", (session) => pickExport(session.id)),
      action("Delete", deleteSession, { class: "danger" }));
    placePopover(btn, false, focusLast);
  }

  // Running Invocations across Sessions (SPEC §6.3): cancel or abandon.
  let runningRefresh = null, runningExposed = false, runningExposureQueued = false;
  let runningExposureEpoch = 0, runningExposurePending = false;
  function runningPanelExposed() {
    const sidebar = $("sidebar");
    // Read the actual layout; both viewport and container queries can hide
    // this drawer. The Running box itself is hidden when its list is empty.
    return !document.hidden && S.view === "chat" && !!sidebar.getClientRects().length
      && !sidebar.closest(".hidden, [hidden]") && getComputedStyle(sidebar).visibility !== "hidden";
  }
  function syncRunningExposure() {
    const exposed = runningPanelExposed();
    if (exposed && !runningExposed) {
      runningExposureEpoch++;
      runningExposurePending = true;
    }
    if (exposed && !runningExposed && !runningExposureQueued) {
      runningExposureQueued = true;
      queueMicrotask(() => {
        runningExposureQueued = false;
        if (runningPanelExposed()) refreshRunning("exposure");
      });
    }
    runningExposed = exposed;
  }
  function sameRunningConnection(query, requireClientId = true) {
    return query.ws === S.ws && query.channel === S.channel && (!requireClientId || query.conn === S.conn);
  }
  async function refreshRunning(reason = "event") {
    if (!S.channel || S.ws?.readyState !== 1 || (!S.local && !S.relayReady)) return;
    if (runningRefresh && sameRunningConnection(runningRefresh, false)) {
      // A change may have happened after the read began; keep one follow-up.
      // Timer ticks can share a read; the exposure generation fences its reply.
      if (reason === "event") runningRefresh.followUp = true;
      return;
    }
    const query = runningRefresh = { ws: S.ws, conn: S.conn, channel: S.channel, exposure: runningExposureEpoch, followUp: false };
    const list = await request({ type: "list_invocations" }).catch(() => null);
    if (runningRefresh !== query) return;
    runningRefresh = null;
    if (!sameRunningConnection(query)) {
      // A Session subscription may change its client identifier without
      // reconnecting. Retry on this socket, but never paint that old reply.
      if (sameRunningConnection(query, false)) refreshRunning();
      return;
    }
    if (query.followUp || (runningExposed && query.exposure !== runningExposureEpoch)) {
      refreshRunning();
      return;
    }
    if (!list) return;
    runningExposurePending = false;
    S.invocations = list;
    reconcileInvocationProgress(list);
    renderRunning(list);
  }
  function pollRunning() {
    if (runningPanelExposed() && (runningExposurePending || S.invocations?.length)) refreshRunning("poll");
  }
  function runningRow(i, row) {
    if (!row) {
      row = h("div", { class: "running-item" });
      row.append(
        h("button", { class: "linklike", type: "button", title: "Open its session", onclick: () => openSession(row.invocation.session_id) }),
        h("button", { class: "btn ghost small", type: "button", title: "Cancel (user-cancelled)", onclick: () => {
          const invocation = row.invocation;
          return act({ type: "cancel_invocation", handle: invocation.handle }, "Cancelled " + invocation.tool_name).then(() => refreshRunning());
        } }, "Cancel"),
        h("button", { class: "btn ghost small", type: "button", title: "Stop waiting for it", onclick: () => {
          const invocation = row.invocation;
          return act({ type: "abandon_invocation", handle: invocation.handle }, "Abandoned " + invocation.tool_name).then(() => refreshRunning());
        } }, "Abandon"));
    }
    row.invocation = i;
    for (const [index, action] of ["open", "cancel", "abandon"].entries()) {
      row.children[index].dataset.runningAction = action;
      row.children[index].dataset.runningHandle = String(i.handle);
    }
    row.classList.toggle("stalled", !!i.stalled);
    const session = S.sessions.find((x) => x.id === i.session_id);
    row.firstElementChild.replaceChildren(h("strong", null, i.tool_name),
      " · " + recordedDuration(i.elapsed_ms) + (session ? " · " + session.name : "") +
      (i.stalled ? " · silent " + recordedDuration(i.silent_ms) : "") + progressDiagnostic(i) + (i.abandoned ? " · abandoned" : ""));
    row.lastElementChild.hidden = !!i.abandoned;
    return row;
  }
  function renderRunning(list) {
    const box = $("running");
    const focused = document.activeElement;
    const focusedRow = focused?.closest("#running .running-item");
    const previous = new Map([...box.children].filter(node => node.invocation).map(row => [row.invocation.handle, row]));
    const rows = list.map(i => runningRow(i, previous.get(i.handle)));
    const heading = box.firstElementChild?.tagName === "H4" ? box.firstElementChild : h("h4", null, "Running tools");
    reconcileChildren(box, rows.length ? [heading, ...rows] : rows);
    box.classList.toggle("hidden", !list.length);
    if (focusedRow && (document.activeElement !== focused || !focusVisible(focused))) {
      const target = focusVisible(focused) ? focused
        : focusedRow.isConnected ? focusedRow.firstElementChild : $("new-session");
      focusReturn(target);
    }
  }
  setInterval(pollRunning, 2000);
  document.addEventListener("visibilitychange", syncRunningExposure);

  function sessionRows(sessions, visible, tree) {
    if (!tree) return visible.map(session => ({ session, depth: 0, note: "" }));
    const shown = new Set(visible.map(session => session.id)), allIds = new Set(sessions.map(session => session.id));
    const children = new Map(), roots = [];
    for (const session of visible) {
      if (session.forked_from && shown.has(session.forked_from)) {
        if (!children.has(session.forked_from)) children.set(session.forked_from, []);
        children.get(session.forked_from).push(session);
      } else roots.push(session);
    }
    const seen = new Set(), rows = [];
    for (const [candidates, detached] of [[roots, false], [visible, true]]) {
      for (const root of candidates) {
        if (seen.has(root.id)) continue;
        const stack = [[root, 0]];
        while (stack.length) {
          const [session, depth] = stack.pop();
          if (seen.has(session.id)) continue;
          seen.add(session.id);
          let note = session.fork_point_seq != null ? "fork at seq " + session.fork_point_seq : "";
          if (session.forked_from) {
            const why = detached && depth === 0 ? "lineage cycle" : !allIds.has(session.forked_from) ? "parent unavailable" : !shown.has(session.forked_from) ? "parent filtered" : null;
            if (why) note += " · " + why + ": " + session.forked_from;
          }
          if (depth > 12) note += " · deep lineage (indent capped)";
          rows.push({ session, depth: Math.min(depth, 12), note });
          for (const child of [...(children.get(session.id) || [])].reverse()) stack.push([child, depth + 1]);
        }
      }
    }
    return rows;
  }
  function resolveSessionRef(query) {
    const exact = S.sessions.find(session => session.id === query);
    if (exact) return exact.id;
    const matches = S.sessions.filter(session => query && (session.id.startsWith(query) || session.name === query));
    if (matches.length !== 1) throw new Error(matches.length ? "Ambiguous Session; use its full ID." : "No matching Session; refresh the Sessions list and use an ID. Paid model comparison is /compare-models.");
    return matches[0].id;
  }
  function sessionComparisonText(comparison) {
    const lines = ["Stored Session comparison — no model request", "Common ancestor: " + (comparison.common_ancestor || "unknown") + " · fork point: " + (comparison.fork_point_seq ?? "unknown") + " · diff after seq " + comparison.diff_from_seq];
    if (comparison.prefix_changed) lines.push("Copied prefix changed: showing full histories.");
    lines.push(...(comparison.warnings || []).map(warning => "Warning: " + warning));
    for (const [label, side] of [["LEFT", comparison.left], ["RIGHT", comparison.right]]) {
      const usage = side.usage;
      lines.push(label + ": " + side.session.name + " (" + side.session.id + ")", "  " + usage.scope,
        "  Recorded: " + usage.recorded_prompt_tokens + " input / " + usage.recorded_completion_tokens + " output tokens · " + usage.recorded_cached_tokens + " cached / " + usage.recorded_cache_write_tokens + " cache-write · " + usage.requests + " requests",
        "  Usage coverage: " + (usage.usage_complete ? "complete" : "incomplete; missing usage is not zero") + " · " + usage.unallocated_requests + " unallocated requests (" + usage.unallocated_prompt_tokens + " input / " + usage.unallocated_completion_tokens + " output) · " + usage.unknown_runs + " unknown Runs",
        usage.cost != null ? "  Recorded branch cost: " + uiMoney(usage.cost, 6) : "  Total cost: unknown · priced allocated subtotal only: " + uiMoney(usage.priced_cost_subtotal, 6));
      if (usage.known_records_cost_range) lines.push("  Observed priced rows allocation range only: " + usage.known_records_cost_range.map(value => uiMoney(value, 6)).join("–") + "; excludes missing usage and unpriced rows");
      if (usage.unallocated_requests) lines.push("  Unallocated priced subtotal only: " + uiMoney(usage.unallocated_priced_cost_subtotal, 6));
      if (usage.estimated_requests) lines.push("  " + usage.estimated_requests + " request prices estimated from local prices");
      if ((usage.unpriced || []).length) lines.push("  Unpriced: " + usage.unpriced.join(", "));
      lines.push(...(usage.warnings || []).map(warning => "  Warning: " + warning));
    }
    for (const diff of comparison.messages) {
      for (const [label, message] of [[diff.kind === "unchanged" ? "=" : "< LEFT", diff.left], ["> RIGHT", diff.kind === "unchanged" ? null : diff.right]]) {
        if (message) {
          lines.push("", label + " " + message.role + " seq " + message.seq + " (" + message.id + ")", message.content);
          if (message.meta != null) lines.push("  Metadata (quoted data): " + JSON.stringify(message.meta));
        }
      }
    }
    return lines.join("\n");
  }
  async function compareModelsCommand(arg) {
    S.sessionCompareGeneration = (S.sessionCompareGeneration || 0) + 1;
    const [a, b, ...rest] = (arg || "").trim().split(/\s+/);
    if (!a || !b) return note("usage: /compare-models MODEL_A MODEL_B [prompt] (no prompt: your last message again)", true);
    const r = await act({ type: "compare_models", session_id: S.current, models: [a, b], prompt: rest.join(" ") || null });
    if (r) { note("comparing " + a + " vs " + b + " in forked Sessions; the answers show here when both finish"); refreshSessions(); }
  }
  async function compareSessionsCommand(arg) {
    const trigger = document.activeElement;
    const generation = S.sessionCompareGeneration = (S.sessionCompareGeneration || 0) + 1;
    const words = (arg || "").trim().split(/\s+/);
    if (words.length !== 2 || !words[0]) return note("usage: /compare SESSION_A SESSION_B (stored history only); /compare-models starts paid model comparisons", true);
    let left, right;
    try { left = resolveSessionRef(words[0]); right = resolveSessionRef(words[1]); }
    catch (error) { return note(error.message, true); }
    const channel = S.channel, conn = S.conn;
    const result = await act({ type: "compare_sessions", left, right });
    if (!result || generation !== S.sessionCompareGeneration || channel !== S.channel || conn !== S.conn) return;
    if (result.left?.session?.id !== left || result.right?.session?.id !== right) return note("Session comparison reply does not match the requested ordered pair; retry /compare.", true);
    const body = h("pre", { class: "session-comparison" }, sessionComparisonText(result));
    showBidi(body);
    modal("Stored Session comparison", body, { trigger });
  }

  function invalidateBranchSummary() {
    const intent = S.branchSummaryIntent;
    S.branchSummaryGeneration = (S.branchSummaryGeneration || 0) + 1;
    S.branchSummaryIntent = null;
    if (intent) { cancelAsk(intent.generateTag); cancelAsk(intent.reviewTag); }
  }
  function branchSummaryStampMatches(stamp) {
    return stamp.generation === S.branchSummaryGeneration && stamp.target === S.current
      && stamp.channel === S.channel && stamp.conn === S.conn && stamp.ws === S.ws;
  }
  function branchSummaryCurrent(stamp) {
    return branchSummaryStampMatches(stamp) && S.branchSummaryIntent
      && S.branchSummaryIntent.source === stamp.source && S.branchSummaryIntent.target === stamp.target;
  }
  function branchSummaryEvent(ev) {
    const intent = S.branchSummaryIntent;
    if (!intent) return;
    const relevant = ["message_appended", "messages_edited", "session_updated", "session_deleted", "run_started", "run_finished", "run_paused", "compaction_notice"];
    const id = ev.session_id || (ev.session && ev.session.id) || (ev.type === "session_deleted" && ev.id);
    if (ev.type === "settings_changed" || (relevant.includes(ev.type) && (id === intent.source || id === intent.target))) {
      invalidateBranchSummary();
      note("A source or target Session changed; generate a fresh branch summary.");
    }
  }
  async function branchSummaryHash(candidate) {
    const bytes = new Uint8Array(await crypto.subtle.digest("SHA-256", new TextEncoder().encode(candidate)));
    return Array.from(bytes, byte => byte.toString(16).padStart(2, "0")).join("");
  }
  async function branchSummaryPreviewMatches(basis, preview) {
    return preview && typeof preview.candidate === "string" && typeof preview.preview_id === "string" && preview.preview_id.length > 0
      && basis.source_id === preview.source_id && basis.target_id === preview.target_id
      && basis.source_revision === preview.source_revision && basis.target_revision === preview.target_revision
      && typeof preview.candidate_hash === "string" && await branchSummaryHash(preview.candidate) === preview.candidate_hash
      && Date.parse(preview.expires_at) > Date.now();
  }
  function branchSummaryReviewText(basis, preview) {
    const usage = preview.usage, projection = preview.projection;
    const lines = [approvalText("Source: " + basis.source_name + " (" + preview.source_id + ")"),
      approvalText("Target: " + basis.target_name + " (" + preview.target_id + ")"),
      approvalText("Provider: " + (preview.provider || "unknown") + " · requested model: " + (preview.requested_model || "unknown") + " · served model: " + (preview.served_model || "unknown")),
      "Reported usage: " + usage.reported_prompt_tokens + " input / " + usage.reported_completion_tokens + " output tokens · " + usage.reported_cached_tokens + " cached / " + usage.reported_cache_write_tokens + " cache-write · " + usage.reported_requests + "/" + usage.requests + " requests reported",
      usage.usage_complete ? "Usage coverage: complete" : "Usage coverage: incomplete or unknown; missing usage is not zero",
      usage.cost != null ? "Recorded cost: " + uiMoney(usage.cost, 6) : "Total cost: unknown · priced subtotal only: " + uiMoney(usage.priced_cost_subtotal, 6),
      "Source projection: " + projection.included_messages + "/" + projection.total_messages + " messages and " + projection.included_bytes + "/" + projection.total_bytes + " bytes; " + projection.omitted_messages + " messages and " + projection.omitted_bytes + " bytes omitted; " + projection.truncated_messages + " messages truncated"];
    if (projection.metadata_omitted) lines.push("Source metadata omitted from generation.");
    if (projection.attachment_bodies_omitted) lines.push("Attachment bodies omitted from generation.");
    if (usage.estimated_requests) lines.push(usage.estimated_requests + " request prices estimated from local prices");
    if ((usage.unpriced || []).length) lines.push("Unpriced: " + approvalText(usage.unpriced.join(", ")));
    lines.push(...(preview.warnings || []).map(warning => "Note: " + warning),
      "Insert appends one note to the target Session; it does not merge transcripts or start a Run.", "Exact candidate note (data):", "", preview.candidate);
    return lines.join("\n");
  }
  async function summariseBranchCommand(arg) {
    invalidateBranchSummary();
    const ref = (arg || "").trim();
    if (ref === "cancel") return;
    if (!S.current) return note("Open the target Session first.", true);
    if (!globalThis.crypto?.subtle) return note("This browser cannot verify a summary candidate; use the TUI.", true);
    let source;
    try { source = resolveSessionRef(ref); } catch (error) { return note(error.message, true); }
    if (source === S.current) return note("Choose another source branch for this target Session.", true);
    const stamp = { generation: S.branchSummaryGeneration, source, target: S.current, channel: S.channel, conn: S.conn, ws: S.ws };
    stamp.generateTag = "branch-summary:generate:" + stamp.generation;
    stamp.reviewTag = "branch-summary:review:" + stamp.generation;
    S.branchSummaryIntent = { source, target: stamp.target, generateTag: stamp.generateTag, reviewTag: stamp.reviewTag };
    let preparing = true;
    const pendingBody = h("p", null, "Source: " + source + " → Target: " + stamp.target + ". Started generation may finish and record usage after review is closed.");
    ask({ tag: stamp.generateTag, title: "Generating branch summary", body: pendingBody, ok: "Close review", hideCancel: true }).then(() => {
      if (preparing && branchSummaryCurrent(stamp)) { invalidateBranchSummary(); note("Branch summary review dismissed. Started generation may finish and record usage."); }
    });
    const inspected = await act({ type: "inspect_branch_summary", source_id: source, target_id: stamp.target });
    if (!inspected || !branchSummaryCurrent(stamp)) { preparing = false; cancelAsk(stamp.generateTag); if (branchSummaryCurrent(stamp)) invalidateBranchSummary(); return; }
    const basis = JSON.parse(JSON.stringify(inspected));
    if (basis.source_id !== source || basis.target_id !== stamp.target || !basis.source_revision || !basis.target_revision) {
      preparing = false; invalidateBranchSummary(); return note("Branch summary basis did not match the requested source and target.", true);
    }
    pendingBody.textContent = approvalText("Source: " + basis.source_name + " → Target: " + basis.target_name + " · " + basis.provider + " / " + basis.requested_model) + ". Started generation may finish and record usage after review is closed.";
    const result = await act({ type: "prepare_branch_summary", source_id: source, target_id: stamp.target, source_revision: basis.source_revision, target_revision: basis.target_revision }, null, 120000);
    preparing = false;
    cancelAsk(stamp.generateTag);
    if (!result || !branchSummaryCurrent(stamp)) { if (branchSummaryCurrent(stamp)) invalidateBranchSummary(); return; }
    const preview = JSON.parse(JSON.stringify(result));
    if (!(await branchSummaryPreviewMatches(basis, preview)) || !branchSummaryCurrent(stamp)) {
      if (branchSummaryCurrent(stamp)) { invalidateBranchSummary(); note("Branch summary candidate was stale or did not match its source, target, revisions and exact hash.", true); }
      return;
    }
    const body = h("pre", { class: "branch-summary-review" }, branchSummaryReviewText(basis, preview));
    showBidi(body);
    const accepted = await ask({ tag: stamp.reviewTag, title: "Review branch summary", body, ok: "Insert one note", cancelLabel: "Close review" });
    if (!accepted) { if (branchSummaryCurrent(stamp)) { invalidateBranchSummary(); note("Branch summary review dismissed."); } return; }
    if (!branchSummaryCurrent(stamp) || !(await branchSummaryPreviewMatches(basis, preview)) || !branchSummaryCurrent(stamp)) return;
    S.branchSummaryIntent = null;
    const applied = await act({ type: "commit_branch_summary", preview_id: preview.preview_id, source_id: source, target_id: stamp.target, source_revision: basis.source_revision, target_revision: basis.target_revision, candidate_hash: preview.candidate_hash });
    if (!applied || !branchSummaryStampMatches(stamp)) return;
    if (applied.preview_id !== preview.preview_id || applied.source_id !== source || applied.target_id !== stamp.target || applied.message?.session_id !== stamp.target || applied.message?.content !== preview.candidate) {
      return note("Branch summary insertion reply did not match the reviewed candidate; inspect the target Session before retrying.", true);
    }
    onEvent({ type: "message_appended", session_id: stamp.target, message: applied.message });
    note(applied.already_applied ? "This summary note was already inserted; no duplicate was added." : "Inserted one branch summary note; no Run was started.");
  }

  function compareSessions(a, b) {
    // Match the daemon's ORDER BY archived, pinned DESC, updated_at DESC.
    const group = Number(!!a.archived) - Number(!!b.archived) || Number(!!b.pinned) - Number(!!a.pinned);
    if (group) return group;
    const first = a.updated_at ?? "", second = b.updated_at ?? "";
    return first < second ? 1 : first > second ? -1 : 0;
  }
  function renderSessionBulkBar() {
    const list = $("sessions"), existing = list.querySelector(".bulk-bar");
    if (!S.marked.size) {
      existing?.remove();
      return;
    }
    const ids = [...S.marked];
    const btn = (label, run, cls) => h("button", { type: "button", class: "btn small" + (cls ? " " + cls : ""), onclick: run }, label);
    const bar = h("li", { class: "bulk-bar" },
      h("span", { class: "muted" }, ids.length + " selected"),
      btn("Archive", () => bulkSessions("archive", ids)),
      btn("Export", () => bulkSessions("export", ids)),
      btn("Tag", () => bulkSessions("tag", ids)),
      btn("Delete", () => bulkSessions("delete", ids), "danger"),
      btn("Clear", () => { S.marked.clear(); renderSessions(); }));
    if (existing) existing.replaceWith(bar);
    else (list.querySelector(".tag-chips") || list.firstElementChild).after(bar);
  }
  function updateSessionTimes() {
    for (const item of $("sessions").querySelectorAll("[data-session-id]")) {
      // Some Run events update S.running before a fresh Session summary.
      if (S.running.has(item.dataset.sessionId) !== !!item.querySelector(".session-open > .running")) {
        renderSessions();
        return;
      }
      const stamp = item.querySelector(".when");
      const text = relTime(stamp.dataset.updatedAt);
      if (stamp.textContent !== text) stamp.textContent = text;
    }
  }
  function renderSessions() {
    renderTopicNavigation();
    const list = $("sessions");
    const focused = document.activeElement;
    const focusedRow = focused?.closest("#sessions [data-session-id]");
    const focusedControl = focusedRow && ["mark", "session-open", "menu"].find((name) => focused.classList.contains(name));
    const menuSession = popoverTrigger?.closest("#sessions [data-session-id]")?.dataset.sessionId;
    let replacementMenu = null, replacementFocus = null;
    list.replaceChildren();
    S.marked = S.marked || new Set();
    if (S.marked.size) {
      // Index the backing list, including filter-hidden and archived Sessions.
      const loadedIds = new Set(S.sessions.map((s) => s.id));
      for (const id of S.marked) if (!loadedIds.has(id)) S.marked.delete(id);
    }
    if (!S.sessions.length) {
      list.append(h("li", { class: "muted", style: "padding:0.5rem" }, "No sessions yet."));
      if (menuSession) closePopover();
      return;
    }
    S.sessions.sort(compareSessions);
    list.append(h("li", null, h("button", { type: "button", class: "btn small", "aria-pressed": String(!!S.sessionTree), onclick: () => { S.sessionTree = !S.sessionTree; renderSessions(); } }, S.sessionTree ? "Tree on" : "Tree off")));
    // Tag chips filter the list (SPEC §10.4).
    const tags = [...new Set(S.sessions.flatMap((s) => s.tags || []))].sort();
    if (S.tagFilter && !tags.includes(S.tagFilter)) S.tagFilter = null;
    if (tags.length) {
      list.append(h("li", { class: "tag-chips" }, tags.map((t) =>
        h("button", { type: "button", class: "chip-btn" + (S.tagFilter === t ? " on" : ""), "aria-pressed": String(S.tagFilter === t),
          onclick: () => { S.tagFilter = S.tagFilter === t ? null : t; renderSessions(); } }, "#" + t))));
    }
    renderSessionBulkBar();
    const tagged = (s) => !S.tagFilter || (s.tags || []).includes(S.tagFilter);
    // Archived Sessions sit in their own collapsible group at the end.
    const topicSessions = topicSessionIds();
    const archived = S.sessions.filter((s) => (!topicSessions.size || !topicSessions.has(s.id)) && s.archived && tagged(s));
    const active = S.sessions.filter((s) => (!topicSessions.size || !topicSessions.has(s.id)) && !s.archived && tagged(s));
    const appendRows = (visible) => {
      for (const row of sessionRows(S.sessions, visible, !!S.sessionTree)) {
        const s = row.session;
        const running = S.running.has(s.id);
        const paused = !running && s.last_run_status === "paused";
        const state = [s.pinned && "Pinned", running && "Running", paused && "Paused"].filter(Boolean).join(", ");
        const stateId = "session-state-" + s.id;
        const menu = h("button", {
          class: "icon-btn menu", type: "button", "aria-label": "Session actions", "aria-haspopup": "menu",
          onkeydown: (e) => menuTriggerKey(e, (last) => sessionMenu(menu, s, last)),
          onclick: (e) => { e.stopPropagation(); sessionMenu(menu, s); },
        }, "⋯");
        const item = h("li", {
          class: "session" + (s.id === S.current ? " active" : ""),
          "data-session-id": s.id,
        },
        h("input", { type: "checkbox", class: "mark", "aria-label": "Select " + s.name, checked: S.marked.has(s.id),
          onchange: (e) => { if (e.target.checked) S.marked.add(s.id); else S.marked.delete(s.id); renderSessionBulkBar(); } }),
        h("button", { class: "session-open", type: "button", "aria-label": s.name,
          "aria-current": s.id === S.current ? "true" : null, "aria-describedby": state ? stateId : null,
          onclick: () => openSession(s.id) },
          running ? h("span", { class: "running", title: "running", "aria-hidden": "true" })
            : paused ? h("span", { class: "state", title: "paused", "aria-hidden": "true" }, "⏸") : null,
          h("span", { class: "name", title: s.name, style: "padding-inline-start:" + (row.depth * 0.75) + "rem" }, s.pinned ? "★ " : null, row.depth ? "↳ " : null, s.name,
            row.note ? h("span", { class: "muted lineage", title: row.note }, " · " + row.note) : null,
            s.preview ? h("span", { class: "preview", title: s.preview }, s.preview) : null),
          h("span", { class: "when", "data-updated-at": s.updated_at,
            title: (s.message_count || 0) + " messages" + (s.model ? " · " + s.model : "") }, relTime(s.updated_at))),
        state ? h("span", { id: stateId, class: "sr-only" }, state) : null, menu);
        list.append(item);
        if (focusedRow?.dataset.sessionId === s.id && focusedControl) replacementFocus = item;
        if (menuSession === s.id) replacementMenu = { menu, session: s };
      }
    };
    appendRows(active);
    if (archived.length) {
      list.append(h("li", { class: "session archived-toggle muted", tabindex: 0, role: "button", "aria-expanded": String(!!S.showArchived),
        onclick: () => { S.showArchived = !S.showArchived; renderSessions(); },
        onkeydown: (e) => { if (e.key === "Enter" || e.key === " ") { e.preventDefault(); S.showArchived = !S.showArchived; renderSessions(); } } },
      (S.showArchived ? "▾ " : "▸ ") + "Archived (" + archived.length + ")"));
      if (S.showArchived) appendRows(archived);
    }
    if (replacementMenu) {
      const { menu, session } = replacementMenu;
      anchorPopover(menu);
      const pin = popover.querySelector('[data-session-action="pin"]'), archive = popover.querySelector('[data-session-action="archive"]');
      const pinLabel = session.pinned ? "Unpin" : "Pin", archiveLabel = session.archived ? "Unarchive" : "Archive";
      if (pin.textContent !== pinLabel) pin.textContent = pinLabel;
      if (archive.textContent !== archiveLabel) archive.textContent = archiveLabel;
      revealSessionControl(menu);
      positionPopover();
    } else if (menuSession) closePopover();
    if (replacementFocus) {
      const control = replacementFocus.querySelector("." + focusedControl);
      if (focusedControl === "menu" && !focusVisible(control)) replacementFocus.querySelector(".session-open").focus({ preventScroll: true });
      control.focus({ preventScroll: true });
      revealSessionControl(document.activeElement);
    }
  }

  // ------------------------------------------------ topic orchestration
  // Topics and attempt transcripts are daemon-owned. Never infer lineage from
  // titles, external harness IDs, or browser-local session preferences.
  function topicSessionIds() {
    const ids = new Set();
    if (!S.topicsAvailable) return ids;
    if (S.mainChat) ids.add(S.mainChat.id);
    for (const topic of S.topics) {
      if (topic.topic_session_id) ids.add(topic.topic_session_id);
      for (const attempt of topic.attempts || []) ids.add(attempt.conversation_session_id);
    }
    return ids;
  }
  function topicForSession(id = S.current) {
    return S.topics.find(topic => topic.topic_session_id === id
      || (topic.attempts || []).some(attempt => attempt.conversation_session_id === id));
  }
  function currentTopicAttempt() {
    return topicForSession()?.attempts?.find(attempt => attempt.conversation_session_id === S.current);
  }
  function topicStatusLabel(status) {
    return ({ pending_approval: "Needs approval", approved: "Ready to start", running: "In progress",
      complete: "Ready for you", pending: "Pending", succeeded: "Succeeded", cancelled: "Cancelled",
      ready: "Ready for you", needs_user: "Needs your input", failed: "Failed", rejected: "Not moved",
      abandoned: "Interrupted" })[status] || status || "Pending";
  }
  function harnessLabel(kind) {
    return ({ claude: "Claude", codex: "Codex", pi: "Pi", opencode: "OpenCode", antigravity: "Antigravity", acp: "ACP", muse: "Muse" })[kind] || kind || "Formless chooses";
  }
  function topicBadge(status) {
    return h("span", { class: "topic-badge topic-state-" + status }, topicStatusLabel(status));
  }
  function topicWorkflowLabel(topic) {
    return (topic.outline || []).map(step => step.title).join(" → ") || "Formless chooses the steps";
  }
  function topicPath(topic) {
    const names = [], seen = new Set();
    while (topic && !seen.has(topic.id)) {
      seen.add(topic.id); names.unshift(topic.name);
      topic = S.topics.find(item => item.id === topic.parent_topic_id);
    }
    return names.join(" / ");
  }
  function topicArtifacts(outputs) {
    return (outputs || []).map(output => {
      if (output.kind === "directory") return h("li", null, "Directory copy ", h("code", null, output.path));
      if (output.kind === "git_revision") return h("li", null, "Git revision ", h("code", null, output.revision));
      if (output.kind === "file") return h("li", null, "File ", h("code", null, output.path),
        h("small", { class: "muted" }, "SHA-256 ", output.sha256));
      if (output.kind === "text") return h("li", null, h("details", null, h("summary", null, output.title || "Text output"),
        h("pre", null, output.content)));
      return h("li", { class: "muted" }, "Unknown output type");
    });
  }
  function replaceTopicContent(root, signature, children) {
    if (root.topicSignature === signature) return;
    const focused = root.contains(document.activeElement) && document.activeElement.dataset.topicControl;
    root.replaceChildren(...children);
    root.topicSignature = signature;
    if (focused) {
      const replacement = [...root.querySelectorAll("[data-topic-control]")].find(node => node.dataset.topicControl === focused);
      (replacement || root.querySelector("button"))?.focus({ preventScroll: true });
    }
  }
  function renderTopicNavigation() {
    const nav = $("topic-nav");
    if (!nav) return;
    nav.classList.toggle("hidden", !S.topicsAvailable);
    $("other-chats-label").classList.toggle("hidden", !S.topicsAvailable);
    if (!S.topicsAvailable) return;
    const signature = JSON.stringify([S.mainChat, S.topics, S.current, S.topicFocus, S.topicCollapsed, S.topicsError]);
    if (nav.topicSignature === signature) return;
    const main = h("button", { type: "button", class: "topic-chat-link main-chat-link" + (S.current === S.mainChat?.id ? " active" : ""),
      "data-topic-control": "main", "aria-current": S.current === S.mainChat?.id ? "page" : null,
      onclick: () => openSession(S.mainChat.id) }, h("span", { "aria-hidden": "true", class: "topic-icon" }, "◇"), "Main Chat");
    const topics = S.topics.filter(topic => topic.status !== "rejected");
    const seen = new Set();
    const row = topic => {
      if (seen.has(topic.id)) return null;
      seen.add(topic.id);
      const active = topicForSession()?.id === topic.id || S.topicFocus === topic.id;
      const collapsed = !!S.topicCollapsed?.[topic.id];
      const attempts = topic.attempts || [];
      const nested = topics.filter(item => item.parent_topic_id === topic.id);
      const ready = topic.features.filter(feature => feature.status === "ready").length;
      const attention = topic.status === "pending_approval" || topic.features.some(feature => ["needs_user", "failed"].includes(feature.status));
      const childrenId = "topic-attempts-" + topic.id;
      return h("li", { class: "topic-item", "data-topic-id": topic.id },
        h("div", { class: "topic-nav-row" }, attempts.length || nested.length ? h("button", { type: "button", class: "icon-btn topic-disclosure",
          "aria-label": (collapsed ? "Expand " : "Collapse ") + topic.name, "aria-expanded": String(!collapsed), "aria-controls": childrenId,
          "data-topic-control": "toggle-" + topic.id,
          onclick: () => { (S.topicCollapsed ||= {})[topic.id] = !collapsed; renderTopicNavigation(); } }, collapsed ? "▸" : "▾") : h("span", { class: "topic-disclosure" }),
        h("button", { type: "button", class: "topic-chat-link" + (active && !currentTopicAttempt() ? " active" : ""),
          "data-topic-control": "topic-" + topic.id, "aria-current": active && !currentTopicAttempt() ? "page" : null,
          onclick: () => openTopic(topic), title: topic.name + " · " + topicStatusLabel(topic.status) },
          h("span", { class: "topic-nav-name" }, topic.name),
          h("span", { class: "topic-nav-count" + (attention ? " attention" : ""), "aria-label": attention ? "Needs attention" : ready + " of " + topic.features.length + " ready" }, attention ? "!" : ready + "/" + topic.features.length))),
        h("ul", { id: childrenId, class: "topic-attempt-list", hidden: collapsed }, attempts.map(attempt => {
          const feature = topic.features.find(item => item.id === attempt.feature_id);
          const name = (attempt.step_title || "Step") + " · " + harnessLabel(attempt.harness);
          return h("li", null, h("button", { type: "button", class: "topic-chat-link attempt-link" + (S.current === attempt.conversation_session_id ? " active" : ""),
            "data-topic-control": "attempt-" + attempt.id, "aria-current": S.current === attempt.conversation_session_id ? "page" : null,
            title: name + " · " + topicStatusLabel(attempt.status) + " · " + (feature?.request || ""),
            onclick: () => openSession(attempt.conversation_session_id) },
            h("span", { class: "attempt-dot topic-state-" + attempt.status, "aria-hidden": "true" }),
            h("span", { class: "attempt-identity" }, h("span", null, name), h("small", { class: "muted" }, feature?.request || "Work item", " · ", topicStatusLabel(attempt.status)))));
        }).concat(nested.map(row).filter(Boolean))));
    };
    const rows = topics.filter(topic => !topics.some(parent => parent.id === topic.parent_topic_id)).map(row);
    // Keep orphaned/corrupt snapshots reachable without following a cycle forever.
    for (const topic of topics) if (!seen.has(topic.id)) rows.push(row(topic));
    replaceTopicContent(nav, signature, [main,
      h("div", { class: "topic-nav-heading" }, h("h3", { class: "sidebar-label" }, "Topic Chats"),
        h("button", { type: "button", class: "icon-btn", "aria-label": "Create topic from this conversation", "data-topic-control": "new-topic", onclick: () => proposeTopic() }, "+")),
      h("ul", { class: "topic-list" }, rows.length ? rows : h("li", { class: "topic-empty muted" }, "Give focused work its own space.")),
      S.topicsError ? h("div", { class: "topic-refresh-error", role: "status" }, "Topic updates unavailable. Showing last known state. ",
        h("button", { type: "button", class: "btn small ghost", "data-topic-control": "refresh", onclick: () => refreshTopics() }, "Retry")) : null].filter(Boolean));
  }
  function renderTopicContext() {
    const root = $("topic-context");
    if (!root) return;
    const attempt = currentTopicAttempt();
    const topic = S.topics.find(item => item.id === S.topicFocus) || topicForSession();
    const pending = S.topics.filter(item => item.source_session_id === S.current && item.status === "pending_approval" && item.id !== topic?.id);
    root.classList.toggle("hidden", !S.topicsAvailable || !topic && !pending.length);
    if (!S.topicsAvailable) return;
    const signature = JSON.stringify([topic, attempt?.id, pending, S.topicBusy, S.topicsError, S.attemptTranscriptError]);
    if (root.topicSignature === signature) return;
    const control = (label, key, action, primary = false) => h("button", { type: "button", class: "btn small" + (primary ? " primary" : " ghost"),
      disabled: !!S.topicBusy, "data-topic-control": key, onclick: action }, label);
    const children = [];
    if (topic) {
      children.push(h("div", { class: "topic-summary" }, h("div", { class: "topic-summary-copy" },
        h("div", { class: "topic-eyebrow" }, attempt ? "Harness conversation · read only" : "Topic workflow", topicBadge(topic.status)),
        h("strong", { class: "topic-heading" }, topicPath(topic)), h("div", { class: "muted topic-workflow" }, topicWorkflowLabel(topic))),
        h("div", { class: "topic-actions" }, attempt ? control("Back to topic", "back", () => openTopic(topic)) : null,
          control("Refresh", "refresh", () => refreshTopicView()))));
      if (attempt) {
        children.push(h("p", { class: "topic-readonly muted" }, attempt.step_title || "Step", " · ", harnessLabel(attempt.harness),
          attempt.acp_agent ? " · " + attempt.acp_agent : "", attempt.model ? " · " + attempt.model : "",
          " · ", attempt.access === "read_only" ? "Read only" : "Can write in isolated copy", " · ", topicStatusLabel(attempt.status),
          " · Formless coordinates handoffs. Continue the conversation in the topic chat."));
        if (attempt.summary || attempt.detail) children.push(h("p", null, attempt.summary || attempt.detail));
        if (attempt.inputs?.length) children.push(h("div", null, h("strong", null, "Inputs"), h("ul", { class: "topic-outputs" }, topicArtifacts(attempt.inputs))));
        if (attempt.outputs?.length) children.push(h("div", null, h("strong", null, "Outputs"), h("ul", { class: "topic-outputs" }, topicArtifacts(attempt.outputs))));
        if (S.attemptTranscriptError === S.current) children.push(h("p", { class: "error", role: "status" }, "Transcript updates unavailable. Refresh to try again."));
      } else if (topic.status === "pending_approval") {
        children.push(h("p", { class: "muted" }, "These requests stay in the original chat until you agree. Approval moves them here and starts your workflow."),
          h("div", { class: "topic-actions" }, control("Keep in original chat", "reject", () => rejectTopic(topic)),
            control("Move & start", "approve", () => approveTopic(topic), true)));
      } else if (topic.status === "approved") {
        children.push(h("div", { class: "topic-actions" }, h("span", { class: "muted" }, "Requests moved. Your workflow has not started."),
          control("Start workflow", "start", () => startTopic(topic), true)));
      }
      if (!attempt) {
        children.push(h("p", { class: "topic-repo" }, "Workspace: ", topic.workspace ? h("code", null, topic.workspace) : "Private scratch workspace"),
          h("p", { class: "muted" }, "The outline is a starting point. Formless adapts the steps, but asks you before substituting an unavailable harness. Write steps use separate copies, never the source workspace."));
        if (!["pending_approval", "rejected"].includes(topic.status)) children.push(h("div", { class: "topic-controls" },
          h("p", { class: "muted" }, topic.allow_external_context ? "Context: all Formless conversations (you allowed external context)." : "Context: this topic branch and its ancestors only.",
            " · Limits: ", String(topic.limits?.max_retries_per_step ?? 3), " retries per step; ", String(topic.limits?.max_attempts_per_topic ?? 20), " attempts per topic."),
          h("div", { class: "topic-actions" }, control("Context access", "context", () => editTopicContext(topic)),
            control("Run limits", "limits", () => editTopicLimits(topic)), control("Add permission rule", "add-rule", () => addTopicPermissionRule(topic)),
            control("Harness status", "harness-status", () => showHarnessStatus())),
          h("p", { class: "muted" }, "Only you can change context access, permission rules, or limits. Unmatched permissions need your decision; reject rules take precedence."),
          (topic.permission_rules || []).map(rule => h("div", { class: "topic-permission-rule" },
            h("span", null, rule.decision === "reject" ? "Reject" : "Allow once per matching request", " · ", rule.harness ? harnessLabel(rule.harness) : "Any harness",
              rule.acp_agent ? " · " + rule.acp_agent : "", " · ", rule.kind || "Any permission kind", " · ", h("code", null, rule.pattern)),
            control("Remove rule", "remove-rule-" + rule.rule_id, () => removeTopicPermissionRule(topic, rule))))));
        if (topic.outline?.length) children.push(h("ol", { class: "topic-outline-preview" }, topic.outline.map(step => h("li", null,
          h("strong", null, step.title), " · ", harnessLabel(step.harness), step.acp_agent ? " · " + step.acp_agent : "",
          step.model ? " · " + step.model : "", " · ", step.access === "read_only" ? "Read only" : "Can write",
          step.instructions ? h("p", null, step.instructions) : null))));
        if (["approved", "running", "needs_user"].includes(topic.status)) children.push(h("div", { class: "topic-actions" },
          control("Cancel workflow", "cancel", () => cancelTopic(topic))));
        if (topic.status === "needs_user") children.push(h("p", { role: "status" }, "Formless needs your input. Answer the pending question or permission request; you can also reply in this topic chat."));
        if (topic.status === "cancelled") children.push(h("p", { role: "status" }, "Workflow cancelled. Retry a work item to resume."));
        children.push(h("div", { class: "topic-features", "aria-label": "Work item progress" }, topic.features.map((feature, i) =>
        h("article", { class: "topic-feature", "data-feature-id": feature.id }, h("div", { class: "topic-feature-head" },
          h("span", { class: "muted" }, String(i + 1).padStart(2, "0")), h("strong", null, feature.request), topicBadge(feature.status)),
          feature.detail ? feature.status === "failed"
            ? h("details", { class: "chip topic-feature-detail" }, h("summary", null, "Work item details"), h("pre", null, feature.detail))
            : h("p", { class: "topic-feature-detail" }, feature.detail) : null,
          feature.attempt_count ? h("small", { class: "muted" }, feature.attempt_count + " attempts") : null,
          feature.outputs?.length ? h("ul", { class: "topic-outputs" }, topicArtifacts(feature.outputs)) : null,
          ["failed", "needs_user", "complete", "cancelled"].includes(topic.status) ? control("Retry work item", "retry-" + feature.id, () => retryTopicFeature(topic, feature)) : null))));
      }
      children.push(h("p", { class: "topic-no-merge muted" }, "Formless orchestrates the work and brings decisions back to you; nothing merges automatically."));
    }
    children.push(...pending.map(item => h("div", { class: "topic-proposal-notice" }, h("span", null, "Move focused work to ", h("strong", null, item.name), "?"),
      topicBadge("pending_approval"), control("Review proposal", "proposal-" + item.id, () => openTopic(item), true))));
    if (S.topicBusy) children.push(h("p", { class: "muted", role: "status" }, S.topicBusy));
    if (S.topicsError) children.push(h("p", { class: "error", role: "status" }, "Could not refresh topics. Status may be out of date."));
    replaceTopicContent(root, signature, children);
  }
  let topicRefresh = null;
  async function refreshTopics(initial = false) {
    const channel = S.channel;
    if (!channel || !S.daemonOnline) return;
    if (initial) S.topicsUnsupported = false;
    initial = initial || !S.mainChat;
    const revision = S.topicRevision || 0;
    if (topicRefresh?.channel === channel && topicRefresh.revision === revision) return topicRefresh.promise;
    const refresh = { channel, revision };
    refresh.promise = (async () => {
      try {
        if (initial) {
          const ping = await request({ type: "ping" });
          if (S.channel !== channel || revision !== (S.topicRevision || 0)) return;
          if (!Array.isArray(ping?.features) || !ping.features.includes("topic_orchestration_v2")) {
            throw Object.assign(new Error("Topic orchestration v2 is not supported"), { daemonResponse: true, code: "unsupported" });
          }
        }
        const [main, topics] = await Promise.all([initial ? request({ type: "get_main_chat" }) : S.mainChat, request({ type: "list_topics" })]);
        if (S.channel !== channel || revision !== (S.topicRevision || 0)) return;
        if (!main?.id || !Array.isArray(topics)) throw new Error("Invalid topic response");
        S.mainChat = main;
        S.topics = topics;
        S.topicsAvailable = true;
        S.topicsUnsupported = false;
        S.topicsError = false;
        if (!S.sessions.some(session => session.id === main.id)) S.sessions.unshift(main);
      } catch (error) {
        if (S.channel !== channel || revision !== (S.topicRevision || 0)) return;
        if (initial && error.daemonResponse && ["bad_request", "unsupported"].includes(error.code)) {
          S.topicsUnsupported = true;
          S.topicsAvailable = false; S.mainChat = null; S.topics = [];
        }
        else S.topicsError = true;
      }
      if (S.channel === channel) { renderSessions(); renderComposer(); }
    })().finally(() => { if (topicRefresh === refresh) topicRefresh = null; });
    topicRefresh = refresh;
    return refresh.promise;
  }
  let attemptRefresh = null;
  async function refreshAttemptTranscript() {
    if (!currentTopicAttempt() || S.sessionLoad?.id !== S.current || S.sessionLoad.status !== "loaded") return;
    const id = S.current, channel = S.channel, sequence = sessionSeq;
    if (attemptRefresh?.id === id && attemptRefresh.channel === channel) return attemptRefresh.promise;
    const refresh = { id, channel };
    const current = () => id === S.current && channel === S.channel && sequence === sessionSeq;
    refresh.promise = (async () => {
      try {
        const result = await request({ type: "get_session", id });
        if (!current()) return;
        if (!Array.isArray(result.messages)) throw new Error("Invalid transcript response");
        S.attemptTranscriptError = null;
        const next = result.messages;
        const unchanged = next.length === S.messages.length && next.every((message, i) => {
          const old = S.messages[i];
          return message.id === old.id && message.content === old.content && message.meta === old.meta;
        });
        if (!unchanged) { S.messages = next; renderHistory({ newContent: true }); }
      } catch (_) {
        if (current()) S.attemptTranscriptError = id;
      }
      if (current()) renderTopicContext();
    })().finally(() => { if (attemptRefresh === refresh) attemptRefresh = null; });
    attemptRefresh = refresh;
    return refresh.promise;
  }
  async function refreshTopicView() {
    await refreshTopics();
    await refreshAttemptTranscript();
  }
  async function openTopic(topic) {
    const id = topic.topic_session_id || topic.source_session_id;
    if (id !== S.current) await openSession(id);
    S.topicFocus = topic.id;
    if (S.view !== "chat") setView("chat");
    closeSidebar();
    renderTopicNavigation();
    renderTopicContext();
  }
  function storeTopic(topic) {
    S.topicRevision = (S.topicRevision || 0) + 1;
    S.topics = [topic, ...S.topics.filter(item => item.id !== topic.id)];
    renderSessions();
    renderComposer();
  }
  async function topicAction(label, action) {
    if (S.topicBusy) return;
    S.topicRevision = (S.topicRevision || 0) + 1;
    S.topicBusy = label;
    renderTopicContext();
    try { await action(); }
    catch (error) { toast(error.message, true); note(error.message, true); await refreshTopics(); }
    finally { S.topicBusy = null; renderTopicContext(); }
  }
  async function startTopic(topic) {
    return topicAction("Starting workflow…", async () => {
      storeTopic(await request({ type: "start_topic_workflow", topic_id: topic.id }));
      await refreshTopics();
    });
  }
  async function approveTopic(topic) {
    return topicAction("Moving requests and starting workflow…", async () => {
      // The explicit Move & start click is consent. Proposing never moves or
      // launches; a failed launch leaves an approved topic with a Retry action.
      const approved = await request({ type: "approve_topic", approval: { topic_id: topic.id } });
      storeTopic(approved);
      await refreshSessions();
      await openTopic(approved);
      storeTopic(await request({ type: "start_topic_workflow", topic_id: topic.id }));
      await refreshTopics();
    });
  }
  async function rejectTopic(topic) {
    return topicAction("Keeping requests in the original chat…", async () => {
      storeTopic(await request({ type: "reject_topic", rejection: { topic_id: topic.id, reason: "User chose to keep the requests in the original chat." } }));
      S.topicFocus = null;
      renderSessions(); renderTopicContext();
    });
  }
  async function retryTopicFeature(topic, feature) {
    if (!(await ask({ title: "Retry this work item?", text: feature.request + "\nFormless will resume orchestration for this work item.", ok: "Retry work item" }))) return;
    return topicAction("Retrying work item…", async () => {
      storeTopic(await request({ type: "retry_topic_feature", retry: { topic_id: topic.id, feature_id: feature.id } }));
      storeTopic(await request({ type: "start_topic_workflow", topic_id: topic.id }));
      await refreshTopics();
    });
  }
  async function cancelTopic(topic) {
    if (!(await ask({ title: "Cancel this workflow?", text: "Stop Formless and this topic’s running steps. No further steps will launch. Completed outputs remain available; use Retry to resume.", ok: "Cancel workflow" }))) return;
    return topicAction("Cancelling workflow…", async () => {
      storeTopic(await request({ type: "cancel_topic", topic_id: topic.id }));
      await refreshTopics();
    });
  }
  async function saveTopicControl(options) {
    const channel = S.channel;
    const result = await ask({ ...options, saveRequest: () => {
      if (channel !== S.channel) throw new Error("The daemon connection changed. Close and reopen this topic control.");
      return options.saveRequest();
    } });
    if (result && S.channel === channel) storeTopic(result);
  }
  async function editTopicContext(topic) {
    const allow = !topic.allow_external_context;
    return saveTopicControl({ title: allow ? "Allow other conversation context?" : "Restrict conversation context?",
      text: allow ? "Harnesses in this topic will be allowed to search and read conversations outside this topic branch. This applies only to this topic; it does not widen child topics."
        : "Harnesses in this topic will only read this topic chat and attempts, plus ancestor topic chats and attempts. Moved Main Chat messages remain included. This does not retract context already read.",
      ok: allow ? "Allow external context" : "Restrict context",
      saveRequest: () => ({ type: "set_topic_context_access", topic_id: topic.id, allow }) });
  }
  async function editTopicLimits(topic) {
    const field = (label, value) => h("label", { class: "topic-form-field" }, h("span", null, label), value);
    const retries = h("input", { type: "number", min: 0, max: 4294967295, step: 1, value: String(topic.limits?.max_retries_per_step ?? 3), "aria-label": "Retries per step" });
    const attempts = h("input", { type: "number", min: 0, max: 4294967295, step: 1, value: String(topic.limits?.max_attempts_per_topic ?? 20), "aria-label": "Attempts per topic" });
    const read = input => {
      if (!/^\d+$/.test(input.value) || Number(input.value) > 4294967295) throw new Error("Enter whole numbers from 0 to 4294967295.");
      return Number(input.value);
    };
    return saveTopicControl({ title: "Topic run limits", text: "Zero retries allows the initial attempt only. Zero attempts blocks all launches. At a cap, Formless asks you; it cannot raise these limits. Saving does not start or resume work.",
      body: h("div", { class: "topic-form-grid" }, field("Retries per step", retries), field("Attempts per topic", attempts)), ok: "Save limits",
      saveRequest: () => ({ type: "set_topic_limits", topic_id: topic.id, limits: { max_retries_per_step: read(retries), max_attempts_per_topic: read(attempts) } }) });
  }
  async function addTopicPermissionRule(topic) {
    const field = (label, value) => h("label", { class: "topic-form-field" }, h("span", null, label), value);
    const harness = h("select", { "aria-label": "Rule harness" }, ["", "claude", "codex", "pi", "opencode", "antigravity", "acp", "muse"].map(kind => h("option", { value: kind }, kind ? harnessLabel(kind) : "Any harness")));
    const agent = h("input", { type: "text", "aria-label": "Rule ACP agent", placeholder: "Any ACP agent" });
    const agentField = field("ACP agent (optional)", agent);
    agentField.hidden = true; harness.onchange = () => { agentField.hidden = harness.value !== "acp"; };
    const kind = h("input", { type: "text", "aria-label": "Permission kind", placeholder: "Any kind" });
    const pattern = h("input", { type: "text", "aria-label": "Permission pattern", placeholder: "Exact title or command, with * and ? wildcards" });
    // Default to reject. A rule is never inferred from a one-time decision.
    const decision = h("select", { "aria-label": "Rule decision" }, h("option", { value: "reject" }, "Reject"), h("option", { value: "allow_once" }, "Allow once per matching request"));
    decision.value = "reject";
    return saveTopicControl({ title: "Add a topic permission rule", text: "Applies only to this topic. Patterns match the full permission title OR command, case-sensitively. * matches any characters; ? matches one. Reject takes precedence. A matching allow rule will answer future matching requests without another prompt; it does not grant permanent harness access. Workspace trust always needs a separate decision.",
      body: h("div", { class: "topic-outline-editor" }, field("Harness", harness), agentField, field("Permission kind (optional)", kind), field("Pattern", pattern), field("Decision", decision)), ok: "Add rule",
      saveRequest: () => {
        if (!pattern.value.trim()) throw new Error("Enter a permission title or command pattern.");
        return { type: "add_topic_permission_rule", topic_id: topic.id, harness: harness.value || null, acp_agent: harness.value === "acp" ? agent.value.trim() || null : null,
          kind: kind.value.trim() || null, pattern: pattern.value, decision: decision.value };
      } });
  }
  async function removeTopicPermissionRule(topic, rule) {
    return saveTopicControl({ title: "Remove this permission rule?", text: "This topic will no longer apply " + rule.decision + " to matching requests: " + rule.pattern,
      ok: "Remove rule", saveRequest: () => ({ type: "remove_topic_permission_rule", topic_id: topic.id, rule_id: rule.rule_id }) });
  }
  function harnessAvailabilityLabel(item) {
    if (item.kind === "acp") {
      if (!item.installed) return "Not installed · unavailable for execution";
      if (item.signed_in === true) return "Installed · ready";
      return item.signed_in === false
        ? "Installed · unavailable for execution; sign-in required"
        : "Installed · unavailable for execution; sign-in readiness cannot be verified";
    }
    if (!item.installed) return "Not installed";
    return item.signed_in === true ? "Installed · signed in" : item.signed_in === false ? "Installed · sign in required" : "Installed · sign-in status unknown";
  }
  async function showHarnessStatus() {
    const channel = S.channel;
    try {
      const items = await request({ type: "list_harnesses" });
      if (channel !== S.channel) return;
      if (!Array.isArray(items)) throw new Error("Invalid harness status response");
      await ask({ title: "Harness status", focusTitle: true, text: "Detected on your daemon’s machine. Installed does not guarantee an integrated adapter or a working sign-in. Formless asks before substituting another harness.", ok: "Close",
        body: h("ul", { class: "topic-outputs" }, items.length ? items.map(item => h("li", null, h("strong", null, harnessLabel(item.kind), item.acp_agent ? " · " + item.acp_agent : ""),
          " · ", harnessAvailabilityLabel(item), item.version ? " · " + item.version : "", item.detail ? h("p", null, item.detail) : null)) : h("li", null, "No harnesses reported.")) });
    } catch (error) { if (channel === S.channel) toast("Harness status unavailable: " + error.message, true); }
  }
  function topicOutlineEditor(initial = []) {
    const field = (label, input) => h("label", { class: "topic-form-field" }, h("span", null, label), input);
    const list = h("div", { class: "topic-outline-editor" });
    const steps = [];
    const render = () => list.replaceChildren(...steps.map((step, i) => {
      const move = delta => { const target = i + delta; [steps[i], steps[target]] = [steps[target], steps[i]]; render(); };
      return h("fieldset", { class: "topic-outline-step" }, h("legend", null, "Step " + (i + 1)),
        field("Title", step.title), field("Instructions (optional)", step.instructions),
        h("div", { class: "topic-form-grid" }, field("Harness", step.harness), field("Model (optional)", step.model), field("Access", step.access)), step.agentField,
        h("div", { class: "topic-actions" }, h("button", { type: "button", class: "btn small ghost", disabled: i === 0, "aria-label": "Move step " + (i + 1) + " up", onclick: () => move(-1) }, "Up"),
          h("button", { type: "button", class: "btn small ghost", disabled: i === steps.length - 1, "aria-label": "Move step " + (i + 1) + " down", onclick: () => move(1) }, "Down"),
          h("button", { type: "button", class: "btn small ghost", "aria-label": "Remove step " + (i + 1), onclick: () => { steps.splice(i, 1); render(); } }, "Remove")));
    }));
    const add = (value = {}) => {
      if (steps.length >= 20) return toast("An outline can have at most 20 steps.", true);
      const step = {
        title: h("input", { type: "text", value: value.title || "", "aria-label": "Step title" }),
        instructions: h("textarea", { rows: 2, value: value.instructions || "", "aria-label": "Step instructions" }),
        harness: h("select", { "aria-label": "Step harness" }, ["", "claude", "codex", "pi", "opencode", "antigravity", "acp", "muse"].map(kind => h("option", { value: kind }, harnessLabel(kind)))),
        model: h("input", { type: "text", value: value.model || "", placeholder: "Harness default", "aria-label": "Step model" }),
        access: h("select", { "aria-label": "Step access" }, h("option", { value: "read_only" }, "Read only"), h("option", { value: "write" }, "Can write in isolated copy")),
        agent: h("input", { type: "text", value: value.acp_agent || "", placeholder: "Configured ACP agent", "aria-label": "ACP agent" })
      };
      step.instructions.value = value.instructions || "";
      step.harness.value = value.harness || ""; step.access.value = value.access === "write" ? "write" : "read_only";
      step.agentField = field("ACP agent", step.agent);
      const syncAgent = () => { step.agentField.hidden = step.harness.value !== "acp"; };
      step.harness.onchange = syncAgent; syncAgent();
      steps.push(step);
    };
    const preset = values => { steps.splice(0); values.forEach(add); render(); };
    preset(initial);
    return {
      element: h("div", null, h("div", { class: "topic-actions" },
        h("button", { type: "button", class: "btn small ghost", onclick: () => preset([]) }, "Let Formless plan"),
        h("button", { type: "button", class: "btn small ghost", onclick: () => preset([
          { title: "Implement", harness: "claude", access: "write" }, { title: "Review", harness: "codex", access: "read_only" }]) }, "Code preset"),
        h("button", { type: "button", class: "btn small ghost", onclick: () => preset([
          { title: "Scan receipts", access: "read_only" }, { title: "Do accounting", access: "write" }, { title: "Create reports", access: "write" }]) }, "Accounting preset")),
        h("p", { class: "muted" }, "Optional outline. Formless adapts these steps but asks before substituting an unavailable harness. Write steps use isolated copies; merging results is explicit."), list,
        h("button", { type: "button", class: "btn small", onclick: () => { add(); render(); } }, "Add step")),
      read: () => steps.map(step => ({ title: recordValue(step.title, "Enter a title for each step."), instructions: step.instructions.value.trim(),
        harness: step.harness.value || null, acp_agent: step.harness.value === "acp" ? recordValue(step.agent, "Enter the ACP agent name.") : null,
        model: step.model.value.trim() || null, access: step.access.value }))
    };
  }
  async function proposeTopic(selectedMessageId) {
    if (!S.topicsAvailable || !S.current || currentTopicAttempt()) return;
    if (S.sessionLoad?.status !== "loaded" || S.sessionLoad.id !== S.current) return toast("Wait for this conversation to load first.", true);
    const source = S.current, channel = S.channel;
    const messages = S.messages.filter(message => message.role === "user" && message.generated_by == null && message.content?.trim());
    if (!messages.length) return toast("Send your requests in this chat first, then move them to a topic.", true);
    const defaults = prefs.get("topicWorkflowV2", {}) || {};
    const field = (label, input) => h("label", { class: "topic-form-field" }, h("span", null, label), input);
    const name = h("input", { type: "text", required: true, maxlength: 160, value: "", placeholder: "e.g. Monthly accounts", "aria-label": "Topic name" });
    const workspace = h("input", { type: "text", spellcheck: "false", value: defaults.workspace || "", placeholder: "Leave blank for a private scratch workspace", "aria-label": "Workspace (optional)" });
    const sourceTopic = S.topics.find(item => item.topic_session_id === source);
    const parent = h("select", { "aria-label": "Parent topic" },
      h("option", { value: "" }, sourceTopic ? "This topic (" + topicPath(sourceTopic) + ")" : "Main Chat"),
      S.topics.filter(item => !["pending_approval", "rejected"].includes(item.status)).map(item => h("option", { value: item.id }, topicPath(item))));
    parent.value = sourceTopic?.id || "";
    const outline = topicOutlineEditor(defaults.outline || []);
    const selected = new Set([messages.some(message => message.id === selectedMessageId) ? selectedMessageId : messages[messages.length - 1].id]);
    const body = h("div", { class: "topic-proposal-form" }, field("Topic name", name), field("Parent topic", parent), field("Workspace (optional)", workspace),
      h("fieldset", { class: "topic-request-picker" }, h("legend", null, "Requests to move · one work item per message"), messages.map(message =>
        h("label", null, h("input", { type: "checkbox", checked: selected.has(message.id), onchange: event => {
          if (event.target.checked) selected.add(message.id); else selected.delete(message.id);
        } }), h("span", null, message.content)))),
      outline.element,
      h("p", { class: "muted" }, "Uses the official harnesses installed on your machine. Review the proposal before anything moves or starts."));
    const topic = await ask({ title: "Create a topic for focused work", text: "Everyday conversation stays in Main Chat. Pick the requests and workflow for this topic.", body, ok: "Review proposal",
      saveRequest: () => {
        if (channel !== S.channel || source !== S.current) throw new Error("This conversation changed. Close and create a fresh proposal.");
        if (!selected.size) throw new Error("Select at least one request to move.");
        return { type: "propose_topic", proposal: { source_session_id: source, message_ids: messages.filter(message => selected.has(message.id)).map(message => message.id),
          name: recordValue(name, "Enter a topic name."), parent_topic_id: parent.value || sourceTopic?.id || null,
          workspace: workspace.value.trim() || null, outline: outline.read() } };
      } });
    if (!topic || channel !== S.channel) return;
    prefs.set("topicWorkflowV2", { workspace: workspace.value.trim(), outline: outline.read() });
    storeTopic(topic);
    await openTopic(topic);
  }

  function renderSearch(hits, query) {
    const box = $("search-results");
    box.replaceChildren();
    if (hits == null) { box.classList.add("hidden"); return; }
    box.classList.remove("hidden");
    box.append(h("div", { class: "muted", style: "padding:0.25rem 0.5rem;font-size:0.75rem" },
      hits.length + " hit" + (hits.length === 1 ? "" : "s") + " for “" + query + "”"));
    const literal = query.trim();
    const words = literal.match(/[\p{Alphabetic}\p{Number}]+/gu);
    const terms = words && new Set(words.map((word) => foldMap(word, foldSearchCharacter).folded).filter(Boolean));
    for (const hit of hits) {
      const snippet = h("div");
      const text = hit.snippet || "";
      let cursor = 0;
      for (const [start, end] of searchHighlightRanges(text, literal, terms)) {
        if (cursor < start) snippet.append(text.slice(cursor, start));
        snippet.append(h("mark", null, text.slice(start, end)));
        cursor = end;
      }
      if (cursor < text.length) snippet.append(text.slice(cursor));
      box.append(h("div", {
        class: "hit", tabindex: 0,
        onclick: () => openSession(hit.session_id),
        onkeydown: (e) => { if (e.key === "Enter") openSession(hit.session_id); },
      }, h("div", { class: "meta" }, hit.session_name + " · #" + hit.seq + " " + hit.role), snippet));
    }
  }

  function foldSearchCharacter(ch) {
    // Standalone combining characters that unicode61 treats as diacritics.
    if (/[\u0300-\u0304\u0306-\u030c\u030f\u0311\u031b\u0323-\u0328\u032d-\u032e\u0330-\u0331]/u.test(ch)) return "";
    const base = ch.normalize("NFD").replace(/\p{M}/gu, "");
    // unicode61's precomposed diacritic table folds to ASCII Latin.
    const lower = (/^[A-Za-z]$/.test(base) ? base : ch).toLowerCase();
    return lower === "ς" ? "σ" : lower;
  }

  function searchHighlightRanges(text, literal, terms) {
    if (!terms) {
      // The daemon searches literally when the query has no word terms.
      const start = literal ? text.indexOf(literal) : -1;
      return start < 0 ? [] : [[start, start + literal.length]];
    }
    const { folded, from } = foldMap(text, foldSearchCharacter);
    const remaining = new Set(terms);
    const spans = [];
    // Approximate unicode61's boundaries within this snippet. Highlight the
    // first visible prefix per term, sharing a mark for overlapping prefixes.
    for (const { 0: word, index } of folded.matchAll(/[\p{L}\p{N}\p{Co}]+/gu)) {
      let end = index;
      for (const term of remaining) {
        if (!word.startsWith(term)) continue;
        const stop = index + term.length;
        // A prefix must cover the complete original character.
        if (from[stop] === from[stop - 1]) continue;
        remaining.delete(term);
        end = Math.max(end, stop);
      }
      if (end > index) spans.push([from[index], from[end]]);
      if (!remaining.size) break;
    }
    return spans;
  }

  // ---------------------------------------------------------- markdown
  // CommonMark-ish + GitHub extensions (tables, task lists, strikethrough,
  // alerts, autolinks), built as DOM nodes — never innerHTML — so model
  // output cannot inject markup. Mermaid diagrams render via a vendored,
  // lazily loaded mermaid.js into <img> data URLs (see renderMermaid).
  const SAFE_URL = /^(https?:|mailto:)/i;
  // Asterisk emphasis consumes escapes as pairs before closing delimiters: an
  // odd backslash run escapes the delimiter while an even run does not.
  const INLINE = new RegExp([
    /(`+)([^`]|[^`][\s\S]*?[^`])\1(?!`)/.source, // 1,2 code
    /\*\*(?=\S)((?:\\[\s\S]|[^\\])*?(?:\\\S|[^\\\s]))\*\*/.source, // 3 strong
    /__(?=\S)([\s\S]*?\S)__(?!\w)/.source, // 4 strong
    /~~(?=\S)([\s\S]*?\S)~~/.source, // 5 strike
    /\*(?=[^\s*])((?:\\[\s\S]|[^*\\]|\*\*[^*]+\*\*)*?(?:\\\S|[^\\\s*]))\*/.source, // 6 em
    /(?<![\w\\])_(?=[^\s_])([^_]*?[^\s_])_(?!\w)/.source, // 7 em
    /!\[([^\]]*)\]\(/.source, // 8 image label
    /\[((?:[^\]\\]|\\.)+)\]\(/.source, // 9 link label
    /<((?:[hH][tT][tT][pP][sS]?:\/\/|[mM][aA][iI][lL][tT][oO]:)[^>\s]+)>/.source, // 10 autolink
    /([hH][tT][tT][pP][sS]?:\/\/[^\s<]*[^\s<.,:;"'!?*_~])/.source, // 11 bare url
    /\$(?=\S)([^$\n]*?\S)\$(?!\d)/.source, // 12 math
    /\\([\\`*_{}\[\]()#+\-.!~|$<>])/.source, // 13 escape
  ].join("|"), "g");

  function inlineDestination(text, start) {
    let end = start, depth = 0;
    for (; end < text.length; end++) {
      const c = text[end], next = text[end + 1];
      if (/\s/.test(c)) break;
      if (c === "\\" && next !== undefined && "()\\".includes(next)) {
        end++;
      } else if (c === "(") {
        depth++;
      } else if (c === ")") {
        if (depth === 0) break;
        depth--;
      }
    }
    if (end === start || depth !== 0) return null;
    const closing = text[end] === ")" ? ")" : /^\s+"[^"]*"\)/.exec(text.slice(end))?.[0];
    if (!closing) return null;
    const url = text.slice(start, end).replace(/\\([()\\])/g, "$1");
    return { url, end: end + closing.length };
  }

  function bareUrlEnd(url) {
    let parens = 0, brackets = 0, end = url.length;
    for (const c of url) {
      if (c === "(") parens++;
      else if (c === ")") parens--;
      else if (c === "[") brackets++;
      else if (c === "]") brackets--;
    }
    for (; end > 0; end--) {
      const c = url[end - 1];
      if (c === ")" && parens < 0) parens++;
      else if (c === "]" && brackets < 0) brackets++;
      else if (!".,:;\"'!?*_~".includes(c)) break;
    }
    return end;
  }

  function link(url, kids) {
    if (!SAFE_URL.test(url)) return kids;
    return h("a", { href: url, target: "_blank", rel: "noopener noreferrer" }, kids);
  }
  // Keep persisted or streaming model text from creating unbounded recursive
  // work. Excess text remains readable verbatim instead of being discarded.
  const MD_DEPTH = 32, MD_INLINE_CHARS = 4096, MD_FORMAT_CHARS = 128 * 1024;
  function inline(text, depth = 0) {
    if (depth >= MD_DEPTH || text.length > MD_INLINE_CHARS) return [text];
    const out = [];
    const re = new RegExp(INLINE.source, "g");
    let last = 0;
    let m;
    while ((m = re.exec(text))) {
      if (m.index > last) out.push(text.slice(last, m.index));
      if (m[2] != null) out.push(h("code", null, m[1].length > 1 ? m[2].trim() : m[2]));
      else if (m[3] != null || m[4] != null) out.push(h("strong", null, inline(m[3] != null ? m[3] : m[4], depth + 1)));
      else if (m[5] != null) out.push(h("del", null, inline(m[5], depth + 1)));
      else if (m[6] != null || m[7] != null) out.push(h("em", null, inline(m[6] != null ? m[6] : m[7], depth + 1)));
      else if (m[8] != null || m[9] != null) {
        const dest = inlineDestination(text, re.lastIndex);
        if (!dest) {
          // Resume after the first literal character of a malformed prefix.
          out.push(text[m.index]);
          re.lastIndex = m.index + 1;
        } else {
          re.lastIndex = dest.end;
          // Remote images use a labelled link because of the image CSP.
          const kids = m[8] != null ? h("span", { class: "img-link" }, "🖼 " + (m[8] || "image"))
            : inline(m[9], depth + 1);
          out.push(link(dest.url, kids));
        }
      } else if (m[10] != null) out.push(link(m[10], m[10].replace(/^mailto:/i, "")));
      else if (m[11] != null) {
        const end = bareUrlEnd(m[11]);
        if (end > m[11].indexOf("://") + 3) {
          const url = m[11].slice(0, end);
          out.push(link(url, url));
          re.lastIndex = m.index + end;
        } else {
          out.push(m[11]);
        }
      } else if (m[12] != null) out.push(h("span", { class: "math" }, m[12]));
      else if (m[13] != null) out.push(m[13]);
      last = re.lastIndex;
    }
    if (last < text.length) out.push(text.slice(last));
    return out;
  }

  // Tiny tokenizer-highlighter, mirroring the TUI's (crates/formless-tui/src/highlight.rs).
  const W = (s) => new Set(s.split(" "));
  const JS_REGEX_KEYWORDS = W("await case delete do else extends in instanceof new of return throw typeof void yield");
  const JS_CONTEXTUAL_REGEX_KEYWORDS = W("await of yield");
  const JS_IDENT_CONTINUE = /[\p{ID_Continue}$\u200c\u200d]/u;
  const JS_LINE_END = /[\n\r\u2028\u2029]/;

  function jsxClosingTagEnd(code, start) {
    const closingTag = /(?:[\p{ID_Start}_$][\p{ID_Continue}$.:-]*)?\s*>/uy;
    closingTag.lastIndex = start + 1;
    if (!closingTag.test(code)) return start;
    const end = closingTag.lastIndex;
    // After a JSX tag, division needs a right operand. A member dot
    // or expression terminator instead permits a regex comparison.
    const regexSuffix = /^[dgimsuvy]*(?:\.(?![0-9])|\s*(?:[;,)\]}]|$))/;
    return code[end] === "/" && regexSuffix.test(code.slice(end + 1)) ? start : end;
  }

  function jsRegexEnd(code, start, state, lang) {
    const tokenEnd = (at) => {
      while (at > 0) {
        const commentStart = state.comments.get(at);
        if (commentStart !== undefined) at = commentStart;
        else if (/\s/.test(code[at - 1])) at--;
        else break;
      }
      return at;
    };
    const before = tokenEnd(start);
    const last = code[before - 1];
    // A closing brace or postfix operator can end a division operand.
    if (before === state.jsxTagEnd || last === "}" || code.slice(Math.max(0, before - 2), before) === "++" ||
        code.slice(Math.max(0, before - 2), before) === "--") return start;
    let keyword = "";
    if (before > 0 && !"=(,:[!&|?{;+-*%<>~^".includes(last)) {
      let wordStart = before;
      while (wordStart > 0 && JS_IDENT_CONTINUE.test(code[wordStart - 1])) wordStart--;
      keyword = code.slice(wordStart, before);
      const prefix = tokenEnd(wordStart);
      const previous = code[prefix - 1];
      const member = previous === "#" || (previous === "." && !code.startsWith("...", prefix - 3));
      const splitSurrogate = /[\udc00-\udfff]/.test(code[wordStart - 1] ?? "");
      if (!JS_REGEX_KEYWORDS.has(keyword) || member || splitSurrogate) return start;
    }
    const contextual = JS_CONTEXTUAL_REGEX_KEYWORDS.has(keyword);
    let inClass = false;
    for (let end = start + 1; end < code.length; end++) {
      const c = code[end];
      if (JS_LINE_END.test(c)) break;
      if (c === "\\") {
        end++;
        if (end === code.length || JS_LINE_END.test(code[end])) break;
      } else if (c === "[") inClass = true;
      else if (c === "]") inClass = false;
      else if (c === "/" && !inClass) {
        // A guessed closer after a contextual identifier can instead open
        // a real comment after the first divisor.
        if (contextual) {
          const adjacentComment = code.startsWith("//", end + 1) ||
            (code.startsWith("/*", end + 1) && code.indexOf("*/", end + 3) >= 0);
          if (!adjacentComment && (code[end + 1] === "/" ||
              (code[end + 1] === "*" && code.indexOf("*/", end + 2) >= 0))) return start;
        }
        end++;
        while (end < code.length && /[a-z]/i.test(code[end])) {
          if (!"dgimsuvy".includes(code[end])) return start;
          end++;
        }
        if (contextual) {
          let next = end;
          for (;;) {
            while (next < code.length && /\s/.test(code[next]) && !JS_LINE_END.test(code[next])) next++;
            if (!code.startsWith("/*", next)) break;
            const close = code.indexOf("*/", next + 2);
            if (close < 0 || JS_LINE_END.test(code.slice(next, close))) return end;
            next = close + 2;
          }
          let wordEnd = next;
          while (wordEnd < code.length && JS_IDENT_CONTINUE.test(code[wordEnd])) wordEnd++;
          const word = code.slice(next, wordEnd);
          const typescript = lang === "ts" || lang === "tsx" || lang === "typescript";
          if (word && word !== "in" && word !== "instanceof" &&
              !(typescript && (word === "as" || word === "satisfies"))) return start;
          if (code[next] === "'" || code[next] === '"') return start;
        }
        return end;
      }
    }
    return start;
  }


  const HL = {
    rust: { lc: ["//"], bc: ["/*", "*/"], q: "\"", kw: W("as async await break const continue crate dyn else enum extern fn for if impl in let loop match mod move mut pub ref return static struct trait type unsafe use where while"), lit: W("true false self Self None Some Ok Err super") },
    js: { lc: ["//"], bc: ["/*", "*/"], q: "\"'`", kw: W("as async await break case catch class const continue debugger default delete do else enum export extends finally for from function if implements import in instanceof interface let new of private protected public readonly return static switch throw try type typeof var void while with yield"), lit: W("true false null undefined NaN this super") },
    py: { lc: ["#"], hashAnywhere: true, q: "\"'", kw: W("and as assert async await break case class continue def del elif else except finally for from global if import in is lambda match nonlocal not or pass raise return try while with yield"), lit: W("True False None self cls") },
    go: { lc: ["//"], bc: ["/*", "*/"], q: "\"`'", kw: W("break case chan const continue default defer else fallthrough for func go goto if import interface map package range return select struct switch type var"), lit: W("true false nil iota") },
    sh: { lc: ["#"], q: "\"'", kw: W("alias case cd do done echo elif else end esac exit export fi for function if in local return set source then unset while"), lit: W("true false") },
    c: { lc: ["//"], bc: ["/*", "*/"], q: "\"'", kw: W("abstract auto break case catch class const continue default do else enum extends extern final for fun func goto if implements import include interface let namespace new override package private protected public return sizeof static struct switch template throw throws try typedef union using val var virtual void volatile while"), lit: W("true false null nullptr nil this self super") },
    json: { q: "\"", kw: W(""), lit: W("true false null") },
    jsonc: { lc: ["//"], bc: ["/*", "*/"], q: "\"", kw: W(""), lit: W("true false null") },
    json5: { lc: ["//"], bc: ["/*", "*/"], q: "\"'", kw: W(""), lit: W("true false null") },
    config: { lc: ["#"], q: "\"'", kw: W(""), lit: W("true false null yes no on off"), key: true },
    conf: { lc: ["#", ";"], q: "\"'", kw: W(""), lit: W("true false null yes no on off"), key: true },
    sql: { lc: ["--"], bc: ["/*", "*/"], q: "'", ci: true, kw: W("add all alter and as asc begin by case commit create default delete desc distinct drop else end exists foreign from group having in index inner insert into is join key left like limit not offset on or order outer primary references returning right select set table then union update values when where with"), lit: W("null true false") },
    lua: { lc: ["--"], q: "\"'", kw: W("and case class data do else elseif end for function if import in instance let local module not of or repeat return then until where while"), lit: W("true false nil True False Nothing") },
    html: { bc: ["<!--", "-->"], q: "\"'", kw: W(""), lit: W(""), tags: true },
    css: { bc: ["/*", "*/"], q: "\"'", kw: W("important media import supports keyframes"), lit: W("") },
  };
  HL.mysql = { ...HL.sql, lc: ["--", "#"], hashAnywhere: true, mysqlComments: true, q: "'\"`" };
  HL.powershell = { ...HL.sh, bc: ["<#", "#>"], hashAnywhere: true };
  HL.php = { ...HL.c, lc: ["//", "#"] };
  HL.scss = { ...HL.css, lc: ["//"] };
  const HL_ALIAS = {
    rs: "rust", javascript: "js", jsx: "js", mjs: "js", cjs: "js", ts: "js", typescript: "js", tsx: "js",
    python: "py", python3: "py", golang: "go", bash: "sh", zsh: "sh", fish: "sh", shell: "sh", console: "sh", ps: "powershell",
    dockerfile: "sh", docker: "sh", make: "sh", makefile: "sh", mk: "sh", ruby: "sh", rb: "sh", perl: "sh", pl: "sh", elixir: "sh", ex: "sh", exs: "sh", r: "sh",
    h: "c", cpp: "c", "c++": "c", cc: "c", hpp: "c", java: "c", kotlin: "c", kt: "c", swift: "c", cs: "c", csharp: "c", scala: "c", dart: "c", zig: "c",
    "c#": "c", "f#": "c", fsharp: "c", objc: "c", haskell: "lua", hs: "lua",
    yaml: "config", yml: "config", toml: "config", ini: "conf", env: "config", dotenv: "config", properties: "conf",
    cfg: "conf", editorconfig: "conf", gitconfig: "conf",
    sqlite: "sql", postgres: "sql", postgresql: "sql", xml: "html", svg: "html", vue: "html", svelte: "html", less: "scss",
  };
  const SHELL_ANSI_ESCAPES = {
    a: "\x07", b: "\b", e: "\x1b", E: "\x1b", f: "\f", n: "\n", r: "\r", t: "\t", v: "\v",
    "\\": "\\", "'": "'", '"': '"', "?": "?",
  };
  function shellCommentState(code, lang) {
    // Track shell words that can contain punctuation from command separators.
    const contexts = [];
    const hereDocs = [];
    let hereDocIndex = 0;
    let inHereDoc = false;
    let lineBoundary = true;
    let regexDepth = 0;
    const ansiQuotes = lang === "bash" || lang === "zsh";
    const context = () => contexts.at(-1) ?? "command";
    const replace = (mode) => { contexts[contexts.length - 1] = mode; };
    const escaped = (at) => {
      let start = at;
      while (start > 0 && code[start - 1] === "\\") start--;
      return (at - start) % 2 !== 0;
    };
    const commandPosition = (at) => {
      let end = at - 1;
      while (end >= 0 && code[end] !== "\n") {
        if (/\s/.test(code[end])) {
          end--;
          continue;
        }
        if (";|&({)".includes(code[end])) return !escaped(end);
        let start = end;
        while (start > 0 && !/[\s;|&({)]/.test(code[start - 1])) start--;
        if (!/^(if|elif|then|else|do|while|until|!|time)$/.test(code.slice(start, end + 1))) return false;
        end = start - 1;
      }
      return lineBoundary;
    };
    const track = (at) => {
      const current = code[at], previous = code[at - 1], next = code[at + 1];
      const mode = context();
      if ((current === "\\" && mode !== "regex-start") || escaped(at)) return;
      if (mode === "regex-start") {
        if (!/\s/.test(current)) {
          regexDepth = Number(current === "(");
          replace("regex");
        }
      } else if (mode === "regex") {
        if (current === "(") regexDepth++;
        else if (current === ")") regexDepth = Math.max(0, regexDepth - 1);
        else if (!regexDepth && /\s/.test(current)) replace("condition");
      } else if (mode === "condition" && current === "~" && previous === "=" &&
                 (!code[at - 2] || /\s/.test(code[at - 2]))) {
        replace("regex-start");
      } else if (mode === "condition" && current === "]" && next === "]" &&
                 (!previous || /\s/.test(previous)) && (!code[at + 2] || /[\s;|&)]/.test(code[at + 2]))) {
        contexts.pop();
      } else if ((mode === "parameter" && current === "}") ||
                 ((mode === "pattern" || mode === "command") && current === ")")) {
        contexts.pop();
      } else if ((mode === "case-pattern" || mode === "case-pattern-alternative") && current === ")") {
        replace("case-body");
      } else if (mode === "case-body" && previous === ";" &&
                 ((current === ";" && next !== "&") || current === "&")) {
        replace("case-pattern");
      } else if (current === "{" && previous === "$" && !escaped(at - 1)) {
        contexts.push("parameter");
      } else if (current === "(" && (mode === "pattern" || (previous && "@!?+*$".includes(previous) && !escaped(at - 1)))) {
        contexts.push(mode !== "pattern" && previous === "$" && next !== "(" ? "command" : "pattern");
      } else if (mode === "case-pattern" && "(|".includes(current)) {
        replace("case-pattern-alternative");
      } else if (current === "(" && (mode === "command" || mode === "case-body")) {
        contexts.push(next === "(" ? "pattern" : "command");
      } else if (current === "[" && next === "[" && (mode === "command" || mode === "case-body") &&
                 (!previous || /[\s;|&(]/.test(previous)) && (!code[at + 2] || /\s/.test(code[at + 2])) &&
                 commandPosition(at)) {
        contexts.push("condition");
      }
    };
    const trackWord = (at, word) => {
      if (lang === "fish" || !["case", "in", "esac"].includes(word)) return;
      const previous = code[at - 1], next = code[at + word.length];
      if ((previous && !/[\s;|&({)]/.test(previous)) ||
          (next && !/[\s;|&()]/.test(next)) || escaped(at - 1)) return;
      const mode = context();
      if ((mode === "command" || mode === "case-body") && word === "case" && commandPosition(at)) {
        contexts.push("case-header");
      } else if (mode === "case-header" && word === "in") {
        replace("case-pattern");
      } else if ((mode === "case-pattern" && word === "esac") ||
                 (mode === "case-body" && word === "esac" && commandPosition(at))) {
        contexts.pop();
      }
    };
    const comment = (at) => {
      const mode = context();
      if (mode === "parameter" || mode === "pattern" || mode === "regex") return false;
      const previous = code[at - 1], before = code[at - 2];
      if (at === 0 || previous === "\n") return lineBoundary;
      let separator = false;
      if (mode === "command" || mode === "case-body") separator = ";|&(".includes(previous);
      else if (mode === "case-pattern" || mode === "case-pattern-alternative") {
        separator = "|(".includes(previous) || before === ";" && ";&".includes(previous);
      }
      else if (mode === "condition" || mode === "regex-start") {
        separator = before === previous && "|&".includes(previous) && !escaped(at - 2);
      }
      return (/\s/.test(previous) || separator) && !escaped(at - 1);
    };
    const endLine = (at, endedByComment) => {
      if (code[at - 1] === "\r") at--;
      const continued = !endedByComment && escaped(at);
      lineBoundary = !continued || comment(at - 1);
      if (!continued) {
        if (context() === "regex" && !regexDepth) replace("condition");
        inHereDoc = hereDocIndex < hereDocs.length;
      }
    };
    const ansiEscape = (at) => {
      const c = code[at];
      if (!c || c === "\n" || c === "\r") return null;
      if (Object.hasOwn(SHELL_ANSI_ESCAPES, c)) return [SHELL_ANSI_ESCAPES[c], at + 1];
      if (c === "c") {
        const control = code[at + 1];
        if (!control || control === "\n" || control === "\r" || control.charCodeAt(0) > 127) return null;
        return [String.fromCharCode(control.toUpperCase().charCodeAt(0) ^ 0x40), at + 2];
      }
      const octal = /[0-7]/.test(c);
      const start = at + Number(!octal);
      const size = ({ x: 2, u: 4, U: 8 })[c];
      const digits = octal ? code.slice(start, start + 3).match(/^[0-7]+/) :
        size ? code.slice(start, start + size).match(/^[0-9a-f]+/i) : null;
      if (!digits) return ["\\", at];
      const value = parseInt(digits[0], octal ? 8 : 16);
      if (octal || c === "x") return [value % 256, start + digits[0].length];
      if (value > 0x10ffff || value >= 0xd800 && value <= 0xdfff) return null;
      return [String.fromCodePoint(value), start + digits[0].length];
    };
    const hereDoc = (at) => {
      if (lang === "fish" || !["command", "case-body"].includes(context()) || escaped(at) ||
          code[at + 1] !== "<" || code[at + 2] === "<" ||
          (code[at - 1] === "<" && !escaped(at - 1))) return at;
      let end = at + 2;
      const stripTabs = code[end] === "-";
      if (stripTabs) end++;
      while (code[end] === " " || code[end] === "\t") end++;
      const start = end;
      const bytes = [];
      const encoder = new TextEncoder();
      const append = (content) => {
        if (typeof content === "number") bytes.push(content);
        else bytes.push(...encoder.encode(content));
      };
      let quote = "";
      let quoted = false, ansiQuote = false;
      if (code[end] === "#") return at;
      while (end < code.length && code[end] !== "\n") {
        const c = String.fromCodePoint(code.codePointAt(end));
        if (!quote && /[\s;|&()<>]/.test(c)) break;
        end += c.length;
        if (quote === c) {
          quote = "";
          ansiQuote = false;
        } else if (!quote && c === "$" && (ansiQuotes && code[end] === "'" || lang === "bash" && code[end] === '"')) {
          quote = code[end++];
          quoted = true;
          ansiQuote = quote === "'";
        } else if (!quote && "\"'".includes(c)) {
          quote = c;
          quoted = true;
        } else if (c === "\\" && ansiQuote) {
          const escapedChar = ansiEscape(end);
          if (!escapedChar) return at;
          append(escapedChar[0]);
          end = escapedChar[1];
        } else if (c === "\\" && quote !== "'" && (!quote || /[\\"$`]/.test(code[end] ?? ""))) {
          if (end === code.length || code[end] === "\n") return at;
          const escapedChar = String.fromCodePoint(code.codePointAt(end));
          append(escapedChar);
          end += escapedChar.length;
          quoted = true;
        } else append(c);
      }
      if (end === start || quote || bytes.includes(0)) return at;
      let delimiter;
      try {
        delimiter = new TextDecoder("utf-8", { fatal: true, ignoreBOM: true }).decode(Uint8Array.from(bytes));
      } catch {
        return at;
      }
      hereDocs.push({ delimiter, stripTabs, quoted, continued: "" });
      return end;
    };
    const literalLine = (at) => {
      if (!inHereDoc) return null;
      const next = code.indexOf("\n", at);
      const end = next < 0 ? code.length : next;
      const doc = hereDocs[hereDocIndex];
      const contentEnd = end - Number(code[end - 1] === "\r");
      if (!doc.quoted && escaped(contentEnd)) {
        doc.continued += code.slice(at, contentEnd - 1);
        return end;
      }
      const line = doc.continued + code.slice(at, contentEnd);
      if ((doc.stripTabs ? line.replace(/^\t+/, "") : line) === doc.delimiter) {
        hereDocIndex++;
        if (hereDocIndex === hereDocs.length) {
          hereDocIndex = hereDocs.length = 0;
          inHereDoc = false;
        }
      } else doc.continued = "";
      return end;
    };
    return { track, trackWord, comment, endLine, hereDoc, literalLine, escaped, ansiQuotes };
  }

  function powershellQuoteEnd(code, from, quote) {
    for (let i = from; i < code.length; i++) {
      const c = code[i];
      if ((quote === '"' && c === "`") || (c === quote && code[i + 1] === quote)) i++;
      else if (c === quote) return i + 1;
    }
    return code.length;
  }

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
    const shell = ["sh", "bash", "zsh", "fish", "shell", "console"].includes(lang)
      ? shellCommentState(code, lang) : null;
    const kotlin = lang === "kotlin" || lang === "kt";
    const js = L === HL.js ? { comments: new Map(), jsxTagEnd: -1 } : null;
    const toml = lang === "toml";
    const php = L === HL.php;
    const shellLiteralQuotes = /^(?:sh|bash|zsh|shell|console)$/.test(lang);
    const out = [];
    let plain = "";
    const powershell = L === HL.powershell;
    const tok = (cls, s) => {
      if (plain) { out.push(plain); plain = ""; }
      out.push(h("span", { class: cls }, s));
    };
    const word = /[\p{L}\p{N}_$]/u;
    const yaml = lang === "yaml" || lang === "yml";
    let yamlScalar = null;
    let i = 0;
    let lineStart = true;
    let endedByComment = false;
    const postgresDollarQuotes = /^(?:sql|postgres|postgresql)$/.test(lang);
    while (i < code.length) {
      if (lineStart && shell) {
        const end = shell.literalLine(i);
        if (end !== null) {
          const next = end + Number(end < code.length);
          plain += code.slice(i, next);
          i = next;
          continue;
        }
      }
      const c = code[i];
      if (L === HL.js && (c === "." || /\d/.test(c))) {
        lineStart = false;
        if (c === "." && code.startsWith("...", i)) {
          plain += "...";
          i += 3;
          continue;
        }
        const before = code[i - 1];
        const boundary = !before || /\s/.test(before) || before.charCodeAt(0) < 128 && !word.test(before);
        if (c === "." ? /\d/.test(code[i + 1] ?? "") : boundary) {
          const end = javascriptNumberEnd(code, i);
          tok("tok-num", code.slice(i, end));
          i = end;
          continue;
        }
        if (c !== ".") {
          plain += c;
          i++;
          continue;
        }
      }
      if (yaml && lineStart) {
        const newline = code.indexOf("\n", i);
        const end = newline === -1 ? code.length : newline;
        const line = code.slice(i, end);
        if (yamlScalar) {
          const indent = line.length - line.replace(/^ +/, "").length;
          const blank = !line.trim();
          if (blank || indent >= (yamlScalar.contentIndent ?? yamlScalar.headerIndent + 1)) {
            if (!blank) yamlScalar.contentIndent ??= indent;
            if (line) tok("tok-str", line);
            if (newline !== -1) plain += "\n";
            i = newline === -1 ? end : end + 1;
            continue;
          }
        }
        yamlScalar = yamlScalarHeader(line);
      }
      if (lineStart && L.key) {
        const m = code.slice(i).match(/^([ \t]*)([\w.\-"'\[\] ]+?)(?=(\s*)[:=])/);
        const key = m?.[2].trim();
        // A multiline TOML array value can start where a key normally would.
        if (key && !(L.lc || []).some((p) => key.startsWith(p)) &&
            (!toml || !/^(['"])\1\1/.test(key) && !/[^\t ]/.test(m[3]))) {
          plain += m[1];
          tok("tok-key", m[2]);
          i += m[0].length;
          lineStart = false;
          continue;
        }
      }
      lineStart = c === "\n";
      if (lineStart && shell) {
        shell.endLine(i, endedByComment);
        endedByComment = false;
      }
      if (L.bc && code.startsWith(L.bc[0], i)) {
        const end = code.indexOf(L.bc[1], i + L.bc[0].length);
        const stop = end < 0 ? code.length : end + L.bc[1].length;
        js?.comments.set(stop, i);
        tok("tok-com", code.slice(i, stop));
        i = stop;
        continue;
      }
      if (powershell && c === "@" && (code[i + 1] === '"' || code[i + 1] === "'")) {
        const newline = code.indexOf("\n", i + 2);
        const lineEnd = newline === -1 ? code.length : newline;
        if (!code.slice(i + 2, lineEnd).trim()) {
          const end = code.indexOf("\n" + code[i + 1] + "@", lineEnd);
          const stop = end === -1 ? code.length : end + 3;
          tok("tok-str", code.slice(i, stop));
          i = stop;
          continue;
        }
      }
      if (powershell && L.q.includes(c)) {
        const stop = powershellQuoteEnd(code, i + 1, c);
        tok("tok-str", code.slice(i, stop));
        i = stop;
        continue;
      }
      const hashComment = c !== "#" || (php ? code[i + 1] !== "[" :
        (shell ? shell.comment(i) : L.hashAnywhere || i === 0 || /\s/.test(code[i - 1]) || toml && L.q.includes(code[i - 1])));
      if (hashComment && (L.lc || []).some(p => code.startsWith(p, i) &&
          (!L.mysqlComments || p !== "--" || i + 2 === code.length || /[\x00-\x20\x7f]/.test(code[i + 2])))) {
        const end = code.indexOf("\n", i);
        const lineEnd = end < 0 ? code.length : end;
        const tag = php ? code.slice(i, lineEnd).indexOf("?>") : -1;
        const stop = tag < 0 ? lineEnd : i + tag;
        js?.comments.set(stop, i);
        tok("tok-com", code.slice(i, stop));
        i = stop;
        endedByComment = true;
        continue;
      }
      if (shell && c !== "\n") {
        const end = c === "<" ? shell.hereDoc(i) : i;
        if (end !== i) {
          plain += code.slice(i, end);
          i = end;
          continue;
        }
        shell.track(i);
      }
      const prevWord = i > 0 && word.test(code[i - 1]);
      if (L === HL.rust && !prevWord &&
          (c === "r" || ((c === "b" || c === "c") && code[i + 1] === "r"))) {
        const prefixEnd = i + (c === "r" ? 1 : 2);
        let opener = prefixEnd;
        while (code[opener] === "#") opener++;
        if (code[opener] === '"') {
          const delimiter = '"' + code.slice(prefixEnd, opener);
          const end = code.indexOf(delimiter, opener + 1);
          const stop = end < 0 ? code.length : end + delimiter.length;
          tok("tok-str", code.slice(i, stop));
          i = stop;
          continue;
        }
      }
      if (postgresDollarQuotes && c === "$" && (i === 0 || !/[\p{L}\p{N}_]/u.test(code[i - 1]))) {
        const opener = /^\$(?:[a-zA-Z_\u0080-\uffff][a-zA-Z_0-9\u0080-\uffff]*)?\$/.exec(code.slice(i));
        if (opener) {
          const delimiter = opener[0];
          const closer = code.indexOf(delimiter, i + delimiter.length);
          const stop = closer === -1 ? code.length : closer + delimiter.length;
          tok("tok-str", code.slice(i, stop));
          i = stop;
          continue;
        }
      }
      if (L.q.includes(c) && (!shell || !shell.escaped(i))) {
        const triple = (toml || L === HL.py || (c === '"' && (lang === "java" || kotlin))) &&
          code.startsWith(c.repeat(3), i);
        const delimiter = triple ? c.repeat(3) : c;
        const multiline = shell || triple || c === "`" || L.mysqlComments;
        const ansiQuote = shell && (shell.ansiQuotes || shellLiteralQuotes) &&
          c === "'" && code[i - 1] === "$" && !shell.escaped(i - 1);
        const escapes = !(L === HL.go && c === "`") && !(kotlin && triple) &&
          !(toml && c === "'") && !(L.mysqlComments && c === "`") &&
          !(shell && lang !== "fish" && c === "'" && !ansiQuote);
        let j = i + delimiter.length;
        while (j < code.length && !code.startsWith(delimiter, j) && (code[j] !== "\n" || multiline)) {
          j += escapes && code[j] === "\\" ? 2 : 1;
        }
        let stop = Math.min(j + delimiter.length, code.length);
        // Kotlin includes any content quotes adjacent to the raw closer.
        if (kotlin && triple) while (code[stop] === '"') stop++;
        if (toml && triple) {
          const trailing = stop + 2;
          while (stop < trailing && code[stop] === c) stop++;
        }
        tok("tok-str", code.slice(i, stop));
        i = stop;
        continue;
      }
      if (/\d/.test(c) && !prevWord) {
        const m = code.slice(i).match(/^[\d][\w.]*/);
        // A raw literal can start immediately after a Rust range operator.
        const range = L === HL.rust ? m[0].indexOf("..") : -1;
        const number = range < 0 ? m[0] : m[0].slice(0, range);
        tok("tok-num", number);
        i += number.length;
        continue;
      }
      if (c === "/" && js) {
        const tag = (lang === "jsx" || lang === "tsx") && code[i - 1] === "<"
          ? jsxClosingTagEnd(code, i) : i;
        if (tag !== i) js.jsxTagEnd = tag;
        else {
          const stop = jsRegexEnd(code, i, js, lang);
          if (stop !== i) {
            tok("tok-str", code.slice(i, stop));
            i = stop;
            continue;
          }
        }
      }
      if (L.tags && c === "<") {
        const m = code.slice(i).match(/^<\/?[\w:-]+/);
        if (m) { tok("tok-kw", m[0]); i += m[0].length; continue; }
      }
      if (/[\p{L}_$]/u.test(c) && !prevWord) {
        const m = code.slice(i).match(/^[\p{L}\p{N}_$]+/u);
        const w = m[0];
        shell?.trackWord(i, w);
        const k = L.ci ? w.toLowerCase() : w;
        const cls = L.kw.has(k) ? "tok-kw" : L.lit.has(k) ? "tok-lit" : code[i + w.length] === "(" ? "tok-fn"
          : !L.ci && !L.tags && /^\p{Lu}/u.test(w) && /\p{Ll}/u.test(w) ? "tok-type" : null;
        if (cls) tok(cls, w); else plain += w;
        const previous = code[i - 1];
        i += w.length;
        if (L === HL.scss && w.toLowerCase() === "url" && code[i] === "(" && previous !== "-") {
          const url = /^\((?:\\[^\r\n]|[^\\()'"\r\n])*\)/.exec(code.slice(i));
          if (url) {
            plain += url[0];
            i += url[0].length;
          }
        }
        continue;
      }
      plain += c;
      i++;
    }
    if (plain) out.push(plain);
    return out;
  }

  function yamlScalarHeader(line) {
    let rest = line.replace(/^ +/, "");
    let headerIndent = line.length - rest.length;
    while (/^-[ \t]/.test(rest)) {
      headerIndent = line.length - rest.length;
      rest = rest.replace(/^-[ \t]+/, "");
    }
    if (!/^[|>]/.test(rest)) {
      headerIndent = line.length - rest.length;
      let quote = rest[0] === "'" || rest[0] === '"' ? rest[0] : null;
      let colon = -1;
      for (let i = quote ? 1 : 0; i < rest.length; i++) {
        const c = rest[i];
        if (quote) {
          if ((quote === '"' && c === "\\") || (quote === "'" && c === "'" && rest[i + 1] === "'")) i++;
          else if (c === quote) quote = null;
        } else if (c === "#" && (i === 0 || /\s/.test(rest[i - 1]))) {
          break;
        } else if (i > 0 && c === ":" && /[ \t]/.test(rest[i + 1] || "")) {
          colon = i;
          break;
        }
      }
      if (colon === -1) return null;
      rest = rest.slice(colon + 1).replace(/^[ \t]+/, "");
    }
    const match = /^[|>]([1-9][+-]?|[+-][1-9]?)?(?:[ \t]+(?:#.*)?)?\r?$/.exec(rest);
    if (!match) return null;
    const digit = match[1]?.match(/[1-9]/);
    return { headerIndent, contentIndent: digit ? headerIndent + Number(digit[0]) : null };
  }

  function javascriptNumberEnd(code, start) {
    const radix = code[start] === "0" && {
      x: /[0-9a-f]/i, o: /[0-7]/, b: /[01]/,
    }[code[start + 1]?.toLowerCase()];
    const digitsEnd = (at, digit = /[0-9]/) => {
      while (at < code.length && digit.test(code[at])) {
        at++;
        if (code[at] === "_" && at + 1 < code.length && digit.test(code[at + 1])) at++;
      }
      return at;
    };
    if (radix) {
      const digits = start + 2;
      const end = digitsEnd(digits, radix);
      return end === digits ? start + 1 : end + Number(code[end] === "n");
    }
    let end = digitsEnd(start);
    if (code[start] === "0" && end > start + 1 && /^[0-7]+$/.test(code.slice(start, end))) return end;
    const fraction = code[end] === ".";
    if (fraction) end = digitsEnd(end + 1);
    let exponent = false;
    if (code[end] === "e" || code[end] === "E") {
      const digits = end + 1 + Number(code[end + 1] === "+" || code[end + 1] === "-");
      const exponentEnd = digitsEnd(digits);
      if (exponentEnd > digits) {
        end = exponentEnd;
        exponent = true;
      }
    }
    return end + Number(!fraction && !exponent && code[end] === "n");
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
  const diagramStates = new WeakMap();
  const darkScheme = window.matchMedia("(prefers-color-scheme: dark)");
  const reducedMotion = window.matchMedia("(prefers-reduced-motion: reduce)");
  let configuredAnimations = true;
  function applyMotion() {
    const setting = S.settings?.tui?.animations;
    if (typeof setting === "boolean") configuredAnimations = setting;
    document.documentElement.toggleAttribute("data-reduced-motion", reducedMotion.matches || !configuredAnimations);
  }
  reducedMotion.addEventListener("change", applyMotion);
  applyMotion();
  /** Scale the browser's default root size (85–200%), per device. */
  function readingScale(scale) {
    return Number.isFinite(scale) ? Math.round(Math.min(2, Math.max(0.85, scale)) * 100) / 100 : 1;
  }
  function setReading(scale, density) {
    if (scale != null) S.readScale = readingScale(scale);
    if (density) S.density = density;
    prefs.set("readScale", S.readScale);
    prefs.set("density", S.density);
    applyReading();
    if (S.view === "settings") renderView();
  }
  function applyReading() {
    S.readScale = readingScale(S.readScale);
    document.documentElement.style.setProperty("--read-scale", String(S.readScale));
    closePopover();
    if (S.density === "compact") document.documentElement.dataset.density = "compact";
    else delete document.documentElement.dataset.density;
  }
  S.readScale = prefs.get("readScale", 1);
  S.density = prefs.get("density", "comfortable");
  applyReading();
  /** Dark unless the user picked light, or picked nothing and the system is light. */
  const isDark = () => {
    const t = document.documentElement.dataset.theme;
    return t ? t === "dark" : darkScheme.matches;
  };
  /** Auto (the system's), Light or Dark; remembered in this browser. */
  function applyTheme(t) {
    if (t === "light" || t === "dark") document.documentElement.dataset.theme = t;
    else delete document.documentElement.dataset.theme;
    $("theme").value = t === "light" || t === "dark" ? t : "auto";
    const color = getComputedStyle(document.documentElement).getPropertyValue("--bg").trim();
    document.querySelectorAll('meta[name="theme-color"]').forEach((meta) => meta.setAttribute("content", color));
  }
  applyTheme(prefs.get("theme", "auto"));
  $("theme").addEventListener("change", (e) => {
    prefs.set("theme", e.target.value);
    applyTheme(e.target.value);
    mermaidCache.clear();
    refreshDiagrams();
  });
  darkScheme.addEventListener("change", () => {
    if (document.documentElement.dataset.theme) return;
    applyTheme("auto");
    mermaidCache.clear();
    refreshDiagrams();
  });
  function mermaidLoadError(message) {
    return Object.assign(new Error(message), { retryableMermaidLoad: true });
  }
  function loadMermaid() {
    if (!mermaidReady) {
      const script = h("script", { src: MERMAID_SRC });
      const job = new Promise((resolve, reject) => {
        script.onload = () => {
          const mermaid = window.mermaid;
          if (mermaid && ["initialize", "parse", "render"].every((key) => typeof mermaid[key] === "function")) resolve(mermaid);
          else reject(mermaidLoadError("mermaid missing"));
        };
        script.onerror = () => reject(mermaidLoadError("could not load " + MERMAID_SRC));
      });
      mermaidReady = job;
      job.catch(() => {
        if (mermaidReady === job) mermaidReady = null;
        script.remove();
      });
      document.head.append(script);
    }
    return mermaidReady;
  }
  const mermaidTheme = () => isDark() ? "dark" : "default";
  function refreshDiagrams(root = document) {
    for (const box of root.querySelectorAll(".diagram")) diagramStates.get(box)?.refresh();
  }
  /** Pie slices in the app palette (mermaid's dark pie is near-black). */
  function pieColors() {
    const css = getComputedStyle(document.documentElement);
    const v = (n) => css.getPropertyValue("--" + n).trim();
    const vars = { pieStrokeColor: v("bg"), pieOuterStrokeColor: v("line"), pieSectionTextColor: v("bg"), pieTitleTextColor: v("text"), pieLegendTextColor: v("text") };
    ["accent", "user", "warn", "think", "err", "muted"].forEach((n, i) => { vars["pie" + (i + 1)] = v(n); });
    return vars;
  }
  function renderMermaid(src, theme = mermaidTheme()) {
    const key = theme + "\n" + src;
    if (!mermaidCache.has(key)) {
      const colors = pieColors();
      const job = mermaidQueue.then(async () => {
        const mermaid = await loadMermaid();
        mermaid.initialize({
          startOnLoad: false, securityLevel: "strict", theme, htmlLabels: false,
          flowchart: { htmlLabels: false }, fontFamily: MERMAID_FONT, fontSize: 16,
          themeVariables: colors,
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
          return { url: "data:image/svg+xml;charset=utf-8," + encodeURIComponent(svg), svg, theme, width: vb ? Math.ceil(+vb[1]) : 0, height: vb ? Math.ceil(+vb[2]) : 0 };
        } finally {
          measure.replaceChildren();
        }
      });
      mermaidQueue = job.catch(() => {});
      mermaidCache.set(key, job);
      job.catch((error) => {
        if (error?.retryableMermaidLoad && mermaidCache.get(key) === job) mermaidCache.delete(key);
      });
      if (mermaidCache.size > 200) mermaidCache.delete(mermaidCache.keys().next().value);
    }
    return mermaidCache.get(key);
  }
  function diagramBlock(src, closed) {
    const code = h("pre", { class: "diagram-src" }, h("code", null, src));
    const view = h("div", { class: "diagram-view" }, h("span", { class: "muted" }, closed ? "Rendering diagram…" : "Waiting for the diagram to finish…"));
    const box = h("div", { class: "codeblock diagram" });
    const errorMessage = h("div", { class: "diagram-error error", role: "status", hidden: true });
    let wantedSource = !closed;
    let generation = 0, theme = null, pending = false;
    const setSource = (on) => { box.classList.toggle("show-src", on); toggle.textContent = on ? "Diagram" : "Source"; };
    const toggle = h("button", { class: "btn ghost small", type: "button", onclick: () => { wantedSource = !box.classList.contains("show-src"); setSource(wantedSource); } }, "Source");
    const save = h("button", { class: "btn ghost small", type: "button", hidden: true }, "SVG");
    const retry = h("button", { class: "btn ghost small", type: "button", hidden: true, onclick: () => { if (!pending) render(true); } }, "Retry");
    const update = (change) => {
      if ($("transcript").contains(box)) withTranscriptLayout(change);
      else change();
    };
    const retryState = (visible) => {
      if (!visible && document.activeElement === retry) toggle.focus({ preventScroll: true });
      retry.hidden = !visible;
      retry.textContent = "Retry";
      retry.removeAttribute("aria-disabled");
    };
    const render = (force = false) => {
      const nextTheme = mermaidTheme();
      if (!closed || (!force && theme === nextTheme)) return;
      theme = nextTheme;
      const attempt = ++generation;
      const current = () => attempt === generation && theme === nextTheme && mermaidTheme() === nextTheme;
      pending = true;
      view.setAttribute("aria-busy", "true");
      if (!retry.hidden) { retry.textContent = "Retrying…"; retry.setAttribute("aria-disabled", "true"); }
      renderMermaid(src, nextTheme).then((d) => {
        if (!current()) return;
        const img = h("img", { src: d.url, alt: "Mermaid diagram", class: "diagram-img", "data-theme": d.theme, title: "Click to zoom" });
        if (d.width) img.style.width = d.width + "px";
        if (d.width && d.height) { img.width = d.width; img.height = d.height; }
        img.addEventListener("click", () => box.classList.toggle("zoom"));
        img.addEventListener("load", () => {
          // The decoded image may already have resized. Keep the reading
          // anchor captured before that reflow, even after a recent scroll.
          if (current() && $("transcript").contains(img)) settleReadingLayout();
        }, { once: true });
        update(() => {
          view.replaceChildren(img);
          errorMessage.hidden = true;
          view.removeAttribute("aria-busy");
          save.hidden = false;
          save.onclick = () => downloadBlob("diagram.svg", new Blob([d.svg], { type: "image/svg+xml" }));
          retryState(false);
          setSource(wantedSource);
        });
        pending = false;
      }, (error) => {
        if (!current()) return;
        update(() => {
          view.replaceChildren();
          errorMessage.textContent = "Diagram not rendered: " + (error?.message || error);
          errorMessage.hidden = false;
          view.removeAttribute("aria-busy");
          if (document.activeElement === save) toggle.focus({ preventScroll: true });
          save.hidden = true;
          retryState(!!error?.retryableMermaidLoad);
          setSource(true);
        });
        pending = false;
      });
    };
    box.append(h("div", { class: "lang" }, h("span", null, "mermaid"), h("span", { class: "spacer" }), toggle, save, retry, copyButton(src)), errorMessage, view, code);
    diagramStates.set(box, { refresh: () => render() });
    if (!closed) setSource(true);
    else render();
    return box;
  }

  const FENCE = /^(\s*)(`{3,}|~{3,})\s*([^\s`{,]*)/;
  const HEADING = /^\s{0,3}(#{1,6})\s+(.*?)(?:\s+#+\s*)?$/;
  const SETEXT = /^ {0,3}(=+|-+)[ \t]*$/;
  // Reuse collected quote spans when a Setext heading returns an outer tail.
  const quoteBuffers = new WeakMap();
  // Message headings nest below the Session title (h2). ARIA levels preserve
  // depths beyond HTML's h6; source-depth classes control readable styling.
  const markdownHeading = (level, content) => h(level <= 4 ? "h" + (level + 2) : "div",
    { class: "md-heading md-heading-" + level, role: "heading", "aria-level": level + 2, "data-heading-level": level }, content);
  const RULE = /^\s{0,3}([-*_])(?:\s*\1){2,}\s*$/;
  const ITEM = /^(\s*)([-*+]|\d{1,9}[.)])(\s+|$)(.*)$/;
  const QUOTE = /^\s{0,3}>/;
  const MATH_BLOCK = /^\s*\$\$/;
  const TABLE_SEP = /^\s*\|?\s*:?-+:?\s*(\|\s*:?-+:?\s*)*\|?\s*$/;
  const ALERTS = { NOTE: "ℹ Note", TIP: "★ Tip", IMPORTANT: "‼ Important", WARNING: "⚠ Warning", CAUTION: "✖ Caution" };
  const indentOf = (l) => l.match(/^\s*/)[0].replace(/\t/g, "    ").length;
  const tableStart = (lines, i) => i + 1 < lines.length && lines[i].includes("|")
    && TABLE_SEP.test(lines[i + 1])
    && splitRow(lines[i]).length === splitRow(lines[i + 1]).length;
  const startsBlock = (lines, i) => {
    const l = lines[i];
    return FENCE.test(l) || HEADING.test(l) || RULE.test(l) || ITEM.test(l) || QUOTE.test(l) || MATH_BLOCK.test(l)
      || tableStart(lines, i);
  };
  // Shared delimiter rules keep persisted and streamed block boundaries aligned.
  function fenceCloses(line, marker) {
    return new RegExp("^\\s*" + marker[0] + "{" + marker.length + ",}\\s*$").test(line);
  }
  function mathBlockOpen(line, open = false) {
    return open ? !line.includes("$$") : MATH_BLOCK.test(line) && !line.replace(MATH_BLOCK, "").includes("$$");
  }
  function splitRow(line) {
    let s = line.trim();
    if (s.startsWith("|")) s = s.slice(1);
    if (s.endsWith("|") && !s.endsWith("\\|")) s = s.slice(0, -1);
    return s.split(/(?<!\\)\|/).map((c) => c.trim().replace(/\\\|/g, "|"));
  }

  /** Append blocks and return the next line index (lazy Setext tails remain outside). */
  function blocks(lines, root, depth = 0, lazy = [], start = 0) {
    if (depth >= MD_DEPTH) { root.append(h("pre", { class: "md-plain" }, lines.slice(start).join("\n"))); return lines.length; }
    let i = start;
    while (i < lines.length) {
      const line = lines[i];
      if (!line.trim()) { i++; continue; }
      const fence = line.match(FENCE);
      if (fence) {
        const marker = fence[2];
        const lead = fence[1].length;
        const buf = [];
        i++;
        while (i < lines.length && !fenceCloses(lines[i], marker)) {
          buf.push(lines[i].slice(Math.min(lead, indentOf(lines[i]))));
          i++;
        }
        const closed = i < lines.length;
        i++;
        const lang = fence[3];
        root.append(lang.toLowerCase() === "mermaid" ? diagramBlock(buf.join("\n"), closed) : codeBlock(buf.join("\n"), lang));
        continue;
      }
      if (MATH_BLOCK.test(line)) {
        const buf = [line.replace(MATH_BLOCK, "")];
        let open = mathBlockOpen(line);
        i++;
        while (i < lines.length && open) {
          const next = lines[i++];
          buf.push(next);
          open = mathBlockOpen(next, true);
        }
        root.append(h("pre", { class: "math block" }, buf.join("\n").replace(/\$\$\s*$/, "").trim()));
        continue;
      }
      const head = line.match(HEADING);
      if (head) { root.append(markdownHeading(head[1].length, inline(head[2]))); i++; continue; }
      if (RULE.test(line)) { root.append(h("hr")); i++; continue; }
      if (tableStart(lines, i)) {
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
        let quote = quoteBuffers.get(lines);
        if (!quote || i < quote.start || i >= quote.end) {
          const begin = i, buf = [], bufLazy = [];
          let end = i;
          while (end < lines.length && (QUOTE.test(lines[end]) || (lines[end].trim() && buf.length && !startsBlock(lines, end)))) {
            bufLazy.push(lazy[end] || !QUOTE.test(lines[end]));
            buf.push(lines[end++].replace(/^\s{0,3}>\s?/, ""));
          }
          quote = { start: begin, end, lines: buf, lazy: bufLazy };
          quoteBuffers.set(lines, quote);
        }
        const from = i - quote.start;
        const alert = quote.lines[from].trim().match(/^\[!(NOTE|TIP|IMPORTANT|WARNING|CAUTION)\]$/i);
        const q = h("blockquote", { class: alert ? "callout " + alert[1].toLowerCase() : null });
        if (alert) q.append(h("div", { class: "callout-title" }, ALERTS[alert[1].toUpperCase()]));
        const consumed = blocks(quote.lines, q, depth + 1, quote.lazy, from + (alert ? 1 : 0));
        root.append(q);
        i = quote.start + consumed;
        if (i < quote.end && lazy[i]) return i;
        continue;
      }
      if (ITEM.test(line)) {
        const result = list(lines, i, root, depth, lazy);
        i = result.next;
        if (result.stopped && lazy[i]) return i;
        continue;
      }
      const content = [...inline(line.trim())];
      let level = null;
      i++;
      while (i < lines.length && lines[i].trim()) {
        // Lazy container continuations are paragraph text, not underlines.
        const underline = !lazy[i] && lines[i].match(SETEXT);
        if (underline) { level = underline[1][0] === "=" ? 1 : 2; i++; break; }
        if (startsBlock(lines, i)) break;
        content.push(h("br"), ...inline(lines[i++].trim()));
      }
      root.append(level ? markdownHeading(level, content) : h("p", null, content));
      // A heading closes its paragraph; the following lazy line belongs outside.
      if (level && lazy[i]) return i;
    }
    return lines.length;
  }

  /** A list plus its next index and whether a Setext heading ended lazy collection. */
  function list(lines, i, root, depth = 0, lazy = []) {
    const first = lines[i].match(ITEM);
    const base = indentOf(first[1]);
    const ordered = /\d/.test(first[2]);
    const el = h(ordered ? "ol" : "ul");
    if (ordered && parseInt(first[2], 10) !== 1) el.setAttribute("start", String(parseInt(first[2], 10)));
    // A marker before the current item's content column is a sibling, even
    // when its indentation varies within the ordinary 0–3-space margin.
    const sibling = (m, limit) => m && /\d/.test(m[2]) === ordered
      && (indentOf(m[1]) <= 3 || indentOf(m[1]) === base) && indentOf(m[1]) < limit;
    let siblingLimit = Infinity;
    let loose = false, stopped = false;
    while (i < lines.length) {
      const itemStart = i;
      let looseAfter = Infinity;
      const m = lines[i].match(ITEM);
      if (RULE.test(lines[i]) || !sibling(m, siblingLimit)) break;
      const contentIndent = indentOf(m[1]) + m[2].length + Math.max(1, Math.min(4, m[3].length));
      siblingLimit = contentIndent;
      const body = [m[4]], bodyLazy = [lazy[i]];
      i++;
      while (i < lines.length) {
        const l = lines[i];
        if (!l.trim()) {
          let j = i + 1;
          while (j < lines.length && !lines[j].trim()) j++;
          if (j < lines.length && indentOf(lines[j]) >= contentIndent) {
            if (!ITEM.test(lines[j])) looseAfter = Math.min(looseAfter, body.length + 1);
            body.push(""); bodyLazy.push(false); i++; continue;
          }
          const next = j < lines.length && lines[j].match(ITEM);
          if (sibling(next, contentIndent) && !RULE.test(lines[j])) {
            looseAfter = Math.min(looseAfter, body.length);
            i = j;
          }
          break;
        }
        const ind = indentOf(l);
        if (ind >= contentIndent) {
          bodyLazy.push(lazy[i]);
          body.push(" ".repeat(Math.max(0, ind - contentIndent)) + l.trim()); i++; continue;
        }
        if (ITEM.test(l) || startsBlock(lines, i) || !body[body.length - 1].trim()) break;
        bodyLazy.push(true);
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
      const consumed = blocks(body, li, depth + 1, bodyLazy);
      // Ignore blank separators collected beyond a heading's rejected lazy tail.
      if (looseAfter <= consumed) loose = true;
      el.append(li);
      if (consumed < body.length) { i = itemStart + consumed; stopped = true; break; }
    }
    if (loose) el.classList.add("loose");
    root.append(el);
    return { next: i, stopped };
  }

  function markdown(text) {
    text = String(text || "");
    const root = h("div", { class: "md bubble" });
    if (text.length > MD_FORMAT_CHARS) { root.append(h("pre", { class: "md-plain" }, text)); return root; }
    blocks(text.replace(/\r\n?/g, "\n").split("\n"), root);
    return root;
  }

  // ------------------------------------------------------------- chat
  const speaker = (role) => (role === "user" ? "user" : role === "system-note" ? "system-note" : "assistant");
  const WHO = { user: "You", assistant: "Formless", "system-note": "System" };

  function toolCallChip(call) {
    const f = call.function || {};
    const name = f.name || "tool";
    return h("details", { class: "chip tool-call" },
      h("summary", null, h("span", { class: "tname" }, h("span", { "aria-hidden": "true" }, "⚙ "), name), h("span", { class: "targs" }, summarizeArgs(f.arguments || ""))),
      payload(f.arguments || ""));
  }
  function toolResultChip(content, meta) {
    // The daemon stores the error outcome separately from arbitrary output.
    // Older or incomplete transcripts may not carry a recorded failure.
    let failed = false;
    let resultMeta = {};
    try {
      const parsed = JSON.parse(meta || "{}");
      if (parsed && typeof parsed === "object") resultMeta = parsed;
      failed = resultMeta.is_error === true;
    } catch (_) {}
    const name = typeof resultMeta.name === "string" ? resultMeta.name : "";
    const duration = Number.isFinite(resultMeta.elapsed_ms) && resultMeta.elapsed_ms >= 0
      ? recordedDuration(resultMeta.elapsed_ms) : "";
    const status = failed ? "failed" : "done";
    const label = [name || "Tool", status, duration && "duration " + duration].filter(Boolean).join(" · ");
    const lines = logicalLineCount(content);
    const refs = artifactRefs(content);
    const chip = h("details", { class: "chip result" + (failed ? " failed" : "") },
      h("summary", null,
        h("span", { class: "tname" },
          h("span", { "aria-hidden": "true" }, "↳ "), label),
        content ? h("span", { class: "sr-only" }, ", output: ") : null,
        h("span", { class: "targs" }, truncate(content, 160)),
        lines > 1 ? h("span", { class: "sr-only" }, ", ") : null,
        lines > 1 ? h("span", { class: "tstat" }, lines + " lines") : null),
      payload(content));
    if (!refs.length) return chip;
    return h("div", null, chip, ...refs.map((id) =>
      h("button", { class: "btn ghost small", type: "button", onclick: () => openArtifact(id) }, "Open full output")));
  }
  // Artifact ids in a tool result (mirrors formless_protocol::artifact_refs).
  function artifactRefs(text) {
    const ids = new Set();
    for (const re of [/stored as artifact ([0-9a-f-]{36})/g, /"emitted_artifact":"([0-9a-f-]{36})"/g]) {
      for (const m of text.matchAll(re)) ids.add(m[1]);
    }
    return [...ids];
  }
  // Stored outputs open over the protocol (GetArtifact), never by path.
  async function openArtifact(id) {
    const trigger = document.activeElement;
    const first = await act({ type: "get_artifact", id, offset: 0 }, null, undefined, () => openArtifact(id));
    if (!first) return;
    let text = first.text;
    let end = first.end;
    let viewer;
    const pre = h("pre", { class: "diff artifact-text" }, text);
    const more = h("button", { class: "btn small", type: "button" }, "Load more");
    const status = h("span", { class: "muted" });
    const sync = () => {
      status.textContent = end + " of " + first.total_chars + " characters";
      more.hidden = end >= first.total_chars;
    };
    const loadMore = async () => {
      if (!viewer?.isConnected) return openArtifact(id);
      const r = await act({ type: "get_artifact", id, offset: end }, null, undefined, loadMore);
      if (!r || !viewer.isConnected) return;
      text += r.text;
      end = r.end;
      pre.textContent = text;
      sync();
    };
    more.onclick = loadMore;
    const download = h("button", { class: "btn small", type: "button" }, "Download");
    download.onclick = async () => {
      download.disabled = true;
      try {
        const chunks = [text];
        let offset = end;
        while (offset < first.total_chars) {
          const r = await act({ type: "get_artifact", id, offset, length: 1000000 }, null, undefined, () => download.onclick());
          if (!r) return;
          chunks.push(r.text);
          offset = r.end;
        }
        downloadBlob("artifact-" + id.slice(0, 8) + ".txt", new Blob(chunks, { type: "text/plain" }));
      } finally {
        download.disabled = false;
      }
    };
    const close = h("button", { class: "btn small primary", type: "button" }, "Close");
    viewer = h("div", { class: "backdrop", role: "dialog", "aria-modal": "true", "aria-label": "Full output" },
      h("div", { class: "modal viewer" },
        h("h2", null, "Output " + id.slice(0, 8) + " · " + first.bytes + " bytes"),
        pre,
        h("div", { class: "modal-actions" }, status, h("span", { class: "spacer" }), more, download, close)));
    const done = () => closeDialog(viewer);
    close.onclick = done;
    openDialog(viewer, { trigger, initialFocus: close, onDismiss: done });
    sync();
  }

  // ------------------------------------------------------- @ mentions
  /** Match ordinary and Markdown-wrapped `@path` mentions. */
  function mentionMatches(text) {
    return text.matchAll(/(^|[\s`(\[{])(@\S+)/g);
  }
  function mentionsIn(text) {
    const out = [];
    for (const m of mentionMatches(text)) {
      const p = m[2].slice(1).replace(/[,.;:!?)"'`\]}>]+$/, "");
      if (p && !out.includes(p)) out.push(p);
    }
    return out;
  }
  /** Text with `@path` mentions as highlighted spans. */
  function withMentions(text) {
    const parts = [];
    let last = 0;
    for (const m of mentionMatches(text)) {
      const at = m.index + m[1].length;
      parts.push(text.slice(last, at), h("span", { class: "mention" }, m[2]));
      last = at + m[2].length;
    }
    parts.push(text.slice(last));
    return parts;
  }
  const mention = { box: null, files: [], sel: 0, query: null, seq: 0 };
  function closeMentions() {
    if (mention.box) mention.box.remove();
    mention.box = null;
    mention.files = [];
    mention.query = null;
  }
  /** The `@word` ending at the caret, or null. */
  function mentionAtCaret() {
    const t = $("input");
    const before = t.value.slice(0, t.selectionStart);
    const m = before.match(/(^|[\s`(\[{])@(\S*)$/);
    return m && !t.value.startsWith("/") ? { query: m[2], start: before.length - m[2].length - 1 } : null;
  }
  async function updateMentions() {
    const at = mentionAtCaret();
    if (!at) return closeMentions();
    if (at.query === mention.query) return;
    mention.query = at.query;
    const seq = ++mention.seq;
    // Tools load with their view; fetch them once if it hasn't opened.
    if (!S.tools.length && !mention.toolsAsked) {
      mention.toolsAsked = true;
      S.tools = (await request({ type: "list_tools" }).catch(() => null)) || [];
    }
    let r;
    try { r = await request({ type: "list_files", query: at.query, limit: 8, session_id: S.current }); } catch (_) { return; }
    if (seq !== mention.seq || !mentionAtCaret()) return;
    // Tools first when their names match, then files.
    const tools = (S.tools || []).filter((t) => !t.retired)
      .map((t) => ({ t, m: fuzzyMatch(at.query, t.name) })).filter((x) => x.m)
      .sort((a, b) => b.m[0] - a.m[0]).slice(0, at.query ? 4 : 3)
      .map((x) => ({ tool: true, path: x.t.name, description: x.t.description }));
    mention.files = tools.concat(r.files || []);
    mention.sel = 0;
    drawMentions();
  }
  function drawMentions() {
    if (!mention.files.length) { if (mention.box) mention.box.remove(); mention.box = null; return; }
    if (!mention.box) {
      mention.box = h("ul", { class: "compose-popup", role: "listbox", "aria-label": "Files" });
      $("composer").append(mention.box);
    }
    mention.box.replaceChildren(...mention.files.map((f, i) => {
      const li = h("li", { role: "option", "aria-selected": String(i === mention.sel), class: i === mention.sel ? "sel" : null },
        f.tool ? [h("span", { class: "picker-tag" }, "Tool"), " " + f.path, h("span", { class: "muted" }, " " + (f.description || "").slice(0, 60))]
          : f.path + (f.dir ? "/" : ""));
      li.onmousedown = (e) => { e.preventDefault(); mention.sel = i; acceptMention(); };
      return li;
    }));
  }
  function acceptMention() {
    const f = mention.files[mention.sel];
    const at = mentionAtCaret();
    if (!f || !at) return;
    const t = $("input");
    const insert = "@" + f.path + (f.dir ? "/" : " ");
    t.value = t.value.slice(0, at.start) + insert + t.value.slice(t.selectionStart);
    const caret = at.start + insert.length;
    t.setSelectionRange(caret, caret);
    if (!S.editMessageId) saveDraft(S.current, t.value);
    mention.query = null;
    autosize();
    updateMentions();
    // A chosen file is attached now, shown as a chip (× takes it back).
    if (!f.tool && !f.dir) attachMention(f.path);
  }
  /** Paths already attached from the @ popup, by Session (not attached
   * again on send). */
  const mentionAttached = {};
  async function attachMention(path) {
    const sid = S.current;
    if (!sid) return;
    try {
      const a = await request({ type: "attach_path", session_id: sid, path });
      const key = sessionCacheKey(S.storageScope, sid);
      (mentionAttached[key] = mentionAttached[key] || new Set()).add(path);
      (S.chips[sid] = S.chips[sid] || []).push({ id: a.reference ? a.path : a.id, name: a.name, mime: a.mime || (a.reference ? "reference" : ""), bytes: a.bytes || 0, path });
      await refreshAttachmentInventory(sid);
      renderChips();
    } catch (e) {
      note("cannot attach @" + path + ": " + e.message, true);
    }
  }
  // ---------------------------------------------------------- / palette
  const palette = { box: null, items: [], sel: 0, hidden: null };
  /** Palette rows for the composer text: commands while the name is being
   * typed, nothing once its arguments start. */
  function paletteItems() {
    const v = $("input").value;
    if (!v.startsWith("/") || v === palette.hidden || S.editMessageId) return [];
    const q = v.slice(1);
    if (/\s/.test(q) && !COMMANDS.some((c) => c.name.startsWith(q.trimEnd()))) return [];
    return matchCommands(q.trimEnd());
  }
  function closePalette() {
    if (palette.box) palette.box.remove();
    palette.box = null;
    palette.items = [];
  }
  function updatePalette() {
    palette.items = paletteItems();
    palette.sel = Math.min(palette.sel, Math.max(0, palette.items.length - 1));
    if (!palette.items.length) return closePalette();
    if (!palette.box) {
      palette.box = h("ul", { class: "compose-popup", role: "listbox", "aria-label": "Commands" });
      $("composer").append(palette.box);
    }
    palette.box.replaceChildren(...palette.items.map((c, i) => {
      const li = h("li", { role: "option", "aria-selected": String(i === palette.sel), class: i === palette.sel ? "sel" : null },
        h("span", { class: "cmd-usage" }, usage(c)), h("span", { class: "muted" }, tr(c.desc)));
      li.onmousedown = (e) => { e.preventDefault(); palette.sel = i; acceptCommand(true); };
      return li;
    }));
    const sel = palette.box.children[palette.sel];
    if (sel) sel.scrollIntoView({ block: "nearest" });
  }
  /** Tab completes the selected command; Enter runs it unless it still
   * needs an argument. */
  function acceptCommand(run) {
    const c = palette.items[palette.sel];
    if (!c) return;
    const t = $("input");
    if (run && !needsArgs(c)) {
      t.value = "/" + c.name;
      saveDraft(S.current, t.value);
      closePalette();
      return submit(false);
    }
    t.value = "/" + c.name + (c.args ? " " : "");
    saveDraft(S.current, t.value);
    if (run) note("usage: " + usage(c));
    palette.sel = 0;
    autosize();
    updatePalette();
  }
  function paletteKey(e) {
    if (!palette.box || !palette.items.length) return false;
    if (e.key === "ArrowDown") palette.sel = Math.min(palette.sel + 1, palette.items.length - 1);
    else if (e.key === "ArrowUp") palette.sel = Math.max(palette.sel - 1, 0);
    else if (e.key === "Tab" && !e.shiftKey) { e.preventDefault(); acceptCommand(false); return true; }
    else if (e.key === "Enter" && !e.shiftKey && !e.ctrlKey && !e.metaKey) { e.preventDefault(); acceptCommand(true); return true; }
    else if (e.key === "Escape") { palette.hidden = $("input").value; closePalette(); }
    else return false;
    e.preventDefault();
    e.stopPropagation();
    if (palette.box) updatePalette();
    return true;
  }

  /** Keys for the open popup; true when used. */
  function mentionKey(e) {
    if (!mention.box || !mention.files.length) return false;
    if (e.key === "ArrowDown") mention.sel = Math.min(mention.sel + 1, mention.files.length - 1);
    else if (e.key === "ArrowUp") mention.sel = Math.max(mention.sel - 1, 0);
    else if ((e.key === "Tab" || e.key === "Enter") && !e.shiftKey && !e.ctrlKey && !e.metaKey) { e.preventDefault(); acceptMention(); return true; }
    else if (e.key === "Escape") { closeMentions(); }
    else return false;
    e.preventDefault();
    e.stopPropagation();
    if (mention.box) drawMentions();
    return true;
  }

  // ------------------------------------------------------- model picker
  /** Case-insensitive subsequence match; higher scores are better
   * (same rules as the TUI's picker). Returns [score, positions] or null. */
  function fuzzyMatch(query, cand) {
    const q = [...query.replace(/\s+/g, "").toLowerCase()];
    if (!q.length) return [0, []];
    const c = [...cand.toLowerCase()];
    // Try each start of the first char: a contiguous later match beats a
    // scattered earlier one.
    let best = null;
    for (let start = c.indexOf(q[0]); start >= 0; start = c.indexOf(q[0], start + 1)) {
      const pos = [];
      let score = 0, prev = -1;
      for (let i = start; i < c.length && pos.length < q.length; i++) {
        if (c[i] !== q[pos.length]) continue;
        score += 1;
        if (prev >= 0 && prev + 1 === i) score += 5;
        if (i === 0 || !/[\p{L}\p{N}]/u.test(c[i - 1])) score += 8;
        if (prev >= 0) score -= Math.min(i - prev - 1, 10);
        pos.push(i);
        prev = i;
      }
      if (pos.length < q.length) break;
      if (!best || score > best[0]) best = [score, pos];
    }
    return best && [best[0] - Math.floor(c.length / 8), best[1]];
  }

  async function setSessionModel(model, effort) {
    if (S.topicsAvailable && currentTopicAttempt()) return;
    if (!S.current) return toast("No current session.", true);
    const req = { type: "set_session_model", id: S.current, model };
    if (effort !== undefined) req.reasoning_effort = effort;
    const s = await act(req);
    if (!s) return;
    const i = S.sessions.findIndex((x) => x.id === s.id);
    if (i >= 0) S.sessions[i] = s;
    renderStatus();
    note("session model: " + (s.model || "default") + " (effort " + (s.reasoning_effort || "default") + ")");
  }

  /** `/model`: filter models as you type; Enter sets this Session's model,
   * Ctrl/⌘+Enter the global default. */
  // "200k ctx · $3/$15 per M · vision · tools · ✗ test failed" (SPEC §4.2).
  function modelContextLabel(value) {
    if (!Number.isSafeInteger(value) || value < 0) return null;
    const raw = String(value);
    if (value < 1000) return raw + " ctx";
    // Keep decimal windows such as 128000 in familiar decimal units.
    if (value % 1000 !== 0) {
      if (value % 1048576 === 0) return value / 1048576 + "Mi ctx";
      if (value % 1024 === 0) return value / 1024 + "Ki ctx";
    }
    // Split integer digits instead of rounding floating-point quotients.
    const places = value >= 1e6 ? 6 : 3;
    const whole = raw.slice(0, -places);
    const fraction = raw.slice(-places).replace(/0+$/, "");
    const compact = whole + (fraction ? "." + fraction : "") + (places === 6 ? "M" : "k");
    return (compact.length <= raw.length ? compact : raw) + " ctx";
  }
  function modelDetail(m, t) {
    const parts = [];
    m = m || {};
    const context = modelContextLabel(m.context);
    if (context !== null) parts.push(context);
    if (m.input_price != null && m.output_price != null) parts.push(uiMoney(m.input_price, null) + "/" + uiMoney(m.output_price, null) + " per M");
    if (m.vision) parts.push("vision");
    if (m.tools === true) parts.push("tools");
    if (m.tools === false) parts.push("no tools");
    if (t) parts.push(t.ok ? "✓ tested" : t.transient === true ? "✗ temporary test failure (use Test to retry)" : "✗ test failed");
    return parts.join(" · ");
  }
  function modelTestText(t) {
    const ms = (v) => (v == null ? "-" : v + " ms");
    return (t.ok ? "✓ " : "✗ ") + t.model + ": first token " + ms(t.first_token_ms) + ", total " + ms(t.total_ms)
      + " · streaming " + (t.streaming ? "yes" : "no") + " · reasoning " + (t.reasoning ? "yes" : "no")
      + " · tool calls " + (t.tool_calling ? "yes" : "no") + (t.prompt_fallback ? " (prompt fallback)" : "")
      + (t.error ? " · " + t.error : "");
  }
  function storedSessionModel(session) {
    return session?.model || S.settings?.provider?.model || "";
  }
  let modelPickerSequence = 0;
  async function openModelPicker(onPick) {
    if (S.topicsAvailable && currentTopicAttempt()) return;
    const selectionOnly = typeof onPick === "function";
    const session = S.sessions.find((x) => x.id === S.current);
    const model = storedSessionModel(session);
    const efforts = ["", "low", "medium", "high"];
    // The scrollable results are navigated from the input, including in
    // browsers that otherwise add scroll containers to the Tab order.
    const list = h("ul", { id: "model-picker-" + ++modelPickerSequence, class: "picker-list", role: "listbox", "aria-label": "Models", tabindex: -1, hidden: true });
    const input = h("input", { type: "text", placeholder: "Filter models, or type a model id", "aria-label": "Filter models", autocomplete: "off", spellcheck: "false",
      role: "combobox", "aria-autocomplete": "list", "aria-controls": list.id, "aria-expanded": "false" });
    const effort = h("select", { "aria-label": "Reasoning effort" },
      efforts.map((e) => h("option", { value: e }, e || "default (provider)")));
    effort.value = (session && session.reasoning_effort) || "";
    const status = h("span", { class: "muted" }, "loading models…");
    const cancel = h("button", { class: "btn small", type: "button" }, "Cancel");
    const asDefault = h("button", { class: "btn small", type: "button", title: "Ctrl/⌘+Enter" }, "Set as default");
    const apply = h("button", { class: "btn small primary", type: "button", title: "Enter" }, selectionOnly ? "Retry" : "Use in this Session");
    const test = h("button", { class: "btn small", type: "button", title: "Latency, streaming, reasoning and tool calls" }, "Test");
    let meta = {}, tests = {};
    const title = selectionOnly ? "Retry with a model" : "Model";
    const dialog = h("div", { class: "backdrop", role: "dialog", "aria-modal": "true", "aria-label": title },
      h("div", { class: "modal picker" },
        h("h2", null, title),
        h("p", { class: "muted picker-current-model" }, model ? "Current selection: " + model : "Current model unavailable"),
        input, list,
        selectionOnly ? null : h("label", { class: "picker-effort" }, "Reasoning effort ", effort),
        h("div", { class: "modal-actions" }, status, h("span", { class: "spacer" }),
          selectionOnly ? null : test, cancel, selectionOnly ? null : asDefault, apply)));
    let items = [], matches = [], sel = 0;
    const close = () => closeDialog(dialog);
    const choice = () => (matches[sel] && matches[sel].item.id) || input.value.trim();
    const multiSource = () => new Set(items.map((i) => i.source)).size > 1;
    function draw() {
      const q = input.value;
      matches = items.map((item, n) => ({ item, n, m: fuzzyMatch(q, item.id) }))
        .filter((x) => x.m)
        .sort((a, b) => (q.trim() ? b.m[0] - a.m[0] : 0) || a.n - b.n)
        .map((x) => ({ item: x.item, optionId: list.id + "-option-" + x.n, pos: new Set(x.m[1]) }));
      sel = Math.min(sel, Math.max(0, matches.length - 1));
      const sources = multiSource();
      // At most 200 rows, in a window that keeps the selection.
      const first = Math.max(0, Math.min(sel - 100, matches.length - 200));
      list.replaceChildren(...matches.slice(first, first + 200).map((x, n) => {
        const i = first + n;
        const id = [...x.item.id].map((ch, n) => x.pos.has(n) ? h("mark", null, ch) : ch);
        const li = h("li", { id: x.optionId, role: "option", "aria-selected": String(i === sel), class: i === sel ? "sel" : null,
          "aria-posinset": i + 1, "aria-setsize": matches.length },
          h("span", { class: "picker-id" }, id),
          x.item.recent ? h("span", { class: "picker-tag" }, "recent") : null,
          sources && x.item.source ? h("span", { class: "muted" }, x.item.source) : null,
          modelDetail(meta[x.item.id], tests[x.item.id]) ? h("span", { class: "muted" }, modelDetail(meta[x.item.id], tests[x.item.id])) : null);
        li.onclick = () => { sel = i; draw(); input.focus({ preventScroll: true }); };
        li.ondblclick = () => { sel = i; finish(false); };
        return li;
      }));
      const selNode = list.children[sel - first];
      list.hidden = !matches.length;
      input.setAttribute("aria-expanded", String(!list.hidden));
      if (selNode) {
        input.setAttribute("aria-activedescendant", selNode.id);
        selNode.scrollIntoView({ block: "nearest" });
      } else {
        input.removeAttribute("aria-activedescendant");
      }
      // Only the efforts the highlighted model accepts (SPEC §4.2).
      const accepted = (meta[choice()] || {}).efforts;
      const offer = Array.isArray(accepted) ? [""].concat(["minimal", "low", "medium", "high"].filter((e) => accepted.includes(e))) : efforts;
      const was = effort.value;
      effort.replaceChildren(...offer.map((e) => h("option", { value: e }, e || "default (provider)")));
      effort.value = offer.includes(was) ? was : "";
      effort.disabled = offer.length <= 1;
      effort.title = offer.length <= 1 ? "Effort has no effect for this model" : "";
      if (!items.length) return;
      status.textContent = matches.length
        ? matches.length + " of " + items.length
        : q.trim() ? "no match · Enter uses “" + q.trim() + "”" : "";
    }
    async function finish(global) {
      const model = choice();
      if (!model) return;
      const e = effort.value || null;
      close();
      if (selectionOnly) return onPick(model);
      if (!global) return setSessionModel(model, e);
      // Settings go through the daemon (principle 6).
      if (await act({ type: "set_setting", key: "provider.model", value: model })
        && await act({ type: "set_setting", key: "provider.reasoning_effort", value: e }))
        note("default model: " + model + " (effort " + (e || "default") + ")");
    }
    input.oninput = () => { sel = 0; draw(); };
    dialog.addEventListener("keydown", (e) => {
      // The model filter owns list navigation; native selects/buttons keep
      // their Arrow and Enter behavior instead of unexpectedly applying a model.
      if (e.target !== input) return;
      if (e.key === "ArrowDown") { e.preventDefault(); sel = Math.min(sel + 1, matches.length - 1); draw(); }
      else if (e.key === "ArrowUp") { e.preventDefault(); sel = Math.max(sel - 1, 0); draw(); }
      else if (e.key === "Enter" && e.target.tagName !== "BUTTON") { e.preventDefault(); finish(e.ctrlKey || e.metaKey); }
    });
    cancel.onclick = close;
    apply.onclick = () => finish(false);
    asDefault.onclick = () => finish(true);
    test.onclick = async () => {
      const model = choice();
      if (!model) return;
      const testing = "testing " + model + "…";
      status.textContent = testing;
      const t = await act({ type: "test_model", model }, null, 120000);
      // act already reported why; end the label it left, unless newer text
      // replaced it (#1730).
      if (!t) {
        if (status.textContent === testing) status.textContent = "✗ " + model + ": test did not complete";
        return;
      }
      tests[model] = t;
      status.textContent = modelTestText(t);
      draw();
    };
    openDialog(dialog, { initialFocus: input, onDismiss: close });
    const r = await act({ type: "list_models" }, null, 30000);
    if (!dialog.isConnected) return;
    if (!r) {
      // act already reported why; free-form entry still works (#1730).
      if (status.textContent === "loading models…") {
        status.textContent = "could not load models — type a model id";
        status.classList.add("err");
      }
      return;
    }
    const recent = r.recent || [];
    meta = r.meta || {};
    tests = r.tests || {};
    const push = (id, source) => {
      if (!items.some((i) => i.id === id)) items.push({ id, source, recent: recent.includes(id) });
    };
    for (const [alias, target] of Object.entries(r.aliases || {})) push(alias, "alias → " + target);
    for (const role of r.roles || [])
      push("role:" + role.name, "role · " + (role.provider || "default") + " · " + (role.model || "its default model"));
    for (const m of r.models || []) push(m, "default provider");
    for (const m of recent) push(m, "recent");
    items.sort((a, b) => b.recent - a.recent);
    const current = (session && session.model) || (r.current && r.current.model) || "";
    sel = Math.max(0, items.findIndex((i) => i.id === current));
    status.textContent = r.error ? "could not list models: " + r.error
      : items.length ? "" : "no models listed — type a model id";
    if (!items.length && r.error) status.classList.add("err");
    draw();
  }

  // Stable keyed wrappers retain selection, focus and disclosure state while
  // later messages arrive. Only an edited message's own body is rebuilt.
  const rendered = new Map();
  const historyRender = { session: undefined, turns: new Map(), live: null, welcome: null, banner: null };
  function reconcileChildren(parent, wanted) {
    const wantedSet = new Set(wanted);
    let child = parent.firstChild;
    while (child) {
      const next = child.nextSibling;
      if (!wantedSet.has(child)) child.remove();
      child = next;
    }
    let cursor = parent.firstChild;
    for (const node of wanted) {
      if (node === cursor) cursor = cursor.nextSibling;
      else parent.insertBefore(node, cursor);
    }
    while (cursor) { const next = cursor.nextSibling; cursor.remove(); cursor = next; }
  }
  function updateText(node, value) {
    const text = typeof value === "number" ? uiFormat(value) : String(value == null ? "" : value);
    if (node.data !== text) node.data = text;
  }
  function messageBody(m, build) {
    let node = rendered.get(m.id);
    if (!node) {
      node = h("div", { class: "msg", "data-message-id": m.id });
      rendered.set(m.id, node);
    }
    const signature = [m.role, m.generated_by, m.content, m.meta, m.seq, m.role === "user" && m.generated_by == null ? sentElsewhere(m) : null];
    const changed = !node.messageSignature || signature.some((part, i) => part !== node.messageSignature[i]);
    node.currentMessage = m;
    if (changed) {
      node.replaceChildren();
      build(node);
      if (m.role !== "system-note") {
        node.append(h("button", { class: "msg-menu", type: "button", title: "Message actions", "aria-label": "Message actions", "aria-haspopup": "menu",
          onkeydown: (e) => menuTriggerKey(e, (last) => messageMenu(e.currentTarget, node.currentMessage, node, last)),
          onclick: (e) => { e.stopPropagation(); messageMenu(e.currentTarget, node.currentMessage, node); } }, "⋯"));
      }
      node.messageSignature = signature;
    }
    refreshDiagrams(node);
    return node;
  }
  function copy(text, what) {
    navigator.clipboard.writeText(text).then(() => toast("Copied " + what), () => toast("Copy failed", true));
  }
  // Read rendered line breaks from an inert, offscreen copy so generated
  // controls never enter the clipboard and the live message stays untouched.
  function messageCopyText(node) {
    const source = node.querySelector(".bubble, .md") || node;
    const clone = source.cloneNode(true);
    for (const control of clone.querySelectorAll("button, [role=button]")) control.remove();
    clone.inert = true;
    clone.setAttribute("aria-hidden", "true");
    clone.style.cssText += ";position:fixed;left:-100000px;top:0;opacity:0;pointer-events:none;width:" + source.getBoundingClientRect().width + "px";
    // User/system-note ancestors supply pre-wrap; preserve the source's
    // computed whitespace when the clone leaves that ancestor context.
    clone.style.whiteSpace = getComputedStyle(source).whiteSpace;
    document.body.append(clone);
    try { return clone.innerText; }
    finally { clone.remove(); }
  }
  /** Actions on one message (SPEC §10.2): copy it (as text or Markdown),
   * edit and re-run a user message, fork the Session from it, and copy a
   * Tool call's input or a result. */
  function messageMenu(btn, m, node, focusLast = false) {
    closePopover();
    let calls = [];
    try { calls = JSON.parse(m.meta || "{}").tool_calls || []; } catch (_) {}
    const item = (label, run, cls) => h("button", { type: "button", class: cls || null, onclick: () => { closePopover(); run(); } }, label);
    const items = [];
    if (m.role === "tool") items.push(item("Copy output", () => copy(m.content || "", "the output")));
    else if (m.content) {
      const text = m.role === "user" && m.generated_by != null ? m.content : messageCopyText(node);
      items.push(item("Copy", () => copy(text, "the message")));
      items.push(item("Copy as Markdown", () => copy(m.content, "as Markdown")));
    }
    for (const c of calls) {
      const f = c.function || {};
      items.push(item("Copy " + (f.name || "tool") + " input", () => copy(f.arguments || "", "the input")));
    }
    if (m.role === "user" && m.generated_by == null && S.topicsAvailable && !currentTopicAttempt()) {
      items.push(item("Move to topic…", () => proposeTopic(m.id)));
    }
    if (m.role === "user" && m.generated_by == null && !isRunning() && !currentTopicAttempt()) {
      const last = S.messages[S.messages.length - 1]?.id === m.id;
      items.push(item(last ? "Edit & re-run" : "Edit & re-run (drops the later turns)", () => beginEdit(m)));
    }
    if (!currentTopicAttempt()) items.push(item("Fork from here", async () => {
      const s = await act({ type: "fork_session", id: S.current, name: null, up_to_seq: m.seq }, "Forked from this message");
      if (s && s.id) { S.sessions = [s, ...S.sessions.filter((x) => x.id !== s.id)]; await openSession(s.id); }
    }));
    btn.setAttribute("aria-expanded", "true");
    btn.classList.add("menu");
    popover = h("div", { class: "popover", role: "menu", "aria-label": "Message actions" }, items);
    placePopover(btn, true, focusLast);
  }

  function assistantStopNote(message) {
    let meta;
    try { meta = JSON.parse(message.meta || "{}"); } catch (_) { return null; }
    if (meta.placeholder) return null; // Already shown as a provider stop note.
    const reason = typeof meta.stop_reason === "string" && meta.stop_reason
      ? meta.stop_reason : meta.truncated === true ? "max_tokens" : null;
    if (!reason) return null;
    const notes = {
      max_tokens: "The answer was cut off at the model's output token limit.",
      model_context_window_exceeded: "The answer stopped because the model's context window is full.",
      pause_turn: "The model paused its turn; ask it to continue.",
      refusal: "The model refused this request.",
      content_filter: "The provider's content filter stopped this answer.",
    };
    return notes[reason] || "The answer ended early (" + reason + ").";
  }
  function sessionLoadNotice() {
    const load = S.sessionLoad;
    if (!S.current || load?.id !== S.current || load.status === "loaded") {
      historyRender.loadNotice = null;
      return null;
    }
    const key = load.id + ":" + load.status;
    // Reuse the notice across ordinary paints, avoiding repeated status
    // announcements and preserving the Retry control while it owns focus.
    if (historyRender.loadNotice?.key !== key) {
      const pending = load.status === "pending";
      const node = h("div", { class: "banner", "data-session-load": load.status,
        role: pending ? "status" : "group", "aria-label": pending ? null : "Transcript load failed" },
        pending ? "Loading transcript…" : [
          h("p", null, "Could not load the transcript."),
          h("button", { class: "btn small", type: "button", onclick: () => openSession(load.id, true) }, "Retry loading transcript")]);
      historyRender.loadNotice = { key, node };
    }
    return historyRender.loadNotice.node;
  }
  function renderHistory({ newContent = false } = {}) {
    const box = $("transcript");
    withTranscriptLayout(() => {
      if (historyRender.session !== S.current) {
        historyRender.session = S.current;
        historyRender.turns.clear();
        historyRender.live = null;
        historyRender.welcome = null;
        historyRender.banner = null;
        rendered.clear();
      }
      const wanted = [];
      const loadNotice = sessionLoadNotice();
      if (loadNotice) wanted.push(loadNotice);
      if (!S.current || (!S.messages.length && !isRunning() && !loadNotice)) {
        historyRender.turns.clear();
        rendered.clear();
        if (!historyRender.welcome) historyRender.welcome = welcome();
        if (S.current && !S.daemonOnline) {
          if (!historyRender.banner) historyRender.banner = h("div", { class: "banner" }, "The daemon is offline. Start formless on your machine; this page reconnects automatically.");
          wanted.push(historyRender.banner);
        }
        wanted.push(historyRender.welcome);
        if (S.current) {
          if (!historyRender.live) historyRender.live = h("div", { id: "live", class: "turn", "data-live-root": S.current });
          wanted.push(historyRender.live);
        }
        reconcileChildren(box, wanted);
        return;
      }
      if (!S.daemonOnline) {
        if (!historyRender.banner) historyRender.banner = h("div", { class: "banner" }, "The daemon is offline. Start formless on your machine; this page reconnects automatically.");
        wanted.push(historyRender.banner);
      }
      const lastUser = [...S.messages].reverse().find((m) => m.role === "user" && m.generated_by == null);
      const lastAnswer = [...S.messages].reverse().find((m) => m.role === "assistant" && m.content);
      const active = new Set(), messageIds = new Set();
      // Recompute the display context on normal renders, including local
      // midnight and timezone changes, without touching stable turn nodes.
      const today = new Date().toDateString();
      const timeContext = [uiLocale(), today, Intl.DateTimeFormat().resolvedOptions().timeZone].join("|");
      let turn = null, group = null;
      const open = (role, when, msg) => {
        const g = speaker(role);
        if (g === group && turn) return turn.items;
        group = g;
        // A turn's first message is its stable key. Users always start a turn.
        const key = g + ":" + currentMessage.id;
        turn = historyRender.turns.get(key);
        if (!turn) {
          const who = h("div", { class: "who " + g }, WHO[g]);
          const time = h("span");
          who.append(time);
          const body = h("div");
          turn = { el: h("div", { class: "turn " + g, "data-turn-id": currentMessage.id }, who, body), who, time, body, edit: null, decorations: null, decorationKey: null };
          historyRender.turns.set(key, turn);
        }
        turn.items = [];
        turn.message = currentMessage;
        if (turn.when !== when || turn.timeContext !== timeContext) {
          const stamp = turnTimestamp(when, today);
          turn.time.replaceChildren(...(stamp ? [stamp] : []));
          turn.time.hidden = !stamp;
          turn.when = when;
          turn.timeContext = timeContext;
        }
        const editable = msg && msg === lastUser && !isRunning() && !(S.topicsAvailable && currentTopicAttempt());
        if (editable && !turn.edit) {
          const entry = turn;
          turn.edit = h("button", { class: "btn ghost small edit", type: "button", title: "Edit and re-run (/edit)", onclick: () => beginEdit(entry.message) }, "Edit");
          turn.who.append(turn.edit);
        } else if (!editable && turn.edit) { turn.edit.remove(); turn.edit = null; }
        active.add(key);
        wanted.push(turn.el);
        return turn.items;
      };
      let currentMessage;
      for (const m of S.messages) {
        currentMessage = m;
        messageIds.add(m.id);
        if (m.role === "user" && m.generated_by == null) {
          // Each user message starts a fresh turn so its Edit button is exact.
          group = null;
          const from = sentElsewhere(m);
          open("user", m.created_at, m).push(messageBody(m, (n) => {
            const steer = steerStep(m);
            n.append(h("div", { class: "bubble" }, withMentions(m.content)),
              ...attachmentTags(m),
              ...(from ? [h("div", { class: "muted sent-from" }, "sent from " + from)] : []),
              ...(steer === null ? [] : [h("div", {
                class: "steer-mark",
                title: steer === "unrecorded" ? "Provider-context inclusion was not recorded" : "First provider request context that included this steer",
              }, steer === "unrecorded" ? "↳ steer sent during a Run" : "↳ steer joined at step " + steer)]));
          }));
        } else if (m.role === "user" && m.generated_by != null) {
          const kickoff = m.generated_by === "topic_kickoff";
          open("assistant", m.created_at).push(messageBody(m, (n) => n.append(
            h("p", null, h("strong", null, kickoff ? "Workflow requested." : "Background instructions added."),
              kickoff ? " Your requests and outline were sent to Formless to coordinate this topic." : " Formless added instructions for this conversation."),
            h("details", { class: "generated-details" }, h("summary", null, kickoff ? "Orchestration details" : "Message details"), h("pre", null, m.content))
          )));
        } else if (m.role === "tool") {
          open("assistant", m.created_at).push(messageBody(m, (n) => n.append(toolResultChip(m.content || "", m.meta))));
        } else if (m.role === "system-note") {
          open("system-note", m.created_at).push(messageBody(m, (n) => n.append(h("div", { class: "bubble" }, m.content))));
        } else {
          open("assistant", m.created_at).push(messageBody(m, (n) => {
            let served = null;
            try { served = JSON.parse(m.meta || "{}").model; } catch (_) {}
            if (served && served.name) n.append(h("div", { class: "muted sent-from", title: "The Session asked for " + (served.requested || "another model") }, "via " + served.name + " (" + (served.via || "fallback") + ")"));
            let placeholder = false;
            try { placeholder = JSON.parse(m.meta || "{}").placeholder === true; } catch (_) {}
            // An empty refusal/filter stop: the daemon's note, not the model's words.
            if (placeholder) n.append(h("div", { class: "muted stop-note" }, m.content));
            else if (m.content) n.append(markdown(m.content));
            const stopNote = assistantStopNote(m);
            if (stopNote) n.append(h("div", { class: "muted stop-note" }, stopNote));
            let calls = [];
            try { calls = (JSON.parse(m.meta || "{}").tool_calls) || []; } catch (_) {}
            calls.forEach((c) => n.append(toolCallChip(c)));
          }));
        }
      }
      for (const [key, entry] of historyRender.turns) {
        if (!active.has(key)) { entry.el.remove(); historyRender.turns.delete(key); continue; }
        const answerHere = lastAnswer && entry.items.includes(rendered.get(lastAnswer.id)) && !isRunning()
          && !(S.topicsAvailable && currentTopicAttempt());
        if (answerHere) {
          const decorationKey = JSON.stringify([S.recap, S.rated, lastAnswer.id]);
          if (!entry.decorations) entry.decorations = h("div", { class: "turn-actions" });
          if (entry.decorationKey !== decorationKey) {
            entry.decorations.replaceChildren(...(S.recap ? [recapBlock(S.recap)] : []), rateButtons());
            entry.decorationKey = decorationKey;
          }
          entry.items.push(entry.decorations);
        }
        reconcileChildren(entry.body, entry.items);
      }
      for (const id of rendered.keys()) if (!messageIds.has(id)) rendered.delete(id);
      if (!historyRender.live) historyRender.live = h("div", { id: "live", class: "turn", "data-live-root": S.current });
      wanted.push(historyRender.live);
      reconcileChildren(box, wanted);
    }, { newContent });
    renderLive();
  }

  // 👍/👎 on the last answer (SPEC §8.7), with an optional comment for 👎.
  function currentRunTarget() {
    return { session: S.current, sequence: sessionSeq, run: S.liveRun?.id || null };
  }
  function ensureRunTarget(target) {
    if (S.topicsAvailable && currentTopicAttempt()) return false;
    if (target.session && target.session === S.current
        && target.sequence === sessionSeq && target.run === (S.liveRun?.id || null)) return true;
    note("The Session or latest Run changed. Open the action again.", true);
    return false;
  }
  function rateButtons() {
    const rate = async (rating) => {
      const target = currentRunTarget();
      let comment = null;
      if (rating === "bad") {
        const input = h("textarea", { rows: 4, "aria-label": "Optional rating comment" });
        const save = await ask({ title: "What was wrong?", body: h("div", null, input), ok: "Submit rating" });
        if (save) comment = input.value || null;
      }
      if (!ensureRunTarget(target)) return;
      const r = await act({ type: "rate_run", session_id: target.session, run_id: target.run, rating, comment });
      if (r && target.session === S.current && target.run === (S.liveRun?.id || null)) {
        S.rated = { session: target.session, rating: r.rating };
        renderHistory();
        note(r.rating ? "rated " + (r.rating === "good" ? "👍" : "👎") : "rating cleared");
      }
    };
    const cur = S.rated && S.rated.session === S.current ? S.rated.rating : "";
    const btn = (rating, label, title) => h("button", { class: "btn ghost small" + (cur === rating ? " active" : ""), type: "button", title, "aria-pressed": String(cur === rating),
      onclick: () => rate(cur === rating ? "" : rating) }, label);
    return h("div", { class: "rate" }, btn("good", "👍", "Good answer"), btn("bad", "👎", "Bad answer"),
      h("button", { class: "btn ghost small", type: "button", title: "Re-run your last message (/retry)", onclick: () => retryRun("") }, tr("Retry")),
      h("button", { class: "btn ghost small", type: "button", title: "Re-run your last message on another model; this answer stays in a fork", onclick: () => {
        const target = currentRunTarget();
        openModelPicker(model => retryRun(model, target));
      } }, tr("Retry with…")));
  }
  // Re-run the last user message, on `model` for that Run when given.
  async function retryRun(model, target = currentRunTarget()) {
    if (!ensureRunTarget(target)) return;
    const sid = target.session;
    if (S.settings && S.settings.limits && S.settings.limits.forecast_confirm_usd != null) {
      try { if (!(await confirmForecast(sid, "", {retry:true,model:model || null,edit_message_id:null}))) return; }
      catch (e) { note("forecast: " + e.message, true); return; }
    }
    if (!ensureRunTarget(target)) return;
    const r = await act({ type: "retry_run", session_id: sid, expected_run_id: target.run, model: model || null });
    if (!r) return;
    note("retrying" + (r.model ? " on " + r.model : "") + (r.original_session ? "; the previous answer is kept in \"… · original answer\"" : ""));
    if (r.original_session) refreshSessions();
  }

  async function resumeRun() {
    if (S.topicsAvailable && currentTopicAttempt()) return;
    const sid = S.current;
    if (!sid) return;
    if (S.settings && S.settings.limits && S.settings.limits.forecast_confirm_usd != null) {
      try { if (!(await confirmForecast(sid, "", {resume:true,edit_message_id:null}))) return; }
      catch (e) { note("forecast: " + e.message, true); return; }
    }
    if (await act({type:"resume_run",session_id:sid})) { S.unfinished = null; renderRunBanner(); }
  }

  function welcomeEnterHint(enterSends) {
    return enterModeHint(enterSends) + (enterSends ? "" : " · Use the " + tr("Send") + " button");
  }
  function welcome() {
    // The same examples as the TUI's empty Session (1–4 there).
    const examples = [
      "Find every TODO in this repo and group them by file",
      "Summarise https://example.com in three bullet points",
      "Every weekday at 9:00, list the top 5 stories on news.ycombinator.com",
      "What can you do, and which Tools have you built so far?",
    ];
    return h("div", { class: "welcome" },
      h("h2", null, h("span", { class: "mark" }, "◇ "), "Formless"),
      h("p", { class: "muted" }, "It builds the Tools a task needs and keeps them, so it gets more capable the more you use it."),
      h("div", { class: "examples" }, examples.map((ex) => h("button", { type: "button", onclick: () => { $("input").value = ex; saveDraft(S.current, ex); $("input").focus(); autosize(); } }, ex))),
      h("p", { class: "muted" }, h("span", { class: "welcome-enter-hint" }, welcomeEnterHint(composerEnterSends())),
        " · /help commands · /model switch model · @ reference a file"));
  }

  function runningTool(name, invocationId = null, depth = null, handle = null) {
    const calls = S.liveTools.filter(x => x.status === "running" && !x.subagent);
    const knownHandle = handle != null && calls.some(x => x.handle === handle);
    const candidates = calls.filter(x => invocationId != null ? x.invocationId === invocationId :
      (name == null || x.name === name) && (depth == null || (x.depth || 0) === depth) &&
      (handle != null ? knownHandle ? x.handle === handle : name != null && !x.handle : name != null));
    return candidates.length === 1 ? candidates[0] : null;
  }
  function reconcileInvocationProgress(list) {
    for (const i of list) {
      if (i.session_id !== S.current) continue;
      if (i.invocation_id != null && S.liveTools.some(t => t.invocationId === i.invocation_id && t.status !== "running")) continue;
      let t = runningTool(i.tool_name, i.invocation_id, null, i.handle);
      if (!t) {
        if (S.liveTools.filter(t => t.metadataPending).length >= 128) continue;
        t = { metadataPending: true, invocationId: i.invocation_id ?? null, name: i.tool_name, status: "running", depth: 0, started: Date.now() - i.elapsed_ms };
        S.liveTools.push(t);
      }
      t.handle = i.handle;
      t.stalledSecs = i.stalled ? Math.floor(i.silent_ms / 1000) : null;
      if (i.last_fraction != null || i.last_label != null) t.progress = { fraction: i.last_fraction, label: i.last_label || "" };
    }
  }
  function progressDiagnostic(ev) {
    if (ev.last_fraction == null && ev.last_label == null) return "";
    return " · last progress " + (ev.last_fraction == null ? "unknown" : uiFormat(ev.last_fraction, { style: "percent", maximumFractionDigits: 0 })) + (ev.last_label ? " (" + ev.last_label + ")" : "");
  }
  async function retryProviderRequest() {
    const sid = S.current;
    if (!sid) return;
    const label = text => note((S.current === sid ? "" : "Session " + sid.slice(0, 8) + ": ") + text);
    try {
      const result = await request({ type: "retry_provider_request", session_id: sid });
      if (result && result.retrying === true) label("Retry requested for the in-flight provider request.");
      else if (result && result.retrying === false) label("Nothing was retried; no provider request is available for retry.");
      else label("The daemon did not confirm whether a provider retry was requested.");
    } catch (_) {
      label("Provider retry was not confirmed; the request may already have been re-sent.");
    }
  }
  function cancelTool(handle) {
    if (handle) act({ type: "cancel_invocation", handle }, "Cancelling the tool");
    else note("no pending or stalled tool to cancel (Esc stops the whole run)", true);
  }

  /** The live calls to draw (the TUI's rule), each with how many
   * descendants it folds away: a node shows while it or an ancestor runs,
   * and under a node that finished successfully its descendants fold. */
  function liveTree(calls, retained = new Set()) {
    const retainedAncestors = new Set();
    const ancestors = [];
    for (const call of calls) {
      const depth = call.depth || 0;
      while (ancestors.length && ancestors[ancestors.length - 1][0] >= depth) ancestors.pop();
      if (retained.has(call)) for (const [, ancestor] of ancestors) retainedAncestors.add(ancestor);
      ancestors.push([depth, call]);
    }
    const out = [];
    const stack = []; // [depth, running, open] of the current ancestors
    for (const c of calls) {
      const d = c.depth || 0;
      while (stack.length && stack[stack.length - 1][0] >= d) stack.pop();
      const parentOpen = !stack.length || stack[stack.length - 1][2];
      const ancestorRunning = stack.some((s) => s[1]);
      const running = c.status === "running";
      const visible = parentOpen && (running || ancestorRunning || retained.has(c));
      const open = visible && (c.status !== "ok" || retainedAncestors.has(c));
      if (visible) out.push([c, 0]);
      else if (ancestorRunning && !parentOpen) {
        for (let i = out.length - 1; i >= 0; i--) {
          if ((out[i][0].depth || 0) < d && out[i][0].status === "ok") { out[i][1]++; break; }
        }
      }
      stack.push([d, running, open]);
    }
    return out;
  }
  function focusedLiveTools(tools) {
    if (typeof document === "undefined" || typeof liveRender === "undefined" || !liveRender.rows) return new Set();
    const active = document.activeElement;
    const focused = new Set(tools.filter((tool) => {
      if (tool.status === "running") return false;
      const row = liveRender.rows.get(tool) || [...liveRender.rows.values()].find((candidate) =>
        (tool.invocationId != null && candidate.tool.invocationId === tool.invocationId)
        || (tool.handle && candidate.tool.handle === tool.handle)
        || (tool.subagent && candidate.tool.subagent === tool.subagent));
      return row?.node.contains?.(active) === true;
    }));
    const retained = new Set(focused);
    const ancestors = [];
    for (const tool of tools) {
      const depth = tool.depth || 0;
      while (ancestors.length && ancestors[ancestors.length - 1][0] >= depth) ancestors.pop();
      if (focused.has(tool)) for (const [, ancestor] of ancestors) retained.add(ancestor);
      ancestors.push([depth, tool]);
    }
    return retained;
  }
  /** A sub-agent's transcript (GetSubagent) in a dialog. */
  async function showSubagent(id) {
    const r = await act({ type: "get_subagent", id });
    if (!r) return;
    const a = r.subagent || {};
    const body = h("div", { class: "subagent-log" }, (r.messages || []).map((m) => {
      const text = m.tool_calls ? "calls " + m.tool_calls.map((c) => c.function.name + " " + summarizeArgs(c.function.arguments || "")).join(", ") : String(m.content || "");
      return h("div", { class: "subagent-msg " + m.role }, h("span", { class: "muted" }, m.role), " ", text);
    }), a.result ? h("p", null, h("strong", null, "Result: "), a.result) : null);
    await ask({ title: "Sub-agent · " + truncate(a.task || "", 60), text: (a.status || "") + " · " + (a.steps || 0) + " step(s)" + (a.model ? " · " + a.model : ""), body, ok: "Close" });
  }
  /** A streaming answer split into its finished Markdown blocks and the
   * unfinished tail (the TUI's complete-prefix rule): blocks end at a blank
   * line outside a code fence or display math. An unfinished block stays
   * plain text, and completed blocks above it keep their DOM identity. */
  function splitStreaming(text, state = {}, trustedAppend = false) {
    if (state.text === text) return state.value;
    const append = typeof state.text === "string" && text.length >= state.text.length
      && (trustedAppend || text.startsWith(state.text));
    let { fence = null, math = false, cut = 0, at = 0, search = 0 } = append ? state : {};
    // Checkpoint before the unfinished line. Its delimiter meaning is only
    // committed when its newline arrives; extending it must not keep a
    // provisional fence/math decision. Search only the newly appended suffix.
    for (let end; (end = text.indexOf("\n", search)) >= 0;) {
      const line = text.slice(at, end), next = end + 1;
      if (math) math = mathBlockOpen(line, true);
      else if (fence) {
        if (fenceCloses(line, fence)) fence = null;
      } else {
        const marker = line.match(FENCE);
        if (marker) fence = marker[2];
        else if (MATH_BLOCK.test(line)) math = mathBlockOpen(line);
        else if (!line.trim()) cut = next;
      }
      at = search = next;
    }
    const done = append && cut === state.cut ? state.value[0] : text.slice(0, cut);
    Object.assign(state, { text, fence, math, cut, at, search: text.length, value: [done, text.slice(cut)] });
    return state.value;
  }
  function thinkingTail(text, state = {}, trustedAppend = false) {
    if (state.text === text) return state.value;
    const append = typeof state.text === "string" && text.length >= state.text.length
      && (trustedAppend || text.startsWith(state.text));
    if (!append) { state.lines = [""]; state.more = false; }
    let at = append ? state.text.length : 0;
    for (let end; (end = text.indexOf("\n", at)) >= 0;) {
      state.lines[state.lines.length - 1] += text.slice(at, end);
      state.lines.push("");
      if (state.lines.length > 6) { state.lines.shift(); state.more = true; }
      at = end + 1;
    }
    state.lines[state.lines.length - 1] += text.slice(at);
    state.text = text;
    state.value = (state.more ? "… " : "") + state.lines.join("\n");
    return state.value;
  }
  // Per-stream caches, not Session-wide caches: a Session can emit many
  // answers in one Run. Every clear/commit/retry invalidates them explicitly.
  const liveText = { session: undefined, run: undefined, answer: {}, thinking: {}, answerSource: undefined, thinkingSource: undefined, expanded: false };
  function currentLiveText() {
    if (liveText.session !== S.current || liveText.run !== S.liveRun?.id) {
      Object.assign(liveText, { session: S.current, run: S.liveRun?.id, answer: {}, thinking: {}, answerSource: undefined, thinkingSource: undefined, expanded: false });
    }
    return liveText;
  }
  function appendLiveText(kind, delta) {
    const state = currentLiveText(), key = kind === "streaming" ? "answer" : "thinking";
    const source = key + "Source";
    // Only our append reducer supplies this proof. A snapshot/direct source
    // replacement invalidates it, without comparing growing prefixes per RAF.
    if (state[source] !== S[kind]) state[key] = {};
    S[kind] += delta;
    state[source] = S[kind];
  }
  function cachedLiveText(kind, text) {
    const state = currentLiveText(), source = kind + "Source";
    const value = (kind === "answer" ? splitStreaming : thinkingTail)(text, state[kind], state[source] === text);
    state[source] = text;
    return value;
  }
  function clearLiveText() {
    S.streaming = "";
    S.thinking = "";
    Object.assign(liveText, { session: undefined, run: undefined, answer: {}, thinking: {}, answerSource: undefined, thinkingSource: undefined, expanded: false });
  }
  function reuseStreamingBlock(previous, node) {
    if (previous?.isEqualNode(node)) return previous;
    // A later loose sibling changes a list's structure, not earlier content.
    // Retain list/item containers and identical descendants when extending it.
    if (previous && /^(UL|OL|LI)$/.test(node.nodeName) && previous.nodeName === node.nodeName
      && previous.getAttribute("start") === node.getAttribute("start")) {
      const items = [...previous.childNodes];
      const next = [...node.childNodes].map((item, i) => reuseStreamingBlock(items[i], item));
      const classes = node.getAttribute("class");
      if (classes == null) previous.removeAttribute("class");
      else previous.setAttribute("class", classes);
      reconcileChildren(previous, next);
      return previous;
    }
    return node;
  }
  // Completed Markdown chunks and the unfinished tail keep their DOM identity.
  const liveDone = { session: undefined, text: "", src: "", node: null, done: null, tail: null, tailText: null, plainText: null, afterSeq: -1, messages: null, messageCount: -1 };
  const liveRender = { session: undefined, root: null, who: null, thinking: null, thinkingText: null, rows: new Map(), activity: null, activityText: null, focusoutRoot: null };
  const statusRender = { root: null, spinner: null, text: null };
  function streamingNode(text) {
    if (!liveDone.node || liveDone.session !== S.current) {
      liveDone.session = S.current;
      liveDone.text = liveDone.src = "";
      liveDone.done = h("div", { class: "md bubble" });
      liveDone.tailText = document.createTextNode("");
      liveDone.tail = h("div", { class: "stream-tail" }, liveDone.tailText);
      liveDone.node = h("div", { class: "streaming" }, liveDone.done, liveDone.tail);
      liveDone.plainText = null;
    }
    const [done, tail] = cachedLiveText("answer", text);
    if (liveDone.src !== done) {
      const extra = done.startsWith(liveDone.src) ? done.slice(liveDone.src.length) : null;
      const last = liveDone.done.lastElementChild;
      const sibling = extra && extra.split("\n").find(line => line.trim())?.match(ITEM);
      const continuation = extra && /^(UL|OL)$/.test(last?.tagName || "")
        && (/^\s+\S/.test(extra) || sibling && (/\d/.test(sibling[2]) ? "OL" : "UL") === last.tagName);
      if (done.length > MD_FORMAT_CHARS) {
        if (!liveDone.plainText) {
          liveDone.plainText = document.createTextNode(done);
          liveDone.done.replaceChildren(h("pre", { class: "md-plain" }, liveDone.plainText));
        } else updateText(liveDone.plainText, done);
      } else if (extra !== null && !continuation && !liveDone.plainText) {
        if (extra) liveDone.done.append(...markdown(extra).childNodes);
      } else {
        // A restarted stream or a list continuation can change the last
        // Markdown block's meaning. Retain identical earlier blocks in place.
        const next = markdown(done);
        const previous = [...liveDone.done.childNodes];
        const nodes = [...next.childNodes].map((node, index) => reuseStreamingBlock(previous[index], node));
        reconcileChildren(liveDone.done, nodes);
        liveDone.plainText = null;
      }
      liveDone.src = done;
    }
    liveDone.text = text;
    if (liveDone.messages !== S.messages || liveDone.messageCount !== S.messages.length) {
      liveDone.afterSeq = S.messages.reduce((seq, item) => Math.max(seq, item.seq ?? -1), -1);
      liveDone.messages = S.messages;
      liveDone.messageCount = S.messages.length;
    }
    liveDone.done.hidden = !done;
    liveDone.tail.hidden = !tail;
    updateText(liveDone.tailText, tail);
    return liveDone.node;
  }
  function liveToolRow(tool) {
    let row = liveRender.rows.get(tool);
    if (row) return row;
    // Reconciled snapshots may replace the record while retaining the same
    // invocation identity. Keep its disclosure/focused controls in that case.
    row = [...liveRender.rows.values()].find((candidate) =>
      tool.invocationId != null && candidate.tool.invocationId === tool.invocationId
      || tool.handle && candidate.tool.handle === tool.handle
      || tool.subagent && candidate.tool.subagent === tool.subagent);
    if (row) {
      liveRender.rows.delete(row.tool);
      row.tool = tool;
      liveRender.rows.set(tool, row);
      return row;
    }
    const mark = h("span", { "aria-hidden": "true" });
    const name = document.createTextNode("");
    const args = document.createTextNode("");
    const statusText = document.createTextNode("");
    const status = h("span", { class: "sr-only", "data-live-status": "" }, statusText);
    const detailLabelText = document.createTextNode(", arguments: ");
    const detailLabel = h("span", { class: "sr-only" }, detailLabelText);
    detailLabel.hidden = true;
    const folded = h("span", { class: "tag" });
    const progress = h("progress", { max: "1", "aria-label": "Progress" });
    const progressText = document.createTextNode("");
    const progressBox = h("span", { class: "tprogress" }, progress, progressText);
    const stalled = h("span", { class: "tag broad" });
    const cancel = h("button", { type: "button", class: "btn danger small", onclick: (e) => { e.preventDefault(); cancelTool(row.tool.handle); } }, "Cancel");
    const transcript = h("button", { type: "button", class: "btn ghost small", onclick: (e) => { e.preventDefault(); showSubagent(row.tool.subagent); } }, "Transcript");
    const timer = document.createTextNode("");
    const summary = h("summary", null, h("span", { class: "muted", role: "img", "data-live-depth": "" }, "└"), mark, status,
      h("span", { class: "sr-only" }, ", tool: "), h("span", { class: "tname" }, name), detailLabel, h("span", { class: "targs" }, args), folded, progressBox, stalled, cancel, transcript,
      h("span", { class: "tstat", "data-live-timer": "", "aria-hidden": "true" }, timer));
    row = { tool, node: h("details", { class: "chip live tree-node" }, summary), summary, mark, name, args, status: statusText, detailLabel, detailLabelText, folded, progress, progressText, progressBox, stalled, cancel, transcript, timer, payload: null, payloadInitialized: false, payloadArgs: undefined, payloadSubagent: undefined };
    liveRender.rows.set(tool, row);
    return row;
  }
  function updateLiveTool(row, folded) {
    const t = row.tool, running = t.status === "running";
    const rowClass = "chip live tree-node" + (running ? "" : " done");
    if (row.node.className !== rowClass) row.node.className = rowClass;
    // Keep room for the call at large reading sizes and narrow viewports.
    // The marker exposes the real depth even when visual indentation is capped.
    const depth = Math.max(0, t.depth || 0);
    const indent = `min(${Math.min(depth, 4) * 1.125}rem, 15%)`;
    if (row.node.style.marginInlineStart !== indent) row.node.style.marginInlineStart = indent;
    const depthLabel = `Nesting depth ${depth}`;
    if (row.summary.firstChild.getAttribute("aria-label") !== depthLabel) {
      row.summary.firstChild.setAttribute("aria-label", depthLabel);
      row.summary.firstChild.title = depthLabel;
    }
    const identity = t.invocationId || t.handle || t.subagent || t.name || "tool";
    if (row.node.dataset.liveTool !== identity) row.node.dataset.liveTool = identity;
    if (row.summary.firstChild.hidden !== !t.depth) row.summary.firstChild.hidden = !t.depth;
    const markClass = running ? "spinner" : t.status === "ok" ? "tree-ok" : "tree-fail";
    if (row.mark.className !== markClass) {
      row.mark.className = markClass;
      row.mark.style.color = running ? "var(--warn)" : "";
      row.mark.textContent = running ? "" : t.status === "ok" ? "✓" : "✗";
    }
    updateText(row.name, t.name + (t.version > 0 ? " v" + t.version : ""));
    const status = running ? "Running" : t.status === "ok" ? "Done" : "Failed";
    const duration = t.elapsedMs != null ? recordedDuration(t.elapsedMs) : elapsed(t.started);
    updateText(row.status, status + " · " + duration);
    const args = t.subagent ? t.step || "" : summarizeArgs(t.arguments || "");
    updateText(row.args, args);
    updateText(row.detailLabelText, t.subagent ? ", progress: " : ", arguments: ");
    if (row.detailLabel.hidden !== !args) row.detailLabel.hidden = !args;
    if (row.folded.hidden !== !folded) row.folded.hidden = !folded;
    if (row.folded.textContent !== "+" + folded + " nested") row.folded.textContent = "+" + folded + " nested";
    const hideProgress = !running || !t.progress;
    if (row.progressBox.hidden !== hideProgress) row.progressBox.hidden = hideProgress;
    if (row.progress.hidden !== (t.progress?.fraction == null)) row.progress.hidden = t.progress?.fraction == null;
    if (t.progress?.fraction != null) row.progress.value = t.progress.fraction;
    updateText(row.progressText, t.progress ? (t.progress.fraction != null ? " " + uiFormat(t.progress.fraction, { style: "percent", maximumFractionDigits: 0 }) : "") + (t.progress.label ? " " + t.progress.label : "") : "");
    if (row.stalled.hidden !== (t.stalledSecs == null)) row.stalled.hidden = t.stalledSecs == null;
    if (row.stalled.textContent !== "stalled " + t.stalledSecs + "s") row.stalled.textContent = "stalled " + t.stalledSecs + "s";
    const hideCancel = !t.handle || !running;
    if (row.cancel.hidden !== hideCancel) row.cancel.hidden = hideCancel;
    if (row.transcript.hidden !== !t.subagent) row.transcript.hidden = !t.subagent;
    updateText(row.timer, t.elapsedMs != null ? uiFixed(t.elapsedMs / 1000, 1) + "s" : elapsed(t.started));
    if (!row.payloadInitialized || row.payloadArgs !== t.arguments || row.payloadSubagent !== t.subagent) {
      row.payloadInitialized = true;
      row.payloadArgs = t.arguments;
      row.payloadSubagent = t.subagent;
      row.payload = t.subagent ? null : payload(t.arguments || "");
      reconcileChildren(row.node, [row.summary, ...(row.payload ? [row.payload] : [])]);
    }
  }
  function renderLive({ newContent = false } = {}) {
    const live = $("live");
    if (!live) return;
    if (liveRender.focusoutRoot !== live) {
      liveRender.focusoutRoot = live;
      live.addEventListener("focusout", (event) => {
        const row = [...liveRender.rows.values()].find((candidate) => candidate.node.contains(event.target));
        if (!row || row.tool.status === "running") return;
        requestAnimationFrame(() => {
          if (liveRender.root !== live || liveRender.session !== S.current || !S.liveTools.includes(row.tool)
            || row.tool.status === "running" || row.node.contains(document.activeElement)) return;
          renderLive();
        });
      });
    }
    withTranscriptLayout(() => {
      const textState = currentLiveText();
      if (liveRender.session !== S.current || liveRender.root !== live) {
        liveRender.session = S.current;
        liveRender.root = live;
        liveRender.who = h("div", { class: "who assistant" }, "Formless");
        liveRender.thinkingText = document.createTextNode("");
        liveRender.thinking = h("div", { class: "thinking", id: "live-reasoning", "aria-label": "Live reasoning" }, liveRender.thinkingText);
        liveRender.thinkingToggleText = document.createTextNode("");
        liveRender.thinkingToggle = h("button", { type: "button", class: "btn ghost small", "aria-controls": "live-reasoning", onclick: () => {
          const state = currentLiveText();
          state.expanded = !state.expanded;
          renderLive();
        } }, liveRender.thinkingToggleText);
        liveRender.thinkingBox = h("div", { class: "live-thinking" }, liveRender.thinkingToggle, liveRender.thinking);
        liveRender.activityText = document.createTextNode("");
        liveRender.activity = h("div", { class: "activity" }, h("span", { class: "spinner", "aria-hidden": "true" }), h("span", { "data-live-activity": "" }, liveRender.activityText));
        liveRender.rows.clear();
      }
      const running = isRunning();
      const focused = focusedLiveTools(S.liveTools);
      const tools = S.liveTools.filter((t) => t.status === "running" || focused.has(t));
      const children = [];
      if (running || S.streaming || tools.length) {
        children.push(liveRender.who);
        if (S.showThinking && S.thinking) {
          const expanded = textState.expanded;
          const top = liveRender.thinking.scrollTop;
          const className = "thinking" + (expanded ? " expanded" : "");
          if (liveRender.thinking.className !== className) liveRender.thinking.className = className;
          const tabIndex = expanded ? 0 : -1;
          if (liveRender.thinking.tabIndex !== tabIndex) liveRender.thinking.tabIndex = tabIndex;
          if (liveRender.thinkingToggle.getAttribute("aria-expanded") !== String(expanded)) {
            liveRender.thinkingToggle.setAttribute("aria-expanded", String(expanded));
          }
          updateText(liveRender.thinkingToggleText, expanded ? "Collapse reasoning" : "Expand reasoning");
          updateText(liveRender.thinkingText, expanded ? S.thinking : cachedLiveText("thinking", S.thinking));
          // Updating the retained full text must not follow its growing end.
          liveRender.thinking.scrollTop = expanded ? top : 0;
          children.push(liveRender.thinkingBox);
        }
        for (const [tool, folded] of liveTree(S.liveTools, focused)) {
          const row = liveToolRow(tool);
          updateLiveTool(row, folded);
          children.push(row.node);
        }
        if (S.streaming) children.push(streamingNode(S.streaming));
        else if (running && S.activity) {
          updateText(liveRender.activityText, activityText(S.activity));
          children.push(liveRender.activity);
        }
      }
      const retained = new Set(S.liveTools);
      for (const [tool] of liveRender.rows) if (!retained.has(tool)) liveRender.rows.delete(tool);
      reconcileChildren(live, children);
    }, { newContent });
  }
  function updateElapsedTimers() {
    withTranscriptLayout(() => {
      if (statusRender.text) updateText(statusRender.text, isRunning() ? S.activity ? activityText(S.activity) : "running" : "● ready");
      if (liveRender.session !== S.current || liveRender.root !== $("live")) return;
      if (liveRender.activity.isConnected && S.activity) updateText(liveRender.activityText, activityText(S.activity));
      for (const row of liveRender.rows.values()) {
        if (row.node.isConnected && row.tool.elapsedMs == null) {
          const duration = elapsed(row.tool.started);
          if (row.tool.status === "running") updateText(row.status, "Running · " + duration);
          updateText(row.timer, duration);
        }
      }
    }, { newContent: false });
  }

  // Explicit anchoring also works in engines without native overflow-anchor.
  // Keep the last committed reading block, not geometry captured after reflow.
  const reading = { session: null, saved: new Map(), anchor: null, unseen: new Map(), unseenIds: new Set(), deferred: new Map(), pendingContent: false, pendingLive: false, afterSeq: -1,
    depth: 0, changed: false, adjustment: 0, intentUntil: 0, direction: 0, writeTop: null,
    writeUntil: 0, inputFrame: 0, pointer: null, observed: new Set() };
  const printMedia = window.matchMedia("print");
  const anchorBlocks = "p, pre, summary, li, h1, h2, h3, h4, .bubble, .diagram-view, .stream-tail";
  function visibleReadingBounds() {
    const box = $("transcript").getBoundingClientRect();
    const chat = $("view-chat").getBoundingClientRect();
    const viewport = window.visualViewport;
    const left = Math.max(box.left, chat.left, viewport?.offsetLeft || 0);
    const right = Math.min(box.right, chat.right, (viewport?.offsetLeft || 0) + (viewport?.width || innerWidth));
    const top = Math.max(box.top, chat.top, viewport?.offsetTop || 0);
    let bottom = Math.min(box.bottom, chat.bottom, (viewport?.offsetTop || 0) + (viewport?.height || innerHeight));
    const composer = $("composer").getBoundingClientRect();
    if (composer.height && composer.top > top) bottom = Math.min(bottom, composer.top);
    return { left, right, top, bottom, width: Math.max(0, right - left), height: Math.max(0, bottom - top) };
  }
  function readingVisible() {
    const box = $("transcript");
    if (printMedia.matches || S.view !== "chat" || !box.clientHeight || !box.getClientRects().length) return false;
    const bounds = visibleReadingBounds();
    return bounds.height > 0 && bounds.width > 0;
  }
  function captureReadingAnchor() {
    if (!readingVisible()) return null;
    const box = $("transcript"), bounds = visibleReadingBounds();
    const origin = box.getBoundingClientRect().top;
    // These are the first four eligible roots in DOM order, not necessarily
    // inside the viewport. Skip the remaining rectangle reads, but keep the
    // same lower bound and do not assume the roots are sorted by geometry.
    const messages = [];
    for (const node of box.querySelectorAll("[data-message-id], [data-live-root]")) {
      const r = node.getBoundingClientRect();
      if (!(r.height && r.bottom > bounds.top + 1)) continue;
      messages.push(node);
      if (messages.length === 4) break;
    }
    const candidates = [];
    for (const message of messages) {
      const blocks = [...message.querySelectorAll(anchorBlocks)];
      const visibleBlocks = blocks.filter(node => { const r = node.getBoundingClientRect(); return r.height && r.bottom > bounds.top + 1; });
      let block = visibleBlocks[0] || message;
      for (const child of visibleBlocks.slice(1)) {
        if (!block.contains(child)) break;
        block = child;
      }
      candidates.push({ node: block, id: message.dataset.messageId || null, live: message.hasAttribute("data-live-root"),
        block: blocks.indexOf(block), offset: block.getBoundingClientRect().top - origin,
        stream: readingStreamStamp(message, block) });
    }
    return { candidates, top: box.scrollTop };
  }
  function readingStreamStamp(message, block) {
    const stream = message.querySelector(".streaming");
    if (!message.hasAttribute("data-live-root") || !stream?.contains(block) || stream !== liveDone.node) return null;
    // Reducer state may already contain the committed step while its old live
    // DOM is still visible. Stamp the rendered text and its original sequence.
    const text = liveDone.text.trimEnd();
    // Copy only the small signatures; a sliced string must not retain an
    // entire long answer through a saved per-Session bookmark.
    const head = JSON.parse(JSON.stringify(text.slice(0, 128)));
    const tail = JSON.parse(JSON.stringify(text.slice(-128)));
    return { block: [...stream.querySelectorAll(anchorBlocks)].indexOf(block), length: text.length,
      head, tail,
      afterSeq: liveDone.afterSeq };
  }
  function readingStreamMatches(message, stream) {
    if (!stream || !stream.length || message.role !== "assistant" || message.seq <= stream.afterSeq || typeof message.content !== "string") return false;
    return message.content.length >= stream.length && message.content.startsWith(stream.head)
      && message.content.slice(stream.length - stream.tail.length, stream.length) === stream.tail;
  }
  function remapReadingStream(candidate, message) {
    if (!candidate.live || !readingStreamMatches(message, candidate.stream)) return false;
    candidate.id = message.id;
    candidate.live = false;
    candidate.block = candidate.stream.block;
    candidate.node = null;
    return true;
  }
  function commitReadingStream(session, message) {
    const anchors = [reading.session === session ? reading.anchor : null, reading.saved.get(session)?.anchor];
    for (const anchor of anchors) for (const candidate of anchor?.candidates || []) remapReadingStream(candidate, message);
  }
  function resolveReadingBlock(candidate) {
    const box = $("transcript");
    if (candidate.node?.isConnected && box.contains(candidate.node)) return candidate.node;
    // A Session may have finished while it was not selected. Resolve a saved
    // live bookmark against the matching persisted step, not an empty #live.
    if (candidate.live && candidate.stream) {
      const committed = S.messages.find(message => readingStreamMatches(message, candidate.stream));
      if (committed) remapReadingStream(candidate, committed);
    }
    const message = candidate.live ? box.querySelector("[data-live-root]")
      : [...box.querySelectorAll("[data-message-id]")].find(node => node.dataset.messageId === candidate.id);
    const node = message && (candidate.block < 0 ? message : message.querySelectorAll(anchorBlocks)[candidate.block]);
    if (node) candidate.node = node;
    return node;
  }
  function writeReadingTop(top) {
    const box = $("transcript");
    box.scrollTop = top;
    reading.writeTop = box.scrollTop;
    reading.writeUntil = performance.now() + 150;
  }
  function setReadingAdjustment(value) {
    if (Math.abs(reading.adjustment - value) < .001) return;
    reading.adjustment = value;
    $("transcript").style.setProperty("--reading-offset", value + "px");
  }
  function restoreReadingAnchor() {
    if (!readingVisible()) return false;
    const box = $("transcript");
    if (S.follow) { setReadingAdjustment(0); writeReadingTop(box.scrollHeight); return true; }
    const anchor = reading.anchor;
    if (!anchor) return false;
    for (const candidate of anchor.candidates) {
      const node = resolveReadingBlock(candidate);
      if (!node || !node.getClientRects().length) continue;
      const delta = node.getBoundingClientRect().top - box.getBoundingClientRect().top - candidate.offset;
      if (Math.abs(delta) > .01) writeReadingTop(box.scrollTop + delta);
      // WebKit quantizes scrollTop to whole CSS pixels. Retain its fractional
      // remainder in layout, rather than accepting a visible subpixel drift.
      const residual = node.getBoundingClientRect().top - box.getBoundingClientRect().top - candidate.offset;
      if (Math.abs(residual) > .01 && Math.abs(residual) < 1) {
        const adjustment = reading.adjustment - residual;
        const whole = Math.trunc(adjustment);
        setReadingAdjustment(adjustment - whole);
        if (whole) writeReadingTop(box.scrollTop - whole);
      } else if (Math.abs(residual) >= 1) reading.anchor = captureReadingAnchor();
      return true;
    }
    // During a Session snapshot load, retain the anchor until its message exists.
    if (S.messages.length) writeReadingTop(anchor.top);
    return false;
  }
  function resetReadingSession(id) {
    if (reading.session === id) return;
    if (reading.session != null) {
      const anchor = reading.anchor || (!S.follow && captureReadingAnchor());
      const unseenIds = [...reading.unseenIds, ...[...reading.unseen.keys(), ...reading.deferred.keys()].map(node =>
        (node.nodeType === Node.TEXT_NODE ? node.parentElement : node)?.closest("[data-message-id]")?.dataset.messageId).filter(Boolean)];
      const unseenLive = reading.pendingLive || [...reading.unseen.keys(), ...reading.deferred.keys()].some(node =>
        (node.nodeType === Node.TEXT_NODE ? node.parentElement : node)?.closest("[data-live-root]"));
      reading.saved.delete(reading.session);
      reading.saved.set(reading.session, { follow: S.follow,
        anchor: anchor && { top: anchor.top, candidates: anchor.candidates.map(candidate => ({ ...candidate, node: null })) }, unseenIds,
        pendingContent: reading.pendingContent, unseenLive,
        afterSeq: reading.pendingLive ? reading.afterSeq : S.messages.reduce((seq, message) => Math.max(seq, message.seq || 0), -1) });
      // Reading bookmarks retain IDs/offsets rather than detached transcript DOM.
      if (reading.saved.size > 32) reading.saved.delete(reading.saved.keys().next().value);
    }
    const saved = reading.saved.get(id);
    reading.session = id;
    reading.anchor = saved?.anchor || null;
    reading.unseen = new Map();
    reading.unseenIds = new Set(saved?.unseenIds || []);
    reading.deferred = new Map();
    reading.pendingContent = saved?.pendingContent || false;
    reading.pendingLive = saved?.unseenLive || false;
    reading.afterSeq = saved?.afterSeq ?? -1;
    setReadingAdjustment(0);
    S.follow = saved ? saved.follow : true;
    reading.intentUntil = 0;
    reading.writeTop = null;
    reading.pointer = null;
    if (reading.inputFrame) cancelAnimationFrame(reading.inputFrame);
    reading.inputFrame = 0;
    $("jump").classList.add("hidden");
  }
  function outputBottom(node, start) {
    if (node.nodeType !== Node.TEXT_NODE) return node.getBoundingClientRect().bottom;
    const range = document.createRange();
    range.setStart(node, Math.min(start || 0, node.length));
    range.setEnd(node, node.length);
    const rects = [...range.getClientRects()];
    return rects.length ? Math.max(...rects.map(rect => rect.bottom)) : -Infinity;
  }
  function markNewOutput(records) {
    if (S.follow) return;
    const box = $("transcript"), visible = readingVisible(), bottom = visibleReadingBounds().bottom;
    const semantic = "[data-message-id], .streaming, .thinking, .tree-node, .recap";
    const mark = (node, start) => {
      if (!visible) reading.deferred.set(node, start);
      else if (outputBottom(node, start) > bottom + 1) reading.unseen.set(node, start);
    };
    for (const record of records) {
      if (record.type === "characterData") {
        const node = record.target, parent = node.parentElement;
        if (!parent?.closest(semantic) || parent.closest(".tstat, .activity")) continue;
        const before = record.oldValue || "", after = node.data;
        let start = 0;
        while (start < before.length && start < after.length && before[start] === after[start]) start++;
        if (start < after.length) mark(node, start);
      } else for (const node of record.addedNodes) {
        if (!(node instanceof Element) || !box.contains(node)) continue;
        if (node.matches("button, .who, .rate, .tstat")) continue;
        const content = node.matches(semantic) || node.closest(semantic) ? [node] : [...node.querySelectorAll(semantic)];
        for (const item of content) mark(item, null);
      }
    }
  }
  function updateReadingPill() {
    const box = $("transcript");
    for (const markers of [reading.unseen, reading.deferred]) for (const node of markers.keys()) {
      if (!node.isConnected || !box.contains(node)) markers.delete(node);
    }
    if (!readingVisible()) return;
    const bottom = visibleReadingBounds().bottom;
    if (reading.pendingLive) {
      const newIds = new Set(S.messages.filter(message => message.seq > reading.afterSeq).map(message => message.id));
      const content = [...box.querySelectorAll("[data-message-id]")].filter(node => newIds.has(node.dataset.messageId));
      const live = [...box.querySelectorAll("[data-live-root] .streaming, [data-live-root] .thinking, [data-live-root] .tree-node")];
      content.push(...live.filter(node => node.textContent));
      if (content.length) {
        for (const node of content) if (node.getBoundingClientRect().bottom > bottom + 1) reading.unseen.set(node, null);
        reading.pendingLive = false;
      } else if (!S.sessionSnapshot && !isRunning()) reading.pendingLive = false;
    }
    for (const [node, start] of reading.deferred) {
      if (node.isConnected && box.contains(node) && outputBottom(node, start) > bottom + 1) reading.unseen.set(node, start);
    }
    reading.deferred.clear();
    if (reading.unseenIds.size) for (const node of box.querySelectorAll("[data-message-id]")) {
      if (reading.unseenIds.delete(node.dataset.messageId) && node.getBoundingClientRect().bottom > bottom + 1) reading.unseen.set(node, null);
    }
    for (const [node, start] of reading.unseen) {
      if (!node.isConnected || !box.contains(node) || outputBottom(node, start) <= bottom + 1) reading.unseen.delete(node);
    }
    $("jump").classList.toggle("hidden", S.follow || !reading.unseen.size);
  }
  function observeReadingNodes(records) {
    for (const record of records) if (record.type === "childList") {
      for (const root of record.addedNodes) if (root instanceof Element) {
        const nodes = root.matches("[data-message-id], [data-live-root]") ? [root] : [...root.querySelectorAll("[data-message-id], [data-live-root]")];
        for (const node of nodes) if (!reading.observed.has(node)) { reading.observed.add(node); readingResize.observe(node); }
      }
      for (const root of record.removedNodes) if (root instanceof Element) {
        const nodes = root.matches("[data-message-id], [data-live-root]") ? [root] : [...root.querySelectorAll("[data-message-id], [data-live-root]")];
        for (const node of nodes) if (!$("transcript").contains(node)) { reading.observed.delete(node); readingResize.unobserve(node); }
      }
    }
  }
  function settleReadingLayout() {
    if (!readingVisible() || reading.depth || reading.inputFrame) return;
    restoreReadingAnchor();
    if (S.follow) reading.anchor = null;
    else if (!reading.anchor) reading.anchor = captureReadingAnchor();
    updateReadingPill();
  }
  function withTranscriptLayout(update, { newContent = false } = {}) {
    resetReadingSession(S.current);
    const outer = !reading.depth++;
    if (outer) {
      reading.changed = reading.pendingContent && readingVisible();
      if (reading.changed) reading.pendingContent = false;
      if (!S.follow && (!reading.anchor || reading.inputFrame || performance.now() < reading.intentUntil)) reading.anchor = captureReadingAnchor() || reading.anchor;
    }
    reading.changed ||= newContent;
    try { return update(); }
    finally {
      reading.depth--;
      if (outer) {
        const records = readingChanges.takeRecords();
        observeReadingNodes(records);
        restoreReadingAnchor();
        if (reading.changed) markNewOutput(records);
        if (S.follow) reading.anchor = null;
        else if (!reading.anchor) reading.anchor = captureReadingAnchor();
        updateReadingPill();
      }
    }
  }
  const readingChanges = new MutationObserver(records => {
    observeReadingNodes(records);
    settleReadingLayout();
  });
  readingChanges.observe($("transcript"), { childList: true, subtree: true,
    characterData: true, characterDataOldValue: true, attributes: true, attributeFilter: ["open", "class"] });
  const readingResize = new ResizeObserver(settleReadingLayout);
  readingResize.observe($("transcript"));
  function resumeReadingFollow() {
    S.follow = true;
    reading.unseen.clear(); reading.unseenIds.clear(); reading.deferred.clear();
    reading.pendingContent = false; reading.pendingLive = false;
    setReadingAdjustment(0);
    writeReadingTop($("transcript").scrollHeight);
  }
  function scrollToBottom(force) {
    if (force) resumeReadingFollow();
    settleReadingLayout();
  }
  function readingInput(direction, duration = 1200) {
    reading.writeTop = null;
    reading.intentUntil = performance.now() + duration;
    reading.direction = direction;
    if (direction < 0) S.follow = false;
    if (reading.inputFrame) cancelAnimationFrame(reading.inputFrame);
    const session = S.current;
    reading.inputFrame = requestAnimationFrame(() => {
      reading.inputFrame = 0;
      if (S.current !== session || !readingVisible()) return;
      const box = $("transcript"), bottom = box.scrollHeight - box.clientHeight - box.scrollTop <= 1;
      if (direction > 0 && bottom) resumeReadingFollow();
      reading.anchor = captureReadingAnchor();
      updateReadingPill();
    });
  }
  $("transcript").addEventListener("wheel", e => readingInput(Math.sign(e.deltaY)), { passive: true });
  $("transcript").addEventListener("touchstart", () => readingInput(0, 2500), { passive: true });
  $("transcript").addEventListener("touchmove", () => readingInput(0, 2500), { passive: true });
  $("transcript").addEventListener("pointerdown", e => {
    const box = $("transcript"), rect = box.getBoundingClientRect();
    reading.pointer = { y: e.clientY, top: box.scrollTop };
    if (e.target === box && (e.clientX >= rect.left + box.clientWidth || e.clientY >= rect.top + box.clientHeight)) readingInput(0, 1500);
  });
  window.addEventListener("pointermove", e => {
    const pointer = reading.pointer;
    if (pointer && e.buttons && Math.abs(e.clientY - pointer.y) > 3) readingInput(Math.sign($("transcript").scrollTop - pointer.top), 1500);
  });
  window.addEventListener("pointerup", () => { reading.pointer = null; });
  $("transcript").addEventListener("keydown", e => {
    if (e.target.matches("input, textarea, select, [contenteditable='true']")) return;
    if (e.defaultPrevented || (e.key === " " && e.target.closest("button, summary, a[href], [role='button']"))) return;
    const down = ["End", "PageDown", "ArrowDown", " "].includes(e.key) && !e.shiftKey;
    const up = ["Home", "PageUp", "ArrowUp"].includes(e.key) || (e.key === " " && e.shiftKey);
    if (down || up) readingInput(down ? 1 : -1);
  });
  $("transcript").addEventListener("scroll", () => {
    if (!readingVisible()) return;
    const box = $("transcript"), now = performance.now();
    const ownWrite = reading.writeTop != null && Math.abs(box.scrollTop - reading.writeTop) <= 1 && now < reading.writeUntil;
    if (ownWrite) { updateReadingPill(); return; }
    const bottom = box.scrollHeight - box.scrollTop - box.clientHeight <= 1;
    if (now < reading.intentUntil) {
      if (!bottom) S.follow = false;
      else if (reading.direction >= 0) resumeReadingFollow();
      reading.anchor = captureReadingAnchor();
      updateReadingPill();
    } else if (S.follow && !bottom) {
      S.follow = false;
      reading.anchor = captureReadingAnchor();
    } else settleReadingLayout();
  });
  printMedia.addEventListener("change", () => { if (!printMedia.matches) requestAnimationFrame(settleReadingLayout); });
  $("jump").onclick = () => scrollToBottom(true);

  // An interrupted/paused last Run (SPEC §5.4): why, and what to do.
  async function abandonRun() {
    const sid = S.current, runId = S.unfinished?.run_id;
    if (!sid || (S.topicsAvailable && currentTopicAttempt())) return;
    const result = await act({ type: "abandon_run", session_id: sid });
    if (!result || S.current !== sid || S.unfinished?.run_id !== runId) return;
    S.unfinished = null;
    renderRunBanner();
    note(result.abandoned ? "This run has ended; it cannot be resumed." : "There is no paused run to abandon.");
  }
  function renderRunBanner() {
    const box = $("run-banner");
    const u = S.unfinished;
    const resumable = u && ["paused", "interrupted"].includes(u.status);
    box.classList.toggle("hidden", !resumable || isRunning());
    if (!resumable) return box.replaceChildren();
    // An explained provider error (SPEC §4.4): what happened, what to do,
    // and the raw error behind "details".
    const x = u.explained;
    const text = x
      ? h("span", { class: "banner-text" }, "⏸ " + x.message,
        x.action ? h("span", { class: "muted" }, " " + x.action + ".") : null,
        h("details", null, h("summary", null, "details"), h("code", null, u.error || x.details)))
      : h("span", { class: "banner-text" }, "⏸ The last Run was " + u.status + (u.error ? ": " + truncate(u.error, 200) : ""));
    box.replaceChildren(
      text,
      h("button", { class: "btn small primary", type: "button", onclick: async () => {
        await resumeRun();
      } }, tr("Resume")),
      h("button", { class: "btn small", type: "button", onclick: () => abandonRun() }, tr("Abandon")));
  }

  /** Step mode (SPEC §5.9): the call waiting for a decision. */
  function renderStep() {
    const box = $("step");
    const st = S.step;
    box.classList.toggle("hidden", !st);
    if (!st) return box.replaceChildren();
    const decide = (action, args) => act({ type: "step_decision", step_id: st.step_id, action, arguments: args || null });
    let pretty = st.arguments;
    try { pretty = JSON.stringify(JSON.parse(st.arguments), null, 2); } catch (_) {}
    box.replaceChildren(
      h("span", { class: "banner-text" }, "⏸ Step mode · about to call ", h("strong", null, st.tool_name), " (" + (st.index + 1) + " of " + st.count + ")",
        h("details", null, h("summary", null, "arguments"), h("pre", { class: "diff" }, pretty))),
      h("button", { class: "btn small primary", type: "button", onclick: () => decide("continue") }, tr("Run")),
      h("button", { class: "btn small", type: "button", onclick: async () => {
        const v = await ask({ title: "Edit the arguments for " + st.tool_name, inputKind: "machine", text: "JSON object", input: true, value: pretty });
        if (v != null) decide("edit", v);
      } }, tr("Edit")),
      h("button", { class: "btn small", type: "button", onclick: () => decide("skip") }, tr("Skip")),
      h("button", { class: "btn small", type: "button", onclick: () => { S.stepMode = false; decide("all"); } }, tr("Run the rest")));
  }

  /** The agent's checklist (`update_plan`, SPEC §5.8), above the composer. */
  function planStateLabel(state) {
    if (!state) return "";
    return [state.previous_task ? "previous task" : "", state.run_status || "", state.origin && state.origin !== "live" ? state.origin : "", Number.isFinite(Date.parse(state.updated_at)) ? "updated " + relTime(state.updated_at) : ""].filter(Boolean).join(" · ");
  }
  function renderPlan() {
    const box = $("plan");
    const items = S.plan || [];
    box.classList.toggle("hidden", !items.length);
    if (!items.length) return box.replaceChildren();
    const done = items.filter((i) => i.status === "completed").length;
    const state = S.planState, lifecycle = planStateLabel(state);
    const updated = Date.parse(state?.updated_at);
    const title = Number.isFinite(updated) ? uiFormat(new Date(updated), {
      year: "numeric", month: "long", day: "numeric", hour: "2-digit", minute: "2-digit", second: "2-digit", timeZoneName: "short",
    }) : null;
    // A successful resumed current task is complete too; copied/imported/legacy
    // and previous-task plans retain their provenance and checklist.
    const completedCurrent = state && !state.previous_task && state.run_status === "completed" &&
      (state.origin === "live" || state.origin === "resumed");
    if (done === items.length && !isRunning() && ((!lifecycle && !state?.updated_at) || completedCurrent)) {
      return box.replaceChildren(h("span", { class: "muted", title }, "✓ " + tr("Plan done") + " (" + done + "/" + items.length + ")"));
    }
    const mark = { completed: "✓", in_progress: "▸" };
    box.replaceChildren(h("span", { class: "muted", title }, tr("Plan") + " (" + done + "/" + items.length + ")" + (lifecycle ? " · " + lifecycle : "")),
      h("ul", null, items.map((i) => h("li", { class: i.status }, (mark[i.status] || "○") + " " + i.content))));
  }

  /** Messages queued for after this Run (SPEC §5.3): edit, reorder, remove. */
  function renderQueue() {
    const box = $("queue");
    const q = S.queued || [];
    box.classList.toggle("hidden", !q.length);
    if (!q.length) return box.replaceChildren();
    const sid = S.current;
    const btn = (label, title, fn) => h("button", { class: "btn ghost small", type: "button", title, "aria-label": title, onclick: fn }, label);
    box.replaceChildren(
      h("span", { class: "muted" }, "⏳ " + tr("Queued for after this Run") + " (" + q.length + ")"),
      h("ol", null, q.map((m, i) => h("li", null,
        h("span", { class: "text", title: m.content }, m.content),
        btn("✎", "Edit", () => editQueuedMessage(sid, m)),
        i > 0 ? btn("↑", "Move up", () => act({ type: "move_queued", session_id: sid, id: m.id, up: true })) : null,
        i < q.length - 1 ? btn("↓", "Move down", () => act({ type: "move_queued", session_id: sid, id: m.id, up: false })) : null,
        btn("✕", "Remove", () => act({ type: "edit_queued", session_id: sid, id: m.id, content: null }))))));
  }
  function editQueuedMessage(sid, item) {
    if (!Number.isSafeInteger(item.revision)) return toast("Update the daemon to edit queued messages safely.", true);
    const sequence = sessionSeq;
    const revision = item.revision;
    const input = h("textarea", { rows: 6, "aria-label": "Queued message" });
    input.value = item.content;
    const initial = input.value;
    return ask({
      title: "Edit the queued message", body: h("div", null, input), ok: "Save",
      saveRequest: () => {
        if (sequence !== sessionSeq || sid !== S.current || !S.queued?.some(m => m.id === item.id && m.revision === revision)) {
          note("The queued message changed. Open it again.", true);
          return null;
        }
        if (!input.value.trim()) throw Object.assign(new Error("Enter a nonempty message."), { field: input });
        return { type: "edit_queued_if_current", session_id: sid, id: item.id, revision, content: input.value === initial ? item.content : input.value };
      },
    });
  }

  /** Chips for the Tools the last Run created, updated or retired (SPEC §10.6). */
  function renderRunTools() {
    const box = $("run-tools");
    const changes = S.runTools || [];
    box.classList.toggle("hidden", !changes.length);
    if (!changes.length) return box.replaceChildren();
    box.replaceChildren(
      h("span", { class: "muted" }, "Tools this Run:"),
      ...changes.map((c) => h("button", { class: "chip " + c.change, type: "button", title: "Open " + c.name, onclick: () => {
        setView("tools");
        selectTool(c.tool_id);
      } }, c.change + " " + c.name + (c.change === "retired" ? "" : " v" + c.version))),
      h("button", { class: "btn small", type: "button", "aria-label": "Dismiss", onclick: () => { S.runTools = null; renderRunTools(); } }, "×"));
  }

  let forecastTimer = null;
  let forecastSequence = 0;
  let forecastStampLast = "";
  let forecastRevision = 0;
  function forecastStamp(draft = $("input").value) {
    const session = S.sessions.find((s) => s.id === S.current);
    return JSON.stringify([S.current, S.editMessageId, draft, session && session.model, session && session.cwd, S.chips[S.current] || [], S.daemonOnline, S.hello, forecastRevision]);
  }
  function renderForecastPreview(result) {
    const hint = $("forecast-hint");
    if (hint) {
      hint.textContent = result ? result.hint : "";
      hint.removeAttribute("title");
    }
    const disclosure = $("forecast-warnings"), list = $("forecast-warning-list");
    if (!disclosure || !list) return;
    const warnings = Array.isArray(result?.warnings) ? result.warnings.filter(text => typeof text === "string") : [];
    list.replaceChildren(...warnings.map(text => h("li", null, text)));
    disclosure.hidden = warnings.length === 0;
    if (!warnings.length) disclosure.open = false;
  }
  function scheduleForecast() {
    const stamp = forecastStamp();
    if (stamp === forecastStampLast) return;
    forecastStampLast = stamp;
    const sequence = ++forecastSequence;
    clearTimeout(forecastTimer);
    renderForecastPreview(null);
    const draft = $("input").value.trim();
    if (!S.daemonOnline || !S.current || !draft || draft.startsWith("/") || isRunning() || currentTopicAttempt()) return;
    const sid = S.current;
    forecastTimer = setTimeout(async () => {
      try {
        const result = await request({type:"forecast_run",input:{session_id:sid,draft,edit_message_id:S.editMessageId || null}});
        if (sequence !== forecastSequence || stamp !== forecastStamp()) return;
        renderForecastPreview(result);
      } catch (_) { /* A failed preview never executes or clears a draft. */ }
    }, 300);
  }
  async function confirmForecast(sid, text, operation = {}) {
    // Use the same live transport for preview, consent and the later send.
    const stamp = forecastStamp();
    const sequence = ++forecastSequence;
    const forecast = await request({type:"forecast_run",for_send:true,input:{session_id:sid,draft:text,edit_message_id:S.editMessageId || null,...operation}});
    if (sid !== S.current || stamp !== forecastStamp() || sequence !== forecastSequence || !S.daemonOnline) return false;
    if (forecast.confirmation_required) {
      if (!(await ask({title:"Forecast confirmation",text:forecast.hint,body:h("p",null,(forecast.warnings || []).join(" ")),ok:"Confirm send"}))) return false;
      if (sid !== S.current || stamp !== forecastStamp() || sequence !== forecastSequence || !S.daemonOnline) return false;
      await request({type:"confirm_forecast",id:forecast.confirmation_id});
    }
    return sid === S.current && stamp === forecastStamp() && sequence === forecastSequence && S.daemonOnline;
  }
  async function refreshForecastMetrics() {
    try {
      const result = await request({type:"run_metrics",limit:200});
      const box = $("forecast-metrics");
      if (!box) return;
      box.replaceChildren(h("h3",null,"Forecast versus actual"));
      const rows = result.forecasts || [];
      if (!rows.length) { box.append(h("p",null,"No recorded forecasts yet. Legacy Runs lack comparable frozen inputs.")); return; }
      const groups = new Map();
      for (const row of rows) {
        const key = row.model + "/" + row.role;
        const group = groups.get(key) || {n:0,error:0,covered:0,unknown:0};
        if (!row.calibration_eligible || row.token_error == null) group.unknown++; else {group.n++;group.error+=row.token_error;group.covered+=row.token_covered_p50_p90 ? 1 : 0;}
        groups.set(key,group);
      }
      for (const [key,g] of groups) box.append(h("p",null,key+": "+g.n+" completed comparisons · mean token error "+(g.n ? uiFormat(Math.round(g.error/g.n)) : "unknown")+" · p50–p90 coverage "+(g.n ? uiFormat(g.covered/g.n,{style:"percent",maximumFractionDigits:0}) : "unknown")+" · "+g.unknown+" excluded/interrupted/unknown/no-history"));
      for (const row of rows) box.append(h("details",null,h("summary",null,row.model+" · "+row.status+" · "+row.comparable_runs+" comparable Runs · actual "+(row.actual.tokens == null ? "unknown" : row.actual.tokens)+" tokens · error "+(row.token_error == null ? "unknown" : uiFormat(Math.round(row.token_error)))),h("pre",null,JSON.stringify({forecast:row.forecast,actual:row.actual},null,2))));
    } catch (_) { /* Metrics are read-only and optional on older daemons. */ }
  }

  function shareSendBlocked() {
    return !!S.shareApplyPending || !!(S.current && S.storageScope
      && S.shareAttachmentFailures?.has(sessionCacheKey(S.storageScope, S.current)));
  }
  function clearShareAttachmentFailure(token, scope, sessionId) {
    const key = sessionCacheKey(scope, sessionId);
    const failure = S.shareAttachmentFailures?.get(key);
    if (failure?.token === token) S.shareAttachmentFailures.delete(key);
  }
  async function resolveFailedSharedFiles(token, record, destination, failedFiles, reason) {
    const failures = S.shareAttachmentFailures || (S.shareAttachmentFailures = new Map());
    failures.set(sessionCacheKey(destination.scope, destination.sessionId), { token, files: failedFiles });
    const names = failedFiles.map(file => file?.name || "unnamed file").join(", ");
    const discard = await ask({
      title: "Shared files couldn't be attached",
      text: reason + ": " + names + ". Keep them to retry, or discard them? The caption remains a draft and will not be sent.",
      ok: "Discard failed files",
      cancelLabel: "Keep files",
      danger: true,
    });
    if (!discard) {
      note("Failed shared files are retained; Send stays disabled until you retry or discard them.", true);
      return false;
    }
    record.files = [];
    await storeIncomingShare(record);
    clearShareAttachmentFailure(token, destination.scope, destination.sessionId);
    note("Discarded failed shared files. The caption remains a draft; nothing was sent.");
    return true;
  }
  function renderComposer() {
    scheduleForecast();
    const running = isRunning();
    $("composer").classList.toggle("running", running);
    $("stop-btn").hidden = !running;
    $("send-btn").textContent = tr(S.editMessageId ? "Re-run" : running ? "Steer" : "Send");
    $("stop-btn").textContent = tr("Stop");
    $("input").placeholder = tr(running ? "Steer, or Ctrl/⌘+Enter to queue…" : "Message Formless…");
    const enterSends = composerEnterSends();
    const welcomeHint = historyRender.welcome?.querySelector(".welcome-enter-hint");
    if (welcomeHint) {
      const hint = welcomeEnterHint(enterSends);
      if (welcomeHint.textContent !== hint) welcomeHint.textContent = hint;
    }
    $("input").setAttribute("enterkeyhint", enterSends ? "send" : "enter");
    $("compose-hint").replaceChildren(
      h("span", { class: "compose-enter-pref", title: "Enter sends: " + (enterSends ? "on" : "off") + " · This browser Settings" }, enterModeHint(enterSends) + " · Settings"),
      h("span", { class: "compose-key-help" }, " · Shift+Enter newline · Ctrl/⌘+Enter queue · /help · ? shortcuts"));
    const mode = $("compose-mode");
    mode.replaceChildren();
    if (S.editMessageId) {
      mode.classList.remove("hidden");
      const last = S.messages[S.messages.length - 1]?.id === S.editMessageId;
      mode.append(last ? "Editing your last message — sending re-runs from there." : "Editing this message — sending deletes all later messages and re-runs from here.",
        h("button", { class: "btn ghost small", type: "button", onclick: cancelEdit }, "Cancel"));
    } else mode.classList.add("hidden");
    const s = S.sessions.find((x) => x.id === S.current);
    const attempt = currentTopicAttempt(), topic = topicForSession();
    $("session-title").textContent = attempt ? (attempt.step_title || "Step") + " · " + harnessLabel(attempt.harness)
      : topic ? topic.name : S.current === S.mainChat?.id ? "Main Chat" : s ? s.name : "No session";
    $("input").disabled = !!attempt;
    $("send-btn").disabled = !!attempt || shareSendBlocked();
    $("attach-btn").disabled = !!attempt;
    $("fork-btn").disabled = !!attempt;
    $("status-model").disabled = !!attempt;
    $("propose-topic").hidden = !S.topicsAvailable || !S.current || !!attempt;
    if (attempt) {
      $("input").placeholder = "Read-only harness transcript · continue in the topic chat";
      $("stop-btn").hidden = true;
      $("compose-hint").textContent = "Formless manages this harness conversation and its handoffs.";
    }
    renderTopicContext();
    $("show-thinking").checked = S.showThinking;
  }

  function renderStatus() {
    const state = $("state");
    if (statusRender.root !== state) {
      statusRender.root = state;
      statusRender.spinner = h("span", { class: "spinner" });
      statusRender.text = document.createTextNode("");
      state.replaceChildren(statusRender.spinner, h("span", { "data-status-activity": "" }, statusRender.text));
    }
    const running = isRunning();
    const stateClass = running ? "state busy" : "state";
    if (state.className !== stateClass) state.className = stateClass;
    if (statusRender.spinner.hidden !== !running) statusRender.spinner.hidden = !running;
    const activity = running ? S.activity : S.pullActivity || S.activity;
    updateText(statusRender.text, activity ? activityText(activity) : running ? "running" : "● ready");
    const s = S.sessions.find((x) => x.id === S.current);
    $("status-session").textContent = s ? s.name : "no session";
    $("status-session").title = s && s.cwd ? "Working directory: " + s.cwd : "";
    const others = S.others || [];
    $("status-others").classList.toggle("hidden", !others.length);
    $("status-others").textContent = others.length ? " · also open in " + othersText(others) : "";
    const model = storedSessionModel(s);
    const modelButton = $("status-model");
    const modelText = model || "Model unavailable";
    const modelLabel = model ? "Change model (" + model + ")" : "Model unavailable. Choose model.";
    if (modelButton.textContent !== modelText) modelButton.textContent = modelText;
    if (modelButton.getAttribute("aria-label") !== modelLabel) modelButton.setAttribute("aria-label", modelLabel);
    const u = S.usage && S.usage[S.current];
    $("status-usage").textContent = u && u.steps ? " │ " + usageText(u) : "";
    $("status-usage").title = S.sessionUsage && S.sessionUsage.id === S.current ? "Session (" + S.sessionUsage.runs + " Runs): " + usageText(S.sessionUsage.total) : "";
    // Context fill (SPEC §10.6): amber above 70%, red above 90%.
    const fill = S.contextFill && S.contextFill[S.current];
    const pct = fill && fill.size > 0 && fill.used > 0 ? Math.floor(fill.used * 100 / fill.size) : null;
    const contextButton = $("status-context");
    if (pct == null && document.activeElement === contextButton) focusReturn(modelButton);
    contextButton.hidden = pct == null;
    contextButton.textContent = pct == null ? "" : " │ ctx " + pct + "%";
    contextButton.setAttribute("aria-label", pct == null ? "Context window" : "Context window (" + pct + "% used)");
    contextButton.className = "linkish " + (pct >= 90 ? "err" : pct >= 70 ? "warn" : "muted");
    const waiting = S.approvals.length;
    const asking = S.pendingQuestion && !S.questionOpen ? 1 : 0;
    $("status-approvals").classList.toggle("hidden", !waiting && !asking);
    $("status-approvals").textContent = waiting
      ? "⚠ " + waiting + " awaiting approval" + (asking ? " · a question" : "")
      : "? the agent asked you something";
    const last = S.notes[S.notes.length - 1];
    const n = $("status-note");
    n.textContent = last ? last.text : "";
    n.className = "muted note" + (last && last.err ? " err" : "");
    const badge = $("notes-badge");
    badge.textContent = S.unread > 99 ? "99+" : String(S.unread);
    badge.classList.toggle("hidden", !S.unread);
    const activityToggle = $("notes-btn");
    const activityLabel = "Activity log, " + S.unread + " unread";
    if (activityToggle.getAttribute("aria-label") !== activityLabel) activityToggle.setAttribute("aria-label", activityLabel);
  }

  function setActivityDrawer(open) {
    $("notes-panel").classList.toggle("hidden", !open);
    $("notes-btn").setAttribute("aria-expanded", String(open));
    if (open) { S.unread = 0; renderStatus(); }
    (open ? $("notes-close") : $("notes-btn")).focus({ preventScroll: true });
  }

  function renderNotes() {
    const list = $("notes");
    list.replaceChildren();
    if (!S.notes.length) list.append(h("li", { class: "muted" }, h("time"), h("span", null, "Nothing yet.")));
    for (const n of [...S.notes].reverse()) {
      list.append(h("li", { class: n.err ? "err" : "" },
        h("time", null, clock(n.at)),
        h("span", null, n.text)));
    }
  }

  // -------------------------------------------------------- approvals
  /** A job's recent firings: when, trigger, outcome, time, cost, Session. */
  async function showJobHistory(j) {
    const body = h("div", { class: "job-history" });
    let closed = false, retrying = false;
    const secs = (r) => r.finished_at ? uiFormat(Math.round((new Date(r.finished_at) - new Date(r.started_at)) / 1000)) + "s" : "…";
    async function load() {
      try {
        const runs = await request({ type: "job_runs", id: j.id });
        if (!Array.isArray(runs)) throw new Error("History response was not a list");
        return { runs };
      } catch (error) { return { error }; }
    }
    function show(result) {
      if ("error" in result) {
        const retry = h("button", { class: "btn small", type: "button", onclick: async () => {
          if (retrying || closed) return;
          retrying = true; retry.disabled = true; retry.textContent = "Retrying…";
          const next = await load();
          retrying = false;
          if (closed) return;
          show(next);
          if (body.isConnected && activeDialog()?.node === $("ask-backdrop")) focusReturn($("ask-ok"));
        } }, "Retry");
        body.replaceChildren(h("p", { class: "error", role: "alert" }, "Could not load Job history. ", result.error?.message || String(result.error)), retry);
      } else if (!result.runs.length) {
        body.replaceChildren(h("p", { class: "muted" }, "It has not run yet."));
      } else {
        body.replaceChildren(h("div", { class: "table-wrap job-history-table" }, h("table", { class: "table" },
          h("thead", null, h("tr", null, ["Started", "Trigger", "Status", "Duration", "Cost", "Session", "Detail"].map(label => h("th", { scope: "col" }, label)))),
          h("tbody", null, result.runs.map((r) => h("tr", null,
            h("td", { class: "muted" }, uiFormat(new Date(r.started_at))),
            h("td", null, r.trigger),
            h("td", { class: r.status === "failed" ? "error" : r.status === "skipped" ? "warn" : "" }, r.status),
            h("td", { class: "muted" }, secs(r)),
            h("td", { class: "muted" }, r.cost != null ? uiMoney(r.cost, 3) : "—"),
            h("td", null, r.session_id ? h("button", { class: "btn ghost small", type: "button", onclick: () => openSession(r.session_id) }, "Open") : null),
            h("td", { class: "muted" }, r.detail || "")))))));
      }
    }
    show(await load());
    ask({ title: "Job history", body, ok: "Close", hideCancel: true }).then(() => { closed = true; });
  }
  /** A path Capability's wider scopes (file, folder, repository), as a
   * select; the pick is sent with the answer (SPEC §10.3). */
  function scopePicker(req, cap) {
    const opt = (req.pathOptions || []).find((o) => o.capability === cap);
    if (!opt) return null;
    const label = (c) => {
      const name = c.path.split("/").filter(Boolean).pop() || c.path;
      if (c.kind === "file") return "this file (" + name + ")";
      if (c.kind === "folder") return "folder " + name + "/";
      return (c.kind === "repo" ? "whole repo " : "project ") + c.path + (c.label ? " (" + c.label + ")" : "");
    };
    const sel = h("select", { "aria-label": "Grant scope for " + approvalText(cap) },
      opt.choices.map((c, i) => h("option", { value: String(i) }, approvalText(label(c)))));
    sel.value = String(req.picks[cap] || 0);
    sel.onchange = () => { req.picks[cap] = Number(sel.value); };
    return h("div", { class: "muted path-note" }, "Grant: ", sel, " (for Always, Session or Hour; Once allows just this call)");
  }
  /** The wider paths picked on an approval: requested spec → folder. */
  function pickedPaths(req) {
    const out = {};
    for (const o of req.pathOptions || []) {
      const i = (req.picks || {})[o.capability] || 0;
      if (i > 0 && o.choices[i]) out[o.capability] = o.choices[i].path;
    }
    return out;
  }
  /** Bidi controls in what a prompt shows (Tool names, paths, summaries)
   * are written out as `⟨U+202E⟩`, never obeyed, so `report\u202Efdp.exe`
   * can't pass for `reportexe.pdf` (#791). */
  const BIDI = /[\u061C\u200E\u200F\u202A-\u202E\u2066-\u2069]/g;
  const APPROVAL_CONTROLS = /[\u00AD\u061C\u180E\u200B-\u200F\u202A-\u202E\u2060-\u2064\u2066-\u2069\uFEFF\u{E0000}-\u{E007F}]/gu;
  function visibleCodePoint(c) {
    return "⟨U+" + c.codePointAt(0).toString(16).toUpperCase().padStart(4, "0") + "⟩";
  }
  // Exact values in a decision show invisible formatting. Prose uses showBidi.
  function approvalText(value) {
    return String(value ?? "").replace(APPROVAL_CONTROLS, visibleCodePoint);
  }
  function showBidi(root) {
    const walk = document.createTreeWalker(root, NodeFilter.SHOW_TEXT);
    for (let n = walk.nextNode(); n; n = walk.nextNode()) {
      if (BIDI.test(n.data)) n.data = n.data.replace(BIDI, visibleCodePoint);
      BIDI.lastIndex = 0;
    }
  }
  function approvalContextCard(context) {
    if (!context) return null;
    const call = context.call;
    return h("div", { class: "approval-context" },
      (context.scope_warnings || []).map(warning => h("p", { class: "error scope-warning" }, "⚠ " + approvalText(warning))),
      (context.sources || []).length ? h("section", { class: "attention source-taint" },
        h("strong", null, "This Run read external content:"),
        h("ul", null, context.sources.map(source => h("li", null, approvalText(source.tool + " (" + source.kind + "): " + source.origin))))) : null,
      call ? h("details", { open: true, class: "call-preview" }, h("summary", null, "Resolved call arguments (redacted)"),
        (call.urls || []).map(url => h("div", null, h("strong", null, approvalText(url.method || "Method unresolved")), " ", h("code", null, approvalText(url.url)),
          url.body_bytes != null ? h("p", null, "Supplied body: " + url.body_bytes + " bytes") : null,
          url.body_preview != null ? h("pre", null, "Body preview (redacted): " + approvalText(url.body_preview)) : null)),
        (call.paths || []).map(path => h("p", null, "Path: ", h("code", null, approvalText(path)))),
        h("pre", null, approvalText(JSON.stringify(call.arguments, null, 2))),
        call.truncated ? h("p", { class: "error" }, "Preview truncated; omitted values remain unresolved here.") : null,
        h("p", { class: "muted" }, call.note)) : null);
  }
  function approvalHelpSeen() {
    return S.approvalHelpSeen === null ? prefs.get("seen.approvalHelp", false) : S.approvalHelpSeen === true;
  }
  async function rememberApprovalHelp() {
    if (S.approvalHelpSeen === undefined) {
      const status = await request({ type: "status" });
      if (S.approvalHelpSeen !== true) {
        S.approvalHelpSeen = typeof status?.approval_help_seen === "boolean" ? status.approval_help_seen : null;
      }
    }
    if (S.approvalHelpSeen === null) {
      prefs.set("seen.approvalHelp", true);
      return;
    }
    if (!S.approvalHelpSeen) {
      const result = await request({ type: "acknowledge_approval_help" });
      if (result?.approval_help_seen !== true) throw new Error("The daemon did not record the preference");
      S.approvalHelpSeen = true;
    }
  }
  function renderApproval() {
    const req = S.approvals[0];
    if (!req) S.approvalLater = false;
    const alert = $("modal-confirm");
    const confirmation = req?.broadConfirm && !S.approvalLater
      ? "This grants broad access. Press the same button (or key) again to confirm, or allow it once." : "";
    if (alert.textContent !== confirmation) alert.textContent = confirmation;
    $("modal-backdrop").classList.toggle("hidden", !req || !!S.approvalLater);
    if (!req || S.approvalLater) return restoreFocus("modal-backdrop");
    if (req.kind === "harness") return renderHarnessPermission(req);
    const promo = req.kind === "promotion";
    for (const id of ["modal-approve", "modal-once", "modal-session", "modal-hour", "modal-deny", "modal-deny-always"]) {
      const button = $(id);
      if (button) button.disabled = !!req.deciding;
    }
    $("modal-title").textContent = tr(promo ? "Promotion approval" : "Approval required");
    document.querySelector("#modal-backdrop .modal").classList.add("alert");
    const body = $("modal-body");
    body.replaceChildren(h("div", null,
      // The first approval ever: what this is and how to undo it (#394).
      !promo && !approvalHelpSeen() ? h("p", { class: "first-approval" },
        tr("First approval? Tools run in a sandbox with no access at all; each Capability below (files, network, programs…) is something this Tool asks to use. Always lets it keep using them, Allow once covers just this call, This Session covers the current Session, and Deny says no. Change your mind any time: /approvals lists what you allowed, /revoke takes it back.")) : null,
      // A parallel batch: every held call, each answered on its own (§5.2).
      req.batch ? h("div", { class: "batch-list" },
        h("strong", null, "Call " + req.batch.index + " of " + req.batch.calls.length + " held in this batch: answer each one"),
        h("ol", null, req.batch.calls.map((c, i) => {
          const answered = ((S.batchAnswers || {})[req.batch.batch_id] || {})[i + 1];
          const now = i + 1 === req.batch.index;
          return h("li", { class: now ? "now" : "muted" },
            h("span", { class: "batch-state" }, answered || (now ? "▶ now" : "waiting")), " ", h("strong", null, approvalText(c.tool_name)), " ",
            (c.new_capabilities.length ? c.new_capabilities : c.capabilities).map((cap) => h("span", { class: "tag" + (isBroad(cap) ? " broad" : "") }, approvalText(cap))));
        }))) : null,
      // What it does in plain words: AI-written, so labelled (SPEC §10.3).
      req.summary && (!req.detail || req.tool_id) ? h("div", { class: "ai-summary" }, h("span", { class: "ai-label" }, "AI summary · may be wrong; the Capabilities are what counts"), h("p", null, req.summary)) : null,
      req.detail ? h("p", { class: "detail-text" }, approvalText(req.detail)) : null,
      !req.detail || req.tool_id ? h("p", null, "Tool ", h("strong", null, approvalText(toolTitleByName(req.tool_name) || req.tool_name)),
          toolTitleByName(req.tool_name) ? h("span", { class: "muted" }, " (" + approvalText(req.tool_name) + ")") : null,
          req.version ? " v" + req.version : "",
          promo ? " is ready to be promoted." : " wants sensitive capabilities:") : null,
      promo && req.reason ? h("p", { class: "muted" }, req.reason) : null,
      approvalContextCard(req.context),
      h("ul", { "aria-label": promo ? "Capabilities" : "Capabilities requested" },
        (req.capabilities.length ? req.capabilities : ["(no capabilities)"]).map((c) =>
          (req.already || []).includes(c)
            ? h("li", { class: "muted" }, h("span", { class: "tag" }, approvalText(c)), " already allowed")
            : h("li", null, h("span", { class: "tag" + (isBroad(c) ? " broad" : "") }, approvalText(c)), isBroad(c) ? h("span", { class: "error" }, " broad access: " + broadWhy(c)) : (req.already || []).length ? h("span", null, " new") : null,
              ((n) => n ? h("div", { class: n[0] ? "error path-note" : "muted path-note" }, (n[0] ? "⚠ " : "") + approvalText(n[1])) : null)(pathNote(c, ((S.sessions.find((s) => s.id === req.session_id) || {}).cwd) || null)),
              scopePicker(req, c))),
        req.detail && !req.tool_id ? null : h("li", null, h("span", { class: "tag" }, "memory " + req.memory_mb + " MB"))),
      req.code ? h("details", { class: "chip" }, h("summary", null, "Code (first lines)"), h("pre", { class: "diff" }, approvalText(req.code))) : null,
      h("p", { class: "muted" }, tr(promo
        ? "The new version replaces the current one; rollback stays available."
        : "Approve lasts for this Tool until its capabilities widen. Allow once covers this call only."))));
    showBidi(body);
    $("modal-once").classList.toggle("hidden", promo);
    $("modal-session").classList.toggle("hidden", promo);
    $("modal-hour").classList.toggle("hidden", promo);
    // Always-deny remembers a Tool's Capabilities: not offered on other
    // prompts (an MCP server, a harness change) (#764).
    $("modal-deny-always").classList.toggle("hidden", promo || !req.tool_id);
    $("modal-approve").classList.remove("hidden");
    $("modal-deny").classList.remove("hidden");
    for (const [id, key, label] of [
      ["modal-approve", "y", promo ? "Approve" : "Always"],
      ["modal-once", "o", "Allow once"], ["modal-session", "s", "This Session"],
      ["modal-hour", "1", "For 1 hour"], ["modal-deny", "n", "Deny"],
      ["modal-deny-always", "N", "Always deny"],
    ]) {
      $(id).replaceChildren(h("kbd", { "aria-hidden": "true" }, key), " " + tr(label));
    }
    $("modal-later").textContent = tr("Later");
    rememberFocus("modal-backdrop");
    $("modal-more").textContent = S.approvals.length > 1 ? "+" + (S.approvals.length - 1) + " more waiting" : "";
    openDialog($("modal-backdrop"), { trigger: focusBefore["modal-backdrop"],
      initialFocus: $("modal-approve"), background: true,
      canDismiss: () => !S.approvals[0]?.deciding,
      onDismiss: () => { S.approvalLater = true; renderApproval(); } });
  }
  function renderHarnessPermission(req) {
    $("modal-title").textContent = harnessLabel(req.harness) + " permission";
    document.querySelector("#modal-backdrop .modal").classList.add("alert");
    for (const id of ["modal-approve", "modal-once", "modal-session", "modal-hour", "modal-deny-always"]) $(id).classList.add("hidden");
    $("modal-deny").classList.remove("hidden");
    $("modal-deny").textContent = "Deny request";
    $("modal-deny").disabled = !!req.deciding;
    const body = $("modal-body");
    body.replaceChildren(h("div", null, h("p", null, req.title), h("p", { class: "muted" },
      sessionName(req.session_id), req.acp_agent ? " · " + req.acp_agent : "", " · ", req.permissionKind || "Permission"),
      req.raw_input == null ? null : h("details", null, h("summary", null, "Request details"), h("pre", { class: "diff" }, JSON.stringify(req.raw_input, null, 2))),
      h("div", { class: "topic-actions" }, (req.options || []).map(option => h("button", {
        type: "button", class: "btn small", disabled: !!req.deciding,
        onclick: () => decideHarnessPermission(req, option.option_id)
      }, option.name, ["allow_always", "reject_always"].includes(option.kind) ? " (this attempt only)" : ""))),
      h("p", { class: "muted" }, "Only you can grant permission. Choices apply to this harness attempt, not future attempts. Leaving it unanswered does not grant access.")));
    showBidi(body);
    $("modal-later").textContent = tr("Later");
    $("modal-more").textContent = S.approvals.length > 1 ? "+" + (S.approvals.length - 1) + " more waiting" : "";
    rememberFocus("modal-backdrop");
    openDialog($("modal-backdrop"), { trigger: focusBefore["modal-backdrop"], initialFocus: $("modal-deny"), background: true,
      canDismiss: () => !S.approvals[0]?.deciding,
      onDismiss: () => { S.approvalLater = true; renderApproval(); } });
  }
  async function decideHarnessPermission(req, optionId) {
    if (req.kind !== "harness" || req.deciding || !S.approvals.includes(req)) return;
    if (optionId !== null && !(req.options || []).some(option => option.option_id === optionId)) return;
    const channel = S.channel;
    req.deciding = true; renderApproval();
    try {
      await request({ type: "harness_permission_decision", permission_id: req.id, option_id: optionId });
      if (S.channel === channel) S.approvals = S.approvals.filter(item => item !== req);
    } catch (error) {
      if (S.channel === channel) toast("Permission decision was not confirmed: " + error.message, true);
    } finally {
      req.deciding = false;
      if (S.channel === channel) { renderApproval(); renderStatus(); }
    }
  }
  /** The agent's open question in a dialog; Cancel keeps it pending (the
   * status bar and `a` bring it back). */
  function queueQuestion(ev) {
    const questions = S.questions = S.questions || [];
    if (questions.some((q) => q.id === ev.question_id)) return;
    const qs = ev.questions && ev.questions.length ? ev.questions
      : [{ question: ev.question, options: (ev.options || []).map((label) => ({ label })) }];
    questions.push({ id: ev.question_id, session_id: ev.session_id, topic_id: ev.topic_id, attempt_id: ev.attempt_id, user_only: !!ev.user_only, qs });
    S.pendingQuestion = questions[0];
    S.questionId = S.pendingQuestion.id;
    showQuestion();
  }
  function removeQuestion(id) {
    S.questions = (S.questions || []).filter((q) => q.id !== id);
    cancelAsk("question:" + id);
    S.pendingQuestion = S.questions[0] || null;
    S.questionId = S.pendingQuestion && S.pendingQuestion.id;
    renderStatus();
    showQuestion();
  }
  function showQuestion() {
    const q = S.pendingQuestion;
    if (!q || S.questionOpen) return;
    S.questionOpen = true;
    renderStatus();
    const body = questionForm(q.qs);
    ask({ title: q.user_only ? "Your decision is needed" : q.attempt_id ? "The harness asks" : "The agent asks", text: q.user_only ? "Only you can answer this question. Formless cannot decide for you."
      : q.attempt_id ? sessionName(q.session_id) + " · Formless may answer only from your request or decision, with a cited basis posted in Topic Chat. The first answer wins." : undefined,
      body, ok: "Answer", tag: "question:" + q.id }).then(async (done) => {
      S.questionOpen = false;
      if (!S.pendingQuestion || S.pendingQuestion.id !== q.id) { renderStatus(); showQuestion(); return; }
      if (!done) return renderStatus();
      const answers = readQuestionForm(body, q.qs);
      if (answers.every((a) => !a.selected.length && !a.text)) return renderStatus();
      const answer = answers.map((a) => a.selected.concat(a.text ? [a.text] : []).join(", ")).join("; ");
      const result = await act({ type: "answer_question", question_id: q.id, answer, answers });
      if (result && result.answered) removeQuestion(q.id);
      else renderStatus();
    });
  }
  /** `a` or the status bar: the oldest pending approval, else the open
   * question, brought back into view and focused. */
  function focusPending() {
    if (S.approvals.length) {
      S.approvalLater = false;
      renderApproval();
      $("modal-approve").focus();
    } else if (S.pendingQuestion) showQuestion();
  }
  $("status-approvals").onclick = focusPending;
  $("modal-later").onclick = () => { S.approvalLater = true; renderApproval(); };
  // Every modal shares the same stack, focus boundary and inert background.
  // Re-rendering an existing dialog does not raise it or replace its invoker.
  const dialogStack = [];
  const dialogInert = new Map();
  const focusBefore = {};
  let syncingDialogs = false;
  let lastFocusTarget = null;
  let focusRecoveryFrame = 0;
  function activeDialog() {
    return dialogStack[dialogStack.length - 1] || null;
  }
  function focusVisible(el) {
    return !!(el && el !== document.body && el.isConnected && typeof el.focus === "function"
      && !el.matches(":disabled") && !el.closest("[inert], .hidden, [hidden]") && el.getClientRects().length
      && getComputedStyle(el).visibility !== "hidden");
  }
  function dialogItems(node) {
    const selector = "button, a[href], input, select, textarea, summary, [tabindex]";
    const feedback = node === activeDialog()?.node ? [...$("toasts").querySelectorAll(selector)] : [];
    return [...node.querySelectorAll(selector), ...feedback]
      .filter((el) => el.tabIndex >= 0 && focusVisible(el));
  }
  function dialogAllows(entry, node) {
    return entry.node.contains(node) || $("toasts").contains(node);
  }
  function focusReturn(el) {
    const top = activeDialog();
    if (focusVisible(el) && (!top || dialogAllows(top, el))) {
      el.focus({ preventScroll: true });
      if (document.activeElement === el) return;
    }
    if (top) return focusDialog(top);
    // A re-render or view change can remove/hide the original control.
    const candidates = [document.querySelector('#tabs button[aria-current="page"]'),
      $("view-" + S.view)?.querySelector("button, input, select, textarea, [tabindex]"), $("input"), $("secret")];
    const fallback = candidates.find(focusVisible);
    if (fallback) return fallback.focus({ preventScroll: true });
    const root = !$("app").classList.contains("hidden") ? $("app") : $("pair");
    root.tabIndex = -1;
    root.focus({ preventScroll: true });
  }
  function focusDialog(entry, preferred) {
    if (activeDialog() !== entry) return;
    const initial = preferred || (typeof entry.initialFocus === "function" ? entry.initialFocus() : entry.initialFocus);
    const target = focusVisible(initial) && entry.node.contains(initial) ? initial : dialogItems(entry.node).find(item => entry.node.contains(item));
    const surface = entry.node.querySelector(".modal") || entry.node;
    if (!target) surface.tabIndex = -1;
    (target || surface).focus({ preventScroll: true });
  }
  function syncDialogs() {
    if (syncingDialogs) return;
    syncingDialogs = true;
    const removed = dialogStack.filter((entry) => !entry.node.isConnected || entry.node.classList.contains("hidden") || entry.node.hidden);
    const previousTop = activeDialog();
    for (const entry of removed) {
      if (popover && entry.node.contains(popover)) closePopover({ restore: false });
      dialogStack.splice(dialogStack.indexOf(entry), 1);
      releaseDialog(entry);
    }
    const top = activeDialog();
    if (top) {
      for (const node of document.body.children) {
        if (!dialogInert.has(node)) dialogInert.set(node, node.inert);
        const inert = node !== top.node && !node.hasAttribute("data-dialog-keep");
        if (node.inert !== inert) node.inert = inert;
      }
      dialogStack.forEach((entry, index) => { entry.node.style.zIndex = String(100 + index); });
      $("toasts").style.zIndex = String(101 + dialogStack.length);
    } else {
      for (const [node, inert] of dialogInert) if (node.inert !== inert) node.inert = inert;
      dialogInert.clear();
      $("toasts").style.removeProperty("z-index");
    }
    syncingDialogs = false;
    if (previousTop && removed.includes(previousTop)) focusReturn(previousTop.trigger);
    else if (top && !dialogAllows(top, document.activeElement)) focusDialog(top);
  }
  function openDialog(node, options = {}) {
    const existing = dialogStack.find((entry) => entry.node === node);
    if (existing) {
      for (const key of ["onDismiss", "canDismiss"]) if (key in options) existing[key] = options[key];
      syncDialogs();
      return existing;
    }
    const trigger = options.trigger || popoverTrigger || document.activeElement;
    closePopover({ restore: false });
    if (!node.isConnected) document.body.append(node);
    node.classList.remove("hidden");
    node.hidden = false;
    const entry = { node, trigger, initialFocus: options.initialFocus, onDismiss: options.onDismiss,
      canDismiss: options.canDismiss, zIndex: node.style.zIndex };
    // An arriving approval waits behind the user's current modal. An explicit
    // nested prompt/viewer instead becomes the active layer.
    if (options.background && dialogStack.length) dialogStack.unshift(entry);
    else dialogStack.push(entry);
    let pointerStartedOutside = false, pointerEndedOutside = false;
    entry.pointerdown = (e) => {
      pointerStartedOutside = e.target === node;
      pointerEndedOutside = false;
      if (pointerStartedOutside) e.preventDefault();
    };
    entry.pointerup = (e) => {
      pointerEndedOutside = e.target === node;
      // Selecting/dragging non-focusable text can blur a control to body.
      // Do not cancel that gesture; restore modal focus after it finishes.
      if (activeDialog() === entry && !node.contains(document.activeElement)) focusDialog(entry, entry.lastFocus);
    };
    entry.click = (e) => {
      if (e.target === node && pointerStartedOutside && pointerEndedOutside) dismissDialog(entry);
      else if (activeDialog() === entry && !node.contains(document.activeElement)) focusDialog(entry, entry.lastFocus);
      pointerStartedOutside = pointerEndedOutside = false;
    };
    node.addEventListener("pointerdown", entry.pointerdown);
    node.addEventListener("pointerup", entry.pointerup);
    node.addEventListener("click", entry.click);
    syncDialogs();
    if (activeDialog() === entry) focusDialog(entry);
    return entry;
  }
  function releaseDialog(entry) {
    entry.node.removeEventListener("pointerdown", entry.pointerdown);
    entry.node.removeEventListener("pointerup", entry.pointerup);
    entry.node.removeEventListener("click", entry.click);
    entry.node.style.zIndex = entry.zIndex;
  }
  function dismissDialog(entry) {
    if (activeDialog() !== entry) return;
    const allowed = typeof entry.canDismiss === "function" ? entry.canDismiss() : entry.canDismiss !== false;
    if (!entry.onDismiss || !allowed) return focusDialog(entry, entry.lastFocus);
    entry.onDismiss();
    if (activeDialog() === entry) focusDialog(entry, entry.lastFocus);
  }
  function closeDialog(node, { remove = true } = {}) {
    const index = dialogStack.findIndex((entry) => entry.node === node);
    const entry = dialogStack[index];
    const wasTop = entry && activeDialog() === entry;
    if (entry) {
      if (popover && node.contains(popover)) closePopover({ restore: false });
      dialogStack.splice(index, 1);
      releaseDialog(entry);
    }
    if (remove) node.remove();
    else node.classList.add("hidden");
    syncDialogs();
    if (wasTop) focusReturn(entry.trigger);
  }
  function rememberFocus(id) {
    if (focusBefore[id] === undefined && !$(id).contains(document.activeElement))
      focusBefore[id] = popoverTrigger || document.activeElement;
  }
  function restoreFocus(id) {
    closeDialog($(id), { remove: false });
    delete focusBefore[id];
  }
  document.addEventListener("keydown", (e) => {
    const top = activeDialog();
    if (!top || (popover && popover.contains(e.target))) return;
    if (e.key === "Escape") {
      e.preventDefault();
      e.stopImmediatePropagation();
      if (!e.repeat) dismissDialog(top);
    } else if (e.key === "Tab") {
      const items = dialogItems(top.node);
      const first = items[0], last = items[items.length - 1];
      if (!items.length) { e.preventDefault(); focusDialog(top); }
      else if (items.some(item => !top.node.contains(item))) {
        const index = items.indexOf(document.activeElement);
        e.preventDefault();
        (index < 0 ? e.shiftKey ? last : first : items[(index + (e.shiftKey ? -1 : 1) + items.length) % items.length]).focus();
      }
      else if (!dialogAllows(top, document.activeElement)) { e.preventDefault(); (e.shiftKey ? last : first).focus(); }
      else if (e.shiftKey && (document.activeElement === first || !items.includes(document.activeElement))) { e.preventDefault(); last.focus(); }
      else if (!e.shiftKey && (document.activeElement === last || !items.includes(document.activeElement))) { e.preventDefault(); first.focus(); }
    }
  }, true);
  document.addEventListener("focusin", (e) => {
    if (e.target !== document.body && e.target !== document.documentElement) lastFocusTarget = e.target;
    const top = activeDialog();
    if (top && !dialogAllows(top, e.target)) focusDialog(top);
    else if (top) top.lastFocus = e.target;
  });
  document.addEventListener("pointerdown", (event) => {
    if (lastFocusTarget && !lastFocusTarget.contains(event.target)) lastFocusTarget = null;
  }, true);
  function focusRemovedByState(target) {
    return !target.isConnected || !!target.closest(".hidden, [hidden], [inert]");
  }
  function scheduleFocusRecovery() {
    if (focusRecoveryFrame || document.activeElement !== document.body || !lastFocusTarget || !focusRemovedByState(lastFocusTarget)) return;
    focusRecoveryFrame = requestAnimationFrame(() => {
      focusRecoveryFrame = 0;
      const target = lastFocusTarget;
      if (target && document.activeElement === document.body && focusRemovedByState(target)) focusReturn(target);
    });
  }
  new MutationObserver(() => {
    if (dialogStack.length || dialogInert.size) syncDialogs();
    // A menu action, view transition or streamed re-render can remove the
    // restored control after a dialog has already closed. Retain visible user
    // focus; recover only when its previous target itself disappeared/was hidden.
    scheduleFocusRecovery();
  })
    .observe(document.body, { childList: true, subtree: true, attributes: true, attributeFilter: ["class", "hidden"] });
  async function decide(approved, scope, rememberDenial) {
    // Granting broad access beyond this call takes a second press (§10.3).
    const head = S.approvals[0];
    if (!head || head.deciding) return;
    // Native approval shortcuts must never translate into a harness grant.
    if (head.kind === "harness") return approved ? undefined : decideHarnessPermission(head, null);
    if (approved && head.kind !== "promotion" && scope !== "once") {
      const broad = (head.capabilities || []).some((c) => !(head.already || []).includes(c) && isBroad(c));
      if (broad && head.broadConfirm !== (scope || "always")) {
        head.broadConfirm = scope || "always";
        renderApproval();
        $("modal-confirm").scrollIntoView({ block: "nearest" });
        return;
      }
    }
    const req = head;
    const sawHelp = req.kind !== "promotion" && !approvalHelpSeen();
    req.deciding = true;
    renderApproval();
    let result;
    try {
      result = await request(req.kind === "promotion"
        ? { type: "promotion_decision", promotion_id: req.id, approved }
        : { type: "approval_decision", approval_id: req.id, approved, scope: scope || "always", paths: scope === "once" ? {} : pickedPaths(req), remember_denial: !!rememberDenial });
    } catch (e) {
      toast("Decision was not confirmed: " + e.message + ". Inspect current approvals or grants before retrying.", true);
      note("Decision was not confirmed: " + e.message + ". Inspect current approvals or grants before retrying.", true);
      return;
    } finally {
      req.deciding = false;
      renderApproval();
      renderStatus();
    }
    S.approvals = S.approvals.filter((a) => a !== req);
    if (!result || !result.resolved) {
      note("That approval is no longer pending; no decision was applied.");
      renderApproval();
      renderStatus();
      return;
    }
    // A batch's later prompts show how each earlier call was answered.
    if (req.batch) {
      const answers = (S.batchAnswers = S.batchAnswers || {})[req.batch.batch_id] = S.batchAnswers[req.batch.batch_id] || {};
      answers[req.batch.index] = !approved ? "denied" : scope === "once" ? "once" : scope === "session" ? "this Session" : scope === "hour" ? "for 1 hour" : "always";
    }
    if (sawHelp) {
      try { await rememberApprovalHelp(); }
      catch (error) { note("Could not save the approval help preference (" + error.message + ")", true); }
    }
    note((approved ? "approved " : "denied ") + (req.kind === "promotion" ? "promotion of " : "") + req.tool_name + (scope === "once" ? " once" : scope === "session" ? " for this Session" : scope === "hour" ? " for an hour" : ""));
    if (!S.approvals.length && isRunning()) setActivity("waiting for model");
    renderApproval();
    renderStatus();
  }
  $("status-model").onclick = () => openModelPicker();
  $("status-context").onclick = () => showContext();
  $("conn-retry").onclick = () => retryNow();
  for (const id of ["tools-filter", "tools-quick"]) $(id).addEventListener("input", () => renderToolsTable());
  // The Sort menu picks a column in its natural order.
  $("tools-sort").addEventListener("input", () => { S.toolSort = { key: $("tools-sort").value, reverse: false }; renderToolsTable(); });
  for (const id of ["proposals-filter", "proposals-quick"]) $(id).addEventListener("input", () => renderProposals());
  $("modal-approve").onclick = () => decide(true);
  $("modal-once").onclick = () => decide(true, "once");
  $("modal-session").onclick = () => decide(true, "session");
  $("modal-hour").onclick = () => decide(true, "hour");
  $("modal-deny").onclick = () => decide(false);
  $("modal-deny-always").onclick = () => decide(false, "always", true);

  // ------------------------------------------------------------- tools
  function toolStatusDot(t) {
    return h("span", { class: "dot" + (t.retired ? " retired" : t.disabled ? " off" : t.needs_attention ? " attention" : ""), title: t.retired ? "retired" : t.disabled ? "switched off" : t.needs_attention ? "needs attention" : "active" });
  }
  /** A Tool's title with its name as secondary text, or just the name (SPEC §7.2). */
  function toolLabel(t) {
    const pin = t.pinned ? h("span", { class: "tool-pin", title: "pinned: always listed to the agent" }, "⚲ ") : null;
    const tags = (t.tags || []).map((g) => h("span", { class: "tool-tag" }, "#" + g));
    return [pin, t.title ? [t.title, h("span", { class: "muted tool-name" }, " " + t.name)] : t.name, tags];
  }
  function toolTitleByName(name) {
    const t = (S.tools || []).find((x) => x.name === name);
    return t && t.title;
  }
  // Tools table columns: [label, sort key or null, numeric].
  const TOOL_COLS = [["Tool", "name"], ["Ver", "version"], ["Calls", "calls", true], ["Success", "rate", true], ["Last used", "used"]];
  function toolsTable(table, tools, opts) {
    table.replaceChildren();
    const sort = opts.sortable && S.toolSort;
    const cols = TOOL_COLS.concat([[opts.bars ? "" : "Capabilities", null]]);
    // Sortable headers: click (or Enter) sorts by the column, again reverses.
    // Name and version ascend; calls, success and last used descend. The
    // active glyph and aria-sort both come from that one direction.
    table.append(h("thead", null, h("tr", null, cols.map(([label, key, num]) => {
      const active = sort && key && sort.key === key;
      const direction = active ? (sort.reverse !== (key === "name" || key === "version") ? "ascending" : "descending") : null;
      const glyph = direction === "ascending" ? " ▴" : direction === "descending" ? " ▾" : "";
      const th = h("th", { class: num ? "num" : null, "aria-sort": direction },
        opts.sortable && key ? h("button", { class: "th-sort", type: "button", title: "Sort by " + label.toLowerCase(), onclick: () => {
          S.toolSort = { key, reverse: active ? !sort.reverse : false };
          if ([...$("tools-sort").options].some((o) => o.value === key)) $("tools-sort").value = key;
          renderToolsTable();
        } }, label, glyph) : label);
      return th;
    }))));
    const body = h("tbody");
    if (!tools.length) body.append(h("tr", null, h("td", { class: "empty", colspan: cols.length },
      opts.emptyMessage || "No Tools yet — the agent builds them with create_tool.")));
    for (const t of tools) {
      body.append(h("tr", {
        class: opts.selected === t.id ? "selected" : null, tabindex: 0,
        onclick: () => opts.onselect(t.id),
        onkeydown: (e) => { if (e.key === "Enter") opts.onselect(t.id); },
      },
      h("td", null, toolStatusDot(t), toolLabel(t), t.retired ? h("span", { class: "muted" }, " (retired)") : null),
      h("td", null, "v" + t.current_version),
      h("td", { class: "num" }, t.invocations),
      h("td", { class: "num" }, rateCell(t.success_rate)),
      h("td", { class: "muted" }, t.last_used_at ? relTime(t.last_used_at) : "—"),
      opts.bars ? h("td", null, bar(t.success_rate)) : h("td", { class: "caps" }, (t.capabilities || []).join(", ") || "—")));
    }
    table.append(body);
  }

  /** The Tools view's rows after its filter, quick filter and sort (the
   * same rules as the TUI's `/`, `v` and `s`). */
  function visibleTools() {
    const q = $("tools-filter").value.trim();
    const quick = $("tools-quick").value;
    const rows = S.tools.filter((t) => {
      if (quick === "active" && t.retired) return false;
      if (quick === "retired" && !t.retired) return false;
      if (quick === "failing" && !(t.success_rate != null && t.success_rate < 0.8)) return false;
      if (quick === "attention" && !t.needs_attention) return false;
      // `#git` filters by tag; anything else also matches tags.
      if (q.startsWith("#")) return (t.tags || []).some((g) => g.startsWith(q.slice(1).toLowerCase()));
      return !q || fuzzyMatch(q, t.name) || fuzzyMatch(q, t.title || "") || fuzzyMatch(q, t.description || "") || fuzzyMatch(q, (t.tags || []).join(" "));
    });
    // Each column's natural order (names A→Z, numbers and dates biggest
    // first); a second click on the header reverses it.
    const by = {
      name: (a, b) => a.name.localeCompare(b.name),
      version: (a, b) => a.current_version - b.current_version,
      rate: (a, b) => (b.success_rate ?? -1) - (a.success_rate ?? -1),
      calls: (a, b) => b.invocations - a.invocations,
    };
    const sort = S.toolSort || { key: $("tools-sort").value, reverse: false };
    if (sort.key === "used") {
      return rows.sort((a, b) => {
        const left = a.last_used_at || "", right = b.last_used_at || "";
        // Binary order matches the retained UTC timestamps in SQLite,
        // including whole seconds next to fractional ones.
        const order = left < right ? -1 : left > right ? 1 : 0;
        return Number(!left) - Number(!right) ||
          (sort.reverse ? order : -order) ||
          a.name.localeCompare(b.name) || a.id.localeCompare(b.id);
      });
    }
    const cmp = by[sort.key];
    if (!cmp) return rows;
    rows.sort(cmp);
    return sort.reverse ? rows.reverse() : rows;
  }
  function renderToolsTable() {
    const rows = visibleTools();
    $("tools-count").textContent = "(" + (rows.length === S.tools.length ? S.tools.length : rows.length + "/" + S.tools.length) + ")";
    toolsTable($("tools-table"), rows, { selected: S.selectedTool, onselect: selectTool, sortable: true,
      emptyMessage: S.tools.length ? "No Tools match the current filters." : null });
  }
  function renderTools() {
    renderToolsTable();
    const box = $("tool-detail");
    box.replaceChildren();
    const d = S.toolDetail;
    if (!d || d.tool.id !== S.selectedTool) {
      box.append(toolDetailFailure(S.selectedTool) || h("p", { class: "placeholder" }, S.selectedTool ? "Loading Tool details…" : S.tools.length ? "Select a Tool for its versions, metrics, optimiser history, and code." : "Tools appear here once the agent creates them."));
    } else {
      renderToolDetail(box, d);
    }
    if (S.modules.length) {
      box.append(h("h3", null, "Shared modules ", h("span", { class: "muted" }, "(requireModule)")));
      box.append(h("dl", { class: "kv" }, S.modules.map((m) => [
        h("dt", null, h("button", { class: "linkish", type: "button", "aria-expanded": S.moduleDetail && S.moduleDetail.module.id === m.id ? "true" : "false",
          onclick: () => openModule(S.moduleDetail && S.moduleDetail.module.id === m.id ? null : m.id) }, m.name + " v" + m.current_version)),
        h("dd", null, m.description)])));
      const md = S.moduleDetail;
      if (md && S.modules.some((m) => m.id === md.module.id)) box.append(moduleDetail(md));
    }
  }

  // --------------------------------------------------- approvals (§10.3)
  const capFamily = (c) => String(c).split(":")[0];
  let approvalListSeq = 0;
  async function refreshApprovals() {
    const seq = ++approvalListSeq;
    const list = await act({ type: "list_approvals" });
    if (seq !== approvalListSeq || !Array.isArray(list)) return false;
    S.grants = list;
    S.grantsSelected = new Set();
    S.unconfirmedGrants?.clear();
    renderApprovals();
    return true;
  }
  const grantKey = (g) => [g.tool_id, g.capability, g.scope, g.session_id || ""].join("|");
  function shownGrants() {
    const fam = $("approvals-family").value;
    return (S.grants || []).filter((g) => !fam || capFamily(g.capability) === fam);
  }
  function renderApprovals() {
    const all = S.grants || [];
    const sel = $("approvals-family");
    const fams = [...new Set(all.map((g) => capFamily(g.capability)))].sort();
    const cur = sel.value;
    sel.replaceChildren(h("option", { value: "" }, "All families"), ...fams.map((f) => h("option", { value: f }, f)));
    sel.value = fams.includes(cur) ? cur : "";
    const rows = shownGrants();
    $("approvals-count").textContent = "(" + rows.length + ")";
    const when = (t) => (t ? relTime(t) : "—");
    const table = $("approvals-table");
    table.replaceChildren(h("thead", null, h("tr", null, ["", "Tool", "Capability", "Scope", "Granted", "By", "Ends", "Last used", ""].map((c) => h("th", null, c)))));
    const body = h("tbody");
    if (!rows.length) body.append(h("tr", null, h("td", { class: "empty", colspan: 9 }, "Nothing approved yet.")));
    for (const g of rows) {
      const key = grantKey(g);
      body.append(h("tr", null,
        h("td", null, h("input", { type: "checkbox", "aria-label": "Select " + g.tool_name + " " + g.capability, checked: S.grantsSelected.has(key),
          onchange: (e) => { if (e.target.checked) S.grantsSelected.add(key); else S.grantsSelected.delete(key); } })),
        h("td", null, g.tool_name),
        h("td", null, h("span", { class: "tag" + (isBroad(g.capability) ? " broad" : "") }, g.capability),
          g.scope_warning ? h("div", { class: "error scope-warning" }, "⚠ " + g.scope_warning) : null,
          g.trusted ? h("div", { class: "muted path-note" }, "also trusted in config.toml: revoking won't make it ask") : null),
        h("td", null, g.scope === "always" ? "always" : g.scope === "session" ? "this Session" : "for an hour"),
        h("td", { class: "muted" }, when(g.granted_at)),
        h("td", { class: "muted" }, g.granted_by || "—"),
        h("td", { class: "muted" }, g.expires_at ? untilTime(g.expires_at) : "—"),
        h("td", { class: "muted" }, when(g.last_used)),
        h("td", null, h("button", { class: "btn ghost small", type: "button", onclick: () => revokeGrants([g]) }, "Revoke"))));
    }
    table.append(body);
  }
  async function revokeGrants(grants) {
    if (!grants.length) return toast("Select grants to revoke first.", true);
    if (S.revokingGrants) return;
    S.revokingGrants = true;
    const feedback = $("revocation-feedback");
    try {
      // A lost response may have followed a write. Re-read before retrying
      // any unconfirmed grant so the user can review what still exists.
      if (grants.some((g) => S.unconfirmedGrants?.has(grantKey(g)))) {
        if (await refreshApprovals()) return toast("Grant list refreshed. Review it before retrying.");
        return toast("Refresh the grants before retrying.", true);
      }
      const trusted = grants.filter((g) => g.trusted).map((g) => g.capability);
      if (grants.length > 1 && !(await ask({ title: "Revoke " + grants.length + " grants?", text: trusted.length
        ? "Their Tools ask again on their next call, except for what your trusted folders/hosts still allow: " + [...new Set(trusted)].join(", ") + "."
        : "Their Tools ask again on their next call.", ok: "Revoke", danger: true }))) return;
      const still = new Set(), revoked = new Set(), unconfirmed = new Set(), failures = [];
      for (const g of grants) {
        const key = grantKey(g);
        try {
          const r = await request({ type: "revoke_grant", tool_id: g.tool_id, capability: g.capability, scope: g.scope, session_id: g.session_id || null });
          if (r?.revoked !== true) throw new Error("The daemon did not confirm revocation.");
          revoked.add(key);
          if (Array.isArray(r.still_trusted)) r.still_trusted.forEach((c) => still.add(c));
        } catch (e) {
          unconfirmed.add(key);
          failures.push(g.tool_name + " · " + g.capability + " (" + g.scope
            + (g.session_id ? " " + g.session_id.slice(0, 8) : "") + "): " + e.message);
        }
      }
      S.unconfirmedGrants = new Set([...(S.unconfirmedGrants || []), ...unconfirmed]);
      S.grants = (S.grants || []).filter((g) => !revoked.has(grantKey(g)));
      for (const key of revoked) S.grantsSelected.delete(key);
      renderApprovals();
      const revokedText = "Revoked " + revoked.size + " grant" + (revoked.size === 1 ? "" : "s");
      const summary = failures.length ? (revoked.size ? revokedText + ". " : "")
        + failures.length + (failures.length === 1 ? " revocation unconfirmed." : " revocations unconfirmed.") : revokedText;
      toast(summary, failures.length > 0);
      feedback.replaceChildren(...(failures.length ? [
        h("p", { class: "error" }, summary),
        h("ul", null, failures.map((message) => h("li", null, message))),
      ] : []));
      if (still.size) note("Still allowed without asking by your trusted folders/hosts: " + [...still].join(", "));
      if (await refreshApprovals()) {
        if (failures.length) feedback.append(h("p", { class: "muted" }, "Check the updated list and retry any grants that are still shown."));
      } else {
        feedback.append(h("p", { class: "error" }, "The grant list could not be refreshed. Refresh it before retrying an unconfirmed revocation."),
          h("button", { class: "btn small", type: "button", onclick: async (e) => {
            const button = e.currentTarget;
            button.disabled = true;
            if (await refreshApprovals()) {
              button.previousElementSibling.textContent = "Check the updated list and retry any grants that are still shown.";
              button.remove();
              focusReturn($("revoke-selected"));
            } else button.disabled = false;
          } }, "Refresh grants"));
      }
    } finally {
      S.revokingGrants = false;
    }
  }
  $("approvals-family").addEventListener("input", () => renderApprovals());
  $("revoke-selected").onclick = () => revokeGrants(shownGrants().filter((g) => S.grantsSelected.has(grantKey(g))));
  $("revoke-all").onclick = () => revokeGrants(shownGrants());

  // ----------------------------------------------------- modules (§7.7)
  async function openModule(id) {
    S.moduleEditing = false;
    S.moduleDetail = id ? await act({ type: "get_module", id }) : null;
    renderTools();
  }
  function moduleDetail(d) {
    const m = d.module;
    const cur = d.versions.find((v) => v.version === m.current_version) || d.versions[d.versions.length - 1] || { code: "" };
    const box = h("div", { class: "module-detail" },
      h("h3", null, "Module " + m.name), descriptionReviewCard(d.description_review, d.description_reviews),
      h("dl", { class: "kv" },
        h("dt", null, "Versions"), h("dd", null, h("div", { class: "versions" }, d.versions.map((v) =>
          v.version === m.current_version
            ? h("span", { class: "version current", title: "current" }, "v" + v.version + " " + v.author)
            : h("button", { class: "version", type: "button", title: "Make v" + v.version + "'s code current again (as a new version)", onclick: async () => {
              if (await act({ type: "rollback_module", id: m.id, version: v.version }, "Rolled " + m.name + " back to v" + v.version + "'s code")) openModule(m.id);
            } }, "v" + v.version + " " + v.author)))),
        h("dt", null, "Used by"), h("dd", null, d.used_by.length ? dependencyLinks(d.used_by) : h("span", { class: "muted" }, "no Tool"))));
    if (S.moduleEditing) {
      const area = h("textarea", { class: "editor", spellcheck: "false", "aria-label": "Module code" });
      area.value = cur.code;
      box.append(area, h("div", { class: "detail-actions" },
        h("button", { class: "btn small primary", type: "button", onclick: async () => {
          const r = await act({ type: "update_module_code", id: m.id, code: area.value });
          if (r) { toast("Saved " + m.name + " v" + r.version); openModule(m.id); }
        } }, "Save as new version"),
        h("button", { class: "btn small", type: "button", onclick: () => { S.moduleEditing = false; renderTools(); } }, "Cancel")));
    } else {
      box.append(h("pre", { class: "code" }, cur.code), h("div", { class: "detail-actions" },
        h("button", { class: "btn small", type: "button", onclick: () => { S.moduleEditing = true; renderTools(); } }, "Edit code"),
        h("button", { class: "btn small danger", type: "button", onclick: async () => {
          const ok = await ask({ title: "Delete module " + m.name + "?", text: (d.used_by.length ? "It is loaded by " + d.used_by.join(", ") + "; they will fail. " : "") + "All its versions are deleted.", ok: "Delete", danger: true });
          if (ok && (await act({ type: "delete_module", id: m.id }, "Deleted module " + m.name))) { S.moduleDetail = null; renderTools(); }
        } }, "Delete")));
    }
    return box;
  }

  function metricsBlock(m) {
    return h("dl", { class: "kv" },
      h("dt", null, "Success"), h("dd", null, bar(m.success_rate), " ", rateCell(m.success_rate), h("span", { class: "muted" }, "  retry " + pct(m.retry_rate))),
      h("dt", null, "Invocations"), h("dd", null, m.total + " total · " + m.success + " ok · " + m.failure + " failed · " + m.denied + " denied · " + m.cancelled + " cancelled · " + m.validation_failures + " invalid"),
      h("dt", null, "Latency"), h("dd", null, "p50 " + (m.p50_ms == null ? "-" : uiFormat(Math.round(m.p50_ms)) + "ms") + " · p95 " + (m.p95_ms == null ? "-" : uiFormat(Math.round(m.p95_ms)) + "ms")),
      h("dt", null, "Cost per success"), h("dd", null, m.ms_per_success == null ? "-" : uiFixed(m.ms_per_success / 1000, 1) + "s · " + uiFormat(Math.round(m.tokens_per_success || 0)) + " tokens"),
      // The Tool's own model use (llm / agent), per call (SPEC §10.4).
      m.model_tokens_per_call == null ? null : [h("dt", null, "Model use"), h("dd", null, uiFormat(Math.round(m.model_tokens_per_call)) + " tokens" +
        (m.model_cost_per_call == null ? " per call (unpriced)" : " · " + uiMoney(m.model_cost_per_call, 4) + " per call"))]);
  }

  /** "Try it": a form from the Tool's input schema; runs it through the
   * usual approval path as part of the current Session (SPEC §10.4). */
  async function tryTool(t, version) {
    if (!S.current) return toast("Open a Session first: the run belongs to one.", true);
    let schema = {};
    try { schema = JSON.parse((version && version.input_schema) || "{}"); } catch (_) {}
    const props = (schema && schema.properties) || {};
    const required = new Set(schema.required || []);
    const typeOf = (p) => (Array.isArray(p.type) ? p.type.find((x) => x !== "null") : p.type) || (p.properties ? "object" : "string");
    const fields = Object.entries(props).map(([name, p]) => {
      const type = typeOf(p);
      const init = p.default !== undefined ? p.default : p.examples && p.examples.length ? p.examples[0] : undefined;
      let input;
      if (p.enum) input = h("select", null, p.enum.map((v) => h("option", { value: JSON.stringify(v) }, String(v))));
      else if (type === "boolean") input = h("input", { type: "checkbox", checked: init === true });
      else if (type === "number" || type === "integer") input = h("input", { type: "number", value: init !== undefined ? String(init) : "" });
      else if (type === "string") input = h("input", { type: "text", value: init !== undefined ? String(init) : "", spellcheck: "false" });
      else input = h("textarea", { rows: 3, spellcheck: "false" }, JSON.stringify(init !== undefined ? init : type === "array" ? [] : {}, null, 2));
      return { name, type, p, input, el: h("label", { class: "try-field" },
        h("span", null, name, required.has(name) ? " *" : "", p.description ? h("span", { class: "muted" }, " — " + p.description) : null), input) };
    });
    const dry = h("input", { type: "checkbox" });
    const body = h("div", { class: "try-form" }, fields.length ? fields.map((f) => f.el) : h("p", { class: "muted" }, "This Tool takes no input."),
      h("label", { class: "try-field try-dry" }, dry, " Dry run: nothing is changed (writes stay in memory, commands and non-GET requests aren't made); shows what it would do"));
    if (!(await ask({ title: "Try " + t.name, text: "Runs in the current Session, asking for approval as usual.", body, ok: "Run" }))) return;
    const input = {};
    for (const f of fields) {
      const el = f.input;
      try {
        if (f.p.enum) input[f.name] = JSON.parse(el.value);
        else if (f.type === "boolean") input[f.name] = el.checked;
        else if (f.type === "number" || f.type === "integer") { if (el.value !== "") input[f.name] = Number(el.value); }
        else if (f.type === "string") { if (el.value !== "" || required.has(f.name)) input[f.name] = el.value; }
        else input[f.name] = JSON.parse(el.value || "null");
      } catch (e) { return toast(f.name + ": not valid JSON", true); }
    }
    if (dry.checked) {
      toast("Dry run of " + t.name + "…");
      const d = await act({ type: "dry_run_tool", id: t.id, input, session_id: S.current }, null, 300000);
      if (!d) return;
      const effects = d.effects || [];
      await ask({ title: t.name + " dry run", text: "Nothing was changed.", ok: "Close", body: h("div", null,
        h("p", null, effects.length ? "It would have:" : "It would have had no side effects."),
        effects.length ? h("ul", { class: "dry-effects" }, effects.map((e) => h("li", null, e))) : null,
        d.ok ? h("pre", { class: "try-result" }, JSON.stringify(d.result, null, 2)) : h("p", { class: "error" }, "Error: " + d.error)) });
      return;
    }
    toast("Running " + t.name + "…");
    const r = await act({ type: "run_tool", id: t.id, input, session_id: S.current }, null, 300000);
    if (!r) return;
    let text = String(r.result);
    try { text = JSON.stringify(JSON.parse(text), null, 2); } catch (_) {}
    await ask({ title: t.name + " result", body: h("pre", { class: "try-result" }, text), ok: "Close" });
    selectTool(t.id, true);
  }
  /** Description text is third-party data. Use text nodes, never HTML. */
  function descriptionReviewCard(metadata, reviews) {
    metadata = metadata || {};
    return h("div", { class: "description-review" },
      (metadata.flags || []).length ? h("div", { class: "attention" }, h("strong", null, "Description warning"),
        h("ul", null, metadata.flags.map((flag) => h("li", null, flag))),
        h("p", null, "Third-party metadata, not instructions"), h("pre", null, JSON.stringify(metadata.text || ""))) : null,
      (reviews || []).map((review) => h("details", { class: "chip", open: !!review.before },
        h("summary", null, review.kind + " " + review.name + " v" + review.version + " · description review"),
        h("p", { class: "muted" }, "Source: " + review.source),
        (review.after.flags || []).map((flag) => h("p", { class: "error" }, flag)),
        review.before ? h("div", null, h("strong", null, "Before"), h("pre", { class: "diff" }, JSON.stringify(review.before.text))) : null,
        h("strong", null, "After · third-party metadata"), h("pre", { class: "diff" }, JSON.stringify(review.after.text)))));
  }
  async function showDescriptionReviews() {
    const connection = [S.ws, S.conn, S.channel];
    const reviews = await act({ type: "list_description_reviews", limit: 100 });
    if (!reviews || connection[0] !== S.ws || connection[1] !== S.conn || connection[2] !== S.channel) return;
    await ask({ title: "Description reviews", text: reviews.length ? "Retained warnings and exact before/after changes." : "No description reviews yet.",
      body: descriptionReviewCard(null, reviews), ok: "Close" });
  }
  async function refreshDescriptionReviews() {
    const revision = S.descriptionReviewRevision = (S.descriptionReviewRevision || 0) + 1;
    const connection = [S.ws, S.conn, S.channel];
    const reviews = await request({ type: "list_description_reviews", limit: 20 }).catch(() => null);
    if (!reviews || revision !== S.descriptionReviewRevision || connection[0] !== S.ws || connection[1] !== S.conn || connection[2] !== S.channel) return;
    const seen = new Set((S.descriptionReviews || []).map((review) => review.id));
    S.descriptionReviews = reviews;
    for (const review of reviews) if (!seen.has(review.id)) {
      note("Description review: " + review.kind + " " + review.name + " v" + review.version +
        ((review.after.flags || []).length ? " · " + review.after.flags.join("; ") : "") +
        (review.before ? "\nBefore: " + JSON.stringify(review.before.text) : "") +
        "\nAfter: " + JSON.stringify(review.after.text) + "\nRetained in Settings → Description reviews.");
    }
  }
  async function previewToolBundleDescriptions(bundle) {
    const connection = [S.ws, S.conn, S.channel];
    const current = () => connection[0] === S.ws && connection[1] === S.conn && connection[2] === S.channel;
    const grouped = Array.isArray(bundle.tools) || Array.isArray(bundle.modules) || Array.isArray(bundle.skills);
    const entries = grouped ? (bundle.tools || []).concat(bundle.modules || [], bundle.skills || []) : [bundle];
    const previews = await act({ type: "preview_description_bundle", bundle });
    if (!previews || !current()) return null;
    const body = h("div", { class: "description-preview" });
    const metadata = new WeakMap();
    for (let index = 0; index < previews.length; index++) {
      const preview = previews[index];
      if (entries[index] && typeof entries[index] === "object") metadata.set(entries[index], preview.after);
      const review = { kind: preview.kind, name: preview.name, version: "preview", source: "import", before: preview.before == null ? null : { text: preview.before }, after: preview.after };
      body.append(descriptionReviewCard(preview.after, [review]));
    }
    const approved = await ask({ title: "Review imported descriptions", text: "Descriptions are third-party metadata. Skill metadata is parsed from the actual bundled SKILL.md. Warnings do not establish safety. Code and capability approval remain separate.", body, ok: "Continue" });
    return approved && current() ? metadata : null;
  }
  // One selected version pair only; never persisted or shared across connections.
  function toolVersionChanges(id, from, to) {
    const channel = S.channel, ws = S.ws, conn = S.conn;
    const valid = () => {
      const d = S.toolDetail;
      const idx = d && d.versions.findIndex((v) => v.version === to);
      return S.channel === channel && S.ws === ws && S.conn === conn &&
        S.selectedTool === id && d && d.tool.id === id && d.tool.current_version === to &&
        idx > 0 && d.versions[idx - 1].version === from;
    };
    const pre = h("pre", { class: "diff" }, "Open to load changes.");
    const details = h("details", { class: "chip" }, h("summary", null, "Changes v" + from + " → v" + to), pre);
    details.addEventListener("toggle", async () => {
      if (!details.open || !details.isConnected || !valid()) return;
      let cached = S.toolVersionDiff;
      if (!cached || cached.channel !== channel || cached.ws !== ws || cached.conn !== conn ||
          cached.id !== id || cached.from !== from || cached.to !== to) {
        cached = { channel, ws, conn, id, from, to };
        S.toolVersionDiff = cached;
        // Keep the settled result (including errors) to avoid repeated requests
        // on toggles. A new selection/connection permits a new attempt.
        cached.result = request({ type: "diff_tool_versions", id, from, to })
          .then((value) => ({ value }), (error) => ({ error }));
      }
      pre.textContent = "loading…";
      const result = await cached.result;
      if (!valid() || S.toolVersionDiff !== cached || !details.isConnected) return;
      if (result.error) pre.textContent = String(result.error.message || result.error);
      else if (typeof result.value?.diff === "string") pre.replaceChildren(...renderDiffLines(result.value.diff, "div"));
      else pre.textContent = "No version diff returned.";
    });
    return details;
  }
  function focusToolEditor(area) {
    const editing = S.editingCode;
    if (!editing || !editing.focusPending) return;
    editing.focusPending = false;
    const active = document.activeElement;
    setTimeout(() => {
      if (S.editingCode === editing && area.isConnected && document.activeElement === active) area.focus();
    }, 0);
  }
  function renderToolDetail(box, d) {
    const t = d.tool;
    const current = d.versions.find((v) => v.version === t.current_version) || d.versions[d.versions.length - 1];
    box.append(h("h3", null, toolLabel(t)), h("p", { class: "muted", style: "margin:0" }, t.description), descriptionReviewCard(d.description_review, d.description_reviews));
    box.append(h("dl", { class: "kv" },
      h("dt", null, "Status"), h("dd", null, h("span", { class: "status-pill " + (t.disabled ? "mid" : t.needs_attention ? "bad" : t.retired ? "" : "good") }, t.disabled ? "switched off" : t.needs_attention ? "needs attention" : t.retired ? "retired" : "active")),
      h("dt", null, "Versions"), h("dd", null, h("div", { class: "versions" }, d.versions.map((v) =>
        v.version === t.current_version
          ? h("span", { class: "version current", title: "current" }, "v" + v.version + " " + v.author)
          : h("button", { class: "version", type: "button", title: "Roll back to v" + v.version, onclick: () => rollbackTool(t, v.version) }, "v" + v.version + " " + v.author)))),
      h("dt", null, "Tags"), h("dd", null, h("input", { class: "tags-input", type: "text", value: (t.tags || []).join(" "), placeholder: "git web …", "aria-label": "Tags, separated by spaces", title: "Enter saves",
        onkeydown: (e) => { if (e.key === "Enter") act({ type: "set_tool_tags", id: t.id, tags: e.target.value.split(/[\s,]+/).filter(Boolean) }, "Tags saved"); } })),
      h("dt", null, "Uses"), h("dd", null, dependencyLinks(d.uses), (d.modules || []).length ? h("span", { class: "muted" }, (d.uses || []).length ? " · modules " : "modules ", d.modules.join(", ")) : null, !(d.uses || []).length && !(d.modules || []).length ? h("span", { class: "muted" }, "nothing") : null),
      h("dt", null, "Used by"), h("dd", null, (d.used_by || []).length ? dependencyLinks(d.used_by) : h("span", { class: "muted" }, "no other Tool")),
      h("dt", null, "Capabilities"), h("dd", null, capTags(t.capabilities)),
      h("dt", null, "Memory"), h("dd", null, t.memory_mb + " MB"),
      h("dt", null, "Approved"), h("dd", null, d.approval
        ? h("div", null,
            d.approval.capabilities.map((c) => h("span", { class: "tag" }, c,
              h("button", { class: "btn ghost small", type: "button", title: "Revoke " + c, onclick: () => revokeApproval(t, c) }, "×"))),
            h("button", { class: "btn small", type: "button", onclick: () => revokeApproval(t, null) }, "Revoke all"))
        : h("span", { class: "muted" }, "nothing yet (asks on first sensitive call)"))));
    if (d.attention && d.attention.length) {
      box.append(h("div", { class: "attention" },
        h("strong", null, t.needs_attention ? "Why it needs attention" : "Worth a look"),
        h("ul", null, d.attention.map((r) => h("li", null, r)))));
    }
    box.append(metricsBlock(d.metrics));
    if (d.example) {
      // A real call: the latest successful Invocation, redacted (SPEC §10.4).
      const pretty = (s) => { try { return JSON.stringify(JSON.parse(s), null, 2); } catch (_) { return s; } };
      box.append(h("details", { class: "chip example", open: true },
        h("summary", null, "Example ", h("span", { class: "muted" }, "· the latest successful call, v" + d.example.version + " · " + relTime(d.example.created_at))),
        h("div", { class: "muted" }, "input"), h("pre", null, pretty(d.example.input)),
        h("div", { class: "muted" }, "output"), h("pre", null, pretty(d.example.output))));
    }
    const idx = d.versions.findIndex((v) => v.version === t.current_version);
    if (idx > 0) {
      // What the current version changed (optimiser promotion or agent edit).
      const from = d.versions[idx - 1].version;
      box.append(toolVersionChanges(t.id, from, t.current_version));
    }
    const editing = S.editingCode && S.editingCode.toolId === t.id;
    box.append(h("div", { class: "detail-actions" },
      editing ? null : h("button", { class: "btn small", type: "button", onclick: () => { S.editingCode = { toolId: t.id, code: current ? current.code : "", focusPending: true }; renderTools(); } }, "Edit code"),
      t.retired ? null : h("button", { class: "btn small", type: "button", onclick: () => tryTool(t, current) }, "Try it"),
      h("button", { class: "btn small", type: "button", onclick: () => act({ type: "run_optimiser", tool_id: t.id }, "Optimiser started for " + t.name) }, "Optimise"),
      h("button", { class: "btn small", type: "button", onclick: () => retireTool(t, d) }, t.retired ? "Restore" : "Retire"),
      t.current_version > 1 ? h("button", { class: "btn small", type: "button", onclick: () => rollbackTool(t, t.current_version - 1) }, "Roll back") : null,
      t.retired ? null : h("button", { class: "btn small", type: "button", title: "Switched off: hidden from the agent and uncallable; versions and metrics stay", onclick: () => act({ type: "set_tool_disabled", id: t.id, disabled: !t.disabled }, (t.disabled ? "Switched on " : "Switched off ") + t.name) }, t.disabled ? "Switch on" : "Switch off"),
      h("button", { class: "btn small", type: "button", title: "Pinned Tools are listed to the agent in every Session (from its next listing)", onclick: () => act({ type: "set_tool_pinned", id: t.id, pinned: !t.pinned }, (t.pinned ? "Unpinned " : "Pinned ") + t.name) },
        t.pinned ? "Unpin" : "Pin (~" + (d.listing_tokens || 0) + " tokens/request)"),
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
      const area = h("textarea", { class: "editor", spellcheck: "false", "aria-label": "Tool code", "aria-describedby": "editor-status" });
      area.value = S.editingCode.code;
      const ed = codeEditor(area);
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
      box.append(ed, h("div", { class: "detail-actions" },
        h("button", { class: "btn primary small", type: "button", onclick: () => saveCode(t) }, "Save new version"),
        h("button", { class: "btn ghost small", type: "button", onclick: () => { S.editingCode = null; renderTools(); } }, "Cancel"),
        h("span", { class: "muted", style: "font-size:0.75rem" }, "Ctrl/⌘+S save · Esc cancel")));
      focusToolEditor(area);
    } else if (current) {
      box.append(h("h3", null, "Code v" + current.version));
      const rows = current.code.split("\n").map((l, i) => h("tr", null, h("td", { class: "ln" }, i + 1), h("td", { class: "src" }, l)));
      box.append(h("div", { class: "code" }, h("table", null, h("tbody", null, rows))));
    }
  }

  /** The Tool code editor (SPEC §10.4): the textarea's text is drawn by a
   * highlighted layer behind it, a second layer marks the bracket pair at
   * the cursor and the line of a syntax error, and the daemon's syntax gate
   * checks the code as you type (CheckToolCode), with its error below. */
  function codeEditor(area) {
    const hl = h("pre", { class: "ed-layer ed-hl", "aria-hidden": "true" });
    const marks = h("pre", { class: "ed-layer ed-marks", "aria-hidden": "true" });
    const status = h("div", { class: "ed-status muted", id: "editor-status", "aria-live": "polite" });
    const wrap = h("div", { class: "code-editor" }, hl, marks, area);
    let errLine = null, timer = null, seq = 0;
    const PAIRS = { "(": ")", "[": "]", "{": "}" };
    const CLOSE = { ")": "(", "]": "[", "}": "{" };
    // The bracket at (or just before) the cursor and its partner.
    function bracketPair(text, at) {
      for (const i of [at, at - 1]) {
        const c = text[i];
        if (PAIRS[c]) {
          let depth = 0;
          for (let j = i; j < text.length; j++) {
            if (text[j] === c) depth++;
            else if (text[j] === PAIRS[c] && --depth === 0) return [i, j];
          }
        } else if (CLOSE[c]) {
          let depth = 0;
          for (let j = i; j >= 0; j--) {
            if (text[j] === c) depth++;
            else if (text[j] === CLOSE[c] && --depth === 0) return [j, i];
          }
        }
      }
      return null;
    }
    let paintedText = null, lineData = [], previousPair = [], previousError = null;
    function rowAt(position) {
      let low = 0, high = lineData.length;
      while (low < high) {
        const mid = (low + high) >>> 1;
        if (lineData[mid].start <= position) low = mid + 1;
        else high = mid;
      }
      const row = low - 1, data = lineData[row];
      return data && position < data.start + data.text.length ? row : null;
    }
    function paint() {
      const text = area.value;
      if (text !== paintedText) {
        const nodes = highlight(text + "\n", "js");
        hl.replaceChildren(...(nodes || [text + "\n"]));
        // Rebuild line structure only for a changed source, preserving the
        // existing extra newline that aligns the textarea's final line.
        const lines = (text + "\n").split("\n");
        let offset = 0;
        lineData = lines.map(line => {
          const data = { text: line, start: offset, node: h("span", { class: "ed-line" }, line) };
          offset += line.length + 1;
          return data;
        });
        marks.replaceChildren(...lineData.flatMap((data, n) => n < lineData.length - 1 ? [data.node, "\n"] : [data.node]));
        paintedText = text;
        previousPair = [];
        previousError = null;
      }
      const pair = bracketPair(text, area.selectionStart) || [];
      if (pair.length !== previousPair.length || pair.some((position, i) => position !== previousPair[i])) {
        const changed = new Set([...previousPair, ...pair].map(rowAt));
        for (const row of changed) {
          if (row === null) continue;
          const data = lineData[row], parts = [];
          let at = 0;
          for (const position of pair) {
            if (position >= data.start && position < data.start + data.text.length) {
              parts.push(data.text.slice(at, position - data.start), h("span", { class: "ed-bracket" }, data.text[position - data.start]));
              at = position - data.start + 1;
            }
          }
          parts.push(data.text.slice(at));
          data.node.replaceChildren(...parts);
        }
        previousPair = pair;
      }
      if (errLine !== previousError) {
        // Keep bracket children intact when only the diagnostic moved.
        for (const line of [previousError, errLine]) {
          const node = Number.isInteger(line) && lineData[line - 1]?.node;
          if (node) node.classList.toggle("ed-err", line === errLine);
        }
        previousError = errLine;
      }
      sync();
    }
    function sync() {
      for (const layer of [hl, marks]) {
        layer.scrollTop = area.scrollTop;
        layer.scrollLeft = area.scrollLeft;
      }
    }
    async function check() {
      const mine = ++seq;
      const r = await request({ type: "check_tool_code", code: area.value }).catch(() => null);
      if (mine !== seq || !r) return;
      errLine = r.ok ? null : r.line || null;
      status.className = "ed-status " + (r.ok ? "muted" : "error");
      status.textContent = r.ok ? "✓ syntax ok" : "✗ " + (r.line ? "line " + r.line + ": " : "") + r.error;
      paint();
    }
    area.addEventListener("input", () => { paint(); clearTimeout(timer); timer = setTimeout(check, 400); });
    for (const ev of ["click", "keyup", "select"]) area.addEventListener(ev, paint);
    area.addEventListener("scroll", sync);
    setTimeout(() => { paint(); check(); }, 0);
    return h("div", null, wrap, status);
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
    } else {
      const failure = toolDetailFailure(S.metricTool);
      if (failure) box.append(failure);
    }
    const rt = $("routing-table");
    rt.replaceChildren(h("thead", null, h("tr", null, ["Model", "Calls", "Success", "Avg latency", "Prompt tokens", "Cached"].map((c, i) => h("th", { class: i ? "num" : null }, c)))));
    const body = h("tbody");
    if (!S.modelStats.length) body.append(h("tr", null, h("td", { class: "empty", colspan: 6 }, "No provider calls measured yet.")));
    for (const st of S.modelStats) {
      const n = st.successes + st.failures;
      const r = n ? st.successes / n : null;
      body.append(h("tr", null, h("td", null, st.model), h("td", { class: "num" }, n), h("td", { class: "num" }, rateCell(r)),
        h("td", { class: "num" }, n ? uiFormat(Math.round(st.total_ms / n)) + "ms" : "-"),
        h("td", { class: "num" }, st.prompt_tokens ? uiFormat(st.prompt_tokens) : "-"),
        h("td", { class: "num" }, st.prompt_tokens ? uiFormat(st.cached_tokens / st.prompt_tokens, { style: "percent", maximumFractionDigits: 0 }) : "-")));
    }
    rt.append(body);
  }

  function renderProposals() {
    const box = $("proposals");
    box.replaceChildren();
    if (!S.proposals.length) {
      $("proposals-count").textContent = "";
      box.append(h("div", { class: "detail placeholder" }, "No proposals yet. Generate one to have the optimiser draft and measure a prompt or memory addendum."));
      return;
    }
    const q = $("proposals-filter").value.trim();
    const quick = $("proposals-quick").value;
    const shown = S.proposals.filter((p) => {
      if (quick === "pending" && ["applied", "rejected", "rolled_back"].includes(p.status)) return false;
      if ((quick === "applied" || quick === "rejected") && p.status !== quick) return false;
      return !q || [p.content, p.rationale, p.kind].some((f) => fuzzyMatch(q, f || ""));
    });
    $("proposals-count").textContent = shown.length === S.proposals.length ? "" : "(" + shown.length + "/" + S.proposals.length + ")";
    if (!shown.length) box.append(h("div", { class: "detail placeholder" }, "No proposal matches the filter."));
    for (const p of shown) {
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
        p.rationale ? h("p", { class: "muted", style: "margin:0.5rem 0 0" }, p.rationale) : null,
        actions.length ? h("div", { class: "actions" }, actions) : null));
    }
  }

  // The daemon projects documented keys through its existing settable allow-list.
  // Never infer edit permission from a section name or a rendered value.
  function settingsRows(settings) {
    const docs = settings.docs || {};
    const choices = settings.choices || {};
    const rows = new Map();
    for (const [key, value] of Object.entries(settings.values || {})) {
      if (!Object.hasOwn(docs, key)) continue;
      const options = choices[key];
      const kind = Array.isArray(options) ? "choice" : typeof value === "boolean" ? "bool"
        : typeof value === "number" ? "number" : typeof value === "string" ? "text"
        : Array.isArray(value) ? "list" : "json";
      rows.set(key, { key, value, kind, choices: options || [], editable: true });
    }
    // Old daemons can still show their safe summary, but cannot grant edit
    // permission without the new explicit projection.
    const readOnly = settings.read_only || Object.fromEntries(
      ["provider", "tui", "limits", "naming", "relay", "paths"].flatMap(prefix =>
        Object.entries(settings[prefix] || {}).map(([key, value]) => [prefix + "." + key, value])));
    for (const [key, value] of Object.entries(readOnly)) {
      if (!rows.has(key)) rows.set(key, { key, value, editable: false });
    }
    return [...rows.values()].sort((a, b) => a.key < b.key ? -1 : a.key > b.key ? 1 : 0);
  }
  function settingDisplay(value) {
    if (value == null) return "";
    if (typeof value === "boolean") return value ? "on" : "off";
    if (typeof value === "number") return uiFormat(value);
    if (typeof value === "string") return value;
    return JSON.stringify(value);
  }
  function parseSettingInput(text, kind) {
    const value = text.trim();
    if (!value) return null; // SetSetting already uses null to restore a default.
    if (kind === "number") {
      const number = Number(value);
      if (!Number.isFinite(number)) throw new Error("not a number");
      return number;
    }
    if (kind === "json") return JSON.parse(value);
    if (kind === "list") {
      if (value.startsWith("[")) {
        const list = JSON.parse(value);
        if (!Array.isArray(list)) throw new Error("Use a JSON array.");
        return list;
      }
      return value.split(",").map(item => item.trim()).filter(Boolean);
    }
    return value;
  }

  function renderSettings() {
    const box = $("settings");
    const focused = document.activeElement;
    const focusedKey = box.contains(focused) ? focused.getAttribute("data-setting-control") : null;
    box.replaceChildren();
    // A row is [label, value] or [label, value, settingKey, kind]: the
    // latter gets an Edit control (SetSetting, validated by the daemon).
    const onTuiStart = new Set((S.settings && S.settings.on_tui_start) || []);
    const docs = (S.settings && S.settings.docs) || {};
    const help = (key) => {
      const d = key && docs[key];
      if (!d) return null;
      const def = typeof d.default === "string" ? d.default : JSON.stringify(d.default);
      const naming = (S.settings && S.settings.naming) || {};
      const notice = key === "naming.auto_download"
        ? " " + namingDownloadInfo(naming).help
        : "";
      return h("div", { class: "setting-desc muted" }, d.description + notice + (def ? " · default " + def : ""));
    };
    const section = (title, rows) => h("section", null, h("h3", null, title), h("dl", { class: "kv" },
      rows.map(([k, v, key, kind, choices]) => [h("dt", { title: key || "", "data-setting-key": key || "" }, k), h("dd", null,
        v === "" || v == null ? h("span", { class: "muted" }, "(not set)") : settingDisplay(v),
        key && onTuiStart.has(key) ? h("span", { class: "tag", title: "A running TUI keeps the old value until it restarts" }, "applies when the TUI starts") : null,
        key && kind ? h("button", { class: "btn ghost small", type: "button", "data-setting-control": key, "aria-label": "Edit " + key, onclick: (event) => {
          // A pointer click need not focus a native button on macOS. Give the
          // prompt a stable invoker to restore when it closes.
          event.currentTarget.focus({ preventScroll: true });
          editSetting(key, v, kind, choices, S.settings);
        } }, "Edit") : key ? h("span", { class: "muted setting-readonly" }, " · read-only: edit config.toml / use its CLI") : null,
        help(key))])));
    const grid = h("div", { class: "settings-grid" });
    const c = S.settings;
    if (c && !c.error) {
      const groups = new Map();
      for (const row of settingsRows(c)) {
        const prefix = ["config", "data"].includes(row.key) ? "files" : row.key.split(".")[0];
        if (!groups.has(prefix)) groups.set(prefix, []);
        const label = row.key === "security.secure" ? "secure (configured)" : row.key.includes(".") ? row.key.slice(prefix.length + 1) : row.key;
        groups.get(prefix).push([label, row.value, row.key, row.editable ? row.kind : null, row.choices]);
      }
      for (const [prefix, rows] of groups) grid.append(section(prefix === "tui" ? "Interface (tui)" : prefix, rows));
      if (!c.values) grid.append(h("p", { class: "muted" }, "Update the daemon for the complete editable Settings inventory."));
      grid.append(relayDevicesSection());
    } else if (c && c.error) {
      grid.append(h("section", null, h("h3", null, "Daemon settings"), h("p", { class: "muted" }, c.error)));
    }
    const st = S.status;
    if (st) {
      const auth = st.auth || {};
      // What the running daemon enforces, and whether it runs old code (#845).
      if (st.stale) grid.append(h("p", { class: "banner warn", role: "status" },
        "The daemon runs an older build than the one installed: restart it (formless daemon restart) to use the new one."));
      if (st.secure === false) grid.append(h("p", { class: "banner", role: "status" },
        "Approvals are off: Tools use their Capabilities without asking, and each sensitive call is logged. Start the daemon with --secure (or set security.secure = true) to be asked."));
      const sec = (c && c.security) || {};
      grid.append(section("Security", [["approvals", st.secure === true ? "on: sensitive calls ask" : st.secure === false ? "off: calls run and are logged" : "unknown (older daemon)"],
        ["trusted folders", sec.trusted_paths || 0], ["trusted hosts", sec.trusted_hosts || 0],
        ["build", st.stale ? "older than the installed binary" : "current"]]));
      grid.append(section("Daemon", [["protocol", st.protocol], ["pid", st.pid], ["sessions", st.sessions],
        ["credential", auth.credential_present ? "present" : "missing"], ["auth mode", auth.mode],
        ["token expires", auth.expires_at ? uiFormat(new Date(auth.expires_at * 1000)) : ""]]));
    }
    for (const warning of ((c && c.security && c.security.scope_warnings) || [])) grid.append(h("p", { class: "error scope-warning" }, "⚠ " + warning));
    grid.append(providersSection());
    grid.append(harnessSection());
    const mcp = S.mcpServers || [];
    grid.append(h("section", null, h("h3", null, "MCP servers"),
      mcp.length ? h("table", { class: "table" },
        h("tbody", null, mcp.map((m) => h("tr", null,
          h("td", null, h("strong", null, m.name), h("div", { class: "muted" }, m.transport + " · " + m.target),
            m.last_error ? h("div", { class: "error" }, m.last_error) : null),
          h("td", null, h("span", { class: "mcp-state " + (m.state || "").replace(" ", "-") }, m.state || (m.disabled ? "disabled" : m.running ? "ready" : "idle"))),
          h("td", { class: "num" }, m.tool_count + " tools" + (m.disabled_tools && m.disabled_tools.length ? " (" + m.disabled_tools.length + " off)" : "")),
          h("td", null,
            h("button", { class: "btn small", type: "button", onclick: () => act({ type: "restart_mcp_server", name: m.name }, "Restarted " + m.name).then(refreshSettings) }, "Restart"),
            m.state === "auth needed" ? h("button", { class: "btn small", type: "button", onclick: () => note("log in from a terminal: formless mcp login " + m.name) }, "Login") : null,
            m.transport === "stdio" ? h("button", { class: "btn small", type: "button", onclick: () => showMcpLog(m.name) }, "Log") : null,
            m.disabled ? null : h("button", { class: "btn small", type: "button", onclick: () => showMcpTools(m.name) }, "Tools"),
            h("button", { class: "btn small danger", type: "button", onclick: async () => {
              const ok = await ask({ title: "Remove MCP server " + m.name + "?", text: "Its tools are retired (metrics kept).", ok: "Remove", danger: true });
              if (ok) { await act({ type: "remove_mcp_server", name: m.name }, "Removed " + m.name); refreshSettings(); }
            } }, "Remove"))))))
        : h("p", { class: "muted" }, "None yet."),
      h("div", { class: "detail-actions" },
        h("button", { class: "btn small", type: "button", onclick: () => addMcpServer() }, "Add MCP server"),
        h("button", { class: "btn small", type: "button", onclick: () => showMcpCatalog() }, "Catalog…"),
        h("button", { class: "btn small", type: "button", onclick: () => showMcpImport() }, "Import…"))));
    grid.append(h("section", null, h("h3", null, "Storage"),
      h("p", { class: "muted" }, "Disk used by Sessions, artifacts, Tool versions, logs and backups; clean-up shows what it frees before doing it."),
      h("div", { class: "detail-actions" },
        h("button", { class: "btn small", type: "button", onclick: () => showStorage() }, "Disk use"),
        h("button", { class: "btn small", type: "button", onclick: () => showGc() }, "Clean up…"),
        h("button", { class: "btn small", type: "button", onclick: () => act({ type: "backup" }).then((r) => r && note("backed up to " + r.file + " (" + humanBytes(r.bytes) + ")")) }, "Back up now"))));
    const skills = S.skills || [];
    grid.append(h("section", null, h("h3", null, "Skills"),
      h("button", { class: "btn small", type: "button", onclick: showDescriptionReviews }, "Description reviews"),
      skills.length ? h("table", { class: "table" },
        h("tbody", null, skills.map((sk) => h("tr", null,
          h("td", null, h("strong", null, sk.name + " v" + sk.version), h("div", { class: "muted" }, sk.description), descriptionReviewCard(sk.description_review, []),
            sk.has_scripts ? h("div", { class: "muted" }, "bundles scripts (never run on the host)") : null,
            sk.author === "auto" ? h("div", { class: "muted" }, sk.retired ? "auto-created · retired (enable to restore)" : "auto-created from a successful Run") : null,
            sk.retired && sk.retire_reason ? h("div", { class: "muted" }, "Retired: " + sk.retire_reason) : null),
          h("td", { class: "num" }, sk.loads + " loads" + (sk.runs ? " · " + uiFormat(sk.runs_completed / sk.runs, { style: "percent", maximumFractionDigits: 0 }) + " of " + sk.runs + " Runs completed" : "")),
          h("td", null,
            h("button", { class: "btn small", type: "button", onclick: () => reviewSkill(sk.name) }, "Review"),
            h("button", { class: "btn small" + (sk.enabled ? "" : " primary"), type: "button", onclick: () => act({ type: "set_skill_enabled", name: sk.name, enabled: !sk.enabled }, (sk.enabled ? "Disabled " : "Enabled ") + sk.name).then(refreshSettings) }, sk.enabled ? "Disable" : "Enable"),
            h("button", { class: "btn small danger", type: "button", onclick: async () => {
              if (await ask({ title: "Remove skill " + sk.name + "?", text: "All its versions are deleted.", ok: "Remove", danger: true })) { await act({ type: "remove_skill", name: sk.name }, "Removed " + sk.name); refreshSettings(); }
            } }, "Remove"))))))
        : h("p", { class: "muted" }, "None yet."),
      h("div", { class: "detail-actions" },
        h("button", { class: "btn small", type: "button", onclick: () => addSkill() }, "Add skill"),
        h("button", { class: "btn small", type: "button", onclick: () => importSkillBundle() }, "Import bundle"))));
    const audit = S.audit || [];
    const auditFilter = h("input", { type: "search", placeholder: "Filter: tool.approve, a Tool name; -text hides; run_unasked or * shows unasked calls", value: S.auditFilter || "" });
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
    grid.append(h("section", { class: "settings-records" }, h("h3", null, "Jobs"),
      (S.settings || {}).jobs_warning ? h("p", { class: "error" }, S.settings.jobs_warning) : null,
      jobs.length ? h("div", { class: "table-wrap" }, h("table", { class: "table" },
        h("tbody", null, jobs.map((j) => h("tr", null,
          h("td", { class: "record-prose" }, h("div", null, j.prompt),
            h("div", { class: "muted" }, (j.cron ? describeCron(j.cron) : "once") + " · next " + (j.next_run ? uiFormat(new Date(j.next_run)) : "-") + (j.last_status ? " · last: " + j.last_status : "") + " · " + j.author)),
          h("td", null,
            h("button", { class: "btn small" + (j.enabled && j.approved ? "" : " primary"), type: "button", onclick: () => act({ type: "set_job_enabled", id: j.id, enabled: !(j.enabled && j.approved) }, "Updated").then(refreshSettings) },
              !j.approved ? "Approve" : j.enabled ? "Disable" : "Enable"),
            h("button", { class: "btn small", type: "button", onclick: () => act({ type: "run_job_now", id: j.id }, "Started").then(refreshSettings) }, "Run now"),
            h("button", { class: "btn small", type: "button", onclick: () => showJobHistory(j) }, "History"),
            h("button", { class: "btn small danger", type: "button", onclick: async () => {
              if (await ask({ title: "Delete this job?", text: j.prompt, ok: "Delete", danger: true })) { await act({ type: "delete_job", id: j.id }, "Deleted"); refreshSettings(); }
            } }, "Delete")))))))
        : h("p", { class: "muted" }, "None yet. The agent can also schedule_run."),
      h("div", { class: "detail-actions" }, h("button", { class: "btn small", type: "button", onclick: () => newJob() }, "New job"))));
    const memories = S.memories || [];
    grid.append(h("section", { class: "settings-records" }, h("h3", null, "Memory"),
      h("p", { class: "muted" }, "Notes the agent keeps about your preferences; they go into new sessions' prompts."),
      memories.length ? h("div", { class: "table-wrap" }, h("table", { class: "table" },
        h("tbody", null, memories.map((m) => h("tr", null,
          h("td", { class: "record-prose" }, h("div", { class: m.enabled ? "" : "muted" }, m.text),
            h("div", { class: "muted" }, m.scope + " · " + (m.session_id ? "from session " + m.session_id.slice(0, 8) : m.author) + (m.enabled ? "" : " · off"))),
          h("td", null,
            h("button", { class: "btn small", type: "button", onclick: async () => {
              await editMemory(m);
            } }, "Edit"),
            h("button", { class: "btn small", type: "button", onclick: () => act({ type: "upsert_memory", id: m.id, enabled: !m.enabled }, m.enabled ? "Disabled" : "Enabled").then(refreshSettings) }, m.enabled ? "Disable" : "Enable"),
            h("button", { class: "btn small danger", type: "button", onclick: async () => {
              if (await ask({ title: "Delete this memory?", text: m.text, ok: "Delete", danger: true })) { await act({ type: "delete_memory", id: m.id }, "Deleted"); refreshSettings(); }
            } }, "Delete")))))))
        : h("p", { class: "muted" }, "None yet."),
      h("div", { class: "detail-actions" },
        h("button", { class: "btn small", type: "button", onclick: () => editMemory(null) }, "Add"))));
    const thinking = h("input", { type: "checkbox", onchange: (e) => setThinking(e.target.checked) });
    thinking.checked = S.showThinking;
    const enterSends = h("input", { id: "enter-sends", type: "checkbox", onchange: e => setEnterSends(e.target.checked) });
    enterSends.checked = composerEnterSends();
    grid.append(h("section", null, h("h3", null, "This browser"),
      h("dl", { class: "kv" },
        h("dt", null, "thinking"), h("dd", null, h("label", { class: "toggle" }, thinking, "show live reasoning")),
        h("dt", null, "Enter sends"), h("dd", null, h("label", { class: "toggle" }, enterSends, "Enter sends"),
          h("div", { class: "muted" }, "Shift+Enter inserts a newline; Ctrl/⌘+Enter queues. This preference is saved on this device."),
          h("button", { class: "btn ghost small", type: "button", onclick: () => setEnterSends(null) }, "Use device default")),
        h("dt", null, "text size"), h("dd", null, h("span", { class: "read-controls" },
          h("button", { class: "btn small", type: "button", "aria-label": "Smaller text", onclick: () => setReading(S.readScale - 0.1, null) }, "A−"),
          h("span", { "aria-live": "polite" }, uiFormat(S.readScale, { style: "percent", maximumFractionDigits: 0 })),
          h("button", { class: "btn small", type: "button", "aria-label": "Larger text", onclick: () => setReading(S.readScale + 0.1, null) }, "A+"))),
        h("dt", null, "density"), h("dd", null, h("span", { class: "read-controls" },
          ["comfortable", "compact"].map((d) => h("button", { class: "btn small" + (S.density === d ? " primary" : ""), type: "button", "aria-pressed": String(S.density === d), onclick: () => setReading(null, d) }, d)))),
        h("dt", null, "encryption"), h("dd", null, "AES-256-GCM, key held only in this tab"),
        S.local ? null : [h("dt", null, "notifications"), h("dd", null, pushControl())]),
      h("div", { class: "detail-actions" },
        h("button", { class: "btn danger small", type: "button", onclick: forget }, "Disconnect and forget this device"))));
    box.append(grid, h("p", { class: "muted" }, "Edits are validated and applied by the daemon; rows marked for TUI startup need a TUI restart. Credentials, endpoint and relay configuration use config.toml or their CLI. Manage MCP servers above."));
    filterSettings();
    // Restore only a control removed by this synchronous redraw. A user who
    // moved elsewhere while an asynchronous save was pending keeps that focus.
    if (focusedKey && (document.activeElement === document.body || document.activeElement === focused)) {
      const replacement = [...box.querySelectorAll("[data-setting-control]")].find(node => node.getAttribute("data-setting-control") === focusedKey);
      const target = focusVisible(replacement) ? replacement : $("settings-filter");
      if (focusVisible(target)) target.focus({ preventScroll: true });
    }
  }
  /** The Settings filter: rows whose label, key, value or description
   * contain the words stay (matches marked); sections with none left hide,
   * unless their title matches. Every word must be in one target (label,
   * value, key or table row), folded by `foldMap` as `markWords` folds it,
   * so a visible label shows its highlight (#1731). */
  function filterSettings() {
    const words = $("settings-filter").value.split(/\s+/).filter(Boolean);
    const keys = words.map((w) => foldMap(w).folded).filter(Boolean);
    // Each target's text is folded once per call, not once per word.
    const hitText = (text) => { const t = foldMap(text).folded; return keys.every((k) => t.includes(k)); };
    const hit = (el) => hitText(el.textContent);
    // Unwrap the last marks, then merge the split text back together so the
    // next query can match across the old mark's edges (#1731).
    const parents = new Set();
    for (const m of $("settings").querySelectorAll("mark.find")) { parents.add(m.parentNode); m.replaceWith(m.textContent); }
    for (const p of parents) p.normalize();
    const sections = $("settings").querySelectorAll("section");
    let visibleSections = 0;
    for (const sec of sections) {
      const titleHit = keys.length && hit(sec.querySelector("h3") || sec);
      let shown = 0;
      for (const dt of sec.querySelectorAll("dl.kv > dt")) {
        const dd = dt.nextElementSibling;
        const ok = !keys.length || titleHit || hit(dt) || (dd && hit(dd)) || (dt.title && hitText(dt.title));
        dt.classList.toggle("hidden", !ok);
        if (dd) dd.classList.toggle("hidden", !ok);
        if (ok) { shown++; if (keys.length) markWords(dt, words); }
      }
      for (const tr of sec.querySelectorAll("tbody > tr")) {
        const ok = !keys.length || titleHit || hit(tr);
        tr.classList.toggle("hidden", !ok);
        if (ok) shown++;
      }
      const hasRows = sec.querySelector("dl.kv > dt, tbody > tr");
      const hidden = !!keys.length && !titleHit && (hasRows ? !shown : !hit(sec));
      sec.classList.toggle("hidden", hidden);
      if (!hidden) visibleSections++;
    }
    const noMatch = keys.length > 0 && sections.length > 0 && visibleSections === 0;
    const status = $("settings-filter-status");
    const message = noMatch ? "No Settings match the current filter." : "";
    if (status.textContent !== message) status.textContent = message;
    const clear = $("settings-filter-clear");
    if (!noMatch && document.activeElement === clear) $("settings-filter").focus({ preventScroll: true });
    clear.hidden = !noMatch;
  }
  /** `text` folded for matching with, per folded UTF-16 unit, the offset
   * of the original character it came from (#789): decomposed, marks
   * dropped, lower-cased one character at a time, so `İ` stays one
   * letter and NFC/NFD spellings match each other. The default is the
   * Settings fold. Session search supplies its own tokenizer fold. */
  function foldMap(text, foldCharacter = (ch) => ch.normalize("NFD").replace(/\p{M}/gu, "").toLowerCase().slice(0, 2)) {
    let folded = "";
    const from = [];
    let at = 0;
    for (const ch of text) {
      const f = foldCharacter(ch);
      for (let k = 0; k < f.length; k++) from.push(at);
      folded += f;
      at += ch.length;
    }
    from.push(at);
    return { folded, from };
  }
  /** Wrap the words' first occurrences in an element's own text in <mark>,
   * adding no empty text nodes. */
  function markWords(el, words) {
    const keys = words.map((w) => foldMap(w).folded).filter((w) => w);
    for (const node of [...el.childNodes].filter((n) => n.nodeType === 3)) {
      const text = node.textContent;
      const { folded, from } = foldMap(text);
      // Offsets in the original text: where the match's first folded unit
      // came from, up to where the one after its last starts.
      const spans = keys
        .map((w) => [folded.indexOf(w), w.length])
        .filter(([i]) => i >= 0)
        .map(([i, n]) => [from[i], from[i + n] - from[i]])
        .filter(([, n]) => n > 0)
        .sort((a, b) => a[0] - b[0]);
      if (!spans.length) continue;
      const parts = [];
      let at = 0;
      for (const [i, n] of spans) {
        if (i < at) continue;
        if (i > at) parts.push(text.slice(at, i));
        parts.push(h("mark", { class: "find" }, text.slice(i, i + n)));
        at = i + n;
      }
      if (at < text.length) parts.push(text.slice(at));
      node.replaceWith(...parts);
    }
  }
  $("settings-filter").addEventListener("input", filterSettings);
  $("settings-filter-clear").addEventListener("click", () => {
    const input = $("settings-filter");
    input.value = "";
    input.focus({ preventScroll: true });
    filterSettings();
  });

  /** Web Push (SPEC §12): opt in per device. The relay keeps this
   * browser's subscription and, when an approval or question waits, sends
   * a push with no content; nothing about the Session leaves the daemon. */
  function pushControl() {
    const box = h("span");
    const supported = "serviceWorker" in navigator && "PushManager" in window && "Notification" in window;
    if (!supported) return h("span", { class: "muted" }, "not supported in this browser");
    navigator.serviceWorker.getRegistration().then(async (reg) => {
      const sub = reg && (await reg.pushManager.getSubscription());
      const refused = sub && S.pushRefusal?.room === S.room && S.pushRefusal?.endpoint === sub.endpoint;
      box.replaceChildren(sub
        ? h("span", null, refused ? S.pushRefusal.message + " " : "on · ",
            refused ? h("button", { class: "btn small", type: "button", onclick: () => pushOn(sub) }, "Retry") : null,
            h("button", { class: "btn ghost small", type: "button", onclick: () => pushOff(sub) }, "Turn off"))
        : h("button", { class: "btn small", type: "button", onclick: () => pushOn() }, "Notify me about approvals"));
    });
    return box;
  }
  function b64uToBytes(s) {
    const b = atob(s.replace(/-/g, "+").replace(/_/g, "/") + "===".slice((s.length + 3) % 4));
    return Uint8Array.from(b, (c) => c.charCodeAt(0));
  }
  async function pushOn(existing) {
    if (S.forgetting || S.pushRemoval || S.pushEnabling) return;
    S.pushEnabling = true;
    try {
      if (existing) {
        registerRelayPush(S.ws, existing, true);
        return;
      }
      const r = await fetch("push/key");
      if (!r.ok) return toast("This relay hasn't set up notifications.", true);
      const { key } = await r.json();
      if ((await Notification.requestPermission()) !== "granted") return toast("Notifications are blocked for this site.", true);
      const reg = await navigator.serviceWorker.ready;
      if (S.forgetting || S.pushRemoval) return;
      S.pushSubscribe = reg.pushManager.subscribe({ userVisibleOnly: true, applicationServerKey: b64uToBytes(key) });
      const sub = await S.pushSubscribe;
      if (S.forgetting || S.pushRemoval) return;
      registerRelayPush(S.ws, sub, true);
    } catch (e) {
      toast("Couldn't turn on notifications: " + e.message, true);
    } finally {
      S.pushSubscribe = null;
      S.pushEnabling = false;
      if (!S.forgetting) renderView();
    }
  }
  /** Re-register only an existing opt-in after an authenticated connection.
   * Room-v3 migration never copies subscriptions from an unverified old room. */
  async function restorePushSubscription(ws) {
    const room = S.room;
    if (!("serviceWorker" in navigator)) return;
    try {
      const reg = await navigator.serviceWorker.getRegistration();
      const sub = reg && await reg.pushManager.getSubscription();
      if (sub && S.pushRefusal?.room === room && S.pushRefusal?.endpoint === sub.endpoint) return;
      if (sub && !S.forgetting && !S.pushRemoval && S.ws === ws && S.room === room && S.channel && S.daemonOnline && ws.readyState === 1) {
        registerRelayPush(ws, sub);
      }
    } catch (_) { /* No permission request or automatic new subscription. */ }
  }
  const PUSH_CLEANUP_MS = 1500;
  function registerRelayPush(ws, sub, interactive = false) {
    if (ws?.readyState !== 1) throw new Error("not connected to the relay");
    if (S.pushRegistration?.socket === ws) return;
    const room = S.room;
    const wire = sub.toJSON();
    let resolve;
    const settled = new Promise((done) => { resolve = done; });
    const finish = (socket, outcome) => {
      if (socket !== ws) return;
      ws.removeEventListener("close", closed);
      if (S.pushRegistration === pending) {
        S.pushRegistration = null;
        if (outcome && S.ws === ws && S.room === room) {
          if (outcome === "accepted") {
            if (S.pushRefusal?.room === room && S.pushRefusal?.endpoint === wire.endpoint) S.pushRefusal = null;
            if (interactive && !S.forgetting) note("This device gets a notification when an approval or question waits.");
          } else {
            S.pushRefusal = { room, endpoint: wire.endpoint, message: outcome };
            if (!S.forgetting) toast(outcome, true);
          }
          if (!S.forgetting && S.view === "settings") renderView();
        }
      }
      resolve();
    };
    const closed = () => finish(ws);
    const pending = { socket: ws, settled, finish };
    S.pushRegistration = pending;
    ws.addEventListener("close", closed, { once: true });
    try { ws.send(JSON.stringify({ relay: "push_subscribe", sub: wire })); }
    catch (error) { finish(ws); throw error; }
  }
  function pushRefused(ws, value) {
    if (value.operation === "unsubscribe") {
      S.pushRemovalAck?.(ws, value.id, false);
      return;
    }
    if (value.operation !== "subscribe") return;
    const message = value.reason === "capacity" ? "The relay has reached its notification device limit."
      : value.reason === "busy" ? "The relay is busy. Retry notification setup."
      : "The relay refused this browser's notification subscription.";
    S.pushRegistration?.finish(ws, message);
  }
  async function boundedPushCleanup(promise) {
    let timer;
    try {
      return await Promise.race([promise, new Promise((_, reject) => {
        timer = setTimeout(() => reject(new Error("Notification cleanup timed out")), PUSH_CLEANUP_MS);
      })]);
    } finally { clearTimeout(timer); }
  }
  async function removeRelayPush(endpoint) {
    const ws = S.ws;
    if (!S.relayReady || ws?.readyState !== 1) return false;
    // A registration already sent by this socket can still be writing. The
    // relay permits only one push write per socket at a time.
    const registration = S.pushRegistration;
    if (registration?.socket === ws) await boundedPushCleanup(registration.settled);
    if (S.ws !== ws || !S.relayReady || ws.readyState !== 1) return false;
    return new Promise((resolve) => {
      const id = crypto.randomUUID();
      const done = (confirmed) => {
        clearTimeout(timer);
        ws.removeEventListener("close", closed);
        S.pushRemovalAck = null;
        resolve(confirmed);
      };
      const closed = () => done(false);
      const timer = setTimeout(closed, PUSH_CLEANUP_MS);
      S.pushRemovalAck = (socket, ack, confirmed = true) => { if (ws === socket && ack === id) done(confirmed); };
      ws.addEventListener("close", closed, { once: true });
      try { ws.send(JSON.stringify({ relay: "push_unsubscribe", endpoint, id })); }
      catch (_) { closed(); }
    });
  }
  async function browserPushSubscription() {
    if (!("serviceWorker" in navigator)) return null;
    const reg = await navigator.serviceWorker.getRegistration();
    return reg?.pushManager ? reg.pushManager.getSubscription() : null;
  }
  function removePushSubscription(knownSub) {
    if (S.pushRemoval) return S.pushRemoval;
    S.pushRemoval = (async () => {
      let sub = knownSub;
      try {
        if (!sub) {
          if (S.pushSubscribe) sub = await boundedPushCleanup(S.pushSubscribe.catch(() => null));
          sub ||= await boundedPushCleanup(browserPushSubscription());
        }
      } catch (_) {
        return "Could not check this browser's notification subscription or confirm its removal.";
      }
      if (!sub) return "";
      const [remote, local] = await Promise.all([
        removeRelayPush(sub.endpoint).catch(() => false),
        boundedPushCleanup((async () => {
          if (await sub.unsubscribe()) return true;
          return (await browserPushSubscription())?.endpoint !== sub.endpoint;
        })()).catch(() => false),
      ]);
      return [
        !local && "Could not confirm that notifications are off in this browser. Check this site's notification permissions.",
        !remote && "The relay did not confirm removal of the notification subscription.",
      ].filter(Boolean).join(" ");
    })().finally(() => { S.pushRemoval = null; });
    return S.pushRemoval;
  }
  async function pushOff(sub) {
    const warning = await removePushSubscription(sub);
    if (!S.forgetting) {
      if (warning) { toast(warning, true); note(warning, true); }
      else note("Notifications are off on this device.");
      renderView();
    }
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
    await refreshDescriptionReviews();
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
    await ask({ title: r.skill.name + " v" + r.skill.version, text: r.body + "\n\nFiles: " + r.files.join(", "), body: descriptionReviewCard(r.skill.description_review, r.skill.description_reviews), ok: "Close" });
  }
  function namingDownloadInfo(naming) {
    const model = typeof naming?.model === "string" ? naming.model.trim() : "";
    const size = typeof naming?.model_size === "string" ? naming.model_size.trim() : "";
    const supported = Boolean(model && size && size !== "—");
    if (!supported) {
      const notice = model
        ? "Configured naming model " + model + " is not supported; titles use a heuristic. Automatic model downloads are unavailable."
        : "No supported naming model is configured; titles use a heuristic. Automatic model downloads are unavailable.";
      return { supported, help: notice, consent: notice };
    }
    return {
      supported,
      help: "First-use download: " + model + " (about " + size + "). Without it, titles use a heuristic.",
      consent: "On first use, formless will download " + model + " (about " + size + "). Without it, Session titles use a heuristic.",
    };
  }

  async function editSetting(key, current, kind, choices = [], settings = null) {
    let value;
    if (kind === "bool") value = !(current === "on" || current === true);
    else if (kind === "choice") {
      const select = h("select", { id: "setting-choice" },
        h("option", { value: "" }, "default"), choices.map(choice => h("option", { value: choice }, choice)));
      select.value = current == null ? "" : String(current);
      const body = h("label", null, "Value ", select);
      if (!await ask({ title: "Set " + key, body, ok: "Save" })) return;
      value = select.value || null;
    } else {
      const initial = current == null ? "" : kind === "json" || kind === "list" ? JSON.stringify(current) : String(current);
      let draft = initial, error = "";
      while (true) {
        const text = await ask({ title: "Set " + key, inputKind: "machine", input: true, value: draft, ok: "Save",
          text: "Leave empty for the default. " + (kind === "list" ? "Comma-separated strings or a JSON array (numbers stay numeric)." : kind === "json" ? "JSON value (quote strings; maps and numeric lists use JSON)." : ""),
          body: error ? h("p", { class: "error", role: "alert" }, error) : null });
        if (text == null) return;
        try { value = parseSettingInput(text, kind); break; }
        catch (cause) { draft = text; error = cause.message || "Invalid value"; }
      }
    }
    if (key === "naming.auto_download" && value === true) {
      const naming = (settings && settings.naming) || {};
      const download = namingDownloadInfo(naming);
      if (!download.supported) {
        await ask({
          title: "No supported session-naming model",
          text: download.consent,
          ok: "Close",
        });
        return;
      }
      const accepted = await ask({
        title: "Allow session-naming model download?",
        text: download.consent,
        ok: "Allow download",
      });
      if (!accepted) return;
    }
    const r = await act({ type: "set_setting", key, value });
    if (!r) return;
    if (r.applies === "on_tui_start") note("Saved " + key + ". It applies when the TUI next starts; restart any running TUI.");
    else note("Saved " + key + " (in effect now).");
    refreshSettings();
  }
  let auditRequest = null, auditOwner = null;
  function ownsAudit(scope, filter) {
    return scope?.ws === S.ws && scope?.channel === S.channel && scope?.filter === filter;
  }
  async function loadAudit(older, background = false) {
    const filter = S.auditFilter || null;
    const loaded = ownsAudit(auditOwner, filter);
    const explicitPending = ownsAudit(auditRequest, filter) && !auditRequest.background;
    // A background refresh may replace another background read, but must
    // not discard an explicit filter or pagination response in flight.
    if (background && (explicitPending || (loaded && S.auditPages > 1))) return;
    if (older && (!loaded || explicitPending || !S.audit?.length)) return;
    const stamp = auditRequest = { ws: S.ws, channel: S.channel, filter, background };
    const last = older ? S.audit[S.audit.length - 1] : null;
    const before = last ? (last.cursor || last.created_at) : null;
    try {
      const page = await request({ type: "list_audit", filter, limit: 50, before }).catch(() => null);
      if (stamp !== auditRequest || !ownsAudit(stamp, S.auditFilter || null)) return;
      // A failed Older request did not advance the cursor. Preserve its page
      // count, so Older remains available and retry sends the same cursor.
      if (older && !Array.isArray(page)) return;
      S.audit = older ? S.audit.concat(page) : (page || []);
      S.auditPages = older ? (S.auditPages || 1) + 1 : 1;
      auditOwner = stamp;
      renderView();
    } finally {
      if (auditRequest === stamp) auditRequest = null;
    }
  }
  /** Settings → Providers & Roles (SPEC §4.5): list, add or edit (keys
   * are write-only), test a connection, and edit Roles. */
  function providersSection() {
    const { providers = [], roles = {} } = S.providers || {};
    const names = providers.map((p) => p.name);
    return h("section", null, h("h3", null, "Providers & Roles"),
      h("table", { class: "table" }, h("tbody", null, providers.map((p) => h("tr", null,
        h("td", null, h("strong", null, p.name), h("div", { class: "muted" }, p.base_url || "(not set)")),
        h("td", null, p.model || h("span", { class: "muted" }, "no default model")),
        h("td", null, p.has_key ? "key stored" : h("span", { class: "muted" }, "no key")),
        h("td", null,
          h("button", { class: "btn small", type: "button", onclick: () => testProvider(p.name) }, "Test"),
          p.name === "default" ? null : h("button", { class: "btn small", type: "button", onclick: () => editProvider(p) }, "Edit"),
          p.name === "default" ? null : h("button", { class: "btn small danger", type: "button", onclick: async () => {
            if (await ask({ title: "Remove provider " + p.name + "?", text: "Refused while a Role uses it.", ok: "Remove", danger: true })) {
              if (await act({ type: "remove_provider", name: p.name }, "Removed " + p.name)) refreshSettings();
            }
          } }, "Remove")))))),
      h("p", { class: "muted" }, "The default provider is set up in the wizard (formless setup) or config.toml."),
      h("div", { class: "detail-actions" }, h("button", { class: "btn small", type: "button", onclick: () => editProvider(null) }, "Add provider")),
      h("h3", null, "Roles"),
      Object.keys(roles).length ? h("table", { class: "table" }, h("tbody", null, Object.entries(roles).map(([name, r]) => h("tr", null,
        h("td", null, h("strong", null, name), h("div", { class: "muted" }, r.description || "")),
        h("td", null, (r.provider || "default") + " · " + (r.model || "provider default") + (r.reasoning_effort ? " · " + r.reasoning_effort : "")),
        h("td", null,
          h("button", { class: "btn small", type: "button", onclick: () => editRole(name, r, names) }, "Edit"),
          h("button", { class: "btn small danger", type: "button", onclick: async () => {
            if (await ask({ title: "Delete role " + name + "?", ok: "Delete", danger: true })) {
              if (await act({ type: "delete_role", name }, "Deleted " + name)) refreshSettings();
            }
          } }, "Delete"))))))
        : h("p", { class: "muted" }, "None. A Role maps a purpose (\"fast search\") to a provider and model for sub-agents."),
      h("div", { class: "detail-actions" }, h("button", { class: "btn small", type: "button", onclick: () => editRole("", {}, names) }, "Add role")));
  }
  /** Settings → Relay devices (SPEC §12): browsers on the relay now, each
   * with Disconnect. A disconnected browser can rejoin with the link;
   * rotating the secret is what revokes access. */
  function relayDevicesSection() {
    const list = S.relayClients || [];
    return h("section", null, h("h3", null, "Relay devices"),
      list.length ? h("table", { class: "table" }, h("tbody", null, list.map((c) => h("tr", null,
        h("td", null, h("strong", null, browserName(c.user_agent)), h("div", { class: "muted" }, c.user_agent || "unknown browser")),
        h("td", { class: "muted" }, "since " + relTime(c.first_seen) + " · active " + relTime(c.last_seen)),
        h("td", null, h("button", { class: "btn small danger", type: "button", onclick: async () => {
          if (!(await ask({ title: "Disconnect this browser?", text: "It can reconnect with the pairing link. To revoke access for good, run formless relay enable --rotate.", ok: "Disconnect", danger: true }))) return;
          const result = await act({ type: "disconnect_relay_client", cid: c.cid });
          if (!result) return;
          if (result.disconnected === true) toast("Disconnected");
          else if (result.disconnected === false) toast("That browser is already disconnected.");
          else toast("The daemon did not confirm disconnection.", true);
          refreshSettings();
        } }, "Disconnect"))))))
        : h("p", { class: "muted" }, "No browsers on the relay right now."));
  }
  /** "Safari on iPhone" from a User-Agent, roughly. */
  function browserName(ua) {
    ua = ua || "";
    const app = /Edg\//.test(ua) ? "Edge" : /Firefox\//.test(ua) ? "Firefox" : /Chrome\//.test(ua) ? "Chrome" : /Safari\//.test(ua) ? "Safari" : "Browser";
    const os = /iPhone/.test(ua) ? "iPhone" : /iPad/.test(ua) ? "iPad" : /Android/.test(ua) ? "Android" : /Mac OS X/.test(ua) ? "macOS" : /Windows/.test(ua) ? "Windows" : /Linux/.test(ua) ? "Linux" : "";
    return os ? app + " on " + os : app;
  }
  /** Settings → Harness changes (SPEC §7.13): what the agent changed in
   * its own setup, when and why, each with Undo. */
  function harnessSection() {
    const changes = S.harness || [];
    const show = (v) => (v == null ? "(none)" : typeof v === "string" ? v : JSON.stringify(v, null, 2));
    return h("section", null, h("h3", null, "Harness changes"),
      changes.length ? h("table", { class: "table" }, h("tbody", null, changes.map((c) => h("tr", { class: c.undone ? "muted" : null },
        h("td", { class: "muted" }, relTime(c.created_at)),
        h("td", null,
          h("strong", null, c.op + " " + c.target), c.undone ? h("span", { class: "tag" }, "undone") : null,
          h("div", { class: "muted" }, (c.reason || "no reason given") + (c.tool ? " · via " + c.tool : "")),
          h("details", { class: "chip" }, h("summary", null, "Before / after"),
            h("div", { class: "muted" }, "Before"), h("pre", null, show(c.before)),
            h("div", { class: "muted" }, "After"), h("pre", null, show(c.after)))),
        h("td", null, c.undone ? null : h("button", { class: "btn small", type: "button", onclick: async () => {
          if (!(await ask({ title: "Undo " + c.op + " " + c.target + "?", text: "Puts back what it replaced.", ok: "Undo" }))) return;
          if (await act({ type: "undo_harness_change", id: c.id }, "Undone")) refreshSettings();
        } }, "Undo"))))))
        : h("p", { class: "muted" }, "The agent hasn't changed its own setup (MCP servers, skills, settings) yet."));
  }
  /** Split a command line into words, keeping "quoted parts" together. */
  function splitArgs(line) {
    return [...line.matchAll(/"([^"]*)"|'([^']*)'|(\S+)/g)].map((m) => m[1] ?? m[2] ?? m[3]);
  }
  /** `KEY=value` / `Key: value` lines to an object. */
  function pairs(text, sep) {
    const out = {};
    for (const line of text.split("\n")) {
      const i = line.indexOf(sep);
      if (i > 0) out[line.slice(0, i).trim()] = line.slice(i + 1).trim();
    }
    return out;
  }
  function humanBytes(b) {
    const u = ["B", "KB", "MB", "GB", "TB"];
    let v = b || 0, i = 0;
    while (v >= 1000 && i < u.length - 1) { v /= 1000; i++; }
    return i ? uiFixed(v, 1) + " " + u[i] : uiFormat(v) + " B";
  }
  function kindsTable(r, gc) {
    return h("table", { class: "table" }, h("tbody", null, (r.kinds || []).map((k) => h("tr", { class: k.inside_db ? "muted" : "" },
      h("td", null, (k.inside_db ? "↳ " : "") + k.kind), gc ? h("td", { class: "num" }, String(k.items)) : null,
      h("td", { class: "num" }, humanBytes(k.bytes)), h("td", { class: "muted" }, k.detail)))));
  }
  // Disk use by kind (SPEC §9.6).
  async function showStorage() {
    const trigger = document.activeElement;
    const r = await act({ type: "storage_usage" });
    if (r) modal("Disk use", h("div", null, kindsTable(r, false), h("p", { class: "muted" }, "Total " + humanBytes(r.total_bytes) + " in " + r.data_dir + (r.last_backup ? " · last backup " + r.last_backup : ""))), { trigger });
  }
  // Clean-up: the dry run first, then free it on request.
  async function showGc() {
    const trigger = document.activeElement;
    const r = await act({ type: "gc", dry_run: true });
    if (!r) return;
    let close = null;
    const run = h("button", { class: "btn small danger", type: "button", onclick: async () => {
      const done = await act({ type: "gc", dry_run: false }, null, 300000);
      if (!done) return;
      if (close) close();
      note("freed about " + humanBytes(done.total_bytes));
    } }, "Free " + humanBytes(r.total_bytes));
    close = modal("Clean up", h("div", null, kindsTable(r, true), h("div", { class: "detail-actions" }, run)), { trigger });
  }
  function daemonUptime(seconds) {
    return seconds >= 3600
      ? Math.floor(seconds / 3600) + "h " + Math.floor(seconds % 3600 / 60) + "m"
      : Math.floor(seconds / 60) + "m " + Math.floor(seconds % 60) + "s";
  }
  // The daemon and what it is using (SPEC §9.3).
  async function showStatus() {
    const trigger = document.activeElement;
    const v = await act({ type: "status" });
    if (!v) return;
    const r = v.resources || {};
    const rows = [
      ["daemon", "pid " + v.pid + " · " + v.version],
      ["uptime", r.uptime_secs != null ? daemonUptime(r.uptime_secs) : "?"],
      ["memory", r.memory_bytes != null ? humanBytes(r.memory_bytes) + " · cpu " + (r.cpu_percent != null ? uiFixed(r.cpu_percent, 1) : "?") + "%" : "n/a"],
      ["database", humanBytes(r.db_bytes) + " · artifacts " + humanBytes(r.artifacts_bytes)],
      ["work", v.sessions + " Sessions · " + (v.running_sessions || []).length + " running Runs · " + r.running_invocations + " pending Invocations"],
    ];
    modal("Daemon", h("table", { class: "table" }, h("tbody", null, rows.map(([k, val]) => h("tr", null, h("td", null, k), h("td", null, val))))), { trigger });
  }

  // A modal with a title, body and Close (Esc closes it).
  function modal(title, body, options = {}) {
    const close = h("button", { class: "btn small primary", type: "button" }, "Close");
    const dialog = h("div", { class: "backdrop", role: "dialog", "aria-modal": "true", "aria-label": title },
      h("div", { class: "modal help" }, h("h2", null, title), h("div", { class: "files-body" }, body),
        h("div", { class: "modal-actions" }, h("span", { class: "spacer" }), close)));
    const done = () => closeDialog(dialog);
    close.onclick = done;
    openDialog(dialog, { ...options, initialFocus: close, onDismiss: done });
    return done;
  }
  // An MCP server's stderr tail (SPEC §7.9).
  async function showMcpLog(name) {
    const trigger = document.activeElement;
    const r = await act({ type: "mcp_server_log", name, lines: 200 });
    if (r) modal("MCP " + name + " log", h("pre", { class: "diff" }, r.lines.length ? r.lines.join("\n") : "(no output yet)"), { trigger });
  }
  // Turn an MCP server's tools on or off, with each one's token cost.
  async function showMcpTools(name) {
    const trigger = document.activeElement;
    const r = await act({ type: "list_mcp_tools", name }, null, 60000);
    if (!r) return;
    const total = h("p", { class: "muted" }, "Enabled tools cost ~" + r.enabled_tokens + " tokens per request.");
    const rows = r.tools.map((t) => {
      const box = h("input", { type: "checkbox", "aria-label": "Enable " + t.name });
      box.checked = t.enabled;
      box.onchange = async () => {
        if (!(await act({ type: "set_mcp_tool_enabled", name, tool: t.name, enabled: box.checked }))) box.checked = !box.checked;
        else refreshSettings();
      };
      return h("tr", null, h("td", null, box), h("td", null, h("code", null, t.name), h("div", { class: "muted" }, t.description || ""), descriptionReviewCard(t.description_review, t.description_reviews)), h("td", { class: "num" }, "~" + t.tokens));
    });
    modal("MCP " + name + " tools", h("div", null, total, h("table", { class: "table" }, h("tbody", null, rows))), { trigger });
  }
  // The curated catalog of popular servers (SPEC §7.9).
  async function showMcpCatalog() {
    const trigger = document.activeElement;
    const list = await act({ type: "mcp_catalog" });
    if (!list) return;
    let close = null;
    const rows = list.map((e) => h("tr", null,
      h("td", null, h("strong", null, e.title), h("div", { class: "muted" }, e.description), h("div", { class: "muted" }, h("code", null, e.target)), h("div", { class: "muted" }, e.execution_warning || "")),
      h("td", null, e.added ? h("span", { class: "muted" }, "added") : h("button", { class: "btn small", type: "button", onclick: async () => {
        const params = {};
        for (const p of e.params) {
          const v = await ask({ title: e.title + ": " + p.name, inputKind: p.kind === "secret" ? "secret" : "machine", text: p.description, input: true, secret: p.kind === "secret" });
          if (v == null || !v.trim()) return;
          params[p.name] = v.trim();
        }
        const preview = await act({ type: "preview_mcp_catalog", id: e.id, params });
        if (!preview) return;
        if (!(await ask({ title: "Review MCP command", text: preview.target + "\n" + (preview.execution_warning || "Connects to this URL."), ok: "Add and run" }))) return;
        params.confirm_run = true;
        for (const warning of preview.warnings || []) {
          if (!(await ask({ title: "Confirm folder exposure", text: warning, danger: true, ok: "Expose this folder" }))) return;
        }
        params.confirm_exposure = true;
        const r = await act({ type: "add_mcp_from_catalog", id: e.id, params }, null, 120000);
        if (!r) return;
        note("added " + r.added + (r.error ? " — failed to start: " + r.error : " · " + r.tools + " tools"), !!r.error);
        (r.env_keys || []).forEach((k) => note(r.added + " needs " + k + " in the daemon's environment"));
        if (r.needs_login) note("log in from a terminal: formless mcp login " + r.added);
        if (close) close();
        refreshSettings();
      } }, "Add"))));
    close = modal("MCP server catalog", h("table", { class: "table" }, h("tbody", null, rows)), { trigger });
  }
  // Servers configured in Claude Desktop, Cursor or VS Code (SPEC §7.9).
  async function showMcpImport() {
    const trigger = document.activeElement;
    const list = await act({ type: "mcp_import_candidates", cwd: null });
    if (!list) return;
    if (!list.length) return note("no MCP servers found in Claude Desktop, Cursor or VS Code configs");
    const rows = list.map((c) => h("tr", null,
      h("td", null, h("strong", null, c.name), h("div", { class: "muted" }, "from " + c.source), h("div", null, h("code", null, c.target)),
        c.missing_env.length ? h("div", { class: "error" }, "not imported (placeholders): " + c.missing_env.join(", ")) : null),
      h("td", null, c.exists ? h("span", { class: "muted" }, "added") : h("button", { class: "btn small", type: "button", onclick: async (ev) => {
        const skip = h("input", { type: "checkbox", checked: true });
        const body = h("div", null,
          h("p", null, "Keys with values: env " + (c.env || []).join(", ") + "; headers " + (c.headers || []).join(", ") + ". Copying values stores them in plaintext config.toml."),
          h("label", null, skip, " Skip secrets; I'll set env/header values myself"));
        if (!(await ask({ title: "Import " + c.name, text: c.target + "\nStarts disabled and untrusted; review before enabling.", body, ok: "Import disabled" }))) return;
        const r = await act({ type: "import_mcp_candidate", candidate_id: c.candidate_id, skip_secrets: skip.checked }, null, 120000);
        if (!r) return;
        ev.target.replaceWith(h("span", { class: "muted" }, "imported disabled and untrusted"));
        refreshSettings();
      } }, "Add"))));
    modal("Import MCP servers", h("div", null, h("p", { class: "muted" }, "Check each command before adding it: it runs on the daemon's machine."), h("table", { class: "table" }, h("tbody", null, rows))), { trigger });
  }

  /** Settings → Add MCP server: a stdio command or an HTTP URL. Adding
   * lists its tools at once, which doubles as the connection test. */
  async function addMcpServer() {
    const name = h("input", { type: "text", placeholder: "github", spellcheck: "false" });
    const transport = h("select", null, h("option", { value: "stdio" }, "Command (stdio)"), h("option", { value: "http" }, "URL (HTTP)"));
    const command = h("input", { type: "text", placeholder: "npx -y @modelcontextprotocol/server-github", spellcheck: "false" });
    const env = h("textarea", { rows: 2, placeholder: "KEY=value, one per line", spellcheck: "false", autocomplete: "off" });
    const url = h("input", { type: "text", placeholder: "https://example.com/mcp", spellcheck: "false" });
    const headers = h("textarea", { rows: 2, placeholder: "Header: value, one per line", spellcheck: "false", autocomplete: "off" });
    const stdio = [field("Command", command), field("Environment", env)];
    const http = [field("URL", url), field("Headers", headers)];
    const sync = () => {
      stdio.forEach((f) => f.classList.toggle("hidden", transport.value !== "stdio"));
      http.forEach((f) => f.classList.toggle("hidden", transport.value !== "http"));
    };
    transport.onchange = sync;
    sync();
    const body = h("div", { class: "try-form" }, field("Name", name), field("Connect with", transport), stdio, http);
    const r = await ask({ title: "Add MCP server", text: "It starts now and its tools are listed; they're asked about before first use.", body, ok: "Add and test",
      timeoutMs: 120000, secretFields: [command, env, url, headers],
      saveError: "Could not save the MCP server. Check the name, connection type, and command or URL.",
      saveRequest: () => {
        const serverName = recordValue(name, "An MCP server name is needed.");
        if (!/^[a-z0-9_-]+$/i.test(serverName) || serverName.includes("__")) {
          throw Object.assign(new Error("Use letters, digits, '-' and '_' in the server name, with no double underscore."), { field: name });
        }
        const server = { transport: transport.value };
        if (transport.value === "stdio") {
          const words = splitArgs(recordValue(command, "A command is needed."));
          if (!words[0]) throw Object.assign(new Error("A command is needed."), { field: command });
          Object.assign(server, { command: words[0], args: words.slice(1), env: pairs(env.value, "=") });
        } else {
          Object.assign(server, { url: recordValue(url, "A URL is needed."), headers: pairs(headers.value, ":") });
        }
        return { type: "add_mcp_server", name: serverName, server };
      } });
    if (!r) return;
    if (r.error) note("Saved " + r.added + ", but it didn't connect. Check its settings and use Restart to retry.", true);
    else note("Added " + r.added + ": " + (r.tools || []).length + " tools");
    refreshSettings();
  }
  /** Settings → Add skill: a folder or zip on the daemon's machine, or a
   * git URL (optionally a subfolder and ref). */
  async function addSkill() {
    const source = h("input", { type: "text", placeholder: "~/skills/pdf  or  https://github.com/org/skills.git", spellcheck: "false" });
    const subdir = h("input", { type: "text", placeholder: "subfolder (optional)", spellcheck: "false" });
    const ref = h("input", { type: "text", placeholder: "branch or tag (optional)", spellcheck: "false" });
    const body = h("div", { class: "try-form" }, field("Path, zip or git URL", source), field("Subfolder", subdir), field("Git ref", ref));
    const r = await ask({ title: "Add skill", text: "Paths are on the daemon's machine.", body, ok: "Add", timeoutMs: 120000,
      secretFields: [source], saveError: "Could not add the skill. Check the source, subfolder, and Git ref.",
      saveRequest: () => ({ type: "add_skill", source: recordValue(source, "A source is needed."),
        subdir: subdir.value.trim() || null, git_ref: ref.value.trim() || null }) });
    if (r) { note("Added skill " + (r.name || "")); await reviewSkill(r.name); refreshSettings(); }
  }
  /** A `formless.skill/1` or `formless.skills/1` bundle from this browser
   * (installed disabled, for review). */
  async function importSkillBundle() {
    const bundle = await pickJson();
    if (!bundle) return;
    if (!(await previewToolBundleDescriptions(bundle))) return;
    const imported = await act({ type: "import_skill", bundle }, "Imported; review and enable it below");
    if (imported) { for (const skill of imported) await reviewSkill(skill.name); refreshSettings(); }
  }
  const DAYS = ["Sunday", "Monday", "Tuesday", "Wednesday", "Thursday", "Friday", "Saturday"];
  /** A 5-field cron in words where it's a common shape, else as is. */
  function describeCron(expr) {
    const e = expr.trim();
    const named = {
      "@hourly": "every hour", "@daily": "every day at 0:00", "@midnight": "every day at 0:00",
      "@weekly": "every Sunday at 0:00", "@monthly": "on the 1st of each month at 0:00",
      "@yearly": "every year on January 1 at 0:00", "@annually": "every year on January 1 at 0:00",
    };
    if (named[e]) return named[e];
    const f = e.split(/\s+/);
    if (f.length !== 5) return e;
    const [m, hr, dom, mon, dow] = f;
    const step = m.match(/^\*\/(\d+)$/);
    if (step && hr === "*" && dom === "*" && mon === "*" && dow === "*") return "every " + step[1] + " minutes";
    const hourStep = hr.match(/^\*\/(\d+)$/);
    if (hourStep && /^\d+$/.test(m) && dom === "*" && mon === "*" && dow === "*") {
      const hours = Number(hourStep[1]), minute = Number(m);
      if (hours < 1 || hours > 0xffffffff || minute > 59) return e;
      const mm = String(minute).padStart(2, "0");
      if (hours === 1) return minute === 0 ? "every hour" : "every hour at minute " + mm;
      if (hours >= 24) return "every day at 0:" + mm;
      const interval = "every " + hours + " hours";
      if (24 % hours !== 0) return interval + " starting at 0:" + mm + " each day";
      return minute === 0 ? interval : interval + " at minute " + mm;
    }
    if (/^\d+$/.test(m) && hr === "*" && dom === "*" && mon === "*" && dow === "*") return Number(m) === 0 ? "every hour" : "every hour at minute " + m.padStart(2, "0");
    if (!/^\d+$/.test(m) || !/^\d+$/.test(hr)) return e;
    const time = hr + ":" + m.padStart(2, "0");
    if (dom === "1" && mon === "1" && dow === "*") return "every year on January 1 at " + time;
    if (mon !== "*") return e;
    if (dom === "*" && dow === "*") return "every day at " + time;
    if (dom === "*" && Number(m) <= 59 && Number(hr) <= 23) {
      const numericDays = dow.replace(/\b[a-z]{3}\b/gi, (day) => {
        const index = DAYS.findIndex((name) => name.slice(0, 3).toLowerCase() === day.toLowerCase());
        return index < 0 ? day : String(index);
      });
      if (numericDays === "1-5") return "every weekday at " + time;
      if (/^[0-7](,[0-7])*$/.test(numericDays)) {
        const days = new Set(numericDays.split(",").map((d) => DAYS[Number(d) % 7]));
        return "every " + [...days].join(", ") + " at " + time;
      }
    }
    if (/^\d+$/.test(dom) && dow === "*") return "on day " + dom + " of each month at " + time;
    return e;
  }
  /** Settings → New job: a task, when to run it, and which Session. */
  async function newJob() {
    const prompt = h("textarea", { rows: 3, placeholder: "Summarise yesterday's GitHub notifications" });
    const when = h("select", null,
      [["daily", "Every day"], ["weekdays", "Every weekday"], ["weekly", "Every week"], ["hourly", "Every hour"], ["once", "Once"], ["cron", "Custom (cron)"]]
        .map(([v, l]) => h("option", { value: v }, l)));
    const time = h("input", { type: "time", value: "09:00" });
    const day = h("select", null, DAYS.map((d, i) => h("option", { value: String(i), selected: i === 1 }, d)));
    const at = h("input", { type: "datetime-local" });
    const cron = h("input", { type: "text", placeholder: "every weekday at 9am, or 0 9 * * 1-5", spellcheck: "false" });
    const session = h("select", null, h("option", { value: "" }, "A new Session each time"),
      S.sessions.filter((s) => !s.archived).map((s) => h("option", { value: s.id }, s.name)));
    const preview = h("div", { class: "muted" });
    const fields = { time: field("Time", time), day: field("Day", day), at: field("When", at), cron: field("Schedule: plain English or cron (minute hour day month weekday)", cron) };
    const schedule = () => {
      const [hh, mm] = (time.value || "09:00").split(":").map(Number);
      switch (when.value) {
        case "daily": return { cron: mm + " " + hh + " * * *" };
        case "weekdays": return { cron: mm + " " + hh + " * * 1-5" };
        case "weekly": return { cron: mm + " " + hh + " * * " + day.value };
        case "hourly": return { cron: "0 * * * *" };
        case "once": return at.value ? { at: at.value.replace("T", " ") } : {};
        default: return cron.value.trim() ? { cron: cron.value.trim() } : {};
      }
    };
    const sync = () => {
      const v = when.value;
      fields.time.classList.toggle("hidden", !["daily", "weekdays", "weekly"].includes(v));
      fields.day.classList.toggle("hidden", v !== "weekly");
      fields.at.classList.toggle("hidden", v !== "once");
      fields.cron.classList.toggle("hidden", v !== "cron");
      const s = schedule();
      preview.textContent = s.cron ? "Runs " + describeCron(s.cron) + " (" + s.cron + ")" : s.at ? "Runs once at " + s.at : "";
    };
    for (const el of [when, time, day, at, cron]) el.addEventListener("input", sync);
    when.addEventListener("change", sync);
    sync();
    const body = h("div", { class: "try-form" }, field("Task", prompt), field("Schedule", when),
      fields.time, fields.day, fields.at, fields.cron, preview, field("Session", session),
      h("p", { class: "muted" }, "It runs with the Session's model; pick a Session set to the model or Role you want."));
    const j = await ask({ title: "New job", body, ok: "Schedule", saveRequest: () => {
      const task = recordValue(prompt, "The job needs a task.");
      const s = schedule();
      if (!s.cron && !s.at) recordValue(when.value === "once" ? at : cron, "Pick when it runs.");
      return { type: "add_job", prompt: task, session_id: session.value || null, cron: s.cron || null, at: s.at || null };
    } });
    if (j) { note("Scheduled: next run " + uiFormat(new Date(j.next_run))); refreshSettings(); }
  }
  /** A labelled form field for ask() bodies. */
  function field(label, input) {
    return h("label", { class: "try-field" }, h("span", null, label), input);
  }
  async function editMemory(memory) {
    const result = await ask({ title: memory ? "Edit memory" : "Add a memory", inputKind: "prose", input: true,
      value: memory?.text, placeholder: "e.g. Use pnpm, not npm", ok: memory ? "Save" : "Add", saveRequest: () => {
        const input = $("ask-input");
        recordValue(input, "Enter a memory first.");
        if (input.value === memory?.text) return null;
        return { type: "upsert_memory", id: memory?.id, text: input.value };
      } });
    if (result) { toast(memory ? "Saved" : "Added"); refreshSettings(); }
  }
  async function testProvider(name) {
    toast("Testing " + name + "…");
    const r = await act({ type: "test_provider", name }, null, 30000);
    if (r) note(name + ": connected, " + (r.models || []).length + " models" + ((r.models || []).length ? " (" + r.models.slice(0, 5).join(", ") + (r.models.length > 5 ? ", …" : "") + ")" : ""));
  }
  async function editProvider(p) {
    const name = h("input", { type: "text", value: p ? p.name : "", disabled: !!p, placeholder: "openrouter", spellcheck: "false" });
    const base = h("input", { type: "text", value: p ? p.base_url : "", placeholder: "https://openrouter.ai/api/v1", spellcheck: "false" });
    const model = h("input", { type: "text", value: p ? p.model : "", placeholder: "default model (optional)", spellcheck: "false" });
    // Write-only: an existing key is never sent back; blank keeps it.
    const key = h("input", { type: "password", value: "", placeholder: p && p.has_key ? "stored · leave blank to keep" : "API key (stored in the keychain)", autocomplete: "off" });
    const body = h("div", { class: "try-form" }, field("Name", name), field("Base URL", base), field("Model", model), field("API key", key));
    const r = await ask({ title: p ? "Edit provider " + p.name : "Add provider", body, ok: "Save",
      secretFields: [key, base], saveError: "Could not save the provider. Check the name, base URL, and key storage on the daemon.",
      saveRequest: () => ({
        type: "upsert_provider", name: recordValue(name, "A provider name is needed."),
        base_url: recordValue(base, "A base URL is needed."), model: model.value.trim() || null, api_key: key.value || null,
      }) });
    if (r) { toast("Saved " + name.value.trim()); refreshSettings(); }
  }
  async function editRole(existing, r, providers) {
    const name = h("input", { type: "text", value: existing, disabled: !!existing, placeholder: "fast", spellcheck: "false" });
    const desc = h("input", { type: "text", value: r.description || "", placeholder: "When to use it, written for the agent" });
    const prov = h("select", null, providers.map((p) => h("option", { value: p, selected: (r.provider || "default") === p }, p)));
    const model = h("input", { type: "text", value: r.model || "", placeholder: "model (blank = the provider's default)", spellcheck: "false" });
    const effort = h("select", null, ["", "minimal", "low", "medium", "high"].map((e) => h("option", { value: e, selected: (r.reasoning_effort || "") === e }, e || "default effort")));
    const body = h("div", { class: "try-form" }, field("Name", name), field("Description", desc), field("Provider", prov), field("Model", model), field("Reasoning effort", effort));
    const saved = await ask({ title: existing ? "Edit role " + existing : "Add role", body, ok: "Save", saveRequest: () => {
      const role = Object.assign({}, r, { description: desc.value.trim(), provider: prov.value === "default" ? null : prov.value, model: model.value.trim(), reasoning_effort: effort.value || null });
      return { type: "upsert_role", name: recordValue(name, "A Role name is needed."), role };
    } });
    if (saved) { toast("Saved role " + name.value.trim()); refreshSettings(); }
  }
  const settingsLists = {
    mcpServers: "list_mcp_servers",
    providers: "list_providers",
    harness: "list_harness_changes",
    relayClients: "list_relay_clients",
    skills: "list_skills",
    memories: "list_memories",
    jobs: "list_jobs",
  };
  const settingsBackground = ["settings", "status", "descriptionReviews"];
  const settingsDomains = [...settingsBackground, ...Object.keys(settingsLists), "audit"];
  let settingsRefresh = null;
  function settingsConnectionReady() {
    return S.ws?.readyState === 1 && !!S.channel && (S.local || S.relayReady);
  }
  function isSettingsConnection(connection) {
    return settingsConnectionReady() && connection?.ws === S.ws && connection?.channel === S.channel;
  }
  function refreshSettings() {
    return requestSettingsRefresh(settingsDomains);
  }
  function requestSettingsRefresh(domains) {
    if (!settingsConnectionReady()) return Promise.resolve();
    if (!isSettingsConnection(settingsRefresh)) {
      settingsRefresh = { ws: S.ws, channel: S.channel, dirty: new Set(), pending: null };
    }
    const refresh = settingsRefresh;
    for (const domain of domains) refresh.dirty.add(domain);
    // Collect arrivals in the same turn before choosing the first request.
    refresh.pending ||= Promise.resolve().then(() => drainSettingsRefresh(refresh));
    return refresh.pending;
  }
  async function drainSettingsRefresh(refresh) {
    const allowed = (domain) => settingsBackground.includes(domain)
      || (S.view === "settings" && !$("app").classList.contains("hidden"));
    try {
      while (isSettingsConnection(refresh)) {
        const batch = settingsDomains.filter(domain => allowed(domain) && refresh.dirty.has(domain));
        if (!batch.length) return;
        for (const domain of batch) {
          if (!allowed(domain)) continue;
          // Invalidate again only if a trigger arrives after this fetch starts.
          refresh.dirty.delete(domain);
          await loadSettingsDomain(domain, refresh);
          if (!isSettingsConnection(refresh)) return;
        }
        renderView();
        renderStatus();
      }
    } finally {
      refresh.pending = null;
    }
  }
  async function loadSettingsDomain(domain, connection) {
    switch (domain) {
      case "settings": {
        const settings = await request({ type: "get_settings" }).catch(e => ({
          error: "This daemon does not expose settings remotely (" + e.message + "). Update formless to see them here.",
        }));
        if (!isSettingsConnection(connection)) return;
        S.settings = settings;
        applyMotion();
        if (applyLanguage()) renderAll();
        break;
      }
      case "status": {
        const status = await act({ type: "status" });
        if (!isSettingsConnection(connection)) return;
        const helpWasSeen = approvalHelpSeen();
        S.status = status;
        if (S.status && S.approvalHelpSeen !== true) {
          S.approvalHelpSeen = typeof S.status.approval_help_seen === "boolean" ? S.status.approval_help_seen : null;
        }
        if (approvalHelpSeen() !== helpWasSeen) renderApproval();
        break;
      }
      case "descriptionReviews":
        await refreshDescriptionReviews();
        break;
      case "audit":
        await loadAudit(false, true);
        break;
      default: {
        const list = await request({ type: settingsLists[domain] }).catch(() => null);
        if (isSettingsConnection(connection)) S[domain] = list || (domain === "providers" ? { providers: [], roles: {} } : []);
        break;
      }
    }
  }
  // "3 steps · 12.3k in / 1.2k out · 40% cached · $0.021" (SPEC §10.6).
  function usageText(u) {
    const k = (n) => (n >= 1000 ? uiFixed(n / 1000, 1) + "k" : uiFormat(n));
    let s = u.steps + " step" + (u.steps === 1 ? "" : "s") + " · " + k(u.prompt_tokens) + " in / " + k(u.completion_tokens) + " out";
    if (u.prompt_tokens > 0 && u.cached_tokens > 0) s += " · " + uiFormat(u.cached_tokens / u.prompt_tokens, { style: "percent", maximumFractionDigits: 0 }) + " cached";
    if (u.cache_write_tokens > 0) s += " · " + u.cache_write_tokens + " written to cache";
    if (u.cost == null && (u.unpriced || []).length) s += " · no price for " + u.unpriced.join(", ");
    if (u.cost != null) s += " · " + uiMoney(u.cost, 3);
    if (u.memories > 0) s += " · used " + u.memories + (u.memories === 1 ? " memory" : " memories");
    return s;
  }
  async function revokeApproval(t, capability) {
    if (!(await ask({ title: "Revoke " + (capability || "every approval") + " for " + t.name + "?", text: "The next call that needs it asks again.", ok: "Revoke", danger: true }))) return;
    const r = await act({ type: "revoke_approval", tool_id: t.id, capability }, "Revoked");
    if (r) {
      // Config trust keeps approving these (#844).
      if ((r.still_trusted || []).length) note("Still allowed without asking by your trusted folders/hosts: " + r.still_trusted.join(", "));
      selectTool(t.id, true);
    }
  }
  function toolDetailFailure(id) {
    if (!id || S.toolDetailError !== id) return null;
    return h("div", { class: "placeholder" },
      h("p", { class: "error" }, "Could not load Tool details."),
      h("button", { class: "btn small", type: "button", onclick: () => selectTool(id, true) }, "Retry"));
  }
  let toolSeq = 0;
  async function selectTool(id, quiet) {
    const seq = ++toolSeq;
    if (S.selectedTool !== id) S.toolVersionDiff = null;
    S.selectedTool = id;
    S.toolDetailError = null;
    renderView();
    if (!quiet && S.editingCode && S.editingCode.toolId !== id) S.editingCode = null;
    const d = await act({ type: "get_tool", id }, null, undefined, () => selectTool(id, true));
    if (seq !== toolSeq || S.selectedTool !== id) return;
    if (d) S.toolDetail = d;
    else S.toolDetailError = id;
    renderView();
  }

  function addApproval(ev) {
    const kind = ev.type === "harness_permission_requested" ? "harness" : ev.type === "promotion_approval_requested" ? "promotion" : "tool";
    const id = kind === "harness" ? ev.permission_id : kind === "promotion" ? ev.promotion_id : ev.approval_id;
    const existing = S.approvals.find((a) => a.kind === kind && a.id === id);
    if (existing) { if (ev.summary) existing.summary = ev.summary; return; }
    S.approvals.push({ kind, id, tool_id: ev.tool_id || null, tool_name: ev.tool_name,
      version: ev.version, reason: ev.reason, capabilities: ev.capabilities || [], memory_mb: ev.memory_mb,
      detail: ev.detail || null, context: ev.context || null, summary: ev.summary, already: ev.already_allowed || [],
      code: ev.code_preview || null, batch: ev.batch || null, session_id: ev.session_id,
      topic_id: ev.topic_id, attempt_id: ev.attempt_id, harness: ev.harness, acp_agent: ev.acp_agent,
      title: ev.title, permissionKind: ev.permission_kind ?? ev.kind, raw_input: ev.raw_input, options: ev.options || [],
      pathOptions: ev.path_options || [], picks: {} });
  }
  function clearQuestions() {
    const questions = S.questions || [];
    S.questions = [];
    S.pendingQuestion = null;
    S.questionId = null;
    for (const q of questions) cancelAsk("question:" + q.id);
  }
  function clearLiveState() {
    S.toolVersionDiff = null;
    invalidateBranchSummary();
    ++sessionSeq; // invalidate a snapshot requested on the old connection
    S.sessionSnapshot = null;
    if (S.sessionLoad?.status === "pending" && S.sessionLoad.id === S.current) {
      S.sessionLoad = { id: S.current, status: "failed" };
    }
    S.approvals = [];
    S.approvalHelpSeen = undefined;
    clearQuestions();
    S.running.clear();
    S.activity = null;
    clearLiveText();
    S.liveTools = []; S.liveRun = null;
    renderApproval();
  }
  function restoreSessionRunState(id, result, events) {
    let latest = result.latest_run && result.latest_run.run_id;
    let recorded = result.last_recap || null;
    if (recorded && latest && recorded.run_id !== latest) recorded = null;
    let plan = result.plan || [], state = result.plan_state || null;
    for (const ev of events) {
      if (ev.session_id !== id) continue;
      if (ev.type === "run_started") { latest = ev.run_id; recorded = null; }
      if (ev.type === "run_recap" && (!latest || latest === ev.run_id)) recorded = { run_id: ev.run_id, recap: ev.recap };
      if (ev.type === "plan_updated") {
        const metadata = ev.plan_state || null;
        const older = state && state.updated_at && metadata && metadata.updated_at && metadata.updated_at < state.updated_at;
        const wrongRun = metadata && !metadata.previous_task && ["live", "resumed"].includes(metadata.origin) && metadata.run_id && latest && metadata.run_id !== latest;
        if (!older && !wrongRun) { plan = ev.items || []; state = metadata; }
      }
      if (["run_finished", "run_paused"].includes(ev.type) && state && state.run_id === ev.run_id && (!latest || latest === ev.run_id)) state = { ...state, run_status: ev.type === "run_paused" ? "paused" : ev.status };
    }
    S.plan = plan; S.planState = state;
    S.recap = recorded && recorded.recap;
    S.recapRunId = recorded && recorded.run_id;
    if (latest) S.liveRun = { id: latest, ended: !S.running.has(id) };
  }
  function applySessionLiveState(id, result, liveEvents) {
    if (typeof result.running !== "boolean" || !Array.isArray(result.pending_events)) return;
    if (result.running) S.running.add(id); else S.running.delete(id);
    // Reduce authoritative global attention plus racing events as data. Replaying
    // onEvent would notify twice and erase already-received streaming text.
    const pending = new Map();
    let latestRun = result.latest_run && result.latest_run.run_id;
    for (const ev of result.pending_events.concat(liveEvents)) {
      switch (ev.type) {
        case "approval_requested": pending.set("tool:" + ev.approval_id, ev); break;
        case "harness_permission_requested": pending.set("harness:" + ev.permission_id, ev); break;
        case "user_question": pending.set("question:" + ev.question_id, ev); break;
        case "promotion_approval_requested": pending.set("promotion:" + ev.promotion_id, ev); break;
        case "approval_closed": case "approval_answered": pending.delete("tool:" + ev.approval_id); break;
        case "harness_permission_closed": pending.delete("harness:" + ev.permission_id); break;
        case "user_question_closed": pending.delete("question:" + ev.question_id); break;
        case "promotion_approval_resolved": pending.delete("promotion:" + ev.promotion_id); break;
        case "approval_summary": {
          const req = pending.get("tool:" + ev.approval_id);
          if (req) pending.set("tool:" + ev.approval_id, { ...req, summary: ev.summary });
          break;
        }
        case "run_started": if (ev.session_id === id) latestRun = ev.run_id; S.running.add(ev.session_id); break;
        case "run_finished": case "run_paused": if (ev.session_id !== id || !latestRun || latestRun === ev.run_id) S.running.delete(ev.session_id); break;
      }
    }
    S.approvals = S.approvals.filter((a) => pending.has(a.kind + ":" + a.id));
    const questions = S.questions || [];
    S.questions = questions.filter((q) => pending.has("question:" + q.id));
    S.pendingQuestion = S.questions[0] || null;
    S.questionId = S.pendingQuestion && S.pendingQuestion.id;
    for (const q of questions) if (!pending.has("question:" + q.id)) cancelAsk("question:" + q.id);
    for (const ev of pending.values()) {
      if (ev.type === "user_question") queueQuestion(ev);
      else addApproval(ev);
    }
    showQuestion();
    renderApproval();
    renderComposer();
    renderStatus();
  }
  /** Open a session; `stay` keeps the current view (initial load). */
  let sessionSeq = 0;
  let metadataRefreshSeq = 0;
  async function refreshEditedMessages(id) {
    const seq = ++metadataRefreshSeq;
    const session = sessionSeq;
    const result = await act({ type: "get_session", id });
    if (!result || seq !== metadataRefreshSeq || session !== sessionSeq || S.current !== id) return;
    const current = new Map((S.messages || []).map((message) => [message.id, message]));
    for (const updated of result.messages || []) {
      const message = current.get(updated.id);
      if (!message || message.content !== updated.content || message.seq !== updated.seq) continue;
      message.meta = updated.meta;
    }
    if (S.view === "chat") renderHistory();
  }
  async function openSession(id, stay) {
    if (S.current && !S.editMessageId) saveDraft(S.current, $("input").value);
    S.topicFocus = null;
    if (S.current !== id) invalidateBranchSummary();
    cancelEventRender();
    resetReadingSession(id);
    const seq = ++sessionSeq;
    closeSidebar();
    if (!stay && S.view !== "chat") setView("chat");
    S.current = id;
    renderSessions();
    S.sessionLoad = { id, status: "pending" };
    const snapshot = S.sessionSnapshot = { events: [] };
    S.messages = [];
    clearLiveText();
    S.liveTools = []; S.liveRun = null;
    renderHistory();
    S.runTools = null;
    renderRunTools();
    request({ type: "context_usage", session_id: id }).then((r) => {
      (S.contextFill = S.contextFill || {})[id] = { used: r.used_tokens, size: r.context_tokens };
      renderStatus();
    }).catch(() => {});
    request({ type: "session_usage", session_id: id }).then((r) => {
      S.sessionUsage = { id, runs: r.runs, total: r.total };
      if (r.last_run && r.last_run.steps) (S.usage = S.usage || {})[id] = r.last_run;
      renderStatus();
    }).catch(() => {});
    rendered.clear(); // a reload may carry edited messages
    renderChips();
    const draftKey = sessionCacheKey(S.storageScope, id);
    const draft = Object.prototype.hasOwnProperty.call(attachmentDrafts, draftKey) ? attachmentDrafts[draftKey] : loadDraft(S.storageScope, id);
    S.drafts[id] = draft;
    delete attachmentDrafts[draftKey];
    $("input").value = draft;
    saveDraft(id, draft);
    autosize();
    void refreshAttachmentInventory(id);
    // Live traffic only for the Session on screen: the relay then carries
    // (and bills) one Session's tokens, not every Session's.
    S.others = [];
    request({ type: "subscribe", sessions: [id], deltas: true, client: CLIENT_LABEL })
      .then((r) => {
        // The client list can arrive before this reply: drop ourselves now.
        S.conn = r && r.conn;
        S.others = (S.others || []).filter((c) => c.conn !== S.conn);
        renderStatus();
      }).catch(() => {});
    prefs.set(sessionStorageKey(S.storageScope), id);
    S.editMessageId = null;
    S.queued = []; renderQueue();
    S.plan = []; S.planState = null; renderPlan();
    S.step = null; renderStep();
    S.recap = null; S.recapRunId = null;
    const r = await act({ type: "get_session", id });
    if (seq !== sessionSeq || S.current !== id) return;
    S.sessionSnapshot = null;
    S.sessionLoad = { id, status: r ? "loaded" : "failed" };
    if (r) {
      applySessionLiveState(id, r, snapshot.events);
      const live = S.messages;
      const ids = new Set((r.messages || []).map((m) => m.id));
      S.messages = (r.messages || []).concat(live.filter((m) => !ids.has(m.id)));
      if (!snapshot.events.some((ev) => ev.session_id === id && ["run_started", "run_finished", "run_paused"].includes(ev.type))) {
        S.unfinished = r.unfinished_run || null;
      }
      renderRunBanner();
      S.queued = r.queued_items || [];
      renderQueue();
      restoreSessionRunState(id, r, snapshot.events);
      if (S.liveRun && !S.liveRun.ended && S.running.has(id)) {
        announcedRun = { session: id, id: S.liveRun.id, afterSeq: Math.max(-1, ...S.messages.map((message) => message.seq ?? -1)) };
      }
      renderPlan();
      S.stepMode = !!r.step_mode;
      const i = S.sessions.findIndex((s) => s.id === id);
      if (i >= 0 && r.session) S.sessions[i] = r.session;
    }
    if (!isRunning()) S.activity = null;
    renderAll();
    void flushOutbox();
  }
  async function newSession() {
    const s = await act({ type: "create_session", name: null });
    if (!s) return;
    S.sessions = [s, ...S.sessions.filter((x) => x.id !== s.id)];
    await openSession(s.id);
    $("input").focus();
  }
  async function openMainChat(expectedSelection) {
    if (!S.mainChat?.id) await refreshTopics(true);
    if (expectedSelection && (S.current !== expectedSelection.sessionId || S.storageScope !== expectedSelection.scope)) return false;
    const main = S.mainChat;
    if (!main?.id) return false;
    await openSession(main.id);
    return S.current === main.id && (!expectedSelection || S.storageScope === expectedSelection.scope)
      && !(S.sessionLoad?.id === main.id && S.sessionLoad.status === "failed");
  }
  /** What this client is called to others on the same Session (§10.4). */
  const CLIENT_LABEL = document.querySelector('meta[name="formless-local"]') ? "web" : "web (relay)";
  /** "TUI, web ×2" for the other clients on this Session. */
  function othersText(others) {
    const counts = new Map();
    for (const o of others) counts.set(o.label, (counts.get(o.label) || 0) + 1);
    return [...counts].map(([l, n]) => (n > 1 ? l + " ×" + n : l)).join(", ");
  }
  /** The client a user message came from, when it wasn't this one: another
   * kind of client, or another one still attached. */
  // The first provider turn that included a steer, "unrecorded", or null.
  function steerStep(m) {
    try {
      const s = JSON.parse(m.meta || "{}").steer;
      if (!s || typeof s.run_id !== "string") return null;
      return s.context_included === true && Number.isInteger(s.step) && s.step > 0 ? s.step : "unrecorded";
    } catch { return null; }
  }
  function sentElsewhere(m) {
    let c;
    try { c = JSON.parse(m.meta || "{}").client; } catch (_) {}
    if (!c || !c.label) return null;
    const otherKind = !c.label.startsWith("web");
    const otherHere = c.conn !== S.conn && (S.others || []).some((o) => o.conn === c.conn);
    return otherKind || otherHere ? c.label : null;
  }
  /** Ctrl/⌘+K: fuzzy-switch Sessions by name, tags or last message;
   * Enter opens, Ctrl/⌘+Enter starts a Session named after the query. */
  function openSwitcher() {
    if (document.querySelector(".switcher")) return;
    const input = h("input", { type: "text", placeholder: "Switch to a Session…", "aria-label": "Find a Session", autocomplete: "off", spellcheck: "false" });
    const list = h("ul", { class: "picker-list", role: "listbox" });
    const status = h("span", { class: "muted" }, "Enter open · Ctrl/⌘+Enter new Session named this");
    const dialog = h("div", { class: "backdrop switcher", role: "dialog", "aria-modal": "true", "aria-label": "Switch Session" },
      h("div", { class: "modal picker" }, h("h2", null, "Switch Session"), input, list,
        h("div", { class: "modal-actions" }, status)));
    let matches = [], sel = 0;
    const close = () => closeDialog(dialog);
    function draw() {
      const q = input.value;
      matches = S.sessions.filter((s) => !s.archived).map((s) => {
        // The name counts most, then tags, then the preview.
        const fields = [s.name, (s.tags || []).map((t) => "#" + t).join(" "), s.preview || ""];
        const scores = fields.map((f, k) => { const m = fuzzyMatch(q, f || ""); return m && m[0] - 20 * k; }).filter((x) => x !== null);
        return { s, score: scores.length ? Math.max(...scores) : null, pos: new Set((fuzzyMatch(q, s.name) || [0, []])[1]) };
      }).filter((x) => x.score !== null)
        .sort((a, b) => q.trim() ? b.score - a.score : String(b.s.updated_at).localeCompare(String(a.s.updated_at)))
        .slice(0, 100);
      sel = Math.min(sel, Math.max(0, matches.length - 1));
      list.replaceChildren(...matches.map((x, i) => {
        const name = [...x.s.name].map((ch, n) => x.pos.has(n) ? h("mark", null, ch) : ch);
        const li = h("li", { role: "option", "aria-selected": String(i === sel), class: i === sel ? "sel" : null },
          h("span", { class: "picker-id" }, name),
          h("span", { class: "muted" }, relTime(x.s.updated_at) + (x.s.preview ? " · “" + x.s.preview.slice(0, 50) + "”" : "")));
        li.onclick = () => { sel = i; finish(false); };
        return li;
      }));
      if (!matches.length && q.trim()) list.replaceChildren(h("li", { class: "muted" }, "no match · Ctrl/⌘+Enter starts “" + q.trim() + "”"));
      const selNode = list.children[sel];
      if (selNode) selNode.scrollIntoView({ block: "nearest" });
    }
    async function finish(create) {
      const q = input.value.trim();
      const hit = matches[sel];
      close();
      if (create || !hit) {
        const s = await act({ type: "create_session", name: q || null });
        if (!s) return;
        S.sessions = [s, ...S.sessions.filter((x) => x.id !== s.id)];
        return openSession(s.id);
      }
      openSession(hit.s.id);
    }
    input.oninput = () => { sel = 0; draw(); };
    dialog.addEventListener("keydown", (e) => {
      if (e.key === "ArrowDown") { e.preventDefault(); sel = Math.min(sel + 1, matches.length - 1); draw(); }
      else if (e.key === "ArrowUp") { e.preventDefault(); sel = Math.max(sel - 1, 0); draw(); }
      else if (e.key === "Enter") { e.preventDefault(); finish(e.ctrlKey || e.metaKey); }
    });
    openDialog(dialog, { initialFocus: input, onDismiss: close });
    draw();
  }
  /** "+ New session": with templates saved, pick one (or blank) first. */
  async function pickNewSession() {
    const list = (await request({ type: "list_session_templates" }).catch(() => [])) || [];
    if (!list.length) return newSession();
    const opts = ["", ...list.map((t) => t.name)];
    const body = h("div", { class: "questions" }, opts.map((name, i) => h("label", { class: "q-option" },
      h("input", { type: "radio", name: "tpl", value: name, checked: i === 0 }),
      h("span", {}, h("span", { text: name || "Blank Session" }),
        name ? h("span", { class: "muted", text: templateSummary(list[i - 1]) }) : null))));
    if (!(await ask({ title: "New session", body, ok: "Create" }))) return;
    const picked = body.querySelector("input:checked");
    return picked && picked.value ? newFromTemplate(picked.value) : newSession();
  }
  function templateSummary(t) {
    return [t.model && "model " + t.model, t.reasoning_effort && "effort " + t.reasoning_effort, t.cwd && "in " + t.cwd,
      t.instructions && "instructions", t.first_message && "“" + t.first_message.slice(0, 40) + "”"].filter(Boolean).join(" · ") || "defaults";
  }
  /** A Session set up from a template, its first message in the composer. */
  async function newFromTemplate(template) {
    const r = await act({ type: "create_session_from_template", template, name: null });
    if (!r || !r.session) return;
    S.sessions = [r.session, ...S.sessions.filter((x) => x.id !== r.session.id)];
    await openSession(r.session.id);
    if (r.first_message) { $("input").value = r.first_message; saveDraft(S.current, r.first_message); }
    $("input").focus();
  }
  async function templateCommand(arg) {
    const [verb, name, ...rest] = (arg || "").trim().split(/\s+/);
    if (!verb || verb === "list") {
      const list = await act({ type: "list_session_templates" });
      if (!list) return;
      return note(list.length ? list.map((t) => "template " + t.name + ": " + templateSummary(t)).join("\n") : "no templates yet: /template save NAME [FIRST MESSAGE]");
    }
    if (!name) return toast("usage: /template save NAME [FIRST MESSAGE] · /template delete NAME", true);
    if (verb === "save") {
      if (!S.current) return toast("No current session.", true);
      const t = await act({ type: "save_session_template", id: S.current, name, first_message: rest.join(" ") || null });
      if (t) note("saved template " + t.name + " (use it with /new " + t.name + ")");
    } else if (verb === "delete" || verb === "rm") {
      if (await act({ type: "delete_session_template", name })) note("deleted template " + name);
    } else toast("usage: /template save NAME [FIRST MESSAGE] · /template delete NAME", true);
  }
  async function tagSession(s) {
    const text = await ask({ title: "Tags for " + s.name, inputKind: "machine", input: true, value: (s.tags || []).join(" "), placeholder: "work client-a", ok: "Save" });
    if (text == null) return;
    const r = await act({ type: "set_session_tags", id: s.id, tags: text.split(/[\s,]+/).filter(Boolean) });
    if (!r) return;
    const i = S.sessions.findIndex((x) => x.id === r.id);
    if (i >= 0) S.sessions[i] = r;
    renderSessions();
  }
  async function renameSession(s) {
    const name = await ask({ title: "Rename session", inputKind: "prose", input: true, value: s.name, ok: "Rename" });
    if (name && name.trim() && name.trim() !== s.name) await act({ type: "rename_session", id: s.id, name: name.trim() });
  }
  /** Re-run the namer on the recent messages and offer its name to edit. */
  async function suggestName(s) {
    const r = await act({ type: "suggest_session_name", id: s.id }, null, 60000);
    if (!r) return;
    if (!r.name) return toast("No name to suggest yet.");
    const name = await ask({ title: "Suggested name", inputKind: "prose", input: true, value: r.name, ok: "Rename" });
    if (name && name.trim() && name.trim() !== s.name) await act({ type: "rename_session", id: s.id, name: name.trim() });
  }
  /** Standing instructions for one Session's agent; empty clears them. */
  async function editInstructions(s, text) {
    if (text === undefined) {
      text = await ask({ title: "Instructions for " + s.name, inputKind: "prose", input: true, value: s.instructions || "", placeholder: "Answer in Thai. This Session is about the billing service.", ok: "Save" });
      if (text == null) return;
    }
    const r = await act({ type: "set_session_instructions", id: s.id, instructions: text.trim() || null }, text.trim() ? "Instructions saved" : "Instructions cleared");
    if (!r) return;
    const i = S.sessions.findIndex((x) => x.id === r.id);
    if (i >= 0) S.sessions[i] = r;
  }
  async function forkSession(id) {
    if (S.topicsAvailable && id === S.current && currentTopicAttempt()) return;
    if (!id) return;
    const s = await act({ type: "fork_session", id, name: null, up_to_seq: null }, "Forked session");
    if (s && s.id) { S.sessions = [s, ...S.sessions.filter((x) => x.id !== s.id)]; await openSession(s.id); }
  }
  /** Archive, export, tag or delete several Sessions at once; deleting
   * asks for "delete" to be typed. */
  async function bulkSessions(action, ids) {
    const list = S.sessions.filter((s) => ids.includes(s.id));
    if (action === "delete") {
      const typed = await ask({ title: "Delete " + list.length + " Sessions?", inputKind: "machine", text: "Type delete to confirm. Their transcripts are deleted.", input: true, placeholder: "delete", ok: "Delete", danger: true });
      if (typed == null || typed.trim() !== "delete") return;
      for (const s of list) await act({ type: "delete_session", id: s.id });
      toast("Deleted " + list.length + " Sessions");
    } else if (action === "archive") {
      const restore = list.every((s) => s.archived);
      for (const s of list) await act({ type: "set_session_archived", id: s.id, archived: !restore });
    } else if (action === "export") {
      for (const s of list) await exportSession(s.id);
    } else if (action === "tag") {
      const text = await ask({ title: "Tag " + list.length + " Sessions", inputKind: "machine", input: true, placeholder: "work client-a", ok: "Add tags" });
      if (!text) return;
      const add = text.split(/[\s,]+/).filter(Boolean);
      for (const s of list) await act({ type: "set_session_tags", id: s.id, tags: [...(s.tags || []), ...add] });
    }
    S.marked.clear();
    await refreshSessions();
  }
  /** `format`: json (the re-importable bundle), markdown or html. */
  async function exportSession(id, format) {
    if (!id) return;
    const r = await act({ type: "export_session", id, format: format || null });
    if (!r) return;
    const base = "formless-session-" + slug(r.name);
    if (r.format === "markdown") downloadBlob(base + ".md", new Blob([r.content], { type: "text/markdown" }));
    else if (r.format === "html") downloadBlob(base + ".html", new Blob([r.content], { type: "text/html" }));
    else download(base + ".json", r);
  }
  async function pickExport(id) {
    if (!id) return;
    const formats = [["markdown", "Markdown", "readable; Tool calls folded"], ["html", "HTML", "a self-contained page"], ["json", "JSON bundle", "re-importable"]];
    const body = h("div", { class: "questions" }, formats.map(([v, label, desc], i) => h("label", { class: "q-option" },
      h("input", { type: "radio", name: "fmt", value: v, checked: i === 0 }),
      h("span", {}, h("span", { text: label }), h("span", { class: "muted", text: desc })))));
    if (!(await ask({ title: "Export session", body, ok: "Export" }))) return;
    exportSession(id, body.querySelector("input:checked").value);
  }
  async function importSession() {
    const bundle = await pickJson();
    if (!bundle) return;
    const s = await act({ type: "import_session", bundle }, "Session imported");
    await refreshSessions();
    if (s && s.id) openSession(s.id);
  }
  async function deleteSession(s) {
    const ok = await ask({ title: "Delete “" + s.name + "”?", text: "The session and its transcript are deleted (you can undo for a short while).", ok: "Delete", danger: true });
    if (!ok) return;
    const r = await act({ type: "delete_session", id: s.id });
    if (r && r.deleted) {
      toast("Deleted “" + s.name + "”", false, { label: "Undo", ms: (r.undo_secs || 30) * 1000, run: async () => {
        if (await act({ type: "restore_session", id: s.id }, "Restored “" + s.name + "”")) refreshSessions();
      } });
    }
  }

  function beginEdit(msg) {
    if (msg.generated_by != null || (S.topicsAvailable && currentTopicAttempt())) return;
    if (!S.editMessageId) saveDraft(S.current, $("input").value);
    S.editMessageId = msg.id;
    $("input").value = msg.content;
    autosize();
    renderComposer();
    $("input").focus();
  }
  function cancelEdit() {
    S.editMessageId = null;
    restoreDraft(S.current);
    renderComposer();
  }

  function rollbackTool(t, version) {
    ask({ title: "Roll back " + t.name + " to v" + version + "?", text: "Later Invocations use v" + version + "; every version is kept.", ok: "Roll back" })
      .then((ok) => { if (ok) act({ type: "rollback_tool", id: t.id, version }, t.name + " rolled back to v" + version).then(() => selectTool(t.id, true)); });
  }
  /** Names of Tools, each opening its detail. */
  function dependencyLinks(names) {
    return (names || []).map((n, i) => [i ? ", " : null, h("button", { class: "linkish", type: "button", onclick: () => {
      const t = S.tools.find((x) => x.name === n);
      if (t) selectTool(t.id);
    } }, n)]);
  }
  /** Retire (warning first when other Tools call it) or restore. */
  async function retireTool(t, d) {
    if (!t.retired && d && d.used_by && d.used_by.length) {
      const ok = await ask({ title: "Retire " + t.name + "?", text: "It is used by " + d.used_by.join(", ") + "; their calls to it will fail until it is restored.", ok: "Retire", danger: true });
      if (!ok) return;
    }
    act({ type: t.retired ? "restore_tool" : "retire_tool", id: t.id }, (t.retired ? "Restored " : "Retired ") + t.name);
  }
  async function purgeTool(t) {
    const users = S.toolDetail && S.toolDetail.tool.id === t.id ? S.toolDetail.used_by || [] : [];
    const typed = await ask({ title: "Purge " + t.name + " permanently?", inputKind: "machine", text: (users.length ? "It is used by " + users.join(", ") + "; their calls to it will fail. " : "") + "All versions, metrics, and history are deleted. Type the Tool name to confirm.", input: true, placeholder: t.name, ok: "Purge", danger: true });
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

  // Slash commands (SPEC §10.2): one table drives the dispatcher, the `/`
  // palette and /help, so they can't drift apart.
  function resolveDocsTopic(topics, input) {
    const topic = (input || "").trim().toLowerCase();
    if (!topic) return { key: "gallery" };
    const names = Object.keys(topics);
    if (names.includes(topic)) return { key: topic };
    const matches = names.filter((name) => name.startsWith(topic));
    if (matches.length === 1) return { key: matches[0] };
    if (matches.length > 1) {
      return { error: `ambiguous docs topic "${topic}": matches ${matches.join(", ")}` };
    }
    return { error: `unknown docs topic "${topic}". Available topics: ${names.join(", ")}` };
  }

  const topics = { gallery: "docs/gallery.md", capabilities: "docs/capabilities.md", approvals: "docs/capabilities.md#are-you-asked", keys: "docs/keys.md", troubleshooting: "docs/troubleshooting.md", docker: "docs/docker.md", mcp: "docs/mcp.md", skills: "docs/skills.md", jobs: "docs/jobs.md", spec: "SPEC.md", usage: "docs/usage.md", cli: "docs/cli.md", architecture: "docs/architecture.md" };

  const COMMANDS = [
    { name: "new", args: "[TEMPLATE]", group: "Session", desc: "start a new Session, optionally from a template", run: (arg) => (arg ? newFromTemplate(arg) : newSession()) },
    { name: "template", args: "save|delete NAME [FIRST MESSAGE]", group: "Session", desc: "save this Session's setup as a template, or delete one", run: (arg) => templateCommand(arg) },
    { name: "export", args: "[json|md|html]", group: "Session", desc: "export this Session: a re-importable bundle, or a readable transcript", run: (arg) => (arg ? exportSession(S.current, arg) : pickExport(S.current)) },
    { name: "templates", group: "Session", desc: "list Session templates", run: () => templateCommand("") },
    { name: "sessions", group: "Session", desc: "list and switch Sessions", run: () => { setView("chat"); openSidebar(); $("search").focus(); } },
    { name: "fork", group: "Session", desc: "fork the current Session", run: () => forkSession(S.current) },
    { name: "search", args: "[QUERY]", group: "Session", desc: "search all Sessions", run: (arg) => { setView("chat"); openSidebar(); $("search").value = arg; return arg ? search(arg) : $("search").focus(); } },
    { name: "edit", group: "Session", desc: "edit your last message and retry from there", run: () => {
      const m = [...S.messages].reverse().find((x) => x.role === "user" && x.generated_by == null);
      return m ? beginEdit(m) : toast("No user message to edit.", true);
    } },
    { name: "usage", group: "Session", desc: "tokens and cost of this Session", run: async () => {
      if (!S.current) return note("open a session first", true);
      const r = await act({ type: "session_usage", session_id: S.current });
      if (!r) return;
      if (r.last_run) note("last Run: " + usageText(r.last_run));
      return note("session (" + r.runs + " Runs): " + usageText(r.total));
    } },
    { name: "compact", args: "[FOCUS]", group: "Session", desc: "summarise the Session now, keeping what FOCUS asks", run: async (arg) => {
      if (!S.current) return note("open a session first", true);
      if (await act({ type: "compact_session", session_id: S.current, focus: arg || null })) note("compacting: a summary will replace the history so far…");
    } },
    { name: "cd", args: "[PATH]", group: "Session", desc: "show or change this Session's working directory", run: async (arg) => {
      const s = S.sessions.find((x) => x.id === S.current);
      if (!S.current) return note("open a session first", true);
      if (!arg) return note("working directory: " + ((s && s.cwd) || "(the daemon's)"));
      const r = await act({ type: "set_session_cwd", id: S.current, cwd: arg });
      if (!r) return;
      const i = S.sessions.findIndex((x) => x.id === r.id);
      if (i >= 0) S.sessions[i] = r;
      renderStatus();
      note("working directory: " + r.cwd);
    } },
    { name: "rename", args: "[NAME|auto]", group: "Session", desc: "rename the Session; auto suggests a name from recent messages", run: (arg) => {
      const s = S.sessions.find((x) => x.id === S.current);
      if (!s) return toast("No current session.", true);
      if (arg === "auto") return suggestName(s);
      if (arg) return act({ type: "rename_session", id: s.id, name: arg });
      return renameSession(s);
    } },
    { name: "instructions", args: "[TEXT|clear]", group: "Session", desc: "standing instructions for this Session's agent", run: (arg) => {
      const s = S.sessions.find((x) => x.id === S.current);
      if (!s) return toast("No current session.", true);
      return editInstructions(s, arg === "clear" ? "" : arg || undefined);
    } },
    { name: "context", group: "Session", desc: "how full the context window is, and what fills it", run: () => showContext() },
    { name: "stop", group: "Run control", desc: "stop the current Run (Esc)", run: () => stop() },
    { name: "cancel", group: "Run control", desc: "cancel the stalled or pending Tool call", run: () => {
      const t = [...S.liveTools].reverse().find((x) => x.status === "running" && x.handle);
      return cancelTool(t && t.handle);
    } },
    { name: "resend", group: "Run control", desc: "re-send a stalled provider request", run: retryProviderRequest },
    { name: "retry", args: "[--model MODEL]", group: "Run control", desc: "re-run your last message, optionally on another model (the old answer stays in a fork)", run: (arg) => retryRun((arg || "").replace(/^--model\s*/, "").trim()) },
    { name: "test", args: "[MODEL]", group: "Settings", desc: "test a model: latency, streaming, reasoning, tool calls", run: async (arg) => {
      const s = S.sessions.find((x) => x.id === S.current);
      const model = (arg || "").trim() || (s && s.model) || (S.settings && S.settings.provider && S.settings.provider.model);
      if (!model) return note("usage: /test MODEL", true);
      note("testing " + model + "…");
      const t = await act({ type: "test_model", model }, null, 120000);
      if (t) note(modelTestText(t), !t.ok);
    } },
    { name: "routing", group: "Settings", desc: "why routing picked each model lately", run: async () => {
      const trigger = document.activeElement;
      const list = await act({ type: "routing_decisions" });
      if (!list) return;
      if (!list.length) return note("no routing decisions yet (provider.model = \"auto\" or Roles with candidates)");
      modal("Routing decisions", h("div", null, list.map((d) => h("section", null,
        h("h3", null, (d.kind || "") + " → " + d.picked), h("p", { class: "muted" }, relTime(d.at) + " · " + d.reason),
        h("table", { class: "table" }, h("tbody", null, (d.candidates || []).map((c) => h("tr", { class: c.model === d.picked ? "" : "muted" },
          h("td", null, (c.model === d.picked ? "▸ " : "") + c.model), h("td", { class: "num" }, c.samples + " samples"),
          h("td", { class: "num" }, c.good_rate != null ? uiFormat(c.good_rate, { style: "percent", maximumFractionDigits: 0 }) + " good" : c.score != null ? "score " + uiFixed(c.score, 2) : ""),
          h("td", { class: "num" }, c.avg_ms != null ? uiFixed(c.avg_ms / 1000, 1) + "s" : ""),
          h("td", null, c.qualified === false ? "below the quality gate" : "")))))))), { trigger });
    } },
    { name: "pull", args: "MODEL [--provider NAME] | cancel [PULL_ID]", group: "Settings", desc: "Ollama: download, attach to, or cancel a model pull", run: async (arg) => {
      const pullRequest = parsePullCommand(arg, S.pullId);
      if (pullRequest.error) return note(pullRequest.error, true);
      if (await requestPull(pullRequest)) note(pullRequest.type === "ollama_pull" ? "checking Ollama endpoint for " + pullRequest.model + "…" : "cancelling Ollama pull…");
    } },
    { name: "docs", args: "[TOPIC]", group: "App", desc: "open the guides: " + Object.keys(topics).join(", "), run: (arg) => {
      const base = "https://github.com/athiwatc/formless/blob/main/";
      const topic = resolveDocsTopic(topics, arg);
      if (topic.error) return note(topic.error, true);
      window.open(base + topics[topic.key], "_blank", "noopener");
    } },
    { name: "status", group: "Settings", desc: "the daemon: memory, CPU, database, running work", run: () => showStatus() },
    { name: "storage", group: "Settings", desc: "disk used by Sessions, artifacts, logs, backups", run: () => showStorage() },
    { name: "gc", group: "Settings", desc: "clean up: shows what it frees first", run: () => showGc() },
    { name: "summarise-branch", args: "SOURCE_SESSION", group: "Session", desc: "generate a branch summary for review, then insert one note here", run: summariseBranchCommand },
    { name: "compare", args: "SESSION_A SESSION_B", group: "Session", desc: "compare stored branch messages and recorded usage (no model call)", run: compareSessionsCommand },
    { name: "compare-models", args: "MODEL_A MODEL_B [PROMPT]", group: "Run control", desc: "the same prompt on two models, side by side with time and cost", run: compareModelsCommand },
    { name: "step", group: "Run control", desc: "step mode: pause before each Tool call", run: async () => {
      S.stepMode = !S.stepMode;
      if (await act({ type: "set_step_mode", session_id: S.current, on: S.stepMode })) note(S.stepMode ? "step mode on: the agent stops before each Tool call" : "step mode off");
    } },
    { name: "files", group: "Run control", desc: "the files the last Run changed, with diffs", run: () => showRunFiles() },
    { name: "rewind", args: "[PATH]", group: "Run control", desc: "put the last Run's file changes back (one file, or all)", run: async (arg) => {
      const path = (arg || "").trim();
      const x = await act({ type: "rewind_run", session_id: S.current, path: path && path !== "all" ? path : null });
      if (!x) return;
      note(x.restored.length ? "reverted " + x.restored.join(", ") : "nothing reverted");
      x.skipped.forEach((k) => note("not reverted: " + k.path + " (" + k.reason + ")", true));
    } },
    { name: "limit", args: "N|off", group: "Run control", desc: "cap the current (or next) Run at N steps: /limit 20, /limit off", run: async (arg) => {
      const n = (arg || "").trim();
      const max = /^(off|none|0)$/.test(n) ? null : parseInt(n, 10);
      if (max !== null && !(max > 0)) return note("usage: /limit N (steps) or /limit off", true);
      const r = await act({ type: "set_step_limit", session_id: S.current, max_steps: max });
      if (r) note(r.max_steps ? (r.active_run ? "this Run" : "the next Run") + " stops after " + r.max_steps + " steps (pauses: /resume continues)" : "no step limit");
    } },
    { name: "resume", group: "Run control", desc: "continue an interrupted or paused Run", run: async () => {
      await resumeRun();
    } },
    { name: "abandon", group: "Run control", desc: "drop an interrupted or paused Run", run: async () => {
      await abandonRun();
    } },
    { name: "agents", args: "[N]", group: "Run control", desc: "list sub-agents, or show one's transcript", run: async (arg) => {
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
    } },
    { name: "thinking", group: "Run control", desc: "show or hide model thinking", run: () => { setThinking(!S.showThinking); note("thinking display " + (S.showThinking ? "on" : "off")); } },
    { name: "tools", group: "Tools", desc: "open the Tools view", run: () => setView("tools") },
    { name: "metrics", group: "Tools", desc: "open the Metrics view", run: () => setView("metrics") },
    { name: "proposals", group: "Tools", desc: "open the prompt Proposals view", run: () => setView("proposals") },
    { name: "propose", group: "Tools", desc: "generate a measured prompt proposal", run: () => { act({ type: "generate_proposal" }); note("generating a measured proposal…"); } },
    { name: "settings", group: "Settings", desc: "open the Settings view", run: () => setView("settings") },
    { name: "model", args: "[ID]", group: "Settings", desc: "pick the model and effort (fuzzy search)", run: (arg) => (arg ? setSessionModel(arg, undefined) : openModelPicker()) },
    { name: "panes", group: "Settings", desc: "multi-pane layout (always on in the web)", run: () => note("The web layout always shows Sessions beside the chat.") },
    { name: "prompts", group: "Integrations", desc: "list MCP prompts", run: async () => {
      const list = (await act({ type: "list_mcp_prompts" })) || [];
      if (!list.length) return note("no MCP prompts available");
      for (const p of list) note("/mcp:" + p.server + ":" + p.name + " " + (p.arguments || []).map((a) => a.name.toUpperCase()).join(" ") + " — " + (p.description || ""));
    } },
    { name: "mcp:", args: "SERVER:PROMPT [ARGS]", group: "Integrations", desc: "send an MCP prompt as your message (/prompts lists them)", run: () => note("usage: /mcp:SERVER:PROMPT [ARGS] — /prompts lists them") },
    { name: "prompt", args: "SERVER:NAME [KEY=VALUE …]", group: "Integrations", desc: "load an MCP prompt into the composer", run: async (arg) => {
      const [target, ...pairs] = arg.split(/\s+/);
      const [server, name] = (target || "").split(":");
      if (!server || !name) return note("usage: /prompt SERVER:NAME [key=value …] (see /prompts)", true);
      const r = await act({ type: "get_mcp_prompt", server, name, arguments: Object.fromEntries(pairs.filter((p) => p.includes("=")).map((p) => p.split(/=(.*)/s).slice(0, 2))) });
      if (r) { $("input").value = r.text; saveDraft(S.current, r.text); autosize(); $("input").focus(); note("prompt loaded into the composer; edit it, then send"); }
    } },
    { name: "help", group: "App", desc: "all commands and keys (also ? on an empty input)", run: () => showHelp() },
    { name: "quit", group: "App", desc: "disconnect from the daemon", run: () => forget() },
  ];
  const ALIASES = { retry: "edit", "": "help" };
  const usage = (c) => "/" + c.name + (c.args ? " " + c.args : "");
  const needsArgs = (c) => !!c.args && !c.args.startsWith("[");
  function editDistance(a, b) {
    let prev = Array.from({ length: b.length + 1 }, (_, j) => j);
    for (let i = 0; i < a.length; i++) {
      const cur = [i + 1];
      for (let j = 0; j < b.length; j++) cur.push(Math.min(prev[j] + (a[i] === b[j] ? 0 : 1), prev[j + 1] + 1, cur[j] + 1));
      prev = cur;
    }
    return prev[b.length];
  }
  /** "did you mean": the closest command name, if close enough. */
  function closestCommand(word) {
    let best = null;
    for (const c of COMMANDS) {
      const d = editDistance(word, c.name);
      if (d <= Math.max(2, Math.floor(c.name.length / 3)) && (!best || d < best.d)) best = { d, name: c.name };
    }
    return best && best.name;
  }
  /** Commands matching what follows `/`, exact prefixes first. */
  function matchCommands(q) {
    if (!q) return COMMANDS.slice();
    return COMMANDS.map((c, i) => ({ c, i, m: fuzzyMatch(q, c.name) }))
      .filter((x) => x.m)
      .sort((a, b) => (b.c.name.startsWith(q) - a.c.name.startsWith(q)) || (b.m[0] - a.m[0]) || (a.i - b.i))
      .map((x) => x.c);
  }
  function showHelp() {
    if (document.querySelector(".modal.help")) return;
    const rows = (pairs) => h("div", { class: "help-grid" }, pairs.map(([k, d]) => [h("code", null, k), h("span", null, d)]));
    const keyName = (k) => (k === "ArrowDown" ? "↓" : k === "ArrowUp" ? "↑" : k);
    const sections = [...new Set(COMMANDS.map((c) => c.group))].map((g) =>
      h("section", null, h("h3", null, tr(g)), rows(COMMANDS.filter((c) => c.group === g).map((c) => [usage(c), tr(c.desc)]))));
    sections.push(h("section", null, h("h3", null, "Keys"), rows([
      ...SHORTCUTS.map((sc) => [sc.keys.map(keyName).join(" "), (sc.view ? "(" + sc.view + ") " : "") + sc.help]),
      ["Enter", "send, or steer the running Run"], ["Shift+Enter", "new line"], ["Ctrl/⌘+Enter", "queue for after the Run"],
      ["Esc", "close a dialog, or stop the Run"], ["/ in the composer", "command palette (Tab completes)"],
      ["@ in the composer", "reference a file"], ["y · o · n", "approvals: always · once · deny"],
    ])));
    const close = h("button", { class: "btn small primary", type: "button" }, "Close");
    const dialog = h("div", { class: "backdrop", role: "dialog", "aria-modal": "true", "aria-label": "Help" },
      h("div", { class: "modal help" }, h("h2", null, "Help"), h("div", { class: "help-body" }, sections),
        h("div", { class: "modal-actions" }, h("span", { class: "spacer" }), close)));
    const done = () => closeDialog(dialog);
    close.onclick = done;
    openDialog(dialog, { initialFocus: close, onDismiss: done });
  }
  function runFilesCaveat() {
    return "Untracked writes are possible: changes by programs the Run launched or MCP tools are not tracked.";
  }
  function runFilesText(files, untracked) {
    const changed = files.filter((f) => f.status !== "unchanged");
    let text = changed.length ? changed.length + " tracked path(s) changed: " + changed.slice(0, 5).map((f) => f.path.split(/[\\/]/).pop() + " (" + f.status + ")").join(", ") + (changed.length > 5 ? ", +" + (changed.length - 5) + " more" : "") + " · /files shows tracked changes; /rewind attempts safe restoration." : files.length ? "No current changes among tracked paths." : "No tracked path changes.";
    return text + (untracked ? " " + runFilesCaveat() : "");
  }
  // Hunk counts distinguish real +/- content from unified-diff file headers.
  function renderDiffLines(diff, tag) {
    let oldLeft = 0, newLeft = 0;
    return String(diff).split("\n").map((line) => {
      let cls = null;
      const hunk = /^@@ -(\d+)(?:,(\d+))? \+(\d+)(?:,(\d+))? @@/.exec(line);
      if (hunk) {
        oldLeft = Number(hunk[2] ?? 1);
        newLeft = Number(hunk[4] ?? 1);
        cls = "muted";
      } else if (oldLeft > 0 || newLeft > 0) {
        if (line[0] === "+") { newLeft = Math.max(0, newLeft - 1); cls = "add"; }
        else if (line[0] === "-") { oldLeft = Math.max(0, oldLeft - 1); cls = "del"; }
        else if (line[0] === " ") {
          oldLeft = Math.max(0, oldLeft - 1);
          newLeft = Math.max(0, newLeft - 1);
        }
      } else if (/^(?:\+\+\+|---)(?:\s|$)/.test(line) || line.startsWith("@@")) {
        cls = "muted";
      } else if (line[0] === "+") cls = "add";
      else if (line[0] === "-") cls = "del";
      return h(tag, { class: cls }, line + (tag === "span" ? "\n" : ""));
    });
  }
  // The files the last Run changed, with diffs and revert buttons (SPEC §5.7).
  async function showRunFiles() {
    const trigger = document.activeElement;
    if (!S.current) return note("open a session first", true);
    const target = currentRunTarget();
    const sid = target.session;
    const r = await act({ type: "run_files", session_id: sid });
    if (!r || !ensureRunTarget(target)) return;
    const close = h("button", { class: "btn small primary", type: "button" }, "Close");
    const rewind = async (path) => {
      const x = await act({ type: "rewind_run", session_id: sid, run_id: r.run_id, path: path || null });
      if (!x) return;
      if (x.restored.length) note("reverted " + x.restored.join(", "));
      x.skipped.forEach((k) => note("not reverted: " + k.path + " (" + k.reason + ")", true));
      done();
    };
    const mark = { created: "A", removed: "D", unchanged: "=" };
    const body = h("div", { class: "files-body" }, r.files.map((f) => h("div", { class: "run-file" },
      h("div", { class: "run-file-head" }, h("code", null, (mark[f.status] || "M") + " " + f.path),
        h("span", { class: "spacer" }),
        f.revertible && f.status !== "unchanged" ? h("button", { class: "btn small", type: "button", onclick: () => rewind(f.path) }, "Revert") : h("span", { class: "muted" }, f.revertible ? "" : "not revertible: " + (f.revert_reason || "snapshot unavailable"))),
      f.diff ? h("pre", { class: "diff" }, renderDiffLines(f.diff, "span")) : null)));
    const all = h("button", { class: "btn small", type: "button", onclick: async () => {
      const confirmed = await ask({ title: "Revert all tracked paths?", danger: true, ok: "Revert all",
        text: "Attempt safe restoration of every tracked path. Paths changed since the Run may be skipped." });
      if (confirmed && dialog.isConnected && ensureRunTarget(target)) await rewind("");
    } }, "Revert all");
    const dialog = h("div", { class: "backdrop", role: "dialog", "aria-modal": "true", "aria-label": "Files changed" },
      h("div", { class: "modal help" }, h("h2", null, "Paths tracked by the Run (" + r.files.length + ")"), r.untracked_writes_possible ? h("p", { class: "muted" }, runFilesCaveat()) : null, body,
        h("div", { class: "modal-actions" }, all, h("span", { class: "spacer" }), close)));
    const done = () => closeDialog(dialog);
    close.onclick = done;
    openDialog(dialog, { trigger, initialFocus: close, onDismiss: done });
  }
  // "What I did" under the last answer of a long Run (SPEC §5.7).
  function recapBlock(r) {
    const n = (k, what) => (k ? " · " + k + " " + what : "");
    const list = (title, items) => (items.length ? [h("div", { class: "muted" }, title), h("ul", null, items.map((i) => h("li", null, i)))] : []);
    return h("details", { class: "recap" },
      h("summary", null, tr("What I did") + " · " + r.steps + " steps · " + recordedDuration(r.duration_ms) + (r.status ? " · " + r.status : "") + (r.total_calls != null ? " · " + r.total_calls + " calls" : "") + n(r.unfinished_calls || 0, "unfinished calls") + n(r.built.length, "Tools changed") + n(r.files.length, "tracked paths") + (r.untracked_writes_possible ? " · " + runFilesCaveat() : "")),
      ...list("Used", r.used), ...list("Built", r.built), ...list("Tracked paths", r.files),
      r.untracked_writes_possible ? h("p", { class: "muted" }, runFilesCaveat()) : null,
      r.steered ? h("div", { class: "muted" }, "You steered it " + r.steered + " time(s).") : null);
  }
  async function showContext() {
    if (!S.current) return note("open a session first", true);
    const r = await act({ type: "context_usage", session_id: S.current });
    if (!r) return;
    (S.contextFill = S.contextFill || {})[S.current] = { used: r.used_tokens, size: r.context_tokens };
    renderStatus();
    const b = r.breakdown || {};
    const pct = r.context_tokens ? Math.floor(r.used_tokens * 100 / r.context_tokens) : 0;
    // Where the window size came from (#770).
    const source = { configured: "from provider.model_context_tokens", endpoint: "reported by the endpoint" }[r.context_source] || "provider.context_tokens (not reported by the endpoint)";
    note("context: " + r.used_tokens + " of " + r.context_tokens + " tokens (" + pct + "%" + (r.measured ? "" : ", estimated") + ") on " + r.model + " · window " + source);
    note("≈ system prompt " + b.system_prompt + " · Tool schemas " + b.tool_schemas + " · history " + b.history + " tokens · " + b.attachments + " attachment(s)");
  }
  function parsePullCommand(arg, activePullId) {
    const parts = (arg || "").trim().split(/\s+/).filter(Boolean);
    if (parts[0] === "cancel") {
      const pull_id = parts[1] || activePullId;
      if (parts.length > 2 || !pull_id) return { error: "usage: /pull cancel [PULL_ID]" };
      return { type: "cancel_ollama_pull", pull_id };
    }
    const model = parts[0];
    if (!model) return { error: "usage: /pull MODEL [--provider NAME]" };
    let provider;
    if (parts.length > 1) {
      if (parts.length !== 3 || parts[1] !== "--provider") return { error: "usage: /pull MODEL [--provider NAME]" };
      provider = parts[2];
    }
    return { type: "ollama_pull", model, provider };
  }

  function hasPullJobsFeature(response) {
    return Array.isArray(response && response.features) && response.features.includes("ollama_pull_jobs_v1");
  }

  async function requestPull(pullRequest) {
    const ping = await act({ type: "ping" });
    if (!ping) return null;
    if (!hasPullJobsFeature(ping)) {
      note("the running daemon does not support tracked Ollama pulls; restart it with this version", true);
      return null;
    }
    const response = await act(pullRequest);
    if (pullRequest.type === "ollama_pull" && response && response.pull_id) S.pullId = response.pull_id;
    return response;
  }

  async function slash(cmd) {
    // `/mcp:SERVER:PROMPT [ARGS]`: send the rendered MCP prompt (SPEC §7.9).
    const mp = /^mcp:([^:\s]+):(\S+)\s*(.*)$/s.exec(cmd);
    if (mp) {
      const words = mp[3].match(/"[^"]*"|'[^']*'|\S+/g) || [];
      const args = {}, positional = [];
      for (let w of words) {
        w = w.replace(/^["']|["']$/g, "");
        const eq = /^([A-Za-z_][\w-]*)=(.*)$/s.exec(w);
        if (eq) args[eq[1]] = eq[2]; else positional.push(w);
      }
      const r = await act({ type: "get_mcp_prompt", server: mp[1], name: mp[2], arguments: args, positional }, null, 60000);
      if (r && r.text && r.text.trim()) { $("input").value = r.text; saveDraft(S.current, r.text); autosize(); return submit(false); }
      return;
    }
    const [first, ...rest] = cmd.split(/\s+/);
    const name = ALIASES[first] !== undefined ? ALIASES[first] : first;
    const c = COMMANDS.find((x) => x.name === name);
    if (!c) {
      const near = closestCommand(name);
      return note("unknown command: /" + name + (near ? " — did you mean /" + near + "?" : " (/help lists them)"), true);
    }
    return c.run(rest.join(" "));
  }

  const attachmentDrafts = {};
  async function submit(queued) {
    if (currentTopicAttempt()) return;
    if (shareSendBlocked()) {
      note("Wait for shared files to finish attaching or resolve the failed files before sending.", true);
      return;
    }
    const input = $("input");
    const draftSnapshot = normalizeTextareaValue(input.value);
    const text = input.value.trim();
    if (!text) return;
    if (text.startsWith("/") && !S.editMessageId && !isSharedSlashDraft(S.storageScope, S.current, input.value)) {
      input.value = "";
      saveDraft(S.current, "");
      autosize();
      return slash(text.slice(1).trim());
    }
    const submitKey = [S.storageScope || "", text].join("\u0000");
    if (S.submitting.has(submitKey)) return;
    if (S.submitting.size) {
      note("A message is still being prepared; this draft remains in the composer.", true);
      return;
    }
    S.submitting.add(submitKey);
    try {
      if (!S.current) await newSession();
      if (!S.current) return;
      const sid = S.current;
      const scope = S.storageScope;
      const saveCurrentDraft = (value) => {
        if (scope !== S.storageScope || S.current !== sid) return false;
        return saveDraft(sid, value, scope);
      };
      const mode = queued ? "queue" : S.running.has(sid) ? "steer" : "send";
      const expectedRunId = mode === "steer" && S.liveRun && !S.liveRun.ended ? S.liveRun.id : null;
      const editMessageId = S.editMessageId;
      const needsForecast = !S.running.has(sid) && S.settings && S.settings.limits && S.settings.limits.forecast_confirm_usd != null;
      if (editMessageId) {
        if (needsForecast) {
          if (!S.daemonOnline) { note("Reconnect to approve the forecast before editing this message.", true); return; }
          try {
            if (!(await confirmForecast(sid, text, { edit_message_id: editMessageId }))) return;
          } catch (error) { note("forecast: " + error.message, true); return; }
        }
        if (scope !== S.storageScope) return;
        S.editMessageId = null;
        restoreDraft(sid);
        renderComposer();
        await act({ type: "edit_message", session_id: sid, message_id: editMessageId, content: text });
        return;
      }
      askNotifyPermission();
      closeMentions();
      const toolNames = new Set((S.tools || []).map((tool) => tool.name));
      const sessionKey = sessionCacheKey(scope, sid);
      const already = mentionAttached[sessionKey] || new Set();
      const fresh = [];
      let attachmentError = "";
      for (const path of mentionsIn(text)) {
        if (toolNames.has(path) || already.has(path)) continue;
        if (!S.daemonOnline || !S.channel) {
          attachmentError = "File mentions could not be attached while offline; review before sending.";
          continue;
        }
        try {
          const attachment = await request({ type: "attach_path", session_id: sid, path });
          if (scope !== S.storageScope) { saveCurrentDraft(text); return; }
          (mentionAttached[sessionKey] = mentionAttached[sessionKey] || new Set()).add(path);
          (S.chips[sid] = S.chips[sid] || []).push(attachment);
          fresh.push(attachment.name);
        } catch (error) {
          if (/^.+:\+?[0-9]+$/.test(path) || !/^no file/.test(error.message)) {
            note("cannot attach @" + path + ": " + error.message, true);
            if (!S.daemonOnline) attachmentError = "File mentions could not be attached while offline; review before sending.";
          }
        }
      }
      if (fresh.length) {
        await refreshAttachmentInventory(sid, scope);
        if (S.current === sid && scope === S.storageScope) { saveCurrentDraft(input.value); autosize(); renderChips(); }
        else { attachmentDrafts[sessionCacheKey(scope, sid)] = text; saveCurrentDraft(text); }
        note("This message attaches " + fresh.join(", ") + " (shown above the box; × removes one). Press Send again to send.");
        return;
      }
      let forecastPending = false;
      if (needsForecast) {
        if (!S.daemonOnline || !S.channel) forecastPending = true;
        else {
          try {
            if (!(await confirmForecast(sid, text))) {
              if (scope !== S.storageScope) saveCurrentDraft(text);
              else if (S.current !== sid) { attachmentDrafts[sessionCacheKey(scope, sid)] = text; saveCurrentDraft(text); }
              return;
            }
          } catch (error) {
            if (!S.daemonOnline) forecastPending = true;
            else { saveCurrentDraft(text); note("Forecast was not confirmed: " + error.message, true); return; }
          }
        }
      }
      if (scope !== S.storageScope) { saveCurrentDraft(text); return; }
      const inventory = await refreshAttachmentInventory(sid, scope);
      if (scope !== S.storageScope) { saveCurrentDraft(text); return; }
      let durableOutboxSupported = null;
      let capabilityError = null;
      if (S.daemonOnline && S.channel) {
        try {
          const ping = await request({ type: "ping" });
          durableOutboxSupported = Array.isArray(ping && ping.features) && ping.features.includes("durable_outbox_v1");
        } catch (error) { capabilityError = error; }
      }
      if (scope !== S.storageScope) { saveCurrentDraft(text); return; }
      if (durableOutboxSupported === false && mode === "send") {
        if (S.current !== sid) {
          saveCurrentDraft(input.value);
          note("The active Session changed while preparing this message; review it before sending with this daemon.", true);
          return;
        }
        if (!Array.isArray(inventory) || inventory.length) {
          saveCurrentDraft(input.value);
          note("This daemon cannot safely send a message with pending attachments; upgrade it before sending.", true);
          return;
        }
        if (S.running.has(sid)) {
          saveCurrentDraft(input.value);
          note("The Session started a Run while this message was being prepared; choose a send mode again.", true);
          return;
        }
        saveCurrentDraft(input.value);
        let legacyResult;
        try {
          legacyResult = await withOutboxLock(scope, async () => {
            if (scope !== S.storageScope || S.current !== sid) return { blocked: "The active Session or pairing changed; this message was not sent." };
            if (loadOutbox(scope).length) return { blocked: "The pairing outbox is not empty; review its saved messages before sending this legacy message." };
            if (S.running.has(sid) || input.value.trim() !== text) return { blocked: "The message or Session state changed while preparing it; review before sending." };
            let currentAttachments;
            try { currentAttachments = await fetchFreshAttachmentInventory(sid, scope); }
            catch (error) { return { inventoryError: error }; }
            if (scope !== S.storageScope || S.current !== sid) return { blocked: "The active Session or pairing changed; this message was not sent." };
            if (!Array.isArray(currentAttachments) || currentAttachments.length || JSON.stringify(currentAttachments) !== JSON.stringify(inventory)) {
              return { blocked: "Pending attachments changed while preparing this message; review it before sending." };
            }
            if (S.running.has(sid) || input.value.trim() !== text) return { blocked: "The message or Session state changed while preparing it; review before sending." };
            try {
              await request({
                type: "send_message",
                session_id: sid,
                content: text,
                mode: "send",
                client_msg_id: crypto.randomUUID ? crypto.randomUUID() : String(Date.now()) + Math.random().toString(16).slice(2),
              });
              return { sent: true };
            } catch (error) { return { requestError: error }; }
          });
        } catch (error) { legacyResult = { lockError: error }; }
        if (!legacyResult) {
          note("Could not coordinate with other tabs; this legacy message was not sent.", true);
          return;
        }
        if (legacyResult.inventoryError) {
          note("Could not verify pending attachments for the legacy send: " + legacyResult.inventoryError.message, true);
          return;
        }
        if (legacyResult.blocked) {
          note(legacyResult.blocked, true);
          return;
        }
        if (legacyResult.lockError) {
          note("Could not coordinate with other tabs; this legacy message was not sent: " + legacyResult.lockError.message, true);
          return;
        }
        if (legacyResult.requestError) {
          const error = legacyResult.requestError;
          note(error.daemonResponse
            ? "Legacy send failed: " + error.message
            : "Legacy send outcome is unknown; check the Session before retrying: " + error.message, true);
          return;
        }
        if (legacyResult.sent && scope === S.storageScope && S.current === sid) {
          if (input.value.trim() === text) { input.value = ""; saveCurrentDraft(""); autosize(); }
          else saveCurrentDraft(input.value);
        }
        return;
      }
      let blockedReason = attachmentError;
      if (mode === "steer" && !expectedRunId) blockedReason = "The active Run could not be identified; this Steer was not sent.";
      if (mode === "queue" && inventory && inventory.length) blockedReason = "Queue mode cannot preserve pending attachments; this message was not sent.";
      if (durableOutboxSupported === false) blockedReason = "This daemon does not support the durable outbox; upgrade it, manually copy this message text, discard the saved item, and resend it.";
      if (capabilityError && capabilityError.daemonResponse) blockedReason = "Could not verify durable outbox support: " + capabilityError.message;
      const item = await enqueueOutbox(sid, text, mode, {
        expectedAttachments: inventory,
        expectedRunId,
        forecastPending,
        blockedReason,
        queuedReason: capabilityError && !capabilityError.daemonResponse
          ? "Could not verify durable outbox support; this message will wait until the daemon can be checked."
          : "",
        storageScope: scope,
      });
      if (!item) {
        note(hasOutboxLock()
          ? "Could not save this message to the outbox; it is still in the composer."
          : "This browser cannot safely save messages because the Web Locks API is unavailable. Open this page in a current browser in a secure context (HTTPS or localhost); the message remains in the composer.", true);
        return;
      }
      const sourceIsCurrent = S.current === sid && scope === S.storageScope;
      const fallbackKey = sessionCacheKey(scope, sid);
      const hasFallback = S.unpersistedDrafts && S.unpersistedDrafts.has(fallbackKey);
      const fallback = hasFallback ? S.unpersistedDrafts.get(fallbackKey) : null;
      const shared = prefs.get(draftStorageKey(scope, sid), null);
      const composerDraft = normalizeTextareaValue(input.value);
      const noConflictingDraft = (!hasFallback || normalizeTextareaValue(fallback) === draftSnapshot)
        && (typeof shared !== "string" || normalizeTextareaValue(shared) === draftSnapshot);
      if (sourceIsCurrent && composerDraft !== draftSnapshot && noConflictingDraft) {
        saveCurrentDraft(input.value);
      }
      const latestSourceDraft = clearSubmittedDraft(scope, sid, draftSnapshot);
      if (sourceIsCurrent && normalizeTextareaValue(input.value) === draftSnapshot) {
        const latest = normalizeTextareaValue(latestSourceDraft);
        input.value = latest === draftSnapshot ? "" : latest;
        autosize();
      }
      if (item.status === "failed") { note(item.error, true); return; }
      return scope === S.storageScope ? flushOutbox() : undefined;
    } finally {
      S.submitting.delete(submitKey);
    }
  }
  function stop() {
    if (S.topicsAvailable && currentTopicAttempt()) return;
    if (S.current && isRunning()) act({ type: "interrupt_run", session_id: S.current });
  }
  let searchSeq = 0;
  async function search(q) {
    const seq = ++searchSeq;
    q = q.trim();
    if (!q) { renderSearch(null); return; }
    const hits = await act({ type: "search_sessions", query: q, limit: 50 });
    if (seq === searchSeq && hits) renderSearch(hits, q);
  }

  function autosize() {
    const t = $("input");
    t.style.height = "auto";
    const maxHeight = parseFloat(getComputedStyle(t).maxHeight);
    t.style.height = Math.min(t.scrollHeight, Number.isFinite(maxHeight) ? maxHeight : t.scrollHeight) + "px";
  }

  // Read CSS visibility so viewport and container-query layouts agree.
  let sidebarLastFocus = null;
  function sidebarVisible(sidebar) {
    return !!(sidebar.getClientRects().length
      && !sidebar.closest(".hidden, [hidden]") && getComputedStyle(sidebar).visibility !== "hidden");
  }
  function syncSidebar() {
    const sidebar = $("sidebar"), menu = $("menu-btn");
    const visible = sidebarVisible(sidebar);
    menu.setAttribute("aria-expanded", String(visible));
    syncRunningExposure();
    const focused = document.activeElement;
    const previous = focused === document.body ? sidebarLastFocus : focused;
    if (!focusVisible(sidebar) && sidebar.contains(previous)) focusReturn(menu);
    else if (!focusVisible(menu) && previous === menu) focusReturn(visible ? $("new-session") : null);
    syncRunningExposure(visible);
  }
  function setSidebar(open, { focus = false } = {}) {
    $("sidebar").classList.toggle("open", open);
    syncSidebar();
    if (open && focus && getComputedStyle($("sidebar")).position === "absolute" && focusVisible($("new-session"))) {
      $("new-session").focus({ preventScroll: true });
    }
  }
  function openSidebar(options) { setSidebar(true, options); }
  function closeSidebar() { setSidebar(false); }
  document.addEventListener("focusin", () => { sidebarLastFocus = document.activeElement; });
  const sidebarResize = new ResizeObserver(syncSidebar);
  for (const element of [$("app"), $("sidebar"), $("menu-btn")]) sidebarResize.observe(element);

  // ============================================================== events
  let toolRefresh = null;
  // ------------------------------------------------------ notifications
  // While the tab is hidden, Runs that finish (after notify_min_secs),
  // fail or stall, and approvals or questions, raise a browser
  // notification (tui.notify = desktop) and count in the tab title.
  const runStarts = new Map();
  let unseen = 0;
  const baseTitle = document.title;
  function notifyMode() {
    return (S.settings && S.settings.tui && S.settings.tui.notify) || "desktop";
  }
  async function openNotificationSession(session) {
    if (typeof session !== "string" || !session) return;
    if (!S.daemonOnline || (!S.local && !S.channel)) {
      S.linkedSession = session;
      return;
    }
    if (!S.sessions.some((item) => item.id === session)) await refreshSessions();
    if (S.sessions.some((item) => item.id === session)) await openSession(session);
    else note("That Session link points to a Session that no longer exists.", true);
  }
  async function showChatNotification(title, body, kind, session) {
    const options = {
      body, tag: "formless:" + encodeURIComponent(session || "") + ":" + kind,
      renotify: true, data: { session },
    };
    try {
      const registration = await navigator.serviceWorker?.getRegistration();
      if (registration?.active) {
        // The worker can identify the sending tab and use the mobile
        // notification API even after this page is backgrounded.
        registration.active.postMessage({ type: "formless-chat-notification", title, options });
        return;
      }
    } catch (_) { /* The page API is available on some browsers without a worker. */ }
    try {
      const notification = new Notification(title, options);
      notification.onclick = () => {
        notification.close();
        window.focus();
        openNotificationSession(session).catch(() => {});
      };
    } catch (_) { /* Notifications are unavailable in this browser. */ }
  }
  function alertUser(title, body, kind, session) {
    const mode = notifyMode();
    if (!document.hidden || mode === "off") return;
    unseen++;
    document.title = "(" + unseen + ") " + baseTitle;
    if (mode === "desktop" && "Notification" in window && Notification.permission === "granted") {
      void showChatNotification(title, body, kind, session);
    }
  }
  document.addEventListener("visibilitychange", () => {
    syncRunningExposure();
    if (!document.hidden) { unseen = 0; document.title = baseTitle; }
  });
  /** Ask once, from a user gesture (sending a message). */
  function askNotifyPermission() {
    if (notifyMode() === "desktop" && "Notification" in window && Notification.permission === "default") {
      Notification.requestPermission().catch(() => {});
    }
  }
  function sessionName(id) {
    const s = S.sessions.find((x) => x.id === id);
    return (s && s.name) || "Session";
  }
  function notifyFor(ev) {
    switch (ev.type) {
      case "run_started": runStarts.set(ev.session_id, Date.now()); return;
      case "run_finished": {
        const took = Date.now() - (runStarts.get(ev.session_id) || Date.now());
        runStarts.delete(ev.session_id);
        const min = (S.settings && S.settings.tui && S.settings.tui.notify_min_secs) || 20;
        if (took >= min * 1000 && ev.status !== "interrupted") alertUser("Run " + ev.status, sessionName(ev.session_id) + " · " + recordedDuration(took), ev.type, ev.session_id);
        return;
      }
      case "run_paused": runStarts.delete(ev.session_id); return alertUser("Run paused", sessionName(ev.session_id) + ": " + truncate((ev.explained && ev.explained.message) || ev.error || "", 120), ev.type, ev.session_id);
      case "provider_stalled": return alertUser("Run stalled", sessionName(ev.session_id) + ": no reply for " + ev.idle_secs + "s", ev.type, ev.session_id);
      case "approval_requested": return alertUser("Approval needed", sessionName(ev.session_id) + ": " + (ev.tool_name || ""), ev.type, ev.session_id);
      case "harness_permission_requested": return alertUser("Harness permission needed", sessionName(ev.session_id) + ": " + (ev.title || ""), ev.type, ev.session_id);
      case "user_question": {
        const q = (ev.questions && ev.questions[0] && ev.questions[0].question) || ev.question || "";
        return alertUser("Question for you", sessionName(ev.session_id) + ": " + truncate(q, 120), ev.type, ev.session_id);
      }
    }
  }

  function reconcileDropped(ev) {
    const kinds = new Set(ev.kinds || ["message_delta"]);
    const id = ev.session_id;
    const here = id === S.current;
    note((id && !here ? "Session " + sessionName(id) + ": " : "") + "live text, reasoning or progress updates were skipped; final answers remain in the transcript");
    if (kinds.has("run_usage") && id) {
      delete (S.usage || {})[id];
      renderStatus();
      const pending = S.gapUsage || (S.gapUsage = new Set());
      if (!pending.has(id)) {
        pending.add(id);
        request({ type: "session_usage", session_id: id }).then((r) => {
          // A streamed update received during the request is fresher.
          if (r.last_run && !(S.usage || {})[id]) (S.usage || (S.usage = {}))[id] = r.last_run;
          renderStatus();
        }).catch(() => note("usage refresh failed; /usage retries recorded usage"))
          .finally(() => pending.delete(id));
      }
    }
    if (here && (kinds.has("tool_progress") || kinds.has("invocation_activity"))) {
      for (const tool of S.liveTools || []) if (tool.status === "running") tool.progress = { fraction: null, label: "progress updates skipped" };
      renderLive();
    }
    if (kinds.has("invocation_activity")) refreshRunning();
    if (kinds.has("model_pull_progress")) setActivity("download progress unavailable for " + (ev.model || "model") + "; /models checks installed models");
  }

  function handlePullProgress(ev) {
    if (ev.done) {
      if (S.pullId === ev.pull_id) S.pullId = null;
      if (S.pullActivity && S.pullActivity.pull_id === ev.pull_id) {
        const label = S.pullActivity.label;
        S.pullActivity = null;
        if (S.activity && S.activity.label === label) setActivity(null);
        else renderStatus();
      }
      if (ev.error) {
        if (ev.status === "incomplete") {
          const provider = ev.provider && ev.provider !== "default" ? " --provider " + ev.provider : "";
          const resume = "/pull " + ev.model + provider;
          const error = ev.error.includes("partial download can resume with") ? ev.error : ev.error + "; rerun `" + resume + "` to resume";
          note("pull incomplete: " + error, true);
        } else note("could not pull " + ev.model + ": " + ev.error, true);
      } else if (ev.status === "success") note("pulled " + ev.model + "; pick it with /model");
    } else {
      const progress = ev.total ? " " + Math.floor((ev.completed || 0) * 100 / ev.total) + "%" : "";
      const expected = ev.expected_size ? "; expected " + ev.expected_size + " bytes" : "";
      const warning = ev.warning ? "; " + ev.warning : "";
      const label = "pulling " + ev.model + ": " + ev.status + progress + expected + warning;
      S.pullActivity = { pull_id: ev.pull_id, label, since: Date.now() };
      setActivity(label);
    }
  }

  function onEvent(ev) {
    branchSummaryEvent(ev);
    if (["settings_changed", "registry_changed", "session_updated", "messages_edited", "compaction_notice", "tool_updated", "tool_deleted", "message_appended", "message_complete", "run_started", "run_finished", "run_paused"].includes(ev.type)) {
      forecastRevision++;
      scheduleForecast();
    }
    if (S.sessionSnapshot && ["run_started", "run_finished", "run_paused", "run_recap", "plan_updated", "approval_requested", "approval_answered", "approval_closed", "approval_summary", "promotion_approval_requested", "promotion_approval_resolved", "user_question", "user_question_closed", "harness_permission_requested", "harness_permission_closed"].includes(ev.type)) {
      S.sessionSnapshot.events.push(ev);
    }
    notifyFor(ev);
    const here = ev.session_id === S.current;
    switch (ev.type) {
      case "deltas_dropped":
        reconcileDropped(ev);
        break;
      case "run_started":
        S.running.add(ev.session_id);
        if (here) { S.unfinished = null; renderRunBanner(); S.runTools = null; renderRunTools(); S.rated = null; S.recap = null; S.recapRunId = null; }
        if (here) {
          announcedRun = { session: ev.session_id, id: ev.run_id, afterSeq: Math.max(-1, ...S.messages.map((message) => message.seq ?? -1)) };
          clearLiveText(); S.liveTools = (S.liveTools || []).filter(t => t.status === "running" && t.handle); S.liveRun = { id: ev.run_id, ended: false }; setActivity("waiting for model");
          announceChat("Run started", ev.session_id, true);
        }
        break;
      case "run_recap":
        if (here && (!S.liveRun || S.liveRun.id === ev.run_id)) {
          if (S.recapRunId !== ev.run_id || JSON.stringify(S.recap) !== JSON.stringify(ev.recap)) { S.recap = ev.recap; S.recapRunId = ev.run_id; scheduleEventRender({ history: true, newContent: true }); }
        }
        break;
      case "run_files_changed":
        if (here) {
          note(runFilesText(ev.files, ev.untracked_writes_possible));
        }
        break;
      case "run_tool_changes":
        if (here) { S.runTools = ev.changes; renderRunTools(); }
        break;
      case "message_appended":
      case "message_complete":
        // A persisted assistant step (e.g. text before a Tool call) replaces
        // the live stream, so the next step starts fresh.
        if (ev.message.role === "assistant") commitReadingStream(ev.session_id, ev.message);
        if (here && (ev.type === "message_complete" || ev.message.role === "assistant")) { clearLiveText(); }
        if (here) {
          const index = S.messages.findIndex((m) => m.id === ev.message.id);
          if (index < 0) S.messages.push(ev.message);
          else if (ev.type === "message_complete") S.messages[index] = ev.message;
          if (index < 0 || ev.type === "message_complete") scheduleEventRender({ history: true, newContent: true });
        }
        break;
      case "message_delta":
        if (here) { appendLiveText("streaming", ev.delta); if (!S.activity || S.activity.label !== "responding") setActivity("responding"); else scheduleEventRender({ live: true }); }
        break;
      case "provider_stalled":
        if (here) { setActivity("provider stalled"); note("no data from the provider for " + ev.idle_secs + "s; /resend re-sends the request", true); }
        break;
      case "model_pull_progress":
        handlePullProgress(ev);
        return;
      case "comparison_ready":
        if (here) {
          const cols = ev.results.map((r) => h("div", { class: "compare-col" },
            h("h3", null, r.model),
            h("div", { class: "muted" }, r.status + " · " + uiFixed(r.duration_ms / 1000, 1) + "s · " + r.tokens + " tokens · " + r.steps + " steps" + (r.cost != null ? " · " + uiMoney(r.cost, 4) : "")),
            h("div", { class: "compare-answer" }, markdown(r.answer || "(no answer)")),
            h("button", { class: "btn small", type: "button", onclick: (e) => { closeDialog(e.currentTarget.closest(".backdrop")); openSession(r.session_id); } }, "Continue in this Session")));
          modal("Compare: " + ev.results.map((r) => r.model).join(" vs "), h("div", { class: "compare" }, cols));
        }
        break;
      case "step_pending":
        if (here) { S.step = ev; renderStep(); }
        break;
      case "step_resolved":
        if (here && S.step && S.step.step_id === ev.step_id) { S.step = null; renderStep(); }
        break;
      case "plan_updated":
        if (here) {
          const state = ev.plan_state || null;
          const older = S.planState && S.planState.updated_at && state && state.updated_at && state.updated_at < S.planState.updated_at;
          const wrongRun = state && !state.previous_task && ["live", "resumed"].includes(state.origin) && state.run_id && S.liveRun && state.run_id !== S.liveRun.id;
          if (!older && !wrongRun) { S.plan = ev.items || []; S.planState = state; renderPlan(); }
        }
        break;
      case "queue_changed":
        if (here) { S.queued = ev.queued || []; renderQueue(); }
        break;
      case "provider_retrying":
        if (here) {
          S.activity = { label: ev.reason, since: Date.now(), retryAt: Date.now() + ev.wait_secs * 1000, count: "(" + ev.attempt + "/" + ev.max_attempts + ")" };
          scheduleEventRender({ live: true });
        }
        break;
      case "stream_restarted":
        if (here) { clearLiveText(); note("provider stream dropped; retrying"); scheduleEventRender({ live: true }); }
        break;
      case "reasoning_delta":
        if (here) { appendLiveText("thinking", ev.delta); if (!S.activity || S.activity.label !== "thinking") setActivity("thinking"); else scheduleEventRender({ live: true }); }
        break;
      case "run_finished":
      case "run_paused":
        if (here && S.liveRun && S.liveRun.id !== ev.run_id) break;
        S.running.delete(ev.session_id);
        if (here) {
          if (S.planState && S.planState.run_id === ev.run_id) S.planState = { ...S.planState, run_status: ev.type === "run_paused" ? "paused" : ev.status };
          renderPlan();
          // Preserve a paused run's explanation only while it can resume.
          // Topic orchestration can finalize the same run as abandoned.
          if (ev.type === "run_paused") S.unfinished = { run_id: ev.run_id, status: "paused", error: ev.error, explained: ev.explained };
          else if (["paused", "interrupted"].includes(ev.status)) S.unfinished = {
            ...(S.unfinished?.run_id === ev.run_id ? S.unfinished : {}), run_id: ev.run_id, status: ev.status };
          else S.unfinished = null;
          renderRunBanner();
        }
        if (here) {
          if (ev.type === "run_finished" && ev.status === "completed" && lastAnnouncedFinishedRun !== ev.run_id) {
            const answer = announcedRun?.session === ev.session_id && announcedRun.id === ev.run_id
              ? completedRunAnswer(S.messages, announcedRun.afterSeq) : "";
            announceChat("Answer finished" + (answer ? ". " + answer : ""), ev.session_id, true);
            lastAnnouncedFinishedRun = ev.run_id;
          }
          if (announcedRun?.session === ev.session_id && announcedRun.id === ev.run_id) announcedRun = null;
          clearLiveText();
          const focused = focusedLiveTools(S.liveTools || []);
          S.liveTools = (S.liveTools || []).filter(t => t.status === "running" && t.handle || focused.has(t)); S.liveRun = { id: ev.run_id, ended: true };
          setActivity(null);
          if (ev.type === "run_paused") note("run paused: " + ((ev.explained && ev.explained.message) || ev.error), true);
          else if (ev.status === "abandoned") note("This run has ended; it cannot be resumed.");
          else if (ev.status !== "completed") note("run " + ev.status, ev.status !== "interrupted");
          scheduleEventRender({ history: true });
        }
        break;
      case "tool_call_started":
        if (here && S.liveRun && (S.liveRun.id !== ev.run_id || S.liveRun.ended) && !(ev.invocation_id != null && runningTool(null, ev.invocation_id))) break;
        if (here && ["create_tool", "create_module"].includes(ev.tool_name)) {
          try {
            const args = JSON.parse(ev.arguments);
            const connection = [S.ws, S.conn, S.channel];
            request({ type: "preview_description", kind: ev.tool_name === "create_module" ? "module" : "tool", name: args.name || "", description: args.description || "" }).then((preview) => {
              if (connection[0] !== S.ws || connection[1] !== S.conn || connection[2] !== S.channel || S.current !== ev.session_id) return;
              if (preview.after.flags.length) note("Description preview: " + preview.name + " · " + preview.after.flags.join("; ") + "\nThird-party metadata: " + JSON.stringify(preview.after.text));
            }).catch(() => {});
          } catch (_) {}
        }
        if (here) {
          announceChat("Running " + ev.tool_name, ev.session_id);
          const existing = ev.invocation_id != null && S.liveTools.find(t => t.invocationId === ev.invocation_id);
          if (existing) {
            if (existing.status === "running" && existing.metadataPending) Object.assign(existing, { metadataPending: false, name: ev.tool_name, version: ev.version, arguments: ev.arguments, depth: ev.depth || 0 });
          } else S.liveTools.push({ metadataPending: false, invocationId: ev.invocation_id ?? null, name: ev.tool_name, version: ev.version, arguments: ev.arguments, status: "running", started: Date.now(), depth: ev.depth || 0 });
          setActivity("running " + ev.tool_name);
        }
        break;
      case "tool_progress":
        if (here && (!S.liveRun || S.liveRun.id === ev.run_id && !S.liveRun.ended || ev.invocation_id != null && runningTool(null, ev.invocation_id))) {
          const t = runningTool(ev.tool_name, ev.invocation_id, ev.depth || 0);
          if (t) { t.progress = { fraction: ev.fraction, label: ev.label }; scheduleEventRender({ live: true }); }
        }
        break;
      case "tool_call_finished":
        if (S.invocations && S.invocations.length) refreshRunning();
        if (here) {
          const t = runningTool(ev.tool_name, ev.invocation_id);
          if (t) { t.status = ev.ok ? "ok" : "failed"; t.elapsedMs = ev.elapsed_ms; }
          if (!ev.ok) note(ev.tool_name + " failed after " + ev.elapsed_ms + "ms", true);
          setActivity(isRunning() ? "waiting for model" : null);
        }
        break;
      case "user_question":
        if (here) setActivity("awaiting your answer");
        queueQuestion(ev);
        break;
      case "run_usage":
        (S.usage = S.usage || {})[ev.session_id] = ev.usage;
        if (ev.usage.context_tokens > 0) (S.contextFill = S.contextFill || {})[ev.session_id] = { used: ev.usage.context_used, size: ev.usage.context_tokens };
        if (here) scheduleEventRender({ status: true });
        break;
      case "session_clients":
        if (ev.session_id === S.current) {
          S.others = (ev.clients || []).filter((c) => c.conn !== S.conn);
          scheduleEventRender({ status: true });
        }
        break;
      case "approval_help_seen":
        S.approvalHelpSeen = true;
        renderApproval();
        break;
      case "approval_closed": {
        const before = S.approvals.length;
        S.approvals = S.approvals.filter(a => !(a.kind === "tool" && a.id === ev.approval_id));
        if (S.approvals.length < before) { note("Approval closed without a user decision."); renderApproval(); scheduleEventRender({ status: true }); }
        break;
      }
      case "approval_answered": {
        // Answered in another client: close ours and say where.
        const before = S.approvals.length;
        S.approvals = S.approvals.filter((a) => !(a.kind === "tool" && a.id === ev.approval_id));
        if (S.approvals.length < before) {
          note("approval " + (ev.approved ? "granted" : "denied") + " in " + (ev.by || "another client"));
          renderApproval();
          scheduleEventRender({ status: true });
        }
        break;
      }
      case "user_question_closed":
        // Answered elsewhere (or the Run stopped): close our prompt.
        if (ev.answered_by) note("Question answered by " + (ev.answered_by === "model" ? "Formless; see the answer and basis in Topic Chat." : "the user."));
        removeQuestion(ev.question_id);
        break;
      case "harness_permission_closed":
        if (ev.by?.startsWith("rule:")) note("Harness permission answered by a topic rule.");
        S.approvals = S.approvals.filter(item => item.kind !== "harness" || item.id !== ev.permission_id);
        renderApproval(); scheduleEventRender({ status: true });
        break;
      case "harness_permission_requested":
        addApproval(ev);
        if (here) announceChat("Approval needed", ev.session_id, true);
        renderApproval(); scheduleEventRender({ status: true });
        break;
      case "approval_summary": {
        const req = S.approvals.find((a) => a.id === ev.approval_id);
        if (req) { req.summary = ev.summary; renderApproval(); }
        break;
      }
      case "approval_requested":
        addApproval(ev);
        if (here) { setActivity("awaiting your approval"); announceChat("Approval needed", ev.session_id, true); }
        renderApproval();
        break;
      case "promotion_approval_requested":
        addApproval(ev);
        if (here) announceChat("Approval needed", ev.session_id, true);
        renderApproval();
        break;
      case "promotion_approval_resolved":
        S.approvals = S.approvals.filter((a) => a.kind !== "promotion" || a.id !== ev.promotion_id);
        renderApproval();
        scheduleEventRender({ status: true });
        break;
      case "session_updated": {
        const i = S.sessions.findIndex((s) => s.id === ev.session.id);
        if (i >= 0) S.sessions[i] = ev.session; else S.sessions.unshift(ev.session);
        renderSessions();
        scheduleEventRender({ composer: true });
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
        if (here) {
          if (ev.metadata_only === true) {
            return refreshEditedMessages(S.current);
          }
          openSession(S.current);
        }
        break;
      case "tool_updated": {
        refreshDescriptionReviews();
        const i = S.tools.findIndex((t) => t.id === ev.tool.id);
        if (i >= 0) S.tools[i] = ev.tool; else S.tools.push(ev.tool);
        if (ev.tool.id === S.selectedTool || ev.tool.id === S.metricTool) {
          clearTimeout(toolRefresh);
          toolRefresh = setTimeout(() => { if (S.selectedTool === ev.tool.id) selectTool(ev.tool.id, true); }, 300);
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
        refreshDescriptionReviews();
        const i = S.modules.findIndex((m) => m.id === ev.module.id);
        if (i >= 0) S.modules[i] = ev.module; else S.modules.push(ev.module);
        if (S.moduleDetail && S.moduleDetail.module.id === ev.module.id && !S.moduleEditing) openModule(ev.module.id);
        else renderView();
        break;
      }
      case "registry_changed": {
        refreshDescriptionReviews();
        // Only daemons advertising a change identity can undo this toast safely.
        const c = ev.change;
        toast(c.author + " " + (c.action === "rolled_back" ? "rolled " + c.name + " back to v" + c.to_version : c.action + " " + c.name + (c.action === "retired" || c.action === "restored" ? "" : " v" + c.to_version)), false,
          c.change_id ? { label: "Undo", ms: 10000, run: async () => {
            const r = await act({ type: "undo_registry_change_if_current", change_id: c.change_id });
            if (r) toast("Undone: " + r.undone);
          } } : null);
        break;
      }
      case "module_deleted":
        S.modules = S.modules.filter((m) => m.id !== ev.id);
        if (S.moduleDetail && S.moduleDetail.module.id === ev.id) S.moduleDetail = null;
        renderView();
        break;
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
        if (here && S.liveRun && (S.liveRun.id !== ev.run_id || S.liveRun.ended) && !(ev.invocation_id != null && runningTool(null, ev.invocation_id))) break;
        if (here && typeof ev.handle === "string" && ev.handle && typeof ev.invocation_id === "string" && ev.invocation_id && !S.liveTools.some(t => t.invocationId === ev.invocation_id) && S.liveTools.filter(t => t.metadataPending).length < 128) {
          S.liveTools.push({ metadataPending: true, invocationId: ev.invocation_id, name: ev.tool_name, version: 0, arguments: "", depth: 0, status: "running", started: Date.now(), handle: ev.handle });
        }
        const t = here && runningTool(ev.tool_name, ev.invocation_id, null, ev.handle);
        if (t) t.handle = ev.handle;
        note(ev.tool_name + " continues in the background (" + ev.handle + ")");
        break;
      }
      case "invocation_stalled": {
        refreshRunning();
        const t = here && runningTool(ev.tool_name, ev.invocation_id, null, ev.handle);
        if (t) {
          t.handle = ev.handle;
          t.stalledSecs = ev.silent_secs;
          if (ev.last_fraction != null || ev.last_label != null) t.progress = { fraction: ev.last_fraction, label: ev.last_label || "" };
          note(ev.tool_name + ": no advancing progress or activity for " + ev.silent_secs + "s" + progressDiagnostic(ev) + "; /cancel stops just this tool", true);
          scheduleEventRender({ live: true });
        }
        break;
      }
      case "settings_changed":
        refreshSettings();
        break;
      case "mcp_servers_changed":
        requestSettingsRefresh([...settingsBackground, "mcpServers", "harness", "audit"]);
        break;
      case "skills_changed":
        requestSettingsRefresh([...settingsBackground, "skills", "harness", "audit"]);
        break;
      case "memory_updated":
        requestSettingsRefresh([...settingsBackground, "memories", "harness", "audit"]);
        break;
      case "jobs_changed":
        requestSettingsRefresh([...settingsBackground, "jobs", "harness", "audit"]);
        break;
      case "subagent_started":
        if (here) {
          note("sub-agent started: " + truncate(ev.subagent.task, 80));
          // A node under the Tool that called agents.run (the latest running one).
          const parent = [...S.liveTools].reverse().find((t) => t.status === "running");
          S.liveTools.push({ name: "sub-agent" + (ev.subagent.model ? " (" + ev.subagent.model + ")" : "") + ": " + truncate(ev.subagent.task, 60),
            subagent: ev.subagent.id, arguments: "", status: "running", started: Date.now(), depth: parent ? (parent.depth || 0) + 1 : 0 });
          scheduleEventRender({ live: true });
        }
        break;
      case "subagent_step":
        if (here) {
          const node = S.liveTools.find((t) => t.subagent === ev.subagent_id);
          if (node) node.step = "step " + ev.step + ": " + (ev.tool_calls.length ? ev.tool_calls.join(", ") : "answering");
          if (isRunning()) setActivity("sub-agent " + (node ? node.step : "step " + ev.step));
          scheduleEventRender({ live: true });
        }
        break;
      case "subagent_finished":
        if (here) {
          note("sub-agent " + ev.subagent.status + " after " + ev.subagent.steps + " step(s) · click it to read its transcript", ev.subagent.status !== "completed");
          const node = S.liveTools.find((t) => t.subagent === ev.subagent.id);
          if (node) { node.status = ev.subagent.status === "completed" ? "ok" : "failed"; node.elapsedMs = Date.now() - node.started; }
          scheduleEventRender({ live: true });
        }
        break;
      case "invocation_activity": {
        const t = here && runningTool(null, ev.invocation_id, null, ev.handle);
        if (t) t.stalledSecs = null;
        if (here) scheduleEventRender({ live: true });
        break;
      }
      case "invocation_completed": {
        refreshRunning();
        const t = here && runningTool(ev.tool_name, ev.invocation_id, null, ev.handle);
        if (t) { t.status = ev.ok ? "ok" : "failed"; t.stalledSecs = null; }
        note(ev.tool_name + " background invocation " + (ev.ok ? "finished" : "failed"), !ev.ok);
        break;
      }
      case "optimiser_activity":
        note("optimiser · " + ev.tool_name + ": " + ev.text);
        break;
    }
    const output = here && (["message_delta", "tool_call_started", "tool_call_finished", "subagent_started", "subagent_finished", "invocation_completed"].includes(ev.type)
      || ev.type === "reasoning_delta" && S.showThinking);
    const deltaOnly = ["message_delta", "reasoning_delta", "tool_progress", "subagent_step", "invocation_activity"].includes(ev.type);
    const composerUnchanged = deltaOnly || ["run_usage", "session_clients", "approval_summary"].includes(ev.type);
    scheduleEventRender({ status: !deltaOnly, composer: !composerUnchanged, live: output, newContent: output });
  }

  // =========================================================== connection
  const launchParams = new URLSearchParams(location.search);
  const launchShareToken = launchParams.get("share");
  const launchShareError = launchParams.has("share-error");
  const launchAction = launchParams.get("action");
  const launchView = launchParams.get("view");
  let launchRouteHandled = false;
  function readPendingShareLaunch() {
    try {
      const pending = JSON.parse(sessionStorage.getItem("formless_pending_share_launch") || "null");
      return pending && typeof pending.token === "string" && pending.token
        && (pending.scope == null || typeof pending.scope === "string") ? pending : null;
    } catch (_) { return null; }
  }
  function rememberPendingShareLaunch(token, scope = S.storageScope) {
    if (typeof token !== "string" || !token) return false;
    const previous = readPendingShareLaunch();
    const boundScope = previous?.token === token && previous.scope ? previous.scope : scope || null;
    try {
      sessionStorage.setItem("formless_pending_share_launch", JSON.stringify({ token, scope: boundScope }));
      return true;
    } catch (_) { return false; }
  }
  function pendingShareLaunchForScope(scope) {
    const pending = readPendingShareLaunch();
    return pending ? { ...pending, scopeMismatch: !!pending.scope && pending.scope !== scope } : null;
  }
  function clearPendingShareLaunch(token) {
    const pending = readPendingShareLaunch();
    if (!pending || pending.token !== token) return false;
    try { sessionStorage.removeItem("formless_pending_share_launch"); return true; } catch (_) { return false; }
  }
  if (launchShareToken) rememberPendingShareLaunch(launchShareToken, S.storageScope);

  function openShareInbox() {
    return new Promise((resolve, reject) => {
      const open = indexedDB.open("formless-share-inbox", 1);
      open.onupgradeneeded = () => {
        if (!open.result.objectStoreNames.contains("shares")) open.result.createObjectStore("shares", { keyPath: "token" });
      };
      open.onerror = () => reject(open.error);
      open.onsuccess = () => resolve(open.result);
    });
  }
  async function readIncomingShare(token) {
    const db = await openShareInbox();
    try {
      return await new Promise((resolve, reject) => {
        const request = db.transaction("shares", "readonly").objectStore("shares").get(token);
        request.onsuccess = () => resolve(request.result || null);
        request.onerror = () => reject(request.error);
      });
    } finally { db.close(); }
  }
  async function storeIncomingShare(share) {
    const db = await openShareInbox();
    try {
      await new Promise((resolve, reject) => {
        const tx = db.transaction("shares", "readwrite");
        tx.objectStore("shares").put(share);
        tx.oncomplete = resolve;
        tx.onerror = () => reject(tx.error);
        tx.onabort = () => reject(tx.error || new Error("share storage aborted"));
      });
    } finally { db.close(); }
  }
  async function deleteIncomingShare(token) {
    const db = await openShareInbox();
    try {
      await new Promise((resolve, reject) => {
        const tx = db.transaction("shares", "readwrite");
        tx.objectStore("shares").delete(token);
        tx.oncomplete = resolve;
        tx.onerror = () => reject(tx.error);
        tx.onabort = () => reject(tx.error || new Error("share cleanup aborted"));
      });
    } finally { db.close(); }
  }
  function incomingShareText(share) {
    const text = typeof share.text === "string" ? share.text : "";
    const title = typeof share.title === "string" ? share.title : "";
    const url = typeof share.url === "string" ? share.url : "";
    const parts = [title, text].filter((part, index, values) => part && values.indexOf(part) === index);
    if (url && !parts.includes(url) && !text.includes(url)) parts.push(url);
    return parts.join("\n");
  }
  function incomingShareFile(value) {
    if (typeof File !== "undefined" && value instanceof File) return value;
    if (typeof File !== "undefined" && typeof Blob !== "undefined" && value instanceof Blob && typeof value.name === "string") {
      return new File([value], value.name, { type: value.type });
    }
    return null;
  }
  async function applyIncomingShare(token, share, selected = { sessionId: S.current, scope: S.storageScope }) {
    const hasFiles = Array.isArray(share.files) && share.files.length > 0;
    const alreadyPending = S.shareApplyPending;
    if (hasFiles) {
      S.shareApplyPending = true;
      renderComposer();
    }
    try { return await applyIncomingShareRecord(token, share, selected); }
    finally {
      if (hasFiles) {
        S.shareApplyPending = alreadyPending;
        renderComposer();
      }
    }
  }
  async function applyIncomingShareRecord(token, share, selected = { sessionId: S.current, scope: S.storageScope }) {
    const record = { ...share, token, files: Array.isArray(share.files) ? share.files : [] };
    const selectionIsCurrent = () => S.current === selected.sessionId && S.storageScope === selected.scope;
    if (record.draftApplied && !record.draftSessionId) {
      if (record.files.length) {
        note("This shared item needs to be shared again because its original draft cannot be identified.", true);
        return false;
      }
      return true;
    }
    if (record.draftSessionId) {
      if (record.draftScope !== S.storageScope) {
        note("This shared item belongs to a different Formless space; reopen that space to continue.", true);
        return false;
      }
      if (S.current !== record.draftSessionId) {
        if (!selectionIsCurrent()) {
          note("The selected Session changed while the share was loading; reload to apply it safely.", true);
          return false;
        }
        if (!S.sessions.some((session) => session.id === record.draftSessionId)) await refreshSessions();
        if (!selectionIsCurrent()) {
          note("The selected Session changed while the share was loading; reload to apply it safely.", true);
          return false;
        }
        if (!S.sessions.some((session) => session.id === record.draftSessionId)) {
          note("The Session for this shared draft is no longer available.", true);
          return false;
        }
        await openSession(record.draftSessionId);
      }
    } else {
      if (!selectionIsCurrent()) {
        note("The selected Session changed while the share was loading; reload to apply it safely.", true);
        return false;
      }
      if (currentTopicAttempt()) {
        const parent = topicForSession()?.topic_session_id;
        if (parent) {
          await openSession(parent);
          if (S.current !== parent || S.storageScope !== selected.scope) {
            note("The selected Session changed while the share was loading; reload to apply it safely.", true);
            return false;
          }
          if (S.sessionLoad?.id === parent && S.sessionLoad.status === "failed"
            && !await openMainChat({ sessionId: parent, scope: selected.scope })) {
            note("Main Chat couldn't be opened for this share; the share is still waiting.", true);
            return false;
          }
        }
      }
      if (!S.current || currentTopicAttempt() || S.sessionLoad?.status === "failed") {
        if (!await openMainChat({ sessionId: S.current, scope: S.storageScope })) {
          note("Main Chat couldn't be opened for this share; the share is still waiting.", true);
          return false;
        }
      }
      if (!S.current || !S.storageScope || currentTopicAttempt() || S.sessionLoad?.status === "failed") return false;
      record.draftSessionId = S.current;
      record.draftScope = S.storageScope;
      await storeIncomingShare(record);
      if (S.current !== record.draftSessionId || S.storageScope !== record.draftScope) {
        note("The selected Session changed while the share was being saved; reload to apply it safely.", true);
        return false;
      }
    }
    const destination = { sessionId: record.draftSessionId, scope: record.draftScope };
    const destinationIsCurrent = () => S.current === destination.sessionId && S.storageScope === destination.scope;
    if (!destinationIsCurrent()
      || S.sessionLoad?.id === record.draftSessionId && S.sessionLoad.status === "failed") {
      note("The destination Session for this share couldn't be opened; the share is still waiting.", true);
      return false;
    }
    if (!record.draftApplied) {
      const sharedText = incomingShareText(record);
      if (sharedText) {
        const input = $("input");
        const wasLiteralSlashDraft = isSharedSlashDraft(destination.scope, destination.sessionId, input.value);
        input.value = input.value ? input.value + (input.value.endsWith("\n") ? "" : "\n\n") + sharedText : sharedText;
        if (!saveDraft(destination.sessionId, input.value, destination.scope)) {
          note("Shared text couldn't be saved to this draft; reload to retry.", true);
          return false;
        }
        if (wasLiteralSlashDraft || sharedText.trimStart().startsWith("/") && input.value.trimStart().startsWith("/")) {
          rememberSharedSlashDraft(destination.scope, destination.sessionId, input.value);
        }
        autosize();
        scheduleForecast();
      }
      record.draftApplied = true;
      await storeIncomingShare(record);
      if (!destinationIsCurrent()) {
        note("The selected Session changed while the share was being saved; reload to finish safely.", true);
        return false;
      }
    }
    const files = record.files.map(incomingShareFile);
    if (files.some((file) => !file)) {
      return resolveFailedSharedFiles(token, record, destination, record.files, "Some shared files couldn't be restored");
    }
    record.files = await attachFiles(files, destination.sessionId, destination.scope);
    await storeIncomingShare(record);
    if (record.files.length) {
      return resolveFailedSharedFiles(token, record, destination, record.files, "Some shared files couldn't be attached");
    }
    clearShareAttachmentFailure(token, destination.scope, destination.sessionId);
    return true;
  }
  function stripLaunchParams(keys) {
    const url = new URL(location.href);
    for (const key of keys) url.searchParams.delete(key);
    history.replaceState(history.state, "", url.pathname + url.search + url.hash);
  }
  async function handleLaunchRoute() {
    if (launchRouteHandled) return;
    launchRouteHandled = true;
    if (launchAction === "main-chat" && await openMainChat()) stripLaunchParams(["action"]);
    if (launchShareError) {
      note("Shared content couldn't be saved. Please share it again.", true);
      stripLaunchParams(["share-error"]);
    }
    const pendingShare = pendingShareLaunchForScope(S.storageScope);
    const shareToken = launchShareToken || (pendingShare && !pendingShare.scopeMismatch ? pendingShare.token : null);
    const shareBlockedByScope = !!(pendingShare?.scopeMismatch && (!launchShareToken || pendingShare.token === launchShareToken));
    const shareLaunchActive = !!shareToken && !shareBlockedByScope;
    if (shareBlockedByScope) {
      note("This pending share belongs to a different Formless space; reopen that space to continue.", true);
    } else if (shareToken) {
      rememberPendingShareLaunch(shareToken, S.storageScope);
      const selected = { sessionId: S.current, scope: S.storageScope };
      try {
        const share = await readIncomingShare(shareToken);
        if (!share) {
          note("This shared content is no longer available. Share it again.", true);
          clearPendingShareLaunch(shareToken);
          stripLaunchParams(["share"]);
        }
        else if (S.current !== selected.sessionId || S.storageScope !== selected.scope) {
          note("The selected Session changed while the share was loading; reload to apply it safely.", true);
        }
        else {
          const confirmed = share.requiresConfirmation === false || await ask({
            title: "Confirm shared content",
            text: "The browser did not provide enough request metadata to verify this share. Add its text and files to the Chat draft? Discarding removes it. Nothing is sent automatically.",
            ok: "Add to draft",
            cancelLabel: "Discard share",
          });
          if (!confirmed) {
            await deleteIncomingShare(shareToken);
            clearPendingShareLaunch(shareToken);
            stripLaunchParams(["share"]);
            note("Shared content was discarded; no draft was changed.");
          } else if (await applyIncomingShare(shareToken, share, selected)) {
            await deleteIncomingShare(shareToken);
            clearPendingShareLaunch(shareToken);
            stripLaunchParams(["share"]);
          }
        }
      } catch (error) {
        note("Shared content couldn't be added to this draft: " + (error?.message || "storage is unavailable"), true);
      }
    }
    if (launchView === "approvals") {
      if (!shareLaunchActive) setView("approvals");
      stripLaunchParams(["view"]);
    }
    if (shareLaunchActive) setView("chat");
    if (launchAction === "focus-pending") {
      focusPending();
      stripLaunchParams(["action"]);
    }
  }

  async function loadAll() {
    if (runningPanelExposed()) refreshRunning("exposure");
    await refreshSessions();
    await refreshTopics(true);
    const saved = prefs.get(sessionStorageKey(S.storageScope), null);
    const linked = S.linkedSession && S.sessions.find((s) => s.id === S.linkedSession);
    if (S.linkedSession && !linked) note("That Session link points to a Session that no longer exists.", true);
    S.linkedSession = null;
    const target = linked || S.sessions.find((s) => s.id === S.current) || S.sessions.find((s) => s.id === saved) || S.mainChat || S.sessions[0];
    if (target) await openSession(target.id, true);
    else renderAll();
    if (S.view !== "chat") setView(S.view);
    refreshTools();
    refreshModules();
    refreshProposals();
    refreshModelStats();
    refreshSettings();
    await handleLaunchRoute();
  }

  /** The page can hold the pairing secret only over HTTPS or on this
   * machine: over plain HTTP anyone on the path could read it (#907). */
  function secureOrigin() {
    const h = location.hostname.replace(/^\[|\]$/g, "");
    return location.protocol === "https:" || h === "localhost" || h.endsWith(".localhost") || h === "::1" || /^127\./.test(h);
  }
  /** Reset the backoff only once a channel has stayed up a while (#906). */
  function markStable() {
    clearTimeout(S.stableTimer);
    S.stableTimer = setTimeout(() => { S.backoff = 1000; }, 30000);
  }
  async function connect(secret) {
    S.toolVersionDiff = null;
    $("pair-error").textContent = "";
    if (!secureOrigin()) {
      $("pair-error").textContent = "This page was not loaded over HTTPS, so it will not take a pairing secret. Open the relay's https:// address.";
      showPair();
      return;
    }
    let keyBytes;
    try {
      keyBytes = b64ToBytes(secret);
      if (keyBytes.length !== 32) throw new Error("length");
    } catch (_) {
      $("pair-error").textContent = "That pairing secret does not look right. Run formless relay status to see it again.";
      showPair();
      return;
    }
    S.userClosed = false;
    // Only a non-extractable key and the room stay; the raw bytes are
    // wiped and the secret string is kept only when the user chose to stay
    // paired across reloads (#908).
    S.key = await crypto.subtle.importKey("raw", keyBytes, "HKDF", false, ["deriveKey", "deriveBits"]);
    S.room = await roomFor(keyBytes);
    S.local = false;
    activateStorageScope();
    keyBytes.fill(0);
    S.relayModule = await import("./transport.mjs");
    // This bound covers retained byte-backed transport copies; rendered
    // application data/DOM and platform-native buffers are separate owners.
    if (!S.relayMemory) S.relayMemory = new S.relayModule.MemoryBudget(512 * 1024 * 1024);
    try {
      if (prefs.get("remember_pairing", false)) sessionStorage.setItem("formless_secret", secret);
      else sessionStorage.removeItem("formless_secret");
    } catch (_) {}
    openRelay();
  }
  /** The stored pairing secret, only when the user chose to keep it. */
  function rememberedSecret() {
    try { return prefs.get("remember_pairing", false) ? sessionStorage.getItem("formless_secret") || "" : ""; } catch (_) { return ""; }
  }
  /** Connect (or reconnect) to the room with the key already imported. */
  function finishRelayReady(ws) {
    if (S.ws !== ws || !S.relayLink || S.relayLink.ws !== ws || !S.relayLocalProof || !S.relayRoomReady || !S.channel) throw new Error("room readiness before E2E proof");
    if (S.relayReady) return;
    S.relayReady = true;
    S.relayLink.endpoint.setReady();
    restorePushSubscription(ws);
    markStable();
    note("daemon online");
    refreshOutbox();
    void flushOutbox();
    loadAll();
  }
  function openRelay() {
    S.local = false;
    if (S.relayLink) S.relayLink.endpoint.dispose();
    S.relayLink = null;
    S.relayLocalProof = false; S.relayRoomReady = false; S.relayReady = false;
    S.channel = null;
    const proto = location.protocol === "https:" ? "wss:" : "ws:";
    const ws = new WebSocket(proto + "//" + location.host + "/relay/" + S.room + "?role=client&transport=1");
    S.ws = ws;
    ws.onopen = () => {
      $("pair").classList.add("hidden");
      $("app").classList.remove("hidden");
      renderAll();
    };
    // Wire data and credit controls are bounded before any async callback.
    // Completed application frames retain their memory lease while decrypting.
    ws.onmessage = (event) => {
      if (S.ws !== ws) return;
      try {
        const frame = String(event.data);
        S.relayModule.wireBytes(frame);
        if (!S.relayLink) {
          const packet = JSON.parse(frame);
          const welcome = packet.relay === "control" && typeof packet.f === "string" && JSON.parse(packet.f);
          if (!welcome || welcome.relay !== "transport" || welcome.v !== 1 || welcome.g !== packet.g || !/^[a-f0-9]{32}$/.test(packet.g)) throw new Error("bounded relay transport required");
          const endpoint = new S.relayModule.WireEndpoint({
            generation: packet.g,
            memory: S.relayMemory,
            send: (text) => ws.send(text),
            close: (code, reason) => { try { ws.close(code, reason); } catch (_) {} },
            consume: async (encrypted) => {
              const estimated = Math.min(S.relayModule.LIMITS.plaintext, Math.ceil(encrypted.length * 3 / 4));
              const lease = S.relayMemory.reserve(9 * estimated + 1024 * 1024);
              try { await onFrame(ws, encrypted); } finally { lease.release(); }
            },
            control: async (text) => {
              const value = JSON.parse(text);
              if (value.relay === "transport") return;
              if (value.relay === "push_subscribed") { S.pushRegistration?.finish(ws, "accepted"); return; }
              if (value.relay === "push_unsubscribed") { S.pushRemovalAck?.(ws, value.id); return; }
              if (value.relay === "push_refused") { pushRefused(ws, value); return; }
              if (value.relay === "ready") {
                if (value.g !== packet.g) throw new Error("stale room readiness");
                S.relayRoomReady = true;
                finishRelayReady(ws);
                return;
              }
              if (value.relay && value.relay !== "daemon_online" && value.relay !== "daemon_offline") throw new Error("unknown relay control");
              await onFrame(ws, text);
            },
          });
          S.relayLink = { ws, endpoint };
        }
        S.relayLink.endpoint.receive(frame);
      } catch (_) { try { ws.close(1008, "relay transport failed"); } catch (_) {} }
    };
    async function onFrame(ws, frame) {
      if (S.ws !== ws) return;
      if (frame.startsWith('{"relay":')) {
        const online = JSON.parse(frame).relay === "daemon_online";
        const was = S.daemonOnline;
        S.daemonOnline = online;
        if (online && !was) startHandshake();
        if (!online && was) {
          S.channel = null;
          clearTimeout(S.handshakeTimer);
          clearLiveState();
          for (const p of S.pending.values()) p.reject(new Error("daemon went offline"));
          S.pending.clear();
          note("daemon went offline", true);
        }
        renderChrome();
        renderHistory();
        return;
      }
      let hs = null;
      try {
        const v = JSON.parse(frame);
        if (v.v === FRAME_VERSION && typeof v.hello === "string") hs = v;
      } catch (_) {}
      // With a channel up, a hello is a stray or replayed one: ignored, so
      // it can neither rekey one side nor break the channel (#904).
      if (S.channel) {
        if (hs) return;
      } else {
        let hello = null;
        try { hello = hs && b64ToBytes(hs.hello); } catch (_) {}
        if (!hello || hello.length !== 16 || !S.hello || typeof hs.confirm !== "string") return;
        // The daemon must prove it holds the key before anything is trusted.
        const clientHello = S.hello, pairingKey = S.key;
        const current = () => S.ws === ws && S.hello === clientHello && S.key === pairingKey;
        const want = await confirmTag(hello, "d2b");
        if (!current()) return;
        let got;
        try { got = b64ToBytes(hs.confirm); } catch (_) { return; }
        if (got.length !== want.length || got.some((b, i) => b !== want[i])) return;
        const proof = await confirmTag(hello, "b2d");
        if (!current()) return;
        ws.send(JSON.stringify({ v: FRAME_VERSION, confirm: bytesToB64(proof) }));
        clearTimeout(S.handshakeTimer);
        if (!await finishHandshake(hello) || !current()) return;
        S.relayLocalProof = true;
        // Room readiness follows the daemon's verification of our confirm.
        // Bulk grants wait for this ready control's consumed receipt.
        if (S.relayRoomReady) finishRelayReady(ws);
        return;
      }
      let msg;
      const channel = S.channel;
      try { msg = JSON.parse(await decrypt(frame)); } catch (_) {
        // Replayed, missing or forged: the channel can't be trusted, so
        // start over (a new handshake reloads everything) (#905).
        note("link integrity failure: reconnecting", true);
        try { ws.close(4002, "link integrity failure"); } catch (_) {}
        return;
      }
      if (S.ws !== ws || S.channel !== channel) return;
      onMessage(msg);
    }
    ws.onclose = (ev) => {
      if (S.ws !== ws) return;
      clearTimeout(S.stableTimer);
      if (ev && ev.code === 4002) note("link integrity failure: reconnecting", true);
      // Disconnected from the daemon's device list: stay off until asked.
      if (ev && ev.code === 4001) {
        S.userClosed = true;
        $("pair-error").textContent = "This browser was disconnected by the daemon. Connect again to rejoin.";
        showPair();
      }
      S.daemonOnline = false;
      S.channel = null;
      S.relayLocalProof = false; S.relayRoomReady = false; S.relayReady = false;
      if (S.relayLink && S.relayLink.ws === ws) S.relayLink.endpoint.dispose();
      clearLiveState();
      for (const p of S.pending.values()) p.reject(new Error("relay connection closed"));
      S.pending.clear();
      if (!$("app").classList.contains("hidden")) renderAll();
      if (S.userClosed) return;
      // Reconnect with backoff; the relay and daemon are both stateless here.
      scheduleReconnect(() => { if (!S.userClosed && S.ws === ws) openRelay(); });
    };
  }

  function onMessage(msg) {
    if (msg.kind === "response") {
      const p = S.pending.get(msg.id);
      if (!p) return; // timed out already
      S.pending.delete(msg.id);
      if (msg.ok) p.resolve(msg.result);
      else {
        // The code (formless_protocol::codes) is what to branch on; the text is for people.
        const err = new Error(msg.error || "daemon error");
        err.code = (msg.result && msg.result.code) || null;
        err.daemonResponse = true;
        p.reject(err);
      }
    } else if (msg.kind === "event") onEvent(msg);
  }

  // Local mode (SPEC §12): `formless web` serves this page on 127.0.0.1
  // and the protocol runs over a same-origin WebSocket, no relay crypto.
  async function connectLocal(token) {
    S.toolVersionDiff = null;
    if (S.relayLink) S.relayLink.endpoint.dispose();
    S.relayLink = null; S.relayReady = false;
    S.local = true;
    activateStorageScope();
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
      markStable();
      S.localConnected = true;
      S.localFails = 0;
      S.channel = { local: true };
      S.daemonOnline = true;
      $("pair").classList.add("hidden");
      $("app").classList.remove("hidden");
      renderAll();
      note("connected to the local daemon");
      refreshOutbox();
      void flushOutbox();
      loadAll();
    };
    ws.onmessage = (e) => {
      let msg;
      try { msg = JSON.parse(String(e.data)); } catch (_) { return; }
      onMessage(msg);
    };
    ws.onclose = () => {
      if (S.ws !== ws) return;
      clearTimeout(S.stableTimer);
      const wasOpen = S.channel != null;
      S.daemonOnline = false;
      S.channel = null;
      clearLiveState();
      for (const p of S.pending.values()) p.reject(new Error("local daemon connection closed"));
      S.pending.clear();
      if (S.userClosed) return;
      if (!wasOpen && !S.localConnected) {
        $("pair-error").textContent = "Not signed in to the local daemon: run formless web for a new link.";
        showPair();
        return;
      }
      // Was connected: keep retrying (the daemon may be restarting).
      if (!wasOpen && ++S.localFails === 3) {
        note("Still offline. If the daemon restarted, run formless web for a new link.", true);
      }
      renderAll();
      scheduleReconnect(() => { if (!S.userClosed && S.ws === ws) connectLocal(null); });
    };
  }

  // Reconnect after S.backoff, with a countdown and a manual retry.
  function scheduleReconnect(fn) {
    clearTimeout(S.reconnect && S.reconnect.timer);
    // Full jitter, so tabs that lost the relay together don't return in waves.
    const delay = 250 + Math.floor(Math.random() * (S.backoff - 250));
    S.reconnect = { at: Date.now() + delay, fn, timer: setTimeout(retryNow, delay) };
    S.backoff = Math.min(S.backoff * 2, 15000);
    renderChrome();
  }
  function retryNow() {
    if (!S.reconnect) return;
    const fn = S.reconnect.fn;
    cancelReconnect();
    fn();
  }
  function cancelReconnect() {
    if (!S.reconnect) return;
    clearTimeout(S.reconnect.timer);
    S.reconnect = null;
    renderChrome();
  }
  setInterval(() => { if (S.reconnect) renderChrome(); }, 1000);
  // A heartbeat, so the daemon can tell a vanished browser from an idle
  // one and drop it from its device list (#910).
  setInterval(() => { if (S.channel && !S.local) request({ type: "ping" }, 20000).catch(() => {}); }, 60000);

  function showPair() {
    $("app").classList.add("hidden");
    $("pair").classList.remove("hidden");
    $("secret").focus();
  }
  async function forget() {
    if (S.forgetting) return;
    S.forgetting = true;
    S.userClosed = true;
    cancelReconnect();
    try { sessionStorage.removeItem("formless_secret"); } catch (_) {}
    let warning = "";
    try {
      if (!S.local) {
        toast("Turning off notifications and forgetting this device…");
        warning = await removePushSubscription();
      }
    } catch (_) {
      warning = "Could not confirm removal of this browser's notification subscription.";
    }
    if (S.ws) S.ws.close();
    S.ws = null;
    // Carry only cleanup feedback across the reload that discards the page's state.
    history.replaceState(warning ? { forgetWarning: warning } : null, "", location.pathname);
    location.reload();
  }

  // ============================================================== wiring
  $("remember").checked = prefs.get("remember_pairing", false);
  $("pair-form").addEventListener("submit", (e) => {
    e.preventDefault();
    const secret = $("secret").value.trim();
    // Not left in the form once read (#908).
    $("secret").value = "";
    prefs.set("remember_pairing", $("remember").checked);
    connect(secret);
  });
  $("disconnect").onclick = forget;
  document.querySelectorAll("#tabs button").forEach((b) => (b.onclick = () => setView(b.dataset.view)));
  $("new-session").onclick = pickNewSession;
  $("import-session").onclick = importSession;
  $("fork-btn").onclick = () => forkSession(S.current);
  $("propose-topic").onclick = () => proposeTopic();
  $("export-session").onclick = () => pickExport(S.current);
  $("show-thinking").onchange = (e) => setThinking(e.target.checked);
  $("search-form").addEventListener("submit", (e) => { e.preventDefault(); search($("search").value); });
  $("search").addEventListener("search", () => { if (!$("search").value) search(""); });
  $("search").addEventListener("input", () => { if (!$("search").value) search(""); });
  $("import-tool").onclick = async () => {
    const bundle = await pickJson();
    if (bundle && (await previewToolBundleDescriptions(bundle)) && (await act({ type: "import_tool", bundle }, "Imported — the Tool needs first-run approval"))) refreshTools();
  };
  // From an https URL or gist (SPEC §7.8): fetched by the daemon, shown in
  // full (source, Capabilities, code), imported only on a second yes.
  $("import-tool-url").onclick = async () => {
    const url = await ask({ title: "Import Tools from a URL", inputKind: "url", text: "An https link to a Tool bundle: a gist, a GitHub file, or any JSON export.", input: true, placeholder: "https://gist.github.com/…", ok: "Fetch" });
    if (!url || !url.trim()) return;
    const fetched = await act({ type: "fetch_tool_bundle", url: url.trim() });
    if (!fetched) return;
    const b = fetched.bundle;
    const descriptions = await previewToolBundleDescriptions(b);
    if (!descriptions) return;
    const tools = Array.isArray(b.tools) ? b.tools : [b];
    const body = h("div", { class: "import-preview" },
      h("p", { class: "muted" }, "Source: " + fetched.source + (fetched.file ? " (" + fetched.file + ")" : "")),
      tools.map((t) => h("details", { class: "chip", open: tools.length === 1 },
        h("summary", null, h("strong", null, t.name || "?"), " ", h("span", { class: "muted" }, (descriptions.get(t) || {}).text || "")),
        h("p", null, (t.capabilities && t.capabilities.length ? t.capabilities : ["(no capabilities)"]).map((c) =>
          h("span", { class: "tag" + (isBroad(c) ? " broad" : "") }, c)), h("span", { class: "tag" }, "memory " + (t.memory_mb || 256) + " MB")),
        h("pre", { class: "diff" }, t.implementation_js || ""))),
      h("p", { class: "muted" }, "Imported Tools are never trusted: each asks before its first sensitive call."));
    if (!(await ask({ title: "Import " + tools.length + " Tool" + (tools.length === 1 ? "" : "s") + "?", body, ok: "Import" }))) return;
    if (await act({ type: "import_tool", bundle: b }, "Imported — the Tool needs first-run approval")) refreshTools();
  };
  $("export-tools").onclick = async () => {
    const bundle = await act({ type: "export_tools" });
    if (bundle) download("formless-tools.json", bundle);
  };
  $("optimise-all").onclick = () => act({ type: "run_optimiser", tool_id: null }, "Optimiser started");
  $("generate-proposal").onclick = () => { act({ type: "generate_proposal" }, "Generating and measuring a proposal…"); };
  $("stop-btn").onclick = stop;
  $("menu-btn").onclick = () => setSidebar(!$("sidebar").classList.contains("open"), { focus: true });
  $("notes-btn").onclick = () => setActivityDrawer($("notes-panel").classList.contains("hidden"));
  $("notes-close").onclick = () => setActivityDrawer(false);
  // Attachments (SPEC §5.6): stored by the daemon, sent with the next message.
  /** Upload `file` in pieces (#909): the daemon checks the size first, then
   * each piece is read, encoded and sent on its own, so neither side holds
   * the whole file several times over, and no relay message is near its
   * limit. A failure part-way attaches nothing. */
  async function uploadFile(sessionId, file) {
    const name = file.name || "pasted-image.png";
    let begun;
    try {
      begun = await request({ type: "attach_begin", session_id: sessionId, name, bytes: file.size, mime: file.type || null });
    } catch (e) {
      note(name + ": " + e.message, true);
      return null;
    }
    const step = begun.chunk_bytes;
    try {
      for (let index = 0, at = 0; at < file.size; index++, at += step) {
        const piece = new Uint8Array(await file.slice(at, at + step).arrayBuffer());
        let bin = "";
        for (let i = 0; i < piece.length; i += 0x8000) bin += String.fromCharCode.apply(null, piece.subarray(i, i + 0x8000));
        await request({ type: "attach_chunk", upload_id: begun.upload_id, index, data_b64: btoa(bin) });
      }
      return await request({ type: "attach_finish", upload_id: begun.upload_id });
    } catch (e) {
      request({ type: "attach_abort", upload_id: begun.upload_id }).catch(() => {});
      note(name + " wasn't attached: " + e.message, true);
      return null;
    }
  }
  async function attachFiles(files, sessionId = S.current, storageScope = S.storageScope) {
    if (sessionId === S.current && currentTopicAttempt()) return files;
    const sid = sessionId;
    if (!sid) { note("Open a session first", true); return files; }
    const failed = [];
    for (const file of files) {
      const r = await uploadFile(sid, file);
      if (r) {
        (S.chips[sid] = S.chips[sid] || []).push(r);
        if (S.current === sid) renderChips();
      } else failed.push(file);
    }
    await refreshAttachmentInventory(sid, storageScope);
    return failed;
  }
  /** A sent message's attachments by name; one retention removed says
   * so instead of vanishing (#858). */
  function attachmentTags(m) {
    let atts = [];
    try { atts = JSON.parse(m.meta || "{}").attachments || []; } catch (_) {}
    if (!atts.length) return [];
    return [h("div", { class: "muted sent-from" }, ...atts.map((a) => h("span", { class: "tag attach-chip" },
      h("span", { class: "attach-icon" }, a.reference ? "↗" : (a.mime || "").startsWith("image/") ? "🖼" : "📄"),
      h("span", { class: "attach-name" }, a.name || "attachment", a.expired ? " · expired" : ""))))];
  }
  /** The current Session's pending attachments, each removable (×). */
  function renderChips() {
    const box = $("attach-chips");
    const previous = [...box.querySelectorAll("button[data-attachment-key]")];
    const focusIndex = previous.indexOf(document.activeElement);
    const focusKey = focusIndex >= 0 ? previous[focusIndex].dataset.attachmentKey : null;
    const list = S.chips[S.current] || [];
    box.classList.toggle("hidden", !list.length);
    box.replaceChildren(...list.map((a) => h("span", { class: "tag attach-chip", title: (a.mime || "file") + " · " + Math.max(1, Math.round(a.bytes / 1024)) + " KB" },
      h("span", { class: "attach-icon" }, (a.mime || "").startsWith("image/") ? "🖼" : "📄"),
      h("span", { class: "attach-name" }, a.name),
      h("button", { class: "btn ghost small", type: "button", "data-attachment-key": a.id || a.path || a.name || "", "aria-label": "Remove " + a.name, onclick: async () => {
        const sid = S.current;
        // A referenced file (too big to attach) is named by its path.
        if (await act({ type: "detach_file", session_id: sid, id: a.id || a.path })) {
          await refreshAttachmentInventory(sid);
          // Its @path stays in the text but isn't attached again on send.
          if (S.current === sid) renderChips();
        }
      } }, "×"))));
    if (focusIndex >= 0) {
      const next = [...box.querySelectorAll("button[data-attachment-key]")];
      // Keep a deliberately focused remaining chip. Otherwise use the next
      // chip, then the previous one, then the composer after the final removal.
      (next.find(button => button.dataset.attachmentKey === focusKey)
        || next[Math.min(focusIndex, next.length - 1)] || $("input")).focus({ preventScroll: true });
    }
  }
  // Drop files anywhere on the chat to attach them.
  {
    const view = $("view-chat");
    let depth = 0;
    const hasFiles = (e) => [...(e.dataTransfer && e.dataTransfer.types || [])].includes("Files");
    view.addEventListener("dragenter", (e) => { if (!hasFiles(e)) return; depth++; $("drop-zone").classList.remove("hidden"); });
    view.addEventListener("dragover", (e) => { if (hasFiles(e)) e.preventDefault(); });
    view.addEventListener("dragleave", () => { if (--depth <= 0) { depth = 0; $("drop-zone").classList.add("hidden"); } });
    view.addEventListener("drop", (e) => {
      if (!hasFiles(e)) return;
      e.preventDefault();
      depth = 0;
      $("drop-zone").classList.add("hidden");
      attachFiles([...e.dataTransfer.files]);
    });
  }
  // A real button so Tab, Enter and Space reach it (#1740); its click is
  // the user gesture that opens the hidden file picker.
  $("attach-btn").addEventListener("click", () => $("attach-input").click());
  $("attach-input").addEventListener("change", (e) => { attachFiles([...e.target.files]); e.target.value = ""; });
  $("input").addEventListener("paste", (e) => {
    const files = [...((e.clipboardData && e.clipboardData.files) || [])];
    if (files.length) { e.preventDefault(); attachFiles(files); }
  });
  $("composer").addEventListener("submit", (e) => { e.preventDefault(); submit(false); });
  $("input").addEventListener("input", () => { if (!S.editMessageId) { clearEditedSharedSlashDraft(S.storageScope, S.current, $("input").value); saveDraft(S.current, $("input").value); } palette.sel = 0; autosize(); updatePalette(); updateMentions(); scheduleForecast(); });
  $("input").addEventListener("blur", () => setTimeout(() => { closeMentions(); closePalette(); }, 100));
  $("input").addEventListener("keydown", (e) => {
    if (e.isComposing || e.keyCode === 229) return;
    // Native newlines (including Shift+Enter and open suggestions) remain
    // native; only explicit send/queue actions suppress held-key repeats.
    if (e.key === "Enter" && (!composerEnterSends() || e.shiftKey) && !e.ctrlKey && !e.metaKey) return;
    if (e.key === "Enter" && e.repeat && (e.ctrlKey || e.metaKey || composerEnterSends())) { e.preventDefault(); return; }
    if (e.key === "Enter" && (e.ctrlKey || e.metaKey)) { e.preventDefault(); submit(true); return; }
    if (paletteKey(e) || mentionKey(e)) return;
    if (e.key === "?" && !$("input").value && !e.ctrlKey && !e.metaKey) { e.preventDefault(); return showHelp(); }
    if (e.key === "Enter" && !e.shiftKey && !e.isComposing) { e.preventDefault(); submit(false); }
  });
  coarsePointer.addEventListener("change", () => {
    syncInputTouchMode();
    renderComposer();
    updateEnterSendsControl();
  });
  document.addEventListener("click", (e) => {
    if (popover && !popover.contains(e.target)) closePopover();
    // A modal click belongs to that layer, including a button that removed
    // its dialog before this event bubbled. Do not dismiss background drawers.
    if (e.composedPath().some((node) => node instanceof Element && node.classList.contains("backdrop"))) return;
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
      const list = visibleTools();
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
    { keys: ["?"], help: "commands and keys", run: () => showHelp() },
    { keys: ["1", "2", "3", "4", "5"], help: "Chat · Tools · Metrics · Proposals · Settings", run: (k) => setView(VIEWS[Number(k) - 1]) },
    { keys: ["[", "]"], help: "previous / next view (TUI Shift+Tab / Tab)", run: (k) => setView(VIEWS[(VIEWS.indexOf(S.view) + (k === "]" ? 1 : VIEWS.length - 1)) % VIEWS.length]) },
    { keys: ["j", "ArrowDown"], help: "next Session (Chat) or Tool (Tools)", run: () => moveSelection(1) },
    { keys: ["k", "ArrowUp"], help: "previous Session or Tool", run: () => moveSelection(-1) },
    { keys: ["n"], help: "new Session", run: () => newSession() },
    { keys: ["a"], help: "show the pending approval or question", run: () => focusPending() },
    { keys: ["x"], view: "tools", help: "retire / restore the selected Tool", run: () => { const t = selectedTool(); if (t) retireTool(t, S.toolDetail && S.toolDetail.tool.id === t.id ? S.toolDetail : null); } },
    { keys: ["b"], view: "tools", help: "roll the selected Tool back one version", run: () => { const t = selectedTool(); if (t && t.current_version > 1) rollbackTool(t, t.current_version - 1); } },
    { keys: ["e"], view: "tools", help: "export the selected Tool", run: () => { const t = selectedTool(); if (t) exportTool(t); } },
    { keys: ["o"], view: "tools", help: "optimise the selected Tool", run: () => { const t = selectedTool(); if (t) act({ type: "run_optimiser", tool_id: t.id }, "Optimiser started for " + t.name); } },
    { keys: ["p"], view: "tools", help: "purge the selected Tool (typed confirmation)", run: () => { const t = selectedTool(); if (t) purgeTool(t); } },
  ];
  const typingIn = (el) => el && (el.tagName === "INPUT" || el.tagName === "TEXTAREA" || el.tagName === "SELECT" || el.isContentEditable);

  document.addEventListener("keydown", (e) => {
    if (e.defaultPrevented) return;
    const layer = activeDialog();
    if (layer && layer.node !== $("modal-backdrop")) return;
    if ((e.ctrlKey || e.metaKey) && !e.altKey && e.key.toLowerCase() === "k" && S.channel && !layer) {
      e.preventDefault();
      openSwitcher();
      return;
    }
    const modalOpen = layer?.node === $("modal-backdrop");
    if (modalOpen && e.repeat && e.key === "Enter") {
      e.preventDefault();
      return;
    }
    if (modalOpen && !e.ctrlKey && !e.metaKey && !e.altKey) {
      if (e.repeat && ["y", "o", "s", "1", "n", "N"].includes(e.key)) {
        e.preventDefault();
        return;
      }
      if (e.key === "y") { e.preventDefault(); decide(true); return; }
      if (e.key === "o" && S.approvals[0] && S.approvals[0].kind !== "promotion") { e.preventDefault(); decide(true, "once"); return; }
      if (e.key === "s" && S.approvals[0] && S.approvals[0].kind !== "promotion") { e.preventDefault(); decide(true, "session"); return; }
      if (e.key === "1" && S.approvals[0] && S.approvals[0].kind !== "promotion") { e.preventDefault(); decide(true, "hour"); return; }
      if (e.key === "n") { e.preventDefault(); decide(false); return; }
      if (e.key === "N" && S.approvals[0] && S.approvals[0].kind !== "promotion" && S.approvals[0].tool_id) { e.preventDefault(); decide(false, "always", true); return; }
    }
    const dialogOpen = !!layer;
    if (!dialogOpen && !e.ctrlKey && !e.metaKey && !e.altKey && !typingIn(document.activeElement) && S.channel) {
      const sc = SHORTCUTS.find((s) => s.keys.includes(e.key) && (!s.view || s.view === S.view));
      if (sc) { e.preventDefault(); sc.run(e.key); return; }
    }
    if (e.key === "Escape") {
      e.preventDefault();
      if (e.repeat) return;
      if (popover) closePopover();
      else if (!$("notes-panel").classList.contains("hidden")) setActivityDrawer(false);
      else if ($("sidebar").classList.contains("open")) closeSidebar();
      else if (e.target === $("input")) {
        if (S.editMessageId) cancelEdit();
        else stop();
      }
    }
  });
  // Tick elapsed timers (activity + running Tools) once a second.
  setInterval(() => {
    if (isRunning() || S.liveTools.some((t) => t.status === "running")) updateElapsedTimers();
  }, 1000);
  // Update relative times without replacing the Session controls.
  setInterval(updateSessionTimes, 60000);
  // Topic execution is independent of the open chat. Poll the small topic
  // index even while Main Chat is open; never subscribe to every transcript.
  setInterval(() => {
    if (!S.topicsUnsupported && S.daemonOnline && !document.hidden && S.view === "chat" && !S.topicBusy) refreshTopicView();
  }, 4000);

  // A new `formless web` link pasted into an open tab only changes the
  // fragment: sign in with it instead of ignoring it.
  window.addEventListener("hashchange", () => {
    const frag = hashParams();
    if (frag.malformed) {
      history.replaceState(null, "", location.pathname);
      $("pair-error").textContent = "That link does not look right. Copy the pairing or Session link again.";
      showPair();
      return;
    }
    // A Session link pasted into an open, connected tab: just open it.
    // The fragment never stays in the address bar (#908). A Session link
    // with a key opens here only when that key is this tab's room.
    if (frag.k) {
      history.replaceState(null, "", location.pathname);
      if (frag.s && S.channel && !S.local) {
        let bytes = null;
        try { bytes = b64ToBytes(frag.k); } catch (_) {}
        if (bytes && bytes.length === 32) {
          roomFor(bytes).then((room) => {
            bytes.fill(0);
            if (room !== S.room) return note("That link is for another daemon: open it in a new tab.", true);
            if (S.sessions.some((x) => x.id === frag.s)) openSession(frag.s);
            else note("That Session link points to a Session that no longer exists.", true);
          });
        }
      }
      return;
    }
    if (frag.s && !frag.local && S.channel) {
      history.replaceState(null, "", location.pathname);
      if (S.sessions.some((s) => s.id === frag.s)) openSession(frag.s);
      else note("That Session link points to a Session that no longer exists.", true);
      return;
    }
    if (!document.querySelector('meta[name="formless-local"]') || !frag.local) return;
    S.linkedSession = frag.s || null;
    const token = frag.local;
    const old = S.ws;
    S.ws = null;
    cancelReconnect();
    if (old) old.close();
    connectLocal(token);
  });

  (function init() {
    if (history.state?.forgetWarning) {
      $("pair-error").textContent = history.state.forgetWarning;
      history.replaceState(null, "", location.pathname);
    }
    const frag = hashParams();
    if (frag.malformed) {
      history.replaceState(null, "", location.pathname);
      $("pair-error").textContent = "That link does not look right. Copy the pairing or Session link again.";
      showPair();
      return;
    }
    // A Session link (#s=…) opens that Session once connected.
    S.linkedSession = frag.s || null;
    if (document.querySelector('meta[name="formless-local"]')) {
      connectLocal(frag.local || null);
      return;
    }
    // Out of the address bar and history before anything touches the
    // network, whether or not the relay is reachable (#908).
    if (location.hash) history.replaceState(null, "", location.pathname);
    const secret = frag.k || rememberedSecret();
    if (secret) connect(secret);
    else showPair();
  })();
})();
