import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import test from 'node:test';
import vm from 'node:vm';

const source = readFileSync(new URL('./web/app.js', import.meta.url), 'utf8');
const start = source.indexOf('  function pathNote(');
const end = source.indexOf('  function capTags(', start);
assert.ok(start >= 0 && end > start);
const pathNote = vm.runInNewContext(source.slice(start, end) + '\npathNote');

test('Composer credential warnings match only the auth file in its config folders', () => {
  for (const file of ['/work/.composer/auth.json', '/work/.config/Composer/AUTH.JSON',
    String.raw`C:\work\.config\composer\auth.json`]) {
    assert.deepEqual(Array.from(pathNote('fs:read:' + file, '/work')), [true, 'holds Composer credentials']);
  }
  for (const file of ['/work/auth.json', '/work/.composer/cache/auth.json', '/work/.config/composer/config.json']) {
    assert.deepEqual(Array.from(pathNote('fs:read:' + file, '/work')), [false, 'inside the working folder']);
  }
});
