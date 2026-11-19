# Worker release registry client

The harness contains the registry client. The registry service is a separate
deployment. Distribution is not an invocation hop: installed
Workers keep no registry reference, and stopping the registry does not affect
execution.

```text
publisher ---> gb-install publish ---> gb-registry (separate service) ---> gb-install registry install
                                                          |
                                                          +--> existing Installer verification
```

The registry never executes a published binary. `registry list` and
`registry show` read metadata only; installation runs the existing signature,
digest, compatibility, permission, sandbox, and smoke path.

## Configuration

The private registry server owns its example configuration. Only token hashes are stored:

```json
{
  "root": "/srv/goldenbee/registry",
  "tokens": [
    {
      "hash": "sha256:<hash of publisher token>",
      "subject": "alice",
      "role": "publisher",
      "publisher": "acme",
      "repositories": ["acme/*"]
    }
  ],
  "max_blob_bytes": 536870912,
  "max_descriptor_bytes": 4194304
}
```

Roles are `publisher`, `reader`, and `admin`. A publisher may only push into
its own namespace and allowed repository patterns. A reader may list and
download only matching repositories. Missing or insufficient credentials are
`401`/`403`; no route accepts a token from a query string. Plain HTTP is
refused for non-loopback listeners because this transport does not implement
TLS. Use a TLS reverse proxy outside loopback.

Generate a token hash with the same SHA-256 convention:

```sh
printf '%s' "$TOKEN" | sha256sum
```

## Commands

```sh
gb-install publish --artifact ./release --registry http://127.0.0.1:8080 \
  --token "$TOKEN" --publisher acme [--json]

gb-install registry list --registry http://127.0.0.1:8080 \
  --token "$TOKEN" --package acme/general [--json]

gb-install registry show --registry http://127.0.0.1:8080 \
  --token "$TOKEN" --package acme/general --version 0.1.0 [--json]

gb-install registry install --registry http://127.0.0.1:8080 \
  --token "$TOKEN" --package acme/general --digest sha256:<hex> \
  --trust ./trust.json --root /srv/goldenbee [--json]
```

`GOLDENBEE_REGISTRY_TOKEN` may replace `--token`. Diagnostics never print the
token.

## Immutable identity

A published version is bound to one OCI manifest digest. Publishing the same
version with different bytes is rejected with `registry.version_conflict`.
Repeating an identical upload converges on the existing entry and does not
create a duplicate. The release descriptor is the exact signed `index.json`, so
the detached ed25519 signature annotation survives transport.

Signed release metadata binds:

- source revision and source-tree digest;
- artifact, metadata, binary, and source digests;
- target triple;
- Worker runtime and pinned ADK compatibility;
- management protocol minimum and maximum;
- declared grants from required permissions and per-capability permissions;
- build evidence, including its evidence digest.

The registry requires a non-empty source revision and build evidence digest at
publication. Installation policy still decides whether the signer, target,
runtime, ADK, protocol range, features, and permissions are acceptable.

## Storage and interruption

```text
<root>/
  blobs/sha256/<hex>
  repositories/<publisher>/<package>/versions.json
  repositories/<publisher>/<package>/releases/<manifest-digest>/index.json
  uploads/<uuid>/
  audit.jsonl
```

Uploads are staged under `uploads/<uuid>/`. Blobs are verified before they are
moved into content-addressed storage. The version index is written last and
atomically. A release becomes visible only after every referenced blob and the
descriptor verify. An interrupted upload is never listed, and startup removes
stale upload sessions.

Downloads are written to a temporary OCI layout and verified before
`Installer::install` sees them. A tampered stored blob is rejected at download
or by the installer's existing digest checks; the cache remains empty.

## Discovery

`GET /v1/packages` accepts optional `publisher`, `package`, `capability`,
`target`, `runtime`, `adk`, and `protocol` filters. Results are filtered by the
reader's scopes and return compatibility metadata plus the immutable digest.
`GET /v1/packages/<publisher>/<package>/versions` returns all versions for one
authorized repository.

## Optional public discovery

Public discovery is disabled by default. An operator opts in with
`public_discovery.enabled`, then a publisher explicitly marks one package
repository public. Publishing never makes a package public by itself.

```json
"public_discovery": {
  "enabled": true,
  "max_results": 50,
  "max_download_bytes": 67108864,
  "publisher_tiers": {"acme": "verified"},
  "revocations": [
    {
      "kind": "release",
      "package": "demo",
      "version": "1.0.0",
      "digest": "sha256:<manifest digest>",
      "reason": "withdrawn build"
    }
  ]
}
```

```sh
gb-install publish ... --public
gb-install visibility --registry <url> --token <token>   --package acme/general --public
gb-install discover --registry <url>   --capability task --target x86_64-unknown-linux-gnu --runtime 0.1.0 --json
```

`GET /v1/public/packages` accepts `publisher`, `package`, `capability`,
`target`, `runtime`, `adk`, `protocol`, and `limit`. `limit` is clamped to the
operator's `max_results`; the response reports `limit` and `truncated`.
Version, release-descriptor, and blob routes exist below
`/v1/public/packages/<publisher>/<name>/...`. Anonymous blob download is
limited to `max_download_bytes` and to digests that belong to an explicitly
public release. Private repositories, upload staging, and unrelated blobs stay
invisible.

Public release views carry the immutable version details, declared
capabilities and permissions, source revision, build evidence, and an explicit
`publisher_trust` object. The tier comes from registry operator policy, never
from a package's self-declared signer string. `safety_asserted` is always
`false` and `install_requires_local_trust_policy` is always `true`: signature
and visibility are provenance signals, not permission to execute.

`gb-install discover --trust <policy> --advisories <advisory>` additionally
shows local `trusted`, `untrusted`, `denied`, `revoked`, `quarantined`, or
`advisory_required` status computed from verified installer evidence.
Installation always repeats that verification locally. Public visibility never
changes instance grants, makes a registry an invocation dependency, or makes a
signed package safe to run outside the sandbox.

## Boundaries

- Runtime Profiles remain user settings; they are not packaged Worker
  identities.
- SDK libraries and Client applications are separate products.
- Worker Packages such as General and Echo are independently versioned and
  distributed; installing one does not install or replace another.
- The registry is not a Gateway route and never becomes an invocation hop.
