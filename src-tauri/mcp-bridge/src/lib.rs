//! Harbor's persistent native stdio-to-HTTP MCP bridge.
//!
//! The bridge intentionally has no dependency on the Harbor desktop crate. It
//! is shipped as a small sidecar and keeps a client-owned stdio session alive
//! while Harbor's loopback HTTP server, bearer token, or port changes.

mod bridge;
mod descriptor;
mod runtime;

pub use bridge::{Bridge, BridgeTiming};
pub use runtime::{InitializationError, NativeRuntime};
