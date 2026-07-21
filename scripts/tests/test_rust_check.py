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
        self,
        root: Path,
        *,
        min_confidence: str = "speculative",
        fix: bool = False,
        include_tests: bool = False,
    ) -> Checker:
        checker = Checker(
            root,
            run_tools="never",
            min_confidence=min_confidence,
            max_width=200,
            fix=fix,
            include_tests=include_tests,
        )
        checker.run()
        return checker

    def codes(self, checker: Checker) -> list[str]:
        return [diagnostic.code for diagnostic in checker.diagnostics]

    def diagnostics(self, checker: Checker, code: str):
        return [
            diagnostic for diagnostic in checker.diagnostics if diagnostic.code == code
        ]

    def test_async_std_fs_call_is_reported(self) -> None:
        root = self.make_workspace(
            "async fn load(path: &std::path::Path) { "
            "let _ = std::fs::read(path); }\n"
        )
        diagnostics = self.diagnostics(self.run_checker(root), "AFS001")
        self.assertEqual(1, len(diagnostics))
        self.assertIn("std::fs::read", diagnostics[0].message)

    def test_async_std_fs_aliases_are_reported(self) -> None:
        root = self.make_workspace(
            "use std::fs as sync_fs;\n"
            "use std::fs::{File, write as sync_write};\n"
            "async fn load(path: &std::path::Path) {\n"
            "    let _ = sync_fs::metadata(path);\n"
            "    let _ = File::open(path);\n"
            "    let _ = sync_write(path, b\"x\");\n"
            "}\n"
        )
        diagnostics = self.diagnostics(self.run_checker(root), "AFS001")
        self.assertEqual(3, len(diagnostics))

    def test_async_local_std_fs_alias_is_reported(self) -> None:
        root = self.make_workspace(
            "async fn load(path: &std::path::Path) {\n"
            "    use std::fs::read as sync_read;\n"
            "    let _ = sync_read(path);\n"
            "}\n"
        )
        diagnostics = self.diagnostics(self.run_checker(root), "AFS001")
        self.assertEqual(1, len(diagnostics))

    def test_sync_fs_helper_called_from_async_is_reported(self) -> None:
        root = self.make_workspace(
            "fn read_sync(path: &std::path::Path) -> std::io::Result<Vec<u8>> {\n"
            "    std::fs::read(path)\n"
            "}\n"
            "async fn load(path: &std::path::Path) {\n"
            "    let _ = read_sync(path);\n"
            "}\n"
        )
        diagnostics = self.diagnostics(self.run_checker(root), "AFS003")
        self.assertEqual(1, len(diagnostics))
        self.assertIn("read_sync", diagnostics[0].message)

    def test_test_only_async_fs_is_skipped_unless_requested(self) -> None:
        root = self.make_workspace(
            "#[cfg(test)]\n"
            "mod tests {\n"
            "    async fn fixture(path: &std::path::Path) {\n"
            "        let _ = std::fs::read(path);\n"
            "    }\n"
            "}\n"
        )
        self.assertNotIn("AFS001", self.codes(self.run_checker(root)))
        self.assertIn(
            "AFS001",
            self.codes(self.run_checker(root, include_tests=True)),
        )

    def test_async_path_probe_is_reported(self) -> None:
        root = self.make_workspace(
            "async fn inspect(root: &std::path::Path) {\n"
            "    let payload = root.join(\"payload.bin\");\n"
            "    let _ = payload.is_file();\n"
            "}\n"
        )
        diagnostics = self.diagnostics(self.run_checker(root), "AFS001")
        self.assertEqual(1, len(diagnostics))
        self.assertIn("payload.is_file", diagnostics[0].message)

    def test_sync_std_fs_call_is_allowed(self) -> None:
        root = self.make_workspace(
            "fn load(path: &std::path::Path) { let _ = std::fs::read(path); }\n"
        )
        self.assertNotIn("AFS001", self.codes(self.run_checker(root)))

    def test_std_fs_inside_blocking_boundary_is_allowed(self) -> None:
        root = self.make_workspace(
            "async fn scan(path: std::path::PathBuf) {\n"
            "    let _ = compio::runtime::spawn_blocking(move || {\n"
            "        std::fs::read_dir(path).unwrap().count()\n"
            "    }).await;\n"
            "}\n"
        )
        checker = self.run_checker(root)
        self.assertNotIn("AFS001", self.codes(checker))
        self.assertNotIn("AFS002", self.codes(checker))

    def test_redundant_blocking_wrapper_is_reported(self) -> None:
        root = self.make_workspace(
            "async fn load(path: std::path::PathBuf) {\n"
            "    let _ = compio::runtime::spawn_blocking(move || {\n"
            "        std::fs::read(path)\n"
            "    }).await;\n"
            "}\n"
        )
        diagnostics = self.diagnostics(self.run_checker(root), "AFS002")
        self.assertEqual(1, len(diagnostics))
        self.assertIn("spawn_blocking", diagnostics[0].message)

    def test_blocking_wrapper_with_cpu_work_is_not_called_fs_only(self) -> None:
        root = self.make_workspace(
            "fn decode(_: Vec<u8>) -> usize { 0 }\n"
            "async fn load(path: std::path::PathBuf) {\n"
            "    let _ = compio::runtime::spawn_blocking(move || {\n"
            "        decode(std::fs::read(path).unwrap())\n"
            "    }).await;\n"
            "}\n"
        )
        self.assertNotIn("AFS002", self.codes(self.run_checker(root)))

    def test_async_block_inside_sync_function_is_checked(self) -> None:
        root = self.make_workspace(
            "fn future(path: std::path::PathBuf) {\n"
            "    let _future = async move { std::fs::metadata(path) };\n"
            "}\n"
        )
        self.assertIn("AFS001", self.codes(self.run_checker(root)))

    def test_task_pool_custom_worker_model_is_rejected(self) -> None:
        root = self.make_workspace(
            "mod runtime;\n",
            {
                "src/runtime.rs": "mod task_pool;\n",
                "src/runtime/task_pool.rs": (
                    "use std::sync::Condvar;\n"
                    "fn worker_loop() { let _ = Condvar::new(); }\n"
                ),
            },
        )
        diagnostics = self.diagnostics(self.run_checker(root), "DSP001")
        self.assertGreaterEqual(len(diagnostics), 2)

    def test_task_pool_dispatcher_admission_model_is_allowed(self) -> None:
        root = self.make_workspace(
            "mod runtime;\n",
            {
                "src/runtime.rs": "mod task_pool;\n",
                "src/runtime/task_pool.rs": (
                    "struct Coordinator { cpu_slots: usize }\n"
                    "fn submit() { dispatcher_dispatch(); dispatcher_dispatch_blocking(); }\n"
                    "fn dispatcher_dispatch() {}\n"
                    "fn dispatcher_dispatch_blocking() {}\n"
                ),
            },
        )
        self.assertNotIn("DSP001", self.codes(self.run_checker(root)))

    def test_task_match_must_cover_new_variants_without_catch_all(self) -> None:
        root = self.make_workspace(
            "pub enum Task { A { value: u32 }, B { value: u32 }, C { value: u32 } }\n"
            "fn route(task: &Task) {\n"
            "    match task {\n"
            "        Task::A { .. } => {},\n"
            "        Task::B { .. } => {},\n"
            "    }\n"
            "}\n"
        )
        diagnostics = self.diagnostics(self.run_checker(root), "DAG001")
        self.assertEqual(1, len(diagnostics))
        self.assertIn("C", diagnostics[0].message)

    def test_task_match_catch_all_remains_allowed(self) -> None:
        root = self.make_workspace(
            "pub enum Task { A { value: u32 }, B { value: u32 }, C { value: u32 } }\n"
            "fn route(task: &Task) {\n"
            "    match task {\n"
            "        Task::A { .. } => {},\n"
            "        _ => {},\n"
            "    }\n"
            "}\n"
        )
        self.assertNotIn("DAG001", self.codes(self.run_checker(root)))

    def test_task_constructor_fields_follow_canonical_variant(self) -> None:
        root = self.make_workspace(
            "pub enum Task { Download { url: String, size: u64 } }\n"
            "fn task() -> Task {\n"
            "    Task::Download { url: String::new(), retry: 1 }\n"
            "}\n"
        )
        diagnostics = self.diagnostics(self.run_checker(root), "DAG002")
        self.assertEqual(1, len(diagnostics))
        self.assertIn("missing fields", diagnostics[0].message)
        self.assertIn("unknown fields", diagnostics[0].message)

    def test_archive_commit_requires_token_aware_graph_insertion(self) -> None:
        root = self.make_workspace(
            "pub enum Task { CommitArchive { work: usize }, ExtractArchiveShard { shard: usize } }\n"
            "struct Expansion;\n"
            "impl Expansion { fn add_root(&mut self, _task: Task) {} }\n"
            "fn plan(expansion: &mut Expansion) {\n"
            "    expansion.add_root(Task::CommitArchive { work: 1 });\n"
            "}\n"
        )
        diagnostics = self.diagnostics(self.run_checker(root), "DAG003")
        self.assertEqual(1, len(diagnostics))
        self.assertIn("CommitArchive", diagnostics[0].message)

    def test_archive_range_tasks_allow_token_aware_graph_insertion(self) -> None:
        root = self.make_workspace(
            "pub enum Task { CommitArchive { work: usize }, ExtractArchiveShard { shard: usize } }\n"
            "struct Expansion;\n"
            "impl Expansion {\n"
            "    fn add_root_with_tokens(&mut self, _task: Task, _tokens: [usize; 1]) {}\n"
            "    fn add_task_with_tokens(&mut self, _task: Task, _deps: [usize; 1], _tokens: [usize; 1]) {}\n"
            "}\n"
            "fn plan(expansion: &mut Expansion) {\n"
            "    expansion.add_root_with_tokens(Task::ExtractArchiveShard { shard: 1 }, [1]);\n"
            "    expansion.add_task_with_tokens(Task::CommitArchive { work: 1 }, [1], [1]);\n"
            "}\n"
        )
        self.assertNotIn("DAG003", self.codes(self.run_checker(root)))

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

    def test_custom_path_submodule_resolution(self) -> None:
        root = self.make_workspace(
            '#[path = "main/entrypoint.rs"]\nmod entrypoint;\n',
            {
                "src/main/entrypoint.rs": '#[path = "entrypoint/tests.rs"]\nmod tests;\n',
                "src/main/entrypoint/tests.rs": "pub struct Test;\n",
            },
        )
        codes = self.codes(self.run_checker(root))
        self.assertNotIn("MOD003", codes)
        self.assertNotIn("MOD006", codes)

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

    def test_grouped_import_name_does_not_count_itself_as_usage(self) -> None:
        root = self.make_workspace(
            "mod values { pub fn used() {} pub fn unused() {} }\n"
            "use values::{used, unused};\n"
            "fn call() { used(); }\n"
        )
        diagnostics = self.diagnostics(self.run_checker(root), "LINT002")
        self.assertEqual(
            ["Likely unused import: unused"], [d.message for d in diagnostics]
        )

    def test_fix_removes_unused_grouped_import(self) -> None:
        root = self.make_workspace(
            "mod values { pub fn used() {} pub fn unused() {} }\n"
            "use values::{used, unused};\n"
            "fn call() { used(); }\n"
        )
        checker = self.run_checker(root, fix=True)
        self.assertNotIn("LINT002", self.codes(checker))
        text = (root / "src/lib.rs").read_text("utf-8")
        self.assertIn("use values::{used};", text)
        self.assertNotIn("used, unused", text)

    def test_restricted_reexport_requires_usage_through_reexport_path(self) -> None:
        root = self.make_workspace(
            "mod inner { pub fn value() {} }\n"
            "pub(crate) use inner::value;\n"
            "mod consumer { use crate::inner::value; fn call() { value(); } }\n"
        )
        diagnostics = self.diagnostics(self.run_checker(root), "LINT002")
        self.assertEqual(1, len(diagnostics))
        self.assertIn("restricted re-export", diagnostics[0].message)

    def test_restricted_reexport_usage_through_binding_is_allowed(self) -> None:
        root = self.make_workspace(
            "mod inner { pub fn value() {} }\n"
            "pub(crate) use inner::value;\n"
            "mod consumer { use crate::value; fn call() { value(); } }\n"
        )
        self.assertNotIn("LINT002", self.codes(self.run_checker(root)))

    def test_missing_parent_scope_import_is_reported_for_value_and_type_names(
        self,
    ) -> None:
        root = self.make_workspace(
            "use std::path::Path;\n"
            "fn helper() {}\n"
            "mod child { fn run(_: &Path) { helper(); } }\n"
        )
        diagnostics = self.diagnostics(self.run_checker(root), "RES007")
        self.assertEqual(1, len(diagnostics))
        self.assertIn("Path", diagnostics[0].evidence[0])
        self.assertIn("helper", diagnostics[0].evidence[0])

    def test_fix_adds_parent_scope_import(self) -> None:
        root = self.make_workspace(
            "use std::path::Path;\n"
            "fn helper() {}\n"
            "mod child {\n"
            "    fn run(_: &Path) { helper(); }\n"
            "}\n"
        )
        checker = self.run_checker(root, fix=True)
        self.assertNotIn("RES007", self.codes(checker))
        self.assertIn("use super::*;", (root / "src/lib.rs").read_text("utf-8"))

    def test_qualified_type_path_is_not_reported_as_missing_import(self) -> None:
        root = self.make_workspace(
            "pub struct Thing;\nmod child { fn run(_: crate::Thing) {} }\n"
        )
        codes = self.codes(self.run_checker(root))
        self.assertNotIn("RES006", codes)
        self.assertNotIn("RES007", codes)

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

    def test_public_progress_callback_is_rejected(self) -> None:
        root = self.make_workspace(
            "pub fn run<F: FnMut(u64)>(progress_callback: F) {}\n"
            "fn local(progress_callback: impl FnMut(u64)) {}\n"
        )
        diagnostics = self.diagnostics(self.run_checker(root), "PRG001")
        self.assertEqual(1, len(diagnostics))
        self.assertIn("run", diagnostics[0].message)

    def test_progress_lane_must_use_shared_constants(self) -> None:
        root = self.make_workspace(
            "mod progress;\nmod consumer;\n",
            {
                "src/progress.rs": (
                    "pub enum ProgressScope { Integrity }\n"
                    "pub enum ProgressPhase { Verify }\n"
                    "pub struct ProgressLane;\n"
                    "impl ProgressLane {\n"
                    "    pub const INTEGRITY_VERIFY: Self = Self::new(ProgressScope::Integrity, ProgressPhase::Verify);\n"
                    "    pub const fn new(_: ProgressScope, _: ProgressPhase) -> Self { Self }\n"
                    "}\n"
                ),
                "src/consumer.rs": (
                    "use crate::progress::{ProgressLane, ProgressPhase, ProgressScope};\n"
                    "fn bad() { let _ = ProgressLane::new(ProgressScope::Integrity, ProgressPhase::Verify); }\n"
                    "fn good() { let _ = ProgressLane::INTEGRITY_VERIFY; }\n"
                ),
            },
        )
        diagnostics = self.diagnostics(self.run_checker(root), "PRG002")
        self.assertEqual(1, len(diagnostics))

    def test_transient_worker_events_cannot_escape_in_results(self) -> None:
        root = self.make_workspace(
            "pub enum WorkerEvent { DownloadedBytes { bytes: u64 }, PatchProgress }\n"
            "pub struct TaskPoolResult { pub events: Vec<WorkerEvent> }\n"
        )
        diagnostics = self.diagnostics(self.run_checker(root), "PRG003")
        self.assertEqual(1, len(diagnostics))

    def test_durable_task_outcomes_are_allowed_in_results(self) -> None:
        root = self.make_workspace(
            "pub enum WorkerEvent { DownloadedBytes { bytes: u64 } }\n"
            "pub enum TaskOutcome { Downloaded { bytes: u64 } }\n"
            "pub struct TaskPoolResult { pub outcomes: Vec<TaskOutcome> }\n"
        )
        self.assertNotIn("PRG003", self.codes(self.run_checker(root)))

    def test_common_crate_cannot_depend_on_indicatif(self) -> None:
        root = self.make_workspace(
            "pub struct ProgressUpdate;\n",
            manifest=(
                '[package]\nname = "griffr-common"\nversion = "0.1.0"\nedition = "2021"\n'
                '[dependencies]\nindicatif = "0.17"\n'
            ),
        )
        self.assertIn("PRG004", self.codes(self.run_checker(root)))

    def test_exported_progress_callback_is_reported(self) -> None:
        root = self.make_workspace(
            "pub fn run(progress_callback: Option<impl FnMut(u64)>) {}\n"
        )
        diagnostics = self.diagnostics(self.run_checker(root), "PRG001")
        self.assertEqual(1, len(diagnostics))
        self.assertEqual("definite", diagnostics[0].confidence)

    def test_callback_in_private_module_is_allowed(self) -> None:
        root = self.make_workspace(
            "mod internal;\n",
            {
                "src/internal.rs": (
                    "pub fn run(progress_callback: Option<impl FnMut(u64)>) {}\n"
                )
            },
        )
        self.assertNotIn("PRG001", self.codes(self.run_checker(root)))

    def test_progress_protocol_package_must_be_renderer_neutral(self) -> None:
        root = self.make_workspace(
            "pub enum ProgressUpdate { Tick }\npub struct ProgressSender;\n",
            manifest=(
                '[package]\nname = "sample"\nversion = "0.1.0"\nedition = "2021"\n'
                '[dependencies]\nindicatif = "0.17"\n'
            ),
        )
        diagnostics = self.diagnostics(self.run_checker(root), "PRG004")
        self.assertEqual(1, len(diagnostics))
        self.assertIn("indicatif", diagnostics[0].message)

    def test_raw_progress_channel_in_public_api_is_reported(self) -> None:
        root = self.make_workspace(
            "pub enum ProgressUpdate { Tick }\n"
            "pub struct ProgressSender { tx: Option<flume::Sender<ProgressUpdate>> }\n"
            "pub fn raw_receiver() -> flume::Receiver<ProgressUpdate> {\n"
            "    unimplemented!()\n"
            "}\n",
            manifest=(
                '[package]\nname = "sample"\nversion = "0.1.0"\nedition = "2021"\n'
                '[dependencies]\nflume = "0.11"\n'
            ),
        )
        diagnostics = self.diagnostics(self.run_checker(root), "PRG005")
        self.assertEqual(1, len(diagnostics))
        self.assertIn("Raw progress channel", diagnostics[0].message)

    def test_private_raw_channel_field_inside_sender_wrapper_is_allowed(self) -> None:
        root = self.make_workspace(
            "pub enum ProgressUpdate { Tick }\n"
            "pub struct ProgressSender { tx: Option<flume::Sender<ProgressUpdate>> }\n",
            manifest=(
                '[package]\nname = "sample"\nversion = "0.1.0"\nedition = "2021"\n'
                '[dependencies]\nflume = "0.11"\n'
            ),
        )
        self.assertNotIn("PRG005", self.codes(self.run_checker(root)))

    def test_conflicting_units_for_same_progress_lane_are_reported(self) -> None:
        root = self.make_workspace(
            "enum ProgressUnit { Items, Bytes }\n"
            "struct ProgressLane;\n"
            "struct ProgressRoute { lane: ProgressLane, unit: ProgressUnit }\n"
            "fn routes(lane: ProgressLane) {\n"
            "    let _items = ProgressRoute { lane, unit: ProgressUnit::Items };\n"
            "    let _bytes = ProgressRoute { lane, unit: ProgressUnit::Bytes };\n"
            "}\n"
        )
        diagnostics = self.diagnostics(self.run_checker(root), "PRG006")
        self.assertEqual(1, len(diagnostics))
        self.assertIn("conflicting units", diagnostics[0].message)

    def test_distinct_progress_lanes_can_use_distinct_units(self) -> None:
        root = self.make_workspace(
            "enum ProgressUnit { Items, Bytes }\n"
            "struct ProgressLane;\n"
            "struct ProgressRoute { lane: ProgressLane, unit: ProgressUnit }\n"
            "fn routes(item_lane: ProgressLane, byte_lane: ProgressLane) {\n"
            "    let _items = ProgressRoute { lane: item_lane, unit: ProgressUnit::Items };\n"
            "    let _bytes = ProgressRoute { lane: byte_lane, unit: ProgressUnit::Bytes };\n"
            "}\n"
        )
        self.assertNotIn("PRG006", self.codes(self.run_checker(root)))

    def test_collapsible_match_is_reported(self) -> None:
        root = self.make_workspace(
            "fn main() {\n"
            "    match x {\n"
            "        Some(y) => {\n"
            "            if y > 0 {\n"
            '                println!("{}", y);\n'
            "            }\n"
            "        }\n"
            "    }\n"
            "}\n"
        )
        self.assertIn("CLP004", self.codes(self.run_checker(root)))

    def test_items_after_inline_test_module_is_reported_and_fixed(self) -> None:
        root = self.make_workspace(
            "mod support { pub fn run() {} }\n"
            "#[cfg(test)]\nmod tests { #[test] fn smoke() {} }\n"
            "use support::run;\n"
            "fn call() { run(); }\n"
        )
        self.assertIn("CLP005", self.codes(self.run_checker(root)))
        checker = self.run_checker(root, fix=True)
        self.assertNotIn("CLP005", self.codes(checker))
        text = (root / "src/lib.rs").read_text("utf-8")
        self.assertLess(text.index("use support::run;"), text.index("#[cfg(test)]"))

    def test_out_of_line_test_module_declaration_is_not_reported(self) -> None:
        root = self.make_workspace(
            "#[cfg(test)]\nmod tests;\npub fn live() {}\n",
            {"src/tests.rs": "#[test] fn smoke() {}\n"},
        )
        self.assertNotIn("CLP005", self.codes(self.run_checker(root)))

    def test_useless_chain_into_iter_is_reported_and_fixed(self) -> None:
        root = self.make_workspace(
            "fn collect<I: Iterator<Item = u8>>(iter: I, tail: Option<u8>) {\n"
            "    let _ = iter.chain(tail.into_iter());\n"
            "}\n"
        )
        self.assertIn("CLP006", self.codes(self.run_checker(root)))
        checker = self.run_checker(root, fix=True)
        self.assertNotIn("CLP006", self.codes(checker))
        self.assertIn("iter.chain(tail)", (root / "src/lib.rs").read_text("utf-8"))

    def test_into_iter_before_adapter_is_not_reported(self) -> None:
        root = self.make_workspace(
            "fn collect<I: Iterator<Item = u8>>(iter: I, tail: Option<u8>) {\n"
            "    let _ = iter.chain(tail.into_iter().map(|x| x));\n"
            "}\n"
        )
        self.assertNotIn("CLP006", self.codes(self.run_checker(root)))

    def test_needless_option_as_deref_mut_is_reported_and_fixed(self) -> None:
        root = self.make_workspace(
            "fn sink(_: Option<&mut dyn FnMut(u8)>) {}\n"
            "fn run(mut callback: Option<&mut dyn FnMut(u8)>) {\n"
            "    sink(callback.as_deref_mut());\n"
            "}\n"
        )
        self.assertIn("CLP007", self.codes(self.run_checker(root)))
        checker = self.run_checker(root, fix=True)
        self.assertNotIn("CLP007", self.codes(checker))
        text = (root / "src/lib.rs").read_text("utf-8")
        self.assertIn("fn run(callback:", text)
        self.assertIn("sink(callback);", text)

    def test_reborrowed_option_as_deref_mut_is_not_reported(self) -> None:
        root = self.make_workspace(
            "fn sink(_: Option<&mut dyn FnMut(u8)>) {}\n"
            "fn run(mut callback: Option<&mut dyn FnMut(u8)>) {\n"
            "    sink(callback.as_deref_mut());\n"
            "    sink(callback.as_deref_mut());\n"
            "}\n"
        )
        self.assertNotIn("CLP007", self.codes(self.run_checker(root)))

    def test_manual_checked_division_is_reported_and_fixed(self) -> None:
        root = self.make_workspace(
            "fn bucket(completed: u64, total: u64) -> u64 {\n"
            "    if total == 0 {\n"
            "        0\n"
            "    } else {\n"
            "        ((completed.saturating_mul(100) / total) / 5) * 5\n"
            "    }\n"
            "}\n"
        )
        self.assertIn("CLP008", self.codes(self.run_checker(root)))
        checker = self.run_checker(root, fix=True)
        self.assertNotIn("CLP008", self.codes(checker))
        text = (root / "src/lib.rs").read_text("utf-8")
        self.assertIn(".checked_div(total)", text)
        self.assertIn(".map_or(0, |quotient|", text)

    def test_manual_checked_division_accepts_reversed_zero_guard(self) -> None:
        root = self.make_workspace(
            "fn ratio(value: u64, total: u64) -> u64 {\n"
            "    if 0 == total { 0 } else { value / total }\n"
            "}\n"
        )
        checker = self.run_checker(root, fix=True)
        self.assertNotIn("CLP008", self.codes(checker))
        text = (root / "src/lib.rs").read_text("utf-8")
        self.assertIn("value", text)
        self.assertIn(".checked_div(total)", text)

    def test_nonzero_fallback_is_not_reported_as_manual_checked_division(self) -> None:
        root = self.make_workspace(
            "fn ratio(value: u64, total: u64) -> u64 {\n"
            "    if total == 0 { 1 } else { value / total }\n"
            "}\n"
        )
        self.assertNotIn("CLP008", self.codes(self.run_checker(root)))

    def test_manual_checked_division_with_effectful_numerator_is_not_fixed(
        self,
    ) -> None:
        root = self.make_workspace(
            "fn next() -> u64 { 1 }\n"
            "fn bucket(total: u64) -> u64 {\n"
            "    if total == 0 { 0 } else { next() / total }\n"
            "}\n"
        )
        checker = self.run_checker(root, fix=True)
        self.assertIn("CLP008", self.codes(checker))
        self.assertIn("next() / total", (root / "src/lib.rs").read_text("utf-8"))

    def test_manual_checked_ops_allow_is_respected(self) -> None:
        root = self.make_workspace(
            "#[allow(clippy::manual_checked_ops)]\n"
            "fn bucket(completed: u64, total: u64) -> u64 {\n"
            "    if total == 0 { 0 } else { completed / total }\n"
            "}\n"
        )
        self.assertNotIn("CLP008", self.codes(self.run_checker(root)))

    def test_consecutive_blank_lines_are_reported_and_fixed(self) -> None:
        root = self.make_workspace("fn one() {}\n\n\nfn two() {}\n")
        self.assertIn("FMT005", self.codes(self.run_checker(root)))
        checker = self.run_checker(root, fix=True)
        self.assertNotIn("FMT005", self.codes(checker))
        self.assertEqual(
            "fn one() {}\n\nfn two() {}\n",
            (root / "src/lib.rs").read_text("utf-8"),
        )

    def test_same_root_imports_are_sorted_and_fixed(self) -> None:
        root = self.make_workspace(
            "mod values { pub fn a() {} pub fn b() {} }\n"
            "use values::b;\n"
            "use values::a;\n"
            "fn call() { a(); b(); }\n"
        )
        self.assertIn("FMT007", self.codes(self.run_checker(root)))
        checker = self.run_checker(root, fix=True)
        self.assertNotIn("FMT007", self.codes(checker))
        text = (root / "src/lib.rs").read_text("utf-8")
        self.assertLess(text.index("use values::a;"), text.index("use values::b;"))

    def test_different_root_imports_are_not_sorted_by_fallback(self) -> None:
        root = self.make_workspace(
            "mod alpha { pub fn a() {} }\nmod beta { pub fn b() {} }\n"
            "use beta::b;\nuse alpha::a;\nfn call() { a(); b(); }\n"
        )
        self.assertNotIn("FMT007", self.codes(self.run_checker(root)))

    def test_collapsible_match_negatives(self) -> None:
        # 1. Has guard already
        root1 = self.make_workspace(
            "fn main() {\n"
            "    match x {\n"
            "        Some(y) if y > 0 => {\n"
            '            println!("{}", y);\n'
            "        }\n"
            "    }\n"
            "}\n"
        )
        self.assertNotIn("CLP004", self.codes(self.run_checker(root1)))

        # 2. Has else branch
        root2 = self.make_workspace(
            "fn main() {\n"
            "    match x {\n"
            "        Some(y) => {\n"
            "            if y > 0 {\n"
            '                println!("{}", y);\n'
            "            } else {\n"
            "                other();\n"
            "            }\n"
            "        }\n"
            "    }\n"
            "}\n"
        )
        self.assertNotIn("CLP004", self.codes(self.run_checker(root2)))

        # 3. Has multiple statements in block
        root3 = self.make_workspace(
            "fn main() {\n"
            "    match x {\n"
            "        Some(y) => {\n"
            "            let z = y;\n"
            "            if z > 0 {\n"
            '                println!("{}", z);\n'
            "            }\n"
            "        }\n"
            "    }\n"
            "}\n"
        )
        self.assertNotIn("CLP004", self.codes(self.run_checker(root3)))


if __name__ == "__main__":
    unittest.main()
