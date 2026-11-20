# Goldenbee documentation

Start with the architecture and one usage path. Detailed protocol and operations
pages are references, not required reading before a local run.

## Start here

| Page | Read it for |
| --- | --- |
| [Architecture](architecture.md) | What Worker, Gateway, Client, SDK, runtime, adapter, registry, installer, and instance manager own; trust boundaries; failure behavior; limits |
| [Deployment and usage](deployment.md) | Setup from zero; standalone Worker; local stack; Gateway + Client; verified install; registry; local host lifecycle; Postgres replicas; systemd; use cases |
| [Repository layout](layout.md) | Accepted directory ownership and migration status |
| [Glossary](../GLOSSARY.md) | Exact product vocabulary |

## Build and develop

| Page | Read it for |
| --- | --- |
| [Develop Goldenbee](dev.md) | Workspace checks, native ADK source requirements, layout rules |
| [ADK compatibility](adk.md) | Pinned ADK-Rust version and recorded compatibility evidence |
| [Install Goldenbee](goldenbee-cli.md) | Install the Gateway and Client directly from a release; generated config; upgrade and migration |
| [Client CLI](gb-client.md) | Gateway profiles, chat, Worker administration, teams, profiles, tasks, package and instance commands |

## Protocols and configuration

| Page | Read it for |
| --- | --- |
| [Gateway protocol v1](gateway-api.md) | Caller authentication, routing, teams, profiles, dispatch, errors |
| [Worker protocol v1](worker-api.md) | Worker management routes, durable tasks, events, delegation, approvals |
| [Worker configuration and assets](worker-config.md) | TOML schema, providers, agents, prompts, skills, profiles, isolation |
| [Fenced interactive control](control.md) | Browser takeover leases, generations, and audit boundaries |

## Install, distribute, and operate

| Page | Read it for |
| --- | --- |
| [Install a local release](install.md) | `gb-install`, trust policy, sandbox smoke checks, cache layout |
| [Instance lifecycle](instance.md) | `gb-instance` create/start/stop/inspect/upgrade/backup/restore |
| [Registry client](registry.md) | Client commands, immutable versions, scopes, optional external server |
| [Trust and revocation](trust.md) | Signed advisories, offline decisions, quarantine |
| [Local host lifecycle](goldenbee-composer.md) | Gateway-owned package cache, provision/inspect/start/stop/restart/upgrade/rollback, Gateway-to-Gateway |
| [Operations](operations.md) | Boot, systemd, drain, restart, upgrade, backup, recovery, Postgres HA |
| [Release](release.md) | Artifact assembly, compatibility, deployment modes, release limits |
| [Release notes](release-notes.md) | Current release candidate evidence and unknown behavior |
| [systemd examples](../deploy/systemd/README.md) | Example units, host layout, enablement, single-writer boundary |

## Decisions

| Page | Read it for |
| --- | --- |
| [ADR 0001: layout](adr/0001-layout.md) | Independent Workers and Clients, SDK role, ownership |
| [ADR 0002: package format](adr/0002-package-format.md) | OCI release layout, signatures, verification |

## Documentation conventions

- CLI and operator commands are checked against the current source and
  `Makefile`.
- Examples use loopback listeners and placeholder tokens. Never commit a real
  token or secret.
- A signature proves provenance, not isolation. Sandboxing and explicit grants
  remain required.
- Do not describe the Gateway-owned local host as a task proxy or supervisor, or
  the JSON Gateway stores as multi-replica safe.
- Provider credentials are Worker-owned today. `secret://` and `vault:`
  references are recognized but have no merged central provider; `env:` is the
  working source.
