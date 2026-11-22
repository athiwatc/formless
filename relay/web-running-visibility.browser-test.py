"""Exercise Running tools polling, invalidation, and focus on a loopback app."""
import argparse
import functools
import http.server
from pathlib import Path
import threading

from playwright.sync_api import expect, sync_playwright

from browser_fixture import browser_engines, chromium_executable


ROOT = Path(__file__).resolve().parent
ROW = {
    "handle": "fixture-handle", "invocation_id": "fixture-call", "session_id": "fixture-session",
    "tool_name": "fixture-tool", "elapsed_ms": 34000, "silent_ms": 30000, "stalled": True,
}
HOOK = """
  const invocationRequests = [], invocationActions = [];
  let fixtureHidden = false;
  let holdSessionList = false, resolveSessionList = null, reconnectLoad = null;
  Object.defineProperty(document, 'hidden', {configurable: true, get: () => fixtureHidden});
  request = command => {
    if (command.type !== 'list_invocations') return Promise.resolve(null);
    return new Promise((resolve, reject) => invocationRequests.push({resolve, reject}));
  };
  act = async command => {
    invocationActions.push(command);
    if (command.type === 'list_sessions' && holdSessionList) {
      return new Promise(resolve => { resolveSessionList = resolve; });
    }
    return null;
  };
  window.__runningFixture = {
    start(row) {
      $('pair').classList.add('hidden');
      $('app').classList.remove('hidden');
      S.current = 'fixture-session';
      S.sessions = [{id: S.current, name: 'Fixture'}];
      S.settings = {tui: {notify: 'off'}};
      S.view = 'settings';
      S.invocations = [row];
      this.connect();
    },
    connect(render = true) {
      S.ws = {readyState: 1};
      S.conn = (S.conn || 0) + 1;
      S.channel = {local: true};
      S.local = true;
      S.daemonOnline = true;
      if (render) renderChrome();
    },
    reconnectWhileExposed() {
      // Swap the transport without renderChrome creating a new exposure.
      this.connect(false);
      holdSessionList = true;
      reconnectLoad = loadAll();
      return {count: invocationRequests.length, sessionsPending: resolveSessionList !== null};
    },
    async finishReconnect() {
      holdSessionList = false;
      resolveSessionList([]);
      await reconnectLoad;
    },
    hide(hidden) {
      fixtureHidden = hidden;
      document.dispatchEvent(new Event('visibilitychange'));
    },
    identify() { S.conn++; },
    event(type) {
      onEvent({type, session_id: S.current, invocation_id: 'fixture-call', handle: 'fixture-handle',
               tool_name: 'fixture-tool', silent_secs: 30, ok: true});
    },
    syncSidebar,
    setView,
    count: () => invocationRequests.length,
    reply({index, rows}) { invocationRequests[index - 1].resolve(rows); },
    fail(index) { invocationRequests[index - 1].reject(new Error('fixture timeout')); },
    actions: () => invocationActions.filter(a => ['cancel_invocation', 'abandon_invocation'].includes(a.type)),
  };
"""


class QuietHandler(http.server.SimpleHTTPRequestHandler):
    def log_message(self, *_):
        pass


def expose(route, app):
    source = app.read_text()
    end = source.rfind("\n})();")
    assert end > 0
    route.fulfill(content_type="text/javascript", body=source[:end] + HOOK + source[end:])


def wait_requests(page, count):
    page.wait_for_function("count => __runningFixture.count() === count", arg=count)


def reply(page, count, *rows):
    page.evaluate("reply => __runningFixture.reply(reply)", {"index": count, "rows": list(rows)})


def tick(page, expected):
    page.clock.fast_forward(2000)
    actual = page.evaluate("__runningFixture.count()")
    assert actual == expected, f"Expected {expected} invocation requests after tick, saw {actual}"


def exercise(page, url, app):
    page.route("**/app.js", lambda route: expose(route, app))
    page.clock.install()
    page.goto(url)
    page.wait_for_function("!!window.__runningFixture")
    page.evaluate("row => __runningFixture.start(row)", ROW)
    tick(page, 0)
    tick(page, 0)

    # A hidden view still gets independently triggered snapshots.
    page.evaluate("__runningFixture.event('invocation_pending')")
    wait_requests(page, 1)
    tick(page, 1)
    reply(page, 1)
    panel = page.locator("#running")
    expect(panel).to_be_hidden()

    page.set_viewport_size({"width": 390, "height": 844})
    page.evaluate("__runningFixture.setView('chat')")
    menu = page.locator("#menu-btn")
    expect(menu).to_have_attribute("aria-expanded", "false")
    tick(page, 1)
    menu.click()
    expect(menu).to_have_attribute("aria-expanded", "true")
    wait_requests(page, 2)
    page.evaluate("() => { __runningFixture.identify(); __runningFixture.syncSidebar(); __runningFixture.hide(false); }")
    assert page.evaluate("__runningFixture.count()") == 2
    page.evaluate("() => { __runningFixture.event('invocation_stalled'); __runningFixture.event('invocation_stalled'); }")
    assert page.evaluate("__runningFixture.count()") == 2
    reply(page, 2, {**ROW, "tool_name": "obsolete snapshot"})
    wait_requests(page, 3)
    expect(panel).to_be_hidden()
    reply(page, 3, ROW)
    expect(panel).to_contain_text("silent")

    cancel = panel.get_by_role("button", name="Cancel", exact=True)
    cancel.focus()
    page.evaluate("window.__savedCancel = document.activeElement")
    resumed = {**ROW, "stalled": False}
    tick(page, 4)
    reply(page, 4, resumed)
    expect(panel).not_to_contain_text("silent")
    expect(cancel).to_be_focused()
    assert page.evaluate("window.__savedCancel === document.activeElement")

    cancel.press("Enter")
    wait_requests(page, 5)
    reply(page, 5, resumed)
    abandon = panel.get_by_role("button", name="Abandon", exact=True)
    abandon.press("Space")
    wait_requests(page, 6)
    reply(page, 6, {**resumed, "abandoned": True})
    expect(abandon).to_have_count(0)
    expect(panel.locator(".linklike")).to_be_focused()
    assert page.evaluate("__runningFixture.actions()") == [
        {"type": "cancel_invocation", "handle": ROW["handle"]},
        {"type": "abandon_invocation", "handle": ROW["handle"]},
    ]

    tick(page, 7)
    page.evaluate("__runningFixture.fail(7)")
    expect(panel).to_contain_text("abandoned")
    expect(panel.locator(".linklike")).to_be_focused()
    page.evaluate("__runningFixture.hide(true)")
    tick(page, 7)
    page.evaluate("__runningFixture.event('invocation_completed')")
    wait_requests(page, 8)
    tick(page, 8)  # Completion refreshes; timer work stays paused.
    reply(page, 8)
    page.evaluate("__runningFixture.hide(false)")
    wait_requests(page, 9)
    reply(page, 9)
    expect(panel).to_be_hidden()
    expect(page.locator("#new-session")).to_be_focused()

    menu.click()
    expect(menu).to_have_attribute("aria-expanded", "false")
    tick(page, 9)
    page.set_viewport_size({"width": 1200, "height": 900})
    wait_requests(page, 10)
    # Real reconnect startup reads Running tools immediately, even while the
    # panel stays exposed and the Session list request is still pending.
    assert page.evaluate("__runningFixture.reconnectWhileExposed()") == {
        "count": 11,
        "sessionsPending": True,
    }
    reply(page, 11, {**resumed, "tool_name": "new connection"})
    expect(panel).to_contain_text("new connection")
    reply(page, 10, {**ROW, "tool_name": "old connection"})
    expect(panel).not_to_contain_text("old connection")
    expect(panel).to_contain_text("new connection")

    page.evaluate("__runningFixture.setView('settings')")
    tick(page, 11)
    page.evaluate("__runningFixture.setView('chat')")
    wait_requests(page, 12)
    reply(page, 12)
    expect(panel).to_be_hidden()

    # A pre-exposure response may be stale, even when its list is empty.
    page.evaluate("() => { __runningFixture.hide(true); __runningFixture.event('invocation_pending'); }")
    wait_requests(page, 13)
    page.evaluate("__runningFixture.hide(false)")
    reply(page, 13, {**ROW, "tool_name": "pre-exposure snapshot"})
    wait_requests(page, 14)
    expect(panel).to_be_hidden()
    page.evaluate("__runningFixture.fail(14)")
    tick(page, 15)
    reply(page, 15)
    tick(page, 15)
    page.evaluate("__runningFixture.finishReconnect()")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--app", type=Path, default=ROOT / "web/app.js")
    args = parser.parse_args()
    handler = functools.partial(QuietHandler, directory=ROOT / "web")
    with http.server.ThreadingHTTPServer(("127.0.0.1", 0), handler) as server:
        thread = threading.Thread(target=server.serve_forever, daemon=True)
        thread.start()
        try:
            with sync_playwright() as playwright:
                for engine in browser_engines():
                    if engine == "chromium":
                        browser = playwright.chromium.launch(
                            executable_path=chromium_executable(playwright), args=["--no-sandbox"],
                        )
                    else:
                        browser = playwright.webkit.launch()
                    page = browser.new_page(viewport={"width": 1200, "height": 900})
                    page.set_default_timeout(5000)
                    exercise(page, f"http://127.0.0.1:{server.server_port}/", args.app)
                    print(f"PASS {engine} {browser.version} Running tools visibility, requests and focus", flush=True)
                    browser.close()
        finally:
            server.shutdown()
            thread.join(timeout=5)


if __name__ == "__main__":
    main()
