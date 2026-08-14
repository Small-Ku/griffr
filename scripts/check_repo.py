#!/usr/bin/env python3
"""Check Griffr-specific repository policies.

This checker intentionally does not duplicate rustfmt, rustc, Clippy, Cargo, or
the Rust test suite. It uses only the Python standard library and checks rules
that those tools do not understand: layered crate boundaries, progress API
shape, task-pool execution policy, explicit blocking filesystem calls in async
code, removed model names, and broad file names.
"""

from __future__ import annotations

import argparse
import os
import re
import sys
import tomllib
from dataclasses import dataclass
from pathlib import Path
from typing import Iterable, Iterator

_SKIP_DIRS = {
    ".git",
    ".idea",
    ".pytest_cache",
    ".ruff_cache",
    ".venv",
    ".vscode",
    "__pycache__",
    "ref",
    "target",
    "tmp",
    "vendor",
}

_RENDERER_CRATES = {"console", "crossterm", "indicatif", "termcolor"}
_PROGRESS_NAMES = re.compile(
    r"\b[A-Za-z_]*(?:callback|observer|progress|reporter)[A-Za-z_]*\b",
    re.IGNORECASE,
)
_CALLBACK_TRAIT = re.compile(r"\b(?:Fn|FnMut|FnOnce)\s*(?:\(|<)")
_RAW_PROGRESS_CHANNEL = re.compile(
    r"(?:flume\s*::\s*)?(?:Sender|Receiver)\s*<\s*"
    r"(?:[A-Za-z_][A-Za-z0-9_]*\s*::\s*)*ProgressUpdate\b"
)

_REMOVED_MODEL_NAMES = {
    "ApiError",
    "ArchiveVolumesReady",
    "DownloadExecInput",
    "FillArchiveVolumeGaps",
    "InstallArchive",
    "PlanArchiveExtraction",
    "PrepareFileRepair",
    "ReadArchiveRepairIndex",
    "RepairFileInput",
    "ReuseFileInput",
    "ReuseMethod",
    "SaveArchiveVolume",
    "TransferDownload",
    "TransferFileRepair",
    "VerifyCommittedBatch",
    "VfsPlanOutcome",
    "VfsUpdateOutcome",
}

_VAGUE_FILE_NAMES = {
    "archive_pipeline.rs",
    "bootstrap.rs",
    "characterization.rs",
    "complete.rs",
    "completion.rs",
    "executor.rs",
    "fixture.rs",
    "initial.rs",
    "inspection.rs",
    "model.rs",
    "models.rs",
    "operations.rs",
    "persistence.rs",
    "planning.rs",
    "space_model.rs",
    "transaction.rs",
    "workflow.rs",
}

_TASK_POOL_PATTERNS = (
    (re.compile(r"\bstd\s*::\s*thread\s*::\s*Builder\b"), "custom OS-thread pool"),
    (re.compile(r"\bthread\s*::\s*spawn\b"), "custom OS-thread pool"),
    (re.compile(r"\bCondvar\b"), "Condvar-backed queue"),
    (re.compile(r"\bfn\s+worker_loop\b"), "class-specific worker loop"),
    (re.compile(r"\bfn\s+dispatch_io\b"), "synchronous Dispatcher bridge"),
    (re.compile(r"\bExecutionClass\s*::\s*Network\b"), "thread-oriented network class"),
    (re.compile(r"\b(?:cpu|blocking)_workers\b"), "worker-count configuration"),
)

_BLOCKING_BOUNDARY = re.compile(
    r"\b(?:dispatch_blocking|run_blocking|spawn_blocking|with_blocking_io_buffer)\s*\("
)

_ASYNC_FN = re.compile(
    r"\basync\s+(?:(?:const|unsafe)\s+)*fn\s+[A-Za-z_][A-Za-z0-9_]*"
)
_ASYNC_BLOCK = re.compile(r"\basync\s+(?:move\s+)?\{")
_CFG_TEST_MOD = re.compile(
    r"#\s*\[\s*cfg\s*\(\s*test\s*\)\s*\]\s*"
    r"(?:pub(?:\s*\([^)]*\))?\s+)?mod\s+[A-Za-z_][A-Za-z0-9_]*\s*\{",
    re.DOTALL,
)


@dataclass(frozen=True)
class RustSource:
    path: Path
    text: str
    masked: str


@dataclass(frozen=True, order=True)
class Diagnostic:
    path: str
    line: int
    column: int
    code: str
    message: str
    hint: str = ""

    def render(self) -> str:
        location = f"{self.path}:{self.line}:{self.column}"
        suffix = f"\n  help: {self.hint}" if self.hint else ""
        return f"ERROR {self.code} {location}: {self.message}{suffix}"


class Checker:
    def __init__(self, root: Path) -> None:
        self.root = root.resolve()
        self.diagnostics: list[Diagnostic] = []

    def add(
        self,
        code: str,
        path: Path,
        text: str,
        index: int,
        message: str,
        hint: str = "",
    ) -> None:
        line = text.count("\n", 0, index) + 1
        last_newline = text.rfind("\n", 0, index)
        column = index - last_newline
        self.diagnostics.append(
            Diagnostic(
                path.relative_to(self.root).as_posix(),
                line,
                column,
                code,
                message,
                hint,
            )
        )

    def add_file(self, code: str, path: Path, message: str, hint: str = "") -> None:
        self.diagnostics.append(
            Diagnostic(
                path.relative_to(self.root).as_posix(),
                1,
                1,
                code,
                message,
                hint,
            )
        )

    def run(self) -> list[Diagnostic]:
        rust_files = [
            RustSource(path, text, _mask_rust(text))
            for path in _source_files(self.root, ".rs")
            for text in (_read(path),)
        ]
        self._check_crate_boundaries(rust_files)
        self._check_progress_api(rust_files)
        self._check_task_pool_model(rust_files)
        self._check_async_filesystem(rust_files)
        self._check_removed_models(rust_files)
        self._check_file_names()
        return sorted(set(self.diagnostics))

    def _check_crate_boundaries(self, rust_files: list[RustSource]) -> None:
        library_crates = {
            "griffr-core": self.root / "crates/griffr-core",
            "griffr-hypergryph-api": self.root / "crates/griffr-hypergryph-api",
            "griffr-yostar-api": self.root / "crates/griffr-yostar-api",
            "griffr-runtime": self.root / "crates/griffr-runtime",
        }
        forbidden_by_crate = {
            "griffr-core": {
                "compio",
                "cyper",
                "zip",
                "hdiffpatch-rs",
                "windows-sys",
                "libc",
                "griffr-hypergryph-api",
                "griffr-yostar-api",
                "griffr-runtime",
            },
            "griffr-hypergryph-api": {"griffr-yostar-api", "griffr-runtime"},
            "griffr-yostar-api": {"griffr-hypergryph-api", "griffr-runtime"},
        }

        for crate_name, crate_root in library_crates.items():
            manifest = crate_root / "Cargo.toml"
            if not manifest.is_file():
                continue
            try:
                data = tomllib.loads(manifest.read_text(encoding="utf-8"))
            except (OSError, UnicodeDecodeError, tomllib.TOMLDecodeError) as error:
                self.add_file("REP001", manifest, f"Cannot parse manifest: {error}")
                continue

            dependencies = _dependency_names(data)
            for dependency in sorted(dependencies & _RENDERER_CRATES):
                self.add_file(
                    "ARC001",
                    manifest,
                    f"{crate_name} depends on frontend renderer crate {dependency!r}",
                    "Keep terminal and GUI rendering dependencies in frontend crates.",
                )
            for dependency in sorted(dependencies & forbidden_by_crate.get(crate_name, set())):
                self.add_file(
                    "ARC002",
                    manifest,
                    f"{crate_name} depends on forbidden upper-layer crate {dependency!r}",
                    "Keep core provider-neutral and keep provider API crates independent from runtime and each other.",
                )

        library_roots = tuple((root / "src").resolve() for root in library_crates.values())
        for source in rust_files:
            path = source.path
            if not any(_is_relative_to(path, root) for root in library_roots):
                continue
            text = source.text
            masked = source.masked
            for crate in sorted(_RENDERER_CRATES):
                match = re.search(rf"\b{re.escape(crate)}\s*::", masked)
                if match:
                    self.add(
                        "ARC001",
                        path,
                        text,
                        match.start(),
                        f"Library crate references frontend renderer crate {crate!r}",
                        "Move terminal or GUI rendering to its frontend crate.",
                    )

    def _check_progress_api(self, rust_files: list[RustSource]) -> None:
        runtime_root = self.root / "crates/griffr-runtime/src"
        progress_file = (runtime_root / "progress.rs").resolve()
        for source in rust_files:
            path = source.path
            if not _is_relative_to(path, runtime_root):
                continue
            text = source.text
            masked = source.masked

            if path.resolve() != progress_file:
                for match in _RAW_PROGRESS_CHANNEL.finditer(masked):
                    self.add(
                        "PRG001",
                        path,
                        text,
                        match.start(),
                        "Raw ProgressUpdate channel escapes the canonical wrapper module",
                        "Use ProgressSender or ProgressReceiver.",
                    )

                for match in re.finditer(r"\bProgressLane\s*::\s*new\s*\(", masked):
                    self.add(
                        "PRG002",
                        path,
                        text,
                        match.start(),
                        "Progress lane is constructed outside the shared lane catalog",
                        "Add a named ProgressLane constant in griffr-runtime/src/progress.rs.",
                    )

            for start, signature in _public_function_signatures(masked):
                if not _CALLBACK_TRAIT.search(signature):
                    continue
                if not _PROGRESS_NAMES.search(signature):
                    continue
                self.add(
                    "PRG003",
                    path,
                    text,
                    start,
                    "Public griffr-runtime API exposes a progress callback",
                    "Expose ProgressSender or a frontend-neutral context object instead.",
                )

    def _check_task_pool_model(self, rust_files: list[RustSource]) -> None:
        task_pool_root = self.root / "crates/griffr-runtime/src/task_pool"
        for source in rust_files:
            path = source.path
            if not _is_relative_to(path, task_pool_root) or _is_test_path(path):
                continue
            text = source.text
            masked = source.masked
            for pattern, description in _TASK_POOL_PATTERNS:
                for match in pattern.finditer(masked):
                    self.add(
                        "DSP001",
                        path,
                        text,
                        match.start(),
                        f"Task pool reintroduces {description}",
                        "Use Dispatcher plus coordinator admission limits.",
                    )

    def _check_async_filesystem(self, rust_files: list[RustSource]) -> None:
        for source in rust_files:
            path = source.path
            if _is_test_path(path):
                continue
            text = source.text
            masked = source.masked
            test_ranges = _item_body_ranges(masked, _CFG_TEST_MOD)
            boundaries = _blocking_boundary_ranges(masked)
            aliases = _std_fs_aliases(masked)
            patterns = [(re.compile(r"\bstd\s*::\s*fs\s*::"), "std::fs")]
            patterns.extend(
                (re.compile(rf"\b{re.escape(alias)}\s*::"), f"{alias}::")
                for alias in sorted(aliases)
            )

            seen: set[int] = set()
            for body_start, body_end in _async_ranges(masked):
                if _inside_any(body_start, test_ranges):
                    continue
                for pattern, label in patterns:
                    for match in pattern.finditer(masked, body_start, body_end):
                        if match.start() in seen or _inside_any(match.start(), boundaries):
                            continue
                        seen.add(match.start())
                        self.add(
                            "AFS001",
                            path,
                            text,
                            match.start(),
                            f"Async code calls blocking filesystem API through {label}",
                            "Use compio::fs or move the complete blocking step into dispatch_blocking/run_blocking.",
                        )

    def _check_removed_models(self, rust_files: list[RustSource]) -> None:
        if not _REMOVED_MODEL_NAMES:
            return
        pattern = re.compile(
            r"\b(?:" + "|".join(map(re.escape, sorted(_REMOVED_MODEL_NAMES))) + r")\b"
        )
        for source in rust_files:
            path = source.path
            text = source.text
            masked = source.masked
            for match in pattern.finditer(masked):
                self.add(
                    "SSOT001",
                    path,
                    text,
                    match.start(),
                    f"Removed duplicate model {match.group(0)!r} is referenced",
                    "Use the canonical Task payload, TaskOutcome, PathReuseMethod, or Option-based API.",
                )

    def _check_file_names(self) -> None:
        for path in _all_files(self.root):
            if path.name.casefold() not in _VAGUE_FILE_NAMES:
                continue
            self.add_file(
                "NAM001",
                path,
                f"File name is too broad: {path.name}",
                "Name the concrete data, action, or result stored in the file.",
            )


def _read(path: Path) -> str:
    return path.read_text(encoding="utf-8")


def _is_relative_to(path: Path, parent: Path) -> bool:
    try:
        path.resolve().relative_to(parent.resolve())
        return True
    except ValueError:
        return False


def _is_test_path(path: Path) -> bool:
    normalized = path.as_posix()
    return (
        "/tests/" in normalized
        or normalized.endswith("/tests.rs")
        or normalized.endswith("/test.rs")
    )


def _all_files(root: Path) -> Iterator[Path]:
    for directory, names, files in os.walk(root, topdown=True, followlinks=False):
        names[:] = [name for name in names if name not in _SKIP_DIRS]
        base = Path(directory)
        for name in files:
            yield base / name


def _source_files(root: Path, suffix: str) -> Iterator[Path]:
    for path in _all_files(root):
        if path.suffix == suffix:
            yield path


def _dependency_names(data: dict[str, object]) -> set[str]:
    found: set[str] = set()

    def visit(value: object, key: str | None = None) -> None:
        if isinstance(value, dict):
            if key in {"dependencies", "dev-dependencies", "build-dependencies"}:
                found.update(str(name) for name in value)
            for child_key, child in value.items():
                visit(child, str(child_key))
        elif isinstance(value, list):
            for child in value:
                visit(child, key)

    visit(data)
    return found


def _mask_rust(text: str) -> str:
    """Replace comments and literals with spaces while preserving offsets."""
    chars = list(text)
    length = len(text)
    index = 0

    def blank(start: int, end: int) -> None:
        for position in range(start, end):
            if chars[position] != "\n":
                chars[position] = " "

    while index < length:
        if text.startswith("//", index):
            end = text.find("\n", index)
            if end < 0:
                end = length
            blank(index, end)
            index = end
            continue

        if text.startswith("/*", index):
            depth = 1
            end = index + 2
            while end < length and depth:
                if text.startswith("/*", end):
                    depth += 1
                    end += 2
                elif text.startswith("*/", end):
                    depth -= 1
                    end += 2
                else:
                    end += 1
            blank(index, end)
            index = end
            continue

        raw = re.match(r"(?:br|rb|r)(?P<hashes>#{0,255})\"", text[index:])
        if raw:
            hashes = raw.group("hashes")
            terminator = '"' + hashes
            start_content = index + raw.end()
            end = text.find(terminator, start_content)
            end = length if end < 0 else end + len(terminator)
            blank(index, end)
            index = end
            continue

        prefix_len = 0
        if text.startswith(('b"', 'c"'), index):
            prefix_len = 1
        if text[index + prefix_len : index + prefix_len + 1] == '"':
            end = index + prefix_len + 1
            escaped = False
            while end < length:
                char = text[end]
                end += 1
                if escaped:
                    escaped = False
                elif char == "\\":
                    escaped = True
                elif char == '"':
                    break
            blank(index, end)
            index = end
            continue

        # Mask character literals, but keep lifetimes such as 'a intact.
        if text[index] == "'":
            end = index + 1
            escaped = False
            while end < min(length, index + 12) and text[end] != "\n":
                char = text[end]
                end += 1
                if escaped:
                    escaped = False
                elif char == "\\":
                    escaped = True
                elif char == "'":
                    blank(index, end)
                    index = end
                    break
            else:
                index += 1
            continue

        index += 1

    return "".join(chars)


def _matching_brace(text: str, opening: int) -> int | None:
    depth = 0
    for index in range(opening, len(text)):
        char = text[index]
        if char == "{":
            depth += 1
        elif char == "}":
            depth -= 1
            if depth == 0:
                return index + 1
    return None


def _item_body_ranges(text: str, pattern: re.Pattern[str]) -> list[tuple[int, int]]:
    ranges: list[tuple[int, int]] = []
    for match in pattern.finditer(text):
        opening = text.find("{", match.start(), match.end())
        if opening < 0:
            continue
        end = _matching_brace(text, opening)
        if end is not None:
            ranges.append((opening, end))
    return ranges


def _async_ranges(text: str) -> list[tuple[int, int]]:
    ranges: set[tuple[int, int]] = set()
    for match in _ASYNC_FN.finditer(text):
        opening = text.find("{", match.end())
        terminator = text.find(";", match.end())
        if opening < 0 or 0 <= terminator < opening:
            continue
        end = _matching_brace(text, opening)
        if end is not None:
            ranges.add((opening, end))
    for match in _ASYNC_BLOCK.finditer(text):
        opening = text.find("{", match.start(), match.end())
        if opening < 0:
            continue
        end = _matching_brace(text, opening)
        if end is not None:
            ranges.add((opening, end))
    return sorted(ranges)


def _matching_parenthesis(text: str, opening: int) -> int | None:
    depth = 0
    for index in range(opening, len(text)):
        char = text[index]
        if char == "(":
            depth += 1
        elif char == ")":
            depth -= 1
            if depth == 0:
                return index + 1
    return None


def _blocking_boundary_ranges(text: str) -> list[tuple[int, int]]:
    ranges: list[tuple[int, int]] = []
    for match in _BLOCKING_BOUNDARY.finditer(text):
        opening = text.rfind("(", match.start(), match.end())
        if opening < 0:
            continue
        end = _matching_parenthesis(text, opening)
        if end is not None:
            ranges.append((opening, end))
    return ranges


def _inside_any(index: int, ranges: Iterable[tuple[int, int]]) -> bool:
    return any(start <= index < end for start, end in ranges)


def _std_fs_aliases(text: str) -> set[str]:
    aliases: set[str] = set()
    for match in re.finditer(
        r"\buse\s+std\s*::\s*fs(?:\s+as\s+([A-Za-z_][A-Za-z0-9_]*))?\s*;",
        text,
    ):
        aliases.add(match.group(1) or "fs")
    for match in re.finditer(r"\buse\s+std\s*::\s*\{([^{};]+)\}\s*;", text):
        for item in match.group(1).split(","):
            item = item.strip()
            alias_match = re.fullmatch(
                r"fs(?:\s+as\s+([A-Za-z_][A-Za-z0-9_]*))?", item
            )
            if alias_match:
                aliases.add(alias_match.group(1) or "fs")
    return aliases


def _public_function_signatures(text: str) -> Iterator[tuple[int, str]]:
    for match in re.finditer(r"\bpub\b", text):
        cursor = match.end()
        while cursor < len(text) and text[cursor].isspace():
            cursor += 1
        if cursor < len(text) and text[cursor] == "(":
            continue

        boundary = len(text)
        for token in ("{", ";"):
            found = text.find(token, cursor)
            if found >= 0:
                boundary = min(boundary, found)
        signature = text[match.start() : boundary]
        if re.search(r"\bfn\s+[A-Za-z_][A-Za-z0-9_]*", signature):
            yield match.start(), signature


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(
        description="Check Griffr-specific repository policies without duplicating Rust tools."
    )
    parser.add_argument("root", nargs="?", default=".", type=Path)
    return parser


def main(argv: list[str] | None = None) -> int:
    args = build_parser().parse_args(argv)
    root = args.root.resolve()
    if not (root / "Cargo.toml").is_file():
        print(f"error: no Cargo.toml at repository root {root}", file=sys.stderr)
        return 2

    diagnostics = Checker(root).run()
    if diagnostics:
        print("\n\n".join(item.render() for item in diagnostics))
        print(f"\n{len(diagnostics)} repository policy violation(s)")
        return 1

    print("repository policy checks passed")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
