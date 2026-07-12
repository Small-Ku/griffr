from __future__ import annotations

import os
import shutil
import subprocess
import tempfile
from pathlib import Path
from typing import Any, Protocol

from .models import ToolResult


class ToolHost(Protocol):
    root: Path
    run_tools: str
    include_tests: bool
    max_tool_output: int
    root_manifest_path: Path
    tool_results: list[ToolResult]

    def add(self, code: str, severity: str, message: str, **kwargs: Any) -> None: ...


def run(host: ToolHost) -> None:
    if host.run_tools == "never":
        return
    if not host.root_manifest_path.is_file():
        return

    commands = [
        ("cargo fmt", ["cargo", "fmt", "--all", "--", "--check"]),
        (
            "cargo check",
            ["cargo", "check", "--workspace", "--all-targets", "--all-features"],
        ),
        (
            "cargo clippy",
            [
                "cargo",
                "clippy",
                "--workspace",
                "--all-targets",
                "--all-features",
                "--",
                "-D",
                "warnings",
            ],
        ),
    ]
    if host.include_tests:
        commands.append(
            (
                "cargo test",
                [
                    "cargo",
                    "test",
                    "--workspace",
                    "--all-targets",
                    "--all-features",
                    "--no-fail-fast",
                ],
            )
        )
    for label, command in commands:
        _run_one(host, label, command)


def _run_one(host: ToolHost, label: str, command: list[str]) -> None:
    available = shutil.which(command[0]) is not None
    if not available:
        host.tool_results.append(ToolResult(label, command, False, None, ""))
        if host.run_tools == "required":
            host.add(
                "TOOL001",
                "error",
                f"Required tool is unavailable: {command[0]}",
                hint=f"Install {command[0]} or use --run-tools auto/never.",
            )
        return

    env = dict(os.environ, CARGO_TERM_COLOR="never")
    with tempfile.TemporaryFile(mode="w+b") as capture:
        process = subprocess.run(
            command,
            cwd=host.root,
            stdout=capture,
            stderr=subprocess.STDOUT,
            env=env,
            check=False,
        )
        size = capture.tell()
        truncated = size > host.max_tool_output
        capture.seek(max(0, size - host.max_tool_output))
        output = capture.read().decode("utf-8", "replace")
    if truncated:
        output = (
            f"[output truncated; retained last {host.max_tool_output} bytes of {size}]\n"
            + output
        )

    host.tool_results.append(
        ToolResult(label, command, True, process.returncode, output)
    )
    if process.returncode != 0:
        host.add(
            f"TOOL_{label.upper().replace(' ', '_')}",
            "error",
            f"{label} failed with exit code {process.returncode}",
            hint="See the captured tool output in the report.",
        )
