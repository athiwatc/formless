"""Local shipped-UI fixture support; no daemon, credentials or remote sources."""
import functools
import http.server
from pathlib import Path
import threading
from playwright.sync_api import sync_playwright

from browser_fixture import chromium_executable

class Quiet(http.server.SimpleHTTPRequestHandler):
    def log_message(self, *_):
        pass

def run(root, hook, check, app=None):
    server = http.server.ThreadingHTTPServer(('127.0.0.1',0), functools.partial(Quiet,directory=str(root/'web')))
    thread=threading.Thread(target=server.serve_forever,daemon=True);thread.start()
    try:
        with sync_playwright() as p:
            browser=p.chromium.launch(executable_path=chromium_executable(p),headless=True,args=['--no-sandbox'])
            page=browser.new_page(viewport={'width':1000,'height':900})
            def expose(route):
                source=(app or root/'web/app.js').read_text();end=source.rfind('\n})();');assert end>0
                route.fulfill(status=200,content_type='text/javascript',body=source[:end]+hook+source[end:])
            page.route('**/app.js',expose)
            page.goto(f'http://127.0.0.1:{server.server_port}/')
            page.wait_for_function('!!window.__fixture')
            check(page)
            browser.close()
    finally:
        server.shutdown();server.server_close();thread.join(timeout=5)

import argparse
parser=argparse.ArgumentParser()
parser.add_argument('--app',type=Path,help='Optional baseline JS for negative control')
parser.add_argument('--case',choices=['all','python','config','shell'],default='all')
args=parser.parse_args()
hook='\nwindow.__fixture={codeBlock,markdown};'

def render(page, source, lang, kind):
    return page.evaluate(r'''({source,lang,kind})=>{
      const node=kind==='fence'?__fixture.markdown('```'+lang+'\n'+source+'\n```'):__fixture.codeBlock(source,lang);
      const code=node.querySelector('pre code');
      return {text:code.textContent,comments:[...code.querySelectorAll('.tok-com')].map(c=>c.textContent)};
    }''',{'source':source,'lang':lang,'kind':kind})

def check(page):
    cases=[]
    if args.case in ('all','python'):
      for lang in ['py','python','python3','PYTHON']:
        for source,comments in [
          ('x=1#keep the rest',['#keep the rest']),('print(x)#done',['#done']),
          ('def f():#entry',['#entry']),('# at the start',['# at the start']),
          ('x = 1  # spaced',['# spaced']),('s="x#y"; x=1#outside',['#outside']),
          ("s='# inside'",[]),('x=1#first\ny=2',['#first']),
        ]:cases.append((lang,source,comments))
    if args.case in ('all','config'):
      for lang in ['yaml','yml','toml','env','dotenv']:
        for source,comments in [
          ('key: a;b',[]),(r'PATH=C:\Windows;C:\Windows\System32',[]),
          ('key="a;b"',[]),('key: foo#bar',[]),('key: foo # comment',['# comment']),
        ]:cases.append((lang,source,comments))
      for lang in ['ini','properties','conf']:
        for source,comments in [('; comment',['; comment']),('key=a;b',[';b']),('key="a;b"',[])]:
          cases.append((lang,source,comments))
      for lang in ['sh','bash','shell']:
        for source,comments in [('echo foo#bar',[]),('${#arr}',[]),('echo foo # comment',['# comment'])]:
          cases.append((lang,source,comments))
    if args.case in ('all','shell'):
      for lang in ['sh','bash','zsh','fish','shell','console']:
        for source,comments in [
          ('echo a;# done',['# done']),('true|# note',['# note']),('true&# note',['# note']),
          ('echo a &&# note',['# note']),('(# note',['# note']),('echo a; # spaced',['# spaced']),
          (r'echo a\;#literal',[]),(r'echo a\ #literal',[]),(r'echo a\\;# note',['# note']),
          ('echo foo#bar',[]),('echo ${#arr} $#',[]),("echo 'a;# quoted'",[]),
          ('echo [[ value;# outside',['# outside']),
          ('echo ${unset:-x;#literal};# outside',['# outside']),
          ('echo ${outer:-${inner:-x|#literal}};# outside',['# outside']),
          ('[[ "#foo" == @(other|#foo) ]];# outside',['# outside']),
          ('[[ "#foo" =~ (other|#foo) ]];# outside',['# outside']),
          ('echo @(other|#foo);# outside',['# outside']),
          (r'echo ${name:-x\};#literal};# outside',['# outside']),
        ]:cases.append((lang,source,comments))
      for lang in ['yaml','toml','ini','powershell']:
        cases.append((lang,'a|# literal',[]))
      for lang,delimiter in [('sh','$"EOF"'),('zsh','$"EOF"'),('sh',"$'EOF'")]:
        cases.append((lang,f'cat <<{delimiter}\nhello;#literal\n$EOF\ntrue;# outside',['# outside']))
      for source,comments in [
        ('cat <<EOF\nhello;#literal\nEOF\nprintf done;# real',['# real']),
        ('cat <<\'FIRST\' <<-E"ND"\n\n# data\nFIRST\n\tsecond |# data\n\tEND\ntrue;# outside',['# outside']),
        ("cat <<$'E\\x4fF'\nhello;#literal\nEOF\nprintf done;# real",['# real']),
        ("cat <<$'caf\\303\\251'\nhello;#literal\ncafé\nprintf done;# real",['# real']),
        ('cat <<$"EOF"\nhello;#literal\nEOF\nprintf done;# real',['# real']),
        ("cat <<EOF\nleft\\\nEOF\nhello;#literal\nEOF\nprintf done;# real",['# real']),
        ("cat <<EOF\nE\\\nOF\nprintf done;# real",['# real']),
        ("cat <<'EOF'\nleft\\\nEOF\nprintf done;# real",['# real']),
        ("cat <<EOF\r\nhello;#literal\r\nEOF\r\nprintf done;# real\r",['# real\r']),
        ("cat <<''\n# data\n\ntrue;# outside",['# outside']),
        ('cat <<EOF\n# literal through the end',[]),
        ('cat <<< literal\ntrue;# outside',['# outside']),
        (r'cat \<< literal'+'\ntrue;# outside',['# outside']),
        ('cat <<EOF # header \\\n# data\nEOF\n# outside',['# header \\', '# outside']),
        ('echo ${unset:-$(case a in a) { :; };; esac) x;#literal};# outside',['# outside']),
        ('echo ${unset:-$(if true; then case a in (esac) :;; esac; fi) x;#literal};# outside',['# outside']),
        ('echo ${unset:-$(echo hi;# inside\n) x;#literal};# outside',['# inside','# outside']),
        ('[[ a == a ||# condition\n b == c ]];# outside',['# condition','# outside']),
        ('[[ a =~ (a||#literal) ]];# outside',['# outside']),
        ('if [[ a =~ a||#literal ]]; then :; fi;# outside',['# outside']),
        ("printf '%s' foo\\\n#literal\nprintf done;# real",['# real']),
        ("printf '%s' foo;\\\n# continued boundary",['# continued boundary']),
        ("echo 'a\n;# inside quote';# outside",['# outside']),
        ("printf '%s' $'it\\'s #inside' # outside",['# outside']),
      ]:cases.append(('bash',source,comments))
    for kind in ['block','fence']:
      for lang,source,comments in cases:
        actual=render(page,source,lang,kind)
        text=source.replace('\r\n','\n').removesuffix('\r') if kind=='fence' else source
        expected_comments=[c.removesuffix('\r') for c in comments] if kind=='fence' else comments
        assert actual=={'text':text,'comments':expected_comments},(kind,lang,source,actual,expected_comments)
    print(f'PASS {len(cases)} comment/source controls through both shipped codeBlock and Markdown fence DOM ({2*len(cases)} views)')
run(Path(__file__).resolve().parent,hook,check,args.app)
