# Contributing to Formless

Thanks for helping improve Formless. Open an issue before starting a change
larger than a typo so its scope can be discussed. Behaviour changes belong in
the shared implementation and in `SPEC.md`, not only in a client.

## Rust version and build expectations

The workspace uses Rust 2021 but does not declare a `rust-version` or promise
an MSRV. CI uses stable; using stable locally matches CI but does not establish
a lower-bound compatibility promise.

There is no project build-time target. Full builds and tests vary with the
platform and Cargo cache. The default `formless-core` feature,
`embedded-llm`, includes optional CPU Candle model dependencies, so the full
workspace path is heavier than a focused crate test. The separate
`crates/formless-desktop` crate is outside the workspace; see
`.github/workflows/desktop.yml` when changing it.

## Local checks

Rust stable, Cargo, a C compiler, and the platform build dependencies used by
CI are required. Linux builds need `pkg-config` and D-Bus development headers.
Node 24 is used for relay tests.

Run the local baseline from the repository root with one command:

```bash
./scripts/check.sh
```

The script runs the shared CI formatting, Clippy, relay tests, wire
conformance, documentation-link, and locked workspace test commands. It is not
all of CI. The CI workflow additionally runs:

- A Linux/macOS workspace build-and-test matrix and a separate Windows daemon
  and console smoke job.
- Pi extension tests, relay test-inventory validation, and Linux Chromium and
  loopback-push browser regressions.
- Offline documentation/tooling suites and Linux AUR, Debian, and RPM
  packaging tests.
- A release workspace build and smoke checks for the binary and generated
  assets.

A separate desktop workflow tests the desktop crate on macOS, Linux, and
Windows when relevant paths change. See
[the CI workflow](.github/workflows/ci.yml) and
[desktop workflow](.github/workflows/desktop.yml) for the full matrix.

For a faster inner loop, test only the package and target you changed:

```bash
cargo test -p formless-protocol --locked
cargo test -p formless-core --no-default-features --test setting_docs --locked
```

The first command does not build `formless-core` or Candle. The second turns
off `formless-core`'s default `embedded-llm` feature, which is useful when the
focused test does not exercise embedded session naming. Do not use it to
validate that feature itself.

## Change recipes

### Add a setting

1. Add the typed field and default in `crates/formless-core/src/config.rs`.
   If it is changeable through the daemon, add it to the appropriate
   `SETTABLE`/map rules and validation there too; secrets and other file-only
   values must stay file-only.
2. Document its meaning and default in [SPEC.md §13](SPEC.md#13-configuration-complete-settings-table),
   and update generated setting help or client controls only through the
   existing shared settings path.
3. Existing `setting_docs::every_config_key_is_documented` catches missing
   SPEC rows for non-file-only config keys, stale rows, and drift in the
   file-only exception list.
   `settings_inventory::documented_settable_values_choices_and_optional_keys_are_complete`
   checks the projection, values, choices, file-only exclusions, and redaction
   for keys already declared in `SETTABLE`. Those tests do not infer that a
   new setting should be settable, and the default test checks only selected
   values. Add a focused assertion in `crates/formless-core/tests/settings.rs`
   for the new setter, default, and validation behavior; the existing
   `set_key_validates_types_keys_and_the_allow_list` test only covers its
   explicit cases.

```bash
cargo test -p formless-core --no-default-features --test setting_docs --locked
cargo test -p formless-core --no-default-features --test settings_inventory --locked
cargo test -p formless-core --no-default-features --test settings --locked
```

### Add a protocol request

1. Add the serialized request and any payload types in
   `crates/formless-protocol/src/lib.rs`; preserve additive compatibility for
   existing clients.
2. Handle it in the `Daemon::handle_inner` request match in
   `crates/formless-core/src/daemon.rs`. Update CLI, TUI, desktop, or web
   clients to send the request as needed, but keep behavior in the daemon or
   shared crate rather than implementing it in just one client.
3. Specify the wire contract and behavior in `SPEC.md`; update
   `GLOSSARY.md` when the change introduces or changes a defined term.
   Existing protocol tests cover existing requests; they do not automatically
   validate a new variant. Add a round-trip/compatibility test for the new
   variant and a daemon test that sends it through the request handler and
   checks its result. The test suites only catch those omissions after the
   new tests exist:

```bash
cargo test -p formless-protocol --locked
cargo test -p formless-core --no-default-features --lib --locked
```

### Add a built-in tool

1. Built-ins are daemon-dispatched tools, not registry-backed user tools.
   Define the name in `RESERVED` and advertise its schema through
   `builtin_definitions`/`builtin_schema` in
   `crates/formless-core/src/tools.rs`.
2. Implement invocation in the daemon's built-in `match tc.name.as_str()`
   dispatch in `crates/formless-core/src/daemon.rs`. Keep authorization and
   side effects in core; a client may present the tool but must not be its
   only implementation.
3. Document the tool's contract in `SPEC.md` and update `GLOSSARY.md` if a
   defined term changes. The existing schema test only checks known examples
   such as `find_tools`; it will not require a new built-in. Add an explicit
   name/schema test and a daemon invocation test (the `v2_builtins` tests
   already show direct calls through the daemon). Then run:

```bash
cargo test -p formless-core --no-default-features --lib --locked
cargo test -p formless-core --no-default-features --test v2_builtins --locked
```

## Before opening a pull request

Use the [pull request template](.github/PULL_REQUEST_TEMPLATE.md) to confirm
formatting, lint, tests, specification/glossary sync, and shared behavior.
If you report a bug, the issue template explains how to attach a redacted
`formless bug-report` file safely.
