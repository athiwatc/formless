#!/usr/bin/env python3
"""Local Chromium check of shipped code-copy controls/CSS; no server or daemon."""
import argparse
from pathlib import Path
from playwright.sync_api import sync_playwright

from browser_fixture import chromium_executable

root = Path(__file__).resolve().parent
parser = argparse.ArgumentParser()
parser.add_argument('--css', type=Path, help='Optional baseline CSS for a negative control')
args = parser.parse_args()
source = (root / 'web/app.js').read_text()

def function(name):
    start = source.index('  function ' + name + '(')
    return source[start:source.index('\n  }', start) + 4]

with sync_playwright() as p:
    browser = p.chromium.launch(executable_path=chromium_executable(p), headless=True, args=['--no-sandbox'])
    page = browser.new_page(viewport={'width': 900, 'height': 700})
    page.set_content('<button id="before">Before</button><main></main><button id="after">After</button>')
    page.add_style_tag(content=(args.css or root / 'web/app.css').read_text())
    page.add_script_tag(content='''
      const copied = [];
      Object.defineProperty(navigator, 'clipboard', {value:{writeText: async text => {copied.push(text);}}});
      const configureTextInput = () => {};
      const highlight = text => [text];
      const toast = () => {};
    ''' + '\n'.join(function(name) for name in ['h', 'copyButton', 'codeBlock']) + '''
      document.querySelector('main').append(codeBlock('Copy remains literal\\nconst x = 1;\\n', 'js'));
      const diagram = h('div', {class:'diagram'}, h('div', {class:'lang'}, copyButton('diagram source')));
      document.querySelector('main').append(diagram);
    ''')
    button = page.locator('.codeblock .copy')
    page.mouse.move(899, 699)
    assert button.evaluate('e => getComputedStyle(e).opacity') == '0', 'idle hidden'
    page.locator('#before').focus()
    page.keyboard.press('Tab')
    assert button.evaluate('e => e === document.activeElement && e.matches(":focus-visible")'), 'native Tab focus'
    assert button.evaluate('e => getComputedStyle(e).opacity') == '1', 'keyboard focus reveals Copy'
    assert button.evaluate('e => getComputedStyle(e).outlineStyle') != 'none', 'visible focus outline'
    for key in ['Enter', 'Space']:
        page.keyboard.press(key)
    assert page.evaluate('copied') == ['Copy remains literal\nconst x = 1;\n'] * 2, 'native activation copies exact code'
    page.locator('#after').focus()
    assert button.evaluate('e => getComputedStyle(e).opacity') == '0', 'blur restores idle'
    button.hover()
    assert button.evaluate('e => getComputedStyle(e).opacity') == '1', 'hover still visible'
    page.mouse.move(899, 699)
    assert page.locator('.diagram .copy').evaluate('e => getComputedStyle(e).opacity') == '1', 'diagram control stays visible'
    print('PASS keyboard focus/outline, Enter/Space exact copy, blur, hover, diagram visibility')
    browser.close()
