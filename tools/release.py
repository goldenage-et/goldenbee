#!/usr/bin/env python3
"""Offline assembly over prebuilt, tested binaries; never compile or publish."""

import argparse
import ctypes
import hashlib
import json
from pathlib import Path
import os
import sys
import tempfile

sys.dont_write_bytecode = True
from launcher import Process


def run(args):
    with Process(args, os.getcwd(), limit=30, max_output=64 * 1024 * 1024) as child:
        child.child.stdin.close()
        child.finish()
        sys.stderr.buffer.write(child.output["stderr"])
        return bytes(child.output["stdout"])


def main():
    if ctypes.CDLL(None, use_errno=True).prctl(36, 1, 0, 0, 0) != 0:
        raise OSError(ctypes.get_errno(), "cannot establish child ownership")
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--bin-dir", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--build-evidence", type=Path, required=True,
                        help="actual toolchain/profile/command/reproducible record")
    parser.add_argument("--release", required=True)
    parser.add_argument("--drill", action="store_true", help="explicit non-production signing identity")
    parser.add_argument("--signing-key", type=Path)
    parser.add_argument("--trusted-key", type=Path)
    parser.add_argument("--signer")
    args = parser.parse_args()
    root = Path(__file__).resolve().parents[1]
    bins = args.bin_dir.resolve()
    installer = str(bins / "gb-install")
    build = json.loads(args.build_evidence.read_text())
    dirty = bool(run(["git", "-C", str(root), "status", "--porcelain", "--untracked-files=normal"]).strip())
    if dirty and not args.drill:
        parser.error("production assembly requires a clean source checkout")
    if args.output.exists():
        parser.error("output must be a new directory")
    if not args.drill and not (args.signing_key and args.trusted_key and args.signer):
        parser.error("supply provisioned signing key, independent trusted key, and signer")
    if args.drill and args.signing_key:
        parser.error("drill cannot use a production signing key")
    revision = run(["git", "-C", str(root), "rev-parse", "HEAD"]).decode().strip()
    archive = run(["git", "-C", str(root), "archive", "--format=tar", revision])
    with tempfile.TemporaryDirectory(prefix="release-") as temp:
        work = Path(temp)
        bundle = work / "source.bundle"
        run(["git", "-C", str(root), "bundle", "create", str(bundle), "HEAD"])
        key, trusted = args.signing_key, args.trusted_key
        if args.drill:
            key, trusted = work / "test-only.key", work / "test-only.pub"
            run([installer, "release", "keygen", "--test-only", "--key-file", str(key),
                 "--public-file", str(trusted)])
            build["reproducible"] = False
        document = {
            "schema": 1, "release": args.release,
            "signer": "test-only-release-drill" if args.drill else args.signer,
            "target": "x86_64-unknown-linux-gnu", "runtime": "0.1.0", "adk": "2.3.0",
            "protocol": "1.0", "protocol_min": "1.0", "protocol_max": "1.0",
            "source": {"revision": "git:" + revision,
                       "tree_digest": "sha256:" + hashlib.sha256(archive).hexdigest(), "dirty": dirty},
            "source_bundle": str(bundle), "build": build,
            "packages": [{"name": name, "version": "0.1.0", "binary": str(bins / name)}
                         for name in ("general", "coding", "knowledge")],
            "artifacts": [{"name": name, "kind": "executable", "path": str(bins / name)}
                          for name in ("gb", "gb-gateway", "gb-client", "gb-install", "gb-instance", "gb-composer")],
        }
        # Dependency/features/licenses are captured from locked Cargo metadata,
        # not guessed from a hand-maintained feature list. No compilation.
        metadata = work / "dependencies.json"
        metadata.write_bytes(run(["cargo", "metadata", "--locked", "--offline", "--format-version=1", "--manifest-path", str(root / "Cargo.toml")]))
        document["artifacts"].append({"name": "dependencies.json", "kind": "dependency-record", "path": str(metadata)})
        for name in ("gateway-v1.json", "worker-v1.json"):
            document["artifacts"].append({"name": name, "kind": "protocol-schema", "path": str(root / "schema" / name)})
        document["artifacts"].append({"name": "Cargo.lock", "kind": "dependency-lock", "path": str(root / "Cargo.lock")})
        compatibility = work / "compatibility.json"
        compatibility.write_text(json.dumps({
            "worker_runtime": "0.1.0", "native_adk": "2.3.0", "sdk": "0.1.0",
            "worker_protocol": "1.0", "gateway_protocol": "1.0",
            "target": "x86_64-unknown-linux-gnu", "launcher": "adk-cli 2.3.0",
            "features_record": "dependencies.json", "profile": "common::ProfileRevision",
            "schemas": {"worker_data": 4, "release_metadata": 1, "backup": 1, "event": 1},
            "unsupported": ["Windows/macOS", "schema downgrade", "full Launcher plugin-manager injection"],
            "scope": "test-only drill" if args.drill else "candidate; acceptance evidence required",
        }, indent=2))
        document["artifacts"].append({"name": "compatibility.json", "kind": "compatibility-record", "path": str(compatibility)})
        spec = work / "release.json"
        spec.write_text(json.dumps(document, indent=2, sort_keys=True))
        print(run([installer, "release", "assemble", "--spec", str(spec), "--output", str(args.output),
                   "--signing-key", str(key), "--json"]).decode(), end="")
        print(run([installer, "release", "verify", "--release", str(args.output),
                   "--trusted-key", str(trusted), "--json"]).decode(), end="")
        if args.drill:
            print("TEST-ONLY signing identity; no production release, clean-build, or reproducibility claim.")


if __name__ == "__main__":
    main()
