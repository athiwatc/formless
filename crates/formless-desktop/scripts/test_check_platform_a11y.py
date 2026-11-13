#!/usr/bin/env python3
"""Failure paths of check-platform-a11y.py, with stub commands in place of
the shell. No real application is started.

    python3 scripts/test_check_platform_a11y.py
"""
import contextlib
import importlib.util
import io
import os
import stat
import tempfile
import textwrap
import time
import unittest

HERE = os.path.dirname(os.path.abspath(__file__))
spec = importlib.util.spec_from_file_location("check", os.path.join(HERE, "check-platform-a11y.py"))
check = importlib.util.module_from_spec(spec)
spec.loader.exec_module(check)

# A stub that writes the two trees it is asked for, then behaves as told.
STUB = """#!/usr/bin/env python3
import os, sys, time
args = sys.argv[1:]
tree = args[args.index("--dump-platform-a11y") + 1]
gpui = args[args.index("--dump-a11y") + 1]
behaviour = os.environ["STUB_BEHAVIOUR"]
pidfile = os.environ.get("STUB_PIDFILE")
if pidfile:
    open(pidfile, "a").write(f"{os.getpid()}\\n")
if behaviour == "no-tree":
    sys.exit(0)
lines = ["(walking)", "AXUnknown", "  AXGroup", '    AXWindow subrole="AXDialog" modal=true',
         "      AXButton", "      AXButton"]
open(tree, "w").write("\\n".join(lines) + "\\n")
open(gpui, "w").write('{"gpui_focus": "a", "nodes": {"a": {"aria": {"role": "Dialog"}}}}'
                      if behaviour != "bad-json" else "{not json")
print("stub out", behaviour)
print("stub err", behaviour, file=sys.stderr)
if behaviour == "hang":
    time.sleep(600)
sys.exit(3 if behaviour == "fail" else 0)
"""


def run_with(behaviour, timeout=20, pidfile=None):
    work = tempfile.mkdtemp(prefix="a11y-check-test-")
    stub = os.path.join(work, "stub")
    with open(stub, "w") as f:
        f.write(STUB)
    os.chmod(stub, os.stat(stub).st_mode | stat.S_IEXEC)
    os.environ["STUB_BEHAVIOUR"] = behaviour
    if pidfile:
        os.environ["STUB_PIDFILE"] = pidfile
    else:
        os.environ.pop("STUB_PIDFILE", None)
    out = os.path.join(work, "out")
    printed = io.StringIO()
    with contextlib.redirect_stdout(printed):
        code = check.run(stub, out, timeout)
    return code, printed.getvalue(), out


class FailurePaths(unittest.TestCase):
    def test_a_hung_run_is_bounded_and_only_that_child_is_stopped(self):
        pidfile = tempfile.mktemp(prefix="a11y-check-pids-")
        started = time.time()
        code, printed, _ = run_with("hang", timeout=1.0, pidfile=pidfile)
        elapsed = time.time() - started
        self.assertEqual(code, 1)
        self.assertIn("FAIL page-1440x900: the run exits by itself", printed)
        # Three runs, each stopped after its one-second bound, not ten minutes.
        self.assertLess(elapsed, 30)
        with open(pidfile) as file:
            pids = [int(pid) for pid in file.read().split()]
        self.assertEqual(len(pids), 3)
        for pid in pids:
            with self.assertRaises(ProcessLookupError, msg=f"stub {pid} still running"):
                os.kill(pid, 0)

    def test_a_nonzero_exit_fails_even_after_writing_a_tree(self):
        code, printed, out = run_with("fail")
        self.assertEqual(code, 1)
        self.assertIn("FAIL page-1440x900: the run exits with status 0: status 3", printed)
        self.assertIn("stub err fail", printed)
        # The run's own output is kept beside its tree.
        for suffix, expected in ((".stdout", "stub out fail"), (".stderr", "stub err fail")):
            with open(os.path.join(out, "page-1440x900" + suffix)) as file:
                self.assertIn(expected, file.read())

    def test_unreadable_gpui_tree_json_is_a_failed_check_not_a_traceback(self):
        code, printed, _ = run_with("bad-json")
        self.assertEqual(code, 1)
        self.assertIn("FAIL sessions-480x360-200pct: GPUI's tree is readable: JSONDecodeError", printed)
        self.assertNotIn("Traceback", printed)

    def test_a_run_that_writes_no_tree_is_a_failed_check(self):
        code, printed, _ = run_with("no-tree")
        self.assertEqual(code, 1)
        self.assertIn("FAIL page-1440x900: the run wrote its tree", printed)
        self.assertNotIn("Traceback", printed)

    def test_a_clean_run_reports_its_checks(self):
        code, printed, _ = run_with("ok", timeout=20)
        # The stub's page tree is a dialog, so the page checks fail, but the
        # run itself is fine and the dialog runs pass.
        self.assertEqual(code, 1)
        self.assertIn("PASS context-1100x900: the dialog is modal", printed)
        self.assertIn("PASS context-1100x900: GPUI's focus is on the dialog", printed)

    def test_a_binary_that_cannot_be_started_is_a_failed_check_not_a_traceback(self):
        work = tempfile.mkdtemp(prefix="a11y-check-test-")
        printed = io.StringIO()
        with contextlib.redirect_stdout(printed):
            code = check.run(os.path.join(work, "missing"), os.path.join(work, "out"), 5)
        self.assertEqual(code, 1)
        self.assertIn("FAIL page-1440x900: the run starts: FileNotFoundError", printed.getvalue())
        self.assertIn("FAIL context-1100x900: the run starts", printed.getvalue())


class Arguments(unittest.TestCase):
    def usage_error(self, *args):
        # Paths that do not exist, in a directory of their own: a usage error
        # must stop before anything is started or written.
        work = tempfile.mkdtemp(prefix="a11y-check-args-")
        paths = {"b": os.path.join(work, "missing"), "o": os.path.join(work, "out")}
        with self.assertRaises(SystemExit) as raised:
            check.main(["check-platform-a11y.py", *(paths.get(arg, arg) for arg in args)])
        self.assertFalse(os.path.exists(paths["o"]), "a usage error wrote output")
        return raised.exception.code

    def test_a_missing_or_unusable_timeout_is_a_usage_error_not_a_traceback(self):
        for args in (("b", "o", "--timeout"), ("b", "o", "--timeout", "x"),
                     ("b", "o", "--timeout", "0"), ("b", "o", "--timeout", "-1"),
                     ("b", "o", "--timeout", "nan"), ("b", "o", "--timeout", "inf")):
            with self.subTest(args=args):
                message = self.usage_error(*args)
                self.assertIsInstance(message, str)
                self.assertIn("--timeout", message)

    def test_a_wrong_number_of_paths_prints_the_usage(self):
        for args in ((), ("b",), ("b", "o", "extra")):
            with self.subTest(args=args):
                self.assertIn("Usage:", self.usage_error(*args))


if __name__ == "__main__":
    unittest.main(verbosity=2)
