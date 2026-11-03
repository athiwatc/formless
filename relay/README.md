# Formless Relay

Hosted relay + web frontend for Formless, deployed as a Cloudflare Worker
with a Durable Object room at <https://formless.athiwat.xyz>.

- The daemon dials out to `wss://formless.athiwat.xyz/relay/<room>` — no
  inbound ports on the user's machine.
- Browsers connect to the same room. The relay only forwards opaque
  AES-256-GCM ciphertext; the pairing secret (32 random bytes) is the
  E2E key and never reaches the relay. Room v3 is a truncated SHA-256
  of a distinct HKDF daemon claim. The daemon sends that derived capability
  only in its Authorization Upgrade header; the room verifies its commitment
  before admission. A room id alone cannot claim the daemon slot. A valid
  restart replaces the old slot (4003), while retired sockets lose authority.
  The relay can see this slot capability but cannot derive the pairing/E2E key.
- Each browser connection handshakes with the daemon (fresh nonces from
  both sides, HKDF-SHA256 session key) and numbers its frames, with the
  sequence number and direction in the AES-GCM associated data, so
  captured frames cannot be replayed or reflected.
- Each browser gets its own logical daemon connection. Matching clients use
  `?transport=1`; the daemon also uses `mux=1`. Native frames carry bounded
  opaque fragments, with server-issued send grants and distinct destination
  receipts. The Worker never decrypts or reassembles a logical message. Legacy
  raw/broadcast transports are refused rather than used as an unbounded fallback.
- The web frontend in `web/` (`index.html`, `app.css`, `app.js`) is
  served by the same Worker as static assets, with a strict CSP
  (`script-src 'self'`, no inline script or style) and `no-referrer`.

## Room v3 compatibility

Worker, browser and daemon must be updated together. This repository change
performs no hosted deployment. Pairing fragments and secrets stay unchanged;
the new derivation changes the room. Reload the updated browser. Existing push
opt-ins re-register in that room after its authenticated handshake; subscriptions
are not copied from old rooms, and no new permission is requested automatically.
Old unauthenticated daemon upgrades are rejected rather than accepted as a
compatibility fallback. End-to-end frame protocol v2 remains unchanged.

The claim is HKDF-SHA256(pairing key, empty salt,
`formless-relay/3 daemon-claim`), 32 bytes. The URL room is its SHA-256 first
16 bytes as lowercase hex; `Authorization: Bearer <64 lowercase hex digits>`
carries the claim. Production transports remain TLS; loopback dev is the local
fixture exception. Claims are not stored in socket attachments or logged.
Bounded transport v1 is a separate framing layer; E2E frame v2 and daemon
claim v3 remain unchanged. Worker, browser and daemon transport changes must be
integrated together before use. Source changes perform no deployment.

## Bounded room transport (#638)

The logical IPC ceiling stays **32 MiB**. Encrypted envelopes travel in 64 KiB
ASCII fragments; individual native text messages are at most 64 KiB + 1024
bytes. Before authenticated readiness only canonical v2 handshake/control
messages of at most 2048 bytes are allowed, with six handshake messages per
CID/direction within the absolute 30-second handshake deadline. Neither a
public room id nor a sender-visible grant proves a destination drained data.
Ready controls must be consumed and ACKed by both endpoints before that
endpoint can receive bulk fragments.

A room retains at most 16 CIDs, including closing/orphaned ones. One fragment
progress reserve per CID/direction plus shared excess prevents one stalled CID
from borrowing another's reserve. Eight grants/receipts per direction bound
native queues; a completed logical message's final receipt remains outstanding
through the endpoint consumer. ACK deadlines are ten seconds; reassembly has
an absolute 30-second deadline. Either expiry first closes only the offending
CID and queues its cancellation control. The daemon has a separate ten-second
control-drain grace; only failure to consume that shared control may close its
native link. A responsive daemon's missing old data receipt remains charged in
a bounded orphan slot until that exact receipt or actual destination teardown;
timeout alone never refunds it. CID-local faults and responses racing browser
close do not tear down the daemon's other channels. Per-native input is limited
to 8192 application packets and 128 MiB per one-second window, with the separate
small pre-ready handshake allowance described above.

One bounded SQLite row stores only lifecycle/receipt metadata, never payloads
or handshake proofs. A browser close does not release bytes already sent to the
daemon. Only its actual destination receipt or actual daemon teardown releases
those charges. Closing sockets remain counted until actual teardown, even if logical
cancellation was ACKed and all data receipts settled. Error alone retires the
logical device/channel promptly without claiming its native queue disappeared;
controls and orphaned deliveries survive hibernation in that same ledger.
Unidentifiable live socket attachments block new authority and keep all unmatched
ledger charges until actual inventory is unambiguous; an unknown owner is never
interpreted as proof of teardown. Pending
client-open/close notices are bounded metadata, retried when control credit
returns. A replacement daemon retires the old immutable socket identity (4003)
and requires affected browsers to open fresh channels (4002). Actual daemon
teardown does the same; an existing bound CID/generation is never recycled for
a different daemon. Initial offline browsers never bound to a daemon can bind
their first one within the original handshake deadline. A retired daemon
may settle its old receipts but cannot send new data, ready, push or disconnect
commands. Two daemon sockets including a closing predecessor and 18 native
sockets total are the admission bounds.

Data reservations have a fourfold known-copy charge; control reservations,
metadata and transient processing have separate bounds within the proposed
16 MiB logical retained-data room policy. This is **not an OS/RSS or garbage
collector cap**. Platform-owned protocol queues are not a measured RSS claim.
Browser/Rust plaintext, crypto, producer and handler budgets need their own
integration proof. This Worker/dev checkpoint alone does not complete #638.
See [room-budget-design.md](room-budget-design.md) and
[transport-design.md](transport-design.md) for exact accounting boundaries.


## Layout

| File | Role |
|---|---|
| `worker.js` | Worker entry: `/relay/<room>` → Durable Object room, `/health`, assets + `SECURITY_HEADERS` |
| `web/` | The browser app (no build step; one vendored file, `web/vendor/mermaid.min.js`) |
| `dev.mjs`, `dev-storage.mjs` | Local native-socket adapter and real Node SQLite storage |
| `room-budget.mjs` | Bounded durable metadata-only lifecycle and credit authority |
| `room-budget.test.mjs`, `room-bounds.test.mjs`, `room-bounds-wire.test.mjs` | SQLite/Worker state transitions and actual private dev-socket resource proofs |
| `push.mjs` | Payload-free Web Push signed with VAPID (shared by the Worker and `dev.mjs`) |
| `vapid-keys.mjs` | Makes the VAPID key pair for push |
| `conformance.mjs` | `node relay/conformance.mjs [BASE]` (one HTTP and room-frame matrix; starts `dev.mjs` without BASE, or checks e.g. `wrangler dev`) |
| `daemon-claim.test.mjs`, `daemon-claim-wire.test.mjs` | `node --test relay/daemon-claim*.test.mjs` (fixed vectors, unauthenticated/wrong-proof denial, authenticated preemption and actual local Upgrade wire) |
| `room.test.mjs` | `node relay/room.test.mjs` (room teardown, disconnect codes, CSP) |
| `push.test.mjs`, `push.dev-test.mjs` | `node relay/push.test.mjs` (signing), `node relay/push.dev-test.mjs` (end to end through `dev.mjs`) |

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

Run the relay/web regression suite with `node --test relay/*.test.mjs` from
the repository root. Display regressions also have a browser check:
`python relay/web-display.browser-test.py /tmp/formless-display-check`.
It requires Python Playwright, `/usr/bin/chromium`, and Poppler's `pdftotext`
and `pdftoppm`. It serves the actual web assets locally with a test-only
transcript fixture, checks browser default font preferences, manual reading
size, theme changes, menus and simulated safe areas, and saves screenshots
and a multipage PDF. Simulated insets do not replace physical phone testing.

Keyboard and overlay regressions run with
`python relay/web-focus.browser-test.py /tmp/formless-focus-check` (Python
Playwright and `/usr/bin/chromium`). This check uses the shipped frontend with
controlled replies, exercises modal focus containment and background inertness,
return focus, nested dialogs and menu keyboard navigation, and saves screenshots.

Streaming and reading-position checks run with
`python relay/web-streaming.browser-test.py /tmp/formless-streaming-check`.
The complete check requires both Chromium and a supported Playwright WebKit
installation with its native dependencies. `--browser chromium` is explicitly
partial validation. It checks animation-frame batching, retained DOM/text nodes,
pixel-stable anchors without native scroll anchoring, delayed layout and
Session/view transitions. It also compares streamed and stored Markdown block
structure over five burst sizes, including loose/nested lists, display math with
blank lines, and math/fence delimiter precedence; list/item identity stays stable
when later siblings arrive. Pure block regressions are in
`node --test relay/web-markdown.test.mjs` (also included in all relay Node tests).
WebKit engine evidence is not physical Safari testing.

Mobile keyboard/viewport and input regressions run with
`python relay/web-mobile.browser-test.py /tmp/formless-mobile-check` using
Chromium and the same supported Playwright WebKit runtime. The check separates
native browser resizing and touch input from simulated visual-viewport/keyboard
geometry. Simulations are not physical iOS/Android keyboard or device evidence.

Live reduced-motion settings and OS preference changes run in Chromium and
WebKit with `python relay/web-motion.browser-test.py /tmp/formless-motion-check`.
It checks repeated settings events, animation/transition restoration, static
spinner labels and failed-refresh recovery with local fixture replies.

Locale formatting and live language metadata run in both engines with
`python relay/web-locale.browser-test.py /tmp/formless-locale-check` (English/
Thai browser contexts, UTC/Bangkok time zones). `node --test relay/*.test.mjs`
also rejects bare locale calls and numeric-formatting bypasses.

`dev.mjs` runs the Worker's own code: each room is `worker.js`'s
`RelayRoom` over a real SQLite ledger through Node's built-in `node:sqlite`
(Node 22.13+), and every
HTTP request goes through the Worker's fetch handler (so `web/`, `/health`
and `/push/key` answer exactly as in production, headers included). Only
the native WebSocket framing and mock provider are its own. Native input is
bounded before body allocation, pre-ready messages stay small, invalid UTF-8
is refused and at most 128 native continuation frames form one message. Ping/
Pong/Close output has a separate 4 KiB pending native-control cap; a native
close request does not imply queued writes are gone. Local native close has a
one-second grace before the owned socket is destroyed; only its actual close
event releases ledger ownership. Capacity is checked before a 101 upgrade so
rejected peers cannot accumulate untracked WebSockets. A real daemon can connect to it. With
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

### Notifications (Web Push)

Optional. Without keys, `/push/key` answers 404 and the web app says the
relay hasn't set up notifications. To turn it on:

```sh
node relay/vapid-keys.mjs          # prints the two values
npx wrangler secret put VAPID_PUBLIC_KEY
npx wrangler secret put VAPID_PRIVATE_JWK
npx wrangler secret put VAPID_SUBJECT   # optional, e.g. mailto:you@example.com
```

Browsers opt in from Settings → This browser. The room keeps their
subscriptions (at most 10). Pushes carry no payload at all, so the relay
and the push service learn nothing about any Session; the notification
just says something is waiting. `dev.mjs` reads the same variables from
its environment.

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

The generated Settings inventory has a Rust-to-browser fixture and a TUI parity
gate. Export the current Rust payload, then exercise typed edits, read-only
rows, filters, keyboard choice input and 200% narrow layout in both engines:

```sh
FORMLESS_SETTINGS_FIXTURE=/tmp/formless-settings.json cargo test -p formless-core --test settings_inventory --locked
python relay/web-settings.browser-test.py /tmp/formless-settings-browser /tmp/formless-settings.json
```

The browser suite uses local fixture replies and performs no real settings,
provider, daemon or Tool operations. WebKit is engine coverage, not physical
Safari or assistive-technology evidence. `node --test relay/*.test.mjs` also
checks dynamic row generation and typed/cancelled/invalid Edit requests.
