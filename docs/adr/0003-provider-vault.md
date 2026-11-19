---
status: accepted
date: 2026-11-19
---

# Gateway-owned provider vault

## Context

Workers need provider API keys to execute models, but distributing one key in
every Worker configuration leaks the key across hosts and makes rotation and
revocation manual. The Gateway already owns Worker approval, identity, and
delivery of control authority. Putting a model proxy in the Gateway would move
prompts, completions, streaming, and provider failures into the control plane,
which contradicts Worker-owned execution and expands the Gateway into a data
plane.

## Decision

The Gateway owns the provider catalog and the provider API keys encrypted at
rest. Workers receive only their assigned metadata and per-Worker encrypted
credential envelopes, decrypt locally, and use the existing native provider
registry.

- `gateway-core` owns the vault. The initial adapter is one encrypted JSON
  document written atomically with `fsync` and directory sync. The operations
  are a stable seam for a later shared database; multi-replica vault operation
  is not claimed until that adapter exists.
- At rest, each credential version is sealed with XChaCha20-Poly1305 under a
  master key read once from `GOLDENBEE_VAULT_MASTER_KEY` (base64, 32 bytes). The
  associated data binds provider id and credential version. The master key is
  never read from Gateway JSON.
- In transit, an assigned credential is sealed to the Worker's X25519 public
  key. The Gateway generates an ephemeral X25519 key, derives a symmetric key
  with HKDF-SHA256 and `goldenbee-provider-key-v1`, and seals with
  XChaCha20-Poly1305. The associated data binds provider, version, Worker id,
  instance, and envelope expiry. Envelopes expire in at most five minutes.
- A one-time pairing code is the only enrollment authority besides the Worker's
  existing control token. The code has at least 128 bits of randomness, is bound
  to one Worker id and instance, expires after five minutes, and is stored only
  as a SHA-256 hash. It is never used as an encryption key.
- Provider writes are write-only. Responses expose metadata, credential version,
  and a SHA-256 fingerprint only. No route, audit record, task, event, error, or
  log includes a plaintext credential.
- Discovery is a Gateway-side catalog operation, not a model call. Creating a
  provider, writing a credential, changing its endpoint or filter, or an explicit
  refresh reads the adapter's model list (`GET /models` for OpenRouter and
  OpenAI-compatible, `GET /api/tags` for Ollama). The credential is used only on
  that server-side request. The discovered identifiers are bounded, deduplicated,
  optionally intersected with `allowed_models`, and stored as non-secret
  metadata. Discovery failure leaves the provider not-ready with a stable,
  redacted code; no model is invented.
- Each ready provider keeps one preferred default model. Every usable model is
  also exposed to the Worker as a selectable alias `provider_id/model`. This
  selects a concrete model inside one Worker process; it never creates a Worker
  or process per model.
- Revocation and rotation mark older versions unusable for new envelopes.
  Already-running tasks keep the adapter they pinned; the Gateway never reaches
  into Worker execution.
- The Gateway does not proxy prompts, completions, streaming, or tool calls.
  Worker model execution remains Worker-owned.
- Default behavior is unchanged: without a `vault` section the Gateway has no
  provider routes’ backing store, and a Worker without `[models] source =
  "gateway"` keeps its `[providers]` and environment references.

## Threat model

- A Worker learns only credentials assigned to it. A compromised Worker cannot
  ask for another Worker's provider, because the bundle is filtered by approved
  identity and assignment.
- A stolen pairing code is useless without the matching Worker control token and
  expires quickly. Replay fails after one redemption.
- A stolen vault file is ciphertext; offline recovery still requires the master
  key. The file permissions are `0600`.
- **A Gateway compromise with permission to read the process environment or the
  KMS/secret store can read provider keys.** The vault protects storage and
  per-Worker transport, not a fully compromised Gateway.
- A file-based master key is a development and single-host convenience. It is
  not a production KMS. Production deployments should inject the master key from
  a host or cloud secret manager with an audited access policy.
- The Gateway operator and the Worker host are separate trust domains only when
  the control token, master key, and provider credential are not co-located in
  one readable environment.

## Consequences

- Existing environment-backed providers keep working. The vault is additive.
- The provider catalog, credential rotation, pairing, and assignment have one
  owner: the Gateway.
- Losing the master key makes stored credentials unrecoverable. Re-provision
  them through the write-only credential route; do not guess or rewrite the
  check value.
- The file store is single-writer. Do not run multiple Gateway replicas against
  one vault directory.
- New provider adapters, modality policy, and meter/billing policy remain
  separate decisions. This ADR covers credential custody and delivery only.
