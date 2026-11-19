# Ticket 47 security acceptance suite

This suite proves the distributed security and failure invariants across real
Client, Gateway, and Worker process boundaries. The new cross-process tests live
beside their owning slices:

- `apps/goldenbee-gateway/tests/security.rs` — namespace isolation, hostile raw
  requests, partition/provider/expiry/storage bounds, and forced Gateway
  termination around acceptance.
- `workers/general/tests/security.rs` — hostile model tool calls, hook/native
  callback policy, and identical packages with independent assets and revisions.

Detailed per-feature contracts stay authoritative in their existing suites:
`approval`, `submit`, `effect`, `recovery`, `trust`, `fleet`, `hooks`,
`config`, `peer`, `mcp_remote`, `sandbox`, `knowledge`, and the dependency
smoke checks.

## Reproducible command

```sh
TMPDIR=/var/tmp/goldenbee-47 tests/security/run.sh
```

Set `GBB_SHARED=1` to run each command through `gbb --shared --wait 900` on a
host with enough `/tmp` quota for C. The script compiles test binaries with
`cargo test --no-run`; it never runs `cargo build`. Logs are kept under
`target/security-47/`. The final line is a deterministic pass/fail summary.

## Evidence

- The suite prints `security-47:` lines with task identities, observed Worker
  task counts, artifact counts, hook counts, and recovery results. It never
  prints bearer tokens, control tokens, or the delegation secret.
- Protected effect counts come from the real `effect-fixture` ledger used by the
  approval and effect suites; the tests assert the external `effects.log` count
  before and after duplicate or interrupted decisions.
- Existing dependency and Launcher checks remain the authority for the ADK-free
  SDK/`common` boundary and Gateway-absent development. This suite reuses their
  commands instead of duplicating crate metadata logic.
