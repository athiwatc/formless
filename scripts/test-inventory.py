#!/usr/bin/env python3
"""Exercise the assertion inventory as a process over a synthetic Rust crate."""
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest

SCRIPT = Path(__file__).with_name("test-assertion-inventory.py").resolve()


class Inventory(unittest.TestCase):
    def report(self, source):
        with tempfile.TemporaryDirectory() as root:
            path = Path(root) / "crates/x/a.rs"
            path.parent.mkdir(parents=True)
            path.write_text(source)
            result = subprocess.run([sys.executable, str(SCRIPT)], cwd=root,
                                    text=True, capture_output=True, check=True, timeout=10)
            return result.stdout

    def test_brace_chars_cannot_hide_or_borrow_assertions(self):
        report = self.report(r'''
#[test]
fn closes_early() { let c = '}'; assert_eq!(c, '}'); }
#[test]
fn opens_and_swallows() { let c = '{'; let _ = c; }
#[test]
fn byte_closes_early() { let c = b'}'; assert_eq!(c, b'}'); }
#[test]
fn byte_opens_and_swallows() { let c = b'{'; let _ = c; }
#[test]
fn no_oracle() { let _ = 1; }
#[test]
fn has_oracle() { assert!(true); }
''')
        for name in ["opens_and_swallows", "byte_opens_and_swallows", "no_oracle"]:
            self.assertIn(": " + name + "\n", report)
        for name in ["closes_early", "byte_closes_early", "has_oracle"]:
            self.assertNotIn(": " + name + "\n", report)
        self.assertIn("3 test(s)", report)

    def test_escapes_lifetimes_strings_and_lines_remain_distinct(self):
        report = self.report(r'''
#[test]
fn escapes() { let _ = ('\x7b', '\u{7d}', '\u{00_7b}', '\'', '\\'); assert!(true); }
#[test]
fn lifetime() { let _: &'static str = "not assert!(true) }"; }
#[test]
fn fixtures() { let _ = r###"{ panic!("fixture") }"###; /* } */ let _ = b'}'; }
''')
        self.assertNotIn(": escapes\n", report)
        self.assertIn("crates/x/a.rs:5: lifetime", report)
        self.assertIn("crates/x/a.rs:7: fixtures", report)
        self.assertIn("2 test(s)", report)


if __name__ == "__main__":
    unittest.main()
