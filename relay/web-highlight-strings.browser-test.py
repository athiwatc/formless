"""Verify literal boundaries through the shipped code-block and Markdown DOM."""
import argparse
import functools
import http.server
from pathlib import Path
import threading

from playwright.sync_api import sync_playwright

from browser_fixture import chromium_executable


ROOT = Path(__file__).resolve().parent


def literal(lang, value, before, after, keywords, comments=None):
    return lang, before + value + after, [value], keywords, comments or []


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

URL_RE = r'/https:\/\//gi'
REGEX = [
    literal('javascript', URL_RE, 'const re = ', '; let z = 1; // outside',
            ['const', 'let'], ['// outside']),
    literal('JS', r'/\/*/', 'const re = ', '; let z = 1;', ['const', 'let']),
    literal('jsx', r'/[/\]]/u', '', '; let z = 1;', ['let']),
    literal('mjs', URL_RE, 'return ', '; let z = 1;', ['return', 'let']),
    literal('cjs', URL_RE, 'throw /* before */ ', '; let z = 1;',
            ['throw', 'let'], ['/* before */']),
    literal('js', URL_RE, 'await ', '; let z = 1;', ['await', 'let']),
    literal('js', URL_RE, 'void ', '; let z = 1;', ['void', 'let']),
    literal('js', URL_RE, 'return [...typeof ', '];', ['return', 'typeof']),
    literal('ts', URL_RE, 'return await ', ' as RegExp;', ['return', 'await', 'as']),
    literal('tsx', URL_RE, 'await ', ' satisfies RegExp;', ['await']),
    literal('js', URL_RE, 'const re = // explanation \t\n', '; let z = 1;',
            ['const', 'let'], ['// explanation \t']),
    literal('js', URL_RE, 'await ', ' /* multi\nline */ let z = 1;',
            ['await', 'let'], ['/* multi\nline */']),
    literal('js', r'/two/', 'await ', '*2;', ['await']),
    literal('js', r'/two/', 'async function f(){ await ', '// outside\n}',
            ['async', 'function', 'await'], ['// outside']),
    literal('js', r'/two/', 'async function f(){ await ', '/* outside */; }',
            ['async', 'function', 'await'], ['/* outside */']),
    literal('js', URL_RE, 'for (const item of ', ') { consume(item); }', ['for', 'const', 'of']),
    literal('js', URL_RE, 'yield ', ';', ['yield']),
    literal('jsx', r'/div/', 'const result = value < ', '.test(s);', ['const']),
    literal('jsx', r'/div>/', 'const result = value <', '.test(s); // outside',
            ['const'], ['// outside']),
    literal('tsx', r'/div>/g', 'const result = value <', '; // outside',
            ['const'], ['// outside']),
    ('js', 'let q = a / b / c; // outside', [], ['let'], ['// outside']),
    ('js', 'obj./* member */throw / a / b;', [], ['throw'], ['/* member */']),
    ('js', 'obj?. /* member */ await / a / b;', [], ['await'], ['/* member */']),
    ('js', 'const await = 48; return await / 2 / 3;', [], ['const', 'await', 'return', 'await'], []),
    ('js', 'const of = 48; return of / 2 / 3;', [], ['const', 'of', 'return', 'of'], []),
    ('js', 'const yield = 48; return yield / 2 / 3;', [], ['const', 'yield', 'return', 'yield'], []),
    ('js', 'const await = 48; return await / 2 /* middle */ / 3;',
     [], ['const', 'await', 'return', 'await'], ['/* middle */']),
    ('js', 'const await = 48; return await / 2 // outside',
     [], ['const', 'await', 'return', 'await'], ['// outside']),
    ('jsx', 'const node = <div>hello</div>; // outside', [], ['const'], ['// outside']),
    ('tsx', 'const node = <Wrapper>{text}</Wrapper>; // outside', [], ['const'], ['// outside']),
    ('jsx', 'const node = <>value</>; // fragment', [], ['const'], ['// fragment']),
    ('jsx', 'const value = <div>hello</div>/.5; // division', [], ['const'], ['// division']),
    ('js', 'const q = α\u0301await / 2 / 3;', [], ['const', 'await'], []),
    ('js', 'n++ / left / right;', [], [], []),
    ('js', 'const n = {value: 1} / left / right;', [], ['const'], []),
    ('rust', 'let n = /value/;', [], ['let'], []),
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
        for lang, source, strings, keywords, comments in cases:
            actual = page.evaluate(r'''({view, lang, source}) => {
                const fixture = window.__literalFixture;
                const node = view === 'fence'
                    ? fixture.markdown('```' + lang + '\n' + source + '\n```')
                    : fixture.codeBlock(source, lang);
                const code = node.querySelector('pre code');
                const tokens = selector => [...code.querySelectorAll(selector)].map(el => el.textContent);
                return {text: code.textContent, strings: tokens('.tok-str'),
                    keywords: tokens('.tok-kw'), comments: tokens('.tok-com')};
            }''', {'view': view, 'lang': lang, 'source': source})
            expected = {'text': source, 'strings': strings, 'keywords': keywords, 'comments': comments}
            assert actual == expected, (view, lang, source, actual, expected)
    print(f'PASS {len(cases)} string cases through code blocks and Markdown fences')


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--app', type=Path, default=ROOT / 'web/app.js')
    parser.add_argument('--case', choices=['all', 'rust', 'go', 'triple', 'regex'], default='all')
    args = parser.parse_args()
    cases = {'all': RUST + GO + TRIPLE + REGEX,
             'rust': RUST, 'go': GO, 'triple': TRIPLE, 'regex': REGEX}[args.case]
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
