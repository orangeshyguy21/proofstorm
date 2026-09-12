#![allow(
    clippy::missing_errors_doc,
    reason = "application operations return the shared Error contract for authorization, validation, storage, and runtime failures"
)]
//! Shared application behavior for developer and MCP clients.
pub mod artifacts;
pub mod bootstrap;
mod command_name;
pub mod config;
pub use command_name::command_name;
pub mod cell;
pub mod connections;
pub mod dev_reset;
pub mod developer;
pub mod environment;
mod error;
mod events;
pub mod gui;
pub mod harness;
pub mod http;
pub mod installation;
pub mod installer;
pub mod journal;
pub mod lifecycle;
pub mod observer;
pub mod platform;
pub mod release;
pub mod runtime;
pub mod telemetry;
pub mod updates;

pub use error::{Error, ErrorKind};
pub use runtime::Runtime;
