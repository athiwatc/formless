#!/usr/bin/env python3
"""Chromium regressions for #2188–#2195 using a local app and synthetic replies.

Run with Python Playwright and /usr/bin/chromium. No live grants, devices,
provider credentials, or push services are used.
"""
import argparse
import functools
import http.server
from pathlib import Path
import re
import threading

from playwright.sync_api import expect, sync_playwright

ROOT = Path(__file__).resolve().parent
HOOK = r"""
  const F = window.__interaction = { calls: [], replies: {}, refreshes: 0 };
  const timestamp = "2026-11-02T12:00:00Z";
  const session = (id, name, extra) => ({id, name, updated_at:timestamp, ...extra});
  const grant = (id) => ({tool_id:id,tool_name:"Tool "+id,capability:"fs:"+id,scope:"always",granted_at:timestamp});
  F.grants = [grant("A"), grant("B")];
  F.reply = (kind, ...values) => F.replies[kind] = values;
  request = async (req) => {
    // Retain only a synthetic projection of account fields for assertions.
    const {api_key, ...record} = req;
    F.calls.push(record.type === "upsert_provider" ? {...record, hasKey:!!api_key} : structuredClone(record));
    const queue = F.replies[req.type];
    if (queue?.length) {
      const reply = queue.shift();
      if (reply.pending) return new Promise((resolve, reject) => {
        F.finish = (value) => value.error ? reject(Object.assign(new Error(value.error), {daemonResponse:!!value.daemon})) : resolve(value);
      });
      if (reply.error) throw Object.assign(new Error(reply.error), {daemonResponse:!!reply.daemon});
      return reply;
    }
    if (req.type === "get_session") return { session:S.sessions.find(s=>s.id===req.id), messages:[] };
    if (req.type === "session_usage") return {runs:0,total:{steps:0,prompt_tokens:0,completion_tokens:0}};
    if (req.type === "list_approvals") return F.grants;
    if (req.type === "revoke_grant") {
      F.grants = F.grants.filter(g=>g.tool_id!==req.tool_id);
      return {revoked:true, still_trusted:[]};
    }
    if (req.type === "undo_registry_change_if_current") {
      if (req.change_id !== F.latest) throw new Error("Undo is no longer available for that Tool change");
      F.latest = null;
      return {undone:"fixture change"};
    }
    return {};
  };
  refreshDescriptionReviews = async () => {};
  refreshSettings = async () => {F.refreshes++;};
  reviewSkill = async () => {};
  F.reveal = (view="chat") => {
    $("pair").classList.add("hidden"); $("app").classList.remove("hidden");
    S.view=view; S.channel={}; S.daemonOnline=true;
    renderChrome();
  };
  F.keyboard = () => {
    F.reveal();
    S.current="first";
    S.sessions=[session("first","First Session",{pinned:true}),session("second","Second Session",{last_run_status:"paused"})];
    S.running.add("first");
    renderSessions(); renderComposer();
    S.unread=105; renderStatus();
  };
  F.edit = () => beginEdit({id:"message",content:"Editing fixture"});
  F.registry = (id, name) => {
    if (id) F.latest=id;
    onEvent({type:"registry_changed",change:{change_id:id,author:"user",action:"retired",name,to_version:1}});
  };
  F.devices = () => {
    F.reveal("settings");
    S.relayClients=[{cid:"fixture",user_agent:"Synthetic browser",first_seen:timestamp,last_seen:timestamp}];
    $("settings").replaceChildren(relayDevicesSection());
  };
  F.approvals = () => {
    F.reveal("approvals");
    S.grants=[...F.grants]; S.grantsSelected=new Set(); renderApprovals();
  };
  F.draft = (kind) => {
    F.reveal("settings");
    return ({job:newJob, mcp:addMcpServer, skill:addSkill, role:()=>editRole("",{},["default"]),
      provider:()=>editProvider(null), memory:()=>editMemory(null)})[kind]();
  };
  F.push = (mode) => {
    F.reveal("settings");
    S.local=false; S.relayReady=mode !== "offline";
    let subscription={endpoint:"https://push-fixture.invalid/only",toJSON(){return {endpoint:this.endpoint};},async unsubscribe(){
      F.localStarted = true;
      await window.recordInteraction("local");
      if (mode==="local-failure") throw new Error("synthetic failure");
      subscription=null;
      return true;
    }};
    Object.defineProperty(navigator,"serviceWorker",{value:{getRegistration:async()=>({pushManager:{getSubscription:async()=>subscription}})}});
    class FixtureSocket extends EventTarget {
      readyState = mode==="offline" ? 3 : 1;
      send(text) {
        const {relay,id} = JSON.parse(text);
        void window.recordInteraction(relay);
        if (relay === "push_subscribe") { F.pushSent = true; return; }
        S.pushRemovalAck?.(this,"unrelated-id");
        if (mode==="timeout") return;
        setTimeout(async ()=> {
          await window.recordInteraction("relay-ack");
          S.pushRemovalAck?.(this,id);
        },70);
      }
      close() {
        if (this.readyState !== 3) {
          this.readyState=3;
          void window.recordInteraction("closed");
        }
        this.dispatchEvent(new Event("close"));
      }
    }
    S.ws=new FixtureSocket();
    F.finishRegistration = async () => {
      await window.recordInteraction("relay-registered");
      S.pushRegistration?.finish(S.ws);
    };
    F.closePushSocket = () => S.ws.close();
    if (mode.startsWith("registration-")) void restorePushSubscription(S.ws);
    sessionStorage.setItem("formless_secret","synthetic-pairing-marker");
    S.settings={};
    renderSettings();
  };
"""


class Quiet(http.server.SimpleHTTPRequestHandler):
    def log_message(self, *_):
        pass


def calls(page, kind):
    return page.evaluate('type => __interaction.calls.filter(call => call.type === type)', kind)


def keyboard(page):
    page.evaluate('__interaction.keyboard()')
    toggle = page.get_by_role("button", name=re.compile(r"^Activity log, \d+ unread$"))
    close = page.get_by_role("button", name="Close Activity", exact=True)
    composer = page.locator("#input")
    expect(toggle).to_have_attribute("aria-controls", "notes-panel")
    expect(toggle).to_have_attribute("aria-expanded", "false")
    expect(toggle).to_have_accessible_name("Activity log, 105 unread")
    expect(page.locator("#notes-badge")).to_have_text("99+")

    toggle.click()
    expect(toggle).to_have_attribute("aria-expanded", "true")
    expect(toggle).to_have_accessible_name("Activity log, 0 unread")
    expect(close).to_be_focused()
    close.click()
    expect(toggle).to_be_focused()
    expect(toggle).to_have_attribute("aria-expanded", "false")

    toggle.press("Space")
    expect(close).to_be_focused()
    composer.focus()
    page.keyboard.press("Escape")
    expect(toggle).to_be_focused()
    expect(toggle).to_have_attribute("aria-expanded", "false")
    assert calls(page, "interrupt_run") == []
    composer.focus()
    composer.dispatch_event("keydown", {"key": "Escape", "repeat": True, "bubbles": True})
    for ime in ({"isComposing": True}, {"keyCode": 229}):
        composer.dispatch_event("keydown", {"key": "Escape", "bubbles": True, **ime})
    assert calls(page, "interrupt_run") == []
    page.keyboard.press("Escape")
    assert len(calls(page, "interrupt_run")) == 1

    page.evaluate('__interaction.edit()')
    toggle.press("Enter")
    expect(close).to_be_focused()
    composer.focus()
    page.keyboard.press("Escape")
    expect(composer).to_have_value("Editing fixture")
    expect(toggle).to_be_focused()
    composer.focus()
    for ime in ({"isComposing": True}, {"keyCode": 229}):
        composer.dispatch_event("keydown", {"key": "Escape", "bubbles": True, **ime})
        expect(composer).to_have_value("Editing fixture")
    page.keyboard.press("Escape")
    expect(composer).to_have_value("")
    assert len(calls(page, "interrupt_run")) == 1
    page.locator("#stop-btn").click()
    assert len(calls(page, "interrupt_run")) == 2

    toggle.click()
    toggle.click()
    expect(toggle).to_be_focused()
    expect(toggle).to_have_attribute("aria-expanded", "false")
    first = page.get_by_role("button", name="First Session", exact=True)
    second = page.get_by_role("button", name="Second Session", exact=True)
    expect(first).to_have_attribute("aria-current", "true")
    expect(first).to_have_accessible_description("Pinned, Running")
    expect(second).to_have_accessible_description("Paused")
    assert first.evaluate("el=>el.closest('li').tabIndex===-1 && !el.querySelector('input,button')")
    second.press("Space")
    expect(second).to_have_attribute("aria-current", "true")
    expect(second).to_be_focused()
    assert calls(page, "get_session")[-1]["id"] == "second"
    first.press("Enter")
    expect(first).to_have_attribute("aria-current", "true")
    assert [r["id"] for r in calls(page, "get_session")] == ["second", "first"]
    select = page.get_by_role("checkbox", name="Select Second Session", exact=True)
    select.press("Space")
    expect(select).to_be_checked()
    expect(select).to_be_focused()
    page.keyboard.press("Tab")
    expect(second).to_be_focused()
    page.keyboard.press("Tab")
    menu = page.locator('[data-session-id="second"]').get_by_role("button", name="Session actions")
    expect(menu).to_be_focused()
    page.keyboard.press("Enter")
    expect(page.get_by_role("menu")).to_be_visible()
    assert len(calls(page, "get_session")) == 2


def undo(page):
    page.evaluate('''() => {
      __interaction.reveal();
      __interaction.registry("a","Tool A");
      __interaction.registry("b","Tool B");
      __interaction.registry(null,"Legacy Tool");
    }''')
    old = page.locator("#toasts .toast").filter(has_text="retired Tool A").get_by_role("button", name="Undo")
    newest = page.locator("#toasts .toast").filter(has_text="retired Tool B").get_by_role("button", name="Undo")
    assert page.locator("#toasts .toast").filter(has_text="Legacy Tool").get_by_role("button").count() == 0
    old.click()
    expect(page.locator("#toasts").get_by_text("Undo is no longer available for that Tool change", exact=True)).to_be_visible()
    newest.evaluate("button=>{button.click();button.dispatchEvent(new MouseEvent('click'));}")
    expect(page.get_by_text("Undone: fixture change", exact=True)).to_be_visible()
    assert [r["change_id"] for r in calls(page, "undo_registry_change_if_current")] == ["a", "b"]
    assert calls(page, "undo_registry_change") == []


def devices(page):
    page.evaluate('''() => {
      __interaction.devices();
      __interaction.reply("disconnect_relay_client",{disconnected:false},{error:"Fixture relay unavailable"},{disconnected:true});
    }''')
    for message, refreshes in [("That browser is already disconnected.", 1), ("Fixture relay unavailable", 1), ("Disconnected", 2)]:
        page.get_by_role("row").get_by_role("button", name="Disconnect", exact=True).click()
        page.get_by_role("dialog").get_by_role("button", name="Disconnect", exact=True).click()
        expect(page.locator("#toasts").get_by_text(message, exact=True)).to_be_visible()
        assert page.evaluate("__interaction.refreshes") == refreshes
        if refreshes == 1:
            assert page.locator("#toasts").get_by_text("Disconnected", exact=True).count() == 0
    assert len(calls(page, "disconnect_relay_client")) == 3


def grants(page, mode):
    page.evaluate("__interaction.approvals()")
    if mode == "all-fail":
        page.evaluate('''__interaction.reply("revoke_grant", {error:"Fixture failure"}, {revoked:false})''')
    elif mode == "partial":
        page.evaluate('''() => {
          __interaction.reply("revoke_grant",{revoked:true,still_trusted:["fs:A"]},{error:"Fixture response lost"});
          __interaction.reply("list_approvals",{error:"Fixture refresh failed"},{error:"Fixture still offline"});
          __interaction.grants=__interaction.grants.filter(g=>g.tool_id!=="A");
        }''')
    page.get_by_role("button", name="Revoke all shown").click()
    page.get_by_role("dialog").get_by_role("button", name="Revoke", exact=True).click()
    region = page.locator("#revocation-feedback")
    table = page.locator("#approvals-table")
    if mode == "all-success":
        expect(page.locator("#toasts").get_by_text("Revoked 2 grants", exact=True)).to_be_visible()
        expect(table).to_contain_text("Nothing approved yet.")
    elif mode == "all-fail":
        expect(region).to_contain_text("2 revocations unconfirmed")
        expect(region).to_contain_text("Tool A · fs:A (always): Fixture failure")
        expect(region).to_contain_text("The daemon did not confirm revocation.")
        assert page.locator("#toasts").get_by_text("Revoked", exact=False).count() == 0
        expect(table.get_by_role("button", name="Revoke")).to_have_count(2)
    else:
        expect(region).to_contain_text("Revoked 1 grant. 1 revocation unconfirmed.")
        expect(region).to_contain_text("Tool B · fs:B (always): Fixture response lost")
        expect(region).to_contain_text("could not be refreshed")
        expect(page.locator("#status-note")).to_contain_text("Fixture refresh failed")
        expect(page.locator("#notes")).to_contain_text("Still allowed without asking by your trusted folders/hosts: fs:A")
        assert table.get_by_text("Tool A").count() == 0
        table.get_by_role("button", name="Revoke").click()
        expect(page.locator("#toasts").get_by_text("Refresh the grants before retrying.", exact=True)).to_be_visible()
        assert len(calls(page, "revoke_grant")) == 2
        region.get_by_role("button", name="Refresh grants").click()
        expect(region).to_contain_text("Check the updated list")
        table.get_by_role("button", name="Revoke").click()
        expect(table).to_contain_text("Nothing approved yet.")
        assert [r["tool_id"] for r in calls(page, "revoke_grant")] == ["A", "B", "B"]


def records(page):
    dialog = page.locator("#ask-backdrop")
    ok = page.locator("#ask-ok")
    error = page.locator("#ask-error")
    page.evaluate('void __interaction.draft("job")')
    task = page.get_by_label("Task", exact=True)
    task.fill("Keep this task")
    page.locator("#ask-body label").filter(has=page.get_by_text("Schedule", exact=True)).locator("select").select_option("once")
    ok.click()
    expect(error).to_have_text("Pick when it runs.")
    expect(task).to_have_value("Keep this task")
    assert calls(page, "add_job") == []
    page.get_by_label("When", exact=True).fill("2027-01-01T10:00")
    page.evaluate('''__interaction.reply("add_job",{error:"Choose another schedule",daemon:true},{pending:true},{next_run:"2027-01-01T10:00:00Z"})''')
    ok.click()
    expect(error).to_have_text("Choose another schedule")
    ok.click()
    expect(ok).to_have_text("Saving…")
    expect(ok).to_be_disabled()
    expect(page.locator("#ask-cancel")).to_be_disabled()
    expect(page.locator("#ask-form")).to_be_focused()
    page.keyboard.press("Tab")
    expect(page.locator("#ask-form")).to_be_focused()
    page.locator("#ask-form").dispatch_event("submit")
    page.keyboard.press("Escape")
    expect(dialog).to_be_visible()
    assert len(calls(page, "add_job")) == 2
    page.evaluate('__interaction.finish({error:"Connection interrupted"})')
    expect(error).to_contain_text("Save was not confirmed")
    expect(ok).to_be_focused()
    expect(task).to_have_value("Keep this task")
    expect(page.get_by_label("When", exact=True)).to_have_value("2027-01-01T10:00")
    ok.click()
    expect(dialog).to_be_hidden()
    assert len(calls(page, "add_job")) == 3

    page.evaluate('void __interaction.draft("provider")')
    page.get_by_label("Name", exact=True).fill("fixture")
    page.get_by_label("Base URL", exact=True).fill("https://provider.invalid/")
    key = page.locator("#ask-body input[type=password]")
    key.fill("test-only-secret")
    page.evaluate('''() => {
      window.oldSecret = document.querySelector("#ask-body input[type=password]");
      __interaction.reply("upsert_provider",{error:"Echo: test-only-secret",daemon:true},{provider:"fixture"});
    }''')
    ok.click()
    expect(error).to_contain_text("Check the name, base URL, and key storage")
    expect(key).to_have_value("test-only-secret")
    assert "test-only-secret" not in page.locator("body").inner_text()
    assert page.evaluate('!JSON.stringify(localStorage).includes("test-only-secret") && !JSON.stringify(sessionStorage).includes("test-only-secret")')
    ok.click()
    expect(dialog).to_be_hidden()
    assert page.evaluate("!oldSecret.isConnected && oldSecret.value === ''")
    assert all(r["hasKey"] for r in calls(page, "upsert_provider"))

    page.evaluate('void __interaction.draft("mcp")')
    page.get_by_label("Name", exact=True).fill("fixture")
    page.get_by_label("Environment", exact=True).fill("TOKEN=fixture-only")
    ok.click()
    expect(error).to_have_text("A command is needed.")
    expect(page.get_by_label("Environment", exact=True)).to_have_value("TOKEN=fixture-only")
    page.locator("#ask-body label").filter(has=page.get_by_text("Connect with", exact=True)).locator("select").select_option("http")
    page.get_by_label("Headers", exact=True).fill("Authorization: fixture-only")
    ok.click()
    expect(error).to_have_text("A URL is needed.")
    page.evaluate('window.oldHeaders=document.querySelector("textarea[placeholder^=Header]")')
    page.locator("#ask-cancel").click()
    expect(dialog).to_be_hidden()
    assert page.evaluate("!oldHeaders.isConnected && oldHeaders.value === ''")
    assert calls(page, "add_mcp_server") == []

    page.evaluate('void __interaction.draft("skill")')
    page.get_by_label("Subfolder", exact=True).fill("fixture")
    ok.click()
    expect(error).to_have_text("A source is needed.")
    expect(page.get_by_label("Subfolder", exact=True)).to_have_value("fixture")
    page.get_by_label("Path, zip or git URL", exact=True).fill("/synthetic/path")
    page.evaluate('''__interaction.reply("add_skill",{name:"fixture"})''')
    ok.click()
    expect(dialog).to_be_hidden()
    assert len(calls(page, "add_skill")) == 1

    page.evaluate('void __interaction.draft("role")')
    page.get_by_label("Description", exact=True).fill("Keep this description")
    ok.click()
    expect(error).to_have_text("A Role name is needed.")
    expect(page.get_by_label("Description", exact=True)).to_have_value("Keep this description")
    page.keyboard.press("Escape")
    expect(dialog).to_be_hidden()
    assert calls(page, "upsert_role") == []

    page.evaluate('void __interaction.draft("memory")')
    text = page.locator("#ask-input")
    text.fill("  Synthetic memory  ")
    page.evaluate('''__interaction.reply("upsert_memory",{error:"Fixture validation",daemon:true},{memory:"fixture"})''')
    ok.click()
    expect(error).to_have_text("Fixture validation")
    expect(text).to_have_value("  Synthetic memory  ")
    ok.click()
    expect(dialog).to_be_hidden()
    assert text.input_value() == ""
    assert calls(page, "upsert_memory")[-1]["text"] == "  Synthetic memory  "


def push(page, mode):
    actions = []
    page.expose_function("recordInteraction", lambda action: actions.append(action))
    page.evaluate("mode=>__interaction.push(mode)", mode)
    if mode.startswith("registration-"):
        page.wait_for_function("__interaction.pushSent")
    page.get_by_role("button", name="Disconnect and forget this device").click()
    if mode.startswith("registration-"):
        page.wait_for_function("__interaction.localStarted")
        assert "push_subscribe" in actions and "push_unsubscribe" not in actions, actions
        if mode == "registration-ready":
            page.evaluate("__interaction.finishRegistration()")
        elif mode == "registration-close":
            page.evaluate("__interaction.closePushSocket()")
    expect(page.locator("#pair")).to_be_visible()
    assert page.evaluate("sessionStorage.getItem('formless_secret')") is None
    feedback = page.locator("#pair-error")
    assert "local" in actions and (mode == "offline" or "closed" in actions), actions
    if mode in ("offline", "timeout", "registration-close", "registration-timeout"):
        expect(feedback).to_contain_text("relay did not confirm removal")
        if mode == "timeout":
            assert "push_unsubscribe" in actions and "relay-ack" not in actions
        else:
            assert "push_unsubscribe" not in actions
    else:
        assert actions.index("push_unsubscribe") < actions.index("relay-ack") < actions.index("closed"), actions
        if mode == "registration-ready":
            assert actions.index("push_subscribe") < actions.index("relay-registered") < actions.index("push_unsubscribe"), actions
        if mode == "local-failure":
            expect(feedback).to_contain_text("Could not confirm that notifications are off in this browser")
            assert "relay did not confirm" not in feedback.inner_text()
        else:
            expect(feedback).to_have_text("")


CASES = {
    "2188-undo": undo,
    "2189-push-connected": lambda page: push(page, "connected"),
    "2189-push-offline": lambda page: push(page, "offline"),
    "2189-push-timeout": lambda page: push(page, "timeout"),
    "2189-local-push-failure": lambda page: push(page, "local-failure"),
    "2189-forget-during-registration": lambda page: push(page, "registration-ready"),
    "2189-registration-socket-close": lambda page: push(page, "registration-close"),
    "2189-registration-ack-timeout": lambda page: push(page, "registration-timeout"),
    "2190-relay-disconnect": devices,
    "2191-2193-keyboard": keyboard,
    "2194-grants-all-fail": lambda page: grants(page, "all-fail"),
    "2194-grants-all-success": lambda page: grants(page, "all-success"),
    "2194-grants-partial": lambda page: grants(page, "partial"),
    "2195-drafts": records,
}


def run():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--app", type=Path, default=ROOT/"web/app.js")
    parser.add_argument("--case", choices=CASES, action="append")
    args = parser.parse_args()
    source = args.app.read_text()
    end = source.rfind("\n})();")
    assert end > 0
    server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), functools.partial(Quiet, directory=str(ROOT/"web")))
    thread = threading.Thread(target=server.serve_forever, daemon=True)
    thread.start()
    try:
        with sync_playwright() as playwright:
            browser = playwright.chromium.launch(executable_path="/usr/bin/chromium", headless=True)
            for name in args.case or CASES:
                context = browser.new_context(viewport={"width": 1280, "height": 900}, service_workers="block")
                page = context.new_page()
                page.set_default_timeout(5000)
                errors = []
                page.on("pageerror", lambda error: errors.append(str(error)))
                page.route("**/app.js", lambda route: route.fulfill(
                    status=200, content_type="text/javascript", body=source[:end] + HOOK + source[end:]))
                page.goto(f"http://127.0.0.1:{server.server_port}/")
                page.wait_for_function("window.__interaction !== undefined")
                CASES[name](page)
                assert not errors, errors
                context.close()
                print("PASS", name, flush=True)
            browser.close()
    finally:
        server.shutdown()
        server.server_close()
        thread.join(timeout=5)


if __name__ == "__main__":
    run()
