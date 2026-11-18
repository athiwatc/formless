"""Verify literal boundaries through the shipped code-block and Markdown DOM."""
import argparse
import functools
import http.server
from pathlib import Path
import threading

from playwright.sync_api import sync_playwright

from browser_fixture import chromium_executable


ROOT = Path(__file__).resolve().parent


def literal(lang, value, before, after, keywords):
    return lang, before + value + after, [value], keywords


RUST = [
    literal('rust', r'r"foo\"', 'let s = ', '; let n = 1;', ['let', 'let']),
    literal('RS', 'r#"say "hi""#', 'let s = ', '; let n = 1;', ['let', 'let']),
    literal('rust', 'br##"first\n"# is short\nlast"##', 'let s = ', '; let n = 1;', ['let', 'let']),
    literal('rust', r'cr"foo\"', 'let s = ', '; let n = 1;', ['let', 'let']),
    literal('rust', 'cr#"say "hi""#', 'let s = ', '; let n = 1;', ['let', 'let']),
    literal('rust', 'cr##"first\nreturn "#\nlast"##', 'let s = ', '; let n = 1;', ['let', 'let']),
    literal('rust', 'r#"no closer\nlet still_inside = 1', 'let s = ', '', ['let']),
    literal('rust', r'"say \"hi\""', 'let s = c', '; let n = 1;', ['let', 'let']),
    literal('rust', r'r"foo\"', 'for _ in 0..', '.len() { let n = 1; }', ['for', 'in', 'let']),
    literal('rust', 'cr#"say "hi""#', 'let n = 0..', '.to_bytes().len();', ['let']),
]
GO = [
    literal('go', r'`C:\`', 's := ', '; return s', ['return']),
    literal('GOLANG', r'`\\server\share\`', 's := ', '; return s', ['return']),
    literal('go', '`first\nreturn 0\nlast\\`', 's := ', '; return s', ['return']),
    literal('go', r'"say \"hi\""', 's := ', '; return s', ['return']),
    literal('javascript', r'`C:\` still inside`', 'const s = ', '; let n = 1;', ['const', 'let']),
]
TRIPLE = [
    literal('python', '"""say "hi" now"""', 'x = ', '\nreturn x', ['return']),
    literal('py', "'''say 'hi' now'''", 'x = ', '\nreturn x', ['return']),
    literal('PYTHON3', '"""doc\n    return 1\n    """', 'def f():\n    ', '\n    return 2', ['def', 'return']),
    literal('python', '"""escaped \\""" inside\nreturn 1\n"""', 'x = ', '\nreturn x', ['return']),
    literal('java', '"""\n    return 1;\n    """', 'String s = ', '; return s;', ['return']),
    literal('kt', '"""raw\\\nreturn 1\nlast\\"""', 'val s = ', '\nreturn s', ['val', 'return']),
    literal('kotlin', '"""quote""""', 'val s = ', '; val after = 1', ['val', 'val']),
]


class QuietHandler(http.server.SimpleHTTPRequestHandler):
    def log_message(self, *_):
        pass


def expose(route, app):
    source = app.read_text()
    end = source.rfind('\n})();')
    assert end > 0
    hook = '\nwindow.__literalFixture = {codeBlock, markdown};'
    route.fulfill(status=200, content_type='text/javascript', body=source[:end] + hook + source[end:])


def check(page, cases):
    for view in ('block', 'fence'):
        for lang, source, strings, keywords in cases:
            actual = page.evaluate(r'''({view, lang, source}) => {
                const fixture = window.__literalFixture;
                const node = view === 'fence'
                    ? fixture.markdown('```' + lang + '\n' + source + '\n```')
                    : fixture.codeBlock(source, lang);
                const code = node.querySelector('pre code');
                const tokens = selector => [...code.querySelectorAll(selector)].map(el => el.textContent);
                return {text: code.textContent, strings: tokens('.tok-str'), keywords: tokens('.tok-kw')};
            }''', {'view': view, 'lang': lang, 'source': source})
            expected = {'text': source, 'strings': strings, 'keywords': keywords}
            assert actual == expected, (view, lang, source, actual, expected)
    print(f'PASS {len(cases)} string cases through code blocks and Markdown fences')


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--app', type=Path, default=ROOT / 'web/app.js')
    parser.add_argument('--case', choices=['all', 'rust', 'go', 'triple'], default='all')
    args = parser.parse_args()
    cases = {'all': RUST + GO + TRIPLE, 'rust': RUST, 'go': GO, 'triple': TRIPLE}[args.case]
    handler = functools.partial(QuietHandler, directory=str(ROOT / 'web'))
    with http.server.ThreadingHTTPServer(('127.0.0.1', 0), handler) as server:
        thread = threading.Thread(target=server.serve_forever, daemon=True)
        thread.start()
        try:
            with sync_playwright() as p:
                browser = p.chromium.launch(executable_path=chromium_executable(p), args=['--no-sandbox'])
                page = browser.new_page()
                errors = []
                page.on('pageerror', lambda error: errors.append(str(error)))
                page.route('**/app.js', lambda route: expose(route, args.app))
                page.set_default_timeout(5000)
                page.goto(f'http://127.0.0.1:{server.server_port}/')
                page.wait_for_function('!!window.__literalFixture')
                check(page, cases)
                assert not errors, errors
                browser.close()
        finally:
            server.shutdown()
            thread.join(timeout=5)


if __name__ == '__main__':
    main()
