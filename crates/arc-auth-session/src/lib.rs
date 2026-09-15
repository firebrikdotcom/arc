//! Cookie-session authentication protocol for Arc browser applications.
//! Browser pages are intentionally provided by `arc-auth-admin`.

use actix_session::{Session, SessionExt};
use actix_web::{
    body::EitherBody,
    dev::{forward_ready, Service, ServiceRequest, ServiceResponse, Transform},
    web, Error, HttpMessage, HttpResponse,
};
use arc_auth_core::{AuthError, Identity, IdentityStore};
use arc_web::{ArcAppBuilder, ArcPlugin};
use futures_util::future::LocalBoxFuture;
use std::{
    future::{ready, Ready},
    sync::Arc,
};

pub const BROWSER_SESSION_KEY: &str = "arc_auth_session_id";

pub const IDENTITY_SESSION_KEY: &str = "arc_auth_identity";

pub fn identity(session: &Session) -> Option<Identity> {
    session.get(IDENTITY_SESSION_KEY).ok().flatten()
}

pub fn cache_identity(session: &Session, identity: &Identity) {
    let _ = session.insert(IDENTITY_SESSION_KEY, identity);
    arc_web::helpers::session::set_session_user(
        session,
        &arc_web::helpers::session::SessionUser {
            id: identity.id.clone(),
            name: identity.name.clone(),
            email: identity.email.clone(),
        },
    );
}

pub async fn authenticate(
    session: &Session,
    store: &dyn IdentityStore,
    email: &str,
    password: &str,
) -> Result<Identity, AuthError> {
    let ttl = arc_web::helpers::session::browser_session_ttl_seconds()
        .map_err(|_| AuthError::InvalidInput("invalid browser session lifetime".into()))?;
    let (identity, id) = store
        .authenticate_browser(email, password, ttl)
        .await
        .inspect_err(|error| {
            if matches!(error, AuthError::Store(_)) {
                tracing::error!(
                    operation = "authenticate_browser",
                    "browser authentication store unavailable"
                );
            }
        })?;
    // Re-authentication rotates the server-side handle as well as the cookie.
    if let Some(previous) = session.get::<String>(BROWSER_SESSION_KEY).ok().flatten() {
        if let Err(error) = store.revoke_browser_session(&previous).await {
            tracing::error!(
                operation = "rotate_browser_session",
                "browser authentication store unavailable"
            );
            if store.revoke_browser_session(&id).await.is_err() {
                tracing::error!(
                    operation = "cleanup_browser_session",
                    "browser authentication store unavailable"
                );
            }
            return Err(error);
        }
    }
    session.clear();
    session.renew();
    session
        .insert(BROWSER_SESSION_KEY, id)
        .map_err(|e| AuthError::Store(e.to_string()))?;
    session
        .insert(
            "last_active_at",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs(),
        )
        .map_err(|e| AuthError::Store(e.to_string()))?;
    cache_identity(session, &identity);
    Ok(identity)
}

/// Revoke before clearing the cookie; a failed store write must not report logout success.
pub async fn sign_out(session: &Session, store: &dyn IdentityStore) -> Result<(), AuthError> {
    if let Some(id) = session.get::<String>(BROWSER_SESSION_KEY).ok().flatten() {
        store.revoke_browser_session(&id).await.inspect_err(|_| {
            tracing::error!(
                operation = "revoke_browser_session",
                "browser authentication store unavailable"
            );
        })?;
    }
    session.purge();
    Ok(())
}

/// Session plugin retained as the stable protocol/middleware registration seam.
pub struct SessionAuthPlugin;
#[async_trait::async_trait]
impl ArcPlugin for SessionAuthPlugin {
    fn name(&self) -> &'static str {
        "auth-session"
    }
    fn register(&self, builder: ArcAppBuilder) -> ArcAppBuilder {
        builder
    }
}

/// Redirect unauthenticated browser requests to the sign-in page.
pub struct RequireSession;
impl<S, B> Transform<S, ServiceRequest> for RequireSession
where
    S: Service<ServiceRequest, Response = ServiceResponse<B>, Error = Error> + 'static,
    S::Future: 'static,
    B: 'static,
{
    type Response = ServiceResponse<EitherBody<B>>;
    type Error = Error;
    type InitError = ();
    type Transform = SessionCheck<S>;
    type Future = Ready<Result<Self::Transform, ()>>;
    fn new_transform(&self, service: S) -> Self::Future {
        ready(Ok(SessionCheck {
            service: Arc::new(service),
        }))
    }
}
pub struct SessionCheck<S> {
    service: Arc<S>,
}
impl<S, B> Service<ServiceRequest> for SessionCheck<S>
where
    S: Service<ServiceRequest, Response = ServiceResponse<B>, Error = Error> + 'static,
    S::Future: 'static,
    B: 'static,
{
    type Response = ServiceResponse<EitherBody<B>>;
    type Error = Error;
    type Future = LocalBoxFuture<'static, Result<Self::Response, Error>>;
    forward_ready!(service);
    fn call(&self, req: ServiceRequest) -> Self::Future {
        let service = self.service.clone();
        Box::pin(async move {
            let session = req.get_session();
            let id = session.get::<String>(BROWSER_SESSION_KEY).ok().flatten();
            let current = match (id, req.app_data::<IdentityStoreData>()) {
                (Some(id), Some(store)) => match store.browser_identity(&id).await {
                    Ok(user) => user,
                    Err(_) => {
                        tracing::error!(
                            operation = "validate_browser_session",
                            "browser authentication store unavailable"
                        );
                        return Ok(req.into_response(
                            HttpResponse::ServiceUnavailable()
                                .body("Authentication is temporarily unavailable.")
                                .map_into_right_body(),
                        ));
                    }
                },
                (Some(_), None) => {
                    tracing::error!(
                        operation = "validate_browser_session",
                        "browser authentication store missing"
                    );
                    return Ok(req.into_response(
                        HttpResponse::ServiceUnavailable()
                            .body("Authentication is temporarily unavailable.")
                            .map_into_right_body(),
                    ));
                }
                _ => None,
            };
            if let Some(user) = current.filter(|user| user.active) {
                cache_identity(&session, &user);
                req.extensions_mut().insert(user);
                return service
                    .call(req)
                    .await
                    .map(ServiceResponse::map_into_left_body);
            }
            session.purge();
            Ok(req.into_response(
                HttpResponse::Found()
                    .insert_header(("Location", "/signin"))
                    .finish()
                    .map_into_right_body(),
            ))
        })
    }
}

/// Convenience extractor for handlers that need the configured identity store.
pub type IdentityStoreData = web::Data<dyn IdentityStore>;

#[cfg(test)]
mod tests {
    use super::*;
    use actix_session::{storage::CookieSessionStore, SessionMiddleware};
    use actix_web::{cookie::Key, test, App};

    #[actix_web::test]
    async fn cached_identity_cannot_authorize_without_a_validated_handle() {
        let app = test::init_service(
            App::new()
                .wrap(
                    SessionMiddleware::builder(CookieSessionStore::default(), Key::generate())
                        .cookie_secure(false)
                        .build(),
                )
                .route(
                    "/legacy",
                    web::get().to(|session: Session| async move {
                        cache_identity(
                            &session,
                            &Identity {
                                id: "1".into(),
                                name: "A".into(),
                                email: "a@example.test".into(),
                                active: true,
                                roles: vec!["admin".into()],
                            },
                        );
                        HttpResponse::Ok().finish()
                    }),
                )
                .route(
                    "/handle",
                    web::get().to(|session: Session| async move {
                        session
                            .insert(BROWSER_SESSION_KEY, "untrusted-handle")
                            .unwrap();
                        HttpResponse::Ok().finish()
                    }),
                )
                .service(
                    web::resource("/protected")
                        .wrap(RequireSession)
                        .route(web::get().to(|| async { HttpResponse::Ok().body("secret") })),
                ),
        )
        .await;
        for (path, status) in [("/legacy", 302), ("/handle", 503)] {
            let response =
                test::call_service(&app, test::TestRequest::get().uri(path).to_request()).await;
            let cookie = response.response().cookies().next().unwrap().into_owned();
            let response = test::call_service(
                &app,
                test::TestRequest::get()
                    .uri("/protected")
                    .cookie(cookie)
                    .to_request(),
            )
            .await;
            assert_eq!(response.status().as_u16(), status);
        }
    }
}
