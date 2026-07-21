from __future__ import annotations

import fnmatch
import shutil
import tempfile
import zipfile
from pathlib import Path, PurePosixPath
from typing import Any, Protocol

from .records import DiffEntry
from .parsing import leaf_tokens


class BaselineHost(Protocol):
    root: Path
    baseline: Path | None
    excludes: tuple[str, ...]
    diff_entries: list[DiffEntry]

    def add(self, code: str, severity: str, message: str, **kwargs: Any) -> None: ...


def compare(host: BaselineHost) -> None:
    if not host.baseline:
        return
    with BaselineTree(host.baseline) as baseline_root:
        baseline_files = _file_map(baseline_root, host.excludes)
        current_files = _file_map(host.root, host.excludes)
        for rel in sorted(baseline_files.keys() | current_files.keys()):
            if rel not in current_files:
                host.diff_entries.append(
                    DiffEntry(rel, "missing", "present only in baseline")
                )
                host.add(
                    "PIPE001",
                    "warning",
                    "File present in baseline is missing from candidate tree",
                    path=rel,
                    hint="Verify that the deletion is intentional.",
                )
                continue
            if rel not in baseline_files:
                host.diff_entries.append(
                    DiffEntry(rel, "added", "present only in candidate")
                )
                host.add(
                    "PIPE002",
                    "warning" if rel.endswith(".rs") else "note",
                    "File exists only in candidate tree",
                    path=rel,
                )
                continue
            old = baseline_files[rel].read_bytes()
            new = current_files[rel].read_bytes()
            if old == new:
                continue
            equivalent = None
            detail = "content changed"
            if rel.endswith(".rs"):
                equivalent = leaf_tokens(old) == leaf_tokens(new)
                detail = (
                    "format/comment-only Rust change"
                    if equivalent
                    else "Rust token stream changed"
                )
            host.diff_entries.append(DiffEntry(rel, "changed", detail, equivalent))


class BaselineTree:
    def __init__(self, path: Path):
        self.path = path.resolve()
        self.temp: tempfile.TemporaryDirectory[str] | None = None

    def __enter__(self) -> Path:
        if self.path.is_dir():
            return self.path
        if self.path.suffix.lower() != ".zip":
            raise ValueError(f"baseline must be a directory or ZIP: {self.path}")

        self.temp = tempfile.TemporaryDirectory(prefix="rust_check_baseline_")
        root = Path(self.temp.name).resolve()
        with zipfile.ZipFile(self.path) as archive:
            for info in archive.infolist():
                member = PurePosixPath(info.filename)
                if member.is_absolute() or ".." in member.parts:
                    raise ValueError(f"unsafe ZIP member path: {info.filename!r}")
                mode = (info.external_attr >> 16) & 0o170000
                if mode == 0o120000:
                    raise ValueError(
                        f"baseline ZIP contains symlink: {info.filename!r}"
                    )
                destination = (root / Path(*member.parts)).resolve()
                if root != destination and root not in destination.parents:
                    raise ValueError(f"unsafe ZIP member path: {info.filename!r}")
                if info.is_dir():
                    destination.mkdir(parents=True, exist_ok=True)
                    continue
                destination.parent.mkdir(parents=True, exist_ok=True)
                with archive.open(info) as source, destination.open("wb") as target:
                    shutil.copyfileobj(source, target, length=1024 * 1024)

        entries = [entry for entry in root.iterdir() if entry.name != "__MACOSX"]
        if (
            len(entries) == 1
            and entries[0].is_dir()
            and not (root / "Cargo.toml").exists()
        ):
            return entries[0]
        return root

    def __exit__(self, *_args: object) -> None:
        if self.temp:
            self.temp.cleanup()


def _file_map(root: Path, excludes: tuple[str, ...]) -> dict[str, Path]:
    out: dict[str, Path] = {}
    patterns = (
        ".git/**",
        "target/**",
        "vendor/**",
        ".venv/**",
        ".ruff_cache/**",
        ".pytest_cache/**",
        ".mypy_cache/**",
        "**/__pycache__/**",
        *excludes,
    )
    for path in root.rglob("*"):
        if not path.is_file():
            continue
        rel = path.relative_to(root).as_posix()
        if any(fnmatch.fnmatch(rel, pattern) for pattern in patterns):
            continue
        out[rel] = path
    return out
