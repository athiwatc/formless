import { readFileSync } from 'node:fs';
import vm from 'node:vm';
import test from 'node:test';
import assert from 'node:assert/strict';

const source = readFileSync(new URL('./web/app.js', import.meta.url), 'utf8');
function fn(name) {
  const start = source.search(new RegExp('  (?:async )?function ' + name + '\\('));
  assert.ok(start >= 0, name);
  return source.slice(start, source.indexOf('\n  }', start) + 4);
}
function fixture(language = 'en', browser = 'en-US') {
  const c = vm.createContext({
    S: { settings: { tui: { language } } }, navigator: { language: browser }, Date,
    document: { documentElement: { lang: 'en' } },
  });
  vm.runInContext(['uiLang', 'uiLocale', 'uiFormat', 'uiFixed', 'uiMoney', 'applyLanguage', 'clock', 'humanBytes', 'usageText'].map(fn).join('\n'), c);
  return c;
}

test('UI locale and HTML language follow explicit, Auto and fallback choices', () => {
  for (const [language, browser, effective] of [
    ['en', 'th-TH', 'en'], ['th', 'en-US', 'th'],
    ['auto', 'th-TH', 'th'], ['auto', 'de-DE', 'en'], ['unknown', 'th-TH', 'en'],
  ]) {
    const c = fixture(language, browser);
    assert.equal(c.uiLang(), effective);
    assert.equal(c.uiLocale(), `${effective === 'th' ? 'th-TH' : 'en-US'}-u-ca-gregory-nu-latn`);
    c.applyLanguage();
    assert.equal(c.document.documentElement.lang, effective);
    assert.equal(c.applyLanguage(), false, 'unchanged language does not trigger another full render');
    c.S.settings.tui.language = effective === 'th' ? 'en' : 'th';
    assert.equal(c.applyLanguage(), true);
    assert.equal(c.document.documentElement.lang, c.uiLang());
  }
});

test('English and Thai dates use Gregorian years and Latin digits', () => {
  for (const language of ['en', 'th']) {
    const c = fixture(language);
    const locale = language === 'th' ? 'th-TH-u-ca-gregory-nu-latn' : 'en-US-u-ca-gregory-nu-latn';
    const options = { year: 'numeric', month: 'long', day: 'numeric', timeZone: 'UTC' };
    for (const iso of ['1970-01-01T12:00:00Z', '2024-02-29T12:00:00Z', '2026-12-31T20:00:00Z']) {
      const date = new Date(iso);
      const text = c.uiFormat(date, options);
      assert.equal(text, new Intl.DateTimeFormat(locale, options).format(date));
      assert.ok(text.includes(String(date.getUTCFullYear())), text);
      assert.equal(text.includes(String(date.getUTCFullYear() + 543)), false, text);
    }
    assert.equal(new Intl.DateTimeFormat(c.uiLocale()).resolvedOptions().calendar, 'gregory');
    assert.equal(new Intl.NumberFormat(c.uiLocale()).resolvedOptions().numberingSystem, 'latn');
    assert.equal(c.clock('2026-01-02T03:04:05Z'), new Date('2026-01-02T03:04:05Z').toLocaleString(locale, { hour: '2-digit', minute: '2-digit' }));
    assert.equal(c.clock(0), new Date(0).toLocaleString(locale, { hour: '2-digit', minute: '2-digit' }));
    assert.equal(c.clock(Date.parse('2026-01-02T03:04:05Z')), c.clock('2026-01-02T03:04:05Z'));
    assert.equal(c.uiFormat(new Date('invalid')), '—');
    assert.equal(c.clock('invalid'), '');
  }
});

test('numbers, fixed decimals, USD and small published prices share the UI formatter', () => {
  for (const language of ['en', 'th']) {
    const c = fixture(language);
    const locale = language === 'th' ? 'th-TH-u-ca-gregory-nu-latn' : 'en-US-u-ca-gregory-nu-latn';
    assert.equal(c.uiFormat(1234567), new Intl.NumberFormat(locale).format(1234567));
    assert.equal(c.uiFixed(1234.5, 2), new Intl.NumberFormat(locale, { minimumFractionDigits: 2, maximumFractionDigits: 2, useGrouping: false }).format(1234.5));
    for (const value of [0, -1.25, 1234.56789]) {
      assert.equal(c.uiMoney(value, 4), new Intl.NumberFormat(locale, { style: 'currency', currency: 'USD', minimumFractionDigits: 4, maximumFractionDigits: 4 }).format(value));
    }
    assert.ok(c.uiMoney(0.000001, null).includes('0.000001'));
    assert.equal(c.uiFormat(Infinity), '—');
    assert.equal(c.uiFormat(null), '—', 'unknown is never displayed as zero');
    assert.equal(c.humanBytes(1), '1 B');
    assert.ok(c.usageText({ steps: 2, prompt_tokens: 1200, completion_tokens: 3, cached_tokens: 0, cost: 0.25 }).includes(c.uiMoney(0.25)));
  }
});

test('numeric DOM labels localize while machine attributes and source text stay unchanged', () => {
  class Node {
    constructor(tag) { this.tagName = tag.toUpperCase(); this.attrs = {}; this.children = []; }
    setAttribute(key, value) { this.attrs[key] = value; }
    append(value) { this.children.push(value); }
  }
  const c = fixture('th');
  c.Node = Node;
  c.document.createElement = tag => new Node(tag);
  c.configureTextInput = () => {};
  vm.runInContext([fn('h'), fn('updateText')].join('\n'), c);
  const option = c.h('option', { value: 12345 }, 12345);
  assert.equal(option.attrs.value, '12345');
  assert.deepEqual(option.children, ['12,345']);
  assert.equal(c.h('td', { text: 12345 }).textContent, '12,345');
  const raw = '{"n":12345.67}';
  assert.equal(c.h('pre', { text: raw }).textContent, raw);
  const text = { data: '' };
  c.updateText(text, 12345);
  assert.equal(text.data, '12,345');
  c.updateText(text, raw);
  assert.equal(text.data, raw);
});

function bareLocaleCalls(text) {
  return [...text.matchAll(/\.toLocale\w+\s*\(/g)]
    .filter(match => !text.slice(match.index + match[0].length).trimStart().startsWith('uiLocale()'))
    .map(match => match[0]);
}
test('locale drift gate rejects bare calls and formatted numeric bypasses', () => {
  assert.deepEqual(bareLocaleCalls(source), []);
  assert.equal([...source.matchAll(/\.toLocaleString\s*\(/g)].length, 1, 'date/number formatting has one central entry');
  assert.ok(fn('uiFormat').includes('.toLocaleString(uiLocale(), options)'));
  assert.equal(/\.to(?:Fixed|Precision|Exponential)\s*\(/.test(source.replace(fn('viewportVariable'), '')), false, 'only machine CSS coordinates use ASCII fixed serialization');
  for (const stale of ['n.toLocaleString()', 'date.toLocaleDateString(undefined)', 'date.toLocaleTimeString([])', 'text.toLocaleLowerCase()']) {
    assert.equal(bareLocaleCalls(stale).length, 1, stale);
  }
  assert.deepEqual(bareLocaleCalls('n.toLocaleString(uiLocale(), {})'), []);
});
