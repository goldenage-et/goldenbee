# systemd deployment examples

These units run the standalone Gateway and any number of Worker instances on
one Linux host. They are examples for operators, not an installer. The CLI does
not write to `/etc`, create users, or change host service state.

| Unit | Purpose |
| --- | --- |
| `goldenbee.target` | Pulls in the Gateway. Worker instances are enabled separately. |
| `goldenbee-gateway.service` | `gb-gateway serve --config /etc/goldenbee/gateway.json` |
| `goldenbee-worker@.service` | One Worker instance: `%i` selects `/etc/goldenbee/workers/%i.toml`. |

The units do not use `Requires=` between Gateway and Workers. A Worker can run
standalone, and a Worker restart does not restart the Gateway. The Worker
heartbeat retries registration while the Gateway is unavailable.

## Layout

```text
/usr/local/bin/gb-gateway                 # goldenbee-gateway executable
/usr/local/bin/goldenbee-worker        # symlink to the selected Worker package binary
/etc/goldenbee/gateway.json
/etc/goldenbee/gateway.env             # root:root, mode 0600
/etc/goldenbee/workers/general-a.toml
/etc/goldenbee/workers/general-a.env   # root:root, mode 0600
/etc/goldenbee/workers/assets/...      # read-only Worker assets
/var/lib/goldenbee/gateway/            # standalone single-writer state only
/var/lib/goldenbee/workers/general-a/  # instance state and sandbox workspace
```

`StateDirectory=` creates the data directories with mode `0700`. `ReadWritePaths=`
limits each service to its own instance directory. The Worker package binary
name is package-specific; point `/usr/local/bin/goldenbee-worker` at the
installed package, for example `general`. If signed releases are installed with
`gb-install`, point the symlink at the verified package entrypoint instead of
copying an unverified binary.

## Install

Create the service account if it does not exist:

```sh
sudo useradd --system --home-dir /var/lib/goldenbee --shell /usr/sbin/nologin goldenbee
```

Then create directories and install the examples:

```sh
sudo install -d -o root -g root -m 0755 /etc/goldenbee/workers
sudo install -d -o goldenbee -g goldenbee -m 0700 /var/lib/goldenbee/gateway
sudo install -d -o goldenbee -g goldenbee -m 0700 /var/lib/goldenbee/workers/general-a

sudo install -o root -g root -m 0644 deploy/systemd/goldenbee.target /etc/systemd/system/
sudo install -o root -g root -m 0644 deploy/systemd/goldenbee-gateway.service /etc/systemd/system/
sudo install -o root -g root -m 0644 deploy/systemd/goldenbee-worker@.service /etc/systemd/system/

sudo install -o root -g root -m 0644 deploy/systemd/gateway.json.example /etc/goldenbee/gateway.json
sudo install -o root -g root -m 0600 deploy/systemd/gateway.env.example /etc/goldenbee/gateway.env
sudo install -o root -g root -m 0600 deploy/systemd/worker.toml.example /etc/goldenbee/workers/general-a.toml
sudo install -o root -g root -m 0600 deploy/systemd/worker.env.example /etc/goldenbee/workers/general-a.env
```

Replace every placeholder token and path before starting. The Gateway and
Worker configuration files contain caller, control, and development tokens, so
they are also `0600` in this example.

## Enable

```sh
sudo systemctl daemon-reload
sudo systemctl enable --now goldenbee.target
sudo systemctl enable --now goldenbee-worker@general-a.service
```

`goldenbee.target` starts the Gateway. Each Worker instance is enabled
individually. Add more instances by creating
`/etc/goldenbee/workers/<instance>.toml`, its environment file, and enabling
`goldenbee-worker@<instance>.service`.

## Operate

```sh
sudo systemctl status goldenbee.target goldenbee-gateway.service
sudo systemctl status 'goldenbee-worker@*'
sudo journalctl -u goldenbee-gateway.service -f
sudo journalctl -u goldenbee-worker@general-a.service -f
curl -fsS http://127.0.0.1:8080/v1/health
curl -fsS http://127.0.0.1:8081/v1/health
```

Durability belongs to the application stores, not to systemd. Autostart and
restart belong to systemd or the container supervisor, not to Goldenbee. Read
[operations](../../docs/operations.md) for drain, upgrade, backup, and recovery
guidance.

## Multiple Gateway replicas

These example units run one Gateway process. A second process pointed at the
same `/var/lib/goldenbee/gateway` directory fails closed with
`storage.single_writer`; the JSON registry and dispatch files are not shared
state.

To run more than one Gateway replica, build the `gateway` binary with the
`postgres` feature (`cargo build -p goldenbee-gateway --features postgres`) and add
a shared store to `gateway.json`:

```json
"store": {
  "kind": "postgres",
  "url_env": "GOLDENBEE_GATEWAY_DATABASE_URL",
  "max_connections": 8,
  "migrate": true
},
"replica_id": "gateway-a"
```

Put the DSN in `/etc/goldenbee/gateway.env` (mode `0600`). Give each replica a
distinct stable `replica_id`, keep the same delegation secret, and ensure every
replica can reach each approved Worker endpoint because the receiving replica
performs live identity verification. The file directory is then unused for
control state. See [Shared Gateway store](../../docs/operations.md#shared-gateway-store)
for claims, lease timestamps, migration, backup, rollback, and the tested scope.
