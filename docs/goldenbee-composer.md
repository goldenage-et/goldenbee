# Local host lifecycle

The Gateway owns local Worker lifecycle. There is no separate `gb-composer`
process, host credential, or composer configuration file. One `gb-gateway`
binary serves Client traffic, the Worker registry, task routing, package
installation, and local instance start/stop/restart.

## Ownership

| Component | Owns | Does not own |
| --- | --- | --- |
| `gb-gateway` | Caller scope, local package cache, instance root, allowed roots and grants, operation journal, Worker registration projection | Worker execution, user profiles outside the instance |
| `crates/instance` | Instance layout, provisioning, profile proxy, start/stop, upgrade, backup, quarantine | Gateway registry and caller policy |
| `crates/install` | Signature, compatibility, permission, sandbox smoke, immutable package cache | Host process lifecycle |
| SDK and CLI | Typed package and instance calls over Gateway HTTP | Filesystem access or a second transport |

## Routes

Mutating routes require `instance:admin`. Read routes accept `instance:read` or
`instance:admin`.

| Route | Purpose |
| --- | --- |
| `GET /v1/host` | Local host descriptor for compatibility and Gateway-to-Gateway calls |
| `POST /v1/packages/install` | Install a verified package from a folder or URL |
| `GET /v1/packages` | List verified packages |
| `POST /v1/instances` | Provision one verified digest, activate an authorized profile, start it |
| `GET /v1/instances` | List local instances |
| `GET /v1/instances/{instance}` | Digest, identity, lifecycle, health, compatibility |
| `POST /v1/instances/{instance}/start` | Start a stopped instance |
| `POST /v1/instances/{instance}/stop` | Stop a running instance |
| `POST /v1/instances/{instance}/restart` | Stop, start, and re-register |
| `POST /v1/instances/{instance}/upgrade` | Managed verified upgrade |
| `POST /v1/instances/{instance}/rollback` | Recover a compatible interrupted activation |
| `GET /v1/instances/{instance}/logs?tail=200` | Bounded process log tail |

The `/v1/hosts/{host}/...` routes remain as a compatibility alias. A root
Gateway can use them to proxy lifecycle operations to another Gateway that
exposes the same caller-authenticated instance routes.

## Gateway configuration

Host policy lives at the top level of `gateway.json`. Both roots enable local
management. Omit both to run a control-plane-only Gateway.

```json
{
  "listen": "127.0.0.1:8080",
  "data_dir": "/var/lib/goldenbee/gateway",
  "registry": "/var/lib/goldenbee/gateway-registry.json",
  "callers": [
    {
      "token": "<caller bearer>",
      "id": "operator",
      "namespace": "local",
      "scopes": ["instance:admin", "task:submit", "task:read"]
    }
  ],

  "host_id": "local",
  "instances_root": "/var/lib/goldenbee/instances",
  "packages_root": "/var/lib/goldenbee/packages",
  "trust_policy": "/etc/goldenbee/trust.json",
  "allowed_package_sources": ["/srv/goldenbee/releases"],
  "operations": ["provision", "inspect", "upgrade", "rollback", "start", "stop", "restart"],
  "allowed_instances": ["general-a"],
  "allowed_grants": ["task:submit", "task:read", "task:cancel", "session:write"],
  "allowed_profile_grants": ["task:submit", "task:read"],
  "allowed_config_roots": ["/etc/goldenbee/workers"],
  "allowed_workspace_roots": ["/srv/goldenbee/workspaces"],
  "allowed_secret_prefixes": ["secret://"],
  "allow_remote_listener": false
}
```

The Gateway resolves a package only inside its verified cache by
`sha256:<digest>`. A request cannot name a binary. A folder package source must
resolve under `allowed_package_sources`; a URL source is downloaded and
verified before cache publication.

An operator configuration selection must resolve under
`allowed_config_roots`; its `instance` and Worker control token must match the
request. A workspace must resolve under `allowed_workspace_roots`. Requested
grants, profile grants, and credential references are checked against the host
lists before any file is written.

## Package installation

`POST /v1/packages/install` accepts:

```json
{
  "source": {"kind": "folder", "path": "/srv/goldenbee/releases/general-0.1.0"},
  "package": "general",
  "expected_digest": "sha256:<64 hex>"
}
```

or:

```json
{
  "source": {"kind": "url", "url": "https://example.invalid/general-release.tar.gz"},
  "package": "general"
}
```

The Gateway verifies release checksums and signatures when the source is a
release root, runs `crates/install` compatibility and permission checks, runs
the constrained smoke probe, and publishes under
`<packages_root>/packages/sha256/<digest>/`.

If the trust policy has no trusted signer, install fails with
`install.signer_untrusted`. If it lacks the package runtime, ADK, protocol, or
permissions, install fails before publication.

## Process environment

The Gateway starts a Worker with a narrow environment: the shared
`GOLDENBEE_DELEGATION_SECRET` when configured, the instance data and sandbox
directories, and no inherited host environment. Provider credentials remain
references in instance configuration or the host secret provider.

## Gateway-to-Gateway

A Gateway with no local roots can list another Gateway as a host:

```json
{
  "hosts": [
    {
      "id": "host-b",
      "endpoint": "https://gateway-b.example:8443",
      "token": "<caller token accepted by gateway-b>",
      "instances": ["general-a"]
    }
  ]
}
```

The remote Gateway must accept the token with `instance:admin` and expose the
same `/v1/instances` contract. Local Client access still points at whichever
Gateway should own the operation.
