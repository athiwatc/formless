"""Check JavaScript number and member boundaries in shipped code blocks."""

import functools
import http.server
import os
from pathlib import Path
import threading

from playwright.sync_api import sync_playwright


ROOT = Path(__file__).resolve().parent
CASES = [
    ('js', '1.5.toFixed(2)', ['1.5', '2'], ['toFixed']),
    ('javascript', '1..toString()', ['1.'], ['toString']),
    ('mjs', '0x1f.toString(16)', ['0x1f', '16'], ['toString']),
    ('TSX', '1e2.toFixed(2)', ['1e2', '2'], ['toFixed']),
    ('typescript', '1e+2.toPrecision(2)', ['1e+2', '2'], ['toPrecision']),
    ('cjs', '2_000n.toString()', ['2_000n'], ['toString']),
    ('js', '077.valueOf()', ['077'], ['valueOf']),
    ('js', '.5.toFixed(2)', ['.5', '2'], ['toFixed']),
    ('js', 'const n = 1_000 / 1.5;', ['1_000', '1.5'], []),
    ('js', 'item1 / α\u03012', [], []),
    ('js', '[...5, ....5]', ['5', '.5'], []),
    ('js', 'return\ufeff2', ['2'], []),
    ('rust', '(2).to_string()', ['2'], ['to_string']),
]


class QuietHandler(http.server.SimpleHTTPRequestHandler):
    def log_message(self, *_):
        pass


def expose(route):
    source = (ROOT / 'web/app.js').read_text()
    end = source.rfind('\n})();')
    assert end > 0
    hook = '\nwindow.__numberFixture = {codeBlock, markdown};'
    route.fulfill(status=200, content_type='text/javascript', body=source[:end] + hook + source[end:])


def check(page):
    for view in ('block', 'fence'):
        for lang, source, numbers, calls in CASES:
            actual = page.evaluate(r'''({view, lang, source}) => {
                const {codeBlock, markdown} = window.__numberFixture;
                const node = view === 'block'
                    ? codeBlock(source, lang)
                    : markdown('```' + lang + '\n' + source + '\n```');
                const code = node.querySelector('pre code');
                const tokens = selector => Array.from(code.querySelectorAll(selector), el => el.textContent);
                return {text: code.textContent, numbers: tokens('.tok-num'),
                    calls: tokens('.tok-fn'), comments: tokens('.tok-com')};
            }''', {'view': view, 'lang': lang, 'source': source})
            expected = {'text': source, 'numbers': numbers, 'calls': calls, 'comments': []}
            assert actual == expected, (view, lang, source, actual, expected)
    print(f'PASS {len(CASES)} numeric boundaries through code blocks and Markdown fences')


def main():
    handler = functools.partial(QuietHandler, directory=str(ROOT / 'web'))
    with http.server.ThreadingHTTPServer(('127.0.0.1', 0), handler) as server:
        thread = threading.Thread(target=server.serve_forever, daemon=True)
        thread.start()
        try:
            with sync_playwright() as p:
                browser = p.chromium.launch(
                    executable_path=os.environ.get('FORMLESS_CHROMIUM_EXECUTABLE', '/usr/bin/chromium'),
                    args=['--no-sandbox'],
                )
                page = browser.new_page()
                page.route('**/app.js', expose)
                page.goto(f'http://127.0.0.1:{server.server_port}/')
                page.wait_for_function('!!window.__numberFixture')
                check(page)
                browser.close()
        finally:
            server.shutdown()
            thread.join(timeout=5)


if __name__ == '__main__':
    main()
