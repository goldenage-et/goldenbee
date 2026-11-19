//! Isolated native browser sessions owned by the Worker runtime.
//!
//! The module wraps the pinned native `adk-browser` WebDriver integration. It
//! adds per-session isolation, destination/network policy, bounded lifetime,
//! and Worker-owned lifecycle; it does not replace the browser engine or the
//! native tool implementations.

mod config;
mod control;
mod driver;
mod facade;
mod policy;
mod proxy;
mod session;
mod tools;

pub use config::{
    BrowserKind, BrowserOptions, BrowserProfile, BrowserSettings, DriverMode, DriverPlan,
};
pub use driver::ManagedDriver;
pub use policy::{BrowserPolicy, Destination};
pub use session::{BrowserController, SessionEntry, SessionKey};
pub use tools::BrowserCapabilityToolset;
