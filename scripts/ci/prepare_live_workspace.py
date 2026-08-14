#!/usr/bin/env python3
"""Prepare a safe live-E2E root and enforce a minimum free-space floor.

The live Rust tests retain their own deletion guards. This helper adds a CI-side
preflight that works on both GitHub-hosted Linux and Windows runners, chooses a
safe RUNNER_TEMP default, and fails before production downloads begin when the
selected volume is already too full.
"""

from __future__ import annotations

import argparse
import os
import shutil
from dataclasses import dataclass
from pathlib import Path


GIB = 1024**3
DEFAULT_MINIMUM_GIB = {
    "archive-sample": 3,
    "lifecycle": 4,
    "streaming": 6,
}


@dataclass(frozen=True)
class LiveWorkspace:
    root: Path
    free_bytes: int
    minimum_free_bytes: int


def _resolve_existing(path: Path) -> Path:
    return path.expanduser().resolve(strict=False)


def _is_relative_to(path: Path, other: Path) -> bool:
    try:
        path.relative_to(other)
    except ValueError:
        return False
    return True


def _validate_root(root: Path, workspace: Path | None, home: Path | None) -> None:
    anchor = Path(root.anchor).resolve(strict=False) if root.anchor else None
    if anchor is not None and root == anchor:
        raise ValueError(f"refusing filesystem root as live E2E root: {root}")

    if home is not None and root == home:
        raise ValueError(f"refusing home directory as live E2E root: {root}")

    if workspace is not None and (
        root == workspace
        or _is_relative_to(root, workspace)
        or _is_relative_to(workspace, root)
    ):
        raise ValueError(
            f"live E2E root must be separate from the checked-out workspace: {root}"
        )


def prepare_workspace(
    *,
    lane: str,
    root_input: str | None,
    runner_temp: str | None,
    workspace: str | None,
    minimum_free_gib: int = 0,
    home: str | None = None,
) -> LiveWorkspace:
    if lane not in DEFAULT_MINIMUM_GIB:
        raise ValueError(f"unknown live lane: {lane}")
    if minimum_free_gib < 0:
        raise ValueError("minimum free GiB cannot be negative")

    if root_input and root_input.strip():
        root = _resolve_existing(Path(root_input.strip()))
    else:
        if not runner_temp:
            raise ValueError("RUNNER_TEMP is required when no explicit live root is supplied")
        root = _resolve_existing(Path(runner_temp) / "griffr-live-e2e" / lane)

    workspace_path = _resolve_existing(Path(workspace)) if workspace else None
    home_path = _resolve_existing(Path(home)) if home else None
    _validate_root(root, workspace_path, home_path)

    root.mkdir(parents=True, exist_ok=True)
    free_bytes = shutil.disk_usage(root).free
    floor_gib = minimum_free_gib or DEFAULT_MINIMUM_GIB[lane]
    minimum_free_bytes = floor_gib * GIB
    if free_bytes < minimum_free_bytes:
        raise RuntimeError(
            f"live {lane} needs at least {floor_gib} GiB free before production I/O; "
            f"{free_bytes / GIB:.2f} GiB is available at {root}"
        )

    return LiveWorkspace(
        root=root,
        free_bytes=free_bytes,
        minimum_free_bytes=minimum_free_bytes,
    )


def _append_github_output(path: str, workspace: LiveWorkspace) -> None:
    with open(path, "a", encoding="utf-8") as output:
        print(f"root={workspace.root}", file=output)
        print(f"free_bytes={workspace.free_bytes}", file=output)
        print(f"minimum_free_bytes={workspace.minimum_free_bytes}", file=output)


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--lane", choices=tuple(DEFAULT_MINIMUM_GIB), required=True)
    parser.add_argument("--root")
    parser.add_argument("--minimum-free-gib", type=int, default=0)
    parser.add_argument("--github-output", action="store_true")
    args = parser.parse_args()

    root_input = args.root
    if root_input is None:
        root_input = os.environ.get("GRIFFR_LIVE_ROOT_INPUT")
    minimum_free_gib = args.minimum_free_gib
    if minimum_free_gib == 0:
        raw_override = os.environ.get("GRIFFR_LIVE_MIN_FREE_GIB", "").strip()
        if raw_override:
            try:
                minimum_free_gib = int(raw_override)
            except ValueError as error:
                raise SystemExit(
                    f"GRIFFR_LIVE_MIN_FREE_GIB must be a non-negative integer, got {raw_override!r}"
                ) from error

    try:
        live = prepare_workspace(
            lane=args.lane,
            root_input=root_input,
            runner_temp=os.environ.get("RUNNER_TEMP"),
            workspace=os.environ.get("GITHUB_WORKSPACE"),
            minimum_free_gib=minimum_free_gib,
            home=str(Path.home()),
        )
    except (OSError, RuntimeError, ValueError) as error:
        raise SystemExit(str(error)) from error

    print(
        f"live {args.lane} root: {live.root}\n"
        f"free: {live.free_bytes / GIB:.2f} GiB; "
        f"required floor: {live.minimum_free_bytes / GIB:.2f} GiB"
    )
    if args.github_output:
        output_path = os.environ.get("GITHUB_OUTPUT")
        if not output_path:
            raise SystemExit("GITHUB_OUTPUT is required with --github-output")
        _append_github_output(output_path, live)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
