# Reader failure schedules

These Unix-only tests put owned IPC proxies between the real validation Client
and a private daemon. Temporary Profiles, fake credentials and loopback fake
providers are used throughout. No GUI, renderer, real Profile or paid API runs.
Production streaming is still disabled; these are bounded correctness schedules,
not native or production acceptance.

The server-pressure proxy stops reading upstream after the reader has admitted
a held prefix. The provider then emits 8,192 deltas with a total canonical output
of 131,091 bytes, exceeding the daemon's 4,096-message subscriber queue while
remaining within desktop payload limits. An independent canonical-only Observer
waits for Run completion. The proxy then drains old socket data slowly enough
to avoid making the UI consumer the saturated queue. Recovery must use a new
physical connection and the exact full canonical answer, with one provider call.
The proxy must independently observe an upstream response EOF/reset or request
write closure; a front/client closure does not satisfy the assertion. The first
test implementation recorded only response EOF and failed after recovery when
that classification was insufficient. Both upstream directions are now recorded;
the failed log remains separate from the passing rerun. No product fix was made.

The delayed-question proxy withholds one GetSession status response containing
an open ask_user question. Other events and responses continue through the same
connection. The Client answers the question and receives a positive acknowledgement;
the Run completes before the held response is released. That old reply must not
reopen the question or publish stale running metadata across the retired basis.
The fresh canonical baseline must contain the final result after exactly two
provider requests. This schedule exercises basis/revision protection together;
it does not isolate every same-basis question-revision interleaving.

The edit proxy captures a status response containing the previous final answer.
An idle Session's past user message is then edited through the real RPC, which
truncates the old answer and starts the replacement Run. Only after the new
provider prefix is observed is the held old status released. The reader must
install the edited canonical baseline, never reinstate truncated history, and
persist exactly one replacement answer after exactly two provider requests.
Edits during an active Run are deliberately rejected by the daemon; this fixture
uses the supported idle-edit admission path.

Read slots remain bounded: an outstanding status response can postpone a fresh
barrier until it returns or times out. These tests release held replies within
the timeout and verify safe recovery; they do not establish an accepted maximum
recovery latency. Server saturation and arbitrary EOF are fail-closed recovery
schedules, not lossless live-delivery guarantees at all rates.

Source identities, focused/full results, first-failure classification, metrics
and disk guards are frozen in the sibling task evidence manifest. The earlier
16-test source and its previously blocked staging evidence remain immutable.
No binary/package rebuild is needed for these daemon-test/documentation changes:
desktop/core/protocol production source and manifests are unchanged from the
last validated integrated base `a5f2b96`.

Independent review of the exact new tests and combined reader/protocol remains
required before publication. Native IME/VoiceOver/clipboard/scaling/picker and
filesystem behavior, Windows/Linux GUI, Step prompt inventory/controls, remaining
required parity, accepted throughput/memory/render budgets and distribution
gates remain open. General mid-attempt replay is absent. The original conflicting
checkout is preserved, and the unresolved foreground approval/action is neither
repeated nor bypassed.

The later [Step inventory increment](step-inventory-20261115.md) addresses the
previously characterized missing reconnect inventory. It does not enable native
Step controls or production streaming.
