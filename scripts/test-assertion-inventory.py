#!/usr/bin/env python3
"""CI review aid: list Rust tests without a direct assertion or panic oracle.

Tests can assert through helpers or Result propagation. This is deliberately a
report, not a textual correctness gate; new entries require reviewer attention.
"""
import pathlib
import re


def erase_literals(source):
    # Keep positions/newlines, so braces and assertion words inside fixtures do
    # not count. Rust raw strings permit arbitrary matching # delimiters.
    pattern = re.compile(
        r"b?'(?:\\(?:x[0-9a-fA-F]{2}|u\{[0-9a-fA-F_]+\}|.)|[^'\\\n])'|"
        r'r(?P<hashes>\#*)".*?"(?P=hashes)|"(?:\\.|[^"\\])*"|//[^\n]*|/\*.*?\*/',
        re.S,
    )
    return pattern.sub(lambda m: re.sub(r'[^\n]', ' ', m.group()), source)


print('Rust tests without a direct assert!/assert_eq!/assert_ne!/panic! oracle:')
count = 0
for path in sorted(pathlib.Path('crates').glob('**/*.rs')):
    text = erase_literals(path.read_text(encoding='utf-8'))
    for match in re.finditer(r'#\[(?:tokio::)?test(?:\([^\]]*\))?\](?P<attrs>.*?)\b(?:async\s+)?fn\s+(?P<name>\w+)\s*\(', text, re.S):
        # Do not let a malformed candidate span the next function or test.
        if re.search(r'\bfn\b|#\[(?:tokio::)?test', match['attrs']):
            continue
        start = text.find('{', match.end())
        if start < 0:
            continue
        depth = 1
        end = start + 1
        while end < len(text) and depth:
            depth += (text[end] == '{') - (text[end] == '}')
            end += 1
        if not re.search(r'\b(?:assert(?:_eq|_ne)?|panic)\s*!', text[start:end]):
            line = text.count('\n', 0, match.start()) + 1
            reason = ' [ignored fixture/manual test]' if '#[ignore' in match['attrs'] else ''
            print(f'  {path}:{line}: {match["name"]}{reason}')
            count += 1
print(f'{count} test(s) require helper/Result/manual oracle review.')
