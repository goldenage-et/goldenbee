# Package trust, signed advisories, and revocation

`gb-install` owns on-host trust roots, verified distribution evidence, and
installer enforcement. `gb-instance` owns the decision for each installed
instance. Registry distribution and Gateway transfer metadata but never become
trust authorities. A valid signature proves provenance, not safety: verified
packages still run only inside the existing OS sandbox and with explicit
instance grants.

See also [installation](install.md), [instances](instance.md), and
[ADR 0002](adr/0002-package-format.md).

## Recorded policy before revocation claims

An operator records these decisions in one JSON policy file before relying on
an advisory. A policy that requires advisories must also record keys and a
freshness window; otherwise loading fails closed.

| Decision | Field | Meaning |
| --- | --- | --- |
| Signer roots | `trusted_signers` | Signer identity to base64 ed25519 release key |
| Publisher tiers | `publisher_tiers` | `official`, `verified`, `community`, `unverified`, `revoked`, or `quarantined` |
| Allowed tiers | `allowed_tiers` | If non-empty, only these tiers are acceptable for new installs |
| Advisory authority | `advisory_keys` | Authority id, public key, optional millisecond `not_after` |
| Advisory freshness | `advisory_freshness_secs` | Maximum accepted age for new trust-sensitive installs |
| Required evidence | `require_advisory` | A new install without a verified advisory is denied |
| Key rotation | `rotation.advisory_key_overlap_secs` | Extra acceptance time after a key's `not_after` |
| Offline policy | `offline.installed_revoked` | `continue`, `restrict`, or `quarantine` for already installed releases that current evidence revokes |
| Offline policy | `offline.installed_without_advisory` | Action when required evidence is unavailable for an already installed release |

`config/trust.example.json` is a shape example with placeholder keys. Replace
the keys with operator-controlled public keys and keep the file owner-writable
only. `TrustPolicy::load` rejects group- or world-writable policy files with
`trust.policy_insecure`.

Revoked or quarantined tiers are always denied. A missing tier is reported as
`unverified`. Signature verification, target, runtime, ADK, protocol, feature,
and permission checks are unchanged from [installation](install.md).

## Signed advisory

A signed advisory is a detached ed25519 signature over one canonical JSON
payload. The envelope contains the authority identity, standard base64 payload
bytes, and a standard base64 signature. The signature covers the `sha256` digest
of the exact payload bytes.

```json
{
  "schema": 1,
  "authority": "advisories",
  "payload": "<base64 AdvisoryPayload bytes>",
  "signature": "<base64 ed25519 signature over the payload digest>"
}
```

Payload:

```json
{
  "schema": 1,
  "generation": 4,
  "issued_at": 1763500000000,
  "expires_at": 1763586400000,
  "revocations": [
    {
      "kind": "release",
      "package": "coding-worker",
      "version": "1.0.0",
      "digest": "sha256:<manifest digest>",
      "signer": "",
      "reason": "withdrawn build"
    },
    {
      "kind": "signer",
      "package": "",
      "version": "",
      "digest": "",
      "signer": "publisher",
      "reason": "compromised key"
    }
  ]
}
```

Release revocations are exact-source-digest matches; package and version may
narrow but do not replace the digest. Signer revocations match the signer
identity. Both reject install before smoke execution and before credential
provisioning.

## Verification order

1. Read the immutable OCI layout and verify every digest and the detached
   release signature against `trusted_signers`.
2. Check the publisher tier and compatibility/permission policy.
3. Verify the advisory envelope against `advisory_keys`, enforcing the key
   retirement cutoff and the recorded freshness window.
4. Reject a generation lower than the accepted generation, or the same
   generation with a different payload digest (`install.advisory_replay`).
5. Apply release and signer revocations to the exact artifact digest and signer.
6. Persist the accepted generation and payload digest in
   `<cache>/trust/advisory.json` with owner-only permissions.
7. Run the constrained smoke check, including both native Launcher development
   modes the release declares, then publish the immutable release.

A tampered signature, malformed payload, stale or expired advisory, retired key,
replayed generation, missing required advisory, revoked release, revoked signer,
or denied tier leaves the previously accepted advisory state unchanged and
executes nothing. Redacted audit lines record the rejection code and reason.

## Installed-instance decisions

`gb-instance trust` applies local evidence to one exact instance and exact
release digest:

```sh
gb-install trust --trust ./trust.json --advisories ./advisory.json --json
gb-install trust --trust ./trust.json --advisories ./advisory.json --root /srv/goldenbee --json

gb-instance trust --root /srv/instances --instance reviewer \
  --trust ./trust.json --advisories ./advisory.json --json
gb-instance inspect --root /srv/instances --instance reviewer --json
```

- `continue`: installed execution remains available. New installs and upgrades
  still require fresh evidence.
- `restrict`: already running work is left running, but new starts and profile
  mutations are denied with `instance.trust_restricted`.
- `quarantine`: the recorded process is stopped and new starts or profile
  mutations are denied with `instance.quarantined`.

The decision, redacted reason, advisory generation, and decision time are
recorded in `state.json` and shown by `gb-instance inspect`. A different
instance created from the same digest keeps its own decision.

Applying a valid, newer advisory through the operator command can change a
quarantine. Starting, stopping, inspecting, or proxying a profile cannot clear
it, and a tampered or unauthorized policy file is rejected before any change.
No SDK, Gateway, or Client route mutates trust roots, advisory state, or
instance quarantine; those remain local operator files and commands.

## Offline behavior

Installed execution never reads a registry. `gb-instance start` and profile
commands work from the verified release and instance files only, so a registry
outage does not stop installed Workers or teams.

A new install or update is trust-sensitive. Without the configured fresh signed
advisory (or with an unavailable registry for a registry install), it is denied.
Missing evidence never silently disables verification or widens grants. For an
already installed release, the recorded `offline` action is used instead:
`continue`, `restrict`, or `quarantine`. The default is `continue` so an
unconfigured host does not stop existing work; an operator who needs stricter
offline behavior must record it explicitly.

## Rotation

- Advisory authority keys can overlap by recording both the retiring and the
  replacement key with the same authority id. Once the retiring key passes
  `not_after` plus the overlap window, signatures from it are refused with
  `install.advisory_key_retired`.
- Release-signer rotation replaces the public key under the signer identity or
  records a new signer identity. A release signed by a removed key fails
  `install.signature_invalid`; there is no implicit old-key acceptance.
- Rotation never lowers the accepted advisory generation, so an old advisory
  cannot reintroduce a revoked release after rotation.

## Related evidence

Installer tests cover compromised signer, revoked digest, exact-digest scope,
tampered/stale/replay advisory, authority-key rotation, required-evidence
absence, denied publisher tier, insecure policy files, and an unavailable
registry with a still-running installed release. Instance tests cover
quarantine, restrict, continue, independent instances from one digest, replay,
and tamper rejection. The existing smoke test proves a trusted signature does
not remove the fixed address-space and allocator sandbox.
