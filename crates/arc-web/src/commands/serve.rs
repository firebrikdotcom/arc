use crate::helpers::config::DatabaseDriver;
use crate::helpers::es_stack;
use crate::helpers::rate_limit;
use crate::http::middlewares::rate_limit_middleware::GlobalRateLimit;
use crate::websocket::server::WsServer;
use crate::{AggregateRegistration, AppState, ProjectorReg, UiRegistry};
use actix::prelude::*;
use actix_session::storage::CookieSessionStore;
use actix_session::{config::PersistentSession, SessionMiddleware};
use actix_web::cookie::{time::Duration, Key, SameSite};
use actix_web::middleware::{Compress, NormalizePath};
use actix_web::web::ServiceConfig;
use actix_web::{web, App, HttpServer};
use std::env;
use std::io;
use std::sync::Mutex;
use tracing::{info, warn};

use arc_core::aggregate::Aggregate;
use arc_core::command_bus::{CommandBus, SnapshotPolicy};
#[cfg(feature = "nats")]
use arc_core::event_bus::TwoLaneEventBus;
use arc_core::event_bus::{EventBus, InProcessEventBus};
use arc_core::projection::{ProjectionEngine, ProjectionEngineHandler};
use arc_core::session::SessionStore;
#[cfg(feature = "nats")]
use arc_es_nats::NatsEventBus;
use arc_es_sqlite::SqliteSessionStore;
use std::sync::Arc;

type RoutesFn = dyn Fn(&mut ServiceConfig) + Send + Sync + 'static;

type AppDataFn = dyn Fn(&mut ServiceConfig) + Send + Sync + 'static;

/// Type-erased Actix application data for one registered aggregate. The
/// captured closure retains the concrete `web::Data<CommandBus<A>>` type so
/// Actix can later satisfy handlers requesting that exact bus.
pub(crate) struct BuiltAggregateRuntime {
    configure: Arc<AppDataFn>,
}

impl BuiltAggregateRuntime {
    fn new<A: Aggregate + 'static>(
        command_bus: CommandBus<A>,
        read_model_store: Arc<dyn arc_core::read_model_store::ReadModelStore>,
        projection_engine: Arc<ProjectionEngine>,
    ) -> Self {
        let command_bus_data = web::Data::new(command_bus);
        let read_model_store_data = web::Data::from(read_model_store);
        let projection_engine_data = web::Data::from(projection_engine);

        Self {
            configure: Arc::new(move |cfg| {
                cfg.app_data(command_bus_data.clone())
                    .app_data(read_model_store_data.clone())
                    .app_data(projection_engine_data.clone());
            }),
        }
    }

    fn configure(&self, cfg: &mut ServiceConfig) {
        (self.configure)(cfg);
    }
}

/// Build the typed runtime for one aggregate registration.
pub(crate) async fn build_aggregate_runtime<A: Aggregate + 'static>(
    projectors: Vec<ProjectorReg>,
    snapshot_policy: Option<SnapshotPolicy>,
) -> io::Result<BuiltAggregateRuntime> {
    let driver = DatabaseDriver::from_env();
    let db_url = crate::helpers::config::database_url();
    let stores = es_stack::build_stores(driver, &db_url)
        .await
        .map_err(|error| io::Error::other(format!("failed to initialize stores: {error}")))?;
    let read_model_store = stores.read_model_store.clone();

    let mut projection_engine = ProjectionEngine::new(stores.projection_event_store);
    for reg in projectors {
        projection_engine.register_projector(reg.projector, read_model_store.clone(), reg.view);
    }
    let projection_engine = Arc::new(projection_engine);

    let event_bus_mode = event_bus_mode();
    let mut event_bus = build_event_bus(&event_bus_mode).await;
    if event_bus_mode == EventBusMode::InProcess {
        event_bus
            .subscribe(Box::new(ProjectionEngineHandler::new(
                projection_engine.clone(),
            )))
            .await
            .map_err(|error| {
                io::Error::other(format!("failed to subscribe projection engine: {error}"))
            })?;

        if let Err(error) = projection_engine.rebuild_all().await {
            tracing::error!(
                aggregate_type = A::aggregate_type(),
                error = ?error,
                "ProjectionEngine.rebuild_all failed at startup"
            );
        } else {
            info!(
                aggregate_type = A::aggregate_type(),
                "Projections rebuilt from event store"
            );
        }
    } else {
        info!(
            aggregate_type = A::aggregate_type(),
            "EVENT_BUS=nats selected; Benthos owns durable projection and event-handler delivery"
        );
    }

    let command_bus = es_stack::apply_snapshot_policy(
        CommandBus::<A>::new(stores.command_event_store, event_bus),
        snapshot_policy,
    );

    Ok(BuiltAggregateRuntime::new(
        command_bus,
        read_model_store,
        projection_engine,
    ))
}

/// Starts the Actix-Web HTTP server with every registered aggregate.
pub(crate) async fn run(
    app_url: String,
    app_port: u16,
    registrations: Vec<AggregateRegistration>,
    routes: Vec<Arc<RoutesFn>>,
    plugin_app_data: Vec<Arc<AppDataFn>>,
    ui_registry: Option<Arc<UiRegistry>>,
) -> io::Result<()> {
    let security_headers = crate::http::middlewares::security_headers::SecurityHeaders::from_env()?;
    crate::check_database_health();

    let secret_key = Key::from(
        env::var("SECRET_KEY")
            .expect("SECRET_KEY must be set")
            .as_bytes(),
    );

    let app_env = env::var("APP_ENV").unwrap_or_else(|_| "development".to_string());
    let is_production = app_env == "production";

    let session_domain = env::var("SESSION_DOMAIN").ok();

    let same_site_str = env::var("SESSION_SAME_SITE").unwrap_or_else(|_| "Lax".to_string());
    let same_site = match same_site_str.as_str() {
        "Strict" => SameSite::Strict,
        "None" => SameSite::None,
        _ => SameSite::Lax,
    };

    info!(
        environment = app_env,
        secure = is_production,
        same_site = ?same_site,
        domain = ?session_domain,
        "Configuring session middleware"
    );

    if !is_production && app_url == "0.0.0.0" {
        warn!(
            "Running in development mode with 0.0.0.0 - sessions will work across network. \
            Ensure APP_ENV=production for production deployments!"
        );
    }

    let ws_server = WsServer::new().start();

    let login_rate_limiter = rate_limit::create_rate_limiter();
    let global_rate_limiter = rate_limit::create_global_rate_limiter();

    let driver = DatabaseDriver::from_env();
    info!(driver = driver.as_str(), "Selected database driver");

    let mut aggregate_runtimes = Vec::with_capacity(registrations.len());
    for registration in registrations {
        info!(
            aggregate_type = registration.aggregate_type,
            "Registering aggregate runtime"
        );
        aggregate_runtimes.push(
            (registration.build)(registration.projectors, registration.snapshot_policy).await?,
        );
    }
    let aggregate_runtimes = Arc::new(aggregate_runtimes);

    let access_logger =
        crate::helpers::access_log_config::build_access_logger(|key| env::var(key).ok()).await?;
    let access_logger_data = web::Data::from(access_logger);

    // Server-side JWT session registry. SQLite-only today; under a non-SQLite
    // primary driver it keeps its own local SQLite database.
    let session_db_url = crate::helpers::config::session_store_url(driver);
    if !driver.is_file_backed() {
        warn!(
            session_db_url = session_db_url,
            "JWT session registry is SQLite-backed; Postgres session store is not yet implemented"
        );
    }
    let session_store_impl = SqliteSessionStore::new(&session_db_url)
        .await
        .expect("Failed to init session store");
    let session_store: Arc<dyn SessionStore> = Arc::new(session_store_impl);
    let session_store_data = web::Data::from(session_store);

    HttpServer::new(move || {
        let mut session_middleware =
            SessionMiddleware::builder(CookieSessionStore::default(), secret_key.clone())
                .cookie_name("arc_session".to_string())
                .cookie_http_only(true)
                .cookie_same_site(same_site)
                .session_lifecycle(PersistentSession::default().session_ttl(Duration::hours(24)));

        if is_production {
            session_middleware = session_middleware.cookie_secure(true);
        } else {
            session_middleware = session_middleware.cookie_secure(false);
        }

        if let Some(ref domain) = session_domain {
            session_middleware = session_middleware.cookie_domain(Some(domain.clone()));
        }

        let routes = routes.clone();
        let plugin_app_data = plugin_app_data.clone();
        let aggregate_runtimes = aggregate_runtimes.clone();
        let ui_registry = ui_registry.clone();

        App::new()
            .wrap(tracing_actix_web::TracingLogger::default())
            .wrap(GlobalRateLimit)
            .wrap(Compress::default())
            .wrap(session_middleware.build())
            .wrap(NormalizePath::trim())
            // Outermost: also covers middleware redirects and rate-limit responses.
            .wrap(security_headers.middleware())
            .app_data(web::Data::new(global_rate_limiter.clone()))
            .app_data(web::Data::new(login_rate_limiter.clone()))
            .app_data(web::Data::new(AppState {
                app_name: Mutex::from(env::var("APP_NAME").unwrap_or_else(|_| "".to_string())),
            }))
            .app_data(access_logger_data.clone())
            .app_data(session_store_data.clone())
            .app_data(web::Data::new(ws_server.clone()))
            .configure(move |cfg| {
                if let Some(registry) = &ui_registry {
                    cfg.app_data(web::Data::from(registry.clone()));
                }
                for runtime in aggregate_runtimes.iter() {
                    runtime.configure(cfg);
                }
                for register_data in &plugin_app_data {
                    register_data.as_ref()(cfg);
                }
                for register_routes in &routes {
                    register_routes.as_ref()(cfg);
                }
            })
    })
    .bind((app_url, app_port))?
    .run()
    .await
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum EventBusMode {
    InProcess,
    Nats,
}

fn event_bus_mode() -> EventBusMode {
    let event_bus = env::var("EVENT_BUS")
        .unwrap_or_else(|_| "inprocess".to_string())
        .to_ascii_lowercase();

    match event_bus.as_str() {
        "inprocess" => EventBusMode::InProcess,
        "nats" => EventBusMode::Nats,
        other => {
            warn!(
                event_bus = other,
                "Unknown EVENT_BUS value; falling back to inprocess"
            );
            EventBusMode::InProcess
        }
    }
}

async fn build_event_bus(mode: &EventBusMode) -> Box<dyn EventBus> {
    match mode {
        EventBusMode::InProcess => Box::new(InProcessEventBus::new()),
        EventBusMode::Nats => build_nats_event_bus().await,
    }
}

#[cfg(feature = "nats")]
async fn build_nats_event_bus() -> Box<dyn EventBus> {
    let nats_url = env::var("NATS_URL").unwrap_or_else(|_| "nats://127.0.0.1:4222".to_string());
    let nats_stream = env::var("NATS_STREAM").unwrap_or_else(|_| "EVENTS".to_string());
    let async_bus = Arc::new(
        NatsEventBus::new(&nats_url, &nats_stream)
            .await
            .expect("Failed to initialize NATS event bus"),
    );

    info!(
        nats_url = nats_url,
        nats_stream = nats_stream,
        "Using NATS JetStream event bus"
    );

    Box::new(TwoLaneEventBus::with_async_bus(async_bus))
}

#[cfg(not(feature = "nats"))]
async fn build_nats_event_bus() -> Box<dyn EventBus> {
    warn!("EVENT_BUS=nats requested without the nats cargo feature; falling back to inprocess");
    Box::new(InProcessEventBus::new())
}

#[cfg(test)]
mod tests {
    use super::*;
    use actix_web::{post, test, App, HttpResponse};
    use arc_core::aggregate::{Aggregate, Command};
    use arc_core::command_bus::CommandContext;
    use arc_core::event::{Event, NewEvent};
    use arc_core::event_store::{EventStore, InMemoryEventStore};
    use arc_core::projection::{ProjectionError, ProjectionResult, Projector};
    use arc_core::read_model_store::{InMemoryReadModelStore, ReadModelStore, Upsert};
    use async_trait::async_trait;
    use serde_json::json;
    use thiserror::Error;

    #[derive(Debug, Error)]
    #[error("command failed")]
    struct AggregateError;

    macro_rules! aggregate {
        ($aggregate:ident, $command:ident, $aggregate_type:literal, $event_type:literal) => {
            #[derive(Default)]
            struct $aggregate {
                version: i64,
            }

            struct $command {
                id: String,
            }

            impl Command for $command {
                fn aggregate_id(&self) -> &str {
                    &self.id
                }
            }

            #[async_trait]
            impl Aggregate for $aggregate {
                type Command = $command;
                type Event = ();
                type Error = AggregateError;

                fn aggregate_type() -> &'static str {
                    $aggregate_type
                }

                fn version(&self) -> i64 {
                    self.version
                }

                async fn handle(&self, command: Self::Command) -> Result<Vec<Event>, Self::Error> {
                    Ok(vec![Event::new(NewEvent {
                        aggregate_type: Self::aggregate_type(),
                        aggregate_id: command.id,
                        sequence: self.version + 1,
                        event_type: $event_type,
                        payload: json!({}),
                    })])
                }

                fn apply(&mut self, event: &Event) {
                    self.version = event.sequence;
                }
            }
        };
    }

    aggregate!(ProductAggregate, CreateProduct, "Product", "ProductCreated");
    aggregate!(OrderAggregate, PlaceOrder, "Order", "OrderPlaced");

    struct RecordingProjector {
        event_type: &'static str,
        table: &'static str,
    }

    #[async_trait]
    impl Projector for RecordingProjector {
        fn name(&self) -> &str {
            self.table
        }

        fn handles(&self) -> Vec<String> {
            vec![self.event_type.to_string()]
        }

        async fn apply(&self, event: &Event, store: &dyn ReadModelStore) -> ProjectionResult<()> {
            store
                .upsert(Upsert::new(
                    self.table,
                    &event.aggregate_id,
                    json!({
                        "id": event.aggregate_id.clone(),
                        "version": event.sequence,
                    }),
                ))
                .await
                .map_err(|error| ProjectionError::other(error.to_string()))
        }
    }

    async fn runtime<A: Aggregate + 'static>(
        event_store: InMemoryEventStore,
        read_model_store: Arc<InMemoryReadModelStore>,
        event_type: &'static str,
        table: &'static str,
    ) -> BuiltAggregateRuntime {
        let mut engine = ProjectionEngine::new(Box::new(event_store.clone()));
        engine.register_projector(
            Box::new(RecordingProjector { event_type, table }),
            read_model_store.clone(),
            table,
        );
        let engine = Arc::new(engine);
        let mut bus = InProcessEventBus::new();
        bus.subscribe(Box::new(ProjectionEngineHandler::new(engine.clone())))
            .await
            .unwrap();
        let command_bus = CommandBus::<A>::new(Box::new(event_store), Box::new(bus));
        BuiltAggregateRuntime::new(command_bus, read_model_store, engine)
    }

    #[post("/products")]
    async fn create_product(bus: web::Data<CommandBus<ProductAggregate>>) -> HttpResponse {
        bus.dispatch(
            CreateProduct {
                id: "shared-id".to_string(),
            },
            CommandContext::for_actor("test"),
        )
        .await
        .unwrap();
        HttpResponse::Created().finish()
    }

    #[post("/orders")]
    async fn place_order(bus: web::Data<CommandBus<OrderAggregate>>) -> HttpResponse {
        bus.dispatch(
            PlaceOrder {
                id: "shared-id".to_string(),
            },
            CommandContext::for_actor("test"),
        )
        .await
        .unwrap();
        HttpResponse::Created().finish()
    }

    #[actix_web::test]
    async fn injects_two_typed_command_buses_and_keeps_streams_separate() {
        let event_store = InMemoryEventStore::new();
        let read_model_store = Arc::new(InMemoryReadModelStore::new());
        let products = runtime::<ProductAggregate>(
            event_store.clone(),
            read_model_store.clone(),
            "ProductCreated",
            "products_view",
        )
        .await;
        let orders = runtime::<OrderAggregate>(
            event_store.clone(),
            read_model_store.clone(),
            "OrderPlaced",
            "orders_view",
        )
        .await;

        let app = test::init_service(App::new().configure(move |cfg| {
            products.configure(cfg);
            orders.configure(cfg);
            cfg.service(create_product).service(place_order);
        }))
        .await;

        let product_response = test::call_service(
            &app,
            test::TestRequest::post().uri("/products").to_request(),
        )
        .await;
        let order_response =
            test::call_service(&app, test::TestRequest::post().uri("/orders").to_request()).await;
        assert!(product_response.status().is_success());
        assert!(order_response.status().is_success());

        assert_eq!(
            event_store
                .load_stream("Product", "shared-id")
                .await
                .unwrap()
                .len(),
            1
        );
        assert_eq!(
            event_store
                .load_stream("Order", "shared-id")
                .await
                .unwrap()
                .len(),
            1
        );
        assert!(read_model_store
            .get("products_view", "shared-id")
            .await
            .unwrap()
            .is_some());
        assert!(read_model_store
            .get("orders_view", "shared-id")
            .await
            .unwrap()
            .is_some());
    }
}
