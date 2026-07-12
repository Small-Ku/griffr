from __future__ import annotations

from collections.abc import Iterator

from tree_sitter import Language, Node, Parser, Tree
import tree_sitter_rust

# tree-sitter and tree-sitter-rust are intentionally pinned to the same release
# in the launcher script. Mixing newer ABI/API releases has caused native crashes.
LANGUAGE = Language(tree_sitter_rust.language())
PARSER = Parser(LANGUAGE)


def parse(data: bytes) -> Tree:
    return PARSER.parse(data)


def walk_named(root: Node, *, skip_inline_modules: bool = False) -> Iterator[Node]:
    stack = list(reversed(root.named_children))
    while stack:
        node = stack.pop()
        yield node
        if (
            skip_inline_modules
            and node.type == "mod_item"
            and any(child.type == "declaration_list" for child in node.named_children)
        ):
            continue
        stack.extend(reversed(node.named_children))


def leaf_tokens(data: bytes) -> tuple[tuple[str, str], ...]:
    tree = parse(data)
    out: list[tuple[str, str]] = []
    stack = [tree.root_node]
    while stack:
        node = stack.pop()
        if node.child_count:
            stack.extend(reversed(node.children))
        elif node.type not in {"line_comment", "block_comment"}:
            out.append(
                (
                    node.type,
                    data[node.start_byte : node.end_byte].decode("utf-8", "replace"),
                )
            )
    return tuple(out)
