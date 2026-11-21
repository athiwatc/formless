# Atomic native model and effort saves

Native Session details now requires `session_model_compare_set_v1`. Save sends
`CompareSetSessionModel` with explicit expected and desired model/effort pairs;
both nullable keys are required. Missing capability fails closed before a
setting read/write, with no fallback to the legacy setter.

Core validates bounds and performs comparison, write and reply read under its
existing SQLite IMMEDIATE transaction. Both current nullable values must match;
a stale companion rejects the entire change. Unrelated Session metadata is
preserved. Inheritance is explicit null; a no-op leaves the timestamp unchanged.
Deleted/missing Sessions fail closed. Existing schema 67, migration behavior,
protocol version 1, manifests and lockfiles remain unchanged.

This API changes metadata only. It does not rewrite `auto`, reserve a Provider
test, append a message or start a Run, even with automatic testing configured.
Desired `auto`, blank Role selectors, whitespace/control/directional-formatting
characters and oversized selectors are rejected. A running Run retains its
sampled selection; future Runs use the saved pair. Manual selectors still do
not establish provider availability. The legacy API remains available to other
clients with its existing behavior.

The comparison checks state, not revision history: an intervening change that
restores the same pair is not detected, and another writer may change it after
commit. A lost response remains Unknown without automatic replay. This slice
does not add durable receipts or preserve Unknown across Client replacement or
restart. Existing form, connection, forecast, draft and IME guards remain.

Focused checks use two SQLite connections racing different reviewed pairs,
rollback/deletion/null/no-op/validation cases, complete wire decoding and a
real Core/native client bridge over in-memory duplex I/O. The bridge changes
the companion through a separate SQLite connection after the native read and
before dispatch: Core rejects the stale write and preserves the external pair.
A daemon fixture enables automatic tests but installs a synchronous owned-file
trap; the new request produces only its Session update and no test/Run/message.
No socket listener, paid provider, personal Profile or native foreground action
is required by these cases. Shell/transport fixtures are compiled only.

The preview and experimental schema-69 prototype branches remain preserved.
Production streaming and Queue stay disabled. Native platform, hosted billing,
performance, licensing/distribution and independent exact-head review gates
remain open; this is not completion or publication approval.
