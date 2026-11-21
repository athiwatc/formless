#!/usr/bin/env python3
"""Six shipped Markdown heading depths in Chromium, with synthetic local text."""
import argparse
import functools
import http.server
import json
from pathlib import Path
import threading
from playwright.sync_api import sync_playwright

from browser_fixture import artifact_dir, chromium_executable

ROOT=Path(__file__).resolve().parent
parser=argparse.ArgumentParser()
parser.add_argument('--out',type=Path,default=artifact_dir('/tmp/formless-headings-1559'))
args=parser.parse_args()
args.out.mkdir(parents=True,exist_ok=True)

class QuietHandler(http.server.SimpleHTTPRequestHandler):
    def log_message(self,*_):
        pass

def expose(route):
    script=(ROOT/'web/app.js').read_text()
    end=script.rfind('\n})();')
    assert end>0
    hook=r'''
    window.__headings1559 = source => {
      $("pair").classList.add("hidden"); $("app").classList.remove("hidden"); setView("chat");
      $("transcript").replaceChildren(h("div",{class:"turn"},h("div",{class:"bubble"},markdown(source))));
    };
'''
    route.fulfill(status=200,content_type='text/javascript',body=script[:end]+hook+script[end:])

labels=[f'Depth {level} ไทย 日本語 — '+'Unicode_heading_'*8 for level in range(1,7)]
markdown_source=('\n\n').join('#'*level+' '+labels[level-1]+' **bold** [link](https://example.invalid)' for level in range(1,7))
server=http.server.ThreadingHTTPServer(('127.0.0.1',0),functools.partial(QuietHandler,directory=str(ROOT/'web')))
thread=threading.Thread(target=server.serve_forever,daemon=True)
thread.start()
bidi_records=[]
try:
    with sync_playwright() as p:
        browser=p.chromium.launch(executable_path=chromium_executable(p),headless=True,args=['--no-sandbox'])
        for width in [320,900]:
            for font in [16,32]:
                page=browser.new_page(viewport={'width':width,'height':1400})
                page.set_default_timeout(5000)
                page.route('**/app.js',expose)
                page.goto(f'http://127.0.0.1:{server.server_port}/')
                page.wait_for_function('!!window.__headings1559')
                title=page.locator('#session-title').evaluate('el=>el.outerHTML')
                page.evaluate("size=>document.documentElement.style.fontSize=size+'px'",font)
                page.evaluate('value=>__headings1559(value)',markdown_source)
                assert page.locator('#session-title').evaluate('el=>el.outerHTML')==title
                signatures=[]
                for index,label in enumerate(labels):
                    heading=page.get_by_role('heading',level=index+3)
                    assert heading.count()==1,(index,page.locator('.md').inner_html())
                    assert heading.get_attribute('data-heading-level')==str(index+1)
                    assert heading.text_content()==label+' bold link'
                    assert heading.locator('strong').text_content()=='bold'
                    assert heading.locator('a').get_attribute('href')=='https://example.invalid'
                    style=heading.evaluate('''el=>{const s=getComputedStyle(el);return [s.fontSize,s.fontWeight,s.fontStyle,s.borderLeftStyle,s.borderLeftWidth,s.borderBottomWidth,s.marginLeft];}''')
                    assert float(style[0].removesuffix('px'))>=font
                    signatures.append(style)
                    heading.scroll_into_view_if_needed()
                    bounds=heading.evaluate('''el=>{const r=el.getBoundingClientRect(),p=document.querySelector('#transcript').getBoundingClientRect();return {left:r.left,right:r.right,min:p.left,max:p.right,client:el.clientWidth,scroll:el.scrollWidth};}''')
                    assert bounds['left']>=bounds['min']-1 and bounds['right']<=bounds['max']+1 and bounds['scroll']<=bounds['client']+1,bounds
                assert all(a!=b for a,b in zip(signatures,signatures[1:])),signatures
                page.locator('#transcript').evaluate('el=>el.scrollTop=0')
                page.screenshot(path=str(args.out/f'headings-{width}-{font}.png'))
                print(f'PASS six heading levels {width}px / root {font}px',flush=True)
                for name,label,edge in [
                    ('arabic','مرحبا (v2)!','right'),('hebrew','שלום (v2)!','right'),
                    ('neutral-arabic','(123) مرحبا!','right'),('neutral-hebrew','(123) שלום!','right'),
                    ('latin-first','Title: שלום (v2)!','left'),
                ]:
                    page.evaluate('value=>__headings1559(value)','\n\n'.join('#'*level+' '+label for level in range(1,7)))
                    for level in range(1,7):
                        heading=page.get_by_role('heading',level=level+2)
                        assert heading.count()==1 and heading.text_content()==label
                        measured=heading.evaluate('''el=>{
                          const s=getComputedStyle(el),r=el.getBoundingClientRect(),range=document.createRange();
                          range.selectNodeContents(el);
                          const rects=[...range.getClientRects()].filter(r=>r.width>0&&r.height>0);
                          const top=Math.min(...rects.map(r=>r.top));
                          const first=rects.filter(r=>Math.abs(r.top-top)<1);
                          return {bidi:s.unicodeBidi,align:s.textAlign,
                            left:r.left+parseFloat(s.borderLeftWidth)+parseFloat(s.paddingLeft),
                            right:r.right-parseFloat(s.borderRightWidth)-parseFloat(s.paddingRight),
                            textLeft:Math.min(...first.map(r=>r.left)),textRight:Math.max(...first.map(r=>r.right))};
                        }''')
                        # Computed direction alone cannot report plaintext's per-paragraph base direction.
                        gap=abs(measured['textRight']-measured['right']) if edge=='right' else abs(measured['textLeft']-measured['left'])
                        passed=measured['bidi']=='plaintext' and measured['align']=='start' and gap<=1
                        bidi_records.append(dict(width=width,font=font,sample=name,level=level,edge=edge,gap=gap,passed=passed,**measured))
                    page.locator('#transcript').evaluate('el=>el.scrollTop=0')
                    page.screenshot(path=str(args.out/f'bidi-{name}-{width}-{font}.png'))
                print(f'CHECK all six bidi depths / five language cases {width}px / root {font}px',flush=True)
                page.close()
        browser.close()
    (args.out/'bidi-report.json').write_text(json.dumps(bidi_records,ensure_ascii=False,indent=2))
    failures=[record for record in bidi_records if not record['passed']]
    assert not failures,(len(failures),failures[:6])
    print(f'PASS {len(bidi_records)} bidi computed-style and physical-alignment checks',flush=True)
finally:
    server.shutdown()
    server.server_close()
    thread.join(timeout=5)
