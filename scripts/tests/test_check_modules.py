#!/usr/bin/env python3
"""Tests for scripts/check-modules.py, the code contract gate.

Run from the repository root:

    python3 -m unittest scripts.tests.test_check_modules -v

Each test builds a small tree in a temporary directory and runs the real command
as a subprocess, because the interface that matters is the command-line one:
`file:line`, the missing label, the exit code, and the baseline ratchet. A test
that imported internals could pass while the gate itself stayed broken.
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
    "check-modules.py",
)

MODULE_CONTRACT = textwrap.dedent(
    """\
    //! Purpose: A fixture module for the contract checker.
    //!
    //! Serves: a gate that reads Rust headers.
    //! Design: `docs/design/fixture.md`
    //! Upstream: nobody; it is a fixture.
    //! Downstream: nobody.
    //! Verify: `cargo test -p fixture`
    """
)

FULL_ITEM = textwrap.dedent(
    """\
    /// Purpose: Do the fixture thing.
    ///
    /// Serves: the fixture module.
    /// Design: `docs/design/fixture.md`
    /// Up: the module contract.
    /// Down: nothing; it is a leaf.
    /// Verify: `cargo test -p fixture`
    pub fn thing() {}
    """
)


class GateTest(unittest.TestCase):
    def setUp(self) -> None:
        self._tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self._tmp.cleanup)
        self.root = self._tmp.name
        self.write("docs/design/fixture.md", "# Fixture\n")

    def write(self, relative: str, content: str) -> str:
        path = os.path.join(self.root, *relative.split("/"))
        os.makedirs(os.path.dirname(path), exist_ok=True)
        with open(path, "w", encoding="utf-8") as handle:
            handle.write(content)
        return path

    def path(self, relative: str) -> str:
        return os.path.join(self.root, *relative.split("/"))

    def run_gate(self, *args: str) -> subprocess.CompletedProcess[str]:
        return subprocess.run(
            [sys.executable, SCRIPT, "--root", self.root, *args],
            capture_output=True,
            text=True,
            cwd=self.root,
        )

    def gate(self, *args: str) -> subprocess.CompletedProcess[str]:
        """Run the gate with --paths, which is also how a fixture stays untracked."""
        return self.run_gate("--paths", "crates/fixture.rs", *args)

    def assert_passes(self, result: subprocess.CompletedProcess[str]) -> None:
        self.assertEqual(
            result.returncode,
            0,
            f"expected exit 0\nstdout:\n{result.stdout}\nstderr:\n{result.stderr}",
        )

    def assert_fails(self, result: subprocess.CompletedProcess[str], *needles: str) -> None:
        output = result.stdout + result.stderr
        self.assertEqual(
            result.returncode, 1, f"expected exit 1\noutput:\n{output}"
        )
        for needle in needles:
            self.assertIn(needle, output)

    def init_git(self) -> None:
        subprocess.run(["git", "init", "-q"], cwd=self.root, check=True)
        subprocess.run(["git", "add", "-A"], cwd=self.root, check=True)

    # -- the module contract -------------------------------------------------

    def test_fully_contracted_file_passes(self) -> None:
        self.write(
            "crates/fixture.rs",
            MODULE_CONTRACT
            + "\n"
            + FULL_ITEM
            + textwrap.dedent(
                """\

                /// Purpose: A private type is still a contract.
                ///
                /// Serves: the fixture module.
                /// Design: `docs/design/fixture.md`
                /// Up: the module contract.
                /// Down: nothing.
                /// Verify: `cargo test -p fixture`
                struct Hidden;
                """
            ),
        )
        self.assert_passes(self.gate())

    def test_missing_module_label_names_file_and_line(self) -> None:
        broken = MODULE_CONTRACT.replace(
            "//! Serves: a gate that reads Rust headers.\n", ""
        )
        self.write("crates/fixture.rs", broken + "\n" + FULL_ITEM)
        self.assert_fails(self.gate(), "crates/fixture.rs:1", "Serves")

    def test_design_path_that_does_not_exist_fails(self) -> None:
        broken = MODULE_CONTRACT.replace(
            "docs/design/fixture.md", "docs/design/missing.md"
        )
        self.write("crates/fixture.rs", broken + "\n" + FULL_ITEM)
        self.assert_fails(self.gate(), "Design")

    def test_adr_anchor_resolves_design(self) -> None:
        self.write("docs/decisions/0004-fixture.md", "# ADR-0004\n")
        broken = MODULE_CONTRACT.replace(
            "`docs/design/fixture.md`", "ADR-0004"
        )
        item = FULL_ITEM.replace("`docs/design/fixture.md`", "ADR-0004")
        self.write("crates/fixture.rs", broken + "\n" + item)
        self.assert_passes(self.gate())

    # -- the item contract ---------------------------------------------------

    def test_undocumented_fn_fails_at_its_line(self) -> None:
        self.write(
            "crates/fixture.rs",
            MODULE_CONTRACT + "pub fn undocumented() {}\n",
        )
        self.assert_fails(self.gate(), "crates/fixture.rs:8", "no doc block")

    def test_pub_fn_with_only_purpose_fails(self) -> None:
        self.write(
            "crates/fixture.rs",
            MODULE_CONTRACT
            + "\n/// Purpose: Too thin for a public item.\npub fn thin() {}\n",
        )
        self.assert_fails(self.gate(), "Serves")

    def test_nontrivial_private_fn_with_only_purpose_fails(self) -> None:
        body = "".join(f"    let value{i} = {i};\n" for i in range(18))
        self.write(
            "crates/fixture.rs",
            MODULE_CONTRACT
            + "\n/// Purpose: Too long to be compact.\n"
            + "fn long_one() -> u8 {\n"
            + body
            + "    0\n}\n",
        )
        self.assert_fails(self.gate(), "non-trivial")

    def test_trivial_private_fn_with_purpose_and_contract_passes(self) -> None:
        self.write(
            "crates/fixture.rs",
            MODULE_CONTRACT
            + "\n/// Purpose: Small enough for the compact form.\n"
            + "///\n/// Contract: `crates/fixture.rs`\n"
            + "fn tiny() -> u8 { 1 }\n",
        )
        self.assert_passes(self.gate())

    def test_struct_with_only_purpose_fails(self) -> None:
        self.write(
            "crates/fixture.rs",
            MODULE_CONTRACT
            + "\n/// Purpose: Types always carry the full block.\nstruct Hidden;\n",
        )
        self.assert_fails(self.gate(), "Serves")

    def test_derive_and_blank_lines_do_not_hide_the_doc_block(self) -> None:
        self.write(
            "crates/fixture.rs",
            MODULE_CONTRACT
            + "\n"
            + textwrap.dedent(
                """\
                /// Purpose: The doc block is above the attribute and a blank line.
                ///
                /// Serves: the fixture module.
                /// Design: `docs/design/fixture.md`
                /// Up: the module contract.
                /// Down: nothing.
                /// Verify: `cargo test -p fixture`
                #[derive(Debug)]

                pub struct Visible;
                """
            ),
        )
        self.assert_passes(self.gate())

    def test_test_function_needs_one_purpose_line(self) -> None:
        self.write(
            "crates/fixture.rs",
            MODULE_CONTRACT
            + "\n/// Purpose: Prove the fixture.\n#[test]\nfn proves_it() {}\n"
            + "\n/// Purpose: Prove it asynchronously.\n#[tokio::test]\nasync fn proves_async() {}\n",
        )
        self.assert_passes(self.gate())

    def test_trait_method_declaration_needs_one_purpose_line(self) -> None:
        self.write(
            "crates/fixture.rs",
            MODULE_CONTRACT
            + "\n"
            + textwrap.dedent(
                """\
                /// Purpose: A trait is a contract and carries the full block.
                ///
                /// Serves: the fixture module.
                /// Design: `docs/design/fixture.md`
                /// Up: the module contract.
                /// Down: its implementors.
                /// Verify: `cargo test -p fixture`
                pub trait Speaks {
                    /// Purpose: Say the thing.
                    fn speak(&self);
                }
                """
            ),
        )
        self.assert_passes(self.gate())

    def test_mod_declaration_needs_no_block(self) -> None:
        self.write(
            "crates/fixture.rs", MODULE_CONTRACT + "\nmod inner;\n"
        )
        self.assert_passes(self.gate())

    # -- the scanner masks non-code -----------------------------------------

    def test_items_inside_strings_and_comments_are_ignored(self) -> None:
        self.write(
            "crates/fixture.rs",
            MODULE_CONTRACT
            + "\n"
            + FULL_ITEM
            + textwrap.dedent(
                '''\

                const RAW: &str = r#"
                fn in_raw_string() {}
                "#;

                const QUOTE: char = '"';

                // fn in_line_comment() {}
                /* fn in_block_comment() {
                   /* fn in_nested_comment() {} */
                } */
                '''
            ),
        )
        self.assert_passes(self.gate())

    # -- exemptions ----------------------------------------------------------

    def test_exempt_file_passes_and_stale_exemption_fails(self) -> None:
        self.write("crates/fixture.rs", "pub fn bare() {}\n")
        self.write(
            "scripts/contract-exemptions.txt",
            "crates/fixture.rs | generated by a tool; edits are overwritten\n",
        )
        self.assert_passes(self.gate())

        self.write("scripts/contract-exemptions.txt", "\n".join([
            "crates/fixture.rs | generated by a tool; edits are overwritten",
            "crates/missing.rs | generated by a tool; edits are overwritten",
            "",
        ]))
        self.assert_fails(self.gate(), "stale exemption", "crates/missing.rs")

    def test_item_exemption_passes_and_stale_item_exemption_fails(self) -> None:
        self.write(
            "crates/fixture.rs",
            MODULE_CONTRACT + "\npub fn generated() {}\n",
        )
        self.write(
            "scripts/contract-exemptions.txt",
            "crates/fixture.rs#generated | emitted by the fixture macro\n",
        )
        self.assert_passes(self.gate())

        # The item is gone now, so the item exemption is stale.
        self.write(
            "crates/fixture.rs",
            MODULE_CONTRACT + "\n" + FULL_ITEM,
        )
        self.write(
            "scripts/contract-exemptions.txt",
            "crates/fixture.rs#generated | emitted by the fixture macro\n",
        )
        self.assert_fails(self.gate(), "stale exemption", "generated")

    def test_inline_item_exemption_passes(self) -> None:
        self.write(
            "crates/fixture.rs",
            MODULE_CONTRACT
            + "\n// Contract-exempt: emitted by the fixture macro\npub fn generated() {}\n",
        )
        self.assert_passes(self.gate())

    def test_short_exemption_reason_fails(self) -> None:
        self.write("crates/fixture.rs", "pub fn bare() {}\n")
        self.write(
            "scripts/contract-exemptions.txt",
            "crates/fixture.rs | short\n",
        )
        self.assert_fails(self.gate(), "at least 10 characters")

    # -- baseline ratchet ----------------------------------------------------

    def test_emit_baseline_then_baseline_skips_and_goes_stale(self) -> None:
        # A nested path, because the gate reads `crates/**/*.rs` and a file
        # directly under crates/ is not part of that pathspec.
        self.write("crates/fixture/src/lib.rs", "pub fn undocumented() {}\n")
        self.init_git()
        baseline = self.path("baseline.txt")

        emitted = self.run_gate("--emit-baseline", baseline)
        self.assert_passes(emitted)
        with open(baseline, encoding="utf-8") as handle:
            text = handle.read()
        self.assertIn("crates/fixture/src/lib.rs", text)
        self.assertTrue(
            text.lstrip().startswith("#"), "the baseline starts with a comment"
        )

        self.assert_passes(self.run_gate("--baseline", baseline))

        # The ratchet only shrinks: a baselined file that now passes is stale.
        self.write("crates/fixture/src/lib.rs", MODULE_CONTRACT + "\n" + FULL_ITEM)
        self.assert_fails(
            self.run_gate("--baseline", baseline),
            "stale baseline",
            "crates/fixture/src/lib.rs",
        )

    def test_missing_baseline_file_is_empty(self) -> None:
        self.write("crates/fixture/src/lib.rs", MODULE_CONTRACT + "\n" + FULL_ITEM)
        self.init_git()
        missing = self.path("no-such-baseline.txt")
        self.assert_passes(self.run_gate("--baseline", missing))

    # -- debugging output ----------------------------------------------------

    def test_list_prints_each_file_and_does_not_change_the_result(self) -> None:
        self.write("crates/fixture.rs", MODULE_CONTRACT + "\n" + FULL_ITEM)
        listed = self.gate("--list")
        self.assert_passes(listed)
        self.assertIn("crates/fixture.rs", listed.stdout)
        self.assertIn("PASS", listed.stdout)

        self.write("crates/fixture.rs", "pub fn bare() {}\n")
        failing = self.gate("--list")
        self.assert_fails(failing, "crates/fixture.rs")
        self.assertIn("FAIL", failing.stdout)


if __name__ == "__main__":
    unittest.main()
