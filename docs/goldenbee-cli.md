# Install Goldenbee

Goldenbee ships two applications:

| Executable | Role |
| --- | --- |
| `gb-gateway` | Control plane, registry, routing, package cache, and local Worker lifecycle |
| `gb-client` | Operator Client that talks to a Gateway over HTTP |

There is no separate composer binary and no `gb` bootstrap executable. The
Gateway owns local installation and process lifecycle directly.

## Bootstrap

Install the Gateway from GitHub:

```sh
curl -fsSL https://raw.githubusercontent.com/goldenage-et/goldenbee/main/install.sh | sh
```

Install only the Client:

```sh
curl -fsSL https://raw.githubusercontent.com/goldenage-et/goldenbee/main/install.sh | sh -s -- client
```

Install both:

```sh
curl -fsSL https://raw.githubusercontent.com/goldenage-et/goldenbee/main/install.sh | sh -s -- all
```

The script supports Linux `x86_64`, downloads the selected release assets,
verifies `SHA256SUMS`, and installs to `~/.local/bin` by default.

## Installed layout

```text
~/.local/bin/gb-gateway
~/.local/bin/gb-client

~/.config/goldenbee/gateway.json
~/.config/goldenbee/trust.json

~/.local/share/goldenbee/gateway/
~/.local/share/goldenbee/instances/
~/.local/share/goldenbee/packages/
~/.local/share/goldenbee/releases/
~/.local/share/goldenbee/workspaces/
```

`install.sh` writes `gateway.json` and `trust.json` with mode `0600` when they
do not exist. It never overwrites an existing configuration. Set
`GOLDENBEE_NO_CONFIG=1` to install only the executable.

The generated trust policy has no trusted signers and no approved runtimes,
ADKs, or permissions. Edit `trust.json` before installing a Worker package.

## Environment

```text
GOLDENBEE_VERSION=vX.Y.Z|latest
GOLDENBEE_BIN_DIR="$HOME/.local/bin"
GOLDENBEE_CONFIG_DIR="$HOME/.config/goldenbee"
GOLDENBEE_DATA_DIR="$HOME/.local/share/goldenbee"
GOLDENBEE_REPO=owner/name
GOLDENBEE_RELEASE_BASE=https://github.com/owner/name/releases
GOLDENBEE_NO_CONFIG=1
```

## Run

```sh
export GOLDENBEE_DELEGATION_SECRET='shared with each Worker process'
gb-gateway serve --config ~/.config/goldenbee/gateway.json
```

The Gateway exposes Client routes and its own local host lifecycle routes on the
same listener. `gb-client` connects with a caller token from `gateway.json`.

## Manage a Worker

```sh
gb-client package install --source /srv/goldenbee/releases/general-0.1.0
gb-client instance deploy \
  --source /srv/goldenbee/releases/general-0.1.0 \
  --package general \
  --instance general-a \
  --grant task:submit --grant task:read \
  --start
gb-client instance list
gb-client instance logs general-a
gb-client instance stop general-a
gb-client instance start general-a
```

For a remote Gateway, connect Client flags to its URL and caller token:

```sh
gb-client --gateway https://gateway.example:8443 --token "$TOKEN" instance list
```

Use an HTTPS URL or a private network path. Caller tokens are bearer secrets.

## Route summary

```text
POST /v1/packages/install
GET  /v1/packages

POST /v1/instances
GET  /v1/instances
GET  /v1/instances/{instance}
POST /v1/instances/{instance}/start
POST /v1/instances/{instance}/stop
POST /v1/instances/{instance}/restart
POST /v1/instances/{instance}/upgrade
POST /v1/instances/{instance}/rollback
GET  /v1/instances/{instance}/logs
```

The older `/v1/hosts/{host}/...` routes remain as a compatibility alias and let
one Gateway proxy lifecycle operations to another Gateway.

## Remove a previous composer install

`gb-composer`, `composer.json`, and the old `gb` bootstrap are no longer
installed. Move the old composer policy fields into `gateway.json`:

- `id` becomes `host_id`
- `instances_root` and `packages_root` stay top-level
- `operations`, grants, roots, and listener policy stay top-level
- remove `callers`; Gateway caller tokens authorize the local host
- add `trust_policy` and `allowed_package_sources`

Then run `gb-gateway serve --config gateway.json` and remove the old
`gb-composer` service.
