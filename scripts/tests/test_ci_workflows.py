from __future__ import annotations

import json
import os
import re
import subprocess
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


def live_matrix_python(workflow: str) -> str:
    marker = '          python - <<\'PY_MATRIX\' >> "$GITHUB_OUTPUT"\n'
    start = workflow.index(marker) + len(marker)
    end = workflow.index("          PY_MATRIX\n", start)
    return "\n".join(
        line[10:] if line.startswith("          ") else line
        for line in workflow[start:end].splitlines()
    )


def run_live_matrix(workflow: str, **env: str) -> tuple[int, dict[str, str], str]:
    process_env = os.environ.copy()
    process_env.update(env)
    result = subprocess.run(
        ["python", "-c", live_matrix_python(workflow)],
        env=process_env,
        text=True,
        capture_output=True,
        check=False,
    )
    outputs = dict(
        line.split("=", 1)
        for line in result.stdout.splitlines()
        if "=" in line
    )
    return result.returncode, outputs, result.stderr


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

    def test_live_matrix_reuses_one_archived_workspace_per_platform(self) -> None:
        workflow = read(".github/workflows/live-e2e.yml")
        linux = job_block(workflow, "prepare-live-linux")
        windows = job_block(workflow, "prepare-live-windows")
        self.assertNotIn("self-hosted", workflow)
        self.assertIn("cargo nextest archive --workspace", linux)
        self.assertIn("nextest-linux-${{ needs.plan.outputs.live_ref }}", linux)
        self.assertIn("github.event.workflow_run.id", linux)
        self.assertIn("cargo nextest archive --workspace", windows)
        self.assertEqual(workflow.count("cargo nextest archive --workspace"), 2)
        self.assertIn("live-nextest-linux-${{ github.run_id }}", linux)
        self.assertIn("live-nextest-windows-${{ github.run_id }}", windows)

    def test_live_smoke_fans_out_by_known_deployment(self) -> None:
        workflow = read(".github/workflows/live-e2e.yml")
        smoke = job_block(workflow, "smoke")
        self.assertIn("name: Production smoke / ${{ matrix.label }}", workflow)
        self.assertIn("fail-fast: false", smoke)
        self.assertIn("fromJSON(needs.plan.outputs.smoke_matrix)", smoke)
        self.assertIn("live-nextest-linux-${{ github.run_id }}", smoke)
        self.assertIn("cargo-nextest nextest run", smoke)
        self.assertNotIn("cargo test", smoke)
        self.assertIn("GRIFFR_LIVE_SMOKE_SUB_CHANNEL", smoke)
        self.assertIn("integration_tests::test_real_api_contract_target", smoke)

        for target in (
            "endfield-cn-official",
            "endfield-cn-bilibili",
            "endfield-global-official",
            "endfield-global-epic",
            "endfield-global-google-play",
            "arknights-cn-official",
            "arknights-cn-bilibili",
            "arknights-en",
            "arknights-jp",
            "arknights-kr",
        ):
            self.assertIn(f'id="{target}"', workflow)

    def test_live_large_lanes_fan_out_by_os_and_target(self) -> None:
        workflow = read(".github/workflows/live-e2e.yml")
        self.assertIn("options: [all, linux, windows]", workflow)
        self.assertIn("options: [smoke, archive-sample, lifecycle, streaming, full-matrix]", workflow)

        for lane in ("lifecycle", "streaming"):
            for os_name in ("linux", "windows"):
                block = job_block(workflow, f"{lane}-{os_name}")
                self.assertIn("github.event_name == 'workflow_dispatch'", block)
                self.assertIn("environment: live-e2e", block)
                self.assertIn("fail-fast: false", block)
                self.assertIn("fromJSON(needs.plan.outputs.payload_matrix)", block)
                self.assertIn("prepare_live_workspace.py", block)
                self.assertIn("GRIFFR_LIVE_MIN_FREE_GIB", block)
                self.assertIn("cargo-nextest nextest run -P live-e2e", block)
                self.assertNotIn("cargo nextest archive", block)

        for os_name in ("linux", "windows"):
            block = job_block(workflow, f"archive-sample-{os_name}")
            self.assertIn("environment: live-e2e", block)
            self.assertIn("prepare_live_workspace.py", block)
            self.assertIn("cargo-nextest nextest run -P live-e2e", block)

    def test_live_matrix_builder_expands_all_payload_targets_on_both_platforms(self) -> None:
        workflow = read(".github/workflows/live-e2e.yml")
        code, outputs, stderr = run_live_matrix(
            workflow,
            EVENT_NAME="workflow_dispatch",
            TARGET_SELECTION="all",
            RUNNER_SELECTION="all",
            SMOKE="true",
            ARCHIVE_SAMPLE="false",
            LIFECYCLE="true",
            STREAMING="false",
        )
        self.assertEqual(code, 0, stderr)
        self.assertEqual(len(json.loads(outputs["smoke_matrix"])), 10)
        self.assertEqual(len(json.loads(outputs["payload_matrix"])), 7)
        self.assertEqual(outputs["run_linux"], "true")
        self.assertEqual(outputs["run_windows"], "true")

    def test_live_matrix_builder_rejects_yostar_payload_selection(self) -> None:
        workflow = read(".github/workflows/live-e2e.yml")
        code, _, stderr = run_live_matrix(
            workflow,
            EVENT_NAME="workflow_dispatch",
            TARGET_SELECTION="arknights-en",
            RUNNER_SELECTION="linux",
            SMOKE="true",
            ARCHIVE_SAMPLE="false",
            LIFECYCLE="true",
            STREAMING="false",
        )
        self.assertNotEqual(code, 0)
        self.assertIn("YoStar smoke-only", stderr)

    def test_live_payload_matrix_excludes_yostar_large_downloads(self) -> None:
        workflow = read(".github/workflows/live-e2e.yml")
        self.assertIn('payload=False, resources=False', workflow)
        self.assertIn('is YoStar smoke-only; lifecycle/streaming currently require', workflow)
        self.assertIn('payload = [target for target in selected if target["payload"]]', workflow)
        self.assertIn("matrix.resources && inputs.resources || 'off'", workflow)

    def test_live_payload_lanes_use_long_running_nextest_profile(self) -> None:
        workflow = read(".github/workflows/live-e2e.yml")
        nextest = read(".config/nextest.toml")
        self.assertIn('[profile.ci]', nextest)
        self.assertIn('slow-timeout = { period = "60s", terminate-after = 5 }', nextest)
        self.assertIn('[profile.live-e2e]', nextest)
        self.assertIn('slow-timeout = { period = "15m", terminate-after = 20 }', nextest)
        for lane in (
            "archive-sample-linux",
            "archive-sample-windows",
            "lifecycle-linux",
            "lifecycle-windows",
            "streaming-linux",
            "streaming-windows",
        ):
            block = job_block(workflow, lane)
            self.assertIn("cargo-nextest nextest run -P live-e2e", block)
            self.assertNotIn("nextest run -P ci", block)

    def test_live_lifecycle_checks_manifest_budget_before_each_matrix_cell(self) -> None:
        workflow = read(".github/workflows/live-e2e.yml")
        for lane in ("lifecycle-linux", "lifecycle-windows"):
            lifecycle = job_block(workflow, lane)
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
        self.assertEqual(workflow.count('GRIFFR_LIVE_E2E_RESOURCES: "off"'), 2)

    def test_live_hosted_root_defaults_are_safe_and_optional(self) -> None:
        workflow = read(".github/workflows/live-e2e.yml")
        self.assertIn("Blank uses a lane-scoped directory under RUNNER_TEMP", workflow)
        self.assertIn("minimum_free_gib:", workflow)
        self.assertIn('default: "0"', workflow)
        self.assertNotIn("workflow_dispatch input root is required", workflow)

    def test_live_automatic_lane_is_read_only_parallel_smoke(self) -> None:
        workflow = read(".github/workflows/live-e2e.yml")
        self.assertIn("workflow_run:", workflow)
        self.assertIn("schedule:", workflow)
        plan = job_block(workflow, "plan")
        self.assertNotIn("actions/checkout", plan)
        self.assertIn('print("archive_sample=false")', plan)
        self.assertIn('print("lifecycle=false")', plan)
        self.assertIn('print("streaming=false")', plan)
        smoke = job_block(workflow, "smoke")
        self.assertIn("runs-on: ubuntu-24.04", smoke)
        self.assertNotIn("GRIFFR_LIVE_E2E_CONFIRM", smoke)
        self.assertIn("recommended=smoke-fallback", workflow)

    def test_extended_platform_owns_real_wine_smoke(self) -> None:
        workflow = read(".github/workflows/extended-platform.yml")
        self.assertIn("wine", workflow)
        self.assertIn("real_wine_launch_smoke", workflow)
        self.assertIn("--ignored --exact", workflow)


if __name__ == "__main__":
    unittest.main()
