#![allow(
    clippy::missing_errors_doc,
    reason = "application operations return the shared Error contract for authorization, validation, storage, and runtime failures"
)]
//! Shared application behavior for developer and MCP clients.
pub mod connections;
pub mod environment;
mod error;
mod events;
pub mod http;
pub mod journal;
pub mod lab;
pub mod lifecycle;
pub mod observer;
pub mod runtime;
pub mod telemetry;
pub mod updates;

pub use error::{Error, ErrorKind};
pub use runtime::Runtime;
