# Install a local Worker release

`gb-install` is on-host operator tooling. It is not a product Client transport,
and it never contacts Gateway. Installation is a separate operation from
instance creation, profile activation, startup, and registration.

The selected artifact format is recorded in
[ADR 0002](adr/0002-package-format.md): an offline OCI image layout with a
detached ed25519 signature over the release metadata.

## Commands

```sh
gb-install inspect --artifact ./release --trust ./trust.json [--advisories ./advisory.json] [--json]
gb-install install --artifact ./release --trust ./trust.json [--advisories ./advisory.json] --root /srv/goldenbee [--json]
gb-install trust --trust ./trust.json [--advisories ./advisory.json] [--root /srv/goldenbee] [--json]
gb-install list --root /srv/goldenbee [--json]

gb-install publish --artifact ./release --registry <url> --token <token> \
  --publisher <namespace> [--public] [--json]
gb-install visibility --registry <url> --token <token> \
  --package <publisher>/<name> (--public|--private) [--json]
gb-install discover --registry <url> [--package <publisher>/<name>] \
  [--capability <id>] [--target <triple>] [--runtime <version>] \
  [--adk <version>] [--protocol <version>] [--limit <n>] \
  [--trust <policy.json>] [--advisories <advisory.json>] [--json]
gb-install registry list --registry <url> --token <token> \
  --package <publisher>/<name> [--json]
gb-install registry show --registry <url> --token <token> \
  --package <publisher>/<name> --version <version> [--json]
gb-install registry install --registry <url> [--token <token>] \
  --package <publisher>/<name> --digest sha256:<hex> \
  --trust ./trust.json [--advisories ./advisory.json] --root /srv/goldenbee [--json]
```

`inspect` reads files and hashes only. It never executes the candidate binary.
`install` performs the same verification, then runs the constrained smoke check
inside the existing Linux sandbox and publishes the release under
`<root>/packages/sha256/<manifest-digest>/`. Registry commands use the same
verification path; see [private registry](registry.md). Metadata list/show
never executes a registry-supplied executable. Omitting `--token` on
`registry install` uses the optional anonymous public routes; the same local
trust, compatibility, permission, sandbox, and smoke checks still run. See
[public discovery](registry.md#optional-public-discovery).

## Trust policy

```json
{
  "trusted_signers": {
    "publisher": "<base64 ed25519 public key>"
  },
  "publisher_tiers": {"publisher": "verified"},
  "allowed_tiers": ["official", "verified"],
  "advisory_keys": [
    {"id": "advisories", "key": "<base64 ed25519 public key>", "not_after": null}
  ],
  "advisory_freshness_secs": 86400,
  "require_advisory": true,
  "rotation": {"advisory_key_overlap_secs": 0},
  "offline": {
    "installed_revoked": "quarantine",
    "installed_without_advisory": "restrict"
  },
  "targets": ["x86_64-unknown-linux-gnu"],
  "runtimes": ["0.1.0"],
  "adks": ["2.3.0"],
  "protocols": ["1.0"],
  "features": ["agents", "runner", "sessions", "server"],
  "approved_permissions": ["task:submit", "session:write"],
  "sensitive_permissions": ["credential:read", "exec:host", "net:outbound"]
}
```

The policy is owned by the installer host. Installing does not widen it.
A sensitive permission that is declared but not approved rejects the release
before any execution or credential provisioning. A signed release also binds
its source revision, target, runtime and ADK compatibility, protocol range,
declared grants, and build evidence. `protocols` contains versions the host
supports; a release is compatible when at least one supported version falls in
its inclusive `protocol_min..protocol_max` range.

When advisories are recorded or required, a new install also verifies the
signed advisory, exact release digest, signer tier, freshness window, and
monotonic generation before smoke execution. A revoked signer, release, or
generation cannot be replaced by older, tampered, or stale data. The policy file
must be owner-writable only. See [package trust and revocation](trust.md) for
the advisory format, rotation, offline decisions, and installed-instance
behavior. `gb-install trust` prints the effective roots, tiers, freshness,
rotation, offline policy, and verified advisory generation.

## Smoke contract

After provenance, digest, compatibility, and permission checks, the installer
runs the staged binary inside the OS sandbox with no network, a cleared
environment, an explicit workspace, and a bounded timeout:

- `<binary> probe --json` reports the package identity, compatibility, actual
  registered capabilities, hooks, launcher modes, and native server readiness.
- `<binary> dev` exercises the console launcher.
- `<binary> serve --probe-only` constructs the native server composition
  without binding a listener.

The reported capabilities, hooks, and launcher modes must equal signed
metadata. A signature never substitutes for isolation or explicit grants. If
the sandbox is unavailable, installation fails closed.

## Cache behavior

- The release identity is the OCI manifest digest.
- Staging is private and cleaned on failure. Publication is an atomic rename.
- Repeating the same digest converges. It does not overwrite another version.
- Seed assets are optional signed release content. The metadata declares each
  path and digest; a single verified catalog layer carries the bytes. Install
  materializes them below `<release>/seeds/`. Instance creation copies a seed
  only when the destination does not exist, so package defaults never overwrite
  user assets or profile revisions. Seed assets are not compiled capabilities
  and are never activated by installation.
- Redacted audit lines are appended to `<root>/audit.jsonl`.
