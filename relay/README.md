# Formless Relay

Hosted relay + web frontend for Formless, deployed as a Cloudflare Worker
with a Durable Object room at <https://formless.athiwat.xyz>.

- The daemon dials out to `wss://formless.athiwat.xyz/relay/<room>` — no
  inbound ports on the user's machine.
- Browsers connect to the same room. The relay only forwards opaque
  AES-256-GCM ciphertext; the pairing secret (32 random bytes) is the
  key and never reaches the relay. The room id is a truncated SHA-256
  of the key.
- Each browser connection handshakes with the daemon (fresh nonces from
  both sides, HKDF-SHA256 session key) and numbers its frames, with the
  sequence number and direction in the AES-GCM associated data, so
  captured frames cannot be replayed or reflected.
- Each browser gets its own logical daemon connection: the daemon
  connects with `?mux=1`, the room tags every client socket with an id,
  tells the daemon `{"relay":"client_open"|"client_close","cid":…}`, and
  routes `{"cid","f"}` frames between them. Daemons without `mux=1` still
  get raw frames broadcast to every client.
- The web frontend in `web/` (`index.html`, `app.css`, `app.js`) is
  served by the same Worker as static assets, with a strict CSP
  (`script-src 'self'`, no inline script or style) and `no-referrer`.

## Layout

| File | Role |
|---|---|
| `worker.js` | Worker entry: `/relay/<room>` → Durable Object room, `/health`, assets + `SECURITY_HEADERS` |
| `web/` | The browser app (no build step; one vendored file, `web/vendor/mermaid.min.js`) |
| `dev.mjs` | Local stand-in for the Worker, for frontend work |

## Use

```sh
formless relay enable    # generates a pairing secret, prints the browser URL
formless relay status    # show the browser URL again
formless relay disable   # --forget also deletes the pairing secret
formless relay enable --rotate   # new secret; old browser links stop working
```

Open the printed `https://formless.athiwat.xyz/#k=...` URL in a browser.
The `#k=` fragment is never sent to any server.

## Local development

`dev.mjs` serves `web/` with the production headers and implements the
same room semantics, so a real daemon can connect to it. With
`--mock-provider` it also serves a scripted OpenAI-compatible endpoint
that streams reasoning, creates and calls a Tool (triggering the
approval dialog), and answers in Markdown. Send `markdown demo` to get a
reply that exercises every rendered block (tables, alerts, highlighted
code, diff, and Mermaid flowchart/sequence/pie).

```sh
node relay/dev.mjs --port 8797 --mock-provider
```

Point a throwaway profile at it (`provider.base_url =
"http://127.0.0.1:8797/mock/v1"`, `relay.enabled = true`, `relay.url =
"ws://127.0.0.1:8797"`), start its daemon with `FORMLESS_API_KEY=mock`
and `FORMLESS_RELAY_SECRET=<32 random bytes, base64url>`, and open
`http://127.0.0.1:8797/#k=<that secret>`.

## Deploy changes

```sh
cd relay
npx wrangler deploy
```

Requires `npx wrangler login` once. The custom domain, Durable Object
binding, and migration are all declared in `wrangler.toml`.

## Vendored Mermaid

`web/vendor/mermaid.min.js` is Mermaid 11.4.1 (MIT, see
`web/vendor/mermaid.LICENSE`), fetched from
`https://cdn.jsdelivr.net/npm/mermaid@11.4.1/dist/mermaid.min.js`
(sha256 `a43bc1afd446f9c4cc66ac5dd45d02e8d65e26fc5344ec0ef787f88d6ddb6f9e`).
It is loaded only when a reply contains a diagram, runs with
`securityLevel: "strict"`, and its SVG output is shown through an `<img>`
data URL, so the page CSP stays `script-src 'self'; style-src 'self'`.
While measuring text, Mermaid sets inline styles on an off-screen copy;
the CSP blocks those and the browser console logs the violations. They
are harmless, because `.mermaid-measure` in `app.css` supplies the font.
To upgrade, replace the file, update the version and hash here, and
check the `markdown demo` reply.
