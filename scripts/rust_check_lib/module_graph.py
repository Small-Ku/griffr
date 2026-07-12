from __future__ import annotations

import os
import re
import sys
from pathlib import Path
from typing import Any, Protocol

from tree_sitter import Node

from .cfg import TRUE, all_of, compatibility, conditions_from_attributes
from .models import CrateTarget, ModuleUnit, SourceFile


class ModuleHost(Protocol):
    root: Path
    targets: list[CrateTarget]

    def source_for(self, path: Path) -> SourceFile | None: ...
    def excluded(self, path: Path) -> bool: ...
    def add(self, code: str, severity: str, message: str, **kwargs: Any) -> None: ...


def build(host: ModuleHost) -> None:
    for target in host.targets:
        if not target.root_file.is_file():
            continue
        source = host.source_for(target.root_file)
        if source is None:
            continue
        root = ModuleUnit(
            target=target,
            path=(),
            source=source,
            body=source.tree.root_node,
            child_dir=target.root_file.parent.resolve(),
            condition=TRUE,
        )
        target.modules[()].append(root)
        _walk_module(host, root, set())

    _check_orphans(host)


def _module_key(module: ModuleUnit) -> tuple[tuple[str, ...], Path, int, int]:
    return (
        module.path,
        module.source.path.resolve(),
        module.body.start_byte,
        module.body.end_byte,
    )


def _walk_module(
    host: ModuleHost,
    module: ModuleUnit,
    stack: set[tuple[tuple[str, ...], Path, int, int]],
) -> None:
    key = _module_key(module)
    if key in stack:
        host.add(
            "MOD001",
            "error",
            f"Module/include cycle detected at {module.display}",
            path=module.source.path,
            evidence=(f"cycle key: {module.source.rel}:{module.body.start_byte}",),
        )
        return

    stack = {*stack, key}
    module.target.reachable_files.add(module.source.path.resolve())
    children = list(module.body.named_children)

    for index, node in enumerate(children):
        attrs = tuple(_preceding_attributes(module, children, index))
        if node.type == "mod_item":
            _register_child_module(host, module, node, attrs, stack)
        else:
            macro_node = node
            if node.type == "expression_statement" and len(node.named_children) == 1:
                macro_node = node.named_children[0]
            if macro_node.type == "macro_invocation":
                _register_literal_include(host, module, macro_node, attrs, stack)


def _register_child_module(
    host: ModuleHost,
    module: ModuleUnit,
    node: Node,
    attrs: tuple[str, ...],
    stack: set[tuple[tuple[str, ...], Path, int, int]],
) -> None:
    name_node = node.child_by_field_name("name")
    if name_node is None:
        return
    name = module.source.text(name_node)
    child_path = module.path + (name,)
    condition = all_of(module.condition, conditions_from_attributes(attrs))
    body = next(
        (child for child in node.named_children if child.type == "declaration_list"),
        None,
    )

    if body is not None:
        child = ModuleUnit(
            target=module.target,
            path=child_path,
            source=module.source,
            body=body,
            child_dir=(module.child_dir / name).resolve(),
            parent=module,
            attributes=attrs,
            condition=condition,
            declaration_node=node,
        )
    else:
        resolved = _resolve_mod_file(host, module, name, attrs, node)
        if resolved is None:
            return
        source = host.source_for(resolved)
        if source is None:
            return
        child_dir = (
            resolved.parent.resolve()
            if resolved.name == "mod.rs"
            else resolved.with_suffix("").resolve()
        )
        child = ModuleUnit(
            target=module.target,
            path=child_path,
            source=source,
            body=source.tree.root_node,
            child_dir=child_dir,
            parent=module,
            attributes=attrs,
            condition=condition,
            declaration_node=node,
        )

    _register_module_variant(host, child)
    _walk_module(host, child, stack)


def _register_module_variant(host: ModuleHost, child: ModuleUnit) -> None:
    variants = child.target.modules[child.path]
    for previous in variants:
        if child.additive_fragment or previous.additive_fragment:
            continue
        overlap = compatibility(previous.condition, child.condition)
        if overlap is False:
            continue
        if overlap is True:
            host.add(
                "MOD002",
                "error",
                f"Module {child.display} has declarations active in the same cfg configuration",
                source=child.source,
                node=child.declaration_node,
                evidence=(
                    f"previous cfg: {previous.condition.describe()}",
                    f"current cfg: {child.condition.describe()}",
                ),
            )
        else:
            host.add(
                "MOD002",
                "warning",
                f"Cannot prove duplicate module declarations for {child.display} are cfg-exclusive",
                source=child.source,
                node=child.declaration_node,
                confidence="speculative",
                hint="Make the cfg branches explicitly mutually exclusive or verify with cargo check for every supported feature/target set.",
                evidence=(
                    f"previous cfg: {previous.condition.describe()}",
                    f"current cfg: {child.condition.describe()}",
                ),
            )
    variants.append(child)


def _register_literal_include(
    host: ModuleHost,
    module: ModuleUnit,
    node: Node,
    attrs: tuple[str, ...],
    stack: set[tuple[tuple[str, ...], Path, int, int]],
) -> None:
    macro = node.child_by_field_name("macro")
    if macro is None or module.source.text(macro).split("::")[-1] != "include":
        return
    token_tree = next(
        (child for child in node.named_children if child.type == "token_tree"), None
    )
    if token_tree is None:
        return
    text = module.source.text(token_tree).strip()
    match = re.fullmatch(r'\(\s*"([^"\\]*(?:\\.[^"\\]*)*)"\s*\)', text, re.DOTALL)
    if not match:
        module.target.dynamic_includes.add(module.source.path.resolve())
        return
    try:
        literal = bytes(match.group(1), "utf-8").decode("unicode_escape")
    except UnicodeDecodeError:
        literal = match.group(1)
    included = (module.source.path.parent / literal).resolve()
    if not included.is_file():
        host.add(
            "MOD007",
            "error",
            f"Literal include! source does not exist: {included}",
            source=module.source,
            node=node,
        )
        return
    source = host.source_for(included)
    if source is None:
        return
    fragment = ModuleUnit(
        target=module.target,
        path=module.path,
        source=source,
        body=source.tree.root_node,
        child_dir=included.parent.resolve(),
        parent=module,
        attributes=attrs,
        condition=all_of(module.condition, conditions_from_attributes(attrs)),
        declaration_node=node,
        additive_fragment=True,
    )
    _register_module_variant(host, fragment)
    _walk_module(host, fragment, stack)


def _preceding_attributes(
    module: ModuleUnit, children: list[Node], index: int
) -> list[str]:
    attrs: list[str] = []
    cursor = index - 1
    while cursor >= 0 and children[cursor].type == "attribute_item":
        attrs.append(module.source.text(children[cursor]))
        cursor -= 1
    attrs.reverse()
    return attrs


def preceding_attributes(
    module: ModuleUnit, children: list[Node], index: int
) -> tuple[str, ...]:
    return tuple(_preceding_attributes(module, children, index))


def _exact_file_exists(path: Path) -> bool:
    if not path.is_file():
        return False
    if os.name == "nt" or sys.platform == "darwin":
        try:
            return path.name in {entry.name for entry in path.parent.iterdir()}
        except OSError:
            return False
    return True


def _resolve_mod_file(
    host: ModuleHost,
    module: ModuleUnit,
    name: str,
    attrs: tuple[str, ...],
    node: Node,
) -> Path | None:
    for attr in attrs:
        match = re.search(r'#\s*\[\s*path\s*=\s*"([^"]+)"\s*\]', attr)
        if match:
            path = (module.child_dir / match.group(1)).resolve()
            if not _exact_file_exists(path):
                host.add(
                    "MOD003",
                    "error",
                    f"#[path] module file does not exist: {path}",
                    source=module.source,
                    node=node,
                )
                return None
            return path

    candidates = [module.child_dir / f"{name}.rs", module.child_dir / name / "mod.rs"]
    existing = [
        candidate.resolve()
        for candidate in candidates
        if _exact_file_exists(candidate.resolve())
    ]
    if len(existing) > 1:
        host.add(
            "MOD004",
            "error",
            f"Both module file layouts exist for {name}: {existing[0]} and {existing[1]}",
            source=module.source,
            node=node,
        )
        return None
    if not existing:
        host.add(
            "MOD005",
            "error",
            f"Module {name!r} has no source file",
            source=module.source,
            node=node,
            hint=f"Expected {candidates[0]} or {candidates[1]}",
        )
        return None
    return existing[0]


def _check_orphans(host: ModuleHost) -> None:
    packages_seen: set[Path] = set()
    for target in host.targets:
        package_dir = target.package.directory.resolve()
        if package_dir in packages_seen:
            continue
        packages_seen.add(package_dir)
        src_dir = target.package.directory / "src"
        if not src_dir.is_dir():
            continue
        reachable: set[Path] = set()
        dynamic_include_roots: set[Path] = set()
        for sibling in host.targets:
            if sibling.package.directory.resolve() == package_dir:
                reachable.update(sibling.reachable_files)
                dynamic_include_roots.update(sibling.dynamic_includes)
        for path in sorted(src_dir.rglob("*.rs")):
            resolved = path.resolve()
            if host.excluded(resolved) or resolved in reachable:
                continue
            confidence = "speculative" if dynamic_include_roots else "definite"
            hint = (
                "Delete stale source files or add the missing mod/include declaration."
            )
            evidence = [
                "No crate target, mod declaration, #[path], or literal include! reaches this file."
            ]
            if dynamic_include_roots:
                hint += " A non-literal include! exists, so verify generated include paths before deleting it."
                evidence.append(
                    "At least one non-literal include! prevented complete reachability proof."
                )
            host.add(
                "MOD006",
                "warning",
                "Rust source file is not reachable from any crate target",
                path=resolved,
                hint=hint,
                confidence=confidence,
                evidence=tuple(evidence),
            )
