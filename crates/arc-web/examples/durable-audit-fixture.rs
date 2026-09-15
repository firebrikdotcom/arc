//! Synthetic browser verification server. Not a deployable application.
use actix_web::{web, App, HttpRequest, HttpResponse, HttpServer};
use arc_core::access_log::{AccessLogger, AccessedResource, PurposeOfUse, Sensitivity};
use arc_web::helpers::{
    access_log::{record_read, Sensitive},
    access_log_config::build_access_logger,
};
use diesel::{connection::SimpleConnection, prelude::*};
use std::{path::PathBuf, sync::Mutex};

struct Fixture {
    path: PathBuf,
    lock: Mutex<Option<SqliteConnection>>,
}
#[derive(QueryableByName)]
struct Row {
    #[diesel(sql_type = diesel::sql_types::Text)]
    entry_json: String,
}
#[actix_web::main]
async fn main() -> std::io::Result<()> {
    let logger = build_access_logger(|key| std::env::var(key).ok()).await?;
    let fixture = web::Data::new(Fixture {
        path: std::env::var("ACCESS_LOG_SQLITE_PATH").unwrap().into(),
        lock: Mutex::new(None),
    });
    let boot = uuid::Uuid::new_v4().to_string();
    HttpServer::new(move || {
        App::new()
            .app_data(web::Data::from(logger.clone()))
            .app_data(fixture.clone())
            .app_data(web::Data::new(boot.clone()))
            .route(
                "/health",
                web::get().to(|boot: web::Data<String>| async move {
                    HttpResponse::Ok().body(format!("durable-audit-fixture:{}", boot.get_ref()))
                }),
            )
            .route(
                "/read/{classification}",
                web::get().to(
                    |req: HttpRequest, logger: web::Data<dyn AccessLogger>| async move {
                        let sensitivity = if req.match_info().get("classification") == Some("pii") {
                            Sensitivity::Pii
                        } else {
                            Sensitivity::Phi
                        };
                        record_read(
                            logger.get_ref(),
                            &req,
                            "synthetic-browser-user",
                            AccessedResource::new("SyntheticRecord", "opaque-1", sensitivity)
                                .with_fields(["display"]),
                            PurposeOfUse::UserInitiated,
                            Sensitive::new("SYNTHETIC-PRIVATE-RESPONSE", sensitivity),
                        )
                        .await
                    },
                ),
            )
            .route(
                "/fixture/entries",
                web::get().to(|f: web::Data<Fixture>| async move {
                    let mut conn = SqliteConnection::establish(f.path.to_str().unwrap()).unwrap();
                    let rows = diesel::sql_query(
                        "SELECT entry_json FROM access_log ORDER BY timestamp_utc_us",
                    )
                    .load::<Row>(&mut conn)
                    .unwrap();
                    HttpResponse::Ok().json(
                        rows.into_iter()
                            .map(|r| {
                                serde_json::from_str::<serde_json::Value>(&r.entry_json).unwrap()
                            })
                            .collect::<Vec<_>>(),
                    )
                }),
            )
            .route(
                "/fixture/lock",
                web::post().to(|f: web::Data<Fixture>| async move {
                    let mut conn = SqliteConnection::establish(f.path.to_str().unwrap()).unwrap();
                    conn.batch_execute("BEGIN EXCLUSIVE").unwrap();
                    *f.lock.lock().unwrap() = Some(conn);
                    HttpResponse::NoContent().finish()
                }),
            )
            .route(
                "/fixture/unlock",
                web::post().to(|f: web::Data<Fixture>| async move {
                    f.lock.lock().unwrap().take();
                    HttpResponse::NoContent().finish()
                }),
            )
    })
    .workers(2)
    .bind("0.0.0.0:18784")?
    .run()
    .await
}
