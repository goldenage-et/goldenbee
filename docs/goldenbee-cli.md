# Operator CLI

`gb` is the Goldenbee operator bootstrap. It installs the Gateway and composer
executables from a release directory and dispatches their existing command
surfaces. It is not a Client and does not include `gb-client`.

## Bootstrap

Install the bootstrap from GitHub:

```sh
curl -fsSL https://raw.githubusercontent.com/goldenage-et/goldenbee/main/install.sh | sh
```

The script supports Linux `x86_64`, downloads `gb` from the latest GitHub
release, verifies `SHA256SUMS`, and installs to `~/.local/bin`.

Then install components:

```text
gb i gateway
gb i composer
gb i client-cli
gb i all
```

## Commands

```text
gb i gateway
gb i composer
gb i client-cli
gb i all

gb gw <gateway args...>
gb cmp <composer args...>
```

Long aliases:

```text
gb install ...
gb gateway ...
gb composer ...
```

`gb i all` installs the Gateway and composer. `gb i client-cli` installs the
independent Client executable but adds no `gb client` command.

## Install

The default installed layout is:

```text
~/.local/bin/gb
~/.local/bin/gb-gateway
~/.local/bin/gb-composer
~/.local/bin/gb-client

~/.config/goldenbee/gateway.json
~/.config/goldenbee/composer.json
~/.config/goldenbee/local-token

~/.local/share/goldenbee/gateway/
~/.local/share/goldenbee/instances/
~/.local/share/goldenbee/packages/
~/.local/share/goldenbee/workspaces/
```

The installer writes configuration and `local-token` with mode `0600`.
Existing binaries and configuration are kept unless `--force` is supplied.
`--no-config` installs only the executable.

`gb i` downloads release assets from GitHub by default. It installs the
Gateway, composer, and Client from the release tag matching the running `gb`
version. Downloads are verified against `SHA256SUMS` before installation.

Local sources remain available:

1. `--from <dir>` uses that directory only.
2. `GOLDENBEE_RELEASE_DIR` uses a local release directory.
3. `target/debug` or `target/release` keeps the development workflow.
4. Otherwise `gb` downloads from GitHub.

Local lookup accepts both unsuffixed development binaries and target-suffixed
release assets.

The local sibling executable names are:

```text
gb-gateway
gb-composer
gb-client
```

Remote assets are target-suffixed:

```text
gb-gateway-x86_64-unknown-linux-gnu
gb-composer-x86_64-unknown-linux-gnu
gb-client-x86_64-unknown-linux-gnu
```

`gb i` installs operator executables; it does not install signed Worker
packages. Use `gb-install` and `gb-instance` for that path.

### Options

```text
--from <dir>
--release <vX.Y.Z|latest>
--repo <owner/name>
--release-base <url>
--bin-dir <dir>
--config-dir <dir>
--data-dir <dir>
--force
--no-config
```

Environment overrides:

```text
GOLDENBEE_BIN_DIR
GOLDENBEE_CONFIG_DIR
GOLDENBEE_DATA_DIR
GOLDENBEE_VERSION
GOLDENBEE_REPO
GOLDENBEE_RELEASE_BASE
GOLDENBEE_GITHUB_TOKEN
```

Checksum verification detects corrupted or mismatched downloads. It is not a
signed-package trust root; signed Worker package installation remains the
`gb-install` path.

## Dispatch

`gb gw` resolves the installed `gb-gateway` and replaces the `gb` process with
it. Lifecycle commands receive the generated Gateway configuration when
`--config` is omitted:

```sh
gb gw up
gb gw status
gb gw down
gb gw pair
gb gw serve
```

`gb cmp` resolves `gb-composer` and injects the generated composer
configuration for `serve`:

```sh
gb cmp serve
gb cmp serve --config /custom/composer.json
```

Other arguments pass through unchanged, including `gb gw provider ...`.

## Generated local configuration

The Gateway config uses one local caller with `scopes = ["*"]` and a local host
entry for the composer. The composer config uses the same bearer token and local
instance/package roots. This is development policy, not a production user
model. Production deployments can replace both files and keep their own scoped
credentials.

`gb` does not run a background reconciler, install systemd units, or manage
running processes after start. Gateway `up/status/down` and composer process
supervision remain the responsibility of their executables or the host
supervisor.
