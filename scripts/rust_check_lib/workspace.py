from __future__ import annotations

from pathlib import Path
from typing import Any, Protocol

from .records import CrateTarget, Dependency, Package


class WorkspaceHost(Protocol):
    root: Path
    root_manifest_path: Path
    root_manifest: dict[str, Any] | None
    packages: list[Package]
    targets: list[CrateTarget]

    def excluded(self, path: Path) -> bool: ...
    def load_toml(self, path: Path) -> dict[str, Any] | None: ...
    def add(self, code: str, severity: str, message: str, **kwargs: Any) -> None: ...


def discover(host: WorkspaceHost) -> None:
    if host.root_manifest_path.is_file():
        host.root_manifest = host.load_toml(host.root_manifest_path)

    manifests: list[Path] = []
    if host.root_manifest and "package" in host.root_manifest:
        manifests.append(host.root_manifest_path)

    workspace = (host.root_manifest or {}).get("workspace", {})
    members = workspace.get("members", []) if isinstance(workspace, dict) else []
    excludes = (
        set(workspace.get("exclude", [])) if isinstance(workspace, dict) else set()
    )
    if members:
        for pattern in members:
            for path in host.root.glob(str(pattern)):
                manifest = path / "Cargo.toml" if path.is_dir() else path
                if not manifest.is_file():
                    continue
                rel_parent = manifest.parent.relative_to(host.root).as_posix()
                if rel_parent in excludes:
                    continue
                manifests.append(manifest)
    else:
        for manifest in host.root.rglob("Cargo.toml"):
            if not host.excluded(manifest) and manifest != host.root_manifest_path:
                manifests.append(manifest)

    for manifest_path in sorted(set(path.resolve() for path in manifests)):
        data = host.load_toml(manifest_path)
        if not data or "package" not in data:
            continue
        package_data = data["package"]
        name = package_data.get("name")
        if not isinstance(name, str):
            host.add(
                "MAN002",
                "error",
                "Package manifest has no string package.name",
                path=manifest_path,
            )
            continue
        package = Package(name, manifest_path, manifest_path.parent, data)
        _collect_dependencies(host, package)
        host.packages.append(package)
        _check_workspace_inheritance(host, package)
        _discover_targets(host, package)

    if not host.packages:
        host.add("MAN003", "error", "No Cargo packages were discovered", path=host.root)


def _merge_dependency(
    package: Package,
    alias: str,
    spec: Any,
    kind: str,
    workspace_deps: dict[str, Any],
) -> None:
    resolved = spec
    if isinstance(spec, dict) and spec.get("workspace") is True:
        resolved = workspace_deps.get(alias, spec)
    package_name = alias
    optional = False
    if isinstance(resolved, dict):
        package_name = str(resolved.get("package", alias))
        optional = bool(resolved.get("optional", False))
    extern_names = {alias.replace("-", "_"), alias.replace("-", "")}
    dependency = next(
        (
            package.dependencies[name]
            for name in extern_names
            if name in package.dependencies
        ),
        None,
    )
    if dependency is None:
        primary = alias.replace("-", "_")
        dependency = Dependency(primary, package_name, optional=optional)
    for extern_name in extern_names:
        package.dependencies[extern_name] = dependency
    dependency.kinds.add(kind)
    dependency.optional = dependency.optional and optional


def _collect_dependencies(host: WorkspaceHost, package: Package) -> None:
    workspace = (host.root_manifest or {}).get("workspace", {}) or {}
    workspace_deps = (
        workspace.get("dependencies", {}) if isinstance(workspace, dict) else {}
    )
    if not isinstance(workspace_deps, dict):
        workspace_deps = {}

    tables = (
        ("dependencies", "normal"),
        ("dev-dependencies", "dev"),
        ("build-dependencies", "build"),
    )
    for table_name, kind in tables:
        table = package.manifest.get(table_name, {})
        if isinstance(table, dict):
            for alias, spec in table.items():
                _merge_dependency(package, str(alias), spec, kind, workspace_deps)

    target_tables = package.manifest.get("target", {})
    if isinstance(target_tables, dict):
        for target_spec in target_tables.values():
            if not isinstance(target_spec, dict):
                continue
            for table_name, kind in tables:
                table = target_spec.get(table_name, {})
                if isinstance(table, dict):
                    for alias, spec in table.items():
                        _merge_dependency(
                            package, str(alias), spec, kind, workspace_deps
                        )


def _contains_workspace_inheritance(value: Any) -> bool:
    if isinstance(value, dict):
        return value.get("workspace") is True or any(
            _contains_workspace_inheritance(item) for item in value.values()
        )
    if isinstance(value, list):
        return any(_contains_workspace_inheritance(item) for item in value)
    return False


def _check_workspace_inheritance(host: WorkspaceHost, package: Package) -> None:
    data = package.manifest
    if _contains_workspace_inheritance(data) and not host.root_manifest:
        host.add(
            "MAN004",
            "error",
            "Manifest uses workspace inheritance, but the repository root Cargo.toml is missing",
            path=package.manifest_path,
            hint="Include the root Cargo.toml/Cargo.lock or run from the full workspace.",
        )
        return

    workspace = (host.root_manifest or {}).get("workspace", {}) or {}
    workspace_deps = workspace.get("dependencies", {})
    for table_name in ("dependencies", "dev-dependencies", "build-dependencies"):
        table = data.get(table_name, {})
        if not isinstance(table, dict):
            continue
        for name, spec in table.items():
            if (
                isinstance(spec, dict)
                and spec.get("workspace") is True
                and name not in workspace_deps
            ):
                host.add(
                    "MAN005",
                    "error",
                    f"Dependency {name!r} inherits from workspace.dependencies, but no root entry exists",
                    path=package.manifest_path,
                )


def _target_path(package: Package, table: dict[str, Any], default: Path) -> Path:
    path = table.get("path") if isinstance(table, dict) else None
    return (
        (package.directory / path).resolve()
        if isinstance(path, str)
        else default.resolve()
    )


def _discover_targets(host: WorkspaceHost, package: Package) -> None:
    data = package.manifest
    lib_table = data.get("lib")
    lib_default = package.directory / "src" / "lib.rs"
    if isinstance(lib_table, dict) or lib_default.is_file():
        table = lib_table if isinstance(lib_table, dict) else {}
        _add_target(
            host,
            package,
            str(table.get("name", package.extern_name)),
            "lib",
            _target_path(package, table, lib_default),
            package.extern_name,
        )

    bins = data.get("bin", [])
    explicit_bin_paths: set[Path] = set()
    if isinstance(bins, list):
        for table in bins:
            if not isinstance(table, dict):
                continue
            name = str(table.get("name", package.name))
            root = _target_path(package, table, package.directory / "src" / "main.rs")
            explicit_bin_paths.add(root)
            _add_target(host, package, name, "bin", root, name.replace("-", "_"))

    main = (package.directory / "src" / "main.rs").resolve()
    if (
        data.get("package", {}).get("autobins", True)
        and main.is_file()
        and main not in explicit_bin_paths
    ):
        _add_target(host, package, package.name, "bin", main, package.extern_name)

    for kind, folder in (
        ("test", "tests"),
        ("example", "examples"),
        ("bench", "benches"),
    ):
        base = package.directory / folder
        if base.is_dir():
            for root in sorted(base.glob("*.rs")):
                _add_target(
                    host,
                    package,
                    root.stem,
                    kind,
                    root.resolve(),
                    root.stem.replace("-", "_"),
                )

    build = package.directory / "build.rs"
    if build.is_file():
        _add_target(
            host, package, "build_script", "build", build.resolve(), "build_script"
        )


def _add_target(
    host: WorkspaceHost,
    package: Package,
    name: str,
    kind: str,
    root: Path,
    extern_name: str,
) -> None:
    target = CrateTarget(
        f"{package.name}:{kind}:{name}", name, extern_name, kind, root, package
    )
    host.targets.append(target)
    if not root.is_file():
        host.add(
            "MAN007",
            "error",
            f"Declared {kind} target does not exist: {root}",
            path=package.manifest_path,
        )
