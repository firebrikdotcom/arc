//! `UserProjector` — builds the `users_view` read model from `User` events.
//!
//! Pure event handler: receives an [`Event`] + a [`ReadModelStore`] and
//! emits a single version-gated upsert, including a retained deletion tombstone.
//! Engine delivery reconciles complete authoritative state, so partial events
//! remain correct under reordered delivery and concurrent writers. Holds no
//! mutable state. Idempotency comes from the store's version gate, not from
//! the projector — replaying a known event sequence twice yields the same
//! `users_view` because each upsert with a given `version` is rejected the
//! second time. `_arc_deleted: true` rows are hidden by read-model stores.
//!
//! Wired into [`InProcessEventBus`](arc_core::event_bus::InProcessEventBus)
//! at startup via a thin [`EventHandler`](arc_core::event_bus::EventHandler) adapter that calls
//! [`ProjectionEngine::process`](arc_core::projection::ProjectionEngine::process).

use arc_core::event::Event;
#[cfg(test)]
use arc_core::event::NewEvent;
use arc_core::event_store::EventStore;
use arc_core::projection::{ProjectionError, ProjectionResult, Projector};
use arc_core::read_model_store::{ReadModelStore, Upsert};
use async_trait::async_trait;
use serde_json::{json, Value};

/// The shared read-model table name. Other modules (controllers, login lookup)
/// reference this constant rather than hard-coding the string.
pub const USERS_VIEW: &str = "users_view";

pub struct UserProjector;

impl UserProjector {
    pub fn new() -> Self {
        Self
    }
}

impl Default for UserProjector {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Projector for UserProjector {
    fn name(&self) -> &str {
        "UserProjector"
    }

    fn handles(&self) -> Vec<String> {
        vec![
            "UserRegistered".to_string(),
            "ProfileUpdated".to_string(),
            "EmailChanged".to_string(),
            "PasswordChanged".to_string(),
            "UserDeleted".to_string(),
        ]
    }

    async fn apply_authoritative(
        &self,
        event: &Event,
        store: &dyn ReadModelStore,
        event_store: &dyn EventStore,
    ) -> ProjectionResult<()> {
        // Fold the complete current stream, not the last projected row. A newer
        // delivery can therefore never discard an older event's different field.
        // Concurrent folds produce complete states; the store's version gate
        // rejects the older result, including results racing with deletion.
        let events = event_store
            .load_stream("User", &event.aggregate_id)
            .await
            .map_err(|error| project_err(self, event, error.to_string()))?;
        let mut row = None;
        for stored in events {
            match stored.event_type.as_str() {
                "UserRegistered" => {
                    row = Some(json!({
                        "id": stored.aggregate_id,
                        "name": payload_str(&stored.payload, "name")?,
                        "email": payload_str(&stored.payload, "email")?,
                        "password_hash": payload_str(&stored.payload, "password_hash")?,
                        "version": stored.sequence,
                    }))
                }
                "UserDeleted" => {
                    row = Some(json!({
                        "id": stored.aggregate_id, "version": stored.sequence, "_arc_deleted": true,
                    }))
                }
                "ProfileUpdated" | "EmailChanged" | "PasswordChanged" => {
                    let current = row.as_mut().ok_or_else(|| {
                        project_err(self, event, "user stream is missing registration")
                    })?;
                    if current.get("_arc_deleted").and_then(Value::as_bool) == Some(true) {
                        continue;
                    }
                    let field = match stored.event_type.as_str() {
                        "ProfileUpdated" => "name",
                        "EmailChanged" => "email",
                        _ => "password_hash",
                    };
                    current[field] = json!(payload_str(&stored.payload, field)?);
                    current["version"] = json!(stored.sequence);
                }
                _ => {}
            }
        }
        let row =
            row.ok_or_else(|| project_err(self, event, "authoritative user stream is empty"))?;
        store
            .upsert(Upsert::new(USERS_VIEW, &event.aggregate_id, row))
            .await
            .map_err(|error| project_err(self, event, error.to_string()))
    }

    async fn apply(&self, event: &Event, store: &dyn ReadModelStore) -> ProjectionResult<()> {
        let id = &event.aggregate_id;

        // For partial-update events we need the prior row so we can carry
        // forward fields the event doesn't touch. UserRegistered seeds the
        // row outright, so the lookup is skipped.
        match event.event_type.as_str() {
            "UserRegistered" => {
                let row = json!({
                    "id": id,
                    "name": payload_str(&event.payload, "name")?,
                    "email": payload_str(&event.payload, "email")?,
                    "password_hash": payload_str(&event.payload, "password_hash")?,
                    "version": event.sequence,
                });
                store
                    .upsert(Upsert::new(USERS_VIEW, id, row))
                    .await
                    .map_err(|e| project_err(self, event, e.to_string()))?;
            }

            "UserDeleted" => {
                store
                    .upsert(Upsert::new(
                        USERS_VIEW,
                        id,
                        json!({
                            "id": id, "version": event.sequence, "_arc_deleted": true,
                        }),
                    ))
                    .await
                    .map_err(|e| project_err(self, event, e.to_string()))?;
            }

            "ProfileUpdated" | "EmailChanged" | "PasswordChanged" => {
                let existing = store
                    .get(USERS_VIEW, id)
                    .await
                    .map_err(|e| project_err(self, event, e.to_string()))?;

                // No prior row to mutate. Either the row was already deleted
                // or this projector is being driven without seeing the
                // upstream UserRegistered event. Skip rather than guess.
                let Some(mut row) = existing else {
                    tracing::warn!(
                        event_type = event.event_type,
                        aggregate_id = id,
                        "UserProjector: skipping update for unknown user"
                    );
                    return Ok(());
                };

                match event.event_type.as_str() {
                    "ProfileUpdated" => {
                        row["name"] = json!(payload_str(&event.payload, "name")?);
                    }
                    "EmailChanged" => {
                        row["email"] = json!(payload_str(&event.payload, "email")?);
                    }
                    "PasswordChanged" => {
                        row["password_hash"] = json!(payload_str(&event.payload, "password_hash")?);
                    }
                    _ => unreachable!(),
                }
                row["version"] = json!(event.sequence);

                store
                    .upsert(Upsert::new(USERS_VIEW, id, row))
                    .await
                    .map_err(|e| project_err(self, event, e.to_string()))?;
            }

            _ => {}
        }

        Ok(())
    }
}

fn payload_str<'a>(payload: &'a Value, field: &str) -> ProjectionResult<&'a str> {
    payload.get(field).and_then(Value::as_str).ok_or_else(|| {
        ProjectionError::other(format!("event payload missing string field '{field}'"))
    })
}

fn project_err(p: &UserProjector, event: &Event, message: impl Into<String>) -> ProjectionError {
    ProjectionError::handle_failed(
        p.name(),
        &event.event_type,
        event.event_id.to_string(),
        message,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use arc_core::audit::AuditMetadata;
    use arc_core::read_model_store::InMemoryReadModelStore;

    fn ev(agg_id: &str, seq: i64, ty: &str, payload: Value) -> Event {
        Event::new(NewEvent {
            aggregate_type: "User",
            aggregate_id: agg_id,
            sequence: seq,
            event_type: ty,
            payload,
        })
        .with_audit(AuditMetadata::test_default())
    }

    /// Suspend the older fold after it has built its complete v2 row but before
    /// the version-gated write. The newer delivery must commit while it waits.
    struct PausedUpsertStore {
        inner: InMemoryReadModelStore,
        pause_once: std::sync::atomic::AtomicBool,
        entered: tokio::sync::Notify,
        release: tokio::sync::Notify,
    }

    #[async_trait]
    impl ReadModelStore for PausedUpsertStore {
        async fn upsert(&self, op: Upsert) -> arc_core::read_model_store::ReadModelResult<()> {
            if op.row["version"] == 2
                && self
                    .pause_once
                    .swap(false, std::sync::atomic::Ordering::SeqCst)
            {
                self.entered.notify_one();
                self.release.notified().await;
            }
            self.inner.upsert(op).await
        }

        async fn delete(
            &self,
            table: &str,
            key: &str,
        ) -> arc_core::read_model_store::ReadModelResult<()> {
            self.inner.delete(table, key).await
        }

        async fn get(
            &self,
            table: &str,
            key: &str,
        ) -> arc_core::read_model_store::ReadModelResult<Option<Value>> {
            self.inner.get(table, key).await
        }

        async fn find_by(
            &self,
            table: &str,
            field: &str,
            value: &Value,
        ) -> arc_core::read_model_store::ReadModelResult<Vec<Value>> {
            self.inner.find_by(table, field, value).await
        }

        async fn list(
            &self,
            table: &str,
        ) -> arc_core::read_model_store::ReadModelResult<Vec<Value>> {
            self.inner.list(table).await
        }

        async fn collection(
            &self,
            table: &str,
            query: &arc_core::read_model_store::CollectionQuery,
        ) -> arc_core::read_model_store::ReadModelResult<arc_core::read_model_store::CollectionPage>
        {
            self.inner.collection(table, query).await
        }

        async fn truncate(&self, table: &str) -> arc_core::read_model_store::ReadModelResult<()> {
            self.inner.truncate(table).await
        }
    }

    async fn assert_paused_snapshot_cannot_overwrite_newer_delivery(newer: Event) {
        use arc_core::event_store::{EventStore, InMemoryEventStore, VersionCheck};
        use arc_core::projection::ProjectionEngine;
        use std::sync::Arc;
        use std::time::Duration;
        let registration = ev(
            "u1",
            1,
            "UserRegistered",
            json!({
                "id":"u1", "name":"Old", "email":"old@example.test", "password_hash":"old-hash"
            }),
        );
        let profile = ev("u1", 2, "ProfileUpdated", json!({"name":"New"}));
        let event_store = InMemoryEventStore::new();
        event_store
            .append_to(
                "User",
                "u1",
                VersionCheck::New,
                vec![registration, profile.clone()],
            )
            .await
            .unwrap();
        let store = Arc::new(PausedUpsertStore {
            inner: InMemoryReadModelStore::new(),
            pause_once: std::sync::atomic::AtomicBool::new(true),
            entered: tokio::sync::Notify::new(),
            release: tokio::sync::Notify::new(),
        });
        let mut engine = ProjectionEngine::new(Box::new(event_store.clone()));
        engine.register_projector(Box::new(UserProjector), store.clone(), USERS_VIEW);
        let engine = Arc::new(engine);
        let older_engine = engine.clone();
        let older = tokio::spawn(async move { older_engine.process(&profile).await });
        tokio::time::timeout(Duration::from_secs(5), store.entered.notified())
            .await
            .expect("older authoritative fold reached its v2 upsert barrier");

        // Only now append v3: the suspended fold necessarily contains stale v2
        // state, rather than both tasks reconstructing the same complete history.
        event_store
            .append_to("User", "u1", VersionCheck::Expected(2), vec![newer.clone()])
            .await
            .unwrap();
        engine
            .process_external_delivery(&newer.with_stored_timestamp_precision())
            .await
            .unwrap();
        let latest = store.inner.get_rows(USERS_VIEW);
        assert_eq!(latest.len(), 1);
        assert_eq!(latest[0]["version"], 3);
        let expected = latest[0].clone();
        if expected["_arc_deleted"] != true {
            assert_eq!(expected["name"], "New");
            assert_eq!(expected["email"], "new@example.test");
            assert_eq!(expected["password_hash"], "old-hash");
        }

        store.release.notify_one();
        tokio::time::timeout(Duration::from_secs(5), older)
            .await
            .expect("older projection finishes after releasing the barrier")
            .expect("older projection task did not panic")
            .expect("stale projection is safely ignored");
        assert_eq!(store.inner.get_rows(USERS_VIEW), vec![expected.clone()]);
        if expected["_arc_deleted"] == true {
            assert!(store.get(USERS_VIEW, "u1").await.unwrap().is_none());
            assert!(store.list(USERS_VIEW).await.unwrap().is_empty());
        }
    }

    #[tokio::test]
    async fn delayed_snapshot_cannot_overwrite_newer_complete_user_state() {
        assert_paused_snapshot_cannot_overwrite_newer_delivery(ev(
            "u1",
            3,
            "EmailChanged",
            json!({"email":"new@example.test"}),
        ))
        .await;
    }

    #[tokio::test]
    async fn delayed_snapshot_cannot_resurrect_newer_user_tombstone() {
        assert_paused_snapshot_cannot_overwrite_newer_delivery(ev(
            "u1",
            3,
            "UserDeleted",
            json!({}),
        ))
        .await;
    }

    #[tokio::test]
    async fn authoritative_delivery_reconciles_reordered_and_concurrent_partial_updates() {
        use arc_core::event_store::{EventStore, InMemoryEventStore, VersionCheck};
        use arc_core::projection::ProjectionEngine;
        use std::sync::Arc;
        let events = vec![
            ev(
                "u1",
                1,
                "UserRegistered",
                json!({"id":"u1","name":"Old","email":"old@example.test","password_hash":"old-hash"}),
            ),
            ev("u1", 2, "ProfileUpdated", json!({"name":"New"})),
            ev("u1", 3, "EmailChanged", json!({"email":"new@example.test"})),
            ev(
                "u1",
                4,
                "PasswordChanged",
                json!({"password_hash":"new-hash"}),
            ),
        ];
        let event_store = InMemoryEventStore::new();
        event_store
            .append_to("User", "u1", VersionCheck::New, events.clone())
            .await
            .unwrap();
        let store = Arc::new(InMemoryReadModelStore::new());
        let mut engine = ProjectionEngine::new(Box::new(event_store));
        engine.register_projector(Box::new(UserProjector), store.clone(), USERS_VIEW);
        // Even a later update delivered before registration projects a complete row.
        engine.process(&events[2]).await.unwrap();
        let (older, newer) = tokio::join!(engine.process(&events[1]), engine.process(&events[3]));
        older.unwrap();
        newer.unwrap();
        engine.process(&events[0]).await.unwrap();
        let row = store.get(USERS_VIEW, "u1").await.unwrap().unwrap();
        assert_eq!(row["name"], "New");
        assert_eq!(row["email"], "new@example.test");
        assert_eq!(row["password_hash"], "new-hash");
        assert_eq!(row["version"], 4);
    }

    #[tokio::test]
    async fn deleted_user_cannot_be_resurrected_by_historical_registration() {
        use arc_core::event_store::{EventStore, InMemoryEventStore, VersionCheck};
        use arc_core::projection::ProjectionEngine;
        use std::sync::Arc;
        let registration = ev(
            "u1",
            1,
            "UserRegistered",
            json!({"id":"u1","name":"Old","email":"old@example.test","password_hash":"old-hash"}),
        );
        let deleted = ev("u1", 2, "UserDeleted", json!({}));
        let event_store = InMemoryEventStore::new();
        event_store
            .append_to(
                "User",
                "u1",
                VersionCheck::New,
                vec![registration.clone(), deleted.clone()],
            )
            .await
            .unwrap();
        let store = Arc::new(InMemoryReadModelStore::new());
        let mut engine = ProjectionEngine::new(Box::new(event_store));
        engine.register_projector(Box::new(UserProjector), store.clone(), USERS_VIEW);
        engine
            .process_external_delivery(&deleted.with_stored_timestamp_precision())
            .await
            .unwrap();
        engine
            .process_external_delivery(&registration.clone().with_stored_timestamp_precision())
            .await
            .unwrap();
        // Low-level replay must also respect the retained tombstone's version.
        UserProjector
            .apply(&registration, store.as_ref())
            .await
            .unwrap();
        assert!(store.get(USERS_VIEW, "u1").await.unwrap().is_none());
        assert!(store
            .find_by(USERS_VIEW, "email", &json!("old@example.test"))
            .await
            .unwrap()
            .is_empty());
        assert!(store.list(USERS_VIEW).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn registered_then_profile_updated_carries_email_forward() {
        let store = InMemoryReadModelStore::new();
        let p = UserProjector::new();

        p.apply(
            &ev(
                "u1",
                1,
                "UserRegistered",
                json!({"id":"u1","name":"Alice","email":"a@b.c","password_hash":"$argon2$x"}),
            ),
            &store,
        )
        .await
        .unwrap();

        p.apply(
            &ev("u1", 2, "ProfileUpdated", json!({"name":"Alice2"})),
            &store,
        )
        .await
        .unwrap();

        let row = store.get(USERS_VIEW, "u1").await.unwrap().unwrap();
        assert_eq!(row["name"], "Alice2");
        assert_eq!(row["email"], "a@b.c");
        assert_eq!(row["version"], 2);
    }

    #[tokio::test]
    async fn deleted_removes_row() {
        let store = InMemoryReadModelStore::new();
        let p = UserProjector::new();

        p.apply(
            &ev(
                "u1",
                1,
                "UserRegistered",
                json!({"id":"u1","name":"Alice","email":"a@b.c","password_hash":"$argon2$x"}),
            ),
            &store,
        )
        .await
        .unwrap();
        p.apply(&ev("u1", 2, "UserDeleted", json!({})), &store)
            .await
            .unwrap();

        assert!(store.get(USERS_VIEW, "u1").await.unwrap().is_none());
    }

    #[tokio::test]
    async fn duplicate_event_delivery_is_idempotent() {
        // Pinned: the framework promises at-least-once semantics on the bus
        // and replay-from-zero on rebuild. Re-applying the same event must
        // not corrupt state.
        let store = InMemoryReadModelStore::new();
        let p = UserProjector::new();
        let registered = ev(
            "u1",
            1,
            "UserRegistered",
            json!({"id":"u1","name":"Alice","email":"a@b.c","password_hash":"$argon2$x"}),
        );
        let updated = ev("u1", 2, "ProfileUpdated", json!({"name":"Alice2"}));

        for _ in 0..3 {
            p.apply(&registered, &store).await.unwrap();
            p.apply(&updated, &store).await.unwrap();
        }

        let row = store.get(USERS_VIEW, "u1").await.unwrap().unwrap();
        assert_eq!(row["name"], "Alice2");
        assert_eq!(row["version"], 2);
    }

    #[tokio::test]
    async fn out_of_order_replay_does_not_regress() {
        let store = InMemoryReadModelStore::new();
        let p = UserProjector::new();

        p.apply(
            &ev(
                "u1",
                1,
                "UserRegistered",
                json!({"id":"u1","name":"Alice","email":"a@b.c","password_hash":"$argon2$x"}),
            ),
            &store,
        )
        .await
        .unwrap();
        // sequence 3 arrives first
        p.apply(
            &ev("u1", 3, "EmailChanged", json!({"email":"new@b.c"})),
            &store,
        )
        .await
        .unwrap();
        // sequence 2 arrives later — must not stomp newer email
        p.apply(
            &ev("u1", 2, "ProfileUpdated", json!({"name":"Alice2"})),
            &store,
        )
        .await
        .unwrap();

        let row = store.get(USERS_VIEW, "u1").await.unwrap().unwrap();
        assert_eq!(row["email"], "new@b.c");
        assert_eq!(row["version"], 3);
    }

    #[tokio::test]
    async fn update_without_prior_row_is_a_warn_skip() {
        let store = InMemoryReadModelStore::new();
        let p = UserProjector::new();
        p.apply(
            &ev("u-orphan", 5, "ProfileUpdated", json!({"name":"X"})),
            &store,
        )
        .await
        .unwrap();
        assert!(store.get(USERS_VIEW, "u-orphan").await.unwrap().is_none());
    }
}

// ---------------------------------------------------------------------------
// Integration test — replay-from-zero against the real SQLite stack.
//
// This exercises the full Step 2 pipeline end-to-end without HTTP: writes
// land in `SqliteEventStore`, `ProjectionEngine::rebuild_all()` truncates
// `users_view` and replays the entire event log through `UserProjector`
// against `SqliteReadModelStore`. Asserts deterministic convergence — the
// promise that lets operators recover from a corrupted read model with
// `rebuild_all`.
#[cfg(test)]
mod replay_from_zero {
    use super::{UserProjector, USERS_VIEW};
    use crate::helpers::database::get_connection;
    use crate::helpers::database::MIGRATIONS;
    use crate::helpers::test::InMemoryTestGuard;
    use arc_core::audit::AuditMetadata;
    use arc_core::event::{Event, NewEvent};
    use arc_core::event_store::{EventStore, VersionCheck};
    use arc_core::projection::ProjectionEngine;
    use arc_core::read_model_store::ReadModelStore;
    use arc_es_sqlite::{SqliteEventStore, SqliteReadModelStore};
    use diesel_migrations::MigrationHarness;
    use serde_json::json;
    use serial_test::serial;
    use std::env;
    use std::sync::Arc;

    fn ev(agg_id: &str, seq: i64, ty: &str, payload: serde_json::Value) -> Event {
        Event::new(NewEvent {
            aggregate_type: "User",
            aggregate_id: agg_id,
            sequence: seq,
            event_type: ty,
            payload,
        })
        .with_audit(AuditMetadata::test_default())
    }

    #[serial]
    #[tokio::test]
    async fn rebuild_all_converges_to_deterministic_users_view() {
        let _guard = InMemoryTestGuard;
        env::set_var("DATABASE_URL", "file::memory:?cache=shared");

        // Run schema migrations once on the shared in-memory database.
        let mut conn = get_connection();
        conn.run_pending_migrations(MIGRATIONS).expect("migrations");
        drop(conn);

        let event_store = SqliteEventStore::new("file::memory:?cache=shared")
            .await
            .expect("event store");
        let rm_store: Arc<dyn ReadModelStore> = Arc::new(
            SqliteReadModelStore::new("file::memory:?cache=shared")
                .await
                .expect("rm store"),
        );

        // u1: register → update profile → change email → delete.
        // u2: register only — survives.
        event_store
            .append(
                "u1",
                VersionCheck::New,
                vec![
                    ev(
                        "u1",
                        1,
                        "UserRegistered",
                        json!({"id":"u1","name":"Alice","email":"a@b.c","password_hash":"$argon$x"}),
                    ),
                    ev("u1", 2, "ProfileUpdated", json!({"name":"Alice2"})),
                    ev("u1", 3, "EmailChanged", json!({"email":"a2@b.c"})),
                    ev("u1", 4, "UserDeleted", json!({})),
                ],
            )
            .await
            .unwrap();
        event_store
            .append(
                "u2",
                VersionCheck::New,
                vec![ev(
                    "u2",
                    1,
                    "UserRegistered",
                    json!({"id":"u2","name":"Bob","email":"b@b.c","password_hash":"$argon$y"}),
                )],
            )
            .await
            .unwrap();

        // Build engine, register projector, rebuild from zero.
        let mut engine = ProjectionEngine::new(Box::new(event_store));
        engine.register_projector(Box::new(UserProjector::new()), rm_store.clone(), USERS_VIEW);
        engine.rebuild_all().await.expect("rebuild_all");

        // u1 should be absent (deleted), u2 present with correct fields.
        assert!(rm_store.get(USERS_VIEW, "u1").await.unwrap().is_none());
        let u2 = rm_store.get(USERS_VIEW, "u2").await.unwrap().unwrap();
        assert_eq!(u2["name"], "Bob");
        assert_eq!(u2["email"], "b@b.c");
        assert_eq!(u2["version"], 1);

        // Email lookup hits the JSON-extract index.
        let by_email = rm_store
            .find_by(USERS_VIEW, "email", &json!("b@b.c"))
            .await
            .unwrap();
        assert_eq!(by_email.len(), 1);
        assert_eq!(by_email[0]["id"], "u2");

        // Idempotency: a second rebuild produces the same final state.
        engine.rebuild_all().await.expect("rebuild_all again");
        assert!(rm_store.get(USERS_VIEW, "u1").await.unwrap().is_none());
        assert_eq!(
            rm_store.get(USERS_VIEW, "u2").await.unwrap().unwrap()["version"],
            1
        );
    }
}
