#!/usr/bin/env python3
"""Read-only dependency-boundary acceptance for ticket 47.

Uses `cargo metadata` only. It intentionally does not build, so it proves the
declared dependency graph (ADK-free SDK/common, Worker runtime without Gateway,
and registry not on the invocation path) without executing a workspace build.
"""

from __future__ import annotations

import json
import subprocess
from pathlib import Path
import sys

ROOT = Path(__file__).resolve().parents[2]


def metadata() -> dict:
    result = subprocess.run(
        ["cargo", "metadata", "--format-version=1", "--locked"],
        cwd=ROOT,
        capture_output=True,
        text=True,
        timeout=300,
        check=False,
    )
    if result.returncode:
        raise SystemExit(f"cargo metadata failed:\n{result.stdout}\n{result.stderr}")
    return json.loads(result.stdout)


def dependency_names(document: dict, consumer: str) -> set[str]:
    packages = {package["id"]: package for package in document["packages"]}
    nodes = {
        node["id"]: [dependency["pkg"] for dependency in node["deps"]]
        for node in document["resolve"]["nodes"]
    }
    member = next(
        package
        for package in document["packages"]
        if package["name"] == consumer and package["id"] in document["workspace_members"]
    )
    pending = list(nodes[member["id"]])
    found: set[str] = set()
    while pending:
        identity = pending.pop()
        if identity not in found:
            found.add(identity)
            pending.extend(nodes[identity])
    return {packages[identity]["name"] for identity in found}


def main() -> int:
    document = metadata()
    failures: list[str] = []

    def require(condition: bool, message: str) -> None:
        if not condition:
            failures.append(message)

    def adk(dependencies: set[str]) -> set[str]:
        return {name for name in dependencies if name == "adk" or name.startswith("adk-")}

    for consumer in (
        "common",
        "sdk",
        "goldenbee-client-cli",
        "sdk-client",
        "gateway-core",
        "registry",
    ):
        dependencies = dependency_names(document, consumer)
        require(not adk(dependencies), f"{consumer} links native ADK execution")
        require(
            dependencies.isdisjoint({"runtime", "general", "knowledge", "install"}),
            f"{consumer} links Worker execution or installation",
        )

    for consumer in ("registry",):
        dependencies = dependency_names(document, consumer)
        require(not adk(dependencies), f"{consumer} links native ADK execution")
        require(
            dependencies.isdisjoint({"runtime", "install", "general", "echo"}),
            f"{consumer} is an invocation dependency",
        )

    for consumer in ("general", "knowledge", "runtime", "sdk", "install", "instance"):
        require(
            dependency_names(document, consumer).isdisjoint(
                {"gateway-core", "goldenbee-gateway"}
            ),
            f"{consumer} links the Gateway control plane",
        )

    core = dependency_names(document, "gateway-core")
    require(
        core.isdisjoint({"axum", "goldenbee-client-cli", "sdk", "goldenbee-gateway"}),
        "gateway-core links an adapter, CLI, SDK, or server edge",
    )
    adapter = dependency_names(document, "goldenbee-gateway")
    require(
        "gateway-core" in adapter,
        "goldenbee-gateway does not depend on gateway-core",
    )
    require(
        {"install", "instance"} <= adapter,
        "goldenbee-gateway does not own local package and instance lifecycle",
    )
    require(
        adapter.isdisjoint({"general", "knowledge", "coding", "echo", "graph"}),
        "goldenbee-gateway links a concrete Worker package",
    )
    require(
        "gateway-core" not in dependency_names(document, "goldenbee-client-cli"),
        "cli bypasses the SDK with gateway-core",
    )

    cli = next(
        package
        for package in document["packages"]
        if package["name"] == "goldenbee-client-cli" and package["id"] in document["workspace_members"]
    )
    direct = {
        dependency["name"]
        for dependency in cli["dependencies"]
        if dependency.get("kind") is None
    }
    require("sdk" in direct, "cli does not depend on the SDK")
    require("reqwest" not in direct, "cli owns a direct HTTP transport")
    require("adk" not in direct, "cli links native ADK")

    installer = dependency_names(document, "install")
    require(
        installer.isdisjoint({"gateway-core", "goldenbee-gateway", "sdk"}),
        "install depends on Gateway or the client SDK",
    )
    require(
        "runtime" not in dependency_names(document, "common"),
        "common depends on Worker execution",
    )

    for package in document["packages"]:
        if package["id"] in document["workspace_members"]:
            expected = Path(package["manifest_path"]).parent.name
            require(
                package["name"] == expected,
                f"crate {package['name']} does not match directory {expected}",
            )

    if failures:
        print("security-47 dependency acceptance: FAIL")
        for failure in failures:
            print(f"  - {failure}")
        return 1
    print(
        "security-47 dependency acceptance: PASS "
        "(SDK/common ADK-free, Worker runtime Gateway-free, registry off invocation path)"
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
