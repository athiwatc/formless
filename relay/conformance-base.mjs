// Conformance probes address a relay origin, never a path or authenticated URL.
export function conformanceBase(value) {
  let url;
  try { url = new URL(value); }
  catch { throw new Error('BASE must be a valid HTTP(S) origin'); }
  if (url.protocol !== 'http:' && url.protocol !== 'https:') {
    throw new Error('BASE must use http:// or https:// (not ws:// or wss://)');
  }
  // Check the supplied spelling too: URL parsing normalizes dot paths,
  // backslashes and whitespace that would otherwise hide ambiguous input.
  if (!/^https?:\/\/[^/?#\\\s]+\/?$/i.test(value) || url.username || url.password) {
    throw new Error('BASE must be an origin with no credentials, path, query or fragment');
  }
  return {
    http: url.origin,
    websocket: url.origin.replace(/^http/, 'ws'),
  };
}
