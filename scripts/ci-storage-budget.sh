#!/usr/bin/env bash
# CI-only artifact lifecycle. No software/cache outside owned build paths is removed.
set -euo pipefail

ci_root=$(pwd -P)
test -f "$ci_root/Cargo.toml"
test -d "$ci_root/.github/workflows"
test "$(cd "${GITHUB_WORKSPACE:?}" && pwd -P)" = "$ci_root"

snapshot() {
  echo "=== CI storage: $1 ==="
  echo "dev.debug=${CARGO_PROFILE_DEV_DEBUG:-default} test.debug=${CARGO_PROFILE_TEST_DEBUG:-default} incremental=${CARGO_INCREMENTAL:-default}"
  df -Pk "$ci_root" "${RUNNER_TEMP:?}"
  local -a ci_paths=("$ci_root/target" "${CARGO_TARGET_DIR:-}" "${CARGO_HOME:-$HOME/.cargo}" "${ci_tool_target:-}")
  local -a ci_labels=("checkout target" "CARGO_TARGET_DIR" "Cargo home" "packaging tool target")
  local -a ci_seen=()
  local ci_index ci_path ci_physical ci_prior ci_duplicate
  for ci_index in "${!ci_paths[@]}"; do
    ci_path=${ci_paths[$ci_index]}
    [[ -n "$ci_path" && -d "$ci_path" ]] || continue
    ci_physical=$(cd "$ci_path" && pwd -P)
    ci_duplicate=false
    for ci_prior in "${ci_seen[@]-}"; do
      if [[ "$ci_prior" = "$ci_physical" ]]; then ci_duplicate=true; break; fi
    done
    if $ci_duplicate; then continue; fi
    ci_seen+=("$ci_physical")
    printf 'storage %s: ' "${ci_labels[$ci_index]}"
    du -sk "$ci_physical"
  done
}

case "${1:?expected snapshot, clean-debug or install-package-tools}" in
  snapshot)
    snapshot "${2:-phase}"
    ;;
  clean-debug)
    # Called only by a success-dependent step AFTER the complete workspace suite.
    # Reject aliases/outside targets before invoking Cargo's profile-only cleanup.
    test ! -L "$ci_root/target"
    test -d "$ci_root/target"
    test "$(cd "${CARGO_TARGET_DIR:-$ci_root/target}" && pwd -P)" = "$ci_root/target"
    snapshot tests-completed
    cargo clean --offline --profile dev --target-dir "$ci_root/target"
    snapshot debug-artifacts-cleaned
    ;;
  install-package-tools)
    # cargo install's release intermediates must not overlap the application's
    # target tree. Installed executables remain in Cargo home; only this fresh,
    # private intermediate directory is removed, including on installation failure.
    ci_tool_target=$(mktemp -d "${RUNNER_TEMP:?}/formless-package-target.XXXXXX")
    readonly ci_tool_target
    cleanup() {
      ci_status=$?
      trap - EXIT
      if ! rm -rf -- "$ci_tool_target"; then
        if [[ "$ci_status" = 0 ]]; then ci_status=1; fi
      fi
      snapshot packaging-tools-cleaned || ci_status=1
      exit "$ci_status"
    }
    trap cleanup EXIT
    snapshot before-packaging-tools
    time -p env CARGO_TARGET_DIR="$ci_tool_target" cargo install cargo-deb cargo-generate-rpm --locked
    snapshot packaging-tools-installed
    ;;
  *) echo 'Unknown CI storage operation' >&2; exit 2 ;;
esac
