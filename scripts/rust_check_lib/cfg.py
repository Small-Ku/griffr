from __future__ import annotations

import re
from dataclasses import dataclass
from itertools import product
from typing import Iterable, Iterator


@dataclass(frozen=True)
class CfgExpr:
    kind: str
    value: str = ""
    children: tuple["CfgExpr", ...] = ()
    raw: str = ""

    def describe(self) -> str:
        if self.kind == "true":
            return "always"
        if self.kind == "unknown":
            return self.raw or "unknown cfg"
        if self.kind == "atom":
            return self.value
        if self.kind == "not":
            return f"not({self.children[0].describe()})"
        return f"{self.kind}({', '.join(child.describe() for child in self.children)})"


TRUE = CfgExpr("true")
FALSE = CfgExpr("false")
UNKNOWN = CfgExpr("unknown", raw="unknown cfg")

_TOKEN_RE = re.compile(
    r'\s*(?:(?P<ident>[A-Za-z_][A-Za-z0-9_]*)|(?P<string>"(?:\\.|[^"\\])*")|(?P<punct>[(),=]))'
)


@dataclass(frozen=True)
class _Token:
    kind: str
    value: str


def _tokens(text: str) -> list[_Token] | None:
    out: list[_Token] = []
    cursor = 0
    while cursor < len(text):
        match = _TOKEN_RE.match(text, cursor)
        if not match:
            if text[cursor:].strip():
                return None
            break
        cursor = match.end()
        kind = (
            "ident"
            if match.group("ident")
            else "string"
            if match.group("string")
            else "punct"
        )
        value = match.group(kind)
        out.append(_Token(kind, value))
    return out


class _Parser:
    def __init__(self, tokens: list[_Token], raw: str):
        self.tokens = tokens
        self.raw = raw
        self.index = 0

    def peek(self, value: str | None = None) -> _Token | None:
        if self.index >= len(self.tokens):
            return None
        token = self.tokens[self.index]
        if value is not None and token.value != value:
            return None
        return token

    def take(self, value: str | None = None) -> _Token | None:
        token = self.peek(value)
        if token is not None:
            self.index += 1
        return token

    def parse(self) -> CfgExpr:
        expr = self.parse_expr()
        if expr is None or self.index != len(self.tokens):
            return CfgExpr("unknown", raw=self.raw)
        return expr

    def parse_expr(self) -> CfgExpr | None:
        name = self.take()
        if name is None or name.kind != "ident":
            return None
        if self.take("=") is not None:
            value = self.take()
            if value is None or value.kind not in {"ident", "string"}:
                return None
            decoded = value.value
            if value.kind == "string":
                try:
                    decoded = bytes(decoded[1:-1], "utf-8").decode("unicode_escape")
                except UnicodeDecodeError:
                    decoded = decoded[1:-1]
            return CfgExpr("atom", f"{name.value}={decoded}")
        if self.take("(") is None:
            return CfgExpr("atom", name.value)
        children: list[CfgExpr] = []
        if self.take(")") is None:
            while True:
                child = self.parse_expr()
                if child is None:
                    return None
                children.append(child)
                if self.take(")") is not None:
                    break
                if self.take(",") is None:
                    return None
        if name.value == "not" and len(children) == 1:
            return CfgExpr("not", children=tuple(children))
        if name.value in {"all", "any"}:
            return CfgExpr(name.value, children=tuple(children))
        return CfgExpr("unknown", raw=self.raw)


def parse_cfg(text: str) -> CfgExpr:
    text = text.strip()
    tokens = _tokens(text)
    if tokens is None:
        return CfgExpr("unknown", raw=text)
    return _Parser(tokens, text).parse()


def _balanced_argument(text: str, start: int) -> str | None:
    depth = 0
    quote = False
    escaped = False
    for index in range(start, len(text)):
        char = text[index]
        if quote:
            if escaped:
                escaped = False
            elif char == "\\":
                escaped = True
            elif char == '"':
                quote = False
            continue
        if char == '"':
            quote = True
        elif char == "(":
            depth += 1
        elif char == ")":
            depth -= 1
            if depth == 0:
                return text[start + 1 : index]
    return None


def conditions_from_attributes(attributes: Iterable[str]) -> CfgExpr:
    expressions: list[CfgExpr] = []
    for attribute in attributes:
        for match in re.finditer(r"\bcfg\s*\(", attribute):
            argument = _balanced_argument(attribute, match.end() - 1)
            if argument is not None:
                expressions.append(parse_cfg(argument))
    if not expressions:
        return TRUE
    return all_of(*expressions)


def all_of(*expressions: CfgExpr) -> CfgExpr:
    flat: list[CfgExpr] = []
    for expr in expressions:
        if expr.kind == "false":
            return FALSE
        if expr.kind == "true":
            continue
        if expr.kind == "all":
            flat.extend(expr.children)
        else:
            flat.append(expr)
    if not flat:
        return TRUE
    if len(flat) == 1:
        return flat[0]
    return CfgExpr("all", children=tuple(flat))


def any_of(*expressions: CfgExpr) -> CfgExpr:
    flat: list[CfgExpr] = []
    for expr in expressions:
        if expr.kind == "true":
            return TRUE
        if expr.kind == "false":
            continue
        if expr.kind == "any":
            flat.extend(expr.children)
        else:
            flat.append(expr)
    if not flat:
        return FALSE
    if len(flat) == 1:
        return flat[0]
    return CfgExpr("any", children=tuple(flat))


Literal = tuple[str, bool]
Clause = frozenset[Literal]


def _negate(expr: CfgExpr) -> CfgExpr:
    if expr.kind == "true":
        return FALSE
    if expr.kind == "false":
        return TRUE
    if expr.kind == "not":
        return expr.children[0]
    if expr.kind == "all":
        return any_of(*(_negate(child) for child in expr.children))
    if expr.kind == "any":
        return all_of(*(_negate(child) for child in expr.children))
    return CfgExpr("not", children=(expr,))


def _dnf(expr: CfgExpr, *, cap: int = 256) -> list[Clause] | None:
    if expr.kind == "true":
        return [frozenset()]
    if expr.kind == "false":
        return []
    if expr.kind == "unknown":
        return None
    if expr.kind == "atom":
        return [frozenset({(expr.value, True)})]
    if expr.kind == "not":
        child = expr.children[0]
        if child.kind == "atom":
            return [frozenset({(child.value, False)})]
        return _dnf(_negate(child), cap=cap)
    child_dnfs = [_dnf(child, cap=cap) for child in expr.children]
    if any(value is None for value in child_dnfs):
        return None
    resolved = [value for value in child_dnfs if value is not None]
    if expr.kind == "any":
        merged = [clause for clauses in resolved for clause in clauses]
        return merged if len(merged) <= cap else None
    if expr.kind == "all":
        current: list[Clause] = [frozenset()]
        for clauses in resolved:
            next_values: list[Clause] = []
            for left, right in product(current, clauses):
                merged = frozenset((*left, *right))
                if _clause_possible(merged):
                    next_values.append(merged)
                    if len(next_values) > cap:
                        return None
            current = next_values
        return current
    return None


_SINGLE_VALUE_KEYS = {
    "target_arch",
    "target_endian",
    "target_env",
    "target_family",
    "target_os",
    "target_pointer_width",
    "target_vendor",
    "panic",
}


def _split_atom(atom: str) -> tuple[str, str | None]:
    if "=" not in atom:
        return atom, None
    return tuple(atom.split("=", 1))  # type: ignore[return-value]


def _canonical_positive(atom: str) -> tuple[str, str | None]:
    if atom == "windows":
        return "target_family", "windows"
    if atom == "unix":
        return "target_family", "unix"
    return _split_atom(atom)


def _clause_possible(clause: Clause) -> bool:
    polarities: dict[str, set[bool]] = {}
    positive_values: dict[str, set[str]] = {}
    for atom, positive in clause:
        polarities.setdefault(atom, set()).add(positive)
        if len(polarities[atom]) > 1:
            return False
        if positive:
            key, value = _canonical_positive(atom)
            if value is not None and key in _SINGLE_VALUE_KEYS:
                positive_values.setdefault(key, set()).add(value)
                if len(positive_values[key]) > 1:
                    return False
    # Canonical aliases must also conflict with explicit negation.
    for atom, positive in clause:
        if positive:
            continue
        key, value = _canonical_positive(atom)
        if value is None:
            continue
        if value in positive_values.get(key, set()):
            return False
    return True


def compatibility(left: CfgExpr, right: CfgExpr) -> bool | None:
    """Return whether both cfg expressions can be active simultaneously.

    ``None`` means the expression exceeded the intentionally small solver or used
    syntax that cannot be represented safely. Callers should surface uncertainty
    rather than silently treating it as exclusive.
    """

    clauses = _dnf(all_of(left, right))
    if clauses is None:
        return None
    return any(_clause_possible(clause) for clause in clauses)


def satisfiable(expr: CfgExpr) -> bool | None:
    clauses = _dnf(expr)
    if clauses is None:
        return None
    return bool(clauses)


def iter_atoms(expr: CfgExpr) -> Iterator[str]:
    if expr.kind == "atom":
        yield expr.value
    for child in expr.children:
        yield from iter_atoms(child)
