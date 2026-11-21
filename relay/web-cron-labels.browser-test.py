#!/usr/bin/env python3
"""Shipped cron labels in Settings and the New Job preview; no daemon or jobs.

Run with Python Playwright and /usr/bin/chromium. --app permits a baseline
negative control; --case selects one issue's regression independently.
"""
import argparse
import functools
import http.server
from pathlib import Path
import threading
from playwright.sync_api import sync_playwright

from browser_fixture import chromium_executable

ROOT = Path(__file__).resolve().parent
parser = argparse.ArgumentParser()
parser.add_argument('--app', type=Path)
parser.add_argument('--case', choices=['all', 'hourly', 'yearly', 'weekdays', 'hour-steps'], default='all')
args = parser.parse_args()

HOURLY = [('0 * * * *', 'every hour'), ('5 * * * *', 'every hour at minute 05'),
          ('59 * * * *', 'every hour at minute 59'), ('@hourly', 'every hour')]
YEARLY = [('@yearly', 'every year on January 1 at 0:00'),
          ('@annually', 'every year on January 1 at 0:00'),
          ('@midnight', 'every day at 0:00'),
          ('0 0 1 1 *', 'every year on January 1 at 0:00'),
          ('0 9 1 1 *', 'every year on January 1 at 9:00'),
          ('5 9 1 1 *', 'every year on January 1 at 9:05')]
WEEKDAYS = [('0 9 * * 1-5', 'every weekday at 9:00'),
            ('0 9 * * MON-FRI', 'every weekday at 9:00'),
            ('30 8 * * Sun', 'every Sunday at 8:30'),
            ('0 9 * * mOn,wed,FRI', 'every Monday, Wednesday, Friday at 9:00'),
            ('0 0 * * 0,7', 'every Sunday at 0:00'),
            ('0 9 * * 1,1', 'every Monday at 9:00'),
            ('5 9 * * 7,MON,0,1,SUN,Tue,2', 'every Sunday, Monday, Tuesday at 9:05'),
            ('0 9 * * MON,XXX', '0 9 * * MON,XXX'),
            ('0 9 1 * MON', '0 9 1 * MON'),
            ('0 9 * * MON-FRI/2', '0 9 * * MON-FRI/2')]
HOUR_STEPS = [('0 */2 * * *', 'every 2 hours'),
              ('30 */6 * * *', 'every 6 hours at minute 30'),
              ('5 */3 * * *', 'every 3 hours at minute 05'),
              ('0 */1 * * *', 'every hour'), ('5 */1 * * *', 'every hour at minute 05'),
              ('0 */7 * * *', 'every 7 hours starting at 0:00 each day'),
              ('30 */7 * * *', 'every 7 hours starting at 0:30 each day'),
              ('5 */24 * * *', 'every day at 0:05'),
              ('0 */0 * * *', '0 */0 * * *'),
              ('60 */2 * * *', '60 */2 * * *'),
              ('0 */4294967296 * * *', '0 */4294967296 * * *'),
              ('0 */2 * * MON', '0 */2 * * MON')]
CONTROLS = [('@daily', 'every day at 0:00'), ('0 9 * * *', 'every day at 9:00'),
            ('@weekly', 'every Sunday at 0:00'), ('@monthly', 'on the 1st of each month at 0:00'),
            ('*/15 * * * *', 'every 15 minutes'),
            ('0 9 1 * *', 'on day 1 of each month at 9:00'),
            ('0 9 2 1 *', '0 9 2 1 *'), ('0 9 1 2 *', '0 9 1 2 *'),
            ('0 9 1 1 1', '0 9 1 1 1'), ('0-5 * * * *', '0-5 * * * *'),
            ('every monday morning', 'every monday morning'), ('@unknown', '@unknown'),
            ('  0 9 * * *  ', 'every day at 9:00')]
CASES = {'hourly': HOURLY, 'yearly': YEARLY, 'weekdays': WEEKDAYS, 'hour-steps': HOUR_STEPS,
         'all': HOURLY + YEARLY + WEEKDAYS + HOUR_STEPS + CONTROLS}[args.case]


class Quiet(http.server.SimpleHTTPRequestHandler):
    def log_message(self, *_):
        pass


def expose(route):
    source = (args.app or ROOT / 'web/app.js').read_text()
    end = source.rfind('\n})();')
    assert end > 0
    hook = '''
    window.__cronFixture={describeCron,setup(jobs){
      $("pair").classList.add("hidden");$("app").classList.remove("hidden");
      S.view="settings";S.local=true;S.jobs=jobs;renderChrome();renderView();renderSettings();
    }};
    '''
    route.fulfill(status=200, content_type='text/javascript', body=source[:end] + hook + source[end:])


server = http.server.ThreadingHTTPServer(('127.0.0.1', 0), functools.partial(Quiet, directory=str(ROOT / 'web')))
thread = threading.Thread(target=server.serve_forever, daemon=True)
thread.start()
try:
    with sync_playwright() as p:
        browser = p.chromium.launch(executable_path=chromium_executable(p), headless=True, args=['--no-sandbox'])
        page = browser.new_page(viewport={'width':1000, 'height':900})
        page.set_default_timeout(5000)
        errors = []
        page.on('pageerror', lambda e: errors.append(str(e)))
        page.route('**/app.js', expose)
        page.goto(f'http://127.0.0.1:{server.server_port}/')
        page.wait_for_function('!!window.__cronFixture')
        for expr, expected in CASES:
            actual = page.evaluate('e=>__cronFixture.describeCron(e)', expr)
            assert actual == expected, (expr, actual, expected)
        print(f'PASS {len(CASES)} shipped cron formatter cases', flush=True)
        jobs = [dict(id=f'job-{i}', prompt=f'Schedule fixture {i}', cron=expr,
                     author='user', enabled=False, approved=True) for i, (expr, _) in enumerate(CASES)]
        page.evaluate('jobs=>__cronFixture.setup(jobs)', jobs)
        labels = page.locator('.settings-records').first.locator('.record-prose .muted').all_text_contents()
        assert labels == [expected + ' · next - · user' for _, expected in CASES], labels
        print(f'PASS {len(CASES)} actual Settings job labels', flush=True)
        page.get_by_role('button', name='New job', exact=True).click()
        page.locator('#ask-backdrop:not(.hidden)').wait_for()
        body = page.locator('#ask-body')
        schedule = body.locator('label').filter(has=page.locator('span:text-is("Schedule")')).locator('select')
        if args.case != 'yearly':
            schedule.select_option('hourly')
            assert body.locator('div.muted').inner_text() == 'Runs every hour (0 * * * *)'
            print('PASS actual hourly preset preview', flush=True)
        schedule.select_option('cron')
        custom = body.get_by_label('Schedule: plain English or cron (minute hour day month weekday)', exact=True)
        for expr, expected in CASES:
            custom.fill(expr)
            assert body.locator('div.muted').inner_text() == f'Runs {expected} ({expr.strip()})', expr
        print(f'PASS {len(CASES)} actual custom cron previews', flush=True)
        # Cancel never invokes add_job, approval or an actual scheduled task.
        page.keyboard.press('Escape')
        page.locator('#ask-backdrop').wait_for(state='hidden')
        assert not errors, errors
        browser.close()
finally:
    server.shutdown()
    server.server_close()
    thread.join(timeout=5)
