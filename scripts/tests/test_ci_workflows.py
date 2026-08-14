from __future__ import annotations

import re
import unittest
from pathlib import Path


ROOT = Path(__file__).resolve().parents[2]


def read(relative: str) -> str:
    return (ROOT / relative).read_text(encoding="utf-8")


def job_block(workflow: str, name: str) -> str:
    marker = f"  {name}:\n"
    tail = workflow.split(marker, 1)[1]
    match = re.search(r"^  [A-Za-z0-9_-]+:\n", tail, re.MULTILINE)
    return tail[: match.start()] if match else tail


class CiWorkflowTopologyTests(unittest.TestCase):
    def test_rust_setup_uses_sccache_without_target_cache(self) -> None:
        action = read(".github/actions/setup-rust/action.yml")
        self.assertIn("mozilla-actions/sccache-action@v0.0.11", action)
        self.assertIn("version: v0.17.0", action)
        self.assertIn("SCCACHE_GHA_ENABLED=true", action)
        self.assertIn("SCCACHE_CLIENT_SIDE=true", action)
        self.assertIn("RUSTC_WRAPPER=sccache", action)
        self.assertIn("if: runner.os != 'Windows'", action)
        self.assertIn("if: runner.os == 'Windows'", action)
        self.assertIn("shell: pwsh", action)
        self.assertNotIn("rustup default", action)
        self.assertNotIn("target/", action)

    def test_repository_policy_installs_actionlint_from_pinned_release(self) -> None:
        workflow = read(".github/workflows/ci.yml")
        policy = job_block(workflow, "repository-policy")
        self.assertIn('ACTIONLINT_VERSION: "1.7.12"', policy)
        self.assertIn(
            'ACTIONLINT_SHA256: "8aca8db96f1b94770f1b0d72b6dddcb1ebb8123cb3712530b08cc387b349a3d8"',
            policy,
        )
        self.assertIn("rhysd/actionlint/releases/download", policy)
        self.assertIn("sha256sum --check --strict", policy)
        self.assertNotIn("tool: actionlint@", policy)
        self.assertNotIn("cargo-binstall", policy)

    def test_feature_boundaries_wait_for_static_gates(self) -> None:
        workflow = read(".github/workflows/ci.yml")
        feature = job_block(workflow, "feature-boundaries")
        self.assertIn("needs: [repository-policy, formatting]", feature)

    def test_ci_builds_one_nextest_archive_per_platform_then_shards(self) -> None:
        workflow = read(".github/workflows/ci.yml")
        self.assertEqual(workflow.count("cargo nextest archive --workspace"), 2)
        self.assertIn("nextest-linux.tar.zst", workflow)
        self.assertIn("nextest-windows.tar.zst", workflow)
        self.assertIn('shard: [1, 2, 3, 4]', workflow)
        self.assertEqual(workflow.count("--partition \"hash:${{ matrix.shard }}/4\""), 2)
        self.assertEqual(workflow.count('--workspace-remap "${{ github.workspace }}"'), 2)

    def test_archive_test_runners_do_not_recompile_workspace(self) -> None:
        workflow = read(".github/workflows/ci.yml")
        linux = workflow.split("  test-linux:", 1)[1].split("  test-windows:", 1)[0]
        windows = workflow.split("  test-windows:", 1)[1].split("  required:", 1)[0]
        for lane in (linux, windows):
            self.assertIn("actions/download-artifact@v8.0.1", lane)
            self.assertIn("cargo-nextest nextest run", lane)
            self.assertNotIn("./.github/actions/setup-rust", lane)
            self.assertNotIn("cargo test", lane)
            self.assertNotIn("cargo build", lane)
        self.assertNotIn("shell: bash", windows)

    def test_quality_and_archive_builds_fan_out_after_static_gates(self) -> None:
        workflow = read(".github/workflows/ci.yml")
        self.assertIn(
            "quality-linux:\n    name: Check + Clippy / Linux\n    needs: [repository-policy, formatting]",
            workflow,
        )
        self.assertIn(
            "quality-windows:\n    name: Check + Clippy / Windows\n    needs: [repository-policy, formatting]",
            workflow,
        )
        self.assertIn(
            "build-tests-linux:\n    name: Compile test archive / Linux\n    needs: [repository-policy, formatting]",
            workflow,
        )

    def test_required_job_aggregates_every_required_lane(self) -> None:
        workflow = read(".github/workflows/ci.yml")
        required = workflow.split("  required:", 1)[1]
        for job in (
            "repository-policy",
            "formatting",
            "feature-boundaries",
            "build-tests-linux",
            "build-tests-windows",
            "quality-linux",
            "quality-windows",
            "test-linux",
            "test-windows",
        ):
            self.assertIn(f"- {job}", required)
        self.assertIn("if: always()", required)
        self.assertIn('if [[ "$result" != success ]]', required)

    def test_live_large_lanes_are_manual_hosted_and_smoke_gated(self) -> None:
        workflow = read(".github/workflows/live-e2e.yml")
        self.assertNotIn("self-hosted", workflow)
        self.assertNotIn("griffr-live", workflow)
        hosted_runner = (
            "runs-on: ${{ inputs.runner_os == 'windows' && 'windows-2025' || 'ubuntu-24.04' }}"
        )
        for lane, output in (
            ("archive-sample", "archive_sample"),
            ("lifecycle", "lifecycle"),
            ("streaming", "streaming"),
        ):
            block = job_block(workflow, lane)
            self.assertIn("needs: [plan, smoke]", block)
            self.assertIn("github.event_name == 'workflow_dispatch'", block)
            self.assertIn(f"needs.plan.outputs.{output} == 'true'", block)
            self.assertIn("needs.smoke.result == 'success'", block)
            self.assertIn(hosted_runner, block)
            self.assertIn("environment: live-e2e", block)
            self.assertIn("cargo nextest archive", block)
            self.assertIn("cargo clean", block)
            self.assertIn("prepare_live_workspace.py", block)
            self.assertIn("GRIFFR_LIVE_MIN_FREE_GIB", block)

    def test_live_lifecycle_reclaims_build_space_and_checks_manifest_budget(self) -> None:
        workflow = read(".github/workflows/live-e2e.yml")
        lifecycle = job_block(workflow, "lifecycle")
        self.assertLess(lifecycle.index("cargo nextest archive"), lifecycle.index("cargo clean"))
        self.assertLess(lifecycle.index("cargo clean"), lifecycle.index("prepare_live_workspace.py"))
        self.assertLess(
            lifecycle.index("official_server_content_lifecycle_disk_preflight"),
            lifecycle.index("official_server_content_lifecycle_without_launch"),
        )
        self.assertIn('options: ["off", base, all]', workflow)
        self.assertIn('default: "off"', workflow)
        self.assertNotIn("default: off", workflow)

    def test_live_resource_off_is_always_a_quoted_string(self) -> None:
        workflow = read(".github/workflows/live-e2e.yml")
        self.assertNotIn("GRIFFR_LIVE_E2E_RESOURCES: off", workflow)
        self.assertIn('GRIFFR_LIVE_E2E_RESOURCES: "off"', workflow)
        self.assertEqual(workflow.count('GRIFFR_LIVE_E2E_RESOURCES: "off"'), 1)

    def test_live_hosted_root_defaults_are_safe_and_optional(self) -> None:
        workflow = read(".github/workflows/live-e2e.yml")
        self.assertIn("Blank uses a lane-scoped directory under RUNNER_TEMP", workflow)
        self.assertIn("minimum_free_gib:", workflow)
        self.assertIn('default: "0"', workflow)
        self.assertNotIn("workflow_dispatch input root is required", workflow)

    def test_live_automatic_lane_is_read_only_smoke(self) -> None:
        workflow = read(".github/workflows/live-e2e.yml")
        self.assertIn("workflow_run:", workflow)
        self.assertIn("schedule:", workflow)
        plan = job_block(workflow, "plan")
        self.assertNotIn("actions/checkout", plan)
        smoke = job_block(workflow, "smoke")
        self.assertIn("runs-on: ubuntu-24.04", smoke)
        self.assertIn("live_api_smoke", smoke)
        self.assertNotIn("GRIFFR_LIVE_E2E_CONFIRM", smoke)
        self.assertIn("recommended=smoke-fallback", workflow)

    def test_extended_platform_owns_real_wine_smoke(self) -> None:
        workflow = read(".github/workflows/extended-platform.yml")
        self.assertIn("wine", workflow)
        self.assertIn("real_wine_launch_smoke", workflow)
        self.assertIn("--ignored --exact", workflow)


if __name__ == "__main__":
    unittest.main()
