"""Exercise shipped approval buttons with an in-memory, local transport."""
import argparse
from functools import partial
from http.server import SimpleHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from threading import Thread

from playwright.sync_api import expect, sync_playwright

from browser_fixture import chromium_executable


HOOK = r'''
  const decisions = [];
  request = async (req) => {
    if (req.type !== 'approval_decision') return null;
    decisions.push(req);
    return {resolved: true};
  };
  window.__fixture = {
    decisions,
    queue(capability, alreadyAllowed, detail) {
      $('pair').classList.add('hidden');
      $('app').classList.remove('hidden');
      S.daemonOnline = true;
      S.approvals = [];
      S.approvalLater = false;
      decisions.length = 0;
      addApproval({type: 'approval_requested', approval_id: 'fixture-approval',
        session_id: 'fixture-session', tool_name: 'fixture-tool', tool_id: 'fixture-tool',
        version: 1, memory_mb: 64, capabilities: [capability], detail,
        already_allowed: alreadyAllowed ? [capability] : []});
      renderApproval();
    }
  };
'''


class QuietServerHandler(SimpleHTTPRequestHandler):
    def log_message(self, *_):
        pass


def check(page):
    def queue(capability, already_allowed=False, detail=None):
        page.evaluate('args => __fixture.queue(...args)', [capability, already_allowed, detail])

    def decisions():
        return page.evaluate('__fixture.decisions')

    def click(scope):
        control = 'approve' if scope == 'always' else scope
        page.locator(f'#modal-{control}').click()

    def check_decision(scope):
        page.wait_for_function('__fixture.decisions.length === 1')
        sent = decisions()[0]
        assert sent['approved'] and sent['scope'] == scope, sent

    modal = page.locator('#modal-backdrop')
    confirm = modal.locator('.broad-confirm')
    for capability, reason, scope in [
        ('process:/usr/bin/bash', 'an interpreter or shell', 'session'),
        ('process:python3.11', 'an interpreter or shell', 'hour'),
        (r'process:C:\Python311\PYTHONW.EXE', 'an interpreter or shell', 'always'),
        ('process:py3', 'an interpreter or shell', 'session'),
        ('process:nodejs', 'an interpreter or shell', 'hour'),
        ('process:/usr/bin/su', 'an interpreter or shell', 'always'),
        ('process:curl', 'it can reach any host and write files', 'hour'),
        (r'process:C:\Program Files\Git\cmd\git.exe:status|log', 'git runs hooks', 'always'),
        ('process:make', 'a build tool runs whatever the build files say', 'session'),
    ]:
        queue(capability)
        warning = modal.get_by_text(f'broad access: {reason}', exact=False)
        assert warning.is_visible(), capability
        click(scope)
        assert confirm.is_visible() and confirm.get_attribute('role') == 'alert', capability
        assert decisions() == [], capability
        click(scope)
        check_decision(scope)

    for capability in ['process:ls', 'process:pytest', 'process:nodejs-helper', 'process:Ksh']:
        queue(capability)
        assert modal.locator('.tag.broad').count() == 0, capability
        click('always')
        check_decision('always')

    queue('process:bash', already_allowed=True)
    assert modal.get_by_text('already allowed', exact=False).is_visible()
    click('session')
    check_decision('session')

    queue('process:bash')
    click('session')
    click('hour')
    assert decisions() == [] and confirm.is_visible()
    click('once')
    check_decision('once')

    for key, scope in [('y', 'always'), ('s', 'session'), ('1', 'hour'), ('Enter', 'always')]:
        queue('process:bash')
        page.locator('#modal-approve').focus()
        page.keyboard.down(key)
        assert confirm.is_visible() and decisions() == [], key
        page.keyboard.down(key)
        assert decisions() == [], f'holding {key} cannot confirm a lasting grant'
        page.keyboard.up(key)
        page.keyboard.press(key)
        check_decision(scope)

    queue('process:bash')
    page.locator('#modal-approve').focus()
    page.keyboard.down('Enter')
    page.keyboard.down('Control')
    page.keyboard.down('Enter')
    assert confirm.is_visible() and decisions() == []
    page.keyboard.up('Enter')
    page.keyboard.up('Control')
    page.keyboard.press('Enter')
    check_decision('always')

    page.set_viewport_size({'width': 460, 'height': 430})
    queue('process:curl', detail='Long fixture explanation.\n' * 60)
    click('hour')
    expect(confirm).to_be_in_viewport()
    assert decisions() == []
    click('hour')
    check_decision('hour')


def run(app):
    web = Path(__file__).resolve().parent / 'web'
    server = ThreadingHTTPServer(('127.0.0.1', 0), partial(QuietServerHandler, directory=web))
    thread = Thread(target=server.serve_forever, daemon=True)
    thread.start()
    try:
        with sync_playwright() as playwright:
            browser = playwright.chromium.launch(
                executable_path=chromium_executable(playwright), headless=True, args=['--no-sandbox']
            )
            page = browser.new_page(viewport={'width': 1050, 'height': 900})
            page.set_default_timeout(5000)

            def expose(route):
                source = (app or web / 'app.js').read_text()
                end = source.rfind('\n})();')
                assert end > 0
                route.fulfill(content_type='text/javascript', body=source[:end] + HOOK + source[end:])

            page.route('**/app.js', expose)
            page.goto(f'http://127.0.0.1:{server.server_port}/')
            page.wait_for_function('!!window.__fixture')
            check(page)
            browser.close()
    finally:
        server.shutdown()
        server.server_close()
        thread.join(timeout=5)


if __name__ == '__main__':
    parser = argparse.ArgumentParser()
    parser.add_argument('--app', type=Path, help='Optional baseline JS for a negative control')
    run(parser.parse_args().app)
    print('PASS browser process warnings, repeated scope, one-time and existing grants')
