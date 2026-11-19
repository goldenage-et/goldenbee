# Coding Worker Package

One independently built `coding` executable composes the shared Goldenbee
Worker runtime with the pinned native ADK coding agents.

- `src/package.rs` — the single `Worker` definition and package state.
- `src/agent.rs` — writable `developer` and read-only `reviewer`
  `adk_agent::coding::CodingAgent` compositions.
- `src/repo.rs` — workspace root, git worktree provisioning, instance ownership.
- `src/services.rs` — instance-scoped settings and service construction.
- `src/build.rs` — bounded sandbox checks with a per-worktree target directory.
- `src/evidence.rs` — diff, status, check, and digest-verified artifact evidence.
- `src/hooks.rs` — startup, task ownership, evidence, cancellation, shutdown.
- `src/tool.rs` — the shared `exec_command` sandbox tool bound to the worktree.

The package does not implement file editing, search, Bash, or an inner agent
loop. Those remain native `adk-devtools` and shared runtime capabilities.

Roles are configuration: profiles can select the compiled `developer` or
`reviewer`, or compose them for a coordinating Master. There are no separate
developer/reviewer binaries.

See `docs/worker-config.md` for the operator settings and the package tests for
verification coverage.
