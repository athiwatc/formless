"""Verify local transcript dates, repeated DST times and stable turn starts."""
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
  const message = (id, role, created_at) => ({id, role, created_at, content: id, meta: '{}'});
  window.__turnTimeFixture = {
    start() {
      $('pair').classList.add('hidden');
      $('app').classList.remove('hidden');
      S.view = 'chat';
      S.current = 'fixture';
      S.sessions = [{id: 'fixture', name: 'Fixture'}];
      S.daemonOnline = true;
      S.settings = {tui: {language: 'en'}};
      S.messages = [
        message('yesterday-local', 'user', '2026-11-01T03:30:00Z'),
        message('first-0130', 'user', '2026-11-01T01:30:00-04:00'),
        message('second-0130', 'assistant', '2026-11-01T01:30:00-05:00'),
        message('later-tool', 'tool', '2026-11-02T00:01:00-05:00'),
        message('invalid', 'user', 'not a date'),
        message('string-zero', 'user', '0'),
        message('impossible-day', 'user', '2026-02-30T05:30:00+00:00'),
        message('impossible-hour', 'user', '2026-11-01T24:00:00Z'),
        message('no-time', 'user', null),
        message('precise-leap-day', 'user', '2024-02-29T14:00:00.123456789+00:00'),
        message('epoch', 'user', 0),
      ];
      renderAll();
    },
    renderHistory,
    thai() { S.settings.tui.language = 'th'; applyLanguage(); renderAll(); },
    orphanTime() {
      S.messages[0] = {...S.messages[0], created_at: null};
      renderHistory();
    },
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


def turn(page, message_id):
    return page.locator(f'#transcript [data-turn-id="{message_id}"]')


def visible_time(stamp):
    return stamp.locator('[aria-hidden="true"]')


def exercise(page, url, app):
    page.route("**/app.js", lambda route: expose(route, app))
    page.clock.install(time=datetime(2026, 11, 2, 4, 59, 30, tzinfo=timezone.utc))
    page.goto(url)
    page.wait_for_function("!!window.__turnTimeFixture")
    page.evaluate("__turnTimeFixture.start()")
    prev = turn(page, "yesterday-local").locator("time")
    assert prev.get_attribute("datetime") == "2026-11-01T03:30:00.000Z", "Turn lacks a machine-readable timestamp"
    expect(visible_time(prev)).to_contain_text("Oct 31, 2026")
    first = turn(page, "first-0130").locator("time")
    second_turn = turn(page, "second-0130")
    second = second_turn.locator("time")
    expect(first).to_have_attribute("datetime", "2026-11-01T05:30:00.000Z")
    expect(second).to_have_attribute("datetime", "2026-11-01T06:30:00.000Z")
    expect(visible_time(first)).to_have_text("01:30 AM")
    expect(visible_time(second)).to_have_text("01:30 AM")
    first_full = first.locator(".sr-only").inner_text()
    second_full = second.locator(".sr-only").inner_text()
    assert "Turn started" in first_full and "GMT-4" in first_full, first_full
    assert "Turn started" in second_full and "GMT-5" in second_full, second_full
    assert "GMT-5" in second_turn.locator(".who").aria_snapshot()
    expect(second_turn.locator('[data-message-id="later-tool"]')).to_have_count(1)
    expect(turn(page, "later-tool")).to_have_count(0)
    for message_id in ["invalid", "string-zero", "impossible-day", "impossible-hour", "no-time"]:
        expect(turn(page, message_id).locator("time")).to_have_count(0)
    expect(turn(page, "precise-leap-day").locator("time")).to_have_attribute("datetime", "2024-02-29T14:00:00.123Z")
    expect(turn(page, "epoch").locator("time")).to_have_attribute("datetime", "1970-01-01T00:00:00.000Z")

    page.evaluate("window.__savedTurn = document.querySelector('[data-turn-id=second-0130]')")
    page.evaluate("window.__savedStamp = window.__savedTurn.querySelector('time'); __turnTimeFixture.renderHistory()")
    assert second.evaluate("el => el === window.__savedStamp")
    page.clock.fast_forward(60_000)
    page.evaluate("__turnTimeFixture.renderHistory()")
    expect(visible_time(first)).to_contain_text("Nov 1, 2026")
    expect(visible_time(second)).to_contain_text("Nov 1, 2026")
    assert second_turn.evaluate("el => el === window.__savedTurn")
    expect(second).to_have_attribute("datetime", "2026-11-01T06:30:00.000Z")

    page.evaluate("__turnTimeFixture.thai()")
    expect(page.locator("html")).to_have_attribute("lang", "th")
    expect(visible_time(second)).to_contain_text("2026")
    assert "2569" not in second.inner_text()
    assert "เริ่มรอบการสนทนา" in second.locator(".sr-only").inner_text()
    page.evaluate("__turnTimeFixture.orphanTime()")
    expect(prev).to_have_count(0)

    page.set_viewport_size({"width": 320, "height": 750})
    page.evaluate("document.documentElement.style.fontSize = '32px'")
    times = page.locator('#transcript time [aria-hidden="true"]')
    assert times.evaluate_all("""labels => {
        const view = document.querySelector('#transcript').getBoundingClientRect();
        return labels.every(label => {
            const range = document.createRange();
            range.selectNodeContents(label);
            return [...range.getClientRects()].every(rect => rect.left >= view.left - 1 && rect.right <= view.right + 1);
        });
    }""")


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
                    page = browser.new_page(viewport={"width": 1200, "height": 900}, locale="en-US",
                                            timezone_id="America/New_York", service_workers="block")
                    page.set_default_timeout(5000)
                    errors = []
                    page.on("pageerror", lambda error: errors.append(str(error)))
                    exercise(page, f"http://127.0.0.1:{server.server_port}/", args.app)
                    assert not errors, errors
                    print(f"PASS {engine} {browser.version} transcript turn dates, local midnight and DST", flush=True)
                    browser.close()
        finally:
            server.shutdown()
            thread.join(timeout=5)


if __name__ == "__main__":
    main()
