#!/bin/sh
# Tool-creation eval (#237): how often does a model build or look up a Tool
# when a task needs one? Each task runs RUNS times in a fresh profile with
# `formless run --yes --json`, and a Run counts when it calls find_tools or
# create_tool. Compare a model with the nudge on and off:
#
#   FORMLESS_API_KEY=... scripts/eval-tool-use.sh https://api.openai.com/v1 gpt-4.1
#   FORMLESS_API_KEY=... NUDGE=false scripts/eval-tool-use.sh https://api.openai.com/v1 gpt-4.1
#
# Env: RUNS (default 5), NUDGE (provider.tool_use_nudge, default true),
# FORMLESS (the binary, default `formless`).
set -eu
[ $# -eq 2 ] || { echo "usage: $0 BASE_URL MODEL" >&2; exit 2; }
base_url="$1"; model="$2"
runs="${RUNS-5}"; nudge="${NUDGE-true}"; bin="${FORMLESS:-formless}"
runs_input="$runs"
invalid_runs() { echo "RUNS must be a positive integer (got '$runs_input')" >&2; exit 2; }
case "$runs" in ''|*[!0-9]*) invalid_runs ;; esac
# Normalize leading zeroes so shell arithmetic cannot interpret octal values.
while [ "${runs#0}" != "$runs" ]; do runs="${runs#0}"; done
[ -n "$runs" ] && [ "$runs" -gt 0 ] 2>/dev/null || invalid_runs
case "$nudge" in true|false) ;; *) echo "NUDGE must be true or false (got '$nudge')" >&2; exit 2 ;; esac
: "${FORMLESS_API_KEY:?set FORMLESS_API_KEY}"

# Short paths: the daemon's socket path must fit in sun_path.
work="$(mktemp -d /tmp/fl-eval.XXXX)"
active_data=""
cleanup() {
  if [ -n "$active_data" ]; then
    FORMLESS_DATA_DIR="$active_data" FORMLESS_CONFIG_DIR="$work/c" "$bin" daemon stop >/dev/null 2>&1 || true
  fi
  rm -rf "$work"
}
trap cleanup EXIT
mkdir -p "$work/c" "$work/repo/src"
printf 'fn main() {\n    // TODO: parse args\n    // TODO: print help\n}\n' > "$work/repo/src/main.rs"
printf '# TODO: write docs\n' > "$work/repo/README.md"
cat > "$work/c/config.toml" <<EOF
[provider]
base_url = "$base_url"
model = "$model"
tool_use_nudge = $nudge
[naming]
enabled = false
[skills]
auto_create = false
EOF
export FORMLESS_CONFIG_DIR="$work/c"

tasks="Count the TODO comments in $work/repo and tell me the number.
Fetch https://example.com and tell me the page title.
List the files under $work/repo with their sizes in bytes.
How many lines does $work/repo/src/main.rs have?"

total=0; hits=0; failures=0; attempted=0; task_index=0
while IFS= read -r task; do
  task_index=$((task_index + 1))
  n=0; ok=0; completed=0; task_failed=0
  while [ "$n" -lt "$runs" ]; do
    n=$((n + 1))
    active_data="$work/d.$task_index.$n"
    mkdir -p "$active_data"
    export FORMLESS_DATA_DIR="$active_data"
    run_status=0
    out="$("$bin" run --yes --json --answer "no" "$task" 2>"$work/run.err")" || run_status=$?
    if ! "$bin" daemon stop >/dev/null 2>&1; then
      echo "error: could not stop the eval daemon for task $task_index Run $n" >&2
      exit 1
    fi
    active_data=""
    # The shipped --json stream uses compact top-level Event records. Anchor
    # the tag so a nested Tool result cannot impersonate completion.
    if [ "$run_status" -ne 0 ] || ! printf '%s\n' "$out" | grep -q '^{"type":"run_finished",.*"status":"completed"'; then
      if [ "$failures" -eq 0 ]; then
        echo "note: first eval failure (exit $run_status or no completed Run event):" >&2
        if [ -s "$work/run.err" ]; then sed -n '1,20p' "$work/run.err" >&2; fi
      fi
      failures=$((failures + 1)); task_failed=$((task_failed + 1))
      continue
    fi
    completed=$((completed + 1))
    if printf '%s\n' "$out" | grep -q '^{"type":"tool_call_started",.*"tool_name":"\(find_tools\|create_tool\)"'; then
      ok=$((ok + 1))
    fi
  done
  total=$((total + completed)); hits=$((hits + ok)); attempted=$((attempted + runs))
  printf '%d/%d completed (failed %d/%d)  %s\n' "$ok" "$completed" "$task_failed" "$runs" "$task"
done <<EOF
$tasks
EOF
[ "$total" -gt 0 ] || { echo "error: all $attempted Runs failed; no percentage is available" >&2; exit 1; }
printf 'model %s, nudge %s: %d/%d Runs used find_tools/create_tool (%d%%); failed %d/%d attempted\n' \
  "$model" "$nudge" "$hits" "$total" "$((hits * 100 / total))" "$failures" "$attempted"
