# Security policy

## Supported versions

The project is pre-1.0. Security fixes are applied to the current development
line; older snapshots are not maintained.

| Version | Supported |
| --- | --- |
| 0.1.x | Yes, current development line |
| Earlier snapshots | No |

## Reporting a vulnerability

Do not open a public issue for a suspected vulnerability.

Use GitHub private vulnerability reporting for this repository. If that channel
is unavailable, contact the maintainers privately through the repository
profile. Include:

- the affected component, version or commit;
- the deployment shape and configuration needed to reproduce;
- a minimal reproduction or proof of concept;
- the impact you believe is possible;
- any suggested mitigation.

You will receive an acknowledgement, an initial assessment, and a disclosure
plan. Please allow time for a fix before publishing details.

## Security boundaries in this project

- Gateway and Worker listeners are loopback or private by default. TLS,
  certificates, and internet exposure are operator responsibilities.
- Caller tokens, Worker control tokens, delegation secrets, and provider
  credentials are secrets. They must be supplied through environment files or
  the documented secret references, never committed.
- The JSON Gateway stores are single-writer. Multiple Gateway replicas require
  the Postgres store.
- Signatures prove artifact provenance. They do not provide process isolation;
  sandboxing and explicit permission grants remain required.
- Release assembly does not upload artifacts or compile code. Production
  signing requires operator-provisioned keys.

## Before publishing a fork

Run a secret scan over the working tree and the full history, remove generated
state and local paths, and rotate any credential that may have been exposed
during development.
