# Fenced interactive control

Ticket 34 added isolated Worker-owned browser sessions but deliberately did not
advertise takeover. Ticket 35 adds the missing ownership layer and compares it
against the pinned native browser facilities before exposing it.

## Native comparison and decision

The pinned `adk-browser` 2.3.0 source was inspected directly
(`~/.cargo/registry/src/*/adk-browser-2.3.0/src/session.rs` and
`src/toolset.rs`). It provides one `BrowserSession` whose methods are callable
by every holder of its `Arc`. It has no lease, owner, takeover, fencing
generation, expiry, revocation, hand-back, audit, or per-session command queue.
`BrowserSession::start` replaces a dead WebDriver and `stop` quits it; neither
is an ownership transition. The native toolsets select tools but do not mediate
another controller.

**Decision:** do not claim native takeover or add a replacement browser engine.
Add a narrow Worker-runtime ownership adapter around the native session. The
adapter owns only fencing and serialization; navigation, clicks, typing,
extraction, profiles, cookies, downloads, egress policy, and WebDriver remain
the pinned native implementation. Terminal control is not implemented or
advertised; reusing this contract for a terminal requires the same tests and
gate first.

## Authority and boundaries

The Worker runtime is the only lease authority
(`crates/runtime/src/browser/control.rs` and the browser controller). The
Gateway checks caller scope and its Worker allow-list, then forwards the request
with a delegation containing only `session:control`. It does not allocate,
store, renew, or validate a generation. The SDK carries the Worker-issued
`epoch` and `generation` unchanged. `common` contains wire types and the
command label only. A fake-Worker Gateway test proves the Worker's generation
survives routing and no Gateway-side lease exists.

Session identity is the exact authenticated `(namespace, principal, session)`
tuple. The Worker encodes namespace and principal with the same native identity
used by agent execution, so a control request addresses exactly the browser
session the authorized agent used. A foreign principal cannot see or operate
another principal's session.

## Ownership outcomes

| Event | Generation | Owner after the event | Old controller |
| --- | --- | --- | --- |
| Agent creates/uses a session | unchanged | Agent | n/a |
| Authorized human `acquire` | +1 | Human | Agent commands are rejected |
| Second `acquire` by the same principal | +1 | newest Human | previous client and agent are fenced |
| Explicit hand-back | +1 | Agent | human commands are fenced |
| Lease expiry (default 120 s, max 1 h) | +1 | Agent | human commands return `control.lease_expired` |
| Client disconnect | unchanged until TTL | Human until expiry, then Agent | A disconnected client cannot issue commands; the lease expires and is fenced |
| Worker process restart | new epoch | Agent | every previous epoch is rejected with `control.epoch_mismatch` |
| Session release or Worker shutdown | state cleared | Agent (no session) | every command is rejected |

All operations for one exact session acquire the same per-session gate. An
acquire waits for an in-flight browser command, then rotates the generation
before returning. A command that was queued while a human owned the session
re-checks ownership after acquiring the gate and is rejected, so a stale click
cannot mutate the page after the new controller acts. The gate and the single
lease record make two concurrent controllers impossible.

## Commands and redaction

Human commands are limited to `navigate`, `click`, `type`, and `extract_text`.
Every navigation still passes the ticket-34 destination, TLS, private-address,
and resolved-address policy. The adapter exposes no cookie API, model-written
JavaScript, file upload, arbitrary host file access, or terminal command.

Control results include only URL, title, bounded page text, and command output.
Audit entries contain time, action, session id, principal, generation, outcome,
and either a command label or a stable error code. Navigation URLs, typed text,
selectors, cookies, page text, and host paths never enter audit. The audit test
in the General browser suite types a canary value and asserts it is absent.

## Capability projection

A package-declared, enabled browser reports `takeover: true`,
`control_protocol: "fenced-lease-v1"`, the four command labels,
`streaming: false`, `evaluate_js: false`, and `terminal_control: false`. A
package without the browser capability cannot be enabled by configuration, and
a disabled browser rejects acquisition with `browser.disabled`.
