//! Server-resident, per-vault Peppy client runtime.
//!
//! The server owns authorization and lifecycle fencing. This crate owns only the
//! encrypted local client state and uses an ordinary device credential for sync.

pub mod config;
pub mod custody;
pub mod enrollment;
pub mod http;
pub mod registry;
pub mod runtime;
pub mod tools;
pub mod webhooks;

pub use config::WorkerConfig;
pub use runtime::{Runtime, RuntimeState};
