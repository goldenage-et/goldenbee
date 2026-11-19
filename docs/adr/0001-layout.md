---
status: accepted
date: 2026-11-18
---

# Independent Workers and Clients

Goldenbee has three building blocks: Worker, Gateway, and Client. The user clarified that Clients are separate CLI, web, desktop, or use-case-specific applications, and that SDK means a reusable Gateway client library rather than a unified application or Worker authoring library.

Different executable capabilities belong in independent packages under `workers/`, sharing the Worker runtime and native ADK implementations. Gateway and Client applications belong under `apps/`. A new model, instruction set, or agent role normally needs a Runtime Profile, not a new Worker Package.

## Consequences

- Build the Worker-to-Gateway-to-CLI path first. Web follows as a separate application. Desktop and other applications remain independently addable.
- Every Client connects through an SDK before reaching Gateway. Language-specific SDKs share Gateway protocol semantics; Client applications do not implement separate direct transports.
- Each Worker Package uses native ADK-Rust Launcher for its local development CLI and web server. These are development tools, not Goldenbee Client applications, and do not require SDK or Gateway. Reuse native launcher behavior instead of implementing a second development CLI or web server.
- Use `crates/runtime` for shared Worker execution, `crates/sdk` for the Rust Gateway client library, and `crates/common` for small shared types and helpers. The user selected `common` instead of `contracts`.
- Keep ADK execution out of Client applications and the client SDK. The web milestone includes a suitable TypeScript SDK without requiring the Rust SDK in the browser.
- Keep Gateway-owned registry, team configuration, and projections out of authoritative Worker execution state. Clients share protocol behavior, not one mandatory UI shell.
- Worker package registration support remains a Worker concern. Any separately published authoring library needs an explicit name and justification distinct from the client SDK.
- This supersedes the original scaffold's placement of native ADK code in a crate named `sdk`, a network library named `client`, and a single `apps/worker` executable. Migration is separate from documenting the decision.

## Alternatives rejected

- One unified Client application would couple unrelated presentation and application workflows.
- One fixed Worker product would blur executable capabilities with configurable profiles and constrain independent packaging.
- Reusing `sdk` for both Worker execution and client connections would expose native execution dependencies to application authors.
