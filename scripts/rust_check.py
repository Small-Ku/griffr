#!/usr/bin/env python3
# /// script
# requires-python = ">=3.11"
# dependencies = [
#     "tree-sitter==0.23.2",
#     "tree-sitter-rust==0.23.2",
# ]
# ///
"""Aggressive Rust repository checker backed by tree-sitter-rust.

Python-only mode maximizes review coverage while labeling every diagnostic as
definite, probable, or speculative. Cargo, rustfmt, and Clippy remain the final
authorities when available.
"""

from __future__ import annotations

import argparse
import sys
from collections import Counter
from pathlib import Path
from typing import Sequence

if sys.version_info < (3, 11):
    raise SystemExit("error: This script requires Python 3.11+.")

try:
    from rust_check_lib import Checker, render_text, write_json, write_markdown
except Exception as exc:  # pragma: no cover - setup/import failure
    print(
        "error: compatible tree-sitter dependencies are required; run this file through uv\n"
        "  uv run scripts/rust_check.py . --run-tools never",
        file=sys.stderr,
    )
    print(f"detail: {exc}", file=sys.stderr)
    raise SystemExit(2) from exc


def build_arg_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(
        description="Aggressive tree-sitter Rust static analysis with confidence-labelled diagnostics and optional Cargo delegation.",
        formatter_class=argparse.ArgumentDefaultsHelpFormatter,
    )
    parser.add_argument(
        "root", nargs="?", default=".", type=Path, help="Repository/workspace root"
    )
    parser.add_argument(
        "--baseline", type=Path, help="Directory or ZIP to compare against"
    )
    parser.add_argument(
        "--run-tools",
        choices=("auto", "never", "required"),
        default="auto",
        help="Run cargo fmt/check/clippy when available",
    )
    parser.add_argument("--cargo-test", action="store_true", help="Also run cargo test")
    parser.add_argument(
        "--exclude",
        action="append",
        default=[],
        help="Additional glob to exclude; repeatable",
    )
    parser.add_argument(
        "--max-width",
        type=int,
        default=100,
        help="Fallback width used for speculative Python-only formatting notes",
    )
    parser.add_argument(
        "--json", type=Path, dest="json_path", help="Write machine-readable JSON report"
    )
    parser.add_argument("--markdown", type=Path, help="Write Markdown report")
    parser.add_argument(
        "--verbose-tools", action="store_true", help="Print captured Cargo output"
    )
    parser.add_argument(
        "--max-tool-output",
        type=int,
        default=4 * 1024 * 1024,
        metavar="BYTES",
        help="Maximum captured output retained per external tool",
    )
    parser.add_argument(
        "--min-confidence",
        choices=("speculative", "probable", "definite"),
        default="speculative",
        help="Lowest-confidence diagnostics to include; speculative shows everything",
    )
    parser.add_argument(
        "--heuristics",
        action="store_true",
        help=argparse.SUPPRESS,
    )
    parser.add_argument(
        "--fix",
        action="store_true",
        help="Apply conservative AST-backed fixes, then re-run analysis on the changed files",
    )
    parser.add_argument(
        "--fail-on",
        choices=("error", "warning", "note", "never"),
        default="warning",
        help="Exit non-zero at this severity or above",
    )
    return parser


def main(argv: Sequence[str] | None = None) -> int:
    args = build_arg_parser().parse_args(argv)
    root = args.root.resolve()
    if not root.is_dir():
        print(f"error: root is not a directory: {root}", file=sys.stderr)
        return 2

    checker = Checker(
        root,
        baseline_path=args.baseline,
        run_tools=args.run_tools,
        include_tests=args.cargo_test,
        excludes=args.exclude,
        max_tool_output=args.max_tool_output,
        max_width=args.max_width,
        min_confidence=args.min_confidence,
        fix=args.fix,
    )
    try:
        checker.run()
    except (OSError, ValueError) as exc:
        print(f"error: {exc}", file=sys.stderr)
        return 2

    sys.stdout.write(render_text(checker, verbose_tools=args.verbose_tools))
    if args.json_path:
        write_json(checker, args.json_path)
    if args.markdown:
        write_markdown(checker, args.markdown)

    counts = Counter(diagnostic.severity for diagnostic in checker.diagnostics)
    if args.fail_on == "never":
        return 0
    if counts.get("error", 0):
        return 1
    if args.fail_on in {"warning", "note"} and counts.get("warning", 0):
        return 1
    if args.fail_on == "note" and counts.get("note", 0):
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
