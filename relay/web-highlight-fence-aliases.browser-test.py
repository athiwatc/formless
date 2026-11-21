"""Check the shipped code block and Markdown fence language aliases."""

from functools import partial
from http.server import SimpleHTTPRequestHandler, ThreadingHTTPServer
import os
from pathlib import Path
import threading

from playwright.sync_api import sync_playwright


ROOT = Path(__file__).resolve().parent
FAMILIES = [
    (('c#', 'C#', 'cs', 'csharp', 'f#', 'fsharp', 'objc'),
     'class Item {} // comment', [('tok-kw', 'class'), ('tok-type', 'Item'), ('tok-com', '// comment')]),
    (('dockerfile', 'docker', 'make', 'makefile', 'mk', 'ruby', 'rb', 'perl', 'pl', 'elixir', 'ex', 'exs', 'r'),
     'echo "hi" # comment', [('tok-kw', 'echo'), ('tok-str', '"hi"'), ('tok-com', '# comment')]),
    (('ps', 'PowerShell'), 'echo "say `"hello`""; if true\n<#\nif true\n#>\necho# tail',
     [('tok-kw', 'echo'), ('tok-str', '"say `"hello`""'), ('tok-kw', 'if'), ('tok-lit', 'true'),
      ('tok-com', '<#\nif true\n#>'), ('tok-kw', 'echo'), ('tok-com', '# tail')]),
    (('powershell',), 'echo @"\n<# data\n  "@\n"@; if true\necho @\'\n# data\n\'@; echo \'it\'\'s\'',
     [('tok-kw', 'echo'), ('tok-str', '@"\n<# data\n  "@\n"@'), ('tok-kw', 'if'), ('tok-lit', 'true'),
      ('tok-kw', 'echo'), ('tok-str', "@'\n# data\n'@"), ('tok-kw', 'echo'), ('tok-str', "'it''s'")]),
    (('cfg', 'editorconfig', 'gitconfig'),
     'name = true\n; comment', [('tok-key', 'name'), ('tok-lit', 'true'), ('tok-com', '; comment')]),
    (('lua', 'haskell', 'hs'),
     'local data = "hi"\nselect SELECT nil -- comment',
     [('tok-kw', 'local'), ('tok-kw', 'data'), ('tok-str', '"hi"'),
      ('tok-lit', 'nil'), ('tok-com', '-- comment')]),
    (('sql',), 'SELECT true -- comment',
     [('tok-kw', 'SELECT'), ('tok-lit', 'true'), ('tok-com', '-- comment')]),
    (('dash', 'no-such-language', ''), 'local let 2 -- comment', []),
    (('sql', 'postgres', 'PostgreSQL'), "AS $$\n/* open-looking -- ' $other$\n\\$$; SELECT $1 -- tail",
     [('tok-kw', 'AS'), ('tok-str', "$$\n/* open-looking -- ' $other$\n\\$$"),
      ('tok-kw', 'SELECT'), ('tok-com', '-- tail')]),
    (('postgresql',), "SELECT $Tag_2$ $tag_2$ /* literal $Tag_2$, $λ$-- literal$λ$, col$tag$ FROM t",
     [('tok-kw', 'SELECT'), ('tok-str', '$Tag_2$ $tag_2$ /* literal $Tag_2$'),
      ('tok-str', '$λ$-- literal$λ$'), ('tok-kw', 'FROM')]),
    (('sqlite', 'mysql'), 'SELECT $$ /* block */ $$', [('tok-kw', 'SELECT'), ('tok-com', '/* block */')]),
]


class QuietHandler(SimpleHTTPRequestHandler):
    def log_message(self, *_):
        pass


def expose(route):
    source = (ROOT / 'web/app.js').read_text()
    end = source.rfind('\n})();')
    assert end > 0
    hook = '\nwindow.__fenceFixture = {codeBlock, markdown};'
    route.fulfill(content_type='text/javascript', body=source[:end] + hook + source[end:])


def check(page):
    for view in ('block', 'fence'):
        for aliases, source, tokens in FAMILIES:
            expected = {'text': source, 'tokens': [list(token) for token in tokens]}
            for lang in aliases:
                actual = page.evaluate(r'''({view, lang, source}) => {
                    const {codeBlock, markdown} = window.__fenceFixture;
                    const node = view === 'block'
                        ? codeBlock(source, lang)
                        : markdown('```' + lang + '\n' + source + '\n```');
                    const code = node.querySelector('pre code');
                    return {text: code.textContent, tokens: Array.from(
                        code.querySelectorAll('span[class^="tok-"]'), el => [el.className, el.textContent])};
                }''', {'view': view, 'lang': lang, 'source': source})
                assert actual == expected, (view, lang, actual, expected)
    print(f'PASS {sum(len(aliases) for aliases, _, _ in FAMILIES)} aliases through code blocks and Markdown fences')


def main():
    handler = partial(QuietHandler, directory=str(ROOT / 'web'))
    with ThreadingHTTPServer(('127.0.0.1', 0), handler) as server:
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
                page.wait_for_function('!!window.__fenceFixture')
                check(page)
                browser.close()
        finally:
            server.shutdown()
            thread.join(timeout=5)


if __name__ == '__main__':
    main()
