"""Exercise the editable model picker with local model and action fixtures."""
import functools
import http.server
from pathlib import Path
import threading

from playwright.sync_api import expect, sync_playwright

from browser_fixture import browser_engines, chromium_executable


ROOT = Path(__file__).resolve().parent
MODELS = [f"fixture/model-{n:03d}" for n in range(225)]
HOOK = """
  const pickerRequests = [];
  let releaseModels;
  act = async request => {
    pickerRequests.push(request);
    if (request.type === 'list_models') return new Promise(resolve => { releaseModels = resolve; });
    return request.type === 'set_setting' ? {} : null;
  };
  window.__pickerFixture = {
    requests: pickerRequests,
    show(model) {
      $('pair').classList.add('hidden');
      $('app').classList.remove('hidden');
      S.view = 'chat';
      S.daemonOnline = true;
      S.current = 'fixture';
      S.sessions = [{ id: 'fixture', name: 'Fixture', model }];
      renderStatus();
    },
    pending: () => !!releaseModels,
    release(models) { const resolve = releaseModels; releaseModels = null; resolve({ models }); },
  };
"""


class QuietHandler(http.server.SimpleHTTPRequestHandler):
    def log_message(self, *_):
        pass


def serve_app(route):
    source = (ROOT / "web/app.js").read_text()
    end = source.rfind("\n})();")
    assert end > 0
    route.fulfill(content_type="text/javascript", body=source[:end] + HOOK + source[end:])


def open_picker(page):
    button = page.locator("#status-model")
    button.press("Enter")
    picker = page.get_by_role("dialog", name="Model", exact=True)
    control = picker.locator("input")
    expect(control).to_be_focused()
    expect(control).to_have_attribute("role", "combobox")
    expect(control).to_have_attribute("aria-expanded", "false")
    assert control.get_attribute("aria-activedescendant") is None
    list_id = control.get_attribute("aria-controls")
    assert list_id
    results = picker.locator(f"#{list_id}")
    expect(results).to_have_attribute("role", "listbox")
    expect(results).to_be_hidden()
    page.wait_for_function("__pickerFixture.pending()")
    return button, picker, control, results


def active(control, results, model, size, position):
    expect(control).to_have_attribute("aria-expanded", "true")
    option = results.get_by_role("option", selected=True)
    expect(option.locator(".picker-id")).to_have_text(model)
    expect(option).to_have_attribute("aria-setsize", str(size))
    expect(option).to_have_attribute("aria-posinset", str(position))
    option_id = option.get_attribute("id")
    assert option_id
    expect(control).to_have_attribute("aria-activedescendant", option_id)
    expect(control.page.locator(f"#{option_id}")).to_have_count(1)
    return option_id


def inspect_chromium_ax(page, list_id, option_id):
    cdp = page.context.new_cdp_session(page)
    try:
        control = cdp.send("Runtime.evaluate", {"expression": "document.activeElement"})
        tree = cdp.send("Accessibility.getPartialAXTree", {
            "objectId": control["result"]["objectId"], "fetchRelatives": False,
        })
        node = tree["nodes"][0]
        assert node["role"]["value"] == "combobox" and node["name"]["value"] == "Filter models"
        props = {prop["name"]: prop["value"] for prop in node["properties"]}
        assert props["expanded"]["value"] is True
        [popup] = props["controls"]["relatedNodes"]
        [target] = props["activedescendant"]["relatedNodes"]
        assert popup["idref"] == list_id
        assert target["idref"] == option_id
    finally:
        cdp.detach()


def exercise(page, url, engine):
    page.route("**/app.js", serve_app)
    page.goto(url)
    page.wait_for_function("!!window.__pickerFixture")
    page.evaluate("(model) => __pickerFixture.show(model)", MODELS[199])

    button, picker, control, results = open_picker(page)
    first_list_id = results.get_attribute("id")
    control.fill(MODELS[199])
    assert control.get_attribute("aria-activedescendant") is None
    page.evaluate("(models) => __pickerFixture.release(models)", MODELS)
    active(control, results, MODELS[199], 1, 1)
    control.fill("absent-custom-model")
    expect(results).to_be_hidden()
    expect(control).to_have_attribute("aria-expanded", "false")
    control.press("ArrowDown")
    assert control.get_attribute("aria-activedescendant") is None
    control.press("Enter")
    expect(picker).to_have_count(0)
    expect(button).to_be_focused()
    assert page.evaluate("__pickerFixture.requests.at(-1)") == {
        "type": "set_session_model", "id": "fixture", "model": "absent-custom-model",
        "reasoning_effort": None,
    }

    button, picker, control, results = open_picker(page)
    page.evaluate("(models) => __pickerFixture.release(models)", MODELS)
    active(control, results, MODELS[199], 225, 200)
    effort = picker.get_by_label("Reasoning effort", exact=True)
    page.keyboard.press("Tab")
    expect(effort).to_be_focused()
    for name in ("Test", "Cancel"):
        page.keyboard.press("Tab")
        expect(picker.get_by_role("button", name=name, exact=True)).to_be_focused()
    page.keyboard.press("Enter")
    expect(picker).to_have_count(0)
    expect(button).to_be_focused()
    assert page.evaluate("__pickerFixture.requests.at(-1).type") == "list_models"

    button, picker, control, results = open_picker(page)
    assert results.get_attribute("id") != first_list_id
    page.evaluate("(models) => __pickerFixture.release(models)", MODELS)
    expect(results.get_by_role("option")).to_have_count(200)
    active(control, results, MODELS[199], 225, 200)
    for index in (200, 201, 202):
        control.press("ArrowDown")
        option_id = active(control, results, MODELS[index], 225, index + 1)
    expect(control).to_be_focused()
    expect(results.get_by_role("option")).to_have_count(200)
    if engine == "chromium":
        inspect_chromium_ax(page, results.get_attribute("id"), option_id)

    results.get_by_role("option", name=MODELS[201], exact=True).click()
    expect(control).to_be_focused()
    active(control, results, MODELS[201], 225, 202)
    control.fill(MODELS[202])
    assert active(control, results, MODELS[202], 1, 1) == option_id
    page.keyboard.press("Tab")
    effort = picker.get_by_label("Reasoning effort", exact=True)
    expect(effort).to_be_focused()
    effort.press("ArrowDown")
    expect(picker).to_be_visible()
    active(control, results, MODELS[202], 1, 1)
    effort.select_option("high")
    control.focus()
    control.press("Enter")
    expect(picker).to_have_count(0)
    expect(button).to_be_focused()
    assert page.evaluate("__pickerFixture.requests.at(-1)") == {
        "type": "set_session_model", "id": "fixture", "model": MODELS[202],
        "reasoning_effort": "high",
    }

    for modifier in ("Control", "Meta"):
        button, picker, control, _ = open_picker(page)
        control.fill("custom-default-model")
        control.press(f"{modifier}+Enter")
        expect(picker).to_have_count(0)
        expect(button).to_be_focused()
        page.wait_for_function("__pickerFixture.requests.at(-1)?.key === 'provider.reasoning_effort'")
        assert page.evaluate("__pickerFixture.requests.slice(-2)") == [
            {"type": "set_setting", "key": "provider.model", "value": "custom-default-model"},
            {"type": "set_setting", "key": "provider.reasoning_effort", "value": None},
        ]
        page.evaluate("(models) => __pickerFixture.release(models)", MODELS)
        expect(picker).to_have_count(0)


def main():
    handler = functools.partial(QuietHandler, directory=str(ROOT / "web"))
    with http.server.ThreadingHTTPServer(("127.0.0.1", 0), handler) as server:
        thread = threading.Thread(target=server.serve_forever, daemon=True)
        thread.start()
        url = f"http://127.0.0.1:{server.server_port}/"
        try:
            with sync_playwright() as playwright:
                for engine in browser_engines():
                    if engine == "chromium":
                        browser = playwright.chromium.launch(
                            executable_path=chromium_executable(playwright), args=["--no-sandbox"],
                        )
                    else:
                        browser = playwright.webkit.launch()
                    print(f"{engine} {browser.version}", flush=True)
                    page = browser.new_page()
                    page.set_default_timeout(5000)
                    exercise(page, url, engine)
                    browser.close()
                    print(f"PASS {engine} model picker active option, filtering, actions and focus", flush=True)
        finally:
            server.shutdown()
            thread.join(timeout=5)


if __name__ == "__main__":
    main()
