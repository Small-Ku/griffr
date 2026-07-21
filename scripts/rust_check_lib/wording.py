from __future__ import annotations

import re
from dataclasses import dataclass
from pathlib import Path
from typing import TYPE_CHECKING, Iterable

from tree_sitter import Node

from .parsing import walk_named
from .records import SourceFile

if TYPE_CHECKING:
    from .checker import Checker
    from .name_resolution import NameResolver

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


@dataclass(frozen=True)
class _TermRule:
    pattern: re.Pattern[str]
    hint: str


_TERM_RULES = (
    _TermRule(
        re.compile(r"materiali[sz](?:e|es|ed|ing|ation|ations|er|ers)", re.I),
        "Use write, create, copy, save, or build.",
    ),
    _TermRule(
        re.compile(r"characteri[sz](?:e|es|ed|ing|ation|ations|er|ers)", re.I),
        "Use check, describe, measure, or identify the format.",
    ),
    _TermRule(
        re.compile(r"bootstrap(?:s|ped|ping)?", re.I),
        "Use set up, load, or start.",
    ),
    _TermRule(
        re.compile(r"preflight(?:s)?", re.I),
        "Use check, scan, or prepare.",
    ),
    _TermRule(
        re.compile(r"pipeline(?:s)?", re.I),
        "Name the concrete steps, flow, queue, or data path.",
    ),
    _TermRule(
        re.compile(r"topolog(?:y|ies|ical|ically)", re.I),
        "Use layout, graph, links, or dependency order.",
    ),
    _TermRule(
        re.compile(r"finali[sz](?:e|es|ed|ing|ation|ations|er|ers)", re.I),
        "Use finish, save, close, or commit.",
    ),
    _TermRule(
        re.compile(r"utili[sz](?:e|es|ed|ing|ation|ations|er|ers)", re.I),
        "Use use.",
    ),
    _TermRule(
        re.compile(r"commenc(?:e|es|ed|ing|ement|ements)", re.I),
        "Use start or begin.",
    ),
    _TermRule(
        re.compile(r"operat(?:ion|ions|ional|ionally)", re.I),
        "Use task, step, work, action, or state the direct verb.",
    ),
    _TermRule(
        re.compile(r"execut(?:e|es|ed|ing|ion|ions|able|ables|or|ors)", re.I),
        "Use run, start, runnable, program file, or state the direct verb.",
    ),
    _TermRule(
        re.compile(r"perform(?:s|ed|ing)?", re.I),
        "Use a direct verb such as check, save, write, do, or run.",
    ),
    _TermRule(
        re.compile(r"obtain(?:s|ed|ing)?", re.I),
        "Use get, read, or receive.",
    ),
    _TermRule(
        re.compile(r"terminat(?:e|es|ed|ing|ion|ions|or|ors)", re.I),
        "Use stop, end, or cancel.",
    ),
    _TermRule(
        re.compile(
            r"(?:complet(?:e|es|ed|ing|ion|ions|eness|ely)|"
            r"incomplet(?:e|ely|eness|ion)?)",
            re.I,
        ),
        "Use finish, done, full, ready, missing, partial, or unfinished.",
    ),
    _TermRule(
        re.compile(r"transact(?:ion|ions|ional|ionally)", re.I),
        "Use batch, patch, step, change, or group.",
    ),
    _TermRule(
        re.compile(r"fixture(?:s)?", re.I),
        "Use sample, test data, or test setup.",
    ),
    _TermRule(
        re.compile(
            r"initial(?:s|ly|i[sz](?:e|es|ed|ing|er|ers|ation|ations))?",
            re.I,
        ),
        "Use first, start, base, root, set up, or start value.",
    ),
)

_PHRASE_RULES = (
    (
        re.compile(r"\bin order\s+to\b", re.I),
        "Use to.",
    ),
    (
        re.compile(r"\bprior\s+to\b", re.I),
        "Use before.",
    ),
    (
        re.compile(
            r"\bis\s+(?:performed|carried\s+out|executed|split\s+into|carried\s+into)\b",
            re.I,
        ),
        "Use direct active voice (for example, 'collects', 'runs', or 'uses').",
    ),
)

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

# These files must contain the restricted terms that they define or explain.
_EXACT_EXCEPTIONS = {
    "docs/WORDING.md",
    "scripts/rust_check_lib/wording.py",
    "scripts/tests/test_rust_check.py",
}

_IDENTIFIER_RE = re.compile(r"[A-Za-z][A-Za-z0-9_]*")
_CAMEL_PART_RE = re.compile(r"[A-Z]+(?=[A-Z][a-z]|\d|$)|[A-Z]?[a-z]+|[A-Z]+|\d+")
_ALLOW_MARKER_RE = re.compile(r"wording:\s*allow\s+([^\n]+)", re.I)
_WINDOWS_API_NAMES = {
    "process_terminate",
    "processterminate",
    "shellexecute",
    "shellexecutew",
    "terminateprocess",
}

_RUST_TEXT_NODE_TYPES = {
    "block_comment",
    "line_comment",
    "raw_string_literal",
    "string_literal",
}

# Tree-sitter gives these declarations a stable `name` field. They are project
# definitions even when they occur inside a trait or impl block.
_RUST_NAMED_DEFINITION_TYPES = {
    "associated_type",
    "const_item",
    "enum_item",
    "enum_variant",
    "field_declaration",
    "function_item",
    "function_signature_item",
    "macro_definition",
    "macro_rules_definition",
    "mod_item",
    "static_item",
    "struct_item",
    "trait_item",
    "type_item",
    "union_item",
}

_RUST_PATTERN_OWNER_TYPES = {
    "for_expression",
    "let_condition",
    "let_declaration",
    "parameter",
}


def _read_text(path: Path) -> str | None:
    try:
        return path.read_text(encoding="utf-8")
    except (OSError, UnicodeDecodeError):
        return None


def _identifier_parts(identifier: str) -> tuple[str, ...]:
    parts: list[str] = []
    for snake_part in identifier.split("_"):
        if not snake_part:
            continue
        camel_parts = _CAMEL_PART_RE.findall(snake_part)
        parts.extend(camel_parts or [snake_part])
    return tuple(parts)


def _allowed_terms(line: str) -> set[str]:
    match = _ALLOW_MARKER_RE.search(line)
    if match is None:
        return set()
    return {
        item.strip().casefold()
        for item in re.split(r"[,\s]+", match.group(1))
        if item.strip()
    }


def _is_external_term(
    *,
    rel: str,
    suffix: str,
    text: str,
    context_line: str,
    identifier: str,
    part: str,
    start: int,
) -> bool:
    part_folded = part.casefold()
    identifier_folded = identifier.casefold()

    if part_folded in _allowed_terms(context_line):
        return True

    # RustCrypto and similar digest APIs use finalize as a fixed method name.
    # This mainly applies to comments and documents. Rust call paths are not
    # scanned as project definitions by the AST-based Rust pass.
    if part_folded.startswith("finaliz"):
        prefix = text[max(0, start - 2) : start]
        if prefix.endswith(".") or prefix.endswith("::"):
            return True

    # Keep Windows API and constant names unchanged.
    if identifier_folded in _WINDOWS_API_NAMES:
        return True

    # I/O completion port is the official Windows term. Keep it in documents.
    if part_folded.startswith("complet") and suffix == ".md":
        folded_line = context_line.casefold()
        if "i/o completion port" in folded_line or "io completion port" in folded_line:
            return True

    # The launcher protocol fixes these resource names and values.
    if part_folded.startswith("initial"):
        # Tree-sitter exposes these fixed Rust grammar node names.
        if identifier_folded in {
            "field_initializer",
            "shorthand_field_initializer",
            "base_field_initializer",
        }:
            return True
        # Keep the exact observed game log text unchanged.
        if "assets initialized" in context_line.casefold():
            return True

    if part_folded == "initial":
        if identifier_folded.startswith(("index_initial", "pref_initial", "initial_")):
            return True
        folded_line = context_line.casefold()
        if "resource_group_base" in folded_line and '"initial"' in folded_line:
            return True
        if any(
            marker in folded_line
            for marker in (
                "index_initial",
                "pref_initial",
                "resource group",
                "res_version",
            )
        ):
            return True
        if rel.endswith("runtime/files/vfs/sync.rs") and context_line.strip().rstrip(",") == '"initial"':
            return True

    return False


def _term_matches(
    *,
    rel: str,
    suffix: str,
    text: str,
    context_line: str | None = None,
) -> list[tuple[str, str, str]]:
    context = text if context_line is None else context_line
    matches: list[tuple[str, str, str]] = []
    seen: set[tuple[str, str]] = set()
    for identifier_match in _IDENTIFIER_RE.finditer(text):
        identifier = identifier_match.group(0)
        search_start = identifier_match.start()
        offset = 0
        for part in _identifier_parts(identifier):
            part_start = text.find(part, search_start + offset, identifier_match.end())
            if part_start < 0:
                part_start = identifier_match.start()
            offset = max(0, part_start + len(part) - identifier_match.start())
            for rule in _TERM_RULES:
                if rule.pattern.fullmatch(part) is None:
                    continue
                if _is_external_term(
                    rel=rel,
                    suffix=suffix,
                    text=text,
                    context_line=context,
                    identifier=identifier,
                    part=part,
                    start=part_start,
                ):
                    continue
                key = (part.casefold(), rule.hint)
                if key in seen:
                    continue
                seen.add(key)
                matches.append((part, identifier, rule.hint))
    return matches


def _pattern_bindings(pattern: Node) -> Iterable[Node]:
    """Yield identifiers that create local bindings in a non-match pattern."""

    if pattern.type in {"identifier", "shorthand_field_identifier"}:
        yield pattern
        return

    if pattern.type in {
        "boolean_literal",
        "char_literal",
        "float_literal",
        "integer_literal",
        "negative_literal",
        "range_pattern",
        "scoped_identifier",
        "scoped_type_identifier",
        "string_literal",
        "type_identifier",
        "wildcard_pattern",
    }:
        return

    if pattern.type == "tuple_struct_pattern":
        type_node = pattern.child_by_field_name("type")
        for child in pattern.named_children:
            if child != type_node:
                yield from _pattern_bindings(child)
        return

    if pattern.type == "struct_pattern":
        for child in pattern.named_children:
            if child.type == "field_pattern":
                nested = child.child_by_field_name("pattern")
                if nested is not None:
                    yield from _pattern_bindings(nested)
                    continue
                shorthand = child.child_by_field_name("name")
                if shorthand is not None and shorthand.type == "shorthand_field_identifier":
                    yield shorthand
            elif child.type not in {"type_identifier", "scoped_type_identifier"}:
                yield from _pattern_bindings(child)
        return

    if pattern.type == "field_pattern":
        nested = pattern.child_by_field_name("pattern")
        if nested is not None:
            yield from _pattern_bindings(nested)
            return
        shorthand = pattern.child_by_field_name("name")
        if shorthand is not None and shorthand.type == "shorthand_field_identifier":
            yield shorthand
        return

    for child in pattern.named_children:
        yield from _pattern_bindings(child)


def _rust_definition_nodes(source: SourceFile) -> Iterable[Node]:
    seen: set[tuple[int, int]] = set()

    def emit(node: Node | None) -> Iterable[Node]:
        if node is None:
            return ()
        key = (node.start_byte, node.end_byte)
        if key in seen:
            return ()
        seen.add(key)
        return (node,)

    for node in walk_named(source.tree.root_node):
        if node.type in _RUST_NAMED_DEFINITION_TYPES:
            yield from emit(node.child_by_field_name("name"))
            continue

        if node.type in _RUST_PATTERN_OWNER_TYPES:
            pattern = node.child_by_field_name("pattern")
            if pattern is not None:
                for binding in _pattern_bindings(pattern):
                    yield from emit(binding)
            continue

        if node.type == "closure_parameters":
            for child in node.named_children:
                if child.type == "parameter":
                    pattern = child.child_by_field_name("pattern")
                    if pattern is not None:
                        for binding in _pattern_bindings(pattern):
                            yield from emit(binding)
                else:
                    for binding in _pattern_bindings(child):
                        yield from emit(binding)
            continue

        if node.type == "type_parameters":
            for child in node.named_children:
                if child.type == "type_identifier":
                    yield from emit(child)
                elif child.type == "constrained_type_parameter":
                    yield from emit(child.child_by_field_name("left"))
                elif child.type == "const_parameter":
                    yield from emit(child.child_by_field_name("name"))
            continue

        # Only an explicit `as Alias` creates a project-chosen import name.
        # The final segment of `use external::TaskCompletion` remains an
        # external or upstream name and is therefore not checked here.
        if node.type == "use_as_clause":
            yield from emit(node.child_by_field_name("alias"))


def _source_lines(source: SourceFile) -> list[str]:
    return source.data.decode("utf-8", "replace").splitlines()


def _context_line(lines: list[str], row: int) -> str:
    return lines[row] if 0 <= row < len(lines) else ""


def _check_rust_source(
    checker: Checker,
    source: SourceFile,
    *,
    resolved_sources: set[Path],
) -> None:
    lines = _source_lines(source)
    source_path = source.path.resolve()

    for node in _rust_definition_nodes(source):
        identifier = source.text(node)
        context = _context_line(lines, node.start_point.row)
        for matched_part, full_identifier, hint in _term_matches(
            rel=source.rel,
            suffix=".rs",
            text=identifier,
            context_line=context,
        ):
            evidence = [
                f"internal definition: {identifier}",
                f"matched text: {matched_part}",
                f"AST node: {node.type}",
            ]
            if source_path in resolved_sources:
                evidence.append("source file resolved from the crate module graph")
            if full_identifier != matched_part:
                evidence.append(f"identifier: {full_identifier}")
            checker.add(
                "WRD001",
                "warning",
                "Use direct project wording in an internal Rust name",
                source=source,
                node=node,
                confidence="definite",
                hint=hint,
                evidence=tuple(evidence),
            )

    for node in walk_named(source.tree.root_node):
        if node.type not in _RUST_TEXT_NODE_TYPES:
            continue
        text = source.text(node)
        for offset, line in enumerate(text.splitlines() or [text]):
            row = node.start_point.row + offset
            context = _context_line(lines, row)
            for pattern, hint in _PHRASE_RULES:
                match = pattern.search(line)
                if match is None:
                    continue
                checker.add(
                    "WRD001",
                    "warning",
                    "Use direct project wording instead of an abstract phrase",
                    path=source.path,
                    line=row + 1,
                    confidence="definite",
                    hint=hint,
                    evidence=(
                        f"line {row + 1}: {context.strip()[:160]}",
                        f"matched text: {match.group(0)}",
                        f"AST text node: {node.type}",
                    ),
                )

            for matched_part, identifier, hint in _term_matches(
                rel=source.rel,
                suffix=".rs",
                text=line,
                context_line=context,
            ):
                evidence = [
                    f"line {row + 1}: {context.strip()[:160]}",
                    f"matched text: {matched_part}",
                    f"AST text node: {node.type}",
                ]
                if identifier != matched_part:
                    evidence.append(f"identifier: {identifier}")
                checker.add(
                    "WRD001",
                    "warning",
                    "Use direct project wording instead of an abstract term",
                    path=source.path,
                    line=row + 1,
                    confidence="definite",
                    hint=hint,
                    evidence=tuple(evidence),
                )


def _resolved_source_paths(resolver: NameResolver) -> set[Path]:
    return {
        module.source.path.resolve()
        for target in resolver.host.targets
        for module in target.iter_modules()
    }


def run(checker: Checker, resolver: NameResolver) -> None:
    resolved_sources = _resolved_source_paths(resolver)

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

        for segment_index, segment in enumerate(Path(rel).parts, start=1):
            name = Path(segment).stem
            for matched_part, identifier, hint in _term_matches(
                rel=rel,
                suffix=path.suffix.casefold(),
                text=name,
            ):
                checker.add(
                    "WRD001",
                    "warning",
                    "Use direct project wording in file and directory names",
                    path=path,
                    column=segment_index,
                    confidence="definite",
                    hint=hint,
                    evidence=(
                        f"path segment: {segment}",
                        f"matched text: {matched_part}",
                        f"identifier: {identifier}",
                    ),
                )

        suffix = path.suffix.casefold()
        if suffix not in _TEXT_SUFFIXES:
            continue

        if suffix == ".rs":
            source = checker.source_for(path)
            if source is not None:
                _check_rust_source(
                    checker,
                    source,
                    resolved_sources=resolved_sources,
                )
            continue

        text = _read_text(path)
        if text is None:
            continue
        for line_number, line in enumerate(text.splitlines(), start=1):
            for pattern, hint in _PHRASE_RULES:
                match = pattern.search(line)
                if match is None:
                    continue
                checker.add(
                    "WRD001",
                    "warning",
                    "Use direct project wording instead of an abstract phrase",
                    path=path,
                    line=line_number,
                    confidence="definite",
                    hint=hint,
                    evidence=(
                        f"line {line_number}: {line.strip()[:160]}",
                        f"matched text: {match.group(0)}",
                    ),
                )

            for matched_part, identifier, hint in _term_matches(
                rel=rel,
                suffix=suffix,
                text=line,
            ):
                evidence = [
                    f"line {line_number}: {line.strip()[:160]}",
                    f"matched text: {matched_part}",
                ]
                if identifier != matched_part:
                    evidence.append(f"identifier: {identifier}")
                checker.add(
                    "WRD001",
                    "warning",
                    "Use direct project wording instead of an abstract term",
                    path=path,
                    line=line_number,
                    confidence="definite",
                    hint=hint,
                    evidence=tuple(evidence),
                )
