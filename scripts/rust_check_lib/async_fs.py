from __future__ import annotations

import re
from dataclasses import dataclass
from pathlib import Path
from typing import Any, Iterable, Protocol

from tree_sitter import Node

from .cfg import CfgExpr
from .records import ModuleUnit, SourceFile
from .parsing import walk_named


class AsyncFsHost(Protocol):
    targets: list[Any]
    include_tests: bool

    def add(self, code: str, severity: str, message: str, **kwargs: Any) -> None: ...


class UseLike(Protocol):
    path: tuple[str, ...]
    alias: str | None
    glob: bool


@dataclass(frozen=True)
class LocalUseSpec:
    path: tuple[str, ...]
    alias: str | None
    glob: bool


@dataclass(frozen=True)
class SyncFsHelper:
    name: str
    qualified_name: tuple[str, ...]
    source: SourceFile
    node: Node
    calls: tuple[str, ...]


# Calls for which compio 0.19 exposes a direct async primitive, or for
# which Griffr already has an owned-buffer async implementation.
_COMPIO_CALLS = {
    "copy",
    "create_dir",
    "create_dir_all",
    "hard_link",
    "metadata",
    "read",
    "remove_dir",
    "remove_file",
    "rename",
    "set_permissions",
    "symlink_metadata",
    "write",
    "File::create",
    "File::open",
    "OpenOptions::new",
}

# These require a synchronous namespace walk or another compatibility boundary
# with compio 0.19. Their presence prevents AFS002 from recommending that the
# whole blocking closure be removed.
_BLOCKING_ONLY_CALLS = {
    "canonicalize",
    "read_dir",
    "read_link",
    "remove_dir_all",
}

_PATH_METHODS = {
    "canonicalize",
    "exists",
    "is_dir",
    "is_file",
    "is_symlink",
    "metadata",
    "read_dir",
    "read_link",
    "symlink_metadata",
    "try_exists",
}

_PATH_METHOD_TO_CALL = {
    "canonicalize": "canonicalize",
    "exists": "metadata",
    "is_dir": "metadata",
    "is_file": "metadata",
    "is_symlink": "symlink_metadata",
    "metadata": "metadata",
    "read_dir": "read_dir",
    "read_link": "read_link",
    "symlink_metadata": "symlink_metadata",
    "try_exists": "metadata",
}

_BLOCKING_BOUNDARY_NAMES = {
    "dispatch_blocking",
    "run_blocking",
    "spawn_blocking",
}

_TRIVIAL_CALL_ADAPTERS = {
    "and_then",
    "map",
    "map_err",
    "ok",
    "ok_or",
    "ok_or_else",
    "unwrap_or",
    "unwrap_or_default",
    "unwrap_or_else",
}

_PATH_TYPE = re.compile(r"(?:^|::)(?:Path|PathBuf)\b")
_PATH_NAME = re.compile(
    r"(?:^|_)(?:path|root|dir|directory|dest|destination|source|target)$"
)


def run(host: AsyncFsHost) -> None:
    helper_indexes = {
        target.key: _collect_sync_fs_helpers(host, target) for target in host.targets
    }
    seen_modules: set[tuple[str, tuple[str, ...], Path, int, int]] = set()
    for target in host.targets:
        for module in target.iter_modules():
            source = module.source
            if _excluded_test_module(host, module):
                continue
            module_key = (
                target.key,
                module.path,
                source.path,
                module.body.start_byte,
                module.body.end_byte,
            )
            if module_key in seen_modules:
                continue
            seen_modules.add(module_key)
            _check_async_contexts(host, module, helper_indexes[target.key])
            _check_redundant_blocking_wrappers(host, module)


def _test_source(rel: str) -> bool:
    path = Path(rel)
    return path.name in {"test.rs", "tests.rs"} or any(
        part in {"test", "tests"} for part in path.parts
    )


def _condition_requires_test(condition: CfgExpr) -> bool:
    if condition.kind == "atom":
        return condition.value == "test"
    if condition.kind == "all":
        return any(_condition_requires_test(child) for child in condition.children)
    if condition.kind == "any":
        return bool(condition.children) and all(
            _condition_requires_test(child) for child in condition.children
        )
    return False


def _excluded_test_module(host: AsyncFsHost, module: ModuleUnit) -> bool:
    return not host.include_tests and (
        _test_source(module.source.rel) or _condition_requires_test(module.condition)
    )


def _collect_sync_fs_helpers(
    host: AsyncFsHost, target: Any
) -> dict[str, list[SyncFsHelper]]:
    helpers: dict[str, list[SyncFsHelper]] = {}
    for module in target.iter_modules():
        if _excluded_test_module(host, module):
            continue
        for node in walk_named(module.body, skip_inline_modules=True):
            if node.type != "function_item" or _function_is_async(module.source, node):
                continue
            name_node = node.child_by_field_name("name")
            if name_node is None:
                continue
            calls = _sync_calls_in_function(module, node)
            if not calls:
                continue
            name = module.source.text(name_node)
            helper = SyncFsHelper(
                name=name,
                qualified_name=(*module.path, name),
                source=module.source,
                node=node,
                calls=tuple(dict.fromkeys(calls)),
            )
            helpers.setdefault(name, []).append(helper)
    return helpers


def _sync_calls_in_function(module: ModuleUnit, function: Node) -> list[str]:
    source = module.source
    module_aliases, imported_functions, glob_import = _std_fs_imports(module.imports)
    local_modules, local_functions, local_glob = _std_fs_imports(
        _local_use_specs(source, function)
    )
    modules = module_aliases | local_modules
    functions = {**imported_functions, **local_functions}
    calls: list[str] = []
    for node in _walk_scope(function, skip_nested_functions=True):
        if node.type != "call_expression" or _inside_blocking_boundary(
            source, node, function
        ):
            continue
        call = _std_fs_call_name(
            source,
            node,
            modules,
            functions,
            glob_import or local_glob,
        )
        if call is None:
            path_method = _path_method_call(source, node, function)
            call = path_method[0] if path_method is not None else None
        if call is not None:
            calls.append(call)
    return calls


def _called_sync_fs_helper(
    source: SourceFile,
    call: Node,
    helper_index: dict[str, list[SyncFsHelper]],
) -> SyncFsHelper | None:
    path = _direct_call_path(source, call)
    if not path:
        return None
    candidates = helper_index.get(path[-1], [])
    if not candidates:
        return None
    unique = {
        (
            candidate.qualified_name,
            candidate.source.path,
            candidate.node.start_byte,
        ): candidate
        for candidate in candidates
    }
    candidates = list(unique.values())
    if len(candidates) == 1:
        return candidates[0]

    suffix = tuple(part for part in path if part not in {"crate", "self", "super"})
    if len(suffix) > 1:
        matches = [
            candidate
            for candidate in candidates
            if candidate.qualified_name[-len(suffix) :] == suffix
        ]
        if len(matches) == 1:
            return matches[0]
    return None


def _direct_call_path(source: SourceFile, call: Node) -> tuple[str, ...] | None:
    function = call.child_by_field_name("function")
    if function is None:
        return None
    if function.type == "generic_function":
        function = function.child_by_field_name("function") or (
            function.named_children[0] if function.named_children else function
        )
    if function.type not in {
        "identifier",
        "scoped_identifier",
        "scoped_type_identifier",
    }:
        return None
    return tuple(part for part in re.split(r"\s*::\s*", source.text(function)) if part)


def _function_is_async(source: SourceFile, function: Node) -> bool:
    modifiers = next(
        (
            child
            for child in function.named_children
            if child.type == "function_modifiers"
        ),
        None,
    )
    return modifiers is not None and "async" in source.text(modifiers).split()


def _inside_blocking_boundary(source: SourceFile, node: Node, stop: Node) -> bool:
    current = node.parent
    while current is not None and current != stop:
        if current.type == "closure_expression" and _closure_is_blocking_boundary(
            source, current
        ):
            return True
        current = current.parent
    return False


def _walk_scope(root: Node, *, skip_nested_functions: bool) -> Iterable[Node]:
    stack = [root]
    while stack:
        node = stack.pop()
        yield node
        children = list(node.named_children)
        for child in reversed(children):
            if (
                skip_nested_functions
                and child != root
                and child.type == "function_item"
            ):
                continue
            stack.append(child)


def _check_async_contexts(
    host: AsyncFsHost,
    module: ModuleUnit,
    helper_index: dict[str, list[SyncFsHelper]],
) -> None:
    source = module.source
    module_aliases, imported_functions, glob_import = _std_fs_imports(module.imports)

    for node in walk_named(module.body, skip_inline_modules=True):
        if node.type != "call_expression":
            continue
        owner = _async_owner(node, source)
        if owner is None:
            continue

        local_specs = _local_use_specs(source, owner)
        local_modules, local_functions, local_glob = _std_fs_imports(local_specs)
        modules = module_aliases | local_modules
        functions = {**imported_functions, **local_functions}
        call = _std_fs_call_name(
            source,
            node,
            modules,
            functions,
            glob_import or local_glob,
        )
        api_text = ""
        if call is not None:
            function = node.child_by_field_name("function")
            api_text = (
                source.text(function) if function is not None else source.text(node)
            )
        else:
            path_method = _path_method_call(source, node, owner)
            if path_method is None:
                helper = _called_sync_fs_helper(source, node, helper_index)
                if helper is None:
                    continue
                host.add(
                    "AFS003",
                    "error",
                    f"Synchronous filesystem helper {source.text(node.child_by_field_name('function'))!r} runs directly in async code",
                    source=source,
                    node=node,
                    confidence="probable",
                    evidence=(
                        f"async context: {_owner_label(source, owner)}",
                        f"resolved helper: {'::'.join(helper.qualified_name)}",
                        *(
                            f"helper filesystem call: {item}"
                            for item in helper.calls[:4]
                        ),
                    ),
                    hint=(
                        "Make the helper async and use compio::fs, or invoke it inside an explicit "
                        "spawn_blocking/dispatch_blocking boundary if it runs an unsupported namespace walk."
                    ),
                )
                continue
            call, api_text = path_method

        host.add(
            "AFS001",
            "error",
            f"Synchronous filesystem access {api_text!r} runs directly in async code",
            source=source,
            node=node,
            confidence="definite"
            if call in _COMPIO_CALLS
            else "probable",
            evidence=(
                f"async context: {_owner_label(source, owner)}",
                f"resolved filesystem call: {call}",
                "no enclosing recognized blocking boundary",
            ),
            hint=(
                "Use compio::fs and await the call. If compio has no matching API, isolate only "
                "the synchronous namespace walk or library call behind spawn_blocking/dispatch_blocking."
            ),
        )


def _check_redundant_blocking_wrappers(host: AsyncFsHost, module: ModuleUnit) -> None:
    source = module.source
    module_aliases, imported_functions, glob_import = _std_fs_imports(module.imports)

    for boundary_call in walk_named(module.body, skip_inline_modules=True):
        if boundary_call.type != "call_expression" or not _is_blocking_boundary_call(
            source, boundary_call
        ):
            continue
        closure = _closure_argument(boundary_call)
        if closure is None:
            continue

        local_specs = _local_use_specs(source, closure)
        local_modules, local_functions, local_glob = _std_fs_imports(local_specs)
        modules = module_aliases | local_modules
        functions = {**imported_functions, **local_functions}

        calls: list[tuple[str, Node, str]] = []
        has_other_calls = False
        for nested in walk_named(closure):
            if nested.type != "call_expression":
                continue
            fs_call = _std_fs_call_name(
                source,
                nested,
                modules,
                functions,
                glob_import or local_glob,
            )
            api_text = ""
            if fs_call is not None:
                function = nested.child_by_field_name("function")
                api_text = (
                    source.text(function)
                    if function is not None
                    else source.text(nested)
                )
            else:
                path_method = _path_method_call(source, nested, closure)
                if path_method is None:
                    function = nested.child_by_field_name("function")
                    function_text = (
                        source.text(function) if function is not None else ""
                    )
                    terminal = (
                        re.sub(r"\s+", "", function_text).split("::")[-1].split(".")[-1]
                    )
                    if terminal not in _TRIVIAL_CALL_ADAPTERS:
                        has_other_calls = True
                    continue
                fs_call, api_text = path_method
            calls.append((fs_call, nested, api_text))

        if not calls or has_other_calls:
            continue
        if any(fs_call in _BLOCKING_ONLY_CALLS for fs_call, _, _ in calls):
            continue
        if not all(fs_call in _COMPIO_CALLS for fs_call, _, _ in calls):
            continue

        function = boundary_call.child_by_field_name("function")
        boundary = source.text(function) if function is not None else "blocking wrapper"
        evidence = tuple(
            f"{api_text} resolves to {fs_call}"
            for fs_call, _, api_text in calls[:6]
        )
        host.add(
            "AFS002",
            "warning",
            f"{boundary} contains only filesystem calls that have async compio replacements",
            source=source,
            node=boundary_call,
            confidence="probable",
            evidence=evidence
            + (
                "no read_dir, read_link, remove_dir_all, or canonicalize call was found in the closure",
            ),
            hint=(
                "Move this work onto the async Dispatcher path and use compio::fs directly; keep blocking "
                "capacity for synchronous libraries and directory walks that compio cannot express."
            ),
        )


def _std_fs_imports(
    specs: Iterable[UseLike],
) -> tuple[set[str], dict[str, str], bool]:
    module_aliases = {"std::fs"}
    imported_functions: dict[str, str] = {}
    glob_import = False
    for spec in specs:
        path = tuple(spec.path)
        if path == ("std", "fs"):
            if spec.glob:
                glob_import = True
            elif spec.alias:
                module_aliases.add(spec.alias)
            continue
        if len(path) >= 3 and path[:2] == ("std", "fs"):
            call = "::".join(path[2:])
            if spec.glob:
                glob_import = True
            elif spec.alias:
                imported_functions[spec.alias] = call
    return module_aliases, imported_functions, glob_import


def _local_use_specs(source: SourceFile, owner: Node) -> list[LocalUseSpec]:
    specs: list[LocalUseSpec] = []
    for node in walk_named(owner):
        if node.type != "use_declaration":
            continue
        target = node.child_by_field_name("argument")
        if target is None:
            target = next(iter(node.named_children), None)
        if target is None:
            continue
        for path, alias, glob, _ in _flatten_use_node(source, target, ()):
            specs.append(LocalUseSpec(path, alias, glob))
    return specs


def _flatten_use_node(
    source: SourceFile,
    node: Node,
    prefix: tuple[str, ...],
) -> list[tuple[tuple[str, ...], str | None, bool, Node]]:
    node_type = node.type
    if node_type in {"identifier", "type_identifier", "crate", "self", "super"}:
        value = source.text(node)
        path = prefix + (value,)
        alias = prefix[-1] if value == "self" and prefix else value
        if value == "self" and prefix:
            path = prefix
        return [(path, alias, False, node)]
    if node_type in {"scoped_identifier", "scoped_type_identifier"}:
        path = tuple(part for part in re.split(r"\s*::\s*", source.text(node)) if part)
        return [(prefix + path, path[-1] if path else None, False, node)]
    if node_type == "use_wildcard":
        text = source.text(node).strip()
        extra: tuple[str, ...] = ()
        if text not in {"*", "::*"}:
            cleaned = text[:-3] if text.endswith("::*") else text.rstrip("*")
            extra = tuple(part for part in re.split(r"\s*::\s*", cleaned) if part)
        return [(prefix + extra, None, True, node)]
    if node_type == "use_as_clause":
        path_node = node.child_by_field_name("path")
        alias_node = node.child_by_field_name("alias")
        if path_node is None or alias_node is None:
            return []
        alias = source.text(alias_node)
        return [
            (path, alias, glob, use_node)
            for path, _, glob, use_node in _flatten_use_node(source, path_node, prefix)
        ]
    if node_type == "scoped_use_list":
        path_node = node.child_by_field_name("path")
        list_node = node.child_by_field_name("list")
        if path_node is None or list_node is None:
            return []
        base = tuple(
            part for part in re.split(r"\s*::\s*", source.text(path_node)) if part
        )
        return _flatten_use_node(source, list_node, prefix + base)
    if node_type == "use_list":
        out: list[tuple[tuple[str, ...], str | None, bool, Node]] = []
        for child in node.named_children:
            out.extend(_flatten_use_node(source, child, prefix))
        return out
    text = source.text(node).strip()
    path = tuple(part for part in re.split(r"\s*::\s*", text) if part)
    return [(prefix + path, path[-1] if path else None, False, node)] if path else []


def _std_fs_call_name(
    source: SourceFile,
    call: Node,
    module_aliases: set[str],
    imported_functions: dict[str, str],
    glob_import: bool,
) -> str | None:
    function = call.child_by_field_name("function")
    if function is None or function.type == "field_expression":
        return None
    text = re.sub(r"\s+", "", source.text(function))
    if text in imported_functions:
        return imported_functions[text]
    for alias, base in imported_functions.items():
        prefix = alias + "::"
        if text.startswith(prefix):
            return base + "::" + text[len(prefix) :]
    if glob_import and text in _COMPIO_CALLS | _BLOCKING_ONLY_CALLS:
        return text
    for alias in sorted(module_aliases, key=len, reverse=True):
        prefix = alias + "::"
        if text.startswith(prefix):
            return text[len(prefix) :]
    return None


def _path_method_call(
    source: SourceFile, call: Node, owner: Node
) -> tuple[str, str] | None:
    function = call.child_by_field_name("function")
    if function is None or function.type != "field_expression":
        return None
    field = function.child_by_field_name("field")
    value = function.child_by_field_name("value")
    if field is None or value is None:
        return None
    method = source.text(field)
    if method not in _PATH_METHODS:
        return None
    receiver = source.text(value)
    path_bindings = _path_bindings(source, owner)
    if not _path_expression(receiver, path_bindings, method):
        return None
    return _PATH_METHOD_TO_CALL[method], f"{receiver}.{method}"


def _path_bindings(source: SourceFile, owner: Node) -> set[str]:
    bindings: set[str] = set()
    if owner.type == "function_item":
        parameters = owner.child_by_field_name("parameters")
        if parameters is not None:
            for parameter in parameters.named_children:
                type_node = parameter.child_by_field_name("type")
                pattern = parameter.child_by_field_name("pattern")
                if type_node is None or pattern is None:
                    continue
                if _PATH_TYPE.search(source.text(type_node)):
                    bindings.update(_pattern_identifiers(source, pattern))

    lets: list[tuple[set[str], str, str]] = []
    for node in walk_named(owner):
        if node.type != "let_declaration":
            continue
        pattern = node.child_by_field_name("pattern")
        value = node.child_by_field_name("value")
        type_node = node.child_by_field_name("type")
        if pattern is None:
            continue
        names = _pattern_identifiers(source, pattern)
        value_text = source.text(value) if value is not None else ""
        type_text = source.text(type_node) if type_node is not None else ""
        lets.append((names, type_text, value_text))

    changed = True
    while changed:
        changed = False
        for names, type_text, value_text in lets:
            if names <= bindings:
                continue
            if _PATH_TYPE.search(type_text) or _path_expression(
                value_text, bindings, ""
            ):
                before = len(bindings)
                bindings.update(names)
                changed |= len(bindings) != before
    return bindings


def _pattern_identifiers(source: SourceFile, pattern: Node) -> set[str]:
    return {
        source.text(node) for node in walk_named(pattern) if node.type == "identifier"
    } | ({source.text(pattern)} if pattern.type == "identifier" else set())


def _path_expression(text: str, bindings: set[str], method: str) -> bool:
    compact = re.sub(r"\s+", "", text)
    compact = compact.lstrip("&*")
    while compact.startswith("(") and compact.endswith(")"):
        compact = compact[1:-1]
    root_match = re.match(r"([A-Za-z_][A-Za-z0-9_]*)", compact)
    root = root_match.group(1) if root_match else ""
    if root in bindings:
        return True
    if "Path::new(" in compact or "PathBuf::from(" in compact:
        return True
    if any(
        part in compact for part in (".join(", ".with_extension(", ".with_file_name(")
    ):
        return True
    if re.search(
        r"\.(?:path|root|dir|directory|dest|destination|source|target)\b", compact
    ):
        return True
    if root and _PATH_NAME.search(root):
        return True
    # exists/try_exists/canonicalize/read_dir/read_link are overwhelmingly Path
    # APIs in ordinary Rust code. Keep the broader inference probable through
    # the AFS001 confidence selection rather than missing direct blocking calls.
    return method in {"canonicalize", "exists", "read_dir", "read_link", "try_exists"}


def _async_owner(node: Node, source: SourceFile) -> Node | None:
    current = node.parent
    while current is not None:
        if current.type == "async_block":
            return current
        if current.type == "closure_expression" and _closure_is_blocking_boundary(
            source, current
        ):
            return None
        if current.type == "function_item":
            modifiers = next(
                (
                    child
                    for child in current.named_children
                    if child.type == "function_modifiers"
                ),
                None,
            )
            if modifiers is not None and "async" in source.text(modifiers).split():
                return current
            return None
        current = current.parent
    return None


def _closure_is_blocking_boundary(source: SourceFile, closure: Node) -> bool:
    current = closure.parent
    while current is not None and current.type in {
        "arguments",
        "parenthesized_expression",
        "reference_expression",
    }:
        current = current.parent
    return (
        current is not None
        and current.type == "call_expression"
        and _is_blocking_boundary_call(source, current)
    )


def _is_blocking_boundary_call(source: SourceFile, call: Node) -> bool:
    function = call.child_by_field_name("function")
    if function is None:
        return False
    text = re.sub(r"\s+", "", source.text(function))
    name = text.split("::")[-1].split(".")[-1]
    return name in _BLOCKING_BOUNDARY_NAMES


def _closure_argument(call: Node) -> Node | None:
    arguments = call.child_by_field_name("arguments")
    if arguments is None:
        return None
    for child in arguments.named_children:
        if child.type == "closure_expression":
            return child
    return None


def _owner_label(source: SourceFile, owner: Node) -> str:
    if owner.type == "async_block":
        line, _ = source.location(owner)
        return f"async block at line {line}"
    name = owner.child_by_field_name("name")
    return f"async fn {source.text(name) if name is not None else '<anonymous>'}"
