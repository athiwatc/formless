#!/usr/bin/env python3
"""Historical call names with shipped DOM/CSS; synthetic text, no daemon."""
import argparse
import functools
import http.server
import json
from pathlib import Path
import threading
from playwright.sync_api import sync_playwright

ROOT = Path(__file__).resolve().parent
parser = argparse.ArgumentParser()
parser.add_argument('--out', type=Path, default=Path('/tmp/formless-tool-chip-1519'))
args = parser.parse_args()
args.out.mkdir(parents=True, exist_ok=True)

class QuietHandler(http.server.SimpleHTTPRequestHandler):
    def log_message(self, *_):
        pass

def expose(route):
    script = (ROOT / 'web/app.js').read_text()
    end = script.rfind('\n})();')
    assert end > 0
    hook = r'''
    window.__chip1519 = (name, args) => {
      $("pair").classList.add("hidden"); $("app").classList.remove("hidden"); setView("chat");
      const call = toolCallChip({function:{name,arguments:args}}); call.id = "fixture-chip";
      const result = toolResultChip("result"); result.id = "result-control";
      const live = h("details",{class:"chip live tree-node",id:"live-control"},h("summary",null,h("span",{class:"tname"},"live name")));
      $("transcript").replaceChildren(h("div",{class:"turn assistant"},h("div",{class:"bubble"},call,result,live)));
    };
'''
    route.fulfill(status=200, content_type='text/javascript', body=script[:end] + hook + script[end:])

server = http.server.ThreadingHTTPServer(('127.0.0.1', 0), functools.partial(QuietHandler, directory=str(ROOT / 'web')))
threading.Thread(target=server.serve_forever, daemon=True).start()
try:
    with sync_playwright() as p:
        browser = p.chromium.launch(executable_path='/usr/bin/chromium', headless=True, args=['--no-sandbox'])
        for width, font in [(320,16),(320,32),(900,16)]:
            page = browser.new_page(viewport={'width':width,'height':1600}, has_touch=width == 320)
            page.set_default_timeout(5000)
            page.route('**/app.js',expose)
            page.goto(f'http://127.0.0.1:{server.server_port}/')
            page.wait_for_function('!!window.__chip1519')
            page.evaluate("font=>document.documentElement.style.fontSize=font+'px'",font)
            arguments = json.dumps({str(n): 'argument_' * 100 + ' ไทย' for n in range(8)}, ensure_ascii=False)
            for index, name in enumerate(['operation_'+'A'*260, '工具_ไทย_👩🏽‍💻_'*15]):
                page.evaluate('([name,args])=>__chip1519(name,args)',[name,arguments])
                chip = page.locator('#fixture-chip')
                summary = chip.locator('summary')
                assert chip.locator('.tname').text_content() == '⚙ '+name
                geometry = chip.locator('.tname').evaluate('''el=>{const r=el.getBoundingClientRect(),s=el.closest('summary').getBoundingClientRect();return [r.left,r.right,s.left,s.right,document.documentElement.scrollWidth,innerWidth]}''')
                assert geometry[0] >= geometry[2]-1 and geometry[1] <= geometry[3]+1, geometry
                assert geometry[4] <= geometry[5]+1, geometry
                preview = chip.locator('.targs').evaluate('el=>[el.clientWidth,el.scrollWidth,getComputedStyle(el).textOverflow,getComputedStyle(el).whiteSpace]')
                assert preview[0]>0 and preview[1]>preview[0],preview
                assert preview[2:]==['ellipsis','nowrap'],preview
                summary.focus()
                summary.press('Enter')
                assert chip.evaluate('el=>el.open')
                assert chip.locator('pre').text_content()==arguments
                overflow = chip.locator('pre').evaluate('el=>getComputedStyle(el).overflowY')
                assert overflow=='auto',overflow
                summary.press('Space')
                assert not chip.evaluate('el=>el.open')
                assert '▸' in summary.evaluate("el=>getComputedStyle(el,'::before').content")
                page.screenshot(path=str(args.out/f'chip-{width}-{font}-{index}.png'))
            page.evaluate('()=>__chip1519("read","{\\"x\\":1}")')
            name = page.locator('#fixture-chip .tname')
            assert name.text_content()=='⚙ read'
            metrics = name.evaluate('el=>[el.getBoundingClientRect().height,parseFloat(getComputedStyle(el).lineHeight)]')
            assert metrics[0] <= metrics[1]+1,metrics
            assert page.locator('#result-control .tname').evaluate('el=>getComputedStyle(el).whiteSpace')=='nowrap'
            assert page.locator('#live-control .tname').evaluate('el=>getComputedStyle(el).whiteSpace')=='normal'
            assert page.locator('#live-control summary').evaluate('el=>getComputedStyle(el).flexWrap')=='wrap'
            print(f'PASS historical names/argument clipping/disclosure/control styles {width}px / {font}px',flush=True)
            page.close()
        browser.close()
finally:
    server.shutdown()
    server.server_close()
