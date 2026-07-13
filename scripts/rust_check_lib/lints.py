from __future__ import annotations

import re
from typing import Any, Protocol

from tree_sitter import Node

from .cfg import compatibility
from .models import CrateTarget, ModuleUnit
from .module_graph import preceding_attributes
from .name_resolution import NameResolver
from .parsing import walk_named


class LintHost(Protocol):
    targets: list[CrateTarget]

    def add(self, code: str, severity: str, message: str, **kwargs: Any) -> None: ...


def run(host: LintHost, resolver: NameResolver) -> None:
    for target in host.targets:
        for module in target.iter_modules():
            _check_enum_variant_names(host, resolver, module)
            _check_derivable_default(host, resolver, module)
            _check_manual_is_multiple_of(host, resolver, module)
            _check_collapsible_match(host, resolver, module)
            # rustfmt is authoritative for import ordering; the lexical fallback
            # produced unstable false positives across valid rustfmt groupings.
            # _check_import_sorting(host, module)


def _item_attributes(module: ModuleUnit, item: Node) -> tuple[str, ...]:
    children = list(module.body.named_children)
    try:
        index = children.index(item)
    except ValueError:
        return ()
    return preceding_attributes(module, children, index)


def _lint_allowed(module: ModuleUnit, item: Node, lint: str) -> bool:
    texts = [
        module.source.text(node)
        for node in module.body.named_children
        if node.type == "attribute_item"
    ]
    texts.extend(module.attributes)
    texts.extend(_item_attributes(module, item))
    pattern = re.compile(
        rf"(?:allow|expect)\s*\([^)]*\b(?:clippy::)?{re.escape(lint)}\b"
    )
    return any(pattern.search(text) for text in texts)


def _check_enum_variant_names(
    host: LintHost, resolver: NameResolver, module: ModuleUnit
) -> None:
    for node in module.body.named_children:
        if node.type != "enum_item" or _lint_allowed(
            module, node, "enum_variant_names"
        ):
            continue
        name_node = node.child_by_field_name("name")
        body = node.child_by_field_name("body") or next(
            (
                child
                for child in node.named_children
                if child.type == "enum_variant_list"
            ),
            None,
        )
        if name_node is None or body is None:
            continue
        enum_name = module.source.text(name_node)
        variants: list[str] = []
        for variant in body.named_children:
            if variant.type != "enum_variant":
                continue
            variant_name = variant.child_by_field_name("name")
            if variant_name is not None:
                variants.append(module.source.text(variant_name))
        offenders = [
            variant
            for variant in variants
            if variant != enum_name
            and (variant.startswith(enum_name) or variant.endswith(enum_name))
        ]
        if offenders:
            host.add(
                "CLP001",
                "warning",
                f"Enum variants repeat the enum name ({enum_name}): {', '.join(offenders)}",
                source=module.source,
                node=node,
                confidence="probable",
                hint="Rename the variants or add a narrowly scoped #[allow(clippy::enum_variant_names)].",
                evidence=("AST enum and variant names were compared directly.",),
            )


def _tail_expression(block: Node) -> Node | None:
    named = [child for child in block.named_children if child.type != "attribute_item"]
    if len(named) != 1:
        return None
    node = named[0]
    if node.type == "expression_statement":
        if node.child_count and any(child.type == ";" for child in node.children):
            return None
        return node.named_children[0] if node.named_children else None
    if node.type == "return_expression":
        return node.named_children[0] if len(node.named_children) == 1 else None
    return node


def _unit_variant(enum_node: Node, source_text, variant_name: str) -> bool:
    body = enum_node.child_by_field_name("body") or next(
        (
            child
            for child in enum_node.named_children
            if child.type == "enum_variant_list"
        ),
        None,
    )
    if body is None:
        return False
    for variant in body.named_children:
        if variant.type != "enum_variant":
            continue
        name = variant.child_by_field_name("name")
        if name is None or source_text(name) != variant_name:
            continue
        return not any(
            child.type in {"field_declaration_list", "ordered_field_declaration_list"}
            for child in variant.named_children
        )
    return False


def _check_derivable_default(
    host: LintHost, resolver: NameResolver, module: ModuleUnit
) -> None:
    enums: dict[str, list[Node]] = {}
    for node in module.body.named_children:
        if node.type != "enum_item":
            continue
        name = node.child_by_field_name("name")
        if name is not None:
            enums.setdefault(module.source.text(name), []).append(node)

    for impl in module.body.named_children:
        if impl.type != "impl_item" or _lint_allowed(module, impl, "derivable_impls"):
            continue
        trait_node = impl.child_by_field_name("trait")
        type_node = impl.child_by_field_name("type")
        if trait_node is None or type_node is None:
            continue
        if module.source.text(trait_node).split("::")[-1] != "Default":
            continue
        type_name = module.source.text(type_node).strip()
        if not re.fullmatch(r"[A-Za-z_][A-Za-z0-9_]*", type_name):
            continue
        enum_nodes = enums.get(type_name, [])
        if not enum_nodes:
            continue
        impl_condition = resolver.condition_for_node(module, impl)
        enum_node = next(
            (
                enum
                for enum in enum_nodes
                if compatibility(
                    resolver.condition_for_node(module, enum), impl_condition
                )
                is not False
            ),
            None,
        )
        if enum_node is None:
            continue
        attrs = "\n".join(_item_attributes(module, enum_node))
        if re.search(r"derive\s*\([^)]*\bDefault\b", attrs):
            continue
        body = impl.child_by_field_name("body") or next(
            (
                child
                for child in impl.named_children
                if child.type == "declaration_list"
            ),
            None,
        )
        if body is None:
            continue
        default_fn = None
        for child in body.named_children:
            if child.type != "function_item":
                continue
            name = child.child_by_field_name("name")
            if name is not None and module.source.text(name) == "default":
                default_fn = child
                break
        if default_fn is None:
            continue
        fn_body = default_fn.child_by_field_name("body")
        if fn_body is None:
            continue
        expression = _tail_expression(fn_body)
        if expression is None or expression.type not in {
            "scoped_identifier",
            "scoped_type_identifier",
        }:
            continue
        text = re.sub(r"\s+", "", module.source.text(expression))
        match = re.fullmatch(
            rf"(?:Self|{re.escape(type_name)})::([A-Za-z_][A-Za-z0-9_]*)", text
        )
        if not match:
            continue
        variant = match.group(1)
        if not _unit_variant(enum_node, module.source.text, variant):
            continue
        host.add(
            "CLP002",
            "warning",
            f"Manual Default impl for enum {type_name} appears derivable; default variant is {variant}",
            source=module.source,
            node=impl,
            confidence="probable",
            hint=f"Derive Default and mark {variant} with #[default], then confirm with Clippy.",
            evidence=(
                "default() contains exactly one unit-variant expression",
                f"cfg: {impl_condition.describe()}",
            ),
        )


def _operator(node: Node, module: ModuleUnit) -> str:
    operator = node.child_by_field_name("operator")
    if operator is not None:
        return module.source.text(operator)
    for child in node.children:
        if not child.is_named and child.type in {"==", "!=", "%"}:
            return child.type
    return ""


def _integer_value(module: ModuleUnit, node: Node) -> int | None:
    if node.type != "integer_literal":
        return None
    text = module.source.text(node).replace("_", "")
    text = re.sub(r"(?:u|i)(?:8|16|32|64|128|size)$", "", text)
    try:
        return int(text, 0)
    except ValueError:
        return None


def _check_manual_is_multiple_of(
    host: LintHost, resolver: NameResolver, module: ModuleUnit
) -> None:
    for node in walk_named(module.body, skip_inline_modules=True):
        if node.type != "binary_expression" or _operator(node, module) not in {
            "==",
            "!=",
        }:
            continue
        right = node.child_by_field_name("right")
        left = node.child_by_field_name("left")
        if right is None or left is None or _integer_value(module, right) != 0:
            continue
        if left.type != "binary_expression" or _operator(left, module) != "%":
            continue
        divisor = left.child_by_field_name("right")
        expression = left.child_by_field_name("left")
        if (
            divisor is None
            or expression is None
            or _integer_value(module, divisor) is None
        ):
            continue
        host.add(
            "CLP003",
            "warning",
            f"Manual multiple-of test: {module.source.text(node)}",
            source=module.source,
            node=node,
            confidence="speculative",
            hint="Clippy may suggest is_multiple_of depending on the operand types and configured Rust version.",
            evidence=(
                "AST shape is `(expression % integer) == 0` or `!= 0`.",
                f"cfg: {resolver.condition_for_node(module, node).describe()}",
            ),
        )


def _check_import_sorting(host: LintHost, module: ModuleUnit) -> None:
    uses = [
        node for node in module.body.named_children if node.type == "use_declaration"
    ]
    groups: list[list[Node]] = []
    current: list[Node] = []
    previous: Node | None = None
    for node in uses:
        if previous is None:
            current = [node]
        else:
            gap = module.source.data[previous.end_byte : node.start_byte].decode(
                "utf-8", "replace"
            )
            same_group = not re.search(r"\n[ \t]*\n", gap) and not re.search(
                r"//|/\*|#\[", gap
            )
            if same_group:
                current.append(node)
            else:
                if len(current) > 1:
                    groups.append(current)
                current = [node]
        previous = node
    if len(current) > 1:
        groups.append(current)

    for group in groups:
        spellings = [re.sub(r"\s+", "", module.source.text(node)) for node in group]
        roots = [
            re.sub(r"^(?:pub(?:\([^)]*\))?)?use", "", spelling).split("::", 1)[0]
            for spelling in spellings
        ]
        # rustfmt does not promise to reorder across semantic root groups.
        if len(set(roots)) != 1:
            continue
        if spellings == sorted(spellings, key=str.casefold):
            continue
        host.add(
            "FMT007",
            "note",
            "Adjacent same-root import declarations are not in simple lexical order",
            source=module.source,
            node=group[0],
            confidence="speculative",
            hint="rustfmt is authoritative; this fallback reports only a same-root lexical mismatch.",
            evidence=(
                f"observed: {' | '.join(spellings)}",
                f"lexical: {' | '.join(sorted(spellings, key=str.casefold))}",
            ),
        )


def _check_collapsible_match(
    host: LintHost, resolver: NameResolver, module: ModuleUnit
) -> None:
    for node in walk_named(module.body, skip_inline_modules=True):
        if node.type != "match_expression":
            continue
        body = node.child_by_field_name("body")
        if body is None or body.type != "match_block":
            continue
        for arm in body.named_children:
            if arm.type != "match_arm":
                continue
            
            pattern = arm.child_by_field_name("pattern")
            if pattern is None:
                continue
            
            if any(c.type == "if" for c in pattern.children):
                continue
                
            value = arm.child_by_field_name("value")
            if value is None:
                continue
                
            if value.type != "block":
                continue
                
            statements = [
                c for c in value.named_children
                if c.type not in {"attribute_item", "line_comment", "block_comment"}
            ]
            if len(statements) != 1:
                continue
                
            statement = statements[0]
            if_node = None
            if statement.type == "if_expression":
                if_node = statement
            elif statement.type == "expression_statement" and len(statement.named_children) == 1:
                child = statement.named_children[0]
                if child.type == "if_expression":
                    if_node = child
                    
            if if_node is None:
                continue

            cond = if_node.child_by_field_name("condition")
            if cond is not None and cond.type == "let_condition":
                continue
                
            if if_node.child_by_field_name("alternative") is not None:
                continue
                
            host.add(
                "CLP004",
                "warning",
                "This `if` statement inside the match arm can be collapsed into the outer `match` using a match guard",
                source=module.source,
                node=if_node,
                confidence="probable",
                hint="Collapse the nested if block into a match guard on the match arm (e.g., `Pattern if condition => { ... }`).",
                evidence=(
                    "Match arm contains a block with a single `if` statement and no `else` block.",
                    f"cfg: {resolver.condition_for_node(module, if_node).describe()}",
                ),
            )

