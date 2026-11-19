---
status: accepted
date: 2026-11-18
---

# Worker package artifact format

Ticket 39 needs a standard, auditable artifact that an operator can inspect and
install on a host with no registry access. The format must bind every byte,
carry signed metadata, support independent package releases, and leave a path
to private registry distribution in ticket 42. It must not invent a new archive
container when an accepted standard already meets those requirements.

## Decision

Use the **OCI image layout** as the offline release envelope, with an OCI
artifact manifest and registered Goldenbee media types.

- `oci-layout` declares image layout version `1.0.0`.
- `index.json` contains one manifest descriptor whose digest is the immutable
  release identity.
- The manifest config is the signed release metadata JSON.
- Layers are raw payload blobs with explicit media types: the executable, the
  source descriptor, and an optional seed-asset catalog. They are not wrapped
  in a second archive format.
- A detached ed25519 signature over `sha256` of the metadata config bytes is
  stored in the index descriptor annotation
  `org.goldenbee.signature.ed25519`.
- Every descriptor digest and every payload digest is verified before any
  execution. A signature is provenance, not permission to run without the
  sandbox and explicit grants.

The reader, writer, trust policy, cache, and smoke checks live in
`crates/install`. `common` carries only the framework-neutral metadata
representations (`PackageManifest`, `ReleaseMetadata`, `SeedAsset`).

## Compared options

| Option | Offline install | Immutable identity | Signature binding | Standard tooling | Verdict |
| --- | --- | --- | --- | --- | --- |
| OCI image layout directory | Yes, no registry required | Manifest digest and per-blob digests | Detached signature over the signed metadata config | OCI tools can inspect/copy the same layout; ticket 42 can push it unchanged | Selected |
| New tarball or zip plus JSON sidecar | Yes | Only if a new digest convention is defined | New sidecar/signature convention | No standard tool understands the release | Rejected as a speculative archive format |
| OCI registry only | No; requires network and a registry | Yes | Registry signatures or detached sidecar | Yes | Rejected for the local/offline requirement |
| Bare executable plus signature file | Yes | Binary digest only | Signature covers only the executable | No metadata, source, compatibility, or seed binding | Rejected as insufficient metadata inspection |

OCI image layout satisfies the offline requirement because the layout is a
directory of content-addressed blobs. A registry is an optional distribution
hop, not an installation-time dependency.

## Consequences

- Inspection parses only files and hashes. It never executes the candidate.
- Installation stages verified content, runs the constrained smoke check inside
  the existing Linux sandbox, and atomically publishes the directory under
  `packages/sha256/<hex>`. Repeating the same digest converges without
  overwriting another release or touching user profiles.
- The release identity is the OCI manifest digest, not a package/version string.
  Two versions of one package coexist; one version cannot be replaced in place.
- Seed assets are optional signed metadata. They are not compiled capabilities,
  are not required for installation, and are never activated as user
  configuration by the installer.
- Raw layer payloads keep this decision free of a tar/gzip sub-format. Media
  types are explicit and versioned, so a future publisher can add a layer only
  through a new format decision.

## Registry transport addendum (ticket 42)

A private registry stores the **exact `index.json` bytes** as an immutable,
content-addressed release descriptor keyed by the manifest digest. This keeps
the detached signature in the index descriptor annotation; a registry that
pushed only an OCI manifest would drop it and break `read_layout`.

Publication order is blobs, manifest, exact index descriptor, then the atomic
version index. A release is discoverable only after all of those verify.
Signed metadata gained defaulted fields for source revision, protocol minimum
and maximum, and build evidence. Schema 1 remains readable because every new
field has a default; older metadata falls back to `package.protocol` as its
protocol range. Publication to the private registry additionally requires a
non-empty source revision matching the source descriptor and a build-evidence
digest.

## Alternatives rejected

- A custom archive would freeze an unstandardized container and make private
  registry distribution in ticket 42 harder.
- A registry-only artifact violates the local and offline installation
  requirement in this ticket.
- A single binary with a detached signature does not bind source provenance,
  compatibility, permissions, or seed-asset declarations.
