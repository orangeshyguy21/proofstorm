#![allow(
    clippy::missing_errors_doc,
    reason = "application operations return the shared Error contract for authorization, validation, storage, and runtime failures"
)]
//! Shared application behavior for developer and MCP clients.
pub mod connections;
pub mod environment;
mod error;
pub mod http;
pub mod journal;
pub mod lab;
pub mod runtime;

pub use error::{Error, ErrorKind};
pub use runtime::Runtime;
