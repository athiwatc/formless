"""Exercise approval control markers and raw identities with local mock requests."""

from functools import partial
from http.server import SimpleHTTPRequestHandler, ThreadingHTTPServer
import os
from pathlib import Path
from threading import Thread

from playwright.sync_api import sync_playwright


ROOT = Path(__file__).resolve().parent
FOLDER = '/fixture/dir\u200c\ufeff'
FILE = FOLDER + '/config\u200b.toml'
CAP = 'fs:read:' + FILE
TOOL = 'read\u00adtool\u2060'
SUMMARY = 'می\u200cخواهم 👩\u200d💻'
HOOK = r'''
  const decisions = [];
  request = async message => {
    if (message.type !== 'approval_decision') return null;
    decisions.push(message);
    return {resolved: true};
  };
  window.__fixture = {
    decisions,
    review: branchSummaryReviewText,
    queue(event) {
      $('pair').classList.add('hidden');
      $('app').classList.remove('hidden');
      S.daemonOnline = true;
      S.approvals = [];
      S.approvalLater = false;
      addApproval(event);
      renderApproval();
      return S.approvals[0];
    }
  };
'''


class QuietHandler(SimpleHTTPRequestHandler):
    def log_message(self, *_):
        pass


def expose(route):
    source = (ROOT / 'web/app.js').read_text()
    end = source.rfind('\n})();')
    assert end > 0
    route.fulfill(content_type='text/javascript', body=source[:end] + HOOK + source[end:])


def check(page):
    event = {
        'type': 'approval_requested', 'approval_id': 'fixture-approval', 'session_id': 'fixture',
        'tool_id': 'fixture-tool', 'tool_name': TOOL, 'version': 1, 'memory_mb': 64,
        'capabilities': [CAP], 'summary': SUMMARY, 'detail': 'Command: inspect\u2061',
        'code_preview': 'let payload = "tag\U000e007f";',
        'context': {
            'sources': [{'tool': 'fetch\u180e', 'kind': 'url', 'origin': 'https://source.example'}],
            'call': {
                'paths': [FILE], 'urls': [], 'arguments': {'path': FILE, 'tag': '\U000e0061'},
            },
        },
        'path_options': [{'capability': CAP, 'choices': [
            {'kind': 'file', 'path': FILE},
            {'kind': 'folder', 'path': FOLDER},
        ]}],
    }
    raw = page.evaluate('event => __fixture.queue(event)', event)
    assert raw['tool_name'] == TOOL and raw['capabilities'] == [CAP]
    assert raw['context'] == event['context'] and raw['code'] == event['code_preview']
    body = page.locator('#modal-body')
    text = body.text_content()
    for marker in ('⟨U+00AD⟩', '⟨U+180E⟩', '⟨U+200B⟩', '⟨U+200C⟩', '⟨U+2060⟩',
                   '⟨U+2061⟩', '⟨U+FEFF⟩', '⟨U+E0061⟩', '⟨U+E007F⟩'):
        assert marker in text, (marker, text)
    assert SUMMARY in text, text
    scope = body.locator('select')
    assert 'config⟨U+200B⟩.toml' in scope.get_attribute('aria-label')
    assert 'dir⟨U+200C⟩⟨U+FEFF⟩' in scope.locator('option').nth(1).text_content()
    scope.select_option('1')
    page.locator('#modal-session').click()
    page.wait_for_function('__fixture.decisions.length === 1')
    decision = page.evaluate('__fixture.decisions[0]')
    assert decision['approved'] and decision['scope'] == 'session', decision
    assert decision['paths'] == {CAP: FOLDER}, decision
    exact_review = page.evaluate(
        '({prose, model}) => __fixture.review({source_name:"source\\u200bname", target_name:"target"},'
        '{source_id:"s", target_id:"t", usage:{cost:0, unpriced:[model]}, projection:{}, candidate:prose})',
        {'prose': SUMMARY, 'model': 'named/model\U000e0061'},
    )
    assert 'source⟨U+200B⟩name (s)' in exact_review
    assert 'Unpriced: named/model⟨U+E0061⟩' in exact_review
    assert SUMMARY in exact_review


def main():
    handler = partial(QuietHandler, directory=str(ROOT / 'web'))
    with ThreadingHTTPServer(('127.0.0.1', 0), handler) as server:
        thread = Thread(target=server.serve_forever, daemon=True)
        thread.start()
        try:
            with sync_playwright() as p:
                browser = p.chromium.launch(
                    executable_path=os.environ.get('FORMLESS_CHROMIUM_EXECUTABLE', '/usr/bin/chromium'),
                    args=['--no-sandbox'],
                )
                page = browser.new_page(viewport={'width': 1050, 'height': 900})
                page.set_default_timeout(5000)
                page.route('**/app.js', expose)
                page.goto(f'http://127.0.0.1:{server.server_port}/')
                page.wait_for_function('!!window.__fixture')
                check(page)
                browser.close()
        finally:
            server.shutdown()
            thread.join(timeout=5)
    print('PASS approval markers, prose shaping, scope labels and raw decisions')


if __name__ == '__main__':
    main()
