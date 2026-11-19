# Operator CLI

`gb` is the Goldenbee operator bootstrap. It installs the Gateway and composer
executables from a release directory and dispatches their existing command
surfaces. It is not a Client and does not include `gb-client`.

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

Source lookup order:

1. `--from <dir>`
2. `GOLDENBEE_RELEASE_DIR`
3. the directory containing `gb`
4. `target/debug` or `target/release`

The expected sibling executable names are:

```text
gb-gateway
gb-composer
gb-client
```

`gb i` installs operator executables; it does not install signed Worker
packages. Use `gb-install` and `gb-instance` for that path.

### Options

```text
--from <dir>
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
GOLDENBEE_RELEASE_DIR
```

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
