"""Exercise deferred and coalesced Settings snapshots on a loopback app."""
import argparse
import functools
import http.server
from pathlib import Path
import threading

from playwright.sync_api import expect, sync_playwright

from browser_fixture import browser_engines, chromium_executable


ROOT = Path(__file__).resolve().parent
BACKGROUND = ["get_settings", "status", "list_description_reviews"]
LISTS = [
    "list_mcp_servers", "list_providers", "list_harness_changes", "list_relay_clients",
    "list_skills", "list_memories", "list_jobs",
]
HOOK = """
  let calls = [], held = [], active = new Map(), maximum = 0, holdType = null, connection = 0;
  const settings = {tui: { language: 'th', animations: false, notify: 'off' }};
  const head = {cursor: 'audit-head', action: 'head', actor: 'fixture'};
  const tail = {cursor: 'audit-tail', action: 'tail', actor: 'fixture'};
  const review = {id: 'fixture-review', kind: 'skill', name: 'example', version: 1,
                  after: {text: 'Fixture description', flags: []}};
  request = command => {
    const {type} = command, conn = connection;
    if (type === 'list_invocations') return Promise.resolve([]);
    const count = (active.get(conn) || 0) + 1;
    active.set(conn, count);
    maximum = Math.max(maximum, count);
    calls.push(type);
    const response = {get_settings: settings, list_description_reviews: [review],
      status: {pid: 42}, list_providers: {providers: [], roles: {}}, list_audit: []};
    const result = type === holdType
      ? new Promise((resolve, reject) => { holdType = null; held.push({resolve, reject}); })
      : Promise.resolve(response[type] ?? []);
    return result.finally(() => active.set(conn, active.get(conn) - 1));
  };
  window.__settingsRefreshFixture = {
    start() {
      $('pair').classList.add('hidden');
      $('app').classList.remove('hidden');
      S.view = 'chat';
      S.auditPages = 2;
      S.audit = [head, tail];
      this.connect();
      // These two fixture pages are already loaded on the current transport.
      // Reconnect below deliberately leaves them owned by the prior channel.
      auditOwner = {ws: S.ws, channel: S.channel, filter: null};
      renderAll();
    },
    connect() {
      S.ws = {readyState: 1};
      S.channel = {local: true};
      S.conn = ++connection;
      S.local = true;
      S.daemonOnline = true;
    },
    setView, refreshSettings, showPair,
    connecting() { S.ws.readyState = 0; },
    auditOlder: () => { loadAudit(true); },
    events(...types) { types.forEach(type => onEvent({type})); },
    hold(type) { holdType = type; },
    settle(index, result) { held[index].resolve(result); },
    reject(index) { held[index].reject(new Error('fixture timeout')); },
    heldCount: () => held.length,
    reset() { calls = []; maximum = 0; },
    snapshot: () => ({calls, maximum, serverCount: S.mcpServers?.length, reviewCount: S.descriptionReviews?.length,
      status: S.status?.pid, notes: S.notes.map(note => note.text),
      auditPages: S.auditPages, audit: S.audit.map(entry => entry.cursor)}),
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


def after_requests(page):
    page.evaluate("new Promise(resolve => requestAnimationFrame(() => resolve()))")
    return page.evaluate("__settingsRefreshFixture.snapshot()")


def wait_held(page, count):
    page.wait_for_function("count => __settingsRefreshFixture.heldCount() === count", arg=count)


def exercise(page, url, app):
    page.route("**/app.js", lambda route: expose(route, app))
    page.goto(url)
    page.wait_for_function("!!window.__settingsRefreshFixture")
    page.evaluate("__settingsRefreshFixture.start()")
    page.evaluate("__settingsRefreshFixture.events('memory_updated', 'memory_updated')")
    result = after_requests(page)
    assert result["calls"] == BACKGROUND, f"Hidden memory event requested {result['calls']}"
    assert result["maximum"] == 1 and result["status"] == 42
    assert any("Description review: skill example" in line for line in result["notes"])
    expect(page.locator("html")).to_have_attribute("lang", "th")
    assert page.locator("html").evaluate("(el) => el.hasAttribute('data-reduced-motion')")

    page.evaluate("() => { const f = __settingsRefreshFixture; f.reset(); f.setView('settings'); }")
    result = after_requests(page)
    assert result["calls"] == BACKGROUND + LISTS, result["calls"]
    assert result["auditPages"] == 2 and result["audit"] == ["audit-head", "audit-tail"]

    # A local mutation's continuation and its broadcast may overlap.
    page.evaluate("""() => {
      const f = __settingsRefreshFixture;
      f.reset();
      f.hold('list_memories');
      f.events('memory_updated', 'memory_updated');
    }""")
    wait_held(page, 1)
    page.evaluate("""() => {
      const f = __settingsRefreshFixture;
      f.events('memory_updated', 'memory_updated');
      f.refreshSettings();
      f.settle(0, []);
    }""")
    result = after_requests(page)
    assert result["maximum"] == 1
    assert result["calls"].count("list_memories") == 2
    assert result["calls"].count("list_providers") == 1
    assert "list_audit" not in result["calls"]

    # Leaving the view stops the remaining Settings-only lists from dispatching.
    page.evaluate("""() => {
      const f = __settingsRefreshFixture;
      f.reset();
      f.hold('list_mcp_servers');
      f.refreshSettings();
    }""")
    wait_held(page, 2)
    page.evaluate("""() => {
      const f = __settingsRefreshFixture;
      f.setView('chat');
      f.reject(1);
    }""")
    result = after_requests(page)
    assert result["calls"] == BACKGROUND + ["list_mcp_servers"]
    assert result["serverCount"] == 0 and result["reviewCount"] == 1

    # A late global snapshot and a manual audit page both belong to the old transport.
    page.evaluate("""() => {
      const f = __settingsRefreshFixture;
      f.hold('get_settings');
      f.events('settings_changed');
    }""")
    wait_held(page, 3)
    page.evaluate("""() => {
      const f = __settingsRefreshFixture;
      f.hold('list_audit');
      f.auditOlder();
    }""")
    wait_held(page, 4)
    page.evaluate("""() => {
      const f = __settingsRefreshFixture;
      f.connect();
      return f.refreshSettings();
    }""")
    page.evaluate("""() => {
      const f = __settingsRefreshFixture;
      f.settle(2, {tui: {language:'en', animations: true}});
      f.settle(3, [{cursor: 'obsolete-audit'}]);
    }""")
    result = after_requests(page)
    expect(page.locator("html")).to_have_attribute("lang", "th")
    assert result["auditPages"] == 2 and result["audit"] == ["audit-head", "audit-tail"]

    # Re-pairing can hide the app or replace the socket before it sets a new channel.
    page.evaluate("""() => {
      const f = __settingsRefreshFixture;
      f.reset();
      f.setView('settings');
      f.showPair();
      f.events('jobs_changed');
    }""")
    assert after_requests(page)["calls"] == BACKGROUND
    page.evaluate("""() => {
      const f = __settingsRefreshFixture;
      f.reset();
      f.connecting();
      return f.refreshSettings();
    }""")
    assert after_requests(page)["calls"] == []


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
                    page = browser.new_page(viewport={"width": 1280, "height": 900})
                    page.set_default_timeout(5000)
                    errors = []
                    page.on("pageerror", lambda error: errors.append(str(error)))
                    exercise(page, f"http://127.0.0.1:{server.server_port}/", args.app)
                    assert not errors, errors
                    print(f"PASS {engine} {browser.version} Settings refresh and connection isolation", flush=True)
                    browser.close()
        finally:
            server.shutdown()
            thread.join(timeout=5)


if __name__ == "__main__":
    main()
