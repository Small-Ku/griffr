#!/usr/bin/env python3
"""Classify changed repository paths into live-test recommendations.

This is deliberately path-based and conservative. It never runs live tests itself;
GitHub Actions and humans use the emitted booleans to decide which live lane is
appropriate after deterministic CI has passed.
"""

from __future__ import annotations

import argparse
import json
import subprocess
from dataclasses import asdict, dataclass
from pathlib import PurePosixPath
from typing import Iterable


@dataclass(frozen=True)
class LiveE2ePolicy:
    smoke: bool = False
    archive_sample: bool = False
    lifecycle: bool = False
    streaming: bool = False

    @property
    def recommended(self) -> str:
        modes: list[str] = []
        if self.smoke:
            modes.append("smoke")
        if self.archive_sample:
            modes.append("archive-sample")
        if self.lifecycle:
            modes.append("lifecycle")
        if self.streaming:
            modes.append("streaming")
        return ",".join(modes) if modes else "none"


SMOKE_PREFIXES = (
    "crates/griffr-core/src/",
    "crates/griffr-hypergryph-api/src/",
    "crates/griffr-yostar-api/src/",
    "crates/griffr-cli/src/target.rs",
    "crates/griffr-cli/src/main/",
    "crates/griffr-cli/src/commands/debug/",
    "crates/griffr-cli/src/commands/info.rs",
    "crates/griffr-cli/src/commands/news.rs",
    "crates/griffr-cli/src/commands/yostar.rs",
    "crates/griffr-cli/tests/live_api_smoke.rs",
)

LIFECYCLE_PREFIXES = (
    "crates/griffr-runtime/src/artifact.rs",
    "crates/griffr-runtime/src/compat_fs.rs",
    "crates/griffr-runtime/src/file_allocation.rs",
    "crates/griffr-runtime/src/files/",
    "crates/griffr-runtime/src/hash.rs",
    "crates/griffr-runtime/src/install_change/",
    "crates/griffr-runtime/src/integrity.rs",
    "crates/griffr-runtime/src/issues.rs",
    "crates/griffr-runtime/src/local_install.rs",
    "crates/griffr-runtime/src/patch_apply/",
    "crates/griffr-runtime/src/paths.rs",
    "crates/griffr-runtime/src/task_pool/",
    "crates/griffr-runtime/src/update_plan.rs",
    "crates/griffr-runtime/src/yostar_files.rs",
    "crates/griffr-runtime/src/yostar_metadata.rs",
    "crates/griffr-cli/src/commands/install/",
    "crates/griffr-cli/src/commands/update/",
    "crates/griffr-cli/src/commands/verify.rs",
    "crates/griffr-cli/src/commands/predownload.rs",
    "crates/griffr-cli/src/commands/setup_persistent_resources.rs",
    "crates/griffr-cli/src/commands/uninstall.rs",
    "crates/griffr-cli/tests/live_cli_e2e.rs",
    "scripts/test_live_cli_e2e.sh",
    "scripts/test_live_cli_e2e.ps1",
)

ARCHIVE_SAMPLE_PREFIXES = (
    "crates/griffr-runtime/src/download/extractor/",
)

STREAMING_PREFIXES = (
    "crates/griffr-runtime/src/content_plan.rs",
    "crates/griffr-runtime/src/download/mod.rs",
    "crates/griffr-runtime/src/http_download.rs",
    "crates/griffr-runtime/src/task_pool/archive_plan.rs",
    "crates/griffr-runtime/src/task_pool/blocking_buffer.rs",
    "crates/griffr-runtime/src/task_pool/download.rs",
    "crates/griffr-runtime/src/task_pool/download_write.rs",
    "crates/griffr-hypergryph-api/src/client/",
    "crates/griffr-hypergryph-api/src/types/",
    "crates/griffr-yostar-api/src/client.rs",
    "crates/griffr-cli/tests/live_cli_e2e.rs",
    "scripts/test_live_streaming.sh",
)

GLOBAL_BUILD_FILES = {
    "Cargo.toml",
    "Cargo.lock",
    "rust-toolchain.toml",
    ".cargo/config.toml",
}


def _matches(path: str, prefixes: tuple[str, ...]) -> bool:
    return any(
        path.startswith(prefix) if prefix.endswith("/") else path == prefix
        for prefix in prefixes
    )


def classify(paths: Iterable[str]) -> LiveE2ePolicy:
    smoke = archive_sample = lifecycle = streaming = False
    for raw_path in paths:
        path = PurePosixPath(raw_path.replace("\\", "/")).as_posix()
        if path.startswith("./"):
            path = path[2:]
        if not path or path.startswith(("docs/", ".github/")):
            continue
        if path in GLOBAL_BUILD_FILES or (
            path.startswith("crates/") and path.endswith("/Cargo.toml")
        ):
            smoke = archive_sample = lifecycle = streaming = True
            continue
        if _matches(path, SMOKE_PREFIXES):
            smoke = True
        if _matches(path, LIFECYCLE_PREFIXES):
            lifecycle = True
        if _matches(path, ARCHIVE_SAMPLE_PREFIXES):
            archive_sample = True
            lifecycle = True
        if _matches(path, STREAMING_PREFIXES):
            streaming = True
            lifecycle = True
    return LiveE2ePolicy(
        smoke=smoke,
        archive_sample=archive_sample,
        lifecycle=lifecycle,
        streaming=streaming,
    )


def changed_paths(base: str, head: str) -> list[str]:
    result = subprocess.run(
        ["git", "diff", "--name-only", "--diff-filter=ACMR", base, head, "--"],
        check=True,
        text=True,
        stdout=subprocess.PIPE,
    )
    return [line.strip() for line in result.stdout.splitlines() if line.strip()]


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("paths", nargs="*")
    parser.add_argument("--base")
    parser.add_argument("--head", default="HEAD")
    parser.add_argument("--github-output", action="store_true")
    args = parser.parse_args()

    if args.base and args.paths:
        parser.error("pass paths or --base, not both")
    paths = changed_paths(args.base, args.head) if args.base else args.paths
    policy = classify(paths)
    payload = {**asdict(policy), "recommended": policy.recommended, "paths": paths}

    if args.github_output:
        for key in ("smoke", "archive_sample", "lifecycle", "streaming"):
            print(f"{key}={str(payload[key]).lower()}")
        print(f"recommended={policy.recommended}")
    else:
        print(json.dumps(payload, indent=2, sort_keys=True))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
