from __future__ import annotations

import re
from pathlib import Path
from typing import TYPE_CHECKING

if TYPE_CHECKING:
    from .checker import Checker

_TEXT_SUFFIXES = {
    ".md",
    ".py",
    ".ps1",
    ".rs",
    ".toml",
    ".txt",
    ".yml",
    ".yaml",
}

# Build restricted terms in parts so this checker does not report its own source.
_TERM_RULES = (
    (
        re.compile(r"\b" + "materiali" + r"[sz](?:e|es|ed|ing|ation)\b", re.I),
        "Use write, create, copy, save, or build.",
    ),
    (
        re.compile(r"\b" + "characteri" + r"[sz](?:e|es|ed|ing|ation)\b", re.I),
        "Use check, describe, measure, or identify the format.",
    ),
    (
        re.compile(r"\b" + "boot" + r"strap(?:ped|ping)?\b", re.I),
        "Use set up, initialize, load, or start.",
    ),
    (
        re.compile(r"\b" + "pre" + r"flight\b", re.I),
        "Use check, scan, or prepare.",
    ),
    (
        re.compile(r"\b" + "pipe" + r"lines?\b", re.I),
        "Name the concrete steps, flow, queue, or data path.",
    ),
    (
        re.compile(r"\b" + "topolog" + r"(?:y|ies)\b", re.I),
        "Use layout, graph, links, or dependency order.",
    ),
    (
        re.compile(
            r"\b" + "finali" + r"[sz](?:ation|ed|ing)\b",
            re.I,
        ),
        "Use finish, save, close, or commit.",
    ),
    (
        re.compile(r"\b" + "utili" + r"[sz](?:e|es|ed|ing|ation)\b", re.I),
        "Use use.",
    ),
    (
        re.compile(r"\b" + "commenc" + r"(?:e|es|ed|ing|ement)\b", re.I),
        "Use start or begin.",
    ),
    (
        re.compile(r"\b" + "in order" + " to" + r"\b", re.I),
        "Use to.",
    ),
    (
        re.compile(r"\b" + "prior" + " to" + r"\b", re.I),
        "Use before.",
    ),
    (
        re.compile(r"\b" + "operat" + r"(?:ion|ions|ional)\b", re.I),
        "Use task, step, work, action, or state the direct verb.",
    ),
    (
        re.compile(r"\b" + "execut" + r"(?:e|es|ed|ing|ion|ions|able)\b", re.I),
        "Use run, start, runnable, or state the direct verb.",
    ),
    (
        re.compile(r"\b" + "perform" + r"(?:s|ed|ing)?\b", re.I),
        "Use direct verb such as check, save, write, do, or run.",
    ),
    (
        re.compile(r"\b" + "obtain" + r"(?:s|ed|ing)?\b", re.I),
        "Use get, read, or receive.",
    ),
    (
        re.compile(r"\b" + "terminat" + r"(?:e|es|ed|ing|ion|ions)\b", re.I),
        "Use stop, end, or cancel.",
    ),
)

_VAGUE_FILE_NAMES = {
    "archive_" + "pipe" + "line.rs",
    "boot" + "strap.rs",
    "character" + "ization.rs",
    "complete.rs",
    "inspection.rs",
    "model.rs",
    "models.rs",
    "operations.rs",
    "persistence.rs",
    "planning.rs",
    "space_model.rs",
    "workflow.rs",
}

_EXACT_EXCEPTIONS = {
    "docs/WORDING.md",
}


def _read_text(path: Path) -> str | None:
    try:
        return path.read_text(encoding="utf-8")
    except (OSError, UnicodeDecodeError):
        return None


def run(checker: Checker) -> None:
    for path in checker.root.rglob("*"):
        if not path.is_file() or checker.excluded(path):
            continue

        rel = path.relative_to(checker.root).as_posix()
        if rel in _EXACT_EXCEPTIONS:
            continue

        if path.name.casefold() in _VAGUE_FILE_NAMES:
            checker.add(
                "WRD002",
                "warning",
                f"File name is too broad: {path.name}",
                path=path,
                confidence="definite",
                hint="Name the data, action, or result that the file contains.",
            )

        if path.suffix.casefold() not in _TEXT_SUFFIXES:
            continue
        text = _read_text(path)
        if text is None:
            continue
        for line_number, line in enumerate(text.splitlines(), start=1):
            for pattern, hint in _TERM_RULES:
                match = pattern.search(line)
                if match is None:
                    continue
                checker.add(
                    "WRD001",
                    "warning",
                    "Use direct project wording instead of an abstract term",
                    path=path,
                    confidence="definite",
                    hint=hint,
                    evidence=(
                        f"line {line_number}: {line.strip()[:160]}",
                        f"matched text: {match.group(0)}",
                    ),
                )
