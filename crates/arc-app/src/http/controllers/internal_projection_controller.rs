use actix_web::{post, web, HttpRequest, HttpResponse, Responder};
use arc_core::event::Event;
#[cfg(test)]
use arc_core::event::NewEvent;
use arc_core::projection::{ExternalProjectionError, ProjectionEngine};
use serde_json::json;

fn configured_token() -> Option<String> {
    std::env::var("INTERNAL_PROJECTION_TOKEN")
        .ok()
        .filter(|token| !token.trim().is_empty())
}

fn bearer_token(req: &HttpRequest) -> Option<&str> {
    req.headers()
        .get("Authorization")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
}

fn authorized(req: &HttpRequest) -> Result<(), Box<HttpResponse>> {
    let Some(expected) = configured_token() else {
        tracing::error!("INTERNAL_PROJECTION_TOKEN is not configured");
        return Err(Box::new(
            HttpResponse::ServiceUnavailable()
                .json(json!({"error": "Projection handler is not configured"})),
        ));
    };

    match bearer_token(req) {
        Some(actual) if actual == expected => Ok(()),
        _ => Err(Box::new(
            HttpResponse::Unauthorized().json(json!({"error": "Unauthorized"})),
        )),
    }
}

#[post("/users/handle")]
pub async fn handle_user_projection(
    req: HttpRequest,
    event: web::Json<Event>,
    projection_engine: web::Data<ProjectionEngine>,
) -> impl Responder {
    if let Err(response) = authorized(&req) {
        return *response;
    }

    if event.aggregate_type != "User" {
        return HttpResponse::BadRequest()
            .json(json!({"error": "Projection endpoint only accepts User events"}));
    }

    match projection_engine.process_external_delivery(&event).await {
        Ok(()) => HttpResponse::NoContent().finish(),
        Err(ExternalProjectionError::MissingEvent | ExternalProjectionError::EventMismatch) => {
            HttpResponse::BadRequest().json(json!({"error": "Invalid event"}))
        }
        Err(ExternalProjectionError::EventStore(error)) => {
            tracing::error!(error = %error, event_id = %event.event_id, "authoritative projection event lookup failed");
            HttpResponse::ServiceUnavailable()
                .json(json!({"error": "Projection handler is unavailable"}))
        }
        Err(ExternalProjectionError::Projection(error)) => {
            tracing::error!(error = ?error, event_id = %event.event_id, event_type = event.event_type, "user projection failed");
            HttpResponse::InternalServerError().json(json!({"error": "Projection failed"}))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::user::projector::{UserProjector, USERS_VIEW};
    use actix_web::{http::StatusCode, test, App};
    use arc_core::audit::AuditMetadata;
    use arc_core::event_store::{
        EventStore, EventStoreError, EventStoreResult, InMemoryEventStore, VersionCheck,
    };
    use arc_core::read_model_store::{InMemoryReadModelStore, ReadModelStore};
    use arc_core::snapshot::Snapshot;
    use async_trait::async_trait;
    use serde_json::json;
    use serial_test::serial;
    use std::sync::Arc;

    #[derive(Debug)]
    struct EmptyEventStore;

    #[async_trait]
    impl EventStore for EmptyEventStore {
        async fn append(
            &self,
            _aggregate_id: &str,
            _version_check: VersionCheck,
            _events: Vec<Event>,
        ) -> EventStoreResult<()> {
            Err(EventStoreError::other("append not supported in test"))
        }

        async fn load(&self, _aggregate_id: &str) -> EventStoreResult<Vec<Event>> {
            Ok(vec![])
        }

        async fn load_from(
            &self,
            _aggregate_id: &str,
            _from_sequence: i64,
        ) -> EventStoreResult<Vec<Event>> {
            Ok(vec![])
        }

        async fn stream_all(&self, _from_position: i64) -> EventStoreResult<Vec<Event>> {
            Ok(vec![])
        }

        async fn get_version(&self, _aggregate_id: &str) -> EventStoreResult<i64> {
            Ok(0)
        }

        async fn save_snapshot(&self, _snapshot: &Snapshot) -> EventStoreResult<()> {
            Err(EventStoreError::other(
                "save_snapshot not supported in test",
            ))
        }

        async fn load_snapshot(&self, _aggregate_id: &str) -> EventStoreResult<Option<Snapshot>> {
            Ok(None)
        }
    }

    fn test_event() -> Event {
        Event::new(NewEvent {
            aggregate_type: "User",
            aggregate_id: "user-123",
            sequence: 1,
            event_type: "UserRegistered",
            payload: json!({
                "name": "Ada Lovelace",
                "email": "ada@example.com",
                "password_hash": "hash",
            }),
        })
        .with_audit(AuditMetadata::test_default())
        // CommandBus stores and publishes whole-second timestamps.
        .with_stored_timestamp_precision()
    }

    fn build_projection_engine(
        event_store: Box<dyn EventStore>,
        read_model_store: Arc<dyn ReadModelStore>,
    ) -> web::Data<ProjectionEngine> {
        let mut engine = ProjectionEngine::new(event_store);
        engine.register_projector(Box::new(UserProjector::new()), read_model_store, USERS_VIEW);
        web::Data::new(engine)
    }

    #[actix_web::test]
    #[serial]
    async fn rejects_missing_internal_projection_token_config() {
        std::env::remove_var("INTERNAL_PROJECTION_TOKEN");
        let store: Arc<dyn ReadModelStore> = Arc::new(InMemoryReadModelStore::new());
        let app = test::init_service(
            App::new()
                .app_data(build_projection_engine(Box::new(EmptyEventStore), store))
                .service(handle_user_projection),
        )
        .await;

        let req = test::TestRequest::post()
            .uri("/users/handle")
            .set_json(test_event())
            .to_request();
        let resp = test::call_service(&app, req).await;

        assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
    }

    #[actix_web::test]
    #[serial]
    async fn rejects_missing_bearer_token() {
        std::env::set_var("INTERNAL_PROJECTION_TOKEN", "test-token");
        let store: Arc<dyn ReadModelStore> = Arc::new(InMemoryReadModelStore::new());
        let app = test::init_service(
            App::new()
                .app_data(build_projection_engine(Box::new(EmptyEventStore), store))
                .service(handle_user_projection),
        )
        .await;

        let req = test::TestRequest::post()
            .uri("/users/handle")
            .set_json(test_event())
            .to_request();
        let resp = test::call_service(&app, req).await;

        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
        std::env::remove_var("INTERNAL_PROJECTION_TOKEN");
    }

    #[actix_web::test]
    #[serial]
    async fn rejects_projection_when_store_does_not_support_exact_lookup() {
        std::env::set_var("INTERNAL_PROJECTION_TOKEN", "test-token");
        let models: Arc<dyn ReadModelStore> = Arc::new(InMemoryReadModelStore::new());
        let app = test::init_service(
            App::new()
                .app_data(build_projection_engine(
                    Box::new(EmptyEventStore),
                    models.clone(),
                ))
                .service(handle_user_projection),
        )
        .await;

        let req = test::TestRequest::post()
            .uri("/users/handle")
            .insert_header(("Authorization", "Bearer test-token"))
            .set_json(test_event())
            .to_request();
        let resp = test::call_service(&app, req).await;

        assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert!(models.get(USERS_VIEW, "user-123").await.unwrap().is_none());
        std::env::remove_var("INTERNAL_PROJECTION_TOKEN");
    }

    #[actix_web::test]
    #[serial]
    async fn projects_user_event_through_arc_owned_read_model_store() {
        std::env::set_var("INTERNAL_PROJECTION_TOKEN", "test-token");
        let store: Arc<dyn ReadModelStore> = Arc::new(InMemoryReadModelStore::new());
        let event_store = InMemoryEventStore::new();
        let event = test_event();
        event_store
            .append_to(
                "User",
                &event.aggregate_id,
                VersionCheck::New,
                vec![event.clone()],
            )
            .await
            .expect("authoritative event append");
        let app = test::init_service(
            App::new()
                .app_data(build_projection_engine(
                    Box::new(event_store),
                    store.clone(),
                ))
                .service(handle_user_projection),
        )
        .await;

        let req = test::TestRequest::post()
            .uri("/users/handle")
            .insert_header(("Authorization", "Bearer test-token"))
            .set_json(&event)
            .to_request();
        let resp = test::call_service(&app, req).await;

        assert_eq!(resp.status(), StatusCode::NO_CONTENT);
        let row = store
            .get(USERS_VIEW, &event.aggregate_id)
            .await
            .expect("read model lookup")
            .expect("projected user row");
        assert_eq!(row["email"], "ada@example.com");
        assert_eq!(row["version"], 1);
        std::env::remove_var("INTERNAL_PROJECTION_TOKEN");
    }
}
