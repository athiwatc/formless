import test from 'node:test';
import assert from 'node:assert/strict';
import {execFile} from 'node:child_process';
import {promisify} from 'node:util';
import {fileURLToPath} from 'node:url';
import {conformanceBase} from './conformance-base.mjs';
import {server} from './test-bounded-wire.mjs';

const run = promisify(execFile);
const script = fileURLToPath(new URL('./conformance.mjs', import.meta.url));

test('HTTP and WebSocket bases share one canonical origin', () => {
  for (const [input, http, websocket] of [
    ['http://localhost:8797', 'http://localhost:8797', 'ws://localhost:8797'],
    ['http://localhost:8797/', 'http://localhost:8797', 'ws://localhost:8797'],
    ['HTTPS://EXAMPLE.COM:443/', 'https://example.com', 'wss://example.com'],
    ['http://[::1]:8797/', 'http://[::1]:8797', 'ws://[::1]:8797'],
  ]) assert.deepEqual(conformanceBase(input), {http, websocket});
});

test('reject unsupported, malformed and ambiguous BASE values before connecting', async () => {
  for (const input of [
    '', 'not a url', 'http://', 'http://localhost:bad', 'http://[::1',
    'ws://127.0.0.1:1', 'wss://127.0.0.1:1', 'ftp://127.0.0.1:1',
    'http://127.0.0.1:1/path', 'http://127.0.0.1:1//',
    'http://127.0.0.1:1/..', 'http://127.0.0.1:1/%2e',
    'http://127.0.0.1:1?', 'http://127.0.0.1:1/#',
    'http://user:password@127.0.0.1:1',
    'http://127.0.0.1:1\\', ' http://127.0.0.1:1', 'http://127.0.0.1:1\n',
  ]) {
    assert.throws(() => conformanceBase(input), /BASE must/);
    await assert.rejects(run(process.execPath, [script, input], {timeout: 3000}), error => {
      assert.equal(error.code, 2, input);
      assert.match(error.stderr, /Usage:.*\nBASE must/);
      assert.equal(error.stdout, '');
      assert.ok(!error.stderr.includes('password'));
      return true;
    });
  }
  await assert.rejects(run(process.execPath, [script, 'http://127.0.0.1:1', 'extra']),
    error => error.code === 2 && /expected at most one BASE/.test(error.stderr));
});

test('actual local relay conformance accepts both trailing slash forms', async () => {
  const owned = await server();
  try {
    const origin = new URL(owned.base).origin.replace(/^ws/, 'http');
    for (const suffix of ['', '/']) {
      const {stdout} = await run(process.execPath, [script, origin + suffix], {timeout: 15000});
      assert.match(stdout, /conformance ok/);
    }
  } finally { await owned.close(); }
});
