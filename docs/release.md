# Release assembly and compatibility

Release assembly combines independent Worker Packages, the reusable
`gateway-core` control plane, the thin `goldenbee-gateway` adapter, the CLI
Client, and the client SDK. Artifact identity, compatibility, and deployment
limits are recorded here.
Exact acceptance status and remaining gaps are summarized in
[release notes](release-notes.md).

**Status: release acceptance is incomplete.** Assembly is not a clean-build,
reproducibility, deployment, or production-signing guarantee.

## Advertised Linux artifacts

The supported release target is `x86_64-unknown-linux-gnu`. No Windows or macOS
artifact is advertised.

| Artifact | Kind | Owner |
| --- | --- | --- |
| `general` | Worker Package | General Worker composition over shared `runtime` |
| `coding` | Worker Package | Coding Worker composition over shared `runtime` |
| `knowledge` | Worker Package | Knowledge Worker composition over shared `runtime` |
| `echo`, `graph` | Development packages | Not assembled by this release command |
| `gb-gateway` | Executable | `goldenbee-gateway` adapter over `gateway-core` |
| `gateway-core` | Library source in `source.tar` | Registry, routing, dispatch, streaming, administration, audit |
| `gb` | Operator executable | `goldenbee-cli` bootstrap and dispatcher |
| `gb-client` | Client executable | Product Client over `sdk` |
| `sdk` | Library source in `source.tar` | ADK-free Rust Gateway client |
| `gb-install` | Operator executable | Verified local release cache and release assembly |
| `gb-instance` | Operator executable | Instance layout, start/stop/inspect, upgrade, backup |
| `gb-composer` | Operator executable | Host-local Worker instance composer |
| `sdk-client` | Example source in `source.tar` | SDK usage example; not a product Client |

Worker Packages are distributed as signed OCI image layouts. The layout manifest
digest is the immutable release identity; see
[ADR 0002](adr/0002-package-format.md). The other executables travel in the same
release directory and are bound by the release manifest and signed
`SHA256SUMS`.

GitHub releases publish target-suffixed operator assets and a separate
`SHA256SUMS` for the `gb` bootstrap path:

```text
gb-x86_64-unknown-linux-gnu
gb-gateway-x86_64-unknown-linux-gnu
gb-composer-x86_64-unknown-linux-gnu
gb-client-x86_64-unknown-linux-gnu
install.sh
SHA256SUMS
```

The bootstrap is `install.sh` at the repository root. `gb i` downloads these
assets and verifies `SHA256SUMS`; `--from` remains available for local installs.

## Reproducible assembly

`tools/release.sh` accepts already-tested binaries. It never compiles, changes
Cargo output directories, or uploads artifacts. Compilation-only `cargo run`
calls are not part of release assembly. Clean locked build evidence remains a
separate required gate, subject to the repository's command restrictions.

```sh
tools/release.sh --bin-dir /path/to/tested/binaries \
  --output /path/to/new/release --build-evidence /path/to/build.json \
  --release 2026.11.19-drill --drill
```

The script:

1. reads prebuilt `general`, `coding`, `knowledge`, `gb`, `gb-gateway`, `gb-client`,
   `gb-install`, `gb-instance`, and `gb-composer`;
2. records a self-contained Git bundle, its commit's archive digest, the dirty
   flag, and the supplied build evidence;
3. probes each Worker Package so signed capabilities, permissions, features,
   hooks, and Launcher modes come from the binary itself;
4. writes one signed OCI layout per Worker Package plus a release manifest,
   provenance record, and `SHA256SUMS`;
5. records locked Cargo dependencies, resolved features, checksums, and license
   metadata, then signs the complete file inventory with ed25519.

`--drill` generates an explicitly test-only identity in a temporary directory
outside the output. The private key is never copied into the release. Other
assemblies require `--signing-key`, `--trusted-key`, and `--signer`, with a clean
source checkout. Production key provisioning stays outside this tool.

The build JSON records `toolchain`, `profile`, `command`, and `reproducible`.
Use the actual command and result. A drill always records `reproducible: false`.
The source bundle proves the archive matches the recorded Git commit. It does
not prove that supplied binaries came from that commit. That claim requires
reviewed builder evidence and an independent rebuild.

Verify without executing any package binary:

```sh
gb-install release verify --release target/release-dist \
  --trusted-key /path/to/independently-trusted-public-key --json
gb-install inspect --artifact target/release-dist/packages/general-0.1.0-x86_64-unknown-linux-gnu \
  --trust ./trust.json --json
```

Never establish trust from `signer-public-key.txt` inside the candidate release.
Verification authenticates the inventory against the external key, rejects
extra files and symlinks, verifies package signatures, and reconstructs the
archive from the bundled Git commit. It does not execute candidate binaries.
Publisher-side assembly runs each `probe` with a 20-second process-group bound.

## Compatibility record

Runtime/ADK compatibility and Client/Gateway protocol compatibility are separate
claims. A compatible Worker runtime says nothing about SDK or Gateway protocol
compatibility.

| Dimension | Release value |
| --- | --- |
| Worker Package version | `0.1.0` |
| Shared Worker runtime | `0.1.0` |
| Pinned ADK | `adk-rust` and native crates `2.3.0` |
| Worker management protocol | `1.0` |
| Gateway client protocol | `1.0` |
| Client SDK | `sdk 0.1.0`, Rust |
| Target | `x86_64-unknown-linux-gnu` |
| Native Launcher | Full `adk_cli::Launcher` through `runtime/launcher-cli` (`adk-rust/cli`) |
| Worker runtime features | `agents`, `runner`, `sessions`, `server`, `plugin`, `skills`, `a2a`, `openrouter`, `ollama`, `openai` |
| Coding package features | `coding`, `devtools` |
| Knowledge package features | `rag` |
| Profile representation | Versioned profile revision schema in `common::ProfileRevision`; no separate profile schema number |
| Persistent Worker data | `DATA_SCHEMA = 4` |
| Release metadata | `RELEASE_METADATA_SCHEMA = 1` |
| Backup representation | `BACKUP_SCHEMA = 1` |
| Event stream | `EVENT_SCHEMA_VERSION = 1` |
| Gateway/Worker JSON schemas | `schema/gateway-v1.json`, `schema/worker-v1.json` |

A host installer accepts a release only when target, runtime, ADK, protocol
range, required features, permissions, and signer trust all match its policy.
Protocol matching accepts a supported host version inside the release's
inclusive `protocol_min..protocol_max` range.

### Unsupported or unverified behavior

- No product web Client, desktop Client, unified Client application, or
  marketplace UI is released. Native Launcher web serving is a development tool,
  not the product web Client.
- No cross-version downgrade across changed persistent schemas. Restore the
  matching binary or a backup.
- No Windows or macOS artifacts, hosted-provider credential, or public TLS
  listener is shipped. TLS termination remains an operator reverse-proxy or
  service-mesh responsibility.
- The bundled JSON Gateway stores are single-writer. Multi-replica operation
  requires the Postgres store and is not proven by depending on
  `gateway-core`.
- `gateway-core` is a library. Its presence in a business backend does not give
  that backend users, authentication, payments, entitlements, or tenant policy;
  those remain outside the core.
- Live model quality, hosted-provider behavior, and every optional capability
  are not release guarantees. See the release notes for the tested boundary.
- Pinned full Launcher constructs its own Runner without a `PluginManager`
  injection API. Worker task hooks apply to durable task admission, not arbitrary
  native console turns. Historical `/v1` hook/plugin evidence does not certify
  full Launcher hook equivalence. This acceptance criterion remains open.

## Deployment modes

```text
standalone-server: Client -> SDK -> goldenbee-gateway -> Worker
direct-core:       business backend -> gateway-core -> Worker
```

Both modes use separate Worker processes. The standalone adapter owns the HTTP
listener, caller bearer-token authentication, operator configuration, JSON/SSE
serialization, and signal handling. The core owns Worker approvals, leases,
routing, dispatch identity and idempotency, delegation, discovery,
administration, audit, and retained/live event forwarding. The adapter does not
duplicate those policies.

A business backend that uses `gateway-core` directly supplies an
already-authenticated `Principal`, scopes, limits, and an optional Worker
allow-list. It submits, streams, cancels, discovers, and administers Workers
without an extra Gateway network hop. Users, authentication, payments,
entitlements, business tenant policy, and end-user streaming stay in the
business application.

## Registry-disabled operation

Installation and all local execution work with no registry process and no
registry network route. The optional registry is neither an invocation
dependency nor a start-up dependency. Published releases are installed from a
local OCI layout, `gb-instance` creates instances from the verified cache, and
Gateway routing and team configuration use approved Workers rather than registry
lookup.

## Multi-replica boundary

Single-node release guidance applies to the file-backed Gateway stores. Multiple
Gateway replicas require the shared Postgres store, a load-balanced reachable
endpoint, per-replica stable identity, and the operational migration, backup,
and rollback procedure in [Operations](operations.md#shared-gateway-store).
The active-active claims and their limits are recorded by ticket 56 and its
multi-process evidence. Library use alone is not high availability and does not
make per-process in-memory registries safe.
