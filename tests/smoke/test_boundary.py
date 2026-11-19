"""Prove the public harness has no private-infrastructure dependency."""

from pathlib import Path
import re
import unittest

ROOT = Path(__file__).resolve().parents[2]


def manifests():
    for path in ROOT.rglob("Cargo.toml"):
        if any(part in {".git", ".gitnexus", "target", "node_modules"} for part in path.parts):
            continue
        yield path


class HarnessBoundaryTests(unittest.TestCase):
    def test_manifests_do_not_reference_infrastructure(self):
        for manifest in manifests():
            with self.subTest(manifest=manifest.relative_to(ROOT)):
                self.assertNotIn("infrastructure", manifest.read_text().lower())

    def test_path_dependencies_stay_inside_the_harness(self):
        for manifest in manifests():
            for value in re.findall(r'path\s*=\s*"([^"]+)"', manifest.read_text()):
                resolved = (manifest.parent / value).resolve()
                with self.subTest(manifest=manifest.relative_to(ROOT), path=value):
                    self.assertTrue(
                        resolved.is_relative_to(ROOT),
                        f"{manifest} points outside the harness: {resolved}",
                    )

    def test_registry_server_is_not_a_harness_package(self):
        self.assertFalse((ROOT / "apps" / "registry-server").exists())
        lock = (ROOT / "Cargo.lock").read_text()
        self.assertNotIn('name = "registry-server"', lock)

    def test_registry_client_does_not_link_server_transport(self):
        manifest = (ROOT / "crates" / "registry" / "Cargo.toml").read_text()
        self.assertNotIn("axum", manifest)
        self.assertNotIn("tokio", manifest)


if __name__ == "__main__":
    unittest.main()
