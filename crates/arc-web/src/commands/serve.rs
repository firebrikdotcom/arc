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
use actix_web::middleware::{Compress, DefaultHeaders, NormalizePath};
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
    publication_relay: Option<PublicationRelay>,
}

/// The relay lives exactly as long as its aggregate runtime. Pending entries
/// remain durable if shutdown interrupts a publish before its acknowledgement.
struct PublicationRelay(tokio::task::JoinHandle<()>);

impl Drop for PublicationRelay {
    fn drop(&mut self) {
        self.0.abort();
    }
}

impl BuiltAggregateRuntime {
    pub(crate) fn new<A: Aggregate + 'static>(
        command_bus: Arc<CommandBus<A>>,
        read_model_store: Arc<dyn arc_core::read_model_store::ReadModelStore>,
        projection_engine: Arc<ProjectionEngine>,
    ) -> Self {
        let command_bus_data = web::Data::from(command_bus);
        let read_model_store_data = web::Data::from(read_model_store);
        let projection_engine_data = web::Data::from(projection_engine);

        Self {
            publication_relay: None,
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
    let mut event_bus = build_event_bus(&event_bus_mode).await?;
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
    let command_bus = if event_bus_mode == EventBusMode::Nats {
        command_bus.with_durable_publication().map_err(|error| {
            io::Error::other(format!("durable event publication is unavailable: {error}"))
        })?
    } else {
        command_bus
    };
    let command_bus = Arc::new(command_bus);
    let mut runtime =
        BuiltAggregateRuntime::new(command_bus.clone(), read_model_store, projection_engine);
    if event_bus_mode == EventBusMode::Nats {
        runtime.publication_relay = Some(start_publication_relay(command_bus));
    }
    Ok(runtime)
}

fn start_publication_relay<A: Aggregate + 'static>(
    command_bus: Arc<CommandBus<A>>,
) -> PublicationRelay {
    PublicationRelay(tokio::spawn(async move {
        let mut interval = tokio::time::interval(std::time::Duration::from_secs(1));
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            // The first tick is immediate, recovering entries from a previous
            // process even if no new command arrives after restart.
            interval.tick().await;
            let outcome = tokio::time::timeout(
                std::time::Duration::from_secs(10),
                arc_core::publication::relay_pending_publications(
                    command_bus.event_store(),
                    command_bus.event_bus(),
                    A::aggregate_type(),
                    100,
                ),
            )
            .await;
            match outcome {
                Ok(Ok(_)) => {}
                Ok(Err(error)) => warn!(aggregate_type = A::aggregate_type(), error = ?error,
                    "Event publication remains queued for retry"),
                Err(_) => warn!(
                    aggregate_type = A::aggregate_type(),
                    "Event publication timed out; pending entries will be retried"
                ),
            }
        }
    }))
}

/// Reject a caller-supplied stack unless `EVENT_BUS` selects the in-process bus.
/// Every `arc_stack::es_stack` builder wires an in-process bus, so under another bus
/// the stack's events would never reach it and durable handlers would never run.
pub(crate) fn ensure_supplied_stack_event_bus(aggregate_type: &str) -> io::Result<()> {
    if event_bus_mode() == EventBusMode::InProcess {
        return Ok(());
    }
    Err(io::Error::new(
        io::ErrorKind::InvalidInput,
        format!(
            "aggregate {aggregate_type} is registered with register_aggregate_with_stack, \
             which supports only the in-process event bus; unset EVENT_BUS or set \
             EVENT_BUS=inprocess (EVENT_BUS=nats requires register_aggregate::<A>())"
        ),
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
    // A non-secret process marker lets test/operations clients prove they
    // reached the instance they launched, rather than an old listener.
    let instance_id = env::var("ARC_INSTANCE_ID")
        .ok()
        .map(|value| {
            actix_web::http::header::HeaderValue::from_str(&value).map_err(|error| {
                io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!("invalid ARC_INSTANCE_ID response header: {error}"),
                )
            })
        })
        .transpose()?;
    crate::check_database_health();

    let secret_key = Key::from(
        env::var("SECRET_KEY")
            .expect("SECRET_KEY must be set")
            .as_bytes(),
    );

    let app_env = env::var("APP_ENV").unwrap_or_else(|_| "development".to_string());
    let is_production = app_env == "production";

    let browser_session_ttl = crate::helpers::session::browser_session_ttl_seconds()?;
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

    // Check every supplied stack before any registration builds or connects to
    // NATS, so the error does not depend on registration order.
    for registration in &registrations {
        if registration.supplied_stack {
            ensure_supplied_stack_event_bus(registration.aggregate_type)?;
        }
    }
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
        let response_defaults = match &instance_id {
            Some(value) => DefaultHeaders::new().add(("X-Arc-Instance-Id", value.clone())),
            None => DefaultHeaders::new(),
        };
        let mut session_middleware =
            SessionMiddleware::builder(CookieSessionStore::default(), secret_key.clone())
                .cookie_name("arc_session".to_string())
                .cookie_http_only(true)
                .cookie_same_site(same_site)
                .session_lifecycle(
                    PersistentSession::default()
                        .session_ttl(Duration::seconds(i64::from(browser_session_ttl))),
                );

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
            .wrap(response_defaults)
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

async fn build_event_bus(mode: &EventBusMode) -> io::Result<Box<dyn EventBus>> {
    match mode {
        EventBusMode::InProcess => Ok(Box::new(InProcessEventBus::new())),
        EventBusMode::Nats => build_nats_event_bus().await,
    }
}

#[cfg(feature = "nats")]
async fn build_nats_event_bus() -> io::Result<Box<dyn EventBus>> {
    let nats_url = env::var("NATS_URL").unwrap_or_else(|_| "nats://127.0.0.1:4222".to_string());
    let nats_stream = env::var("NATS_STREAM").unwrap_or_else(|_| "EVENTS".to_string());
    let async_bus = Arc::new(NatsEventBus::new(&nats_url, &nats_stream).await.map_err(
        |error| io::Error::other(format!("failed to initialize NATS event bus: {error}")),
    )?);

    info!(
        nats_url = nats_url,
        nats_stream = nats_stream,
        "Using NATS JetStream event bus"
    );

    Ok(Box::new(TwoLaneEventBus::with_async_bus(async_bus)))
}

#[cfg(not(feature = "nats"))]
async fn build_nats_event_bus() -> io::Result<Box<dyn EventBus>> {
    Err(io::Error::new(
        io::ErrorKind::InvalidInput,
        "EVENT_BUS=nats requires building with the nats cargo feature",
    ))
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

    #[derive(Clone)]
    struct RecoveryStore {
        inner: InMemoryEventStore,
        pending: Arc<tokio::sync::Mutex<Vec<Event>>>,
    }

    #[async_trait]
    impl EventStore for RecoveryStore {
        async fn append(
            &self,
            id: &str,
            version: arc_core::event_store::VersionCheck,
            events: Vec<Event>,
        ) -> arc_core::event_store::EventStoreResult<()> {
            self.inner.append(id, version, events).await
        }
        async fn load(&self, id: &str) -> arc_core::event_store::EventStoreResult<Vec<Event>> {
            self.inner.load(id).await
        }
        async fn load_from(
            &self,
            id: &str,
            from: i64,
        ) -> arc_core::event_store::EventStoreResult<Vec<Event>> {
            self.inner.load_from(id, from).await
        }
        async fn stream_all(
            &self,
            from: i64,
        ) -> arc_core::event_store::EventStoreResult<Vec<Event>> {
            self.inner.stream_all(from).await
        }
        async fn get_version(&self, id: &str) -> arc_core::event_store::EventStoreResult<i64> {
            self.inner.get_version(id).await
        }
        async fn pending_publications(
            &self,
            aggregate_type: &str,
            limit: usize,
        ) -> arc_core::event_store::EventStoreResult<Vec<Event>> {
            Ok(self
                .pending
                .lock()
                .await
                .iter()
                .filter(|e| e.aggregate_type == aggregate_type)
                .take(limit)
                .cloned()
                .collect())
        }
        async fn acknowledge_publication(
            &self,
            id: uuid::Uuid,
        ) -> arc_core::event_store::EventStoreResult<()> {
            self.pending
                .lock()
                .await
                .retain(|event| event.event_id != id);
            Ok(())
        }
    }

    struct RecoveringBus {
        attempts: Arc<std::sync::atomic::AtomicUsize>,
    }

    #[async_trait]
    impl EventBus for RecoveringBus {
        async fn publish(&self, _: Vec<Event>) -> arc_core::event_bus::EventBusResult<()> {
            if self
                .attempts
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst)
                == 0
            {
                Err(arc_core::event_bus::EventBusError::other(
                    "broker unavailable",
                ))
            } else {
                Ok(())
            }
        }
        async fn subscribe(
            &mut self,
            _: Box<dyn arc_core::event_bus::EventHandler>,
        ) -> arc_core::event_bus::EventBusResult<()> {
            Ok(())
        }
    }

    #[tokio::test]
    async fn runtime_relay_recovers_without_new_commands_and_stops_on_drop() {
        let event = Event::new(NewEvent {
            aggregate_type: "Product",
            aggregate_id: "recovery",
            sequence: 1,
            event_type: "ProductCreated",
            payload: json!({}),
        });
        let store = RecoveryStore {
            inner: InMemoryEventStore::new(),
            pending: Arc::new(tokio::sync::Mutex::new(vec![event])),
        };
        let attempts = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let command_bus = Arc::new(CommandBus::<ProductAggregate>::new(
            Box::new(store.clone()),
            Box::new(RecoveringBus {
                attempts: attempts.clone(),
            }),
        ));
        let weak_bus = Arc::downgrade(&command_bus);
        let relay = start_publication_relay(command_bus.clone());
        drop(command_bus);
        tokio::time::timeout(std::time::Duration::from_secs(4), async {
            while !store.pending.lock().await.is_empty() {
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("runtime must retry queued work without a new dispatch");
        assert_eq!(attempts.load(std::sync::atomic::Ordering::SeqCst), 2);
        drop(relay);
        tokio::time::timeout(std::time::Duration::from_secs(1), async {
            while weak_bus.upgrade().is_some() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("dropping runtime must release its relay task");
    }

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
        BuiltAggregateRuntime::new(Arc::new(command_bus), read_model_store, engine)
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
