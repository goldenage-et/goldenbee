## Agent skills

### Issue tracker

Public issue tracking is not open yet. Before reading or publishing issues, read `docs/agents/tracker.md`.

### Triage labels

Use the default triage vocabulary. Before assigning status, read `docs/agents/labels.md`.

### Domain docs

Use a single-context layout. Before exploring or specifying changes, read `docs/agents/domain.md`.

### Architecture

Before changing Worker, Gateway, Client, or shared-library ownership, read `GLOSSARY.md`, `docs/layout.md`, and the relevant ADRs.


## 6. Command Restrictions and Verification

Run the narrowest affected check while iterating, then run the full workspace
check before declaring the work complete. Record exactly what ran and its
result; never fabricate evidence.

- Use `cargo check -p <crate>` for quick, package-scoped iteration.
- Use `cargo test -p <crate> [filter]` for focused tests, and `make check` for
  formatting, Clippy, tests, and documentation before a pull request.
- Keep one shared `target/` directory per checkout; do not set a per-agent
  `CARGO_TARGET_DIR`.
- Throughout implementation, manually review imports, types, call sites, data
  flow, security, error paths, and integration. After implementation and
  refinement, follow **type checking → tests → runtime verification as
  applicable → fix issues → rerun affected checks**.
- If required verification cannot run, report **IMPLEMENTED BUT UNVERIFIED**
  with the specific pending checks. Never call unverified work
  production-ready.


### Exploration and documentation work

Use the main checkout on the `main` branch for read-only codebase exploration and documentation-only changes. Do not create a worktree merely to inspect code or write docs; reserve worktrees for isolated implementation changes.

### Code organization

Organize by ownership and cohesion so code is found by the concept it serves. The tree follows current responsibilities; it is not fixed and must not be preserved after those responsibilities change.

- Put a module with the feature that owns its state, validation, and policy, not in a layer named after a technology or convenience bucket.
- Keep nesting shallow. Add a level only when it expresses a real sub-concept.
- Keep the crate root a facade: module wiring and public re-exports, no implementation.
- Give each piece of mutable state or persisted store one owner. Other modules read or change it through that owner.
- Preserve public paths when moving code. Move and delete in the same change. No duplicate implementations or compatibility shims beyond re-exports.
- Split a file when it holds unrelated responsibilities or becomes hard to navigate. Merge files that are too small to justify a module.
- ADK may be used throughout Worker-side code. Keep it out of shared or client-facing crates and transport handlers that do not execute agents.
- Worker Packages follow the same ownership rule, not a prescribed file list.
- When adding a module, name its owner and responsibility. If no existing module owns it, create one whose name says what it does.

### Worker authoring

Before changing `crates/runtime`, any package under `workers/`, or Worker startup, configuration, asset loading, tools, hooks, or plugins, read `docs/agents/worker.md`. It defines the agreed runtime-config, automatic asset loading, host API, hook, and plugin rules.

### Rust development

Before changing Rust packages or build tooling, read `docs/dev.md` for workspace checks, scaffold limits, and test boundaries.

### Native ADK work

Before adding or changing any Worker execution, agent, tool, toolset, session, model, graph, A2A, MCP, memory, RAG, coding, browser, sandbox, callback, or plugin behavior, read the official documentation at <https://adk-rust.com/en/docs> and the relevant pages for the capability you are changing. Then read `docs/adk.md` and the matching pinned 2.3.0 source and examples under `~/.cargo/registry/src/*/adk-*/`. The site currently documents v2.1.0; the pinned source is authoritative when they disagree. Cite the exact symbols and feature flags in the pull request. If the pinned release cannot support the requirement, record the gap. Do not invent an API, silently substitute a replacement engine, or claim unexecuted native behavior.

### Naming

Use short, generic file, directory, and crate names. Match crate names to their directories. Keep standard tool filenames unchanged.
