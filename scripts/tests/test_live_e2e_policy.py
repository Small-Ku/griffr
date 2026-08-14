from __future__ import annotations

import importlib.util
import sys
import unittest
from pathlib import Path


ROOT = Path(__file__).resolve().parents[2]
SPEC = importlib.util.spec_from_file_location(
    "live_e2e_policy", ROOT / "scripts" / "ci" / "live_e2e_policy.py"
)
assert SPEC and SPEC.loader
MODULE = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = MODULE
SPEC.loader.exec_module(MODULE)
classify = MODULE.classify


class LiveE2ePolicyTests(unittest.TestCase):
    def test_docs_only_needs_no_live_lane(self) -> None:
        policy = classify(["docs/TESTING.md", ".github/workflows/ci.yml"])
        self.assertEqual(policy.recommended, "none")

    def test_provider_protocol_change_needs_read_only_smoke(self) -> None:
        policy = classify(["crates/griffr-yostar-api/src/target.rs"])
        self.assertTrue(policy.smoke)
        self.assertFalse(policy.archive_sample)
        self.assertFalse(policy.lifecycle)
        self.assertFalse(policy.streaming)

    def test_install_engine_change_needs_lifecycle_without_remote_smoke(self) -> None:
        policy = classify(["crates/griffr-runtime/src/local_install.rs"])
        self.assertFalse(policy.smoke)
        self.assertTrue(policy.lifecycle)
        self.assertFalse(policy.archive_sample)
        self.assertFalse(policy.streaming)

    def test_extractor_change_needs_bounded_archive_sample(self) -> None:
        policy = classify(["crates/griffr-runtime/src/download/extractor/range.rs"])
        self.assertEqual(policy.recommended, "archive-sample,lifecycle")

    def test_download_change_also_needs_streaming_soak(self) -> None:
        policy = classify(["crates/griffr-runtime/src/task_pool/download.rs"])
        self.assertEqual(
            policy.recommended, "lifecycle,streaming"
        )

    def test_launcher_change_stays_in_extended_platform_lane(self) -> None:
        policy = classify(["crates/griffr-runtime/src/launcher.rs"])
        self.assertEqual(policy.recommended, "none")

    def test_predownload_cli_change_needs_lifecycle(self) -> None:
        policy = classify(["crates/griffr-cli/src/commands/predownload.rs"])
        self.assertEqual(policy.recommended, "lifecycle")

    def test_provider_package_schema_needs_smoke_and_streaming(self) -> None:
        policy = classify(["crates/griffr-hypergryph-api/src/types/core.rs"])
        self.assertEqual(policy.recommended, "smoke,lifecycle,streaming")

    def test_workspace_dependency_change_is_conservative(self) -> None:
        policy = classify(["Cargo.lock"])
        self.assertEqual(
            policy.recommended, "smoke,archive-sample,lifecycle,streaming"
        )

    def test_download_write_change_needs_streaming(self) -> None:
        policy = classify(["crates/griffr-runtime/src/task_pool/download_write.rs"])
        self.assertEqual(policy.recommended, "lifecycle,streaming")

    def test_exact_file_match_does_not_match_similar_filename(self) -> None:
        policy = classify(["crates/griffr-runtime/src/local_install.rs.old"])
        self.assertEqual(policy.recommended, "none")

    def test_live_cli_e2e_harness_recommends_lifecycle_and_streaming(self) -> None:
        policy = classify(["crates/griffr-cli/tests/live_cli_e2e.rs"])
        self.assertEqual(policy.recommended, "lifecycle,streaming")

    def test_live_cli_script_recommends_lifecycle(self) -> None:
        policy = classify(["scripts/test_live_cli_e2e.sh"])
        self.assertEqual(policy.recommended, "lifecycle")

    def test_live_streaming_script_recommends_lifecycle_and_streaming(self) -> None:
        policy = classify(["scripts/test_live_streaming.sh"])
        self.assertEqual(policy.recommended, "lifecycle,streaming")

    def test_crate_manifest_change_is_conservative(self) -> None:
        policy = classify(["crates/griffr-runtime/Cargo.toml"])
        self.assertEqual(
            policy.recommended, "smoke,archive-sample,lifecycle,streaming"
        )

    def test_windows_paths_are_normalized(self) -> None:
        policy = classify([r"crates\griffr-runtime\src\files\reuse\ensure.rs"])
        self.assertTrue(policy.lifecycle)


if __name__ == "__main__":
    unittest.main()
