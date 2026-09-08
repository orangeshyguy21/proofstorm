//! Host-side acceptance harness for Proofstorm.
//!
//! This crate replaces the Python MCP acceptance clients under
//! `tests/kubernetes/`. It provides the three things every gate needs:
//!
//! - [`McpClient`], a synchronous stdio JSON-RPC client that spawns
//!   `proofstorm-mcp` and performs the MCP handshake;
//! - [`Kubectl`], a context-pinned wrapper for the post-conditions that gates
//!   assert outside MCP (teardown receipts, residual namespaces);
//! - [`json`], fail-loud accessors so a missing field aborts the gate instead
//!   of silently reading `null`.
//!
//! Python's `dict["key"]` raises on a missing key while Rust's `value["key"]`
//! returns `Value::Null`. Ports must go through [`json`] so no assertion is
//! silently weakened in translation.

// This is a test-support crate: every public function is called from gate code
// that treats an `Err` as an immediate abort, so per-function error and panic
// prose would restate the signature without informing a caller.
// A gate is a long sequential script by nature: it mirrors, statement for
// statement, the Python client it replaces. Splitting one into helpers to
// satisfy a line count would obscure that correspondence, which is the whole
// basis for verifying a port.
// Product names such as PostgreSQL, SQLite and Kubernetes appear in prose
// throughout; backticking them would read as code identifiers, which they are
// not.
#![allow(
    clippy::doc_markdown,
    clippy::missing_errors_doc,
    clippy::missing_panics_doc,
    clippy::must_use_candidate,
    clippy::similar_names,
    clippy::too_many_lines
)]

pub mod client;
pub mod doctor;
pub mod gate;
pub mod gates;
pub mod http;
pub mod images;
pub mod json;
pub mod kubectl;
pub mod lab;
pub mod postgres;

pub use client::{McpClient, PROTOCOL_VERSION};
pub use gate::{EXPERIMENT_CAPABILITIES, GateContext, LIFECYCLE_CAPABILITIES};
pub use kubectl::Kubectl;
