#!/usr/bin/env python3
"""Offline process checks for eval argument validation; never call a provider."""
import os
from pathlib import Path
import subprocess
import tempfile
import unittest

SCRIPT = Path(__file__).with_name("eval-tool-use.sh").resolve()


class EvalArguments(unittest.TestCase):
    def run_eval(self, overrides, expect_valid=False):
        with tempfile.TemporaryDirectory() as root:
            root = Path(root)
            binary = root / "formless"
            calls = root / "calls"
            binary.write_text('''#!/bin/sh
printf '%s\\n' "$1" >> "$EVAL_CALLS"
if [ "$1" = run ]; then
  printf '%s\\n' "$FORMLESS_DATA_DIR" >> "$EVAL_PROFILES"
  [ ! -e "$FORMLESS_DATA_DIR/learned-tool" ] || exit 9
  : > "$FORMLESS_DATA_DIR/learned-tool"
  case "${EVAL_MODE:-success}" in
    mixed)
      count=$(cat "$EVAL_ATTEMPTS" 2>/dev/null || printf 0)
      count=$((count + 1)); printf '%s' "$count" > "$EVAL_ATTEMPTS"
      if [ "$((count % 2))" -eq 1 ]; then echo synthetic-provider-refused >&2; exit 9; fi
      ;;
    fail) echo synthetic-provider-refused >&2; exit 9 ;;
    unfinished) exit 0 ;;
    failed_status) printf '%s\\n' '{"type":"run_finished","status":"paused"}'; exit 0 ;;
    spoof) printf '%s\\n' '{"type":"tool_result","output":{"type":"run_finished","status":"completed"}}'; exit 0 ;;
    no_tools) printf '%s\\n' '{"type":"run_finished","status":"completed"}'; exit 0 ;;
  esac
  printf '%s\\n' '{"type":"tool_call_started","tool_name":"find_tools"}'
  printf '%s\\n' '{"type":"run_finished","status":"completed"}'
fi
''')
            binary.chmod(0o700)
            env = {**os.environ, "FORMLESS": str(binary), "EVAL_CALLS": str(calls),
                   "FORMLESS_API_KEY": "synthetic-offline-key", "EVAL_PROFILES": str(root / "profiles"), "EVAL_ATTEMPTS": str(root / "attempts")}
            env.pop("RUNS", None)
            env.pop("NUDGE", None)
            env.update(overrides)
            result = subprocess.run([str(SCRIPT), "https://fixture.invalid", "fixture-model"],
                                    env=env, text=True, capture_output=True, timeout=10)
            commands = calls.read_text().splitlines() if calls.exists() else []
            if not expect_valid:
                self.assertEqual(result.returncode, 2, result.stderr)
                self.assertEqual(commands, [], "invalid arguments must not invoke the binary")
                self.assertEqual(result.stdout, "")
                self.assertNotIn("division by zero", result.stderr)
                self.assertNotIn("integer expression", result.stderr)
            if expect_valid:
                profiles = (root / "profiles").read_text().splitlines()
                self.assertEqual(len(profiles), commands.count("run"))
                self.assertEqual(len(profiles), len(set(profiles)), "each Run must have a fresh data root")
                self.assertEqual(commands.count("daemon"), commands.count("run"), "each Run daemon must stop")
            return result, commands

    def test_invalid_runs_fail_before_profile_or_daemon_work(self):
        for value in ["", "0", "000", "-1", "abc", "1.5", "1 2", "9999999999999999999999999"]:
            with self.subTest(value=value):
                result, _ = self.run_eval({"RUNS": value})
                self.assertIn("RUNS must be a positive integer", result.stderr)

    def test_invalid_nudge_is_not_written_as_toml(self):
        for value in ["", "True", "typo"]:
            result, _ = self.run_eval({"NUDGE": value})
            self.assertIn("NUDGE must be true or false", result.stderr)

    def test_default_and_leading_zero_counts_complete_with_fake_runs(self):
        for overrides, runs in [({}, 20), ({"RUNS": "0002", "NUDGE": "false"}, 8)]:
            result, commands = self.run_eval(overrides, expect_valid=True)
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertEqual(commands.count("run"), runs)
            self.assertIn(f"{runs}/{runs} Runs used find_tools/create_tool (100%)", result.stdout)
            self.assertEqual(commands[-1], "daemon")

    def test_all_failures_have_no_misleading_percentage(self):
        for mode in ["fail", "unfinished", "failed_status", "spoof"]:
            result, _ = self.run_eval({"RUNS": "1", "EVAL_MODE": mode}, expect_valid=True)
            self.assertEqual(result.returncode, 1, mode)
            self.assertIn("all 4 Runs failed; no percentage", result.stderr)
            self.assertNotIn("0%", result.stdout)

    def test_mixed_failures_are_excluded_and_first_diagnostic_is_retained(self):
        result, _ = self.run_eval({"RUNS": "1", "EVAL_MODE": "mixed"}, expect_valid=True)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("2/2 Runs used find_tools/create_tool (100%); failed 2/4 attempted", result.stdout)
        self.assertEqual(result.stderr.count("synthetic-provider-refused"), 1)

    def test_success_without_tools_is_a_valid_zero_percent(self):
        result, _ = self.run_eval({"RUNS": "1", "EVAL_MODE": "no_tools"}, expect_valid=True)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("0/4 Runs used find_tools/create_tool (0%); failed 0/4 attempted", result.stdout)


if __name__ == "__main__":
    unittest.main()
