#!/usr/bin/env python3
"""Tests for scripts/check-comment-only.py, the migration proof.

Run from the repository root:

    python3 -m unittest scripts.tests.test_check_comment_only -v

Each test builds a small git repository in a temporary directory and runs the
real command as a subprocess, because the interface that matters is the command
line: the exit code, the revision that was compared, and the one clear line for
a deleted file. A test that imported internals could pass while the command
itself stayed broken.
"""
from __future__ import annotations

import os
import subprocess
import sys
import tempfile
import textwrap
import unittest

SCRIPT = os.path.join(
    os.path.dirname(os.path.dirname(os.path.abspath(__file__))),
    "check-comment-only.py",
)

# The unannotated fixture. Its three functions cover the literal forms the
# comparison must keep apart: a string with a space, a char, and plain code.
BASE = textwrap.dedent(
    """\
    //! Purpose: A fixture module that a migration annotates.

    pub fn greeting() -> &'static str {
        "a b"
    }

    pub fn letter() -> char {
        'x'
    }

    pub fn total() -> u8 {
        1 + 2
    }
    """
)

# The same code after the migration: module labels and a doc block per item.
# Nothing below the comment lines changed, so this must pass.
COMMENTED = textwrap.dedent(
    """\
    //! Purpose: A fixture module that a migration annotates.
    //!
    //! Serves: a migration that only adds comments.
    //! Design: `docs/design/fixture.md`
    //! Upstream: nobody; it is a fixture.
    //! Downstream: nobody.
    //! Verify: `cargo test -p fixture`

    /// Purpose: Return the fixture greeting.
    ///
    /// Serves: the fixture module.
    /// Design: `docs/design/fixture.md`
    /// Up: the module contract.
    /// Down: nothing; it is a leaf.
    /// Verify: `cargo test -p fixture`
    pub fn greeting() -> &'static str {
        "a b"
    }

    /// Purpose: Return the fixture letter.
    pub fn letter() -> char {
        'x'
    }

    /// Purpose: Add two numbers.
    pub fn total() -> u8 {
        1 + 2
    }
    """
)

NEW_FILE = textwrap.dedent(
    """\
    //! Purpose: A new fixture module that only holds its contract.
    //!
    //! Serves: a migration that only adds comments.
    """
)


class CommentOnlyTest(unittest.TestCase):
    def setUp(self) -> None:
        self._tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self._tmp.cleanup)
        self.root = self._tmp.name
        self.git("init", "-q")

    # -- fixture helpers -----------------------------------------------------

    def path(self, relative: str) -> str:
        return os.path.join(self.root, *relative.split("/"))

    def write(self, relative: str, content: str) -> str:
        path = self.path(relative)
        os.makedirs(os.path.dirname(path), exist_ok=True)
        with open(path, "w", encoding="utf-8") as handle:
            handle.write(content)
        return path

    def git(self, *args: str) -> subprocess.CompletedProcess[str]:
        result = subprocess.run(
            ["git", "-C", self.root, *args],
            capture_output=True,
            text=True,
        )
        self.assertEqual(
            result.returncode,
            0,
            f"git {' '.join(args)} failed:\n{result.stdout}{result.stderr}",
        )
        return result

    def commit(self) -> None:
        self.git("add", "-A")
        self.git(
            "-c",
            "user.name=Test",
            "-c",
            "user.email=test@example.com",
            "commit",
            "-q",
            "-m",
            "fixture",
        )

    def run_gate(self, *args: str) -> subprocess.CompletedProcess[str]:
        return subprocess.run(
            [sys.executable, SCRIPT, "--root", self.root, *args],
            capture_output=True,
            text=True,
            cwd=self.root,
        )

    def assert_passes(self, result: subprocess.CompletedProcess[str]) -> None:
        self.assertEqual(
            result.returncode,
            0,
            f"expected exit 0\nstdout:\n{result.stdout}\nstderr:\n{result.stderr}",
        )

    def assert_fails(
        self, result: subprocess.CompletedProcess[str], *needles: str
    ) -> None:
        output = result.stdout + result.stderr
        self.assertEqual(
            result.returncode, 1, f"expected exit 1\noutput:\n{output}"
        )
        self.assertNotIn("Traceback", output)
        for needle in needles:
            self.assertIn(needle, output)

    # -- comment-only changes -------------------------------------------------

    def test_only_added_doc_lines_pass(self) -> None:
        self.write("crates/fixture/src/lib.rs", BASE)
        self.commit()
        self.write("crates/fixture/src/lib.rs", COMMENTED)
        result = self.run_gate()
        self.assert_passes(result)
        self.assertIn("comment-only: OK (1 files)", result.stdout)

    def test_a_changed_char_literal_fails(self) -> None:
        self.write("crates/fixture/src/lib.rs", BASE)
        self.commit()
        self.write(
            "crates/fixture/src/lib.rs", COMMENTED.replace("'x'", "'y'")
        )
        result = self.run_gate()
        self.assert_fails(
            result,
            "crates/fixture/src/lib.rs",
            "differs from HEAD",
            "worktree line",
            "base line",
        )

    def test_literal_whitespace_change_is_not_a_comment_change(self) -> None:
        # Stripping comments and then deleting every space would turn "a b"
        # into "ab" and pass; the literal's contents must stay in the stream.
        self.write("crates/fixture/src/lib.rs", BASE)
        self.commit()
        self.write(
            "crates/fixture/src/lib.rs", COMMENTED.replace('"a b"', '"ab"')
        )
        result = self.run_gate()
        self.assert_fails(result, "crates/fixture/src/lib.rs")

    def test_new_comment_only_file_passes(self) -> None:
        self.write("crates/fixture/src/lib.rs", BASE)
        self.commit()
        self.write("crates/fixture/src/lib.rs", COMMENTED)
        self.write("crates/fixture/src/extra.rs", NEW_FILE)
        result = self.run_gate()
        self.assert_passes(result)
        self.assertIn("comment-only: OK (2 files)", result.stdout)

    def test_deleted_file_is_a_failure_not_a_crash(self) -> None:
        self.write("crates/fixture/src/lib.rs", BASE)
        self.commit()
        os.remove(self.path("crates/fixture/src/lib.rs"))
        result = self.run_gate()
        self.assert_fails(
            result, "crates/fixture/src/lib.rs", "deleted from the working tree"
        )

    @unittest.skipIf(
        os.geteuid() == 0, "root reads a mode-000 file; the failure cannot fire"
    )
    def test_unreadable_file_is_one_clear_line(self) -> None:
        self.write("crates/fixture/src/lib.rs", BASE)
        self.commit()
        self.write("crates/fixture/src/lib.rs", COMMENTED)
        os.chmod(self.path("crates/fixture/src/lib.rs"), 0o000)
        self.addCleanup(
            os.chmod, self.path("crates/fixture/src/lib.rs"), 0o644
        )
        result = self.run_gate()
        self.assert_fails(result, "crates/fixture/src/lib.rs", "cannot read")

    def test_untracked_files_outside_crates_and_ignored_ones_are_skipped(
        self,
    ) -> None:
        self.write(".gitignore", "scratch.rs\n")
        self.write("crates/fixture/src/lib.rs", BASE)
        self.commit()
        self.write("crates/fixture/src/scratch.rs", "pub fn ignored() {}\n")
        self.write("scratch.rs", "pub fn root_scratch() {}\n")
        result = self.run_gate()
        self.assert_passes(result)
        self.assertIn("comment-only: OK (0 files)", result.stdout)

    def test_comment_markers_inside_literals_are_not_comments(self) -> None:
        source = textwrap.dedent(
            """\
            pub fn raw_marker() -> &'static str {
                r#"/* not a comment */"#
            }

            pub fn normal_marker() -> &'static str {
                "// not a comment"
            }
            """
        )
        self.write("crates/fixture/src/lib.rs", source)
        self.commit()
        self.write(
            "crates/fixture/src/lib.rs",
            "/// Purpose: Keep the markers.\n" + source,
        )
        self.assert_passes(self.run_gate())

    # -- the base revision ----------------------------------------------------

    def test_base_names_the_revision_actually_compared(self) -> None:
        self.write("crates/fixture/src/lib.rs", BASE)
        self.commit()
        self.write(
            "crates/fixture/src/lib.rs", BASE.replace("1 + 2", "1 + 3")
        )
        self.commit()
        # Annotate the second commit. Against HEAD only comments changed; the
        # older revision has a different body, so --base HEAD~1 must fail.
        self.write(
            "crates/fixture/src/lib.rs",
            COMMENTED.replace("1 + 2", "1 + 3"),
        )
        self.assert_passes(self.run_gate())
        result = self.run_gate("--base", "HEAD~1")
        self.assert_fails(
            result, "crates/fixture/src/lib.rs", "differs from HEAD~1"
        )

    def test_unknown_base_revision_is_one_clear_line(self) -> None:
        self.write("crates/fixture/src/lib.rs", BASE)
        self.commit()
        result = self.run_gate("--base", "no-such-revision")
        self.assert_fails(result, "base revision does not exist", "no-such-revision")
        self.assertEqual(len(result.stderr.strip().splitlines()), 1, result.stderr)


if __name__ == "__main__":
    unittest.main()
