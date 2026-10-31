use dotenvy::dotenv;
use std::env;
use tracing::{error, info};
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

use arc_core::command_bus::SnapshotPolicy;
use arc_web::{
    check_app_health, check_database_health, validate_environment, ArcApp, ProjectorReg,
};

use domain::user::aggregate::UserAggregate;
use domain::user::projector::{UserProjector, USERS_VIEW};

mod routes;

mod http {
    pub mod controllers {
        pub mod admin_controller;
        pub mod api_controller;
        pub mod auth_controller;
        pub mod diag_controller;
        pub mod home_controller;
        pub mod internal_projection_controller;
    }
    // Framework-owned HTTP machinery, consumed by version.
    pub use arc_web::http::{errors, middlewares};

    #[cfg(test)]
    mod auth_middleware_test;
}

mod database {
    pub mod seeders {
        pub mod create_users;
    }
}

mod schema;

mod helpers {
    // Framework helpers, consumed by version from arc-web.
    pub use arc_web::helpers::{
        access_log, audit_context, config, csrf, es_stack, general, jwt, rate_limit, session,
        template,
    };
    // The application owns its migration set; pool machinery remains framework-owned.
    pub mod database;
    // Application-owned, aggregate-coupled test scaffolding.
    pub mod test;
}

mod services {
    pub mod user_service;
}

mod validation;

mod commands;
mod domain;

// Re-export framework items under `crate::` so application routes/controllers
// reference them by their familiar paths.
pub use arc_web::AppState;

/// The application's read-model projectors, registered against its aggregate.
pub(crate) fn user_projectors() -> Vec<ProjectorReg> {
    vec![ProjectorReg::new(UserProjector::new(), USERS_VIEW)]
}

/// The application's snapshot policy, if configured.
pub(crate) fn user_snapshot_policy() -> Option<SnapshotPolicy> {
    helpers::config::user_snapshot_interval_events().map(SnapshotPolicy::EveryNEvents)
}

#[actix_web::main]
async fn main() -> std::io::Result<()> {
    tracing_subscriber::registry()
        .with(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "arc=info,arc_web=info,actix_web=info".into()),
        )
        .with(tracing_subscriber::fmt::layer())
        .init();

    check_app_health();

    dotenv().ok();

    validate_environment();

    info!("Arc application starting");

    let args: Vec<String> = env::args().collect();
    let app_url: String = env::var("APP_URL").expect("APP_URL must be set");
    let app_port: u16 = env::var("APP_PORT")
        .unwrap_or_else(|_| "8080".to_string())
        .parse::<u16>()
        .expect("APP_PORT must be a valid u16");

    let mut command: &str = "serve";
    if args.len() > 1 {
        command = args[1].as_str();
    }

    match command {
        "serve" => {
            ArcApp::builder()
                .register_aggregate::<UserAggregate>()
                .register_projectors(user_projectors())
                .snapshot_policy(user_snapshot_policy())
                .register_routes(routes::config)
                .serve(app_url, app_port)
                .await
        }
        "develop" => {
            check_database_health();
            arc_web::commands::develop::run_development().await
        }
        "migrate" => commands::migrate::run(&args).await,
        "seed" => commands::seed::run().await,
        _ => {
            error!("Unknown command: {}", command);
            Ok(())
        }
    }
}
