from __future__ import annotations

import dataclasses
import json
import textwrap
from collections import Counter
from pathlib import Path

from .checker import Checker
from .models import Diagnostic


def render_text(checker: Checker, *, verbose_tools: bool = False) -> str:
    summary = checker.summary()
    counts = summary["diagnostics"]
    confidence = summary["confidence"]
    lines = [
        "Rust repository aggressive structural check",
        "=" * 43,
        f"Root: {summary['root']}",
        (
            f"Parsed {summary['rust_files_parsed']} Rust files across "
            f"{summary['packages']} packages / {summary['targets']} targets."
        ),
        (
            f"Diagnostics: {counts.get('error', 0)} errors, "
            f"{counts.get('warning', 0)} warnings, {counts.get('note', 0)} notes."
        ),
        (
            f"Confidence: {confidence.get('definite', 0)} definite, "
            f"{confidence.get('probable', 0)} probable, "
            f"{confidence.get('speculative', 0)} speculative."
        ),
        (
            f"Fixes: {summary['applied_fixes']} applied, "
            f"{summary['fixable_edits']} remaining fixable edits."
        ),
    ]
    if checker.baseline:
        lines.append(f"Baseline diff entries: {len(checker.diff_entries)}")
    lines.append("")

    if checker.applied_fixes:
        lines.extend(["Applied fixes", "-------------"])
        for fix in checker.applied_fixes:
            lines.append(f"{fix.code:8} {fix.path}: {fix.description}")
        lines.append("")
    if checker.skipped_fix_conflicts:
        lines.extend(["Skipped fix conflicts", "---------------------"])
        lines.extend(checker.skipped_fix_conflicts)
        lines.append("")

    for diagnostic in sorted(checker.diagnostics, key=Diagnostic.sort_key):
        location = diagnostic.path
        if diagnostic.line:
            location += f":{diagnostic.line}:{diagnostic.column}"
        lines.append(
            f"{diagnostic.severity.upper()} {diagnostic.code} [{diagnostic.confidence}] "
            f"{location}: {diagnostic.message}".rstrip()
        )
        for evidence in diagnostic.evidence:
            lines.append(f"  evidence: {evidence}")
        if diagnostic.hint:
            lines.append(f"  help: {diagnostic.hint}")

    if checker.tool_results:
        lines.extend(["", "External tools", "--------------"])
        for result in checker.tool_results:
            status = (
                "unavailable"
                if not result.available
                else (
                    "ok" if result.returncode == 0 else f"failed ({result.returncode})"
                )
            )
            lines.append(f"{result.label}: {status} — {' '.join(result.command)}")
            if verbose_tools and result.output:
                lines.append(textwrap.indent(result.output.rstrip(), "    "))

    if checker.diff_entries:
        lines.extend(["", "Baseline comparison", "-------------------"])
        diff_counts = Counter(entry.status for entry in checker.diff_entries)
        lines.append(
            ", ".join(f"{key}={value}" for key, value in sorted(diff_counts.items()))
        )
        for entry in checker.diff_entries[:200]:
            suffix = f" ({entry.detail})" if entry.detail else ""
            lines.append(f"{entry.status:8} {entry.path}{suffix}")
        if len(checker.diff_entries) > 200:
            lines.append(f"... {len(checker.diff_entries) - 200} more entries omitted")

    return "\n".join(lines) + "\n"


def write_json(checker: Checker, path: Path) -> None:
    payload = {
        "summary": checker.summary(),
        "diagnostics": [
            diagnostic.as_dict()
            for diagnostic in sorted(checker.diagnostics, key=Diagnostic.sort_key)
        ],
        "applied_fixes": [fix.as_dict() for fix in checker.applied_fixes],
        "skipped_fix_conflicts": checker.skipped_fix_conflicts,
        "baseline_diff": [dataclasses.asdict(entry) for entry in checker.diff_entries],
    }
    path.write_text(json.dumps(payload, indent=2, ensure_ascii=False) + "\n", "utf-8")


def write_markdown(checker: Checker, path: Path) -> None:
    summary = checker.summary()
    counts = summary["diagnostics"]
    confidence = summary["confidence"]
    lines = [
        "# Rust aggressive static-analysis report",
        "",
        f"- Root: `{summary['root']}`",
        f"- Packages/targets: {summary['packages']} / {summary['targets']}",
        f"- Rust files parsed: {summary['rust_files_parsed']}",
        (
            f"- Diagnostics: **{counts.get('error', 0)} errors**, "
            f"**{counts.get('warning', 0)} warnings**, {counts.get('note', 0)} notes"
        ),
        (
            f"- Confidence: {confidence.get('definite', 0)} definite, "
            f"{confidence.get('probable', 0)} probable, "
            f"{confidence.get('speculative', 0)} speculative"
        ),
        (
            f"- Fixes: {summary['applied_fixes']} applied, "
            f"{summary['fixable_edits']} remaining fixable edits"
        ),
        "",
        "## Applied fixes",
        "",
    ]
    if not checker.applied_fixes:
        lines.append("No fixes were applied.")
    for fix in checker.applied_fixes:
        lines.append(f"- **`{fix.code}`** `{fix.path}` — {fix.description}")

    lines.extend(
        [
            "",
            "## Diagnostics",
            "",
        ]
    )
    if not checker.diagnostics:
        lines.append("No diagnostics.")
    for diagnostic in sorted(checker.diagnostics, key=Diagnostic.sort_key):
        location = diagnostic.path + (
            f":{diagnostic.line}:{diagnostic.column}" if diagnostic.line else ""
        )
        lines.append(
            f"- **{diagnostic.severity.upper()} `{diagnostic.code}`** "
            f"({diagnostic.confidence}) `{location}` — {diagnostic.message}"
        )
        for evidence in diagnostic.evidence:
            lines.append(f"  - Evidence: {evidence}")
        if diagnostic.hint:
            lines.append(f"  - Help: {diagnostic.hint}")

    lines.extend(["", "## External tools", ""])
    if not checker.tool_results:
        lines.append("External tools were disabled.")
    for result in checker.tool_results:
        status = (
            "unavailable"
            if not result.available
            else ("ok" if result.returncode == 0 else f"failed ({result.returncode})")
        )
        lines.append(f"- **{result.label}**: {status} — `{' '.join(result.command)}`")

    if checker.diff_entries:
        lines.extend(["", "## Baseline comparison", ""])
        for entry in checker.diff_entries:
            lines.append(f"- `{entry.status}` `{entry.path}` — {entry.detail}")
    path.write_text("\n".join(lines) + "\n", "utf-8")
