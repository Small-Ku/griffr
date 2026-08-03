from __future__ import annotations

import importlib.util
import tempfile
import sys
import textwrap
import unittest
from pathlib import Path

SCRIPT = Path(__file__).parents[1] / "check_repo.py"
SPEC = importlib.util.spec_from_file_location("check_repo", SCRIPT)
assert SPEC is not None and SPEC.loader is not None
check_repo = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = check_repo
SPEC.loader.exec_module(check_repo)


class CheckerTests(unittest.TestCase):
    def make_repo(self, files: dict[str, str]) -> Path:
        temp = tempfile.TemporaryDirectory()
        self.addCleanup(temp.cleanup)
        root = Path(temp.name)
        (root / "Cargo.toml").write_text(
            '[workspace]\nmembers = ["crates/griffr-common"]\n', encoding="utf-8"
        )
        common_manifest = root / "crates/griffr-common/Cargo.toml"
        common_manifest.parent.mkdir(parents=True, exist_ok=True)
        common_manifest.write_text(
            '[package]\nname = "griffr-common"\nversion = "0.0.0"\n',
            encoding="utf-8",
        )
        for relative, body in files.items():
            path = root / relative
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text(textwrap.dedent(body), encoding="utf-8")
        return root

    def codes(self, root: Path) -> set[str]:
        return {item.code for item in check_repo.Checker(root).run()}

    def test_rejects_frontend_dependency(self) -> None:
        root = self.make_repo({})
        (root / "crates/griffr-common/Cargo.toml").write_text(
            '[package]\nname = "griffr-common"\nversion = "0.0.0"\n'
            '[dependencies]\nindicatif = "0.18"\n',
            encoding="utf-8",
        )
        self.assertIn("ARC001", self.codes(root))

    def test_rejects_raw_progress_channel_outside_wrapper(self) -> None:
        root = self.make_repo(
            {
                "crates/griffr-common/src/lib.rs": """
                    pub struct Bad {
                        tx: flume::Sender<ProgressUpdate>,
                    }
                """
            }
        )
        self.assertIn("PRG001", self.codes(root))

    def test_allows_raw_progress_channel_in_wrapper(self) -> None:
        root = self.make_repo(
            {
                "crates/griffr-common/src/runtime/progress.rs": """
                    pub struct ProgressSender {
                        tx: Option<flume::Sender<ProgressUpdate>>,
                    }
                """
            }
        )
        self.assertNotIn("PRG001", self.codes(root))

    def test_rejects_public_progress_callback(self) -> None:
        root = self.make_repo(
            {
                "crates/griffr-common/src/lib.rs": """
                    pub async fn run(progress_callback: impl FnMut(u64)) {}
                """
            }
        )
        self.assertIn("PRG003", self.codes(root))

    def test_allows_crate_private_callback(self) -> None:
        root = self.make_repo(
            {
                "crates/griffr-common/src/lib.rs": """
                    pub(crate) fn run(progress_callback: impl FnMut(u64)) {}
                """
            }
        )
        self.assertNotIn("PRG003", self.codes(root))

    def test_rejects_lane_constructor_outside_catalog(self) -> None:
        root = self.make_repo(
            {
                "crates/griffr-common/src/lib.rs": """
                    fn lane() { let _ = ProgressLane::new(scope, phase); }
                """
            }
        )
        self.assertIn("PRG002", self.codes(root))

    def test_rejects_custom_task_pool_thread(self) -> None:
        root = self.make_repo(
            {
                "crates/griffr-common/src/runtime/task_pool/queue.rs": """
                    fn start() { let _ = std::thread::Builder::new(); }
                """
            }
        )
        self.assertIn("DSP001", self.codes(root))

    def test_allows_task_pool_retry_sleep(self) -> None:
        root = self.make_repo(
            {
                "crates/griffr-common/src/runtime/task_pool/queue.rs": """
                    fn retry() { std::thread::sleep(delay); }
                """
            }
        )
        self.assertNotIn("DSP001", self.codes(root))

    def test_rejects_std_fs_in_async_function(self) -> None:
        root = self.make_repo(
            {
                "crates/griffr-common/src/lib.rs": """
                    async fn load(path: &Path) {
                        let _ = std::fs::read(path);
                    }
                """
            }
        )
        self.assertIn("AFS001", self.codes(root))

    def test_rejects_imported_fs_alias_in_async_function(self) -> None:
        root = self.make_repo(
            {
                "crates/griffr-common/src/lib.rs": """
                    use std::fs as blocking_fs;
                    async fn load(path: &Path) {
                        let _ = blocking_fs::read(path);
                    }
                """
            }
        )
        self.assertIn("AFS001", self.codes(root))

    def test_allows_std_fs_inside_blocking_boundary(self) -> None:
        root = self.make_repo(
            {
                "crates/griffr-common/src/lib.rs": """
                    async fn load(path: PathBuf) {
                        dispatch_blocking(move || {
                            let _ = std::fs::read(path);
                        }).await;
                    }
                """
            }
        )
        self.assertNotIn("AFS001", self.codes(root))

    def test_allows_std_fs_inside_expression_blocking_boundary(self) -> None:
        root = self.make_repo(
            {
                "crates/griffr-common/src/lib.rs": """
                    async fn load(path: PathBuf) {
                        run_blocking("read", move || std::fs::read(path)).await;
                    }
                """
            }
        )
        self.assertNotIn("AFS001", self.codes(root))

    def test_ignores_async_trait_declaration_without_body(self) -> None:
        root = self.make_repo(
            {
                "crates/griffr-common/src/lib.rs": """
                    trait Load {
                        async fn load(path: &Path);
                    }
                    fn sync_example(path: &Path) { let _ = std::fs::read(path); }
                """
            }
        )
        self.assertNotIn("AFS001", self.codes(root))

    def test_ignores_inline_test_module(self) -> None:
        root = self.make_repo(
            {
                "crates/griffr-common/src/lib.rs": """
                    #[cfg(test)]
                    mod tests {
                        async fn sample(path: &Path) {
                            let _ = std::fs::read(path);
                        }
                    }
                """
            }
        )
        self.assertNotIn("AFS001", self.codes(root))

    def test_comments_and_strings_do_not_trigger_rules(self) -> None:
        root = self.make_repo(
            {
                "crates/griffr-common/src/lib.rs": r'''
                    // std::fs::read and ProgressLane::new are examples.
                    const SAMPLE: &str = "flume::Sender<ProgressUpdate>";
                '''
            }
        )
        codes = self.codes(root)
        self.assertNotIn("AFS001", codes)
        self.assertNotIn("PRG001", codes)
        self.assertNotIn("PRG002", codes)

    def test_rejects_removed_model_name(self) -> None:
        root = self.make_repo(
            {"crates/griffr-common/src/lib.rs": "struct TransferDownload;"}
        )
        self.assertIn("SSOT001", self.codes(root))

    def test_rejects_vague_file_name(self) -> None:
        root = self.make_repo({"crates/griffr-common/src/model.rs": ""})
        self.assertIn("NAM001", self.codes(root))


if __name__ == "__main__":
    unittest.main()
