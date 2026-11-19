# Test boundaries

`smoke` contains process smoke checks and Cargo dependency-boundary checks for the current workspace. Run them through `make test`. They do not establish runtime or framework acceptance.

Feature tests use the approved public-client seam: real processes, native ADK execution and persistence, deterministic external endpoints, observed effects, and digest-checked artifacts. Separate compile-and-run fixtures verify Worker package integration and the client SDK consumer contract.

CLI, web, and desktop Clients should reach equivalent Gateway behavior without sharing one UI application. Dependency checks keep native ADK execution out of Client apps, the client SDK, and `common`. Adapt checks to actual crate roles during migration, not to the legacy meaning of `sdk`.

Each Worker Package also needs native ADK-Rust Launcher CLI and web-server smoke checks with Gateway absent. Those development checks exercise the same real package composition and are separate from product Client-to-SDK-to-Gateway acceptance.

Add protocol, cross-process, security, recovery, and evaluation suites with their owning slices. Do not prepopulate them with empty passing tests or substitute mocked Runner behavior for native integration evidence.

`load-harness` is test-only measurement tooling. It records
environment provenance, latency distributions, completed work, terminal-event
counts, and bounded `/proc`/state-file samples. Its raw JSON reports are not
product telemetry and do not define a universal SLO.
