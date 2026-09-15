//! TEST ONLY. Deliberately exposes fixture controls; never deploy this example.
use actix::{Actor, Addr};
use actix_session::{storage::CookieSessionStore, Session, SessionMiddleware};
use actix_web::{cookie::Key, web, App, HttpResponse, HttpServer};
use arc_auth_core::IdentityStore;
use arc_auth_db::{DbIdentityPlugin, DbIdentityStore};
use arc_auth_rbac::RequireRoles;
use arc_auth_session::{authenticate, sign_out, RequireSession};
use arc_core::session::{InMemorySessionStore, SessionRecord, SessionStore};
use arc_web::{
    helpers::jwt::create_token,
    http::middlewares::{
        idle_timeout_middleware::IdleTimeoutMiddleware, jwt_middleware::JwtMiddleware,
    },
    websocket::{
        connection::ws_handler,
        server::{BroadcastToRoom, BroadcastToUser, WsServer},
    },
};
use arc_web::{ArcPlugin, PluginSetupContext};
use std::sync::Arc;

#[actix_web::main]
async fn main() -> std::io::Result<()> {
    assert_eq!(
        std::env::var("ARC_SECURITY_FIXTURE").as_deref(),
        Ok("1"),
        "test fixture requires explicit opt-in"
    );
    std::env::set_var("JWT_SECRET", "fixture-only-secret-at-least-32-bytes");
    let path = format!("/tmp/arc-nineties-security-{}.db", uuid::Uuid::new_v4());
    std::env::set_var("DATABASE_DRIVER", "sqlite");
    std::env::set_var("ARC_SETUP_ADMIN_NAME", "fixture-admin");
    std::env::set_var("ARC_SETUP_ADMIN_EMAIL", "fixture-admin@example.test");
    std::env::set_var("ARC_SETUP_ADMIN_PASSWORD", "fixture-only-password");
    DbIdentityPlugin::new(&path)
        .setup(&PluginSetupContext {
            database_url: &path,
            project_root: std::path::Path::new("/tmp"),
        })
        .await?;
    let store = DbIdentityStore::new(&path);
    for name in ["alice", "bob"] {
        store
            .create_user(
                name,
                &format!("{name}@example.test"),
                "test-only-password",
                &["admin".into()],
            )
            .await
            .unwrap();
    }
    let outage_path = path.clone();
    let identities: Arc<dyn IdentityStore> = Arc::new(store);
    let sessions: Arc<dyn SessionStore> = Arc::new(InMemorySessionStore::new());
    let sockets = WsServer::new().start();
    let key = Key::generate();
    let server = HttpServer::new(move || App::new()
        .app_data(web::Data::new(outage_path.clone()))
        .app_data(web::Data::from(identities.clone()))
        .app_data(web::Data::from(sessions.clone()))
        .app_data(web::Data::new(sockets.clone()))
        .wrap(SessionMiddleware::builder(CookieSessionStore::default(), key.clone()).cookie_secure(false).cookie_http_only(true).build())
        .route("/signin", web::get().to(|| async { HttpResponse::Ok().content_type("text/html").body("<main>Sign in required</main>") }))
        .configure(arc_auth_admin::routes)
        .route("/fixture/csrf", web::get().to(|session: Session| async move { HttpResponse::Ok().body(arc_web::helpers::csrf::get_csrf_token(&session)) }))
        .route("/health", web::get().to(|| async { HttpResponse::Ok().finish() }))
        .route("/", web::get().to(|| async { HttpResponse::Ok().content_type("text/html").body("<main>Security test fixture</main>") }))
        .route("/fixture/login/{name}", web::get().to(|name: web::Path<String>, session: Session, store: web::Data<dyn IdentityStore>, sessions: web::Data<dyn SessionStore>| async move {
            let user = authenticate(&session, store.get_ref(), &format!("{}@example.test", name.as_str()), "test-only-password").await.unwrap();
            let (token, jti) = create_token(&user.id).unwrap();
            sessions.record_session(SessionRecord { jti, actor_id: user.id.clone(), created_at_us: 1, expires_at_us: i64::MAX, revoked_at_us: None }).await.unwrap();
            HttpResponse::Ok().json(serde_json::json!({"id":user.id,"token":token}))
        }))
        .route("/fixture/change/{name}/{change}", web::post().to(|p: web::Path<(String,String)>, store: web::Data<dyn IdentityStore>| async move {
            let user = store.list().await.unwrap().into_iter().find(|u| u.name == p.0).unwrap();
            match p.1.as_str() {
                "remove-role" => { store.set_roles(&user.id, &[]).await.unwrap(); },
                "disable" => { store.set_active(&user.id, false).await.unwrap(); },
                "reset" => { store.set_roles(&user.id, &["admin".into()]).await.unwrap(); store.set_active(&user.id, true).await.unwrap(); },
                _ => panic!("unknown fixture operation"),
            }
            HttpResponse::NoContent().finish()
        }))
        .route("/fixture/logout", web::post().to(|session: Session, store: web::Data<dyn IdentityStore>| async move { if sign_out(&session, store.get_ref()).await.is_err() { return HttpResponse::ServiceUnavailable().finish(); } HttpResponse::NoContent().finish() }))
        .route("/fixture/outage/{state}", web::post().to(|state: web::Path<String>, path: web::Data<String>| async move {
            let offline = format!("{}.offline", path.get_ref());
            if state.as_str() == "on" { std::fs::rename(path.get_ref(), &offline).unwrap(); }
            else { std::fs::rename(&offline, path.get_ref()).unwrap(); }
            HttpResponse::NoContent().finish()
        }))
        .service(web::resource("/resources").wrap(RequireSession).wrap(IdleTimeoutMiddleware::new(900)).route(web::get().to(|| async { HttpResponse::Ok().body("resource-data") })))
        .route("/fixture/expire", web::post().to(|session: Session| async move { session.insert("last_active_at", 1u64).unwrap(); HttpResponse::NoContent().finish() }))
        .service(web::scope("/browser").wrap(RequireRoles::new(&["admin"])).wrap(RequireSession).wrap(IdleTimeoutMiddleware::new(900)).route("/admin", web::get().to(|| async { HttpResponse::Ok().body("admin-data") })))
        .service(web::scope("/api").wrap(RequireRoles::new(&["admin"])).wrap(JwtMiddleware).route("/admin", web::get().to(|| async { HttpResponse::Ok().body("admin-data") })))
        .route("/ws", web::get().to(ws_handler))
        .route("/fixture/user/{id}", web::post().to(|id: web::Path<String>, server: web::Data<Addr<WsServer>>| async move { server.send(BroadcastToUser { user_id: id.into_inner(), message: "private-user-message".into() }).await.unwrap(); HttpResponse::NoContent().finish() }))
        .route("/fixture/room/{room}", web::post().to(|room: web::Path<String>, server: web::Data<Addr<WsServer>>| async move { server.send(BroadcastToRoom { room: room.into_inner(), message: "private-room-message".into(), skip_id: None }).await.unwrap(); HttpResponse::NoContent().finish() }))
    ).workers(1).bind("0.0.0.0:18764")?.run();
    let result = server.await;
    let _ = std::fs::remove_file(path);
    result
}
