from __future__ import annotations

import dataclasses
from collections import defaultdict
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any

from tree_sitter import Node, Tree

from .cfg import CfgExpr, TRUE

SEVERITY_ORDER = {"error": 0, "warning": 1, "note": 2}
CONFIDENCE_ORDER = {"definite": 0, "probable": 1, "speculative": 2}


@dataclass(frozen=True)
class Diagnostic:
    code: str
    severity: str
    message: str
    path: str = ""
    line: int = 0
    column: int = 0
    hint: str = ""
    confidence: str = "definite"
    evidence: tuple[str, ...] = ()

    def sort_key(self) -> tuple[Any, ...]:
        return (
            SEVERITY_ORDER.get(self.severity, 9),
            CONFIDENCE_ORDER.get(self.confidence, 9),
            self.path,
            self.line,
            self.column,
            self.code,
            self.message,
        )

    def as_dict(self) -> dict[str, Any]:
        return dataclasses.asdict(self)


@dataclass(frozen=True)
class TextEdit:
    code: str
    path: Path
    start_byte: int
    end_byte: int
    replacement: bytes
    description: str
    priority: int = 100

    def key(self) -> tuple[Any, ...]:
        return (
            str(self.path),
            self.start_byte,
            self.end_byte,
            self.replacement,
            self.code,
        )


@dataclass(frozen=True)
class AppliedFix:
    code: str
    path: str
    description: str

    def as_dict(self) -> dict[str, str]:
        return dataclasses.asdict(self)


@dataclass
class SourceFile:
    path: Path
    rel: str
    data: bytes
    tree: Tree

    def text(self, node: Node) -> str:
        return self.data[node.start_byte : node.end_byte].decode("utf-8", "replace")

    def location(self, node: Node) -> tuple[int, int]:
        return node.start_point.row + 1, node.start_point.column + 1


@dataclass(frozen=True)
class Visibility:
    kind: str
    module_path: tuple[str, ...] = ()

    def describe(self) -> str:
        if self.kind != "in":
            return self.kind
        return "pub(in crate::" + "::".join(self.module_path) + ")"


PRIVATE = Visibility("private")
PUBLIC = Visibility("public")
CRATE_VISIBLE = Visibility("crate")


@dataclass
class Dependency:
    extern_name: str
    package_name: str
    kinds: set[str] = field(default_factory=set)
    optional: bool = False


@dataclass
class UseSpec:
    path: tuple[str, ...]
    alias: str | None
    glob: bool
    visibility: Visibility
    node: Node
    module: "ModuleUnit"
    condition: CfgExpr = TRUE
    symbol: "Symbol | None" = None


@dataclass
class Symbol:
    name: str
    kind: str
    visibility: Visibility
    module: "ModuleUnit"
    node: Node
    condition: CfgExpr = TRUE
    target_module_path: tuple[str, ...] | None = None
    import_path: tuple[str, ...] | None = None
    generated_by_macro: str = ""
    use_spec: UseSpec | None = None


@dataclass
class ModuleUnit:
    target: "CrateTarget"
    path: tuple[str, ...]
    source: SourceFile
    body: Node
    child_dir: Path
    parent: "ModuleUnit | None" = None
    attributes: tuple[str, ...] = ()
    condition: CfgExpr = TRUE
    declaration_node: Node | None = None
    additive_fragment: bool = False
    symbols: dict[str, list[Symbol]] = field(default_factory=lambda: defaultdict(list))
    imports: list[UseSpec] = field(default_factory=list)
    glob_imports: list[UseSpec] = field(default_factory=list)
    unknown_item_macros: list[tuple[str, Node, CfgExpr]] = field(default_factory=list)
    walk_cache: tuple[Node, ...] | None = None
    item_conditions: dict[int, CfgExpr] = field(default_factory=dict)

    @property
    def display(self) -> str:
        return "crate" if not self.path else "crate::" + "::".join(self.path)


@dataclass
class Package:
    name: str
    manifest_path: Path
    directory: Path
    manifest: dict[str, Any]
    dependencies: dict[str, Dependency] = field(default_factory=dict)

    @property
    def extern_name(self) -> str:
        return self.name.replace("-", "_")


@dataclass
class CrateTarget:
    key: str
    name: str
    extern_name: str
    kind: str
    root_file: Path
    package: Package
    reachable_files: set[Path] = field(default_factory=set)
    modules: dict[tuple[str, ...], list[ModuleUnit]] = field(
        default_factory=lambda: defaultdict(list)
    )
    dynamic_includes: set[Path] = field(default_factory=set)

    def iter_modules(self):
        for variants in self.modules.values():
            yield from variants


@dataclass
class Resolution:
    symbols: tuple[Symbol, ...] = ()
    modules: tuple[ModuleUnit, ...] = ()
    external_unknown: bool = False
    inaccessible: bool = False
    uncertain_macro: bool = False
    associated_unknown: bool = False
    missing_segment: str = ""
    detail: str = ""

    @property
    def symbol(self) -> Symbol | None:
        return self.symbols[0] if self.symbols else None

    @property
    def module(self) -> ModuleUnit | None:
        return self.modules[0] if self.modules else None

    @property
    def resolved(self) -> bool:
        return bool(
            self.symbols
            or self.modules
            or self.external_unknown
            or self.associated_unknown
        )


@dataclass
class ToolResult:
    label: str
    command: list[str]
    available: bool
    returncode: int | None
    output: str


@dataclass
class DiffEntry:
    path: str
    status: str
    detail: str = ""
    ast_equivalent: bool | None = None
