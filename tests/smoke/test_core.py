"""Check executable surfaces and independent Cargo dependency boundaries."""

from functools import cache
import json
from pathlib import Path
import subprocess
import unittest


ROOT = Path(__file__).resolve().parents[2]


def cargo(*arguments):
    result = subprocess.run(
        ["cargo", *arguments],
        cwd=ROOT,
        capture_output=True,
        text=True,
        timeout=300,
        check=False,
    )
    if result.returncode:
        raise RuntimeError(f"cargo {' '.join(arguments)} failed:\n{result.stdout}\n{result.stderr}")
    return result.stdout


@cache
def workspace():
    artifacts = cargo(
        "build", "--workspace", "--bins", "--all-features", "--locked", "--offline",
        "--message-format=json",
    )
    metadata = json.loads(cargo("metadata", "--format-version=1", "--all-features", "--locked", "--offline"))
    packages = {package["id"]: package for package in metadata["packages"]}
    members = set(metadata["workspace_members"])
    binaries = {}
    for line in artifacts.splitlines():
        artifact = json.loads(line)
        if (
            artifact.get("reason") == "compiler-artifact"
            and artifact["package_id"] in members
            and artifact.get("executable")
            and "bin" in artifact["target"]["kind"]
            and artifact["target"].get("test") is not False
        ):
            name = artifact["target"]["name"]
            binaries[name] = (Path(artifact["executable"]), packages[artifact["package_id"]]["version"])
    return metadata, binaries


def dependency_names(metadata, consumer):
    packages = {package["id"]: package["name"] for package in metadata["packages"]}
    nodes = {
        node["id"]: [dependency["pkg"] for dependency in node["deps"]]
        for node in metadata["resolve"]["nodes"]
    }
    member = next(
        package for package in metadata["packages"]
        if package["name"] == consumer and package["id"] in metadata["workspace_members"]
    )
    pending = list(nodes[member["id"]])
    found = set()
    while pending:
        identity = pending.pop()
        if identity not in found:
            found.add(identity)
            pending.extend(nodes[identity])
    return {packages[identity] for identity in found}


class CommandSurfaceTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        metadata, cls.binaries = workspace()
        expected = {
            target["name"]
            for package in metadata["packages"]
            if package["id"] in metadata["workspace_members"]
            for target in package["targets"]
            if "bin" in target["kind"] and target.get("test") is not False
        }
        if not expected or set(cls.binaries) != expected:
            raise AssertionError("Build artifacts must cover all declared workspace executables")

    def run_binary(self, executable, *arguments):
        return subprocess.run(
            [str(executable), *arguments],
            cwd=ROOT,
            capture_output=True,
            text=True,
            timeout=10,
            check=False,
        )

    def test_help_describes_supported_commands(self):
        commands = {
            "general": "run",
            "coding": "run",
            "echo": "run",
            "graph": "run",
            "knowledge": "run",
            "gb": "install",
            "gb-gateway": "serve",
            "gb-composer": "serve",
            "gb-client": "submit",
            "gb-install": "inspect",
            "gb-instance": "create",
            "sdk-client": "--gateway",
        }
        for name, (executable, _) in self.binaries.items():
            for flag in ("--help", "-h"):
                with self.subTest(binary=name, flag=flag):
                    result = self.run_binary(executable, flag)
                    self.assertEqual(result.returncode, 0, result.stderr)
                    self.assertIn(f"Usage: {name}", result.stdout)
                    self.assertIn(commands[name], result.stdout)
                    self.assertNotIn("Scaffold only.", result.stdout)
                    self.assertEqual(result.stderr, "")

    def test_version_matches_package_metadata(self):
        for name, (executable, version) in self.binaries.items():
            for flag in ("--version", "-V"):
                with self.subTest(binary=name, flag=flag):
                    result = self.run_binary(executable, flag)
                    self.assertEqual(result.returncode, 0, result.stderr)
                    self.assertEqual(result.stdout, f"{name} {version}\n")
                    self.assertEqual(result.stderr, "")

    def test_unknown_commands_are_rejected_without_execution(self):
        cases = {
            "general": ((), ("task", "submit"), ("--unknown",), ("--help", "extra")),
            "coding": ((), ("task", "submit"), ("--unknown",), ("--help", "extra")),
            "echo": ((), ("task", "submit"), ("--unknown",), ("--help", "extra")),
            "graph": ((), ("task", "submit"), ("--unknown",), ("--help", "extra")),
            "knowledge": ((), ("task", "submit"), ("--unknown",), ("--help", "extra")),
            "gb": (("bogus",), ("--unknown",)),
            "gb-gateway": ((), ("task", "submit"), ("--unknown",), ("--help", "extra")),
            "gb-composer": ((), ("task", "submit"), ("--unknown",), ("--help", "extra")),
            "gb-client": (("task", "submit"), ("--unknown",), ("--help", "extra")),
            "gb-install": (("task", "submit"), ("--unknown",), ("--help", "extra")),
            "gb-instance": (("task", "submit"), ("--unknown",), ("--help", "extra")),
            "sdk-client": ((), ("--unknown",)),
        }
        for name, (executable, _) in self.binaries.items():
            for arguments in cases[name]:
                with self.subTest(binary=name, arguments=arguments):
                    result = self.run_binary(executable, *arguments)
                    self.assertNotEqual(result.returncode, 0)
                    self.assertEqual(result.stdout, "")
                    self.assertIn(f"{name}:", result.stderr)

    def test_unknown_arguments_are_not_echoed_into_diagnostics(self):
        secret = "scaffold-secret-canary-do-not-log"
        for name, (executable, _) in self.binaries.items():
            with self.subTest(binary=name):
                result = self.run_binary(executable, "--definitely-not-an-option", secret)
                self.assertNotEqual(result.returncode, 0)
                self.assertNotIn(secret, result.stdout + result.stderr)


class DependencyBoundaryTests(unittest.TestCase):
    def test_contracts_and_clients_do_not_link_execution(self):
        metadata, _ = workspace()
        for consumer in ("common", "sdk", "goldenbee-client-cli", "goldenbee-cli", "sdk-client", "gateway-core", "goldenbee-gateway", "registry"):
            with self.subTest(consumer=consumer):
                dependencies = dependency_names(metadata, consumer)
                self.assertFalse(
                    {name for name in dependencies if name == "adk" or name.startswith("adk-")},
                    f"{consumer} must not link native ADK implementation crates",
                )
                self.assertTrue(
                    dependencies.isdisjoint({"runtime", "general", "knowledge", "install"}),
                    f"{consumer} must not link Worker execution or installation",
                )

    def test_registry_is_not_an_invocation_dependency(self):
        metadata, _ = workspace()
        for consumer in ("registry",):
            with self.subTest(consumer=consumer):
                dependencies = dependency_names(metadata, consumer)
                self.assertTrue(dependencies.isdisjoint({"runtime", "install", "general", "echo"}))
                self.assertFalse(
                    {name for name in dependencies if name == "adk" or name.startswith("adk-")},
                    f"{consumer} must not link native ADK execution",
                )

    def test_worker_runtime_remains_independent_of_gateway(self):
        metadata, _ = workspace()
        for consumer in ("general", "knowledge", "runtime", "sdk", "install", "instance"):
            with self.subTest(consumer=consumer):
                self.assertTrue(
                    dependency_names(metadata, consumer).isdisjoint({"gateway-core", "goldenbee-gateway"}),
                    f"{consumer} must not link the Gateway control plane or adapter",
                )

    def test_gateway_adapters_do_not_link_worker_runtime(self):
        metadata, _ = workspace()
        for consumer in ("gateway-core", "goldenbee-gateway"):
            with self.subTest(consumer=consumer):
                dependencies = dependency_names(metadata, consumer)
                self.assertTrue(dependencies.isdisjoint({"runtime", "general", "knowledge", "install"}))
                self.assertFalse(
                    {name for name in dependencies if name == "adk" or name.startswith("adk-")},
                    f"{consumer} must not link native ADK execution",
                )
        # sdk is only a test-only consumer of the adapter's public HTTP API.
        package = next(
            package for package in metadata["packages"]
            if package["name"] == "goldenbee-gateway"
            and package["id"] in metadata["workspace_members"]
        )
        for dependency in package["dependencies"]:
            if dependency["name"] == "sdk":
                self.assertEqual(
                    dependency.get("kind"),
                    "dev",
                    "goldenbee-gateway must use the SDK only from its tests",
                )

    def test_gateway_core_has_no_adapter_transport_or_business_edge(self):
        metadata, _ = workspace()
        dependencies = dependency_names(metadata, "gateway-core")
        self.assertTrue(
            dependencies.isdisjoint(
                {"axum", "goldenbee-client-cli", "sdk", "goldenbee-gateway"}
            ),
            "gateway-core must stay independent of HTTP adapters, the CLI, the SDK, and business policy",
        )
        # The adapter is allowed to depend on the core, but never the reverse.
        self.assertIn("gateway-core", dependency_names(metadata, "goldenbee-gateway"))
        self.assertNotIn("gateway-core", dependency_names(metadata, "goldenbee-client-cli"))

    def test_cli_uses_the_sdk_instead_of_its_own_transport(self):
        metadata, _ = workspace()
        cli = next(
            package for package in metadata["packages"]
            if package["name"] == "goldenbee-client-cli" and package["id"] in metadata["workspace_members"]
        )
        direct = {
            dependency["name"]
            for dependency in cli["dependencies"]
            if dependency.get("kind") is None
        }
        self.assertIn("sdk", direct)
        self.assertNotIn("reqwest", direct)
        self.assertNotIn("adk", direct)

    def test_installer_owns_trust_and_stays_worker_side(self):
        metadata, _ = workspace()
        dependencies = dependency_names(metadata, "install")
        self.assertIn("runtime", dependencies)
        self.assertTrue(dependencies.isdisjoint({"gateway-core", "goldenbee-gateway", "sdk"}))
        self.assertNotIn("runtime", dependency_names(metadata, "common"))

    def test_crate_names_match_their_directories(self):
        metadata, _ = workspace()
        for package in metadata["packages"]:
            if package["id"] in metadata["workspace_members"]:
                with self.subTest(package=package["name"]):
                    self.assertEqual(package["name"], Path(package["manifest_path"]).parent.name)


if __name__ == "__main__":
    unittest.main()
