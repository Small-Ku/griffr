from __future__ import annotations

import re
from pathlib import Path
from typing import Any, Protocol

from tree_sitter import Node

from .models import CrateTarget, ModuleUnit, Package, SourceFile
from .parsing import walk_named


class ArchitectureHost(Protocol):
    targets: list[CrateTarget]
    packages: list[Package]

    def add(self, code: str, severity: str, message: str, **kwargs: Any) -> None: ...


_CALLBACK_TRAIT = re.compile(r"\b(?:Fn|FnMut|FnOnce)\s*(?:\(|<)")
_PROGRESS_NAME = re.compile(
    r"(?:^|_)(?:progress|progress_callback|callback|reporter|observer)(?:$|_)",
    re.IGNORECASE,
)
_RAW_PROGRESS_CHANNEL = re.compile(
    r"(?:flume\s*::\s*)?(?:Sender|Receiver)\s*<\s*(?:[A-Za-z_][A-Za-z0-9_]*\s*::\s*)*ProgressUpdate\b"
)
_RENDERER_CRATES = {"indicatif", "console", "crossterm", "termcolor"}
_PROGRESS_STRUCT_EXPRESSIONS = {"ProgressRoute", "ProgressUpdate::Started"}
_PROGRESS_UNITS = {"ProgressUnit::Items", "ProgressUnit::Bytes"}


def run(host: ArchitectureHost) -> None:
    protocol_packages = _progress_protocol_packages(host)
    for target in host.targets:
        if target.kind != "lib":
            continue
        _check_exported_progress_callbacks(host, target)
        if target.package in protocol_packages:
            _check_raw_channel_exposure(host, target)
    for package in host.packages:
        if package in protocol_packages or package.name == "griffr-common":
            _check_renderer_neutrality(host, package)
    _check_progress_lane_constants(host)
    _check_transient_outcome_leaks(host)
    _check_lane_unit_conflicts(host)
    _check_dispatcher_task_model(host)
    _check_task_enum_construction(host)
    _check_task_match_exhaustiveness(host)
    _check_archive_token_barriers(host)


def _task_enum_shape(
    host: ArchitectureHost,
) -> tuple[set[str], dict[str, set[str]]] | None:
    seen: set[tuple[Path, int]] = set()
    for target in host.targets:
        for module in target.iter_modules():
            for item in module.body.named_children:
                if item.type != "enum_item":
                    continue
                name = item.child_by_field_name("name")
                if name is None or module.source.text(name) != "Task":
                    continue
                coordinate = (module.source.path, item.start_byte)
                if coordinate in seen:
                    continue
                seen.add(coordinate)
                body = item.child_by_field_name("body")
                if body is None:
                    continue
                variants: set[str] = set()
                fields: dict[str, set[str]] = {}
                for variant in body.named_children:
                    if variant.type != "enum_variant":
                        continue
                    variant_name = variant.child_by_field_name("name")
                    if variant_name is None:
                        continue
                    variant_text = module.source.text(variant_name)
                    variants.add(variant_text)
                    field_names: set[str] = set()
                    variant_body = variant.child_by_field_name("body")
                    if variant_body is not None and variant_body.type == "field_declaration_list":
                        for field in variant_body.named_children:
                            if field.type != "field_declaration":
                                continue
                            field_name = field.child_by_field_name("name")
                            if field_name is not None:
                                field_names.add(module.source.text(field_name))
                    fields[variant_text] = field_names
                if variants:
                    return variants, fields
    return None


def _task_variant_from_name(text: str) -> str | None:
    normalized = re.sub(r"\s+", "", text)
    match = re.fullmatch(r"(?:[A-Za-z_][A-Za-z0-9_]*::)*Task::([A-Za-z_][A-Za-z0-9_]*)", normalized)
    return match.group(1) if match else None


def _check_task_enum_construction(host: ArchitectureHost) -> None:
    shape = _task_enum_shape(host)
    if shape is None:
        return
    variants, fields = shape
    seen: set[tuple[Path, int]] = set()
    for target in host.targets:
        for module in target.iter_modules():
            for node in walk_named(module.body):
                if node.type != "struct_expression":
                    continue
                coordinate = (module.source.path, node.start_byte)
                if coordinate in seen:
                    continue
                seen.add(coordinate)
                name = node.child_by_field_name("name")
                body = node.child_by_field_name("body")
                if name is None or body is None:
                    continue
                variant = _task_variant_from_name(module.source.text(name))
                if variant is None:
                    continue
                if variant not in variants:
                    host.add(
                        "DAG002",
                        "error",
                        f"Task constructor references unknown variant {variant}",
                        source=module.source,
                        node=name,
                        confidence="definite",
                        evidence=("constructor path resolves textually to Task::" + variant,),
                        hint="Update the constructor or the canonical Task enum before changing executor routing.",
                    )
                    continue
                provided: set[str] = set()
                has_base_update = False
                for field in body.named_children:
                    if field.type == "field_initializer":
                        field_name = field.child_by_field_name("field")
                        if field_name is not None:
                            provided.add(module.source.text(field_name))
                    elif field.type == "shorthand_field_initializer":
                        identifier = next(
                            (child for child in field.named_children if child.type == "identifier"),
                            None,
                        )
                        if identifier is not None:
                            provided.add(module.source.text(identifier))
                    elif field.type == "base_field_initializer":
                        has_base_update = True
                expected = fields[variant]
                unknown = sorted(provided - expected)
                missing = [] if has_base_update else sorted(expected - provided)
                if not unknown and not missing:
                    continue
                details = []
                if missing:
                    details.append(f"missing fields {missing!r}")
                if unknown:
                    details.append(f"unknown fields {unknown!r}")
                host.add(
                    "DAG002",
                    "error",
                    f"Task::{variant} constructor has " + " and ".join(details),
                    source=module.source,
                    node=node,
                    confidence="definite",
                    evidence=(
                        f"canonical fields: {sorted(expected)!r}",
                        f"provided fields: {sorted(provided)!r}",
                    ),
                    hint="Keep Task enum payloads and every constructor synchronized; rustc remains authoritative for type checking.",
                )


def _check_task_match_exhaustiveness(host: ArchitectureHost) -> None:
    shape = _task_enum_shape(host)
    if shape is None:
        return
    variants, _ = shape
    seen: set[tuple[Path, int]] = set()
    pattern_re = re.compile(r"\bTask\s*::\s*([A-Za-z_][A-Za-z0-9_]*)")
    for target in host.targets:
        for module in target.iter_modules():
            for node in walk_named(module.body):
                if node.type != "match_expression":
                    continue
                coordinate = (module.source.path, node.start_byte)
                if coordinate in seen:
                    continue
                seen.add(coordinate)
                body = node.child_by_field_name("body")
                if body is None:
                    continue
                covered: set[str] = set()
                catch_all = False
                task_pattern_arms = 0
                for arm in body.named_children:
                    if arm.type != "match_arm":
                        continue
                    pattern = arm.child_by_field_name("pattern")
                    if pattern is None:
                        continue
                    pattern_text = module.source.text(pattern)
                    arm_variants = set(pattern_re.findall(pattern_text))
                    if arm_variants:
                        task_pattern_arms += 1
                        covered.update(arm_variants)
                    else:
                        catch_all = True
                if catch_all or task_pattern_arms < 2 or not covered.issubset(variants):
                    continue
                missing = sorted(variants - covered)
                if not missing:
                    continue
                host.add(
                    "DAG001",
                    "error",
                    f"Task match is missing variants {missing!r}",
                    source=module.source,
                    node=node,
                    confidence="definite",
                    evidence=(
                        f"covered variants: {sorted(covered)!r}",
                        "no wildcard or binding catch-all arm was found",
                    ),
                    hint="Update executor, resource routing, path, estimate, and execution-class matches whenever Task gains a variant.",
                )


def _check_archive_token_barriers(host: ArchitectureHost) -> None:
    """Keep range-local extraction separate from the all-volume commit barrier."""
    required_methods = {
        "CommitArchive": {"add_root_with_tokens", "add_task_with_tokens"},
        "ExtractArchiveShard": {"add_root_with_tokens", "add_task_with_tokens"},
    }
    seen: set[tuple[Path, int]] = set()
    for target in host.targets:
        for module in target.iter_modules():
            for node in walk_named(module.body):
                if node.type != "struct_expression":
                    continue
                coordinate = (module.source.path, node.start_byte)
                if coordinate in seen:
                    continue
                seen.add(coordinate)
                name = node.child_by_field_name("name")
                if name is None:
                    continue
                variant = _task_variant_from_name(module.source.text(name))
                allowed = required_methods.get(variant or "")
                if allowed is None:
                    continue

                current = node.parent
                insertion = None
                while current is not None:
                    if current.type == "call_expression":
                        function = current.child_by_field_name("function")
                        if function is not None:
                            function_text = re.sub(r"\s+", "", module.source.text(function))
                            insertion = function_text.rsplit(".", 1)[-1]
                        break
                    current = current.parent
                if insertion in allowed:
                    continue
                host.add(
                    "DAG003",
                    "error",
                    f"Task::{variant} must be inserted through a token-aware graph API",
                    source=module.source,
                    node=node,
                    confidence="definite",
                    evidence=(
                        f"nearest insertion call: {insertion or 'none'}",
                        f"allowed methods: {sorted(allowed)!r}",
                    ),
                    hint=(
                        "Use add_root_with_tokens/add_task_with_tokens so extraction waits for "
                        "its source ranges and commit also joins every package-part token."
                    ),
                )


def _progress_protocol_packages(host: ArchitectureHost) -> list[Package]:
    found: list[Package] = []
    for package in host.packages:
        has_sender = False
        has_update = False
        for target in host.targets:
            if target.package is not package:
                continue
            for module in target.iter_modules():
                text = module.source.text(module.body)
                has_sender = has_sender or bool(
                    re.search(r"\bpub\s+struct\s+ProgressSender\b", text)
                )
                has_update = has_update or bool(
                    re.search(r"\bpub\s+enum\s+ProgressUpdate\b", text)
                )
        if has_sender and has_update:
            found.append(package)
    return found


def _visibility_text(source: SourceFile, node: Node) -> str:
    for child in node.named_children:
        if child.type == "visibility_modifier":
            return source.text(child).strip()
    return ""


def _is_public(source: SourceFile, node: Node) -> bool:
    return _visibility_text(source, node) == "pub"


def _module_is_exported(module: ModuleUnit) -> bool:
    current = module
    while current.parent is not None:
        declaration = current.declaration_node
        if declaration is not None and not _is_public(
            current.parent.source, declaration
        ):
            return False
        current = current.parent
    return True


def _public_type_names(module: ModuleUnit) -> set[str]:
    names: set[str] = set()
    for item in module.body.named_children:
        if item.type not in {
            "struct_item",
            "enum_item",
            "union_item",
            "type_item",
            "trait_item",
        } or not _is_public(module.source, item):
            continue
        name = item.child_by_field_name("name")
        if name is not None:
            names.add(module.source.text(name))
    return names


def _impl_public_type(
    module: ModuleUnit, impl_item: Node, public_types: set[str]
) -> bool:
    type_node = impl_item.child_by_field_name("type")
    if type_node is None:
        return False
    identifiers = re.findall(r"[A-Za-z_][A-Za-z0-9_]*", module.source.text(type_node))
    return bool(identifiers and identifiers[-1] in public_types)


def _callable_name(source: SourceFile, node: Node) -> str:
    name = node.child_by_field_name("name")
    return source.text(name) if name is not None else "<anonymous>"


def _signature_text(source: SourceFile, node: Node) -> str:
    body = node.child_by_field_name("body")
    end = body.start_byte if body is not None else node.end_byte
    return source.data[node.start_byte : end].decode("utf-8", "replace")


def _parameter_uses_callback_bound(
    source: SourceFile, callable_node: Node, parameter: Node
) -> bool:
    type_node = parameter.child_by_field_name("type")
    if type_node is None:
        return False
    type_text = source.text(type_node)
    if _CALLBACK_TRAIT.search(type_text):
        return True
    if not re.fullmatch(r"[A-Za-z_][A-Za-z0-9_]*", type_text.strip()):
        return False
    type_name = re.escape(type_text.strip())
    signature = _signature_text(source, callable_node)
    return bool(
        re.search(
            rf"\b{type_name}\s*:\s*[^,>{{;]*\b(?:Fn|FnMut|FnOnce)\s*(?:\(|<)",
            signature,
        )
        or re.search(
            rf"\bwhere\b[\s\S]*?\b{type_name}\s*:\s*[^,{{;]*\b(?:Fn|FnMut|FnOnce)\s*(?:\(|<)",
            signature,
        )
    )


def _parameter_name(source: SourceFile, parameter: Node) -> str:
    pattern = parameter.child_by_field_name("pattern")
    if pattern is None:
        return ""
    identifiers = re.findall(r"[A-Za-z_][A-Za-z0-9_]*", source.text(pattern))
    return identifiers[-1] if identifiers else ""


def _check_callable_callback(
    host: ArchitectureHost,
    module: ModuleUnit,
    callable_node: Node,
    *,
    implicitly_public: bool = False,
) -> None:
    if not implicitly_public and not _is_public(module.source, callable_node):
        return
    name = _callable_name(module.source, callable_node)
    function_is_progress = bool(_PROGRESS_NAME.search(name))
    parameters = callable_node.child_by_field_name("parameters")
    if parameters is None:
        return
    for parameter in parameters.named_children:
        if parameter.type != "parameter":
            continue
        parameter_name = _parameter_name(module.source, parameter)
        if not (function_is_progress or _PROGRESS_NAME.search(parameter_name)):
            continue
        if not _parameter_uses_callback_bound(module.source, callable_node, parameter):
            continue
        host.add(
            "PRG001",
            "warning",
            f"Exported progress API {_callable_name(module.source, callable_node)!r} exposes a callable callback",
            source=module.source,
            node=parameter,
            confidence="definite",
            evidence=(
                f"parameter {parameter_name!r} is bounded by Fn/FnMut/FnOnce",
                "the containing module and callable are externally exported",
            ),
            hint="Emit frontend-neutral typed progress through a cloneable sender; keep mutable callbacks inside crate-private synchronous helpers.",
        )


def _check_exported_progress_callbacks(
    host: ArchitectureHost, target: CrateTarget
) -> None:
    seen: set[tuple[Path, int]] = set()
    for module in target.iter_modules():
        if not _module_is_exported(module):
            continue
        public_types = _public_type_names(module)
        for item in module.body.named_children:
            if item.type == "function_item":
                key = (module.source.path, item.start_byte)
                if key not in seen:
                    seen.add(key)
                    _check_callable_callback(host, module, item)
                continue
            if item.type == "impl_item" and _impl_public_type(
                module, item, public_types
            ):
                body = item.child_by_field_name("body")
                if body is None:
                    continue
                for member in body.named_children:
                    if member.type != "function_item":
                        continue
                    key = (module.source.path, member.start_byte)
                    if key not in seen:
                        seen.add(key)
                        _check_callable_callback(host, module, member)
                continue
            if item.type == "trait_item" and _is_public(module.source, item):
                body = item.child_by_field_name("body")
                if body is None:
                    continue
                for member in body.named_children:
                    if member.type not in {"function_item", "function_signature_item"}:
                        continue
                    key = (module.source.path, member.start_byte)
                    if key not in seen:
                        seen.add(key)
                        _check_callable_callback(
                            host, module, member, implicitly_public=True
                        )


def _normal_dependency_names(package: Package) -> set[str]:
    names: set[str] = set()
    seen: set[int] = set()
    for dependency in package.dependencies.values():
        identity = id(dependency)
        if identity in seen or "normal" not in dependency.kinds:
            continue
        seen.add(identity)
        names.add(dependency.package_name.replace("-", "_").lower())
        names.add(dependency.extern_name.replace("-", "_").lower())
    return names


def _check_renderer_neutrality(host: ArchitectureHost, package: Package) -> None:
    renderer_dependencies = sorted(
        _RENDERER_CRATES.intersection(_normal_dependency_names(package))
    )
    for dependency in renderer_dependencies:
        host.add(
            "PRG004",
            "warning",
            f"Progress protocol package {package.name!r} depends on renderer crate {dependency!r}",
            path=package.manifest_path,
            confidence="definite",
            evidence=(
                "this package exports both ProgressSender and ProgressUpdate",
                f"{dependency!r} is a normal dependency",
            ),
            hint="Keep terminal/GUI rendering in frontend crates and leave the protocol-owner crate frontend-neutral.",
        )


def _contains_raw_channel(source: SourceFile, node: Node) -> bool:
    return bool(_RAW_PROGRESS_CHANNEL.search(_signature_text(source, node)))


def _check_raw_channel_exposure(host: ArchitectureHost, target: CrateTarget) -> None:
    seen: set[tuple[Path, int]] = set()
    for module in target.iter_modules():
        if not _module_is_exported(module):
            continue
        public_types = _public_type_names(module)
        for item in module.body.named_children:
            candidates: list[Node] = []
            if item.type == "function_item" and _is_public(module.source, item):
                candidates.append(item)
            elif item.type in {"type_item", "enum_item"} and _is_public(
                module.source, item
            ):
                candidates.append(item)
            elif item.type == "struct_item" and _is_public(module.source, item):
                body = item.child_by_field_name("body")
                if body is not None:
                    candidates.extend(
                        field
                        for field in body.named_children
                        if field.type == "field_declaration"
                        and _is_public(module.source, field)
                    )
            elif item.type == "impl_item" and _impl_public_type(
                module, item, public_types
            ):
                body = item.child_by_field_name("body")
                if body is not None:
                    candidates.extend(
                        member
                        for member in body.named_children
                        if member.type == "function_item"
                        and _is_public(module.source, member)
                    )
            elif item.type == "trait_item" and _is_public(module.source, item):
                body = item.child_by_field_name("body")
                if body is not None:
                    candidates.extend(
                        member
                        for member in body.named_children
                        if member.type in {"function_item", "function_signature_item"}
                    )

            for candidate in candidates:
                key = (module.source.path, candidate.start_byte)
                if key in seen or not _contains_raw_channel(module.source, candidate):
                    continue
                seen.add(key)
                host.add(
                    "PRG005",
                    "warning",
                    "Raw progress channel type is exposed through a public API",
                    source=module.source,
                    node=candidate,
                    confidence="definite",
                    evidence=(
                        "public signature contains Sender<ProgressUpdate> or Receiver<ProgressUpdate>",
                    ),
                    hint="Expose the repository's ProgressSender/ProgressReceiver newtypes so the transport remains encapsulated.",
                )


def _field_values(source: SourceFile, expression: Node) -> dict[str, tuple[str, Node]]:
    body = expression.child_by_field_name("body")
    if body is None:
        return {}
    values: dict[str, tuple[str, Node]] = {}
    for field in body.named_children:
        if field.type == "shorthand_field_initializer":
            identifier = next(
                (child for child in field.named_children if child.type == "identifier"),
                None,
            )
            if identifier is not None:
                name = source.text(identifier)
                values[name] = (name, identifier)
            continue
        if field.type != "field_initializer":
            continue
        field_name = field.child_by_field_name("field")
        value = field.child_by_field_name("value")
        if field_name is None or value is None:
            continue
        values[source.text(field_name)] = (source.text(value).strip(), value)
    return values


def _enclosing_callable(node: Node) -> Node | None:
    current = node.parent
    while current is not None:
        if current.type in {
            "function_item",
            "function_signature_item",
            "closure_expression",
        }:
            return current
        current = current.parent
    return None


def _lane_key(
    source: SourceFile, expression: Node, lane: str
) -> tuple[str, str, int] | None:
    normalized = re.sub(r"\s+", "", lane)
    if "ProgressLane::" in normalized:
        return ("constant", normalized, 0)
    if re.fullmatch(r"[A-Za-z_][A-Za-z0-9_]*", normalized):
        callable_node = _enclosing_callable(expression)
        if callable_node is None:
            return None
        return (source.rel, normalized, callable_node.start_byte)
    return None


def _check_progress_lane_constants(host: ArchitectureHost) -> None:
    seen: set[tuple[Path, int]] = set()
    for target in host.targets:
        for module in target.iter_modules():
            for node in walk_named(module.body):
                if node.type != "call_expression":
                    continue
                coordinate = (module.source.path, node.start_byte)
                if coordinate in seen:
                    continue
                seen.add(coordinate)
                function = node.child_by_field_name("function")
                if function is None:
                    continue
                function_text = re.sub(r"\s+", "", module.source.text(function))
                if not function_text.endswith("ProgressLane::new"):
                    continue
                host.add(
                    "PRG002",
                    "warning",
                    "Progress lane is constructed outside the shared lane catalog",
                    source=module.source,
                    node=node,
                    confidence="definite",
                    evidence=(f"call uses {function_text}(...) directly",),
                    hint="Add a named ProgressLane constant in the protocol module and use that constant from producers and renderers.",
                )


_TRANSIENT_EVENT_VARIANTS = {
    "DownloadStarted",
    "DownloadedBytes",
    "Retried",
    "ExtractedBytes",
    "ArchiveCommitProgress",
    "PatchProgress",
    "DeleteProgress",
}


def _enum_variants(source: SourceFile, enum_item: Node) -> set[str]:
    body = enum_item.child_by_field_name("body")
    if body is None:
        return set()
    variants: set[str] = set()
    for variant in body.named_children:
        if variant.type != "enum_variant":
            continue
        name = variant.child_by_field_name("name")
        if name is not None:
            variants.add(source.text(name))
    return variants


def _check_transient_outcome_leaks(host: ArchitectureHost) -> None:
    transient_event_types: dict[str, tuple[set[str], SourceFile, Node]] = {}
    for target in host.targets:
        for module in target.iter_modules():
            for item in module.body.named_children:
                if item.type != "enum_item":
                    continue
                name = item.child_by_field_name("name")
                if name is None:
                    continue
                variants = _enum_variants(module.source, item)
                transient = variants.intersection(_TRANSIENT_EVENT_VARIANTS)
                if transient:
                    transient_event_types[module.source.text(name)] = (
                        transient,
                        module.source,
                        item,
                    )

    if not transient_event_types:
        return

    seen: set[tuple[Path, int]] = set()
    for target in host.targets:
        if target.kind != "lib":
            continue
        for module in target.iter_modules():
            if not _module_is_exported(module):
                continue
            for item in module.body.named_children:
                if item.type != "struct_item" or not _is_public(module.source, item):
                    continue
                name = item.child_by_field_name("name")
                if name is None or module.source.text(name) != "TaskPoolResult":
                    continue
                body = item.child_by_field_name("body")
                if body is None:
                    continue
                for field in body.named_children:
                    if field.type != "field_declaration" or not _is_public(
                        module.source, field
                    ):
                        continue
                    coordinate = (module.source.path, field.start_byte)
                    if coordinate in seen:
                        continue
                    type_node = field.child_by_field_name("type")
                    if type_node is None:
                        continue
                    type_text = module.source.text(type_node)
                    leaked = next(
                        (
                            (event_type, data)
                            for event_type, data in transient_event_types.items()
                            if re.search(rf"\b{re.escape(event_type)}\b", type_text)
                        ),
                        None,
                    )
                    if leaked is None:
                        continue
                    seen.add(coordinate)
                    event_type, (variants, event_source, event_node) = leaked
                    event_line, _ = event_source.location(event_node)
                    host.add(
                        "PRG003",
                        "warning",
                        "TaskPoolResult exposes transient worker progress events",
                        source=module.source,
                        node=field,
                        confidence="definite",
                        evidence=(
                            f"public result field contains {event_type}",
                            f"{event_type} has transient variants {sorted(variants)!r} at {event_source.rel}:{event_line}",
                        ),
                        hint="Reduce worker events into frontend progress updates and retain only durable TaskOutcome values in TaskPoolResult.",
                    )


def _check_lane_unit_conflicts(host: ArchitectureHost) -> None:
    mappings: dict[tuple[str, str, int], tuple[str, SourceFile, Node, str]] = {}
    seen_nodes: set[tuple[Path, int]] = set()
    for target in host.targets:
        for module in target.iter_modules():
            for node in walk_named(module.body):
                if node.type != "struct_expression":
                    continue
                coordinate = (module.source.path, node.start_byte)
                if coordinate in seen_nodes:
                    continue
                seen_nodes.add(coordinate)
                name = node.child_by_field_name("name")
                if name is None:
                    continue
                expression_name = module.source.text(name).replace(" ", "")
                if expression_name not in _PROGRESS_STRUCT_EXPRESSIONS:
                    continue
                fields = _field_values(module.source, node)
                lane_entry = fields.get("lane")
                unit_entry = fields.get("unit")
                if lane_entry is None or unit_entry is None:
                    continue
                lane, _ = lane_entry
                unit, unit_node = unit_entry
                unit = re.sub(r"\s+", "", unit)
                if unit not in _PROGRESS_UNITS:
                    continue
                key = _lane_key(module.source, node, lane)
                if key is None:
                    continue
                previous = mappings.get(key)
                if previous is None:
                    mappings[key] = (unit, module.source, unit_node, expression_name)
                    continue
                previous_unit, previous_source, previous_node, previous_expression = (
                    previous
                )
                if previous_unit == unit:
                    continue
                previous_line, _ = previous_source.location(previous_node)
                host.add(
                    "PRG006",
                    "warning",
                    f"Progress lane {lane!r} is assigned conflicting units",
                    source=module.source,
                    node=unit_node,
                    confidence="definite",
                    evidence=(
                        f"{previous_expression} uses {previous_unit} at {previous_source.rel}:{previous_line}",
                        f"{expression_name} uses {unit}",
                    ),
                    hint="Use one stable unit per lane and create a distinct lane for count- and byte-based progress.",
                )


_TASK_POOL_FORBIDDEN_PATTERNS = (
    (re.compile(r"\bstd\s*::\s*thread\s*::\s*Builder\b"), "custom std::thread worker construction"),
    (re.compile(r"\bCondvar\b"), "Condvar-backed task queue"),
    (re.compile(r"\bfn\s+worker_loop\b"), "class-specific worker loop"),
    (re.compile(r"\bfn\s+dispatch_io\b"), "synchronous Dispatcher bridge"),
    (re.compile(r"\bExecutionClass\s*::\s*Network\b"), "thread-oriented network execution class"),
    (re.compile(r"\b(?:cpu|blocking)_workers\b"), "worker-count configuration"),
)


def _check_dispatcher_task_model(host: ArchitectureHost) -> None:
    """Keep task-pool concurrency in Dispatcher plus coordinator admissions."""
    seen_sources: set[Path] = set()
    for target in host.targets:
        for module in target.iter_modules():
            source = module.source
            if source.path in seen_sources:
                continue
            seen_sources.add(source.path)
            normalized = source.rel.replace("\\", "/")
            if "/runtime/task_pool/" not in f"/{normalized}" and not normalized.endswith(
                "/runtime/task_pool.rs"
            ):
                continue
            text = source.data.decode("utf-8", "replace")
            for pattern, description in _TASK_POOL_FORBIDDEN_PATTERNS:
                for match in pattern.finditer(text):
                    start = len(text[: match.start()].encode("utf-8"))
                    end = len(text[: match.end()].encode("utf-8"))
                    node = source.tree.root_node.descendant_for_byte_range(start, end)
                    host.add(
                        "DSP001",
                        "error",
                        f"Task pool reintroduces {description}",
                        source=source,
                        node=node,
                        confidence="definite",
                        evidence=(f"matched {match.group(0)!r}",),
                        hint=(
                            "Submit async I/O with Dispatcher::dispatch, CPU/blocking work with "
                            "Dispatcher::dispatch_blocking, and keep limits as coordinator admissions."
                        ),
                    )
