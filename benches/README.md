# Offline benchmarks

The Criterion suites exercise current in-process hot paths with local fixtures:

- `formless-core/benches/hot_paths.rs`: listing and FTS-searching 1,000 sessions, building a transcript from stored messages, checkpoint write/read, ranking a 200-tool registry with the default BM25 `find_tools` search, and parsing provider SSE data lines.
- `formless-tui/benches/markdown.rs`: rendering a long synthetic answer.

The workloads need no provider credentials or external services. Database and registry fixtures are prepared outside timed iterations. The checkpoint benchmark creates a fresh in-memory session with one existing checkpoint for each timed iteration, then measures one save/read pair so the fixture size stays bounded. Timings depend on the machine, Rust toolchain, and build environment, so compare runs on the same idle machine and toolchain.

The full workspace suite runs offline with:

```sh
cargo bench --offline --workspace --locked
```

All locked Cargo dependencies must already be cached for offline runs; run `cargo fetch --locked` first when network access is available.

Each Criterion target keeps its own baseline. Save and compare the core and TUI results with:

```sh
cargo bench --offline --locked -p formless-core --bench hot_paths -- --save-baseline main
cargo bench --offline --locked -p formless-tui --bench markdown -- --save-baseline main
cargo bench --offline --locked -p formless-core --bench hot_paths -- --baseline main
cargo bench --offline --locked -p formless-tui --bench markdown -- --baseline main
```

Criterion stores measurements under `target/criterion`. To compile all benchmark targets without running them:

```sh
cargo bench --workspace --no-run --locked
```
