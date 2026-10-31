//! Actual HTTP projection boundary rejects unpersisted and modified event envelopes.
#[path = "support/env_guard.rs"]
mod env_guard;
use actix_web::{web, App, HttpServer};
use arc::domain::user::projector::{UserProjector, USERS_VIEW};
use arc::http::controllers::internal_projection_controller::handle_user_projection;
use arc_core::{
    audit::AuditMetadata,
    event::{Event, NewEvent},
    event_store::{EventStore, InMemoryEventStore, VersionCheck},
    projection::ProjectionEngine,
    read_model_store::{InMemoryReadModelStore, ReadModelStore},
};
use std::{
    io::{Read, Write},
    sync::Arc,
};

#[actix_web::test]
async fn projection_requires_persisted_unchanged_event_and_is_idempotent() {
    let _env = env_guard::EnvGuard::new(&[("INTERNAL_PROJECTION_TOKEN", Some("fixture-token"))]);
    let models: Arc<dyn ReadModelStore> = Arc::new(InMemoryReadModelStore::new());
    let event_store = InMemoryEventStore::new();
    let mut engine = ProjectionEngine::new(Box::new(event_store.clone()));
    engine.register_projector(Box::new(UserProjector::new()), models.clone(), USERS_VIEW);
    let engine = web::Data::new(engine);
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let server = HttpServer::new(move || {
        App::new()
            .app_data(engine.clone())
            .service(handle_user_projection)
    })
    .workers(1)
    .listen(listener)
    .unwrap()
    .run();
    let handle = server.handle();
    actix_web::rt::spawn(server);
    let event = Event::new(NewEvent { aggregate_type: "User", aggregate_id: "persisted", sequence: 1, event_type: "UserRegistered", payload: serde_json::json!({"name":"Ada", "email":"ada@example.test", "password_hash":"persisted-hash"}) }).with_audit(AuditMetadata::test_default()).with_stored_timestamp_precision();
    event_store
        .append_to(
            "User",
            &event.aggregate_id,
            VersionCheck::New,
            vec![event.clone()],
        )
        .await
        .unwrap();
    let mut forged = event.clone();
    forged.aggregate_id = "forged".to_string();
    let mut tampered = event.clone();
    tampered.payload["email"] = serde_json::json!("attacker@example.test");
    let mut timestamp_tampered = event.clone();
    timestamp_tampered.timestamp += 1;
    let mut metadata_tampered = event.clone();
    metadata_tampered.audit.actor_id = "attacker".to_string();
    for credential in ["", "Bearer wrong", "Basic fixture-token"] {
        let response = post(addr, credential, &event).await;
        assert!(response.starts_with("HTTP/1.1 401"), "{response}");
        assert!(models.get(USERS_VIEW, "persisted").await.unwrap().is_none());
    }
    for submitted in [&forged, &tampered, &timestamp_tampered, &metadata_tampered] {
        let response = post(addr, "Bearer fixture-token", submitted).await;
        assert!(response.starts_with("HTTP/1.1 400"), "{response}");
        assert!(models.get(USERS_VIEW, "persisted").await.unwrap().is_none());
        assert!(models.get(USERS_VIEW, "forged").await.unwrap().is_none());
    }
    let first = post(addr, "Bearer fixture-token", &event).await;
    assert!(first.starts_with("HTTP/1.1 204"), "{first}");
    let first_row = models.get(USERS_VIEW, "persisted").await.unwrap();
    let second = post(addr, "Bearer fixture-token", &event).await;
    assert!(second.starts_with("HTTP/1.1 204"), "{second}");
    let second_row = models.get(USERS_VIEW, "persisted").await.unwrap();
    handle.stop(true).await;
    assert_eq!(
        first_row, second_row,
        "duplicate delivery must not change projection state"
    );
    assert_eq!(second_row.unwrap()["email"], "ada@example.test");
}
async fn post(addr: std::net::SocketAddr, authorization: &str, event: &Event) -> String {
    let body = serde_json::to_string(event).unwrap();
    let authorization = authorization.to_owned();
    tokio::task::spawn_blocking(move || {
        let mut socket = std::net::TcpStream::connect(addr).unwrap();
        socket.set_read_timeout(Some(std::time::Duration::from_secs(5))).unwrap();
        write!(socket,"POST /users/handle HTTP/1.1\r\nHost: localhost\r\nAuthorization: {authorization}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len()).unwrap();
        let mut response = String::new(); socket.read_to_string(&mut response).unwrap(); response
    }).await.unwrap()
}
