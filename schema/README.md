# Public schemas

Add versioned Goldenbee management schemas only after auditing native ADK routes as native, proxy, or new. Schema publication must describe implemented behavior rather than freeze routes or formats solely from proposal examples.

Gateway owns its client-facing protocol. Rust Clients can use shared representations from `common`; other Clients can consume language-neutral schemas without importing a Rust crate. SDKs are connection libraries, not a unified Client application.

Schema fixtures must be exercised against actual public behavior. Shared wire types remain independent of native runtime implementation types, storage state, and UI assumptions.
