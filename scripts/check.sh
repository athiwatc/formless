#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
cd -- "$repo_root"
export PYTHONDONTWRITEBYTECODE=1

cargo fmt --all -- --check
cargo clippy --workspace --all-targets --locked
node --test relay/*.test.mjs
node relay/conformance.mjs
python3 scripts/test-doc-links.py
cargo test --workspace --locked --no-fail-fast
