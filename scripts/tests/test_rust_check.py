from __future__ import annotations

import sys
import tempfile
import unittest
from pathlib import Path

SCRIPTS = Path(__file__).resolve().parents[1]
if str(SCRIPTS) not in sys.path:
    sys.path.insert(0, str(SCRIPTS))

from rust_check_lib import Checker  # noqa: E402


class RustCheckTests(unittest.TestCase):
    def make_workspace(
        self,
        lib_rs: str,
        extra: dict[str, str] | None = None,
        *,
        manifest: str | None = None,
    ) -> Path:
        temp = tempfile.TemporaryDirectory()
        self.addCleanup(temp.cleanup)
        root = Path(temp.name)
        (root / "src").mkdir()
        (root / "Cargo.toml").write_text(
            manifest
            or '[package]\nname = "sample"\nversion = "0.1.0"\nedition = "2021"\n',
            encoding="utf-8",
        )
        (root / "src/lib.rs").write_text(lib_rs, encoding="utf-8")
        for rel, content in (extra or {}).items():
            path = root / rel
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text(content, encoding="utf-8")
        return root

    def run_checker(
        self, root: Path, *, min_confidence: str = "speculative"
    ) -> Checker:
        checker = Checker(
            root, run_tools="never", min_confidence=min_confidence, max_width=200
        )
        checker.run()
        return checker

    def codes(self, checker: Checker) -> list[str]:
        return [diagnostic.code for diagnostic in checker.diagnostics]

    def diagnostics(self, checker: Checker, code: str):
        return [
            diagnostic for diagnostic in checker.diagnostics if diagnostic.code == code
        ]

    def test_cfg_guarded_duplicate_functions_are_not_reported(self) -> None:
        root = self.make_workspace(
            "#[cfg(windows)]\nfn platform() {}\n"
            "#[cfg(not(windows))]\nfn platform() {}\n"
        )
        self.assertNotIn("RES001", self.codes(self.run_checker(root)))

    def test_cfg_branches_that_can_overlap_are_reported(self) -> None:
        root = self.make_workspace(
            '#[cfg(feature = "a")]\nfn platform() {}\n'
            '#[cfg(feature = "b")]\nfn platform() {}\n'
        )
        diagnostics = self.diagnostics(self.run_checker(root), "RES001")
        self.assertEqual(1, len(diagnostics))
        self.assertEqual("definite", diagnostics[0].confidence)

    def test_unguarded_duplicate_function_is_reported(self) -> None:
        root = self.make_workspace("fn duplicate() {}\nfn duplicate() {}\n")
        self.assertIn("RES001", self.codes(self.run_checker(root)))

    def test_missing_module_file_is_reported(self) -> None:
        root = self.make_workspace("mod absent;\n")
        self.assertIn("MOD005", self.codes(self.run_checker(root)))

    def test_literal_include_marks_file_reachable_and_exports_items(self) -> None:
        root = self.make_workspace(
            'include!("generated.rs");\nmod child { use crate::Generated; }\n',
            {"src/generated.rs": "pub struct Generated;\n"},
        )
        codes = self.codes(self.run_checker(root))
        self.assertNotIn("MOD006", codes)
        self.assertNotIn("RES003", codes)

    def test_orphan_source_file_is_reported(self) -> None:
        root = self.make_workspace(
            "pub fn live() {}\n", {"src/stale.rs": "fn stale() {}\n"}
        )
        self.assertIn("MOD006", self.codes(self.run_checker(root)))

    def test_syntax_error_is_reported(self) -> None:
        root = self.make_workspace("pub fn broken( {\n")
        self.assertIn("SYN001", self.codes(self.run_checker(root)))

    def test_crlf_is_not_treated_as_a_format_failure(self) -> None:
        root = self.make_workspace("pub fn ok() {}\n")
        (root / "src/lib.rs").write_bytes(b"pub fn ok() {}\r\n")
        self.assertNotIn("FMT001", self.codes(self.run_checker(root)))

    def test_recursive_public_glob_reexport_resolves(self) -> None:
        root = self.make_workspace(
            "mod types {\n"
            "    mod core { pub struct Thing; }\n"
            "    pub use core::*;\n"
            "}\n"
            "pub use types::*;\n"
            "mod consumer { use crate::Thing; fn use_it(_: Thing) {} }\n"
        )
        codes = self.codes(self.run_checker(root))
        self.assertNotIn("RES003", codes)
        self.assertNotIn("RES006", codes)

    def test_hyphenated_package_can_use_compact_library_name(self) -> None:
        root = self.make_workspace(
            "use md5::Digest;\npub fn use_trait<T: Digest>() {}\n",
            manifest=(
                '[package]\nname = "sample"\nversion = "0.1.0"\nedition = "2021"\n'
                '[dependencies]\nmd-5 = "0.10"\n'
            ),
        )
        self.assertNotIn("RES003", self.codes(self.run_checker(root)))

    def test_bitflags_macro_generated_type_resolves(self) -> None:
        root = self.make_workspace(
            "use bitflags::bitflags;\n"
            "bitflags! { pub struct Flags: u8 { const A = 1; } }\n"
            "mod child { use crate::Flags; fn f(_: Flags) {} }\n",
            manifest=(
                '[package]\nname = "sample"\nversion = "0.1.0"\nedition = "2021"\n'
                '[dependencies]\nbitflags = "2"\n'
            ),
        )
        codes = self.codes(self.run_checker(root))
        self.assertNotIn("RES003", codes)
        self.assertNotIn("RES006", codes)

    def test_local_macro_rules_generated_type_resolves(self) -> None:
        root = self.make_workspace(
            "macro_rules! make_type {\n"
            "    () => { pub struct Made; };\n"
            "}\n"
            "make_type!();\n"
            "mod child { use crate::Made; fn f(_: Made) {} }\n"
        )
        codes = self.codes(self.run_checker(root))
        self.assertNotIn("RES003", codes)
        self.assertNotIn("RES006", codes)

    def test_function_local_use_is_in_lexical_scope(self) -> None:
        root = self.make_workspace(
            "mod values { pub struct Thing; }\n"
            "fn f() { use crate::values::Thing; let _: Option<Thing> = None; }\n"
        )
        self.assertNotIn("RES006", self.codes(self.run_checker(root)))

    def test_actual_missing_import_is_reported(self) -> None:
        root = self.make_workspace(
            "mod values { pub struct Thing; }\nmod consumer { fn f(_: Thing) {} }\n"
        )
        diagnostics = self.diagnostics(self.run_checker(root), "RES006")
        self.assertEqual(1, len(diagnostics))
        self.assertIn("crate::values::Thing", diagnostics[0].evidence[0])

    def test_child_super_glob_counts_parent_import_as_used(self) -> None:
        root = self.make_workspace(
            "mod values { pub struct Thing; }\n"
            "use values::*;\n"
            "mod child { use super::*; fn f(_: Thing) {} }\n"
        )
        self.assertNotIn("LINT001", self.codes(self.run_checker(root)))

    def test_direct_scoped_call_arity_mismatch_is_reported(self) -> None:
        root = self.make_workspace(
            "fn target(a: u8, b: u8) {}\nfn caller() { crate::target(1); }\n"
        )
        diagnostics = self.diagnostics(self.run_checker(root), "TYPE001")
        self.assertEqual(1, len(diagnostics))
        self.assertEqual("definite", diagnostics[0].confidence)

    def test_local_callable_shadow_skips_free_function_arity_check(self) -> None:
        root = self.make_workspace(
            "fn target(a: u8, b: u8) {}\n"
            "fn caller() { let target = |a: u8| a; target(1); }\n"
        )
        self.assertNotIn("TYPE001", self.codes(self.run_checker(root)))

    def test_baseline_ignores_python_tool_caches(self) -> None:
        baseline = self.make_workspace("pub fn live() {}\n")
        candidate = self.make_workspace("pub fn live() {}\n")
        (candidate / ".ruff_cache").mkdir()
        (candidate / ".ruff_cache/cache-entry").write_text(
            "transient", encoding="utf-8"
        )
        checker = Checker(
            candidate,
            baseline_path=baseline,
            run_tools="never",
            max_width=200,
        )
        checker.run()
        self.assertFalse(checker.diff_entries)
        self.assertNotIn("PIPE002", self.codes(checker))

    def test_min_confidence_filters_speculative_diagnostics(self) -> None:
        root = self.make_workspace(
            "use external::prelude::*;\n",
            manifest=(
                '[package]\nname = "sample"\nversion = "0.1.0"\nedition = "2021"\n'
                '[dependencies]\nexternal = "1"\n'
            ),
        )
        self.assertIn("LINT001", self.codes(self.run_checker(root)))
        self.assertNotIn(
            "LINT001",
            self.codes(self.run_checker(root, min_confidence="probable")),
        )


if __name__ == "__main__":
    unittest.main()
