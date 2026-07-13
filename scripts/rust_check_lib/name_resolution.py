from __future__ import annotations

import dataclasses
import re
from collections import Counter, defaultdict
from collections.abc import Iterable, Iterator, Sequence
from pathlib import Path
from typing import Any, Protocol

from tree_sitter import Node

from .cfg import CfgExpr, TRUE, all_of, compatibility, conditions_from_attributes
from .models import (
    CRATE_VISIBLE,
    PRIVATE,
    PUBLIC,
    CrateTarget,
    ModuleUnit,
    Package,
    Resolution,
    SourceFile,
    Symbol,
    UseSpec,
    Visibility,
)
from .module_graph import preceding_attributes
from .parsing import walk_named

ITEM_KIND_BY_NODE = {
    "const_item": "const",
    "enum_item": "enum",
    "function_item": "function",
    "macro_definition": "macro",
    "macro_rules_definition": "macro",
    "mod_item": "module",
    "static_item": "static",
    "struct_item": "struct",
    "trait_item": "trait",
    "type_item": "type",
    "union_item": "union",
}

RUST_PRELUDE = {
    "AsMut",
    "AsRef",
    "Box",
    "Clone",
    "Copy",
    "Default",
    "DoubleEndedIterator",
    "Drop",
    "Eq",
    "Err",
    "ExactSizeIterator",
    "Extend",
    "Fn",
    "FnMut",
    "FnOnce",
    "From",
    "FromIterator",
    "Future",
    "Into",
    "IntoFuture",
    "IntoIterator",
    "Iterator",
    "None",
    "Ok",
    "Option",
    "Ord",
    "PartialEq",
    "PartialOrd",
    "Result",
    "Send",
    "Sized",
    "Some",
    "String",
    "Sync",
    "ToOwned",
    "ToString",
    "TryFrom",
    "TryInto",
    "Unpin",
    "Vec",
    "bool",
    "char",
    "f32",
    "f64",
    "i128",
    "i16",
    "i32",
    "i64",
    "i8",
    "isize",
    "str",
    "u128",
    "u16",
    "u32",
    "u64",
    "u8",
    "usize",
}

BUILTIN_EXTERNAL_ROOTS = {"std", "core", "alloc", "proc_macro", "test"}

PREDEFINED_GLOB_EXPORTS: dict[tuple[str, ...], set[str]] = {
    ("winio", "prelude"): {
        "App",
        "Child",
        "Window",
        "Component",
        "ComponentSender",
        "Error",
        "Size",
        "WindowEvent",
        "Grid",
        "Result",
        "Layoutable",
        "Visible",
        "TextWidget",
        "init",
        "start",
        "update_children",
        "layout",
        "widget_tree",
    }
}



class AnalysisHost(Protocol):
    root: Path
    packages: list[Package]
    targets: list[CrateTarget]

    def add(self, code: str, severity: str, message: str, **kwargs: Any) -> None: ...


class NameResolver:
    def __init__(self, host: AnalysisHost):
        self.host = host
        self.symbols_by_name: dict[tuple[str, str], list[Symbol]] = defaultdict(list)
        self.lib_targets_by_package: dict[str, list[CrateTarget]] = defaultdict(list)
        self.lib_targets_by_extern: dict[str, list[CrateTarget]] = defaultdict(list)
        self.macro_templates: dict[
            tuple[str, str], list[tuple[ModuleUnit, Node, CfgExpr, str]]
        ] = defaultdict(list)
        for target in host.targets:
            if target.kind == "lib":
                self.lib_targets_by_package[target.package.name].append(target)
                self.lib_targets_by_extern[target.extern_name].append(target)

    def run(self) -> None:
        self._collect_all_items()
        self._check_duplicate_definitions()
        self._check_imports_and_paths()
        self._check_direct_call_arity()

    # ---------- collection ----------
    def _collect_all_items(self) -> None:
        for target in self.host.targets:
            for module in target.iter_modules():
                self._collect_module_items(module)
        for target in self.host.targets:
            for module in target.iter_modules():
                for symbols in module.symbols.values():
                    for symbol in symbols:
                        self.symbols_by_name[(target.key, symbol.name)].append(symbol)

    def _collect_module_items(self, module: ModuleUnit) -> None:
        children = list(module.body.named_children)
        for index, node in enumerate(children):
            attrs = preceding_attributes(module, children, index)
            condition = all_of(module.condition, conditions_from_attributes(attrs))
            module.item_conditions[node.start_byte] = condition
            macro_node = node
            if node.type == "expression_statement" and len(node.named_children) == 1:
                macro_node = node.named_children[0]
            if macro_node.type == "macro_invocation":
                self._collect_macro_generated_items(module, macro_node, condition)
                continue
            if node.type == "use_declaration":
                specs = self._parse_use_declaration(module, node, condition)
                for spec in specs:
                    module.imports.append(spec)
                    if spec.glob:
                        module.glob_imports.append(spec)
                        continue
                    if spec.alias:
                        symbol = Symbol(
                            name=spec.alias,
                            kind="import",
                            visibility=spec.visibility,
                            module=module,
                            node=node,
                            condition=condition,
                            import_path=spec.path,
                            use_spec=spec,
                        )
                        spec.symbol = symbol
                        module.symbols[spec.alias].append(symbol)
                continue
            if node.type in {"macro_definition", "macro_rules_definition"}:
                name_node = node.child_by_field_name("name")
                if name_node is not None:
                    macro_name = module.source.text(name_node)
                    self.macro_templates[(module.target.key, macro_name)].append(
                        (module, node, condition, module.source.text(node))
                    )
            if node.type == "extern_crate_declaration":
                self._collect_extern_crate(module, node, condition)
                continue
            kind = ITEM_KIND_BY_NODE.get(node.type)
            if not kind:
                continue
            name_node = node.child_by_field_name("name")
            if name_node is None:
                name_node = next(
                    (
                        child
                        for child in node.named_children
                        if child.type in {"identifier", "type_identifier"}
                    ),
                    None,
                )
            if name_node is None:
                continue
            name = module.source.text(name_node)
            symbol = Symbol(
                name=name,
                kind=kind,
                visibility=self._visibility(node, module),
                module=module,
                node=node,
                condition=condition,
                target_module_path=module.path + (name,) if kind == "module" else None,
            )
            module.symbols[name].append(symbol)

    def _collect_extern_crate(
        self, module: ModuleUnit, node: Node, condition: CfgExpr
    ) -> None:
        text = module.source.text(node)
        match = re.search(
            r"\bextern\s+crate\s+([A-Za-z_][A-Za-z0-9_]*)(?:\s+as\s+([A-Za-z_][A-Za-z0-9_]*))?",
            text,
        )
        if not match:
            return
        original = match.group(1)
        alias = match.group(2) or original
        symbol = Symbol(
            alias,
            "extern_crate",
            self._visibility(node, module),
            module,
            node,
            condition=condition,
            import_path=(original,),
        )
        module.symbols[alias].append(symbol)

    def _collect_macro_generated_items(
        self, module: ModuleUnit, node: Node, condition: CfgExpr
    ) -> None:
        macro_node = node.child_by_field_name("macro")
        if macro_node is None:
            return
        macro_path = module.source.text(macro_node)
        macro_name = macro_path.split("::")[-1]
        if macro_name == "include":
            return
        token_tree = next(
            (child for child in node.named_children if child.type == "token_tree"), None
        )
        if token_tree is None:
            module.unknown_item_macros.append((macro_path, node, condition))
            return
        text = module.source.text(token_tree)
        inferred: list[tuple[str, str, Visibility]] = []

        if macro_name in {
            "bitflags",
            "pin_project",
            "pin_project_lite",
            "newtype_index",
            "opaque_typedef",
        }:
            for match in re.finditer(
                r"(?m)(?P<vis>pub(?:\s*\([^)]*\))?\s+)?(?P<kind>struct|enum|union)\s+(?P<name>[A-Za-z_][A-Za-z0-9_]*)",
                text,
            ):
                inferred.append(
                    (
                        match.group("name"),
                        match.group("kind"),
                        self._visibility_from_text(match.group("vis") or "", module),
                    )
                )
        elif macro_name == "lazy_static":
            for match in re.finditer(
                r"(?m)(?P<vis>pub(?:\s*\([^)]*\))?\s+)?static\s+ref\s+(?P<name>[A-Za-z_][A-Za-z0-9_]*)",
                text,
            ):
                inferred.append(
                    (
                        match.group("name"),
                        "static",
                        self._visibility_from_text(match.group("vis") or "", module),
                    )
                )
        elif macro_name == "thread_local":
            for match in re.finditer(
                r"(?m)(?P<vis>pub(?:\s*\([^)]*\))?\s+)?static\s+(?P<name>[A-Za-z_][A-Za-z0-9_]*)",
                text,
            ):
                inferred.append(
                    (
                        match.group("name"),
                        "static",
                        self._visibility_from_text(match.group("vis") or "", module),
                    )
                )

        if not inferred:
            templates = self.macro_templates.get((module.target.key, macro_name), [])
            template = next(
                (
                    item
                    for item in reversed(templates)
                    if item[0].path == module.path
                    and item[1].start_byte < node.start_byte
                    and compatibility(item[2], condition) is not False
                ),
                templates[-1] if templates else None,
            )
            if template is not None:
                _, _, template_condition, template_text = template
                item_pattern = re.compile(
                    r"(?m)(?<![$A-Za-z0-9_])"
                    r"(?P<vis>pub(?:\s*\([^)]*\))?\s+)?"
                    r"(?P<kind>struct|enum|union|trait|type|fn|const|static|mod)\s+"
                    r"(?P<name>[A-Za-z_][A-Za-z0-9_]*)"
                )
                kind_map = {"fn": "function", "mod": "module"}
                for match in item_pattern.finditer(template_text):
                    inferred.append(
                        (
                            match.group("name"),
                            kind_map.get(match.group("kind"), match.group("kind")),
                            self._visibility_from_text(
                                match.group("vis") or "", module
                            ),
                        )
                    )
                condition = all_of(condition, template_condition)

        if not inferred:
            module.unknown_item_macros.append((macro_path, node, condition))
            return
        for name, kind, visibility in inferred:
            module.symbols[name].append(
                Symbol(
                    name=name,
                    kind=kind,
                    visibility=visibility,
                    module=module,
                    node=node,
                    condition=condition,
                    target_module_path=module.path + (name,)
                    if kind == "module"
                    else None,
                    generated_by_macro=macro_path,
                )
            )

    # ---------- attributes / visibility / use ----------
    def _visibility(self, node: Node, module: ModuleUnit) -> Visibility:
        vis = next(
            (
                child
                for child in node.named_children
                if child.type == "visibility_modifier"
            ),
            None,
        )
        return self._visibility_from_text(
            module.source.text(vis) if vis is not None else "", module
        )

    def _visibility_from_text(self, text: str, module: ModuleUnit) -> Visibility:
        normalized = re.sub(r"\s+", "", text)
        if not normalized:
            return PRIVATE
        if normalized == "pub":
            return PUBLIC
        if normalized in {"pub(crate)", "crate"}:
            return CRATE_VISIBLE
        if normalized == "pub(self)":
            return Visibility("in", module.path)
        if normalized == "pub(super)":
            return Visibility("in", module.path[:-1])
        match = re.fullmatch(r"pub\(in(.+)\)", normalized)
        if not match:
            return Visibility("restricted", module.path)
        path = tuple(part for part in match.group(1).split("::") if part)
        if path and path[0] == "crate":
            return Visibility("in", path[1:])
        if path and path[0] == "self":
            return Visibility("in", module.path + path[1:])
        if path and path[0] == "super":
            cursor = module.path
            index = 0
            while index < len(path) and path[index] == "super":
                cursor = cursor[:-1]
                index += 1
            return Visibility("in", cursor + path[index:])
        return Visibility("restricted", module.path)

    def _parse_use_declaration(
        self, module: ModuleUnit, node: Node, condition: CfgExpr
    ) -> list[UseSpec]:
        visibility = self._visibility(node, module)
        target = node.child_by_field_name("argument")
        if target is None:
            target = next(
                (
                    child
                    for child in node.named_children
                    if child.type != "visibility_modifier"
                ),
                None,
            )
        if target is None:
            return []
        return self._flatten_use_node(module, target, (), visibility, condition)

    def _flatten_use_node(
        self,
        module: ModuleUnit,
        node: Node,
        prefix: tuple[str, ...],
        visibility: Visibility,
        condition: CfgExpr,
    ) -> list[UseSpec]:
        source = module.source
        node_type = node.type
        if node_type in {"identifier", "type_identifier", "crate", "self", "super"}:
            value = source.text(node)
            path = prefix + (value,)
            alias = prefix[-1] if value == "self" and prefix else value
            if value == "self" and prefix:
                path = prefix
            return [UseSpec(path, alias, False, visibility, node, module, condition)]
        if node_type in {"scoped_identifier", "scoped_type_identifier"}:
            path = tuple(
                part for part in re.split(r"\s*::\s*", source.text(node)) if part
            )
            return [
                UseSpec(
                    prefix + path,
                    path[-1] if path else None,
                    False,
                    visibility,
                    node,
                    module,
                    condition,
                )
            ]
        if node_type == "use_wildcard":
            text = source.text(node).strip()
            extra = ()
            if text not in {"*", "::*"}:
                cleaned = text[:-3] if text.endswith("::*") else text.rstrip("*")
                extra = tuple(part for part in re.split(r"\s*::\s*", cleaned) if part)
            return [
                UseSpec(prefix + extra, None, True, visibility, node, module, condition)
            ]
        if node_type == "use_as_clause":
            path_node = node.child_by_field_name("path")
            alias_node = node.child_by_field_name("alias")
            if path_node is None or alias_node is None:
                return []
            specs = self._flatten_use_node(
                module, path_node, prefix, visibility, condition
            )
            alias = source.text(alias_node)
            return [dataclasses.replace(spec, alias=alias) for spec in specs]
        if node_type == "scoped_use_list":
            path_node = node.child_by_field_name("path")
            list_node = node.child_by_field_name("list")
            if path_node is None or list_node is None:
                return []
            base = tuple(
                part for part in re.split(r"\s*::\s*", source.text(path_node)) if part
            )
            return self._flatten_use_node(
                module, list_node, prefix + base, visibility, condition
            )
        if node_type == "use_list":
            out: list[UseSpec] = []
            for child in node.named_children:
                out.extend(
                    self._flatten_use_node(module, child, prefix, visibility, condition)
                )
            return out
        text = source.text(node).strip()
        if not text:
            return []
        path = tuple(part for part in re.split(r"\s*::\s*", text) if part)
        return [
            UseSpec(
                prefix + path,
                path[-1] if path else None,
                False,
                visibility,
                node,
                module,
                condition,
            )
        ]

    # ---------- duplicate definitions ----------
    def _check_duplicate_definitions(self) -> None:
        for target in self.host.targets:
            grouped: dict[tuple[tuple[str, ...], str, str], list[Symbol]] = defaultdict(
                list
            )
            for module in target.iter_modules():
                for name, symbols in module.symbols.items():
                    for symbol in symbols:
                        if symbol.kind in {"import", "extern_crate"}:
                            continue
                        grouped[(module.path, name, symbol.kind)].append(symbol)
            for (module_path, name, kind), symbols in grouped.items():
                for index, current in enumerate(symbols):
                    for previous in symbols[:index]:
                        if (
                            current.node.start_byte == previous.node.start_byte
                            and current.module.source.path
                            == previous.module.source.path
                        ):
                            continue
                        overlap = compatibility(previous.condition, current.condition)
                        if overlap is False:
                            continue
                        location = (
                            "crate"
                            if not module_path
                            else "crate::" + "::".join(module_path)
                        )
                        evidence = (
                            f"scope: {location}",
                            f"previous cfg: {previous.condition.describe()}",
                            f"current cfg: {current.condition.describe()}",
                        )
                        if overlap is True:
                            self.host.add(
                                "RES001",
                                "error",
                                f"Duplicate {kind} definition {name!r} can be active in the same cfg configuration",
                                source=current.module.source,
                                node=current.node,
                                evidence=evidence,
                            )
                        else:
                            self.host.add(
                                "RES001",
                                "warning",
                                f"Cannot prove duplicate {kind} definitions {name!r} are cfg-exclusive",
                                source=current.module.source,
                                node=current.node,
                                confidence="speculative",
                                hint="Check the cfg expressions across all supported targets/features.",
                                evidence=evidence,
                            )

    # ---------- path resolution ----------
    def _modules_for(
        self, target: CrateTarget, path: tuple[str, ...], condition: CfgExpr
    ) -> tuple[list[ModuleUnit], bool]:
        modules: list[ModuleUnit] = []
        uncertain = False
        for module in target.modules.get(path, []):
            overlap = compatibility(module.condition, condition)
            if overlap is False:
                continue
            if overlap is None:
                uncertain = True
            modules.append(module)
        return modules, uncertain

    def _visible(self, symbol: Symbol, requester: ModuleUnit) -> bool:
        same_crate = symbol.module.target.key == requester.target.key
        visibility = symbol.visibility
        if not same_crate:
            return visibility.kind == "public"
        if visibility.kind in {"public", "crate"}:
            return True
        if visibility.kind in {"restricted", "in"}:
            root = visibility.module_path
            return requester.path[: len(root)] == root
        defining = symbol.module.path
        return requester.path[: len(defining)] == defining

    def _dependency_target(
        self, requester: ModuleUnit, extern_name: str
    ) -> tuple[list[CrateTarget], bool]:
        if (
            extern_name == requester.target.package.extern_name
            and requester.target.kind != "lib"
        ):
            return self.lib_targets_by_package.get(
                requester.target.package.name, []
            ), True
        dependency = requester.target.package.dependencies.get(extern_name)
        if dependency is None:
            return [], False
        linked = self.lib_targets_by_package.get(dependency.package_name, [])
        return linked, True

    def resolve_path(
        self,
        requester: ModuleUnit,
        path: Sequence[str],
        *,
        condition: CfgExpr = TRUE,
        exclude_symbol: Symbol | None = None,
        _seen: set[tuple[str, tuple[str, ...], tuple[str, ...], int]] | None = None,
    ) -> Resolution:
        parts = tuple(part for part in path if part)
        if not parts:
            return Resolution(modules=(requester,))
        seen = set() if _seen is None else set(_seen)
        first = parts[0]
        index = 0
        roots: list[ModuleUnit] = []
        uncertain = False

        if first == "crate":
            roots, uncertain = self._modules_for(requester.target, (), condition)
            index = 1
        elif first in {"self", "Self"}:
            roots, uncertain = self._modules_for(
                requester.target, requester.path, condition
            )
            index = 1
        elif first == "super":
            cursor = requester.path
            while index < len(parts) and parts[index] == "super":
                if not cursor:
                    return Resolution(
                        missing_segment="super", detail="super escapes crate root"
                    )
                cursor = cursor[:-1]
                index += 1
            roots, uncertain = self._modules_for(requester.target, cursor, condition)
        else:
            local_modules, local_uncertain = self._modules_for(
                requester.target, requester.path, condition
            )
            local = self._resolve_from_modules(
                requester,
                local_modules,
                parts,
                0,
                condition,
                seen,
                exclude_symbol,
            )
            if local.resolved or local.inaccessible:
                return local

            dependency_targets, declared = self._dependency_target(requester, first)
            if declared:
                if not dependency_targets:
                    return Resolution(external_unknown=True, missing_segment=first)
                for target in dependency_targets:
                    modules, maybe = self._modules_for(target, (), condition)
                    roots.extend(modules)
                    uncertain = uncertain or maybe
                index = 1
            elif first in BUILTIN_EXTERNAL_ROOTS:
                return Resolution(external_unknown=True, missing_segment=first)
            else:
                local.uncertain_macro = local.uncertain_macro or local_uncertain
                return local

        if not roots:
            return Resolution(
                missing_segment=first,
                detail="crate/module root unavailable",
                uncertain_macro=uncertain,
            )
        result = self._resolve_from_modules(
            requester,
            roots,
            parts,
            index,
            condition,
            seen,
            exclude_symbol,
        )
        if uncertain and not result.resolved:
            result.uncertain_macro = True
        return result

    def _resolve_from_modules(
        self,
        requester: ModuleUnit,
        current_modules: Sequence[ModuleUnit],
        parts: tuple[str, ...],
        index: int,
        condition: CfgExpr,
        seen: set[tuple[str, tuple[str, ...], tuple[str, ...], int]],
        exclude_symbol: Symbol | None,
    ) -> Resolution:
        if index >= len(parts):
            return Resolution(modules=tuple(_dedupe_modules(current_modules)))
        segment = parts[index]
        symbols: list[Symbol] = []
        inaccessible = False
        uncertain_macro = False
        external_glob = False

        module_groups: dict[tuple[str, tuple[str, ...]], list[ModuleUnit]] = (
            defaultdict(list)
        )
        for module in current_modules:
            module_groups[(module.target.key, module.path)].append(module)

        for (target_key, module_path), modules in module_groups.items():
            key = (target_key, module_path, parts, index)
            if key in seen:
                continue
            local_seen = {*seen, key}
            for module in modules:
                for symbol in module.symbols.get(segment, []):
                    if symbol is exclude_symbol:
                        continue
                    overlap = compatibility(symbol.condition, condition)
                    if overlap is False:
                        continue
                    if self._visible(symbol, requester):
                        symbols.append(symbol)
                    else:
                        inaccessible = True
                if any(
                    compatibility(macro_condition, condition) is not False
                    for _, _, macro_condition in module.unknown_item_macros
                ):
                    uncertain_macro = True

            if not symbols:
                for module in modules:
                    for glob in module.glob_imports:
                        if compatibility(glob.condition, condition) is False:
                            continue
                        target = self.resolve_path(
                            glob.module,
                            glob.path,
                            condition=all_of(condition, glob.condition),
                            exclude_symbol=glob.symbol,
                            _seen=local_seen,
                        )
                        if target.external_unknown:
                            external_glob = True
                            continue
                        if target.modules:
                            nested = self._resolve_from_modules(
                                requester,
                                target.modules,
                                parts,
                                index,
                                condition,
                                local_seen,
                                exclude_symbol,
                            )
                            if nested.resolved:
                                return nested
                            inaccessible = inaccessible or nested.inaccessible
                            uncertain_macro = uncertain_macro or nested.uncertain_macro

        symbols = _dedupe_symbols(symbols)
        if not symbols:
            if inaccessible:
                return Resolution(
                    inaccessible=True,
                    missing_segment=segment,
                    detail=f"{segment} is private",
                )
            return Resolution(
                external_unknown=external_glob,
                uncertain_macro=uncertain_macro,
                missing_segment=segment,
                detail=(
                    "name may be provided by an external glob import"
                    if external_glob
                    else "name may be generated by an unexpanded item macro"
                    if uncertain_macro
                    else f"not found in {', '.join(sorted({m.display for m in current_modules}))}"
                ),
            )

        resolved_symbols: list[Symbol] = []
        resolved_modules: list[ModuleUnit] = []
        associated_unknown = False
        details: list[str] = []
        for symbol in symbols:
            if symbol.kind == "module":
                target_path = symbol.target_module_path or symbol.module.path + (
                    segment,
                )
                modules, maybe = self._modules_for(
                    symbol.module.target,
                    target_path,
                    all_of(condition, symbol.condition),
                )
                uncertain_macro = uncertain_macro or maybe
                if index + 1 >= len(parts):
                    resolved_modules.extend(modules)
                elif modules:
                    nested = self._resolve_from_modules(
                        requester,
                        modules,
                        parts,
                        index + 1,
                        condition,
                        seen,
                        exclude_symbol,
                    )
                    resolved_symbols.extend(nested.symbols)
                    resolved_modules.extend(nested.modules)
                    associated_unknown = associated_unknown or nested.associated_unknown
                    uncertain_macro = uncertain_macro or nested.uncertain_macro
                    if nested.detail:
                        details.append(nested.detail)
                continue
            if symbol.kind in {"import", "extern_crate"} and symbol.import_path:
                imported = self.resolve_path(
                    symbol.module,
                    symbol.import_path,
                    condition=all_of(condition, symbol.condition),
                    exclude_symbol=symbol,
                    _seen=seen,
                )
                if index + 1 >= len(parts):
                    resolved_symbols.extend(imported.symbols)
                    resolved_modules.extend(imported.modules)
                    associated_unknown = (
                        associated_unknown or imported.associated_unknown
                    )
                    uncertain_macro = uncertain_macro or imported.uncertain_macro
                    if imported.external_unknown:
                        return imported
                elif imported.modules:
                    nested = self._resolve_from_modules(
                        requester,
                        imported.modules,
                        parts,
                        index + 1,
                        condition,
                        seen,
                        exclude_symbol,
                    )
                    resolved_symbols.extend(nested.symbols)
                    resolved_modules.extend(nested.modules)
                    associated_unknown = associated_unknown or nested.associated_unknown
                    uncertain_macro = uncertain_macro or nested.uncertain_macro
                elif imported.symbols and all(
                    item.kind in {"enum", "struct", "trait", "type", "union"}
                    for item in imported.symbols
                ):
                    resolved_symbols.extend(imported.symbols)
                    associated_unknown = True
                continue
            if index + 1 < len(parts):
                if symbol.kind in {"enum", "struct", "trait", "type", "union"}:
                    resolved_symbols.append(symbol)
                    associated_unknown = True
                else:
                    details.append(f"{segment} is not a module/type")
            else:
                resolved_symbols.append(symbol)

        return Resolution(
            symbols=tuple(_dedupe_symbols(resolved_symbols)),
            modules=tuple(_dedupe_modules(resolved_modules)),
            uncertain_macro=uncertain_macro,
            associated_unknown=associated_unknown,
            missing_segment=""
            if resolved_symbols or resolved_modules or associated_unknown
            else segment,
            detail="; ".join(dict.fromkeys(details)),
        )

    # ---------- checks ----------
    def _check_imports_and_paths(self) -> None:
        for target in self.host.targets:
            for module in target.iter_modules():
                for spec in module.imports:
                    if not spec.path:
                        continue
                    result = self.resolve_path(
                        module,
                        spec.path,
                        condition=spec.condition,
                        exclude_symbol=spec.symbol,
                    )
                    path_text = "::".join(spec.path) + ("::*" if spec.glob else "")
                    if result.inaccessible:
                        self.host.add(
                            "RES002",
                            "error",
                            f"Import reaches a private item: {path_text}",
                            source=module.source,
                            node=spec.node,
                            hint="Import a visible re-export or change the defining visibility.",
                            evidence=(
                                f"cfg: {spec.condition.describe()}",
                                result.detail,
                            ),
                        )
                    elif not result.resolved:
                        uncertain = result.uncertain_macro or result.external_unknown
                        self.host.add(
                            "RES003",
                            "warning" if uncertain else "error",
                            f"Unresolved import: {path_text} ({result.detail or 'no matching symbol'})",
                            source=module.source,
                            node=spec.node,
                            confidence="speculative" if uncertain else "probable",
                            hint=(
                                "The target module contains an unexpanded macro or external glob; confirm with cargo check."
                                if uncertain
                                else "Check the module path, re-export chain, cfg guards, and Cargo dependency declaration."
                            ),
                            evidence=(f"cfg: {spec.condition.describe()}",),
                        )
                self._check_explicit_paths(module)
                self._check_likely_missing_imports(module)
                self._check_unused_imports(module)

    def _walk_module_nodes(self, module: ModuleUnit) -> Iterator[Node]:
        if module.walk_cache is None:
            module.walk_cache = tuple(walk_named(module.body, skip_inline_modules=True))
        return iter(module.walk_cache)

    def _top_level_item(self, module: ModuleUnit, node: Node) -> Node | None:
        current = node
        while current.parent is not None and current.parent != module.body:
            current = current.parent
        return current if current.parent == module.body else None

    def condition_for_node(self, module: ModuleUnit, node: Node) -> CfgExpr:
        item = self._top_level_item(module, node)
        if item is None:
            return module.condition
        return module.item_conditions.get(item.start_byte, module.condition)

    def _check_explicit_paths(self, module: ModuleUnit) -> None:
        for node in self._walk_module_nodes(module):
            if node.type not in {"scoped_identifier", "scoped_type_identifier"}:
                continue
            if node.parent and node.parent.type in {
                "scoped_identifier",
                "scoped_type_identifier",
                "use_declaration",
                "scoped_use_list",
            }:
                continue
            text = module.source.text(node)
            parts = tuple(part for part in re.split(r"\s*::\s*", text) if part)
            if not parts or parts[0] not in {"crate", "self", "super"}:
                continue
            condition = self.condition_for_node(module, node)
            result = self.resolve_path(module, parts, condition=condition)
            if result.inaccessible:
                self.host.add(
                    "RES004",
                    "error",
                    f"Path accesses a private item: {text}",
                    source=module.source,
                    node=node,
                    evidence=(f"cfg: {condition.describe()}", result.detail),
                )
            elif not result.resolved:
                uncertain = result.uncertain_macro or result.external_unknown
                self.host.add(
                    "RES005",
                    "warning" if uncertain else "error",
                    f"Unresolved same-crate path: {text} ({result.detail or 'no matching symbol'})",
                    source=module.source,
                    node=node,
                    confidence="speculative" if uncertain else "probable",
                    evidence=(f"cfg: {condition.describe()}",),
                )

    def _available_names(
        self, module: ModuleUnit, condition: CfgExpr
    ) -> tuple[set[str], bool]:
        names = set(RUST_PRELUDE)
        names.update(BUILTIN_EXTERNAL_ROOTS)
        names.update(module.target.package.dependencies)
        if module.target.kind != "lib":
            names.add(module.target.package.extern_name)
        uncertain = False
        modules, _ = self._modules_for(module.target, module.path, condition)
        for current in modules:
            for name, symbols in current.symbols.items():
                if any(
                    compatibility(symbol.condition, condition) is not False
                    and self._visible(symbol, module)
                    for symbol in symbols
                ):
                    names.add(name)
            if current.unknown_item_macros:
                uncertain = True
            for glob in current.glob_imports:
                exported, maybe = self._exported_names_from_glob(
                    module, glob, condition
                )
                names.update(exported)
                uncertain = uncertain or maybe
        return names, uncertain

    def _exported_names_from_glob(
        self, requester: ModuleUnit, glob: UseSpec, condition: CfgExpr
    ) -> tuple[set[str], bool]:
        path_tuple = tuple(glob.path)
        if path_tuple in PREDEFINED_GLOB_EXPORTS:
            return PREDEFINED_GLOB_EXPORTS[path_tuple].copy(), False

        result = self.resolve_path(
            glob.module,
            glob.path,
            condition=all_of(condition, glob.condition),
            exclude_symbol=glob.symbol,
        )
        if result.external_unknown:
            return set(), True
        names: set[str] = set()
        uncertain = result.uncertain_macro
        seen: set[tuple[str, tuple[str, ...]]] = set()
        stack = list(result.modules)
        while stack:
            module = stack.pop()
            key = (module.target.key, module.path)
            if key in seen:
                continue
            seen.add(key)
            siblings, _ = self._modules_for(module.target, module.path, condition)
            for sibling in siblings:
                for name, symbols in sibling.symbols.items():
                    if any(
                        compatibility(symbol.condition, condition) is not False
                        and self._visible(symbol, requester)
                        for symbol in symbols
                    ):
                        names.add(name)
                if sibling.unknown_item_macros:
                    uncertain = True
                for nested_glob in sibling.glob_imports:
                    nested = self.resolve_path(
                        sibling,
                        nested_glob.path,
                        condition=all_of(condition, nested_glob.condition),
                        exclude_symbol=nested_glob.symbol,
                    )
                    if nested.external_unknown:
                        uncertain = True
                    stack.extend(nested.modules)
        return names, uncertain

    def _generic_names_for_node(self, module: ModuleUnit, node: Node) -> set[str]:
        names: set[str] = set()
        current: Node | None = node
        while current is not None and current != module.body:
            parameters = current.child_by_field_name("type_parameters")
            if parameters is not None:
                text = module.source.text(parameters)
                names.update(re.findall(r"\b[A-Z][A-Za-z0-9_]*\b", text))
            current = current.parent
        return names

    def _is_declaration_name(self, node: Node) -> bool:
        parent = node.parent
        if parent is None:
            return False
        for field in ("name", "alias", "pattern"):
            if parent.child_by_field_name(field) == node:
                return True
        return parent.type in {
            "type_parameter",
            "const_parameter",
            "lifetime",
            "shorthand_field_identifier",
            "field_identifier",
        }

    def _reference_candidates(self, module: ModuleUnit) -> dict[str, tuple[Node, bool]]:
        references: dict[str, tuple[Node, bool]] = {}
        for node in self._walk_module_nodes(module):
            if node.type in {"use_declaration", "attribute_item"}:
                continue
            if node.type in {"scoped_identifier", "scoped_type_identifier"}:
                if node.parent and node.parent.type in {
                    "scoped_identifier",
                    "scoped_type_identifier",
                }:
                    continue
                first = re.split(r"\s*::\s*", module.source.text(node))[0]
                if re.fullmatch(r"[A-Z][A-Za-z0-9_]*", first):
                    references.setdefault(first, (node, False))
            elif node.type == "type_identifier" and not self._is_declaration_name(node):
                name = module.source.text(node)
                if re.fullmatch(r"[A-Z][A-Za-z0-9_]*", name):
                    references.setdefault(name, (node, False))
            elif node.type == "macro_invocation":
                token_tree = next(
                    (
                        child
                        for child in node.named_children
                        if child.type == "token_tree"
                    ),
                    None,
                )
                if token_tree is not None:
                    for match in re.finditer(
                        r"(?<![:A-Za-z0-9_])([A-Z][A-Za-z0-9_]*)\s*::",
                        module.source.text(token_tree),
                    ):
                        references.setdefault(match.group(1), (node, True))
        return references

    def _candidate_symbols(
        self, module: ModuleUnit, name: str, condition: CfgExpr
    ) -> list[Symbol]:
        return [
            symbol
            for symbol in self.symbols_by_name.get((module.target.key, name), [])
            if self._visible(symbol, module)
            and compatibility(symbol.condition, condition) is not False
            and symbol.kind != "import"
        ]

    def _local_names_for_node(self, module: ModuleUnit, node: Node) -> set[str]:
        names: set[str] = set()
        current: Node | None = node
        while current is not None and current != module.body:
            parent = current.parent
            if parent is None:
                break
            if parent.type == "block":
                for sibling in parent.named_children:
                    if sibling.start_byte >= current.start_byte:
                        break
                    if sibling.type == "use_declaration":
                        condition = self.condition_for_node(module, sibling)
                        for spec in self._parse_use_declaration(
                            module, sibling, condition
                        ):
                            if spec.alias:
                                names.add(spec.alias)
                            elif spec.glob:
                                result = self.resolve_path(
                                    module, spec.path, condition=condition
                                )
                                for target_module in result.modules:
                                    exported, _ = self._available_names(
                                        target_module, condition
                                    )
                                    names.update(exported)
                    elif sibling.type in {
                        "struct_item",
                        "enum_item",
                        "type_item",
                        "union_item",
                        "trait_item",
                        "function_item",
                        "const_item",
                        "static_item",
                    }:
                        name_node = sibling.child_by_field_name("name")
                        if name_node is not None:
                            names.add(module.source.text(name_node))
            current = parent
        return names

    def _check_likely_missing_imports(self, module: ModuleUnit) -> None:
        references = self._reference_candidates(module)
        for name, (node, macro_context) in references.items():
            condition = self.condition_for_node(module, node)
            available, uncertain_available = self._available_names(module, condition)
            available.update(self._local_names_for_node(module, node))
            if name in available or name in self._generic_names_for_node(module, node):
                continue
            candidates = _dedupe_symbols(
                self._candidate_symbols(module, name, condition)
            )
            if not candidates:
                continue
            paths = sorted(
                {
                    "crate::" + "::".join((*candidate.module.path, candidate.name))
                    for candidate in candidates
                }
            )
            definite_choice = (
                len(paths) == 1 and not macro_context and not uncertain_available
            )
            self.host.add(
                "RES006",
                "error" if definite_choice else "warning",
                f"Likely missing import for same-crate item {name}",
                source=module.source,
                node=node,
                confidence="probable" if definite_choice else "speculative",
                hint=(
                    f"Consider `use {paths[0]};`."
                    if len(paths) == 1
                    else "Qualify the reference or import one of the candidate paths explicitly."
                ),
                evidence=(
                    f"candidate paths: {', '.join(paths)}",
                    f"cfg: {condition.describe()}",
                    "reference occurs inside macro tokens"
                    if macro_context
                    else "reference occurs in parsed Rust syntax",
                ),
            )

    def _identifier_usage(self, module: ModuleUnit) -> Counter[str]:
        usage: Counter[str] = Counter()
        for node in self._walk_module_nodes(module):
            if node.type == "use_declaration":
                continue
            if node.type in {
                "identifier",
                "type_identifier",
                "field_identifier",
                "shorthand_field_identifier",
            }:
                usage[module.source.text(node)] += 1
            elif node.type in {"token_tree", "attribute_item"}:
                for name in re.findall(
                    r"\b[A-Za-z_][A-Za-z0-9_]*\b", module.source.text(node)
                ):
                    usage[name] += 1
        return usage

    def _downstream_glob_usage(self, module: ModuleUnit) -> Counter[str]:
        usage: Counter[str] = Counter()
        for candidate in module.target.iter_modules():
            if len(candidate.path) <= len(module.path):
                continue
            if candidate.path[: len(module.path)] != module.path:
                continue
            imports_parent = False
            for glob in candidate.glob_imports:
                result = self.resolve_path(
                    candidate,
                    glob.path,
                    condition=glob.condition,
                    exclude_symbol=glob.symbol,
                )
                if any(
                    target.target.key == module.target.key
                    and target.path == module.path
                    for target in result.modules
                ):
                    imports_parent = True
                    break
            if imports_parent:
                usage.update(self._identifier_usage(candidate))
        return usage

    def _check_unused_imports(self, module: ModuleUnit) -> None:
        usage = self._identifier_usage(module)
        usage.update(self._downstream_glob_usage(module))
        for spec in module.imports:
            if spec.visibility.kind != "private":
                continue
            if spec.glob:
                exported, uncertain = self._exported_names_from_glob(
                    module, spec, spec.condition
                )
                if exported & set(usage):
                    continue
                self.host.add(
                    "LINT001",
                    "warning",
                    (
                        f"Unverified glob import usage: {'::'.join(spec.path)}::*"
                        if uncertain
                        else f"Likely unused glob import: {'::'.join(spec.path)}::*"
                    ),
                    source=module.source,
                    node=spec.node,
                    confidence="speculative" if uncertain else "probable",
                    hint="Glob use can hide trait/macro resolution; confirm with rustc before removal.",
                    evidence=(
                        f"resolved exported names: {len(exported)}",
                        "external glob or unexpanded macro prevents a complete proof"
                        if uncertain
                        else "none of the resolved exported names appears outside use declarations",
                    ),
                )
                continue
            alias = spec.alias
            if not alias or alias == "_" or usage[alias] > 0:
                continue
            result = self.resolve_path(
                module,
                spec.path,
                condition=spec.condition,
                exclude_symbol=spec.symbol,
            )
            trait_or_external = result.external_unknown or any(
                symbol.kind == "trait" for symbol in result.symbols
            )
            self.host.add(
                "LINT002",
                "warning",
                f"Likely unused import: {alias}",
                source=module.source,
                node=spec.node,
                confidence="speculative" if trait_or_external else "probable",
                hint=(
                    "This may be a trait import used only by method resolution; confirm with rustc/Clippy."
                    if trait_or_external
                    else "No identifier or macro token references this alias."
                ),
                evidence=(
                    f"import path: {'::'.join(spec.path)}",
                    f"identifier occurrences outside use declarations: {usage[alias]}",
                ),
            )

    # ---------- call arity ----------
    def _pattern_bindings(self, source: SourceFile, pattern: Node | None) -> set[str]:
        if pattern is None:
            return set()
        bindings: set[str] = set()
        for node in walk_named(pattern):
            if node.type not in {
                "identifier",
                "shorthand_field_identifier",
                "mut_pattern",
                "ref_pattern",
            }:
                continue
            text = source.text(node)
            if re.fullmatch(r"[a-z_][A-Za-z0-9_]*", text):
                bindings.add(text)
        if pattern.type == "identifier":
            text = source.text(pattern)
            if re.fullmatch(r"[a-z_][A-Za-z0-9_]*", text):
                bindings.add(text)
        return bindings

    def _local_value_bindings_before(self, module: ModuleUnit, node: Node) -> set[str]:
        bindings: set[str] = set()
        current: Node | None = node
        while current is not None and current != module.body:
            parent = current.parent
            if parent is None:
                break
            if parent.type == "parameters":
                pass
            if parent.type in {"function_item", "closure_expression"}:
                parameters = parent.child_by_field_name("parameters")
                if parameters is not None:
                    for parameter in parameters.named_children:
                        bindings.update(
                            self._pattern_bindings(
                                module.source,
                                parameter.child_by_field_name("pattern"),
                            )
                        )
            if parent.type == "block":
                for sibling in parent.named_children:
                    if sibling.start_byte >= current.start_byte:
                        break
                    if sibling.type == "let_declaration":
                        bindings.update(
                            self._pattern_bindings(
                                module.source,
                                sibling.child_by_field_name("pattern"),
                            )
                        )
            if parent.type in {"for_expression", "match_arm"}:
                bindings.update(
                    self._pattern_bindings(
                        module.source, parent.child_by_field_name("pattern")
                    )
                )
            current = parent
        return bindings

    def _call_path(self, module: ModuleUnit, function: Node) -> tuple[str, ...] | None:
        target = function
        if function.type == "generic_function":
            target = function.child_by_field_name("function") or (
                function.named_children[0] if function.named_children else function
            )
        if target.type not in {
            "identifier",
            "scoped_identifier",
            "scoped_type_identifier",
        }:
            return None
        return tuple(
            part for part in re.split(r"\s*::\s*", module.source.text(target)) if part
        )

    def _function_arity(self, symbol: Symbol) -> tuple[int, int | None] | None:
        parameters = symbol.node.child_by_field_name("parameters")
        if parameters is None:
            return None
        fixed = 0
        variadic = False
        for child in parameters.named_children:
            if child.type == "self_parameter":
                fixed += 1
            elif "variadic" in child.type:
                variadic = True
            else:
                fixed += 1
        return fixed, None if variadic else fixed

    def _check_direct_call_arity(self) -> None:
        for target in self.host.targets:
            for module in target.iter_modules():
                for node in self._walk_module_nodes(module):
                    if node.type != "call_expression":
                        continue
                    function = node.child_by_field_name("function")
                    arguments = node.child_by_field_name("arguments")
                    if function is None or arguments is None:
                        continue
                    path = self._call_path(module, function)
                    if not path:
                        continue
                    if len(path) == 1 and path[0] in self._local_value_bindings_before(
                        module, node
                    ):
                        continue
                    condition = self.condition_for_node(module, node)
                    result = self.resolve_path(module, path, condition=condition)
                    functions = _dedupe_symbols(
                        [
                            symbol
                            for symbol in result.symbols
                            if symbol.kind == "function"
                        ]
                    )
                    if not functions:
                        continue
                    actual = len(arguments.named_children)
                    ranges = [
                        arity
                        for arity in (
                            self._function_arity(symbol) for symbol in functions
                        )
                        if arity is not None
                    ]
                    if not ranges:
                        continue
                    if any(
                        actual >= minimum and (maximum is None or actual <= maximum)
                        for minimum, maximum in ranges
                    ):
                        continue
                    expected = sorted(
                        {
                            f">={minimum}" if maximum is None else str(minimum)
                            for minimum, maximum in ranges
                        }
                    )
                    explicit = len(path) > 1 and path[0] in {"crate", "self", "super"}
                    self.host.add(
                        "TYPE001",
                        "error" if explicit and len(expected) == 1 else "warning",
                        f"Direct call argument count mismatch for {'::'.join(path)}: expected {' or '.join(expected)}, got {actual}",
                        source=module.source,
                        node=node,
                        confidence="definite"
                        if explicit and len(expected) == 1
                        else "probable",
                        hint="Only directly resolved same-crate free functions are checked; methods and function pointers are excluded.",
                        evidence=(
                            f"resolved definitions: {len(functions)}",
                            f"cfg: {condition.describe()}",
                        ),
                    )


def _dedupe_symbols(symbols: Iterable[Symbol]) -> list[Symbol]:
    out: list[Symbol] = []
    seen: set[tuple[str, int, str, str]] = set()
    for symbol in symbols:
        key = (
            str(symbol.module.source.path),
            symbol.node.start_byte,
            symbol.name,
            symbol.kind,
        )
        if key in seen:
            continue
        seen.add(key)
        out.append(symbol)
    return out


def _dedupe_modules(modules: Iterable[ModuleUnit]) -> list[ModuleUnit]:
    out: list[ModuleUnit] = []
    seen: set[tuple[str, tuple[str, ...], str, int]] = set()
    for module in modules:
        key = (
            module.target.key,
            module.path,
            str(module.source.path),
            module.body.start_byte,
        )
        if key in seen:
            continue
        seen.add(key)
        out.append(module)
    return out


def analyze(host: AnalysisHost) -> NameResolver:
    resolver = NameResolver(host)
    resolver.run()
    return resolver
