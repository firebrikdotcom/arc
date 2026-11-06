//! Durable publication relay. Runtime adapters own scheduling and shutdown.
//!
//! An event and its pending marker commit together. Publication is at least once:
//! a crash after the broker ack but before marker removal can deliver a duplicate,
//! always with the original event ID. Concurrent relays are safe under the same
//! contract and need no process-local lock or destructive claim.

use crate::event_bus::{EventBus, EventBusError};
use crate::event_store::{EventStore, EventStoreError};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum PublicationError {
    #[error("publication store failed: {0}")]
    Store(#[from] EventStoreError),
    #[error("event publication failed: {0}")]
    Publish(#[from] EventBusError),
}

/// Relay one bounded batch for one registered aggregate type.
/// Stop on the first failure, retaining every unacknowledged marker for retry.
pub async fn relay_pending_publications(
    store: &dyn EventStore,
    bus: &dyn EventBus,
    aggregate_type: &str,
    limit: usize,
) -> Result<usize, PublicationError> {
    let mut delivered = 0;
    for event in store.pending_publications(aggregate_type, limit).await? {
        let event_id = event.event_id;
        bus.publish(vec![event]).await?;
        store.acknowledge_publication(event_id).await?;
        delivered += 1;
    }
    Ok(delivered)
}
