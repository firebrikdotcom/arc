use actix_web::{test, web, App, HttpRequest};
use arc_core::access_log::{AccessLogger, AccessedResource, PurposeOfUse, Sensitivity};
use arc_es_sqlite::{AccessLogPrivacy, SqliteAccessLogger};
use arc_web::helpers::access_log::{record_read, Sensitive};
use diesel::{connection::SimpleConnection, Connection, SqliteConnection};
use std::sync::Arc;
use uuid::Uuid;

#[actix_web::test]
async fn durable_sink_failure_never_releases_phi_and_recovers() {
    let path = std::env::temp_dir().join(format!("arc-nineties-http-audit-{}.db", Uuid::new_v4()));
    struct Cleanup(std::path::PathBuf);
    impl Drop for Cleanup {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
        }
    }
    let _cleanup = Cleanup(path.clone());
    let logger: Arc<dyn AccessLogger> = Arc::new(
        SqliteAccessLogger::open(path.clone(), AccessLogPrivacy::default(), 25)
            .await
            .unwrap(),
    );
    let app = test::init_service(App::new().app_data(web::Data::from(logger)).route(
        "/",
        web::get().to(
            |req: HttpRequest, logger: web::Data<dyn AccessLogger>| async move {
                record_read(
                    logger.get_ref(),
                    &req,
                    "synthetic-user",
                    AccessedResource::new("Record", "1", Sensitivity::Phi),
                    PurposeOfUse::Treatment,
                    Sensitive::new("PRIVATE-RESPONSE-VALUE", Sensitivity::Phi),
                )
                .await
            },
        ),
    ))
    .await;
    let mut conn = SqliteConnection::establish(path.to_str().unwrap()).unwrap();
    for fail in [false, true, false] {
        if fail {
            conn.batch_execute("BEGIN EXCLUSIVE").unwrap();
        }
        let response =
            test::call_service(&app, test::TestRequest::get().uri("/").to_request()).await;
        assert_eq!(response.status().as_u16(), if fail { 503 } else { 200 });
        let body = String::from_utf8(test::read_body(response).await.to_vec()).unwrap();
        assert_eq!(body.contains("PRIVATE-RESPONSE-VALUE"), !fail);
        if fail {
            conn.batch_execute("ROLLBACK").unwrap();
        }
    }
    // An unavailable disk path must produce the same fail-closed wire response.
    drop(conn);
    let saved = path.with_extension("saved");
    std::fs::rename(&path, &saved).unwrap();
    std::fs::create_dir(&path).unwrap();
    let response = test::call_service(&app, test::TestRequest::get().uri("/").to_request()).await;
    let status = response.status();
    let body = test::read_body(response).await;
    std::fs::remove_dir(&path).unwrap();
    std::fs::rename(saved, &path).unwrap();
    assert_eq!(status.as_u16(), 503);
    assert!(!String::from_utf8_lossy(&body).contains("PRIVATE-RESPONSE-VALUE"));
}
