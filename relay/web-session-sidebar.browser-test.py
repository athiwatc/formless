"""Verify Session ordering, keyboard focus and minute ticks in the shipped UI."""
import argparse
from datetime import datetime, timezone
import functools
import http.server
from pathlib import Path
import threading

from playwright.sync_api import expect, sync_playwright

from browser_fixture import browser_engines, chromium_executable


ROOT = Path(__file__).resolve().parent
HOOK = """
  const actions = [];
  request = async () => [];
  act = async command => {
    if (!['set_session_pinned', 'set_session_archived'].includes(command.type)) return null;
    actions.push(command);
    const session = S.sessions.find(s => s.id === command.id);
    return command.type === 'set_session_pinned' ? {...session, pinned: command.pinned}
      : {...session, archived: command.archived};
  };
  window.__sessionSidebarFixture = {
    start() {
      $('pair').classList.add('hidden');
      $('app').classList.remove('hidden');
      S.view = 'chat';
      S.local = true;
      S.daemonOnline = true;
      S.current = 'a';
      S.settings = {tui: {notify: 'off'}};
      S.sessions = [
        {id: 'a', name: 'Pinned A', pinned: true, updated_at: '2026-11-18T17:30:00Z'},
        {id: 'b', name: 'Recent B', updated_at: '2026-11-18T18:00:00Z'},
        {id: 'c', name: 'Session C', updated_at: '2026-11-18T17:59:00Z'},
        {id: 'd', name: 'Archived D', archived: true, pinned: true, updated_at: '2026-11-18T17:20:00Z'},
        {id: 'e', name: 'Archived E', archived: true, updated_at: '2026-11-18T18:00:15Z'},
      ];
      S.sessionTree = false;
      renderAll();
    },
    update(id, fields) {
      onEvent({type:'session_updated', session: {...S.sessions.find(s => s.id === id), ...fields}});
    },
    event(type, session_id) { onEvent({type, session_id, id: session_id, run_id: 'fixture-run'}); },
    addPinned() {
      S.sessions.push(...Array.from({length: 40}, (_, index) => ({
        id: 'overflow-' + index, name: 'Another pinned Session ' + index, pinned: true,
        updated_at: '2026-11-18T18:00:25Z',
      })));
      renderSessions();
    },
    actions: () => actions,
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


def row(page, session_id):
    return page.locator(f'#sessions > [data-session-id="{session_id}"]')


def order(page):
    return page.locator("#sessions > .session").evaluate_all(
        "items => items.map(item => item.dataset.sessionId || 'archived')"
    )


def update(page, session_id, **fields):
    page.evaluate("([id, fields]) => __sessionSidebarFixture.update(id, fields)", [session_id, fields])


def open_menu(page, session_id):
    target = row(page, session_id)
    opener = target.locator(".session-open")
    expect(opener).to_be_visible()
    opener.focus()
    target.get_by_role("button", name="Session actions").press("ArrowDown")
    menu = page.get_by_role("menu", name="Session actions")
    expect(menu).to_be_visible()
    return menu


def in_sidebar(control):
    return control.evaluate("""el => {
        const item = el.getBoundingClientRect(), list = document.querySelector('#sessions').getBoundingClientRect();
        return item.top >= list.top && item.bottom <= list.bottom;
    }""")


def check_ordering(page):
    page.locator(".archived-toggle").press("Enter")
    assert order(page) == ["a", "b", "c", "archived", "d", "e"]
    update(page, "b", updated_at="2026-11-18T18:00:20Z")
    actual = order(page)
    assert actual == ["a", "b", "c", "archived", "d", "e"], f"Live recency update moved a row: {actual}"

    open_menu(page, "b").get_by_role("menuitem", name="Pin", exact=True).click()
    assert order(page) == ["b", "a", "c", "archived", "d", "e"]
    open_menu(page, "b").get_by_role("menuitem", name="Archive", exact=True).click()
    assert order(page) == ["a", "c", "archived", "b", "d", "e"]
    open_menu(page, "b").get_by_role("menuitem", name="Unarchive", exact=True).click()
    assert order(page) == ["b", "a", "c", "archived", "d", "e"]
    assert page.evaluate("__sessionSidebarFixture.actions()") == [
        {"type": "set_session_pinned", "id": "b", "pinned": True},
        {"type": "set_session_archived", "id": "b", "archived": True},
        {"type": "set_session_archived", "id": "b", "archived": False},
    ]


def check_focus(page):
    mark = row(page, "b").get_by_role("checkbox")
    mark.focus()
    page.evaluate("window.__savedMark = document.activeElement")
    mark.press("Space")
    expect(mark).to_be_checked()
    expect(mark).to_be_focused()
    assert page.evaluate("__savedMark === document.activeElement"), "Selecting replaced the focused checkbox"
    expect(page.locator(".bulk-bar")).to_contain_text("1 selected")
    mark.press("Space")
    expect(mark).not_to_be_checked()
    expect(mark).to_be_focused()
    expect(page.locator(".bulk-bar")).to_have_count(0)

    opener = row(page, "b").locator(".session-open")
    opener.focus()
    update(page, "c", name="New name for C")
    expect(opener).to_be_focused()
    page.evaluate("window.__savedOpener = document.activeElement")
    expect(row(page, "b").locator(".when")).to_have_text("just now")
    page.clock.fast_forward(60_000)
    expect(row(page, "b").locator(".when")).to_have_text("1m ago")
    expect(opener).to_be_focused()
    assert page.evaluate("__savedOpener === document.activeElement"), "Minute tick replaced the focused control"

    page.evaluate("__sessionSidebarFixture.event('run_started', 'b')")
    page.clock.fast_forward(60_000)
    expect(row(page, "b").locator(".running")).to_have_count(1)
    expect(opener).to_be_focused()
    page.evaluate("__sessionSidebarFixture.addPinned()")
    expect(opener).to_be_focused()
    assert in_sidebar(opener), "Focus was restored below the scrolled Session list"


def check_menu(page):
    popup = open_menu(page, "b")
    pin = popup.get_by_role("menuitem", name="Pin", exact=True)
    expect(pin).to_be_focused()
    page.evaluate("window.__savedMenuItem = document.activeElement")
    update(page, "c", name="Renamed while B’s menu is open")
    expect(pin).to_be_focused()
    trigger = row(page, "b").locator(".menu")
    assert trigger.get_attribute("aria-expanded") == "true", "Menu lost its trigger after a Session update"
    assert page.evaluate("__savedMenuItem === document.activeElement")
    pin.press("Escape")
    expect(popup).to_have_count(0)
    expect(row(page, "b").get_by_role("button", name="Session actions")).to_be_focused()

    popup = open_menu(page, "b")
    update(page, "b", pinned=True)
    unpin = popup.get_by_role("menuitem", name="Unpin", exact=True)
    expect(unpin).to_be_focused()
    unpin.press("Enter")
    assert page.evaluate("__sessionSidebarFixture.actions()") == [
        {"type": "set_session_pinned", "id": "b", "pinned": False},
    ]
    expect(row(page, "b").get_by_role("button", name="Session actions")).to_be_focused()

    popup = open_menu(page, "b")
    page.evaluate("__sessionSidebarFixture.addPinned()")
    expect(popup).to_be_visible()
    popup.get_by_role("menuitem", name="Pin", exact=True).press("Escape")
    expect(trigger).to_be_focused()
    assert in_sidebar(trigger), "Escape focused a trigger outside the scrolled Session list"

    popup = open_menu(page, "b")
    page.set_viewport_size({"width": 390, "height": 844})
    expect(popup).to_have_count(0)
    assert page.evaluate("document.activeElement !== document.body")
    page.set_viewport_size({"width": 1200, "height": 900})

    popup = open_menu(page, "b")
    page.evaluate("__sessionSidebarFixture.event('session_deleted', 'b')")
    expect(popup).to_have_count(0)
    assert page.evaluate("document.activeElement !== document.body")


def exercise(browser, url, app, check):
    page = browser.new_page(viewport={"width": 1200, "height": 900}, locale="en-US", service_workers="block")
    page.set_default_timeout(5000)
    errors = []
    page.on("pageerror", lambda error: errors.append(str(error)))
    page.route("**/app.js", lambda route: expose(route, app))
    page.clock.install(time=datetime(2026, 11, 18, 18, 0, 30, tzinfo=timezone.utc))
    page.goto(url)
    page.wait_for_function("!!window.__sessionSidebarFixture")
    page.evaluate("__sessionSidebarFixture.start()")
    check(page)
    assert not errors, errors
    page.close()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--app", type=Path, default=ROOT / "web/app.js")
    parser.add_argument("--case", choices=["all", "ordering", "focus", "menu"], default="all")
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
                    for case, check in [("ordering", check_ordering), ("focus", check_focus), ("menu", check_menu)]:
                        if args.case in ("all", case):
                            exercise(browser, f"http://127.0.0.1:{server.server_port}/", args.app, check)
                            print(f"PASS {engine} {browser.version} Session sidebar {case}", flush=True)
                    browser.close()
        finally:
            server.shutdown()
            thread.join(timeout=5)


if __name__ == "__main__":
    main()
