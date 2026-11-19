# Executable examples

Ticket 01 has a recorded native Runner proof. Its current executable location and limitations are documented in [ADK compatibility](../docs/adk.md). The current crate name predates the clarified client SDK role.

Future examples include a standalone Worker Package, a complete CLI-to-Gateway-to-Worker journey, a Master with two remote subworkers, and durable approvals. Each must compile against pinned dependencies and demonstrate actual behavior, not a successful placeholder response.

Worker examples use the shared Worker runtime. Client examples use the client SDK and do not embed native ADK execution. Web and desktop examples remain independent applications when introduced.

Worker development examples use native ADK-Rust Launcher CLI and web-server modes directly, without SDK or Gateway. Label them separately from product Client examples and verify both launcher modes against the pinned release.
