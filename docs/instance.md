# Create and operate local Worker instances

`gb-instance` is local operator tooling for on-host Worker lifecycle. It is not
a product Client, a Gateway route, a registry, or a replacement supervisor.
Installation and instance lifecycle stay separate operations:

```sh
gb-install inspect --artifact ./release --trust ./trust.json [--json]
gb-install install --artifact ./release --trust ./trust.json --root /srv/goldenbee [--json]

gb-instance create --root /srv/instances --release /srv/goldenbee/packages/sha256/<digest> \
  --id reviewer --grants task:read,profile:read --read-only [--json]
gb-instance profile --root /srv/instances --instance reviewer create review \
  --instruction "Review only; do not edit." --member general --capability general \
  --grant task:read --activate
gb-instance start --root /srv/instances --instance reviewer [--json]
gb-instance inspect --root /srv/instances --instance reviewer [--json]
gb-instance stop --root /srv/instances --instance reviewer [--json]
gb-instance trust --root /srv/instances --instance reviewer \
  --trust /srv/goldenbee/trust.json --advisories /srv/goldenbee/advisory.json [--json]
```

`gb-instance profile` proxies to the installed binary's existing native
`profile` subcommands, including `list`, `show`, `create`, `revise`, `activate`,
`deactivate`, `delete`, and `export`. The instance tool does not reimplement
profile revisions. The Worker-native `dev` and `serve` Launcher modes remain the
package development path and are not duplicated here.

## Layout

```text
<instances-root>/<instance-id>/
  instance.json     # schema, id, package/version, artifact and binary digests,
                    # signer, release directory, binary, config, data, workspace,
                    # grants, limits, secret references, seed paths, created_at
  state.json        # lifecycle, pid, incarnation, active profile, listener,
                    # start/stop time, last error, explicit trust decision
  worker.toml       # generated once when --config is not supplied
  token             # development token, mode 0600
  assets/           # seed defaults copied once; user-owned afterwards
  state/            # durable tasks, sessions, memory, artifacts
  workspace/        # authorized workspace root by default
  process.log       # child stdout/stderr for local diagnosis
```

`instance.json` never contains a secret value; `secret_refs` are references.
Creation uses a private staging directory, writes and fsyncs every file, then
renames the directory into place. A stale `.staging-<id>-*` directory from an
interrupted create is removed by the next create for that id. A half-created
instance is never listed as an instance.

Creating the same id for the same release converges. Creating the same id for a
different release is a conflict. Existing user assets are never overwritten by
creation, an upgrade, or another instance.

## Generated configuration

When `--config` is omitted, `worker.toml` binds the stable instance id, a
generated development token, the instance grants as both `grants` and
`token_scopes`, the configured capacity ceilings, and the asset directories
materialized from the verified seed catalog. Relative paths resolve against the
configuration file, not the executable or the current working directory.

When `--config` is supplied, the operator owns that file and its directories.
It may live anywhere permitted by host policy, outside the install tree and the
current working directory. The instance still records stable identity, data,
workspace, grants, limits, and the same immutable binary digest. Start sets
`GOLDENBEE_INSTANCE_ID`, `GOLDENBEE_DATA_DIR`, and `GOLDENBEE_SANDBOX_DIR` and
does not inherit the caller's environment.

A read-only instance is created with `--read-only` and read-only grants. The
generated token receives only those grants, so changing instructions or copying
a writable profile cannot create task-submission authority. Profile creation is
also rejected when a grant is outside the instance's authority.

## Distinct operational fields

`inspect --json` reports three independent fields:

- `process_health`: `running`, `exited`, or `unknown`. `running` means the
  recorded process is alive and its command line still names this instance's
  configuration; `exited` means a recorded process is gone; `unknown` means no
  process control record or the process is alive without a reachable listener.
- `lifecycle`: `configuring`, `running`, `stopped`, or `faulted`. A failed
  startup writes `faulted` and never `running`, because `start` writes
  `running` only after `/v1/health` and `/v1/status` report a ready Worker.
- `task_activity`: `idle` when capacity reports `active=0` and `queued=0`,
  `executing` otherwise, and `unknown` when the authenticated status endpoint
  is unreachable.

An idle instance is still `running` with `task_activity=idle`. A stopped
instance keeps its stable identity and durable task history and receives a new
process incarnation on the next start.

## Trust decisions

`gb-instance trust` applies one explicit local decision to the exact release
digest recorded for this instance. It reads the recorded trust policy and exact
signed advisory bytes, and never contacts a registry.

- `continue` leaves installed execution available. New installs and upgrades
  still need fresh evidence.
- `restrict` leaves already running work running, but denies new starts and
  profile mutations with `instance.trust_restricted`.
- `quarantine` stops the recorded process and denies new starts and profile
  mutations with `instance.quarantined`.

`inspect` reports the decision, redacted reason, advisory generation, and
decision time. Two instances created from the same immutable digest keep
independent decisions. Only an operator applying a valid newer advisory can
clear a quarantine; start, stop, inspect, and profile commands cannot, and no
Gateway, SDK, or Client route mutates roots or quarantine. See
[package trust and revocation](trust.md).

## Isolation and stop

Start and stop act only on the recorded pid whose command line carries this
instance's configuration. A pid-reuse mismatch is not signalled. Stop sends
SIGTERM, waits for the Worker drain, and leaves `lifecycle=running` with an
`instance.stop_timeout` error if the process does not exit. One instance's
start, stop, assets, or data never touch another instance's directory.

Execution reads only the verified release directory and the instance directory.
There is no registry client, registry lookup, or registry configuration in the
instance record; installed Workers run while distribution is unavailable.

Production restart, boot, and resource isolation remain the responsibility of
the host supervisor. See `deploy/systemd/goldenbee-worker@.service`.
