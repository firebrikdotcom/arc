//! # Arc Core
//!
//! Event sourcing primitives for the arc framework.
//! This crate is headless and contains no web dependencies.
//!
//! ## Features
//!
//! - Event store trait definitions
//! - Aggregate trait definitions
//! - Command and event bus traits
//! - Projector, projection, and projection engine traits
//! - Read model store trait
//!

// Re-export commonly used types
pub use serde::{Deserialize, Serialize};
pub use uuid::Uuid;

// Module structure
pub mod access_log;
pub mod aggregate;
pub mod audit;
pub mod command_bus;
pub mod event;
pub mod event_bus;
pub mod event_store;
pub mod integrity;
pub mod projection;
pub mod publication;
pub mod read_model_store;
pub mod session;
pub mod snapshot;

#[cfg(test)]
mod tests {
    #[test]
    fn it_works() {
        assert_eq!(2 + 2, 4);
    }
}
