use crate::http::controllers::api_controller::{
    delete_profile, login, logout, profile, register, update_profile,
};
use crate::http::controllers::diag_controller::{diag_health, list_events};
use crate::http::controllers::internal_projection_controller::handle_user_projection;
use crate::http::controllers::{admin_controller, auth_controller, home_controller};
use crate::http::middlewares::{
    auth_middleware::AuthMiddleware, idle_timeout_middleware::IdleTimeoutMiddleware,
    jwt_middleware::JwtMiddleware,
};
use actix_files as fs;
use actix_web::http::header::{CACHE_CONTROL, ETAG, IF_NONE_MATCH};
use actix_web::{get, web, Error, HttpRequest, HttpResponse, Responder};

/// Serves static files from the `dist/` directory with ETag-based conditional
/// requests and tiered Cache-Control headers (immutable for hashed assets).
#[get("/public/{filename:.*}")]
pub async fn static_file(req: HttpRequest) -> Result<HttpResponse, Error> {
    let path: std::path::PathBuf = req.match_info().query("filename").parse().unwrap();
    let filename = path.to_string_lossy();

    // Check if file has a hash in the name (e.g., script-1MSs88XQ.js)
    // These are immutable and can be cached forever
    let is_hashed =
        filename.contains('-') && (filename.ends_with(".js") || filename.ends_with(".css"));

    let file = fs::NamedFile::open(std::path::Path::new("./dist").join(path.clone()))?;

    // Generate ETag from file metadata
    let metadata = file.file().metadata()?;
    let etag_value = format!(
        "{:x}-{:x}",
        metadata.len(),
        metadata
            .modified()
            .map(|t| t
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs())
            .unwrap_or(0)
    );

    // Check If-None-Match header for conditional requests
    if let Some(if_none_match) = req.headers().get(IF_NONE_MATCH) {
        if let Ok(header_value) = if_none_match.to_str() {
            if header_value.trim_matches('"') == etag_value
                || header_value == format!("\"{}\"", etag_value)
            {
                return Ok(HttpResponse::NotModified().finish());
            }
        }
    }

    let mut response = file.into_response(&req);
    let headers = response.headers_mut();

    // Add ETag header
    headers.insert(ETAG, format!("\"{}\"", etag_value).parse().unwrap());

    // Add Cache-Control header
    if is_hashed {
        // Hashed files are immutable - cache for 1 year
        headers.insert(
            CACHE_CONTROL,
            "public, max-age=31536000, immutable".parse().unwrap(),
        );
    } else {
        // Other static files - cache for 1 hour, revalidate
        headers.insert(
            CACHE_CONTROL,
            "public, max-age=3600, must-revalidate".parse().unwrap(),
        );
    }

    Ok(response)
}

/// Health check endpoint for monitoring and load balancers.
/// Returns 200 OK with JSON status when the application is running.
#[get("/health")]
pub async fn health() -> impl Responder {
    HttpResponse::Ok().json(serde_json::json!({
        "status": "healthy",
        "version": env!("CARGO_PKG_VERSION")
    }))
}

/// Registers all application routes: health check, auth, admin, API (v1 + legacy), and static files.
///
/// `/ws` is deliberately not mounted. No page subscribes to a room and nothing
/// here broadcasts, and the browser login keeps only a cached `SessionUser`
/// with no server-side handle that a `WebSocketPrincipalProvider` could verify.
/// An app that needs sockets wires them as described in docs/websocket-security.md.
pub fn config(cfg: &mut web::ServiceConfig) {
    cfg
        // Health check
        .service(health)
        // GET /home
        .service(home_controller::home)
        // GET /signin
        .service(auth_controller::signin)
        // POST /signin
        .service(auth_controller::signin_post)
        // GET /signout
        .service(auth_controller::signout)
        // API routes v1 (JWT protected)
        .service(
            web::scope("/api/v1")
                .service(login)
                .service(register)
                .service(
                    web::scope("/protected")
                        .wrap(JwtMiddleware)
                        .service(profile)
                        .service(update_profile)
                        .service(delete_profile)
                        .service(logout),
                ),
        )
        // Backwards-compatible API routes (will be deprecated)
        .service(
            web::scope("/api").service(login).service(
                web::scope("/protected")
                    .wrap(JwtMiddleware)
                    .service(profile),
            ),
        )
        // Internal Benthos delivery endpoints. These require their own bearer
        // token and keep projection database writes inside Arc-owned code.
        .service(web::scope("/internal/projections").service(handle_user_projection))
        // GET /admin
        // AuthMiddleware is innermost (runs last, closest to handler) so the
        // idle-timeout enforcer sees the session-bound user_id and can purge.
        .service(
            web::scope("/admin")
                .wrap(AuthMiddleware)
                .wrap(IdleTimeoutMiddleware::from_env())
                .service(admin_controller::dashboard)
                .service(admin_controller::settings)
                .service(admin_controller::profile)
                .service(admin_controller::profile_post)
                .service(admin_controller::profile_password_post),
        )
        .service(static_file);

    // Diagnostic endpoints — only mounted when APP_ENV=e2e so production
    // builds never expose them. Backend-agnostic: routes through EventStore.
    if std::env::var("APP_ENV").as_deref() == Ok("e2e") {
        cfg.service(
            web::scope("/__diag__")
                .service(diag_health)
                .service(list_events),
        );
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;
    use actix_web::{http, test, App};

    #[actix_web::test]
    async fn test_static_file_ok() {
        let app = test::init_service(App::new().service(static_file)).await;

        fs::create_dir_all("./dist").unwrap();
        fs::write("./dist/styles.css", "").unwrap();

        let req = test::TestRequest::get()
            .uri("/public/styles.css")
            .to_request();
        let resp = test::call_service(&app, req).await;

        fs::remove_file("./dist/styles.css").unwrap();

        assert_eq!(resp.status(), http::StatusCode::OK);
    }

    #[actix_web::test]
    async fn websocket_endpoint_is_not_mounted() {
        let app = test::init_service(App::new().configure(config)).await;

        let req = test::TestRequest::get()
            .uri("/ws")
            .insert_header((http::header::ORIGIN, "http://127.0.0.1:18080"))
            .insert_header((http::header::CONNECTION, "Upgrade"))
            .insert_header((http::header::UPGRADE, "websocket"))
            .insert_header((http::header::SEC_WEBSOCKET_VERSION, "13"))
            .insert_header((http::header::SEC_WEBSOCKET_KEY, "dGhlIHNhbXBsZSBub25jZQ=="))
            .to_request();
        let resp = test::call_service(&app, req).await;
        assert_eq!(resp.status(), http::StatusCode::NOT_FOUND);
    }

    #[actix_web::test]
    async fn admin_page_does_not_open_a_websocket() {
        let page = include_str!("resources/views/admin/index.html");
        assert!(!page.contains("websocket"));
        let script = include_str!("resources/js/script.js");
        assert!(!script.contains("websocket"));
    }

    #[actix_web::test]
    async fn test_static_file_not_found() {
        let app = test::init_service(App::new().service(static_file)).await;

        let req = test::TestRequest::get()
            .uri("/public/not-existing-styles.css")
            .to_request();
        let resp = test::call_service(&app, req).await;
        assert_eq!(resp.status(), http::StatusCode::NOT_FOUND);
    }
}
