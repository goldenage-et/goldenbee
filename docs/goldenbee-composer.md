# Host deployment composers

A host composer is the only process that performs privileged Worker lifecycle
actions for fleet management. The Gateway orchestrates an authorized operation
and keeps the registry projection; it never resolves a host path, reads Worker
asset directories, or accepts an arbitrary executable. Local operator
management through `gb-instance` remains independent of Gateway and composers.

## Ownership

| Component | Owns | Does not own |
| --- | --- | --- |
| Gateway `gateway-core` | Caller scope, host selection, instance allow-list, lifecycle operation journal, Worker registration projection | Host credentials, filesystem roots, package cache, privileged installation |
| `apps/goldenbee-composer` | One host's verified package cache, instance root, allowed configuration/workspace roots, grants, composer credentials, operation journal | Gateway registry, task routing, caller identity, Client presentation |
| `crates/instance` | Instance layout, provisioning, profile proxy, start/stop, upgrade, backup, quarantine | Fleet orchestration, host selection, caller policy |
| SDK and CLI | Typed lifecycle calls over Gateway HTTP | Host credentials, filesystem access, a second transport |

## Composer API

All routes require a host-scoped bearer credential. Mutating routes require the
`instance:admin` scope. `GET` inspection accepts `instance:read` or
`instance:admin`, so a host may expose inspection without granting installation
authority. Every credential is bound to a Gateway identity and an optional
instance allow-list.

| Route | Purpose |
| --- | --- |
| `GET /v1/host` | Already-authenticated composer identity and supported operations |
| `POST /v1/instances` | Provision one verified digest, activate an authorized profile, start it |
| `GET /v1/instances/{instance}` | Digest, identity, lifecycle, health, capacity state, compatibility |
| `POST /v1/instances/{instance}/upgrade` | Managed verified upgrade |
| `POST /v1/instances/{instance}/rollback` | Recover a compatible interrupted activation |
| `GET /v1/operations/{operation}` | Retained terminal outcome for reconciliation |

Composers return `host.unsupported` for an operation they do not implement.
They never report a skipped operation as success.

## Host policy

`gb-composer serve --config <path>` reads one JSON file. A safe example is
`config/goldenbee-composer.example.json`; replace its placeholder credential and
roots for the host.

```json
{
  "id": "host-a",
  "instances_root": "/var/lib/goldenbee/instances",
  "packages_root": "/var/lib/goldenbee/packages",
  "operations": ["provision", "inspect", "upgrade", "rollback"],
  "allowed_grants": ["task:submit", "task:read", "task:cancel", "session:write"],
  "allowed_profile_grants": ["task:submit", "task:read"],
  "allowed_config_roots": ["/etc/goldenbee/workers"],
  "allowed_workspace_roots": ["/srv/goldenbee/workspaces"],
  "allowed_secret_prefixes": ["secret://"],
  "allow_remote_listener": false,
  "callers": [
    {
      "token": "<host-scoped bearer>",
      "gateway": "goldenbee-prod",
      "scopes": ["instance:admin"],
      "instances": ["general-a"]
    }
  ]
}
```

The composer resolves a package only inside its verified cache by
`sha256:<digest>`. A request cannot name a path or a binary. An operator
configuration selection must resolve under `allowed_config_roots`; its
`instance` and Worker control token must match the request. A workspace must
resolve under `allowed_workspace_roots`. Requested grants, profile grants, and
credential references are intersected with the host lists before any file is
written.

The host composer starts the Worker with a narrow process environment: the
shared `GOLDENBEE_DELEGATION_SECRET` when configured, and no inherited host
environment. Provider credentials stay references in the instance
configuration or the host's secret provider.

## Gateway configuration

A standalone Gateway lists composers next to its Worker approvals:

```json
{
  "hosts": [
    {
      "id": "host-a",
      "endpoint": "http://127.0.0.1:9090",
      "token": "<host-scoped bearer>",
      "instances": ["general-a"],
      "operations": ["provision", "inspect", "upgrade", "rollback"]
    }
  ]
}
```

Callers need `instance:admin`. The Gateway also enforces the configured
instance allow-list and operation allow-list before it contacts the composer.
Changing a host endpoint requires an operator configuration edit; a caller
cannot supply a host endpoint, credential, release path, or executable.

## Lifecycle operations

- **Provision** verifies the digest exists in the host cache, creates or
  converges one instance, activates the selected profile, starts the Worker,
  then registers it through the existing endpoint, instance, package,
  capability, and incarnation checks. A Gateway-generated Worker control token
  is bound to the approval; it is never returned to the caller.
- **Inspect** returns the digest, instance identity, lifecycle, process health,
  task activity, active profile, grants, and target/protocol/runtime/ADK
  compatibility. Host filesystem paths are not part of the response.
- **Upgrade** runs the same plan, explicit permission authorization, work
  policy, data snapshot, binary replacement, readiness, and real capability
  check as `gb-instance upgrade`. Failed activation restores the previous
  binary and data snapshot; unprovable restoration is quarantined.
- **Rollback** uses the same recovery journal as `gb-instance recover`. A
  completed upgrade is not relabeled as a rollback; if no interrupted
  activation exists, the composer returns `upgrade.no_recovery` or
  `upgrade.nothing_to_recover`.

## Reconciliation

Each operation carries a caller-supplied `operation_id`. The composer retains
the request digest and terminal outcome host-side; the Gateway retains the same
identity in `lifecycle.json` under its data directory. Repeating the same
operation after a lost acknowledgment or Gateway restart returns the original
outcome with `replayed: true`. Reusing an operation id with different content
returns `host.operation_conflict`. An in-flight operation whose host answer was
lost is retried by repeating the same id; the composer converges on the existing
instance or returns its recorded outcome. The Gateway does not run a background
reconciler, and host loss returns `host.unreachable` rather than a false
success.
