from __future__ import annotations

import fnmatch
import gc
import re
import tomllib
from collections import Counter
from pathlib import Path
from typing import Any, Sequence

from tree_sitter import Node

from . import architecture, baseline, lints, module_graph, name_resolution, tools, workspace
from .models import (
    CONFIDENCE_ORDER,
    CrateTarget,
    Diagnostic,
    DiffEntry,
    Package,
    SourceFile,
    ToolResult,
)
from .parsing import parse


class Checker:
    def __init__(
        self,
        root: Path,
        *,
        baseline_path: Path | None = None,
        run_tools: str = "auto",
        include_tests: bool = False,
        excludes: Sequence[str] = (),
        max_tool_output: int = 4 * 1024 * 1024,
        max_width: int = 100,
        min_confidence: str = "speculative",
    ) -> None:
        self.root = root.resolve()
        self.baseline = baseline_path.resolve() if baseline_path else None
        self.run_tools = run_tools
        self.include_tests = include_tests
        self.excludes = tuple(excludes)
        self.max_tool_output = max(64 * 1024, max_tool_output)
        self.max_width = max(40, max_width)
        self.min_confidence = min_confidence

        self.root_manifest_path = self.root / "Cargo.toml"
        self.root_manifest: dict[str, Any] | None = None
        self.packages: list[Package] = []
        self.targets: list[CrateTarget] = []
        self.sources: dict[Path, SourceFile] = {}
        self.diagnostics: list[Diagnostic] = []
        self.tool_results: list[ToolResult] = []
        self.diff_entries: list[DiffEntry] = []
        self.parsed_file_count = 0
        self._diagnostic_keys: set[tuple[Any, ...]] = set()

    def add(
        self,
        code: str,
        severity: str,
        message: str,
        *,
        path: Path | str | None = None,
        node: Node | None = None,
        source: SourceFile | None = None,
        hint: str = "",
        confidence: str = "definite",
        evidence: Sequence[str] = (),
    ) -> None:
        threshold = CONFIDENCE_ORDER.get(self.min_confidence, 2)
        if CONFIDENCE_ORDER.get(confidence, 2) > threshold:
            return
        rel = ""
        line = column = 0
        if path is not None:
            if isinstance(path, Path):
                try:
                    rel = path.resolve().relative_to(self.root).as_posix()
                except (OSError, ValueError):
                    rel = str(path)
            else:
                rel = path
        if node is not None and source is not None:
            line, column = source.location(node)
            rel = rel or source.rel
        normalized_evidence = tuple(item for item in evidence if item)
        diagnostic = Diagnostic(
            code,
            severity,
            message,
            rel,
            line,
            column,
            hint,
            confidence,
            normalized_evidence,
        )
        key = (
            diagnostic.code,
            diagnostic.severity,
            diagnostic.message,
            diagnostic.path,
            diagnostic.line,
            diagnostic.column,
            diagnostic.confidence,
        )
        if key in self._diagnostic_keys:
            return
        self._diagnostic_keys.add(key)
        self.diagnostics.append(diagnostic)

    def excluded(self, path: Path) -> bool:
        try:
            rel = path.resolve().relative_to(self.root).as_posix()
        except (OSError, ValueError):
            rel = path.as_posix()
        defaults = (
            ".git/**",
            "target/**",
            "vendor/**",
            "node_modules/**",
            ".venv/**",
            ".ruff_cache/**",
            ".pytest_cache/**",
            ".mypy_cache/**",
            "**/__pycache__/**",
        )
        return any(
            fnmatch.fnmatch(rel, pattern) for pattern in (*defaults, *self.excludes)
        )

    def load_toml(self, path: Path) -> dict[str, Any] | None:
        try:
            return tomllib.loads(path.read_text("utf-8"))
        except Exception as exc:
            self.add("MAN001", "error", f"Cannot parse TOML: {exc}", path=path)
            return None

    def source_for(self, path: Path) -> SourceFile | None:
        path = path.resolve()
        if path in self.sources:
            return self.sources[path]
        if not path.is_file():
            return None
        try:
            data = path.read_bytes()
            tree = parse(data)
        except Exception as exc:
            self.add("IO001", "error", f"Cannot parse source file: {exc}", path=path)
            return None
        try:
            rel = path.relative_to(self.root).as_posix()
        except ValueError:
            rel = str(path)
        source = SourceFile(path, rel, data, tree)
        self.sources[path] = source
        self._check_parse_errors(source)
        self._check_text_hygiene(source)
        return source

    def _check_parse_errors(self, source: SourceFile) -> None:
        root = source.tree.root_node
        if not root.has_error:
            return
        stack = [root]
        emitted = 0
        while stack and emitted < 30:
            node = stack.pop()
            if node.type == "ERROR" or node.is_missing:
                kind = "missing token" if node.is_missing else "syntax error"
                snippet = source.text(node)[:80].replace("\n", "\\n")
                self.add(
                    "SYN001",
                    "error",
                    f"Rust {kind}: {snippet!r}",
                    source=source,
                    node=node,
                    evidence=("tree-sitter marked this node as ERROR or missing",),
                )
                emitted += 1
            stack.extend(reversed(node.children))

    @staticmethod
    def _line_is_unbreakable(line: str) -> bool:
        stripped = line.strip()
        return bool(
            re.fullmatch(r"(?://|///|//!|#\s*\[doc\s*=\s*)?\s*https?://\S+", stripped)
            or re.fullmatch(r"[A-Za-z0-9+/=_-]{80,}", stripped)
            or (stripped.startswith('r#"') and stripped.endswith('"#'))
        )

    def _check_text_hygiene(self, source: SourceFile) -> None:
        data = source.data
        if b"\r\n" in data and re.search(rb"(?<!\r)\n", data):
            self.add(
                "FMT001",
                "warning",
                "File mixes CRLF and LF line endings",
                path=source.path,
                confidence="probable",
                hint="Normalize the file with rustfmt or the repository's configured newline style.",
            )
        if data and not data.endswith(b"\n"):
            self.add("FMT002", "warning", "File has no final newline", path=source.path)
        protected_lines: set[int] = set()
        stack = [source.tree.root_node]
        protected_types = {
            "line_comment",
            "block_comment",
            "string_literal",
            "raw_string_literal",
            "token_tree",
        }
        while stack:
            node = stack.pop()
            if node.type in protected_types:
                protected_lines.update(
                    range(node.start_point.row + 1, node.end_point.row + 2)
                )
                continue
            stack.extend(node.named_children)

        blank_run = 0
        for line_number, line in enumerate(
            data.decode("utf-8", "replace").splitlines(), start=1
        ):
            stripped = line.rstrip(" \t")
            if stripped != line:
                self.add(
                    "FMT003",
                    "warning",
                    "Trailing whitespace",
                    path=source.path,
                    confidence="definite",
                    evidence=(f"line {line_number}, column {len(stripped) + 1}",),
                )
            if line.startswith("\t"):
                self.add(
                    "FMT004",
                    "note",
                    "Tab indentation is unlikely to match rustfmt output",
                    path=source.path,
                    confidence="probable",
                    evidence=(f"line {line_number}",),
                )
            if line.strip():
                blank_run = 0
            else:
                blank_run += 1
                if blank_run == 3:
                    self.add(
                        "FMT005",
                        "note",
                        "More than two consecutive blank lines",
                        path=source.path,
                        confidence="probable",
                        evidence=(f"run reaches line {line_number}",),
                    )
            if (
                len(line) > self.max_width
                and line_number not in protected_lines
                and not self._line_is_unbreakable(line)
                and not line.lstrip().startswith("//")
            ):
                self.add(
                    "FMT006",
                    "note",
                    f"Line exceeds configured width {self.max_width} ({len(line)} columns)",
                    path=source.path,
                    confidence="speculative",
                    hint="rustfmt is authoritative and may intentionally retain this line.",
                    evidence=(f"line {line_number}",),
                )

    def free_analysis_memory(self) -> None:
        self.parsed_file_count = max(self.parsed_file_count, len(self.sources))
        for target in self.targets:
            target.modules.clear()
            target.reachable_files.clear()
            target.dynamic_includes.clear()
        self.sources.clear()
        gc.collect()

    def run(self) -> None:
        baseline.compare(self)
        workspace.discover(self)
        module_graph.build(self)
        resolver = name_resolution.analyze(self)
        lints.run(self, resolver)
        architecture.run(self)
        architecture.run(self)
        self.parsed_file_count = len(self.sources)
        self.free_analysis_memory()
        tools.run(self)

    def summary(self) -> dict[str, Any]:
        counts = Counter(diagnostic.severity for diagnostic in self.diagnostics)
        confidence = Counter(diagnostic.confidence for diagnostic in self.diagnostics)
        return {
            "root": str(self.root),
            "packages": len(self.packages),
            "targets": len(self.targets),
            "rust_files_parsed": self.parsed_file_count or len(self.sources),
            "diagnostics": dict(counts),
            "confidence": dict(confidence),
            "baseline_diff_entries": len(self.diff_entries),
            "tools": [
                {
                    "label": result.label,
                    "command": result.command,
                    "available": result.available,
                    "returncode": result.returncode,
                    "output": result.output,
                }
                for result in self.tool_results
            ],
        }
