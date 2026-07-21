from __future__ import annotations

import re
from typing import Any, Protocol

from tree_sitter import Node

from .cfg import compatibility
from .records import CrateTarget, ModuleUnit
from .module_graph import preceding_attributes
from .name_resolution import NameResolver
from .parsing import walk_named


class LintHost(Protocol):
    targets: list[CrateTarget]

    def add(self, code: str, severity: str, message: str, **kwargs: Any) -> None: ...

    def add_fix(self, code: str, description: str, **kwargs: Any) -> None: ...


def run(host: LintHost, resolver: NameResolver) -> None:
    for target in host.targets:
        for module in target.iter_modules():
            _check_enum_variant_names(host, resolver, module)
            _check_derivable_default(host, resolver, module)
            _check_manual_is_multiple_of(host, resolver, module)
            _check_collapsible_match(host, resolver, module)
            _check_items_after_test_module(host, module)
            _check_useless_chain_into_iter(host, module)
            _check_needless_option_as_deref_mut(host, module)
            _check_manual_checked_division(host, module)
            _check_import_sorting(host, module)


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


def _newline(module: ModuleUnit) -> str:
    return "\r\n" if b"\r\n" in module.source.data else "\n"


def _cfg_test_module(module: ModuleUnit, node: Node) -> bool:
    if node.type != "mod_item":
        return False
    if not any(child.type == "declaration_list" for child in node.named_children):
        return False
    attrs = _item_attributes(module, node)
    return any(re.search(r"cfg\s*\(\s*test\s*\)", attr) for attr in attrs)


def _consume_declaration_line(module: ModuleUnit, node: Node) -> tuple[int, int]:
    data = module.source.data
    start = node.start_byte
    end = node.end_byte
    while end < len(data) and data[end : end + 1] in {b" ", b"\t"}:
        end += 1
    if data[end : end + 2] == b"\r\n":
        end += 2
    elif data[end : end + 1] == b"\n":
        end += 1
    return start, end


def _check_items_after_test_module(host: LintHost, module: ModuleUnit) -> None:
    children = list(module.body.named_children)
    for index, node in enumerate(children):
        if not _cfg_test_module(module, node) or _lint_allowed(
            module, node, "items_after_test_module"
        ):
            continue
        trailing = [
            child
            for child in children[index + 1 :]
            if child.type not in {"attribute_item", "line_comment", "block_comment"}
        ]
        if not trailing:
            continue
        host.add(
            "CLP005",
            "warning",
            "Items appear after a #[cfg(test)] module",
            source=module.source,
            node=trailing[0],
            confidence="definite",
            hint="Move non-test items before the test module.",
            evidence=(
                f"test module starts at line {node.start_point.row + 1}",
                f"items after it: {len(trailing)}",
            ),
        )

        fixable_types = {
            "const_item",
            "enum_item",
            "function_item",
            "impl_item",
            "mod_item",
            "static_item",
            "struct_item",
            "trait_item",
            "type_item",
            "union_item",
            "use_declaration",
        }
        if not all(child.type in fixable_types for child in trailing):
            continue
        if any(_item_attributes(module, child) for child in trailing):
            continue
        trailing_span = module.source.data[
            node.end_byte : trailing[-1].end_byte
        ].decode("utf-8", "replace")
        if "//" in trailing_span or "/*" in trailing_span:
            continue
        first_attr = preceding_attributes(module, children, index)
        insertion_point = node.start_byte
        if first_attr:
            attr_count = len(first_attr)
            attr_nodes = [
                child
                for child in children[max(0, index - attr_count) : index]
                if child.type == "attribute_item"
            ]
            if attr_nodes:
                insertion_point = attr_nodes[0].start_byte
        newline = _newline(module)
        moved = newline.join(module.source.text(child).strip() for child in trailing)
        host.add_fix(
            "CLP005",
            "move items before the #[cfg(test)] module",
            source=module.source,
            start_byte=insertion_point,
            end_byte=insertion_point,
            replacement=moved + newline + newline,
            priority=60,
        )
        for child in trailing:
            start, end = _consume_declaration_line(module, child)
            host.add_fix(
                "CLP005",
                "remove the old post-test item location",
                source=module.source,
                start_byte=start,
                end_byte=end,
                replacement=b"",
                priority=60,
            )
        break


def _method_call_parts(node: Node, module: ModuleUnit) -> tuple[str, Node, Node] | None:
    if node.type != "call_expression":
        return None
    function = node.child_by_field_name("function")
    arguments = node.child_by_field_name("arguments")
    if function is None or arguments is None or function.type != "field_expression":
        return None
    field = function.child_by_field_name("field")
    value = function.child_by_field_name("value")
    if field is None or value is None:
        return None
    return module.source.text(field), value, arguments


def _check_useless_chain_into_iter(host: LintHost, module: ModuleUnit) -> None:
    for node in walk_named(module.body, skip_inline_modules=True):
        outer = _method_call_parts(node, module)
        if (
            outer is None
            or outer[0] != "chain"
            or _lint_allowed(module, node, "useless_conversion")
        ):
            continue
        arguments = outer[2]
        if len(arguments.named_children) != 1:
            continue
        inner = arguments.named_children[0]
        inner_parts = _method_call_parts(inner, module)
        if inner_parts is None or inner_parts[0] != "into_iter":
            continue
        if inner_parts[2].named_children:
            continue
        receiver = inner_parts[1]
        host.add(
            "CLP006",
            "warning",
            "Explicit .into_iter() is unnecessary in Iterator::chain",
            source=module.source,
            node=inner,
            confidence="definite",
            hint="Pass the IntoIterator value directly to chain().",
            evidence=("chain accepts an IntoIterator argument",),
        )
        host.add_fix(
            "CLP006",
            "remove redundant `.into_iter()` from chain argument",
            source=module.source,
            start_byte=receiver.end_byte,
            end_byte=inner.end_byte,
            replacement=b"",
            priority=70,
        )


def _safe_option_mut_reference_parameter(
    module: ModuleUnit, parameter: Node
) -> tuple[str, Node | None] | None:
    name = next(
        (child for child in parameter.named_children if child.type == "identifier"),
        None,
    )
    generic = next(
        (child for child in parameter.named_children if child.type == "generic_type"),
        None,
    )
    if name is None or generic is None:
        return None
    base = generic.child_by_field_name("type") or next(
        (child for child in generic.named_children if child.type == "type_identifier"),
        None,
    )
    arguments = generic.child_by_field_name("type_arguments") or next(
        (child for child in generic.named_children if child.type == "type_arguments"),
        None,
    )
    if base is None or arguments is None or module.source.text(base) != "Option":
        return None
    if len(arguments.named_children) != 1:
        return None
    reference = arguments.named_children[0]
    if reference.type != "reference_type" or not any(
        child.type == "mutable_specifier" for child in reference.named_children
    ):
        return None
    target = next(
        (
            child
            for child in reference.named_children
            if child.type not in {"mutable_specifier", "lifetime"}
        ),
        None,
    )
    if target is None:
        return None
    if target.type not in {"dynamic_type", "slice_type"} and not (
        target.type == "primitive_type" and module.source.text(target) == "str"
    ):
        return None
    mutable = next(
        (
            child
            for child in parameter.named_children
            if child.type == "mutable_specifier"
        ),
        None,
    )
    return module.source.text(name), mutable


def _inside_repeated_context(node: Node, function_body: Node) -> bool:
    current = node.parent
    while current is not None and current != function_body:
        if current.type in {
            "for_expression",
            "while_expression",
            "loop_expression",
            "closure_expression",
        }:
            return True
        current = current.parent
    return False


def _check_needless_option_as_deref_mut(host: LintHost, module: ModuleUnit) -> None:
    for function in walk_named(module.body, skip_inline_modules=True):
        if function.type != "function_item":
            continue
        parameters = function.child_by_field_name("parameters")
        body = function.child_by_field_name("body")
        if parameters is None or body is None:
            continue
        safe: dict[str, Node | None] = {}
        for parameter in parameters.named_children:
            parsed = _safe_option_mut_reference_parameter(module, parameter)
            if parsed is not None:
                safe[parsed[0]] = parsed[1]
        if not safe:
            continue

        occurrences: dict[str, list[Node]] = {name: [] for name in safe}
        calls: dict[str, list[tuple[Node, Node]]] = {name: [] for name in safe}
        for node in walk_named(body, skip_inline_modules=True):
            if node.type == "identifier":
                name = module.source.text(node)
                if name in occurrences:
                    occurrences[name].append(node)
            parts = _method_call_parts(node, module)
            if parts is None or parts[0] != "as_deref_mut" or parts[2].named_children:
                continue
            receiver = parts[1]
            if receiver.type != "identifier":
                continue
            name = module.source.text(receiver)
            if name in calls:
                calls[name].append((node, receiver))

        for name, method_calls in calls.items():
            if len(method_calls) != 1 or len(occurrences[name]) != 1:
                continue
            call, receiver = method_calls[0]
            if _inside_repeated_context(call, body) or _lint_allowed(
                module, function, "needless_option_as_deref"
            ):
                continue
            host.add(
                "CLP007",
                "warning",
                f"Needless as_deref_mut() on {name}",
                source=module.source,
                node=call,
                confidence="definite",
                hint="Pass the Option<&mut ...> directly; its type is unchanged by as_deref_mut().",
                evidence=(
                    "parameter type is Option<&mut dyn Trait>, Option<&mut [T]>, or Option<&mut str>",
                    "the parameter has exactly one body occurrence and is not inside a repeated context",
                ),
            )
            host.add_fix(
                "CLP007",
                f"pass `{name}` directly instead of calling as_deref_mut()",
                source=module.source,
                start_byte=receiver.end_byte,
                end_byte=call.end_byte,
                replacement=b"",
                priority=70,
            )
            mutable = safe[name]
            if mutable is not None:
                identifier = next(
                    (
                        child
                        for child in mutable.parent.named_children
                        if child.type == "identifier"
                    ),
                    None,
                )
                if identifier is not None:
                    host.add_fix(
                        "CLP007",
                        f"remove unnecessary mutability from `{name}`",
                        source=module.source,
                        start_byte=mutable.start_byte,
                        end_byte=identifier.start_byte,
                        replacement=b"",
                        priority=70,
                    )


def _single_else_block(alternative: Node | None) -> Node | None:
    if alternative is None or alternative.type != "else_clause":
        return None
    named = alternative.named_children
    if len(named) != 1 or named[0].type != "block":
        return None
    return named[0]


def _same_expression(module: ModuleUnit, left: Node, right: Node) -> bool:
    def compact(node: Node) -> str:
        return re.sub(r"\s+", "", module.source.text(node))

    return compact(left) == compact(right)


def _simple_place_expression(node: Node) -> bool:
    if node.type in {"identifier", "self", "scoped_identifier"}:
        return True
    if node.type == "field_expression":
        value = node.child_by_field_name("value")
        return value is not None and _simple_place_expression(value)
    if node.type == "parenthesized_expression" and len(node.named_children) == 1:
        return _simple_place_expression(node.named_children[0])
    return False


def _side_effect_free_integer_expression(node: Node, module: ModuleUnit) -> bool:
    if node.type in {
        "identifier",
        "self",
        "integer_literal",
        "scoped_identifier",
    }:
        return True
    if node.type == "field_expression":
        value = node.child_by_field_name("value")
        return value is not None and _side_effect_free_integer_expression(value, module)
    if node.type in {
        "parenthesized_expression",
        "type_cast_expression",
        "unary_expression",
    }:
        return all(
            _side_effect_free_integer_expression(child, module)
            for child in node.named_children
            if child.type not in {"primitive_type", "type_identifier"}
        )
    if node.type == "binary_expression":
        left = node.child_by_field_name("left")
        right = node.child_by_field_name("right")
        return (
            left is not None
            and right is not None
            and _operator(node, module)
            in {"+", "-", "*", "%", "&", "|", "^", "<<", ">>"}
            and _side_effect_free_integer_expression(left, module)
            and _side_effect_free_integer_expression(right, module)
        )
    method = _method_call_parts(node, module)
    if method is None:
        return False
    name, receiver, arguments = method
    if name not in {
        "saturating_add",
        "saturating_sub",
        "saturating_mul",
        "wrapping_add",
        "wrapping_sub",
        "wrapping_mul",
    }:
        return False
    return _side_effect_free_integer_expression(receiver, module) and all(
        _side_effect_free_integer_expression(argument, module)
        for argument in arguments.named_children
    )


def _unique_closure_name(module: ModuleUnit, node: Node) -> str:
    identifiers = {
        module.source.text(child)
        for child in walk_named(node)
        if child.type == "identifier"
    }
    base = "quotient"
    candidate = base
    suffix = 2
    while candidate in identifiers:
        candidate = f"{base}_{suffix}"
        suffix += 1
    return candidate


def _check_manual_checked_division(host: LintHost, module: ModuleUnit) -> None:
    for node in walk_named(module.body, skip_inline_modules=True):
        if node.type != "if_expression" or _lint_allowed(
            module, node, "manual_checked_ops"
        ):
            continue
        condition = node.child_by_field_name("condition")
        consequence = node.child_by_field_name("consequence")
        alternative = _single_else_block(node.child_by_field_name("alternative"))
        if (
            condition is None
            or condition.type != "binary_expression"
            or consequence is None
            or consequence.type != "block"
            or alternative is None
            or _operator(condition, module) != "=="
        ):
            continue
        left = condition.child_by_field_name("left")
        right = condition.child_by_field_name("right")
        if left is None or right is None:
            continue
        if _integer_value(module, right) == 0:
            divisor = left
        elif _integer_value(module, left) == 0:
            divisor = right
        else:
            continue
        fallback = _tail_expression(consequence)
        value = _tail_expression(alternative)
        if fallback is None or value is None or _integer_value(module, fallback) != 0:
            continue
        divisions = [
            candidate
            for candidate in (value, *walk_named(value))
            if candidate.type == "binary_expression"
            and _operator(candidate, module) == "/"
            and (denominator := candidate.child_by_field_name("right")) is not None
            and _same_expression(module, denominator, divisor)
        ]
        if len(divisions) != 1:
            continue
        division = divisions[0]
        numerator = division.child_by_field_name("left")
        if numerator is None:
            continue
        host.add(
            "CLP008",
            "warning",
            "Manual zero check guards an integer division",
            source=module.source,
            node=node,
            confidence="probable",
            hint="Use checked_div() and map the successful quotient instead of checking the divisor manually.",
            evidence=(
                "condition is `divisor == 0` with a zero fallback",
                "the else expression contains exactly one division by the guarded divisor",
            ),
        )
        if not (
            _simple_place_expression(divisor)
            and _side_effect_free_integer_expression(numerator, module)
        ):
            continue
        closure = _unique_closure_name(module, value)
        value_data = module.source.data[value.start_byte : value.end_byte]
        relative_start = division.start_byte - value.start_byte
        relative_end = division.end_byte - value.start_byte
        mapped = (
            value_data[:relative_start]
            + closure.encode("utf-8")
            + value_data[relative_end:]
        ).decode("utf-8", "replace")
        mapped = re.sub(rf"\(\s*{re.escape(closure)}\s*\)", closure, mapped)
        numerator_text = module.source.text(numerator).strip()
        receiver = (
            numerator_text
            if numerator.type
            in {
                "identifier",
                "self",
                "field_expression",
                "call_expression",
                "parenthesized_expression",
                "scoped_identifier",
            }
            else f"({numerator_text})"
        )
        data = module.source.data
        line_start = data.rfind(b"\n", 0, node.start_byte) + 1
        leading = re.match(rb"[ \t]*", data[line_start : node.start_byte])
        base_indent = (leading.group(0) if leading is not None else b"").decode(
            "utf-8", "replace"
        )
        expression_indent = (
            base_indent + "    "
            if node.parent is not None and node.parent.type == "else_clause"
            else " " * node.start_point.column
        )
        continuation = expression_indent + "    "
        expression = (
            f"{receiver}{_newline(module)}"
            f"{continuation}.checked_div({module.source.text(divisor).strip()}){_newline(module)}"
            f"{continuation}.map_or(0, |{closure}| {mapped.strip()})"
        )
        replacement = expression
        if node.parent is not None and node.parent.type == "else_clause":
            replacement = (
                f"{{{_newline(module)}"
                f"{expression_indent}{expression}{_newline(module)}"
                f"{base_indent}}}"
            )
        host.add_fix(
            "CLP008",
            "replace the manual zero guard with checked_div()",
            source=module.source,
            start_byte=node.start_byte,
            end_byte=node.end_byte,
            replacement=replacement,
            priority=70,
        )


def _check_import_sorting(host: LintHost, module: ModuleUnit) -> None:
    groups: list[list[Node]] = []
    current: list[Node] = []
    for node in module.body.named_children:
        if node.type != "use_declaration" or _item_attributes(module, node):
            if len(current) > 1:
                groups.append(current)
            current = []
            continue
        if current:
            gap = module.source.data[current[-1].end_byte : node.start_byte].decode(
                "utf-8", "replace"
            )
            if re.search(r"\n[ \t]*\n|//|/\*|#\[", gap):
                if len(current) > 1:
                    groups.append(current)
                current = []
        current.append(node)
    if len(current) > 1:
        groups.append(current)

    for group in groups:
        spellings = [re.sub(r"\s+", "", module.source.text(node)) for node in group]
        roots = [
            re.sub(r"^(?:pub(?:\([^)]*\))?)?use", "", spelling).split("::", 1)[0]
            for spelling in spellings
        ]
        visibilities = [
            module.source.text(
                next(
                    (
                        child
                        for child in node.named_children
                        if child.type == "visibility_modifier"
                    ),
                    node,
                )
            )
            if any(child.type == "visibility_modifier" for child in node.named_children)
            else "private"
            for node in group
        ]
        # rustfmt does not promise to reorder across semantic root groups.
        if len(set(roots)) != 1 or len(set(visibilities)) != 1:
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
        sorted_nodes = [
            node
            for _, node in sorted(
                zip(spellings, group, strict=True), key=lambda pair: pair[0].casefold()
            )
        ]
        newline = _newline(module)
        first = group[0]
        line_start = module.source.data.rfind(b"\n", 0, first.start_byte) + 1
        indent = module.source.data[line_start : first.start_byte].decode(
            "utf-8", "replace"
        )
        replacement = (newline + indent).join(
            module.source.text(node).strip() for node in sorted_nodes
        )
        host.add_fix(
            "FMT007",
            "sort adjacent same-root imports lexically",
            source=module.source,
            start_byte=group[0].start_byte,
            end_byte=group[-1].end_byte,
            replacement=replacement,
            priority=20,
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
                c
                for c in value.named_children
                if c.type not in {"attribute_item", "line_comment", "block_comment"}
            ]
            if len(statements) != 1:
                continue

            statement = statements[0]
            if_node = None
            if statement.type == "if_expression":
                if_node = statement
            elif (
                statement.type == "expression_statement"
                and len(statement.named_children) == 1
            ):
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
