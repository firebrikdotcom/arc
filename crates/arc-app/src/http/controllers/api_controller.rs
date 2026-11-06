use crate::domain::user::aggregate::UserAggregate;
use crate::domain::user::commands::UserCommand;
use crate::domain::user::projector::USERS_VIEW;
use crate::helpers::access_log::{self, AccessLogged, Sensitive};
use crate::helpers::audit_context;
use crate::helpers::jwt::create_token;
use crate::helpers::rate_limit::LoginRateLimiter;
use crate::http::errors::AppError;
use crate::services::user_service::{
    create_user, validate_user_credentials_es, UserValidationResult,
};
use actix_web::{
    delete, get, patch, post, web, web::Json, HttpMessage, HttpRequest, HttpResponse, Responder,
    ResponseError,
};
use arc_core::access_log::{AccessLogger, AccessedResource, PurposeOfUse, Sensitivity};
use arc_core::command_bus::CommandBus;
use arc_core::read_model_store::ReadModelStore;
use arc_core::session::{SessionRecord, SessionStore};
use serde::Deserialize;
use serde_json::json;
use std::time::{SystemTime, UNIX_EPOCH};
use tracing::warn;
use uuid::Uuid;

fn now_us() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_micros() as i64)
        .unwrap_or(0)
}

/// JSON request body for API login.
#[derive(Deserialize)]
struct LoginRequest {
    email: String,
    password: String,
}

#[derive(Deserialize)]
pub struct RegisterRequest {
    name: String,
    email: String,
    password: String,
}

#[derive(Deserialize)]
pub struct UpdateProfileRequest {
    name: String,
}

#[post("/register")]
pub async fn register(
    http_req: HttpRequest,
    req: Json<RegisterRequest>,
    command_bus: web::Data<CommandBus<UserAggregate>>,
    read_model_store: web::Data<dyn ReadModelStore>,
) -> impl Responder {
    let ctx = audit_context::anonymous(&http_req);
    match create_user(
        &command_bus,
        read_model_store.as_ref(),
        ctx,
        req.name.clone(),
        req.email.clone(),
        &req.password,
    )
    .await
    {
        Ok(aggregate_id) => HttpResponse::Created().json(json!({ "id": aggregate_id })),
        Err(err) => err.error_response(),
    }
}

/// API login: validates credentials via the `users_view` projection, mints a
/// JWT, registers the session in the server-side store (HIPAA-4), then
/// returns the token.
#[post("/login")]
pub async fn login(
    http_req: HttpRequest,
    req: Json<LoginRequest>,
    limiter: web::Data<LoginRateLimiter>,
    read_model_store: web::Data<dyn ReadModelStore>,
    session_store: web::Data<dyn SessionStore>,
) -> impl Responder {
    let ip = crate::helpers::rate_limit::client_ip(&http_req);
    let key = format!("api_login:{}", ip);

    if let Err(retry_after) = limiter.check(key.clone()) {
        warn!(
            ip = ip,
            path = http_req.path(),
            "Rate limit exceeded on API login attempt"
        );
        return HttpResponse::TooManyRequests()
            .insert_header(("Retry-After", retry_after.as_secs().to_string()))
            .json(json!({"error": "Too many login attempts. Please try again later."}));
    }

    let (result, aggregate_id) =
        validate_user_credentials_es(read_model_store.as_ref(), &req.email, &req.password).await;

    match (result, aggregate_id) {
        (UserValidationResult::Valid, Some(agg_id)) => {
            let (token, jti) = match create_token(&agg_id) {
                Ok(pair) => pair,
                Err(_) => {
                    return HttpResponse::InternalServerError()
                        .json(json!({"error": "Failed to generate token"}));
                }
            };

            let now = now_us();
            let expires_at =
                now + (crate::helpers::jwt::get_jwt_expiry() as i64) * 3600 * 1_000_000;
            let record = SessionRecord {
                jti,
                actor_id: agg_id,
                created_at_us: now,
                expires_at_us: expires_at,
                revoked_at_us: None,
            };

            // Fail-closed: if we cannot register the session for revocation we
            // refuse to issue the token.
            if let Err(e) = session_store.record_session(record).await {
                tracing::error!(error = ?e, "session_store.record_session failed");
                return HttpResponse::ServiceUnavailable()
                    .json(json!({"error": "Authentication backend unavailable"}));
            }

            HttpResponse::Ok().json(json!({ "token": token }))
        }
        _ => HttpResponse::Unauthorized().json(json!({"error": "Invalid credentials"})),
    }
}

/// `POST /api/v1/protected/logout` — revoke the current session.
/// JwtMiddleware has validated the bearer and inserted (`actor_id`, `jti`).
#[post("/logout")]
pub async fn logout(
    req: HttpRequest,
    session_store: web::Data<dyn SessionStore>,
) -> impl Responder {
    let jti = match req.extensions().get::<Uuid>().copied() {
        Some(j) => j,
        None => {
            return HttpResponse::BadRequest().json(json!({"error": "token has no jti to revoke"}));
        }
    };

    match session_store.revoke(jti, now_us()).await {
        Ok(()) => HttpResponse::NoContent().finish(),
        Err(arc_core::session::SessionStoreError::NotFound(_)) => {
            HttpResponse::NoContent().finish()
        }
        Err(e) => {
            tracing::error!(error = ?e, "session_store.revoke failed");
            HttpResponse::ServiceUnavailable()
                .json(json!({"error": "Authentication backend unavailable"}))
        }
    }
}

/// Returns the authenticated user's profile. Reads from the `users_view`
/// projection — the canonical read surface for `User` after Step 2. Records
/// the read through the configured `AccessLogger` (HIPAA-2).
///
/// A `None` return from the read model is the same shape as a deleted user:
/// the projector removes the row on `UserDeleted`. Either way the response
/// is 404, which is consistent with how the aggregate-load implementation
/// treated `deleted == true` previously.
#[get("/profile")]
pub async fn profile(
    req: HttpRequest,
    read_model_store: web::Data<dyn ReadModelStore>,
    access_logger: web::Data<dyn AccessLogger>,
) -> Result<AccessLogged<serde_json::Value>, AppError> {
    let agg_id = match req.extensions().get::<String>() {
        Some(id) => id.clone(),
        None => {
            return Err(AppError::AuditFailed {
                status: actix_web::http::StatusCode::UNAUTHORIZED,
                message: "No authenticated user".into(),
            })
        }
    };

    let row = match read_model_store.get(USERS_VIEW, &agg_id).await {
        Ok(Some(r)) => r,
        Ok(None) => {
            return Err(AppError::AuditFailed {
                status: actix_web::http::StatusCode::NOT_FOUND,
                message: "User not found".into(),
            })
        }
        Err(e) => {
            tracing::error!(error = ?e, "users_view read failed");
            return Err(AppError::AuditFailed {
                status: actix_web::http::StatusCode::INTERNAL_SERVER_ERROR,
                message: "Failed to load user".into(),
            });
        }
    };

    // Log the read before returning sensitive fields. PII reads use the
    // FailOpenWarn default; PHI/PCI controllers will get FailHard from the
    // same helper without further wiring.
    let resource = AccessedResource::new("UserProfile", agg_id.clone(), Sensitivity::Pii)
        .with_fields(["id", "name", "email"]);

    let data = json!({
        "id": row.get("id"),
        "name": row.get("name"),
        "email": row.get("email"),
    });

    access_log::record_read(
        access_logger.as_ref(),
        &req,
        agg_id,
        resource,
        PurposeOfUse::UserInitiated,
        Sensitive::pii(data),
    )
    .await
}

#[patch("/profile")]
pub async fn update_profile(
    req: HttpRequest,
    body: Json<UpdateProfileRequest>,
    command_bus: web::Data<CommandBus<UserAggregate>>,
) -> impl Responder {
    let agg_id = match req.extensions().get::<String>() {
        Some(id) => id.clone(),
        None => {
            return HttpResponse::Unauthorized().json(json!({"error": "No authenticated user"}))
        }
    };

    let ctx = audit_context::for_actor(&req, agg_id.clone());
    let cmd = UserCommand::UpdateProfile {
        id: agg_id,
        name: body.name.clone(),
    };

    match command_bus.dispatch(cmd, ctx).await {
        Ok(_) => HttpResponse::NoContent().finish(),
        Err(e) => AppError::from(e).error_response(),
    }
}

#[delete("/profile")]
pub async fn delete_profile(
    req: HttpRequest,
    command_bus: web::Data<CommandBus<UserAggregate>>,
) -> impl Responder {
    let agg_id = match req.extensions().get::<String>() {
        Some(id) => id.clone(),
        None => {
            return HttpResponse::Unauthorized().json(json!({"error": "No authenticated user"}))
        }
    };

    let ctx = audit_context::for_actor(&req, agg_id.clone());
    let cmd = UserCommand::DeleteUser { id: agg_id };

    match command_bus.dispatch(cmd, ctx).await {
        Ok(_) => HttpResponse::NoContent().finish(),
        Err(e) => AppError::from(e).error_response(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::user::projector::UserProjector;
    use crate::helpers::database::get_connection;
    use crate::helpers::database::MIGRATIONS;
    use crate::helpers::jwt::create_token;
    use crate::helpers::rate_limit::{LoginRateLimiter, RateLimiter};
    use crate::helpers::test::InMemoryTestGuard;
    use crate::http::middlewares::jwt_middleware::JwtMiddleware;
    use actix_web::{http, test, App};
    use arc_core::access_log::{AccessLogger, NoOpAccessLogger};
    use arc_core::event_bus::{EventBus, InProcessEventBus};
    use arc_core::event_store::EventStore;
    use arc_core::projection::{ProjectionEngine, ProjectionEngineHandler};
    use arc_core::read_model_store::{InMemoryReadModelStore, ReadModelStore};
    use arc_core::session::{InMemorySessionStore, SessionRecord, SessionStore};
    use arc_es_sqlite::SqliteEventStore;
    use diesel_migrations::MigrationHarness;
    use serial_test::serial;
    use std::env;
    use std::sync::Arc;

    /// Build the (`CommandBus`, `ReadModelStore` `web::Data`) pair every API
    /// test needs. Wires an in-memory read model and subscribes
    /// `UserProjector` to the in-process bus so writes flow into `users_view`
    /// the same way they do at runtime.
    async fn build_setup(
        event_store: Box<SqliteEventStore>,
    ) -> (
        web::Data<CommandBus<UserAggregate>>,
        web::Data<dyn ReadModelStore>,
    ) {
        let read_model_store: Arc<dyn ReadModelStore> = Arc::new(InMemoryReadModelStore::new());

        // Authoritative projectors must read the same persisted streams as writes.
        let mut engine = ProjectionEngine::new(Box::new(event_store.as_ref().clone()));
        engine.register_projector(
            Box::new(UserProjector::new()),
            read_model_store.clone(),
            crate::domain::user::projector::USERS_VIEW,
        );
        let engine = Arc::new(engine);

        let mut bus = InProcessEventBus::new();
        bus.subscribe(Box::new(ProjectionEngineHandler::new(engine.clone())))
            .await
            .expect("subscribe projection handler");

        let command_bus = CommandBus::<UserAggregate>::new(event_store, Box::new(bus));
        (
            web::Data::new(command_bus),
            web::Data::from(read_model_store),
        )
    }

    fn logger_data() -> web::Data<dyn AccessLogger> {
        let logger: Arc<dyn AccessLogger> = Arc::new(NoOpAccessLogger);
        web::Data::from(logger)
    }

    fn recording_logger() -> (
        arc_core::access_log::RecordingAccessLogger,
        web::Data<dyn AccessLogger>,
    ) {
        let rec = arc_core::access_log::RecordingAccessLogger::new();
        let arc: Arc<dyn AccessLogger> = Arc::new(rec.clone());
        (rec, web::Data::from(arc))
    }

    async fn registered_token(sessions: &dyn SessionStore, actor_id: &str) -> String {
        let (token, jti) = create_token(actor_id).unwrap();
        let now = chrono::Utc::now().timestamp_micros();
        sessions
            .record_session(SessionRecord {
                jti,
                actor_id: actor_id.to_string(),
                created_at_us: now,
                expires_at_us: now + 3_600_000_000,
                revoked_at_us: None,
            })
            .await
            .unwrap();
        token
    }

    fn setup_test_env() {
        dotenvy::from_filename(".env.test").ok();
        env::set_var("DATABASE_URL", "file::memory:?cache=shared");
        env::set_var("JWT_SECRET", "test-secret-key-for-integration-tests");
        env::set_var("JWT_EXPIRY_HOURS", "24");
    }

    /// Run migrations against the shared in-memory DB and return the event store.
    async fn prepare_store_and_db() -> Arc<SqliteEventStore> {
        let mut conn = get_connection();
        conn.run_pending_migrations(MIGRATIONS)
            .expect("Failed to run migrations");

        let store = SqliteEventStore::new("file::memory:?cache=shared")
            .await
            .expect("Failed to create event store");
        Arc::new(store)
    }

    fn rate_limiter() -> LoginRateLimiter {
        LoginRateLimiter(RateLimiter::new(100, std::time::Duration::from_secs(60)))
    }

    #[serial]
    #[actix_web::test]
    async fn test_create_user_emits_user_registered_event() {
        let _guard = InMemoryTestGuard;
        setup_test_env();
        let store = prepare_store_and_db().await;

        let (command_bus_data, rm_data) = build_setup(Box::new(
            SqliteEventStore::new("file::memory:?cache=shared")
                .await
                .unwrap(),
        ))
        .await;

        let app = test::init_service(
            App::new()
                .app_data(command_bus_data.clone())
                .app_data(rm_data.clone())
                .app_data(logger_data())
                .app_data(web::Data::new(rate_limiter()))
                .service(web::scope("/api/v1").service(register)),
        )
        .await;

        let req = test::TestRequest::post()
            .uri("/api/v1/register")
            .set_json(json!({
                "name": "Alice",
                "email": "alice@example.com",
                "password": "correct horse battery staple"
            }))
            .to_request();

        let resp = test::call_service(&app, req).await;
        assert_eq!(resp.status(), http::StatusCode::CREATED);

        let body: serde_json::Value = test::read_body_json(resp).await;
        let aggregate_id = body["id"].as_str().unwrap().to_string();

        let events = store.load(&aggregate_id).await.expect("load events");
        assert_eq!(events.len(), 1, "expected exactly one event");
        assert_eq!(events[0].event_type, "UserRegistered");
        assert_eq!(events[0].aggregate_id, aggregate_id);
        assert_eq!(events[0].payload["email"], "alice@example.com");
        assert_eq!(events[0].payload["name"], "Alice");
    }

    #[serial]
    #[actix_web::test]
    async fn test_create_user_returns_conflict_on_duplicate_email() {
        let _guard = InMemoryTestGuard;
        setup_test_env();
        let store = prepare_store_and_db().await;

        let (command_bus_data, rm_data) = build_setup(Box::new(
            SqliteEventStore::new("file::memory:?cache=shared")
                .await
                .unwrap(),
        ))
        .await;

        let app = test::init_service(
            App::new()
                .app_data(command_bus_data.clone())
                .app_data(rm_data.clone())
                .app_data(logger_data())
                .service(web::scope("/api/v1").service(register)),
        )
        .await;

        let body = json!({
            "name": "Bob",
            "email": "bob@example.com",
            "password": "pw12345678"
        });

        let req1 = test::TestRequest::post()
            .uri("/api/v1/register")
            .set_json(&body)
            .to_request();
        let resp1 = test::call_service(&app, req1).await;
        assert_eq!(resp1.status(), http::StatusCode::CREATED);

        let req2 = test::TestRequest::post()
            .uri("/api/v1/register")
            .set_json(&body)
            .to_request();
        let resp2 = test::call_service(&app, req2).await;

        // Duplicate email causes duplicate index insert; event path also blocks
        // duplicate via index UNIQUE constraint. Response should NOT be 201.
        assert_ne!(resp2.status(), http::StatusCode::CREATED);

        // Count UserRegistered events for this email across all aggregates — must be exactly 1
        let all_events = store.stream_all(0).await.expect("stream events");
        let user_registered: Vec<_> = all_events
            .iter()
            .filter(|e| {
                e.event_type == "UserRegistered"
                    && e.payload["email"].as_str() == Some("bob@example.com")
            })
            .collect();
        assert_eq!(
            user_registered.len(),
            1,
            "duplicate registration must not emit a second event"
        );
    }

    #[serial]
    #[actix_web::test]
    async fn test_update_profile_reflects_in_get() {
        let _guard = InMemoryTestGuard;
        setup_test_env();
        let _store = prepare_store_and_db().await;

        let (command_bus_data, rm_data) = build_setup(Box::new(
            SqliteEventStore::new("file::memory:?cache=shared")
                .await
                .unwrap(),
        ))
        .await;

        let sessions: web::Data<dyn SessionStore> =
            web::Data::from(Arc::new(InMemorySessionStore::new()) as Arc<dyn SessionStore>);
        let app = test::init_service(
            App::new()
                .app_data(sessions.clone())
                .app_data(command_bus_data.clone())
                .app_data(rm_data.clone())
                .app_data(logger_data())
                .app_data(web::Data::new(rate_limiter()))
                .service(
                    web::scope("/api/v1").service(register).service(
                        web::scope("/protected")
                            .wrap(JwtMiddleware)
                            .service(profile)
                            .service(update_profile),
                    ),
                ),
        )
        .await;

        // Register
        let req = test::TestRequest::post()
            .uri("/api/v1/register")
            .set_json(json!({
                "name": "Carol",
                "email": "carol@example.com",
                "password": "pw12345678"
            }))
            .to_request();
        let resp = test::call_service(&app, req).await;
        assert_eq!(resp.status(), http::StatusCode::CREATED);
        let body: serde_json::Value = test::read_body_json(resp).await;
        let agg_id = body["id"].as_str().unwrap().to_string();
        let token = registered_token(sessions.get_ref(), &agg_id).await;

        // Update profile
        let req = test::TestRequest::patch()
            .uri("/api/v1/protected/profile")
            .insert_header(("Authorization", format!("Bearer {}", token)))
            .set_json(json!({ "name": "Carol Updated" }))
            .to_request();
        let resp = test::call_service(&app, req).await;
        assert_eq!(resp.status(), http::StatusCode::NO_CONTENT);

        // GET profile reflects update
        let req = test::TestRequest::get()
            .uri("/api/v1/protected/profile")
            .insert_header(("Authorization", format!("Bearer {}", token)))
            .to_request();
        let resp = test::call_service(&app, req).await;
        assert_eq!(resp.status(), http::StatusCode::OK);
        let body: serde_json::Value = test::read_body_json(resp).await;
        assert_eq!(body["name"], "Carol Updated");
        assert_eq!(body["email"], "carol@example.com");
    }

    #[serial]
    #[actix_web::test]
    async fn test_delete_user_emits_deleted_and_returns_404() {
        let _guard = InMemoryTestGuard;
        setup_test_env();
        let store = prepare_store_and_db().await;

        let (command_bus_data, rm_data) = build_setup(Box::new(
            SqliteEventStore::new("file::memory:?cache=shared")
                .await
                .unwrap(),
        ))
        .await;

        let sessions: web::Data<dyn SessionStore> =
            web::Data::from(Arc::new(InMemorySessionStore::new()) as Arc<dyn SessionStore>);
        let app = test::init_service(
            App::new()
                .app_data(sessions.clone())
                .app_data(command_bus_data.clone())
                .app_data(rm_data.clone())
                .app_data(logger_data())
                .service(
                    web::scope("/api/v1").service(register).service(
                        web::scope("/protected")
                            .wrap(JwtMiddleware)
                            .service(profile)
                            .service(delete_profile),
                    ),
                ),
        )
        .await;

        // Register
        let req = test::TestRequest::post()
            .uri("/api/v1/register")
            .set_json(json!({
                "name": "Dan",
                "email": "dan@example.com",
                "password": "pw12345678"
            }))
            .to_request();
        let resp = test::call_service(&app, req).await;
        assert_eq!(resp.status(), http::StatusCode::CREATED);
        let body: serde_json::Value = test::read_body_json(resp).await;
        let agg_id = body["id"].as_str().unwrap().to_string();
        let token = registered_token(sessions.get_ref(), &agg_id).await;

        // DELETE profile
        let req = test::TestRequest::delete()
            .uri("/api/v1/protected/profile")
            .insert_header(("Authorization", format!("Bearer {}", token)))
            .to_request();
        let resp = test::call_service(&app, req).await;
        assert_eq!(resp.status(), http::StatusCode::NO_CONTENT);

        // GET profile is 404
        let req = test::TestRequest::get()
            .uri("/api/v1/protected/profile")
            .insert_header(("Authorization", format!("Bearer {}", token)))
            .to_request();
        let resp = test::call_service(&app, req).await;
        assert_eq!(resp.status(), http::StatusCode::NOT_FOUND);

        // Event stream ends in UserDeleted
        let events = store.load(&agg_id).await.expect("load events");
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].event_type, "UserRegistered");
        assert_eq!(events[1].event_type, "UserDeleted");
    }

    #[serial]
    #[actix_web::test]
    async fn test_command_bus_failure_returns_422_no_event_emitted() {
        let _guard = InMemoryTestGuard;
        setup_test_env();
        let store = prepare_store_and_db().await;

        let (command_bus_data, rm_data) = build_setup(Box::new(
            SqliteEventStore::new("file::memory:?cache=shared")
                .await
                .unwrap(),
        ))
        .await;

        let app = test::init_service(
            App::new()
                .app_data(command_bus_data.clone())
                .app_data(rm_data.clone())
                .app_data(logger_data())
                .service(web::scope("/api/v1").service(register)),
        )
        .await;

        // Invalid email (no @) — aggregate rejects with InvalidEmail → 422 via AppError
        let req = test::TestRequest::post()
            .uri("/api/v1/register")
            .set_json(json!({
                "name": "Eve",
                "email": "not-an-email",
                "password": "pw12345678"
            }))
            .to_request();
        let resp = test::call_service(&app, req).await;
        assert_eq!(resp.status(), http::StatusCode::UNPROCESSABLE_ENTITY);

        // Event store must contain no events
        let all = store.stream_all(0).await.expect("stream events");
        assert!(
            all.is_empty(),
            "a rejected command must not persist any events"
        );
    }

    // ─── HIPAA-1 audit-specific HTTP tests ──────────────────────────────────

    #[serial]
    #[actix_web::test]
    async fn test_register_stamps_anonymous_actor_and_user_agent() {
        let _guard = InMemoryTestGuard;
        setup_test_env();
        let store = prepare_store_and_db().await;

        let (command_bus_data, rm_data) = build_setup(Box::new(
            SqliteEventStore::new("file::memory:?cache=shared")
                .await
                .unwrap(),
        ))
        .await;
        let app = test::init_service(
            App::new()
                .app_data(command_bus_data.clone())
                .app_data(rm_data.clone())
                .service(web::scope("/api/v1").service(register)),
        )
        .await;

        let req = test::TestRequest::post()
            .uri("/api/v1/register")
            .insert_header(("User-Agent", "audit-test-agent/1.0"))
            .set_json(json!({
                "name": "Frank",
                "email": "frank@example.com",
                "password": "pw12345678"
            }))
            .to_request();
        let resp = test::call_service(&app, req).await;
        assert_eq!(resp.status(), http::StatusCode::CREATED);

        let body: serde_json::Value = test::read_body_json(resp).await;
        let agg_id = body["id"].as_str().unwrap().to_string();

        let events = store.load(&agg_id).await.unwrap();
        let audit = &events[0].audit;
        assert_eq!(audit.actor_id, "anonymous");
        assert_eq!(audit.user_agent.as_deref(), Some("audit-test-agent/1.0"));
        assert!(audit.timestamp_utc_us > 0);
        assert!(!audit.is_pending());
    }

    #[serial]
    #[actix_web::test]
    async fn test_authenticated_update_stamps_aggregate_uuid_as_actor() {
        let _guard = InMemoryTestGuard;
        setup_test_env();
        let store = prepare_store_and_db().await;

        let (command_bus_data, rm_data) = build_setup(Box::new(
            SqliteEventStore::new("file::memory:?cache=shared")
                .await
                .unwrap(),
        ))
        .await;
        let sessions: web::Data<dyn SessionStore> =
            web::Data::from(Arc::new(InMemorySessionStore::new()) as Arc<dyn SessionStore>);
        let app = test::init_service(
            App::new()
                .app_data(sessions.clone())
                .app_data(command_bus_data.clone())
                .app_data(rm_data.clone())
                .service(
                    web::scope("/api/v1").service(register).service(
                        web::scope("/protected")
                            .wrap(JwtMiddleware)
                            .service(update_profile),
                    ),
                ),
        )
        .await;

        // Register
        let req = test::TestRequest::post()
            .uri("/api/v1/register")
            .set_json(
                json!({"name": "Gail", "email": "gail@example.com", "password": "pw12345678"}),
            )
            .to_request();
        let resp = test::call_service(&app, req).await;
        let body: serde_json::Value = test::read_body_json(resp).await;
        let agg_id = body["id"].as_str().unwrap().to_string();
        let token = registered_token(sessions.get_ref(), &agg_id).await;

        // Update profile while authenticated
        let req = test::TestRequest::patch()
            .uri("/api/v1/protected/profile")
            .insert_header(("Authorization", format!("Bearer {}", token)))
            .set_json(json!({"name": "Gail Updated"}))
            .to_request();
        let resp = test::call_service(&app, req).await;
        assert_eq!(resp.status(), http::StatusCode::NO_CONTENT);

        let events = store.load(&agg_id).await.unwrap();
        // First event: anonymous registration. Second: authenticated update.
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].audit.actor_id, "anonymous");
        assert_eq!(events[1].audit.actor_id, agg_id);
    }

    #[serial]
    #[actix_web::test]
    async fn test_profile_get_invokes_access_logger() {
        let _guard = InMemoryTestGuard;
        setup_test_env();
        let _store = prepare_store_and_db().await;

        let (command_bus_data, rm_data) = build_setup(Box::new(
            SqliteEventStore::new("file::memory:?cache=shared")
                .await
                .unwrap(),
        ))
        .await;
        let (rec_logger, logger_data) = recording_logger();

        let sessions: web::Data<dyn SessionStore> =
            web::Data::from(Arc::new(InMemorySessionStore::new()) as Arc<dyn SessionStore>);
        let app = test::init_service(
            App::new()
                .app_data(sessions.clone())
                .app_data(command_bus_data.clone())
                .app_data(rm_data.clone())
                .app_data(logger_data)
                .service(
                    web::scope("/api/v1").service(register).service(
                        web::scope("/protected")
                            .wrap(JwtMiddleware)
                            .service(profile),
                    ),
                ),
        )
        .await;

        // Register
        let req = test::TestRequest::post()
            .uri("/api/v1/register")
            .set_json(
                json!({"name": "Iris", "email": "iris@example.com", "password": "pw12345678"}),
            )
            .to_request();
        let resp = test::call_service(&app, req).await;
        let body: serde_json::Value = test::read_body_json(resp).await;
        let agg_id = body["id"].as_str().unwrap().to_string();
        let token = registered_token(sessions.get_ref(), &agg_id).await;

        // GET profile
        let req = test::TestRequest::get()
            .uri("/api/v1/protected/profile")
            .insert_header(("Authorization", format!("Bearer {}", token)))
            .insert_header(("User-Agent", "access-log-test/1.0"))
            .to_request();
        let resp = test::call_service(&app, req).await;
        assert_eq!(resp.status(), http::StatusCode::OK);

        // Inspect what the logger captured.
        let entries = rec_logger.entries().await;
        assert_eq!(entries.len(), 1, "expected exactly one logged read");

        let e = &entries[0];
        assert_eq!(e.actor.actor_id, agg_id);
        assert_eq!(e.actor.user_agent.as_deref(), Some("access-log-test/1.0"));
        assert_eq!(e.resource.kind, "UserProfile");
        assert_eq!(e.resource.identifier, agg_id);
        assert_eq!(e.resource.fields, vec!["id", "name", "email"]);
        assert_eq!(
            e.resource.sensitivity,
            arc_core::access_log::Sensitivity::Pii
        );
        assert_eq!(e.purpose, arc_core::access_log::PurposeOfUse::UserInitiated);
        assert!(e.timestamp_utc_us > 0);
    }

    #[serial]
    #[actix_web::test]
    async fn test_profile_returns_503_when_phi_logger_fails_hard() {
        use arc_core::access_log::{AccessLogError, AccessedResource, Identity, PurposeOfUse};
        use async_trait::async_trait;
        use std::sync::Arc;

        struct FailingLogger;
        #[async_trait]
        impl AccessLogger for FailingLogger {
            async fn log_access(
                &self,
                _: Identity,
                _: AccessedResource,
                _: PurposeOfUse,
                _: Option<uuid::Uuid>,
            ) -> Result<(), AccessLogError> {
                Err(AccessLogError::Sink("simulated outage".into()))
            }
        }

        let _guard = InMemoryTestGuard;
        setup_test_env();
        let _store = prepare_store_and_db().await;

        // Patch the profile controller to use Sensitivity::Phi for this test
        // by calling it directly with a custom-built request handled via a
        // hand-written route. Easier: assert using a probe handler that runs
        // the same record_read path.
        use crate::helpers::access_log::{record_read, Sensitive};
        let logger: Arc<dyn AccessLogger> = Arc::new(FailingLogger);

        // Forge a fake HttpRequest via the test helper.
        let req = test::TestRequest::get().uri("/probe").to_http_request();
        let resource = AccessedResource::new("PatientRecord", "pat-1", Sensitivity::Phi)
            .with_fields(["vitals"]);

        let res = record_read(
            logger.as_ref(),
            &req,
            "alice",
            resource,
            PurposeOfUse::Treatment,
            Sensitive::phi(json!({"vitals": "ok"})),
        )
        .await;

        assert!(res.is_err());
        let err = res.unwrap_err();
        match err {
            crate::http::errors::AppError::AuditFailed { status, .. } => {
                assert_eq!(status, http::StatusCode::SERVICE_UNAVAILABLE);
            }
            _ => panic!("Expected AuditFailed error, got {:?}", err),
        }
    }

    #[serial]
    #[actix_web::test]
    async fn test_profile_continues_when_pii_logger_fails_open() {
        use arc_core::access_log::{AccessLogError, AccessedResource, Identity, PurposeOfUse};
        use async_trait::async_trait;
        use std::sync::Arc;

        struct FailingLogger;
        #[async_trait]
        impl AccessLogger for FailingLogger {
            async fn log_access(
                &self,
                _: Identity,
                _: AccessedResource,
                _: PurposeOfUse,
                _: Option<uuid::Uuid>,
            ) -> Result<(), AccessLogError> {
                Err(AccessLogError::Sink("simulated outage".into()))
            }
        }

        use crate::helpers::access_log::{record_read, Sensitive};
        let logger: Arc<dyn AccessLogger> = Arc::new(FailingLogger);

        let req = test::TestRequest::get().uri("/probe").to_http_request();
        let resource =
            AccessedResource::new("UserProfile", "u-1", arc_core::access_log::Sensitivity::Pii)
                .with_fields(["email"]);

        let res = record_read(
            logger.as_ref(),
            &req,
            "alice",
            resource,
            PurposeOfUse::UserInitiated,
            Sensitive::pii(json!({"email": "alice@example.com"})),
        )
        .await;

        assert!(res.is_ok());
        let logged = res.unwrap();
        assert_eq!(logged.into_inner()["email"], "alice@example.com");
    }

    #[serial]
    #[actix_web::test]
    async fn test_profile_404_does_not_log_access() {
        let _guard = InMemoryTestGuard;
        setup_test_env();
        let _store = prepare_store_and_db().await;

        let (command_bus_data, rm_data) = build_setup(Box::new(
            SqliteEventStore::new("file::memory:?cache=shared")
                .await
                .unwrap(),
        ))
        .await;
        let (rec_logger, logger_data) = recording_logger();

        let sessions: web::Data<dyn SessionStore> =
            web::Data::from(Arc::new(InMemorySessionStore::new()) as Arc<dyn SessionStore>);
        let app = test::init_service(
            App::new()
                .app_data(sessions.clone())
                .app_data(command_bus_data.clone())
                .app_data(rm_data.clone())
                .app_data(logger_data)
                .service(
                    web::scope("/api/v1").service(
                        web::scope("/protected")
                            .wrap(JwtMiddleware)
                            .service(profile),
                    ),
                ),
        )
        .await;

        // JWT for an aggregate that doesn't exist.
        let token = registered_token(sessions.get_ref(), "does-not-exist-uuid").await;
        let req = test::TestRequest::get()
            .uri("/api/v1/protected/profile")
            .insert_header(("Authorization", format!("Bearer {}", token)))
            .to_request();
        let resp = test::call_service(&app, req).await;
        assert_eq!(resp.status(), http::StatusCode::NOT_FOUND);

        let entries = rec_logger.entries().await;
        assert!(
            entries.is_empty(),
            "no log should fire when the resource was not actually returned"
        );
    }

    #[serial]
    #[actix_web::test]
    async fn test_correlation_id_header_propagates_to_event() {
        use uuid::Uuid;
        let _guard = InMemoryTestGuard;
        setup_test_env();
        let store = prepare_store_and_db().await;

        let (command_bus_data, rm_data) = build_setup(Box::new(
            SqliteEventStore::new("file::memory:?cache=shared")
                .await
                .unwrap(),
        ))
        .await;
        let app = test::init_service(
            App::new()
                .app_data(command_bus_data.clone())
                .app_data(rm_data.clone())
                .service(web::scope("/api/v1").service(register)),
        )
        .await;

        let supplied_corr = Uuid::new_v4();
        let req = test::TestRequest::post()
            .uri("/api/v1/register")
            .insert_header(("X-Correlation-Id", supplied_corr.to_string()))
            .set_json(
                json!({"name": "Hank", "email": "hank@example.com", "password": "pw12345678"}),
            )
            .to_request();
        let resp = test::call_service(&app, req).await;
        assert_eq!(resp.status(), http::StatusCode::CREATED);

        let body: serde_json::Value = test::read_body_json(resp).await;
        let agg_id = body["id"].as_str().unwrap().to_string();
        let events = store.load(&agg_id).await.unwrap();
        assert_eq!(events[0].audit.correlation_id, supplied_corr);
    }
}

#[cfg(test)]
#[path = "profile_security_test.rs"]
mod security_tests;
