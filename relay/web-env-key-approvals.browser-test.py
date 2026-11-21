"""Exercise environment key approval labels and clicks with local mock requests."""

from functools import partial
from http.server import SimpleHTTPRequestHandler, ThreadingHTTPServer
import os
from pathlib import Path
from threading import Thread

from playwright.sync_api import sync_playwright


ROOT = Path(__file__).resolve().parent
HOOK = r'''
  const decisions = [];
  request = async (message) => {
    if (message.type !== 'approval_decision') return null;
    decisions.push(message);
    return {resolved: true};
  };
  window.__fixture = {
    decisions,
    queue(names) {
      $('pair').classList.add('hidden');
      $('app').classList.remove('hidden');
      $('toasts').replaceChildren();
      S.daemonOnline = true;
      S.approvals = [];
      S.approvalLater = false;
      decisions.length = 0;
      addApproval({type: 'approval_requested', approval_id: 'fixture-approval',
        session_id: 'fixture-session', tool_name: 'fixture-tool', tool_id: 'fixture-tool',
        version: 1, memory_mb: 64, capabilities: names.map(name => 'env:' + name),
        already_allowed: []});
      renderApproval();
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
    modal = page.locator('#modal-backdrop')
    labels = modal.locator('.tag.broad')
    confirm = modal.locator('.broad-confirm')

    def queue(*names):
        page.evaluate('names => __fixture.queue(names)', list(names))

    def click(scope):
        control = 'approve' if scope == 'always' else scope
        page.locator(f'#modal-{control}').click()

    def decided(scope):
        page.wait_for_function('__fixture.decisions.length === 1')
        decision = page.evaluate('__fixture.decisions[0]')
        assert decision['approved'] and decision['scope'] == scope, decision

    for name, scope in [
        ('AWS_ACCESS_KEY_ID', 'session'),
        ('aws_access_key_id', 'hour'),
        ('KEY', 'always'),
        ('OPENAI_API_KEY', 'session'),
        ('OPENAIAPIKEY', 'hour'),
        ('ENCRYPTIONKEY', 'always'),
        ('HMACKEY', 'session'),
        ('PRIMARY_KEY_TOKEN', 'session'),
        ('SSH_AUTH_SOCK', 'hour'),
        ('DATABASE_URL', 'session'),
        ('redis_url', 'hour'),
        ('MONGODB_URI', 'always'),
    ]:
        queue(name)
        assert labels.all_text_contents() == [f'env:{name}'], name
        assert modal.get_by_text('broad access: a secret from your environment', exact=False).is_visible(), name
        click(scope)
        assert confirm.is_visible() and page.evaluate('__fixture.decisions.length') == 0, name
        click(scope)
        decided(scope)

    for name in ('MONKEY', 'TURKEY', 'DONKEY', 'HOCKEY', 'JOCKEY', 'HOTKEY', 'PRIMARY_KEY',
                 'foreign_key', 'UNIQUE_KEY', 'CANDIDATE_KEY', 'SORT_KEY', 'PARTITION_KEY', 'HOME',
                 'PUBLIC_URL', 'DATABASE_URL_SAMPLE'):
        queue(name)
        assert labels.count() == 0, name
        click('always')
        decided('always')

    queue('PRIMARY_KEY', 'AWS_ACCESS_KEY_ID')
    assert labels.all_text_contents() == ['env:AWS_ACCESS_KEY_ID']
    click('once')
    decided('once')


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
    print('PASS environment key labels, lasting scopes, and one-time confirmation')


if __name__ == '__main__':
    main()
