# Goldenbee 0.1.0 release notes

This is an incomplete release candidate of the Goldenbee agent harness.
The notes summarize earlier component runs and the architecture work leading to
this candidate. Marketplace discovery is present but is not required for
release.

## Release shape

- Independent Worker Packages: General, Coding, Knowledge, plus the smaller
  Echo and Graph packages.
- Reusable `gateway-core` control plane library.
- Thin `goldenbee-gateway` HTTP adapter (`gb-gateway` executable).
- `gb-client` product Client over the Rust `sdk`.
- Local operator tools: `gb-install`, `gb-instance`, and the Gateway-owned local
  host lifecycle.
- No required business server, no mandatory UI, and no second ADK execution
  engine. `common` and `sdk` stay free of native ADK execution.

## Component status

These notes describe earlier component runs. They do not certify current
release artifacts, independent clean builds, or the final deployment drill.

### Execution and durability

- Standalone Worker execution through the shared runtime and pinned native ADK
  Runner.
- Durable task handles, idempotent Gateway submission, retained replay, and
  resumable event streams after restart.
- Independent binaries for General, Coding, and Knowledge with operator-selected
  configuration and assets outside the install directory.
- Crash-safe file publication, bounded drain, restart recovery, and Linux
  autostart units.

### Gateway and Client

- Conflict-checked Worker registration and leases, explicit routing,
  caller-scoped discovery, administration, audit, idempotency, cancellation, and
  streaming.
- Direct `gateway-core` use by a business backend with an already-authenticated
  principal, without an extra network hop, and standalone adapter equivalence
  for submission, discovery, authorization, streaming, cancellation, lease
  expiry, restart, and redaction require a paired release conformance gate.
  The direct-core test covers submission, retained events, streaming, and
  cancellation, not that complete equivalence matrix.
- CLI product journey through `sdk` to Gateway, including full-screen chat,
  profiles, approvals, Worker administration, and diagnostics.

### Capabilities

- Runtime Profiles with immutable revisions, skills, prompt and agent assets,
  native workflow templates, and revision pinning.
- Local and remote MCP authorization, execution budgets, context compaction,
  memory, knowledge provenance, document ingestion, sandboxed commands, isolated
  Coding worktrees, browser profiles, fenced human takeover, artifacts, and
  retention/erasure.
- The pinned full Launcher feature (`adk-rust/cli` / `adk_cli::Launcher`) is
  enabled by `runtime/launcher-cli`. Bounded CLI and native HTTP checks drive
  each package's real composition. Native hook and plugin limitations remain.

### Security and operations

- Caller authentication, namespace isolation, scope enforcement, delegation
  binding, secret redaction, and hostile-protocol rejection.
- Measured execution and relay limits with a disclosed host and workload.
- Backup, restore, migration refusal, quarantine, and upgrade rollback limits.

## Unknown or not guaranteed

- Final clean-install, upgrade, mixed Coding/Knowledge team, artifact, approval,
  live inference, and restart drills against one signed assembled artifact set.
- Independent locked clean-build reproducibility and production signing.
- Full Launcher plugin injection and Worker task-hook equivalence. The pinned
  Launcher has no plugin-manager setter. Native development routes are not the
  durable `/v1` task-admission path.

- Live hosted-provider behavior, model quality, provider outages, and cost.
  A local live Ollama run is recorded separately; its model
  output channel is reasoning-oriented and is not a general quality claim.
- Power-loss durability, filesystem corruption, disk-full exhaustion, and
  kernel or hardware failure. Process-restart durability is tested.
- Public TLS, certificate rotation, DDoS protection, and internet exposure.
  Listeners are loopback/private by default; TLS is an operator boundary.
- Multi-replica Gateway safety without the shared Postgres store. The JSON
  stores are single-writer. See
  [Operations](operations.md#shared-gateway-store) and ticket 56.
- Windows and macOS artifacts. Only `x86_64-unknown-linux-gnu` is advertised.
- Cross-version downgrade across changed persistent schemas.
- Product web and desktop Clients, billing, marketplace UI, and public
  storefront behavior.

## Optional features

Public Marketplace discovery is optional, disabled by default, and not
a release prerequisite. Workers, teams, and local installation work with the
registry process absent or unreachable. Anonymous discovery never grants trust
or installation authority; the local signer policy remains authoritative.

## Upgrade notes

Read [Release compatibility](release.md) and [Operations](operations.md) before
upgrading. Keep Worker state and configuration in place, install the verified
candidate first, review `gb-instance upgrade` without `--apply`, then drain and
activate one instance at a time. A failed readiness or capability check restores
the previous binary record and data snapshot. Data that cannot be restored is
moved to quarantine, which is not a successful rollback.
