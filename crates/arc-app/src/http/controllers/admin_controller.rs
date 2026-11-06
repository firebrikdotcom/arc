use crate::domain::user::aggregate::UserAggregate;
use crate::domain::user::commands::UserCommand;
use crate::helpers::audit_context;
use crate::helpers::csrf::{get_csrf_token, validate_csrf_token};
use crate::helpers::general::gravatar_url;
use crate::helpers::session::{get_session_user, set_session_user, SessionUser};
use crate::helpers::template::load_template;
use crate::services::user_service::{
    prepare_password, validate_user_credentials_es, UserValidationResult,
};
use crate::validation::user_validation::UpdateProfileForm;
use crate::AppState;
use actix_session::Session;
use actix_web::{get, post, web, HttpRequest, HttpResponse, Responder};
use arc_core::command_bus::CommandBus;
use arc_core::read_model_store::ReadModelStore;
use serde::{Deserialize, Serialize};
use validator::Validate;

/// Renders the admin dashboard page. Redirects to `/signin` if the session has expired.
#[get("")] // /admin - The Dashboard
pub async fn dashboard(data: web::Data<AppState>, session: Session) -> HttpResponse {
    let user: SessionUser = match get_session_user(&session) {
        Some(u) => u,
        None => {
            return HttpResponse::SeeOther()
                .insert_header(("Location", "/signin"))
                .finish()
        }
    };
    let app_name = &data.app_name.lock().unwrap();
    let user_avatar = gravatar_url(&user.email);

    HttpResponse::Ok()
        .content_type("text/html; charset=utf-8")
        .body(load_template(
            "admin/pages/dashboard.html",
            vec![
                ("name", app_name),
                ("user_name", &user.name),
                ("user_avatar", &user_avatar),
            ],
            None,
        ))
}

/// Renders the admin settings page.
#[get("/settings")]
pub async fn settings(data: web::Data<AppState>, session: Session) -> impl Responder {
    let user: SessionUser = match get_session_user(&session) {
        Some(u) => u,
        None => {
            return HttpResponse::SeeOther()
                .insert_header(("Location", "/signin"))
                .finish()
        }
    };
    let app_name = &data.app_name.lock().unwrap();
    let user_avatar = gravatar_url(&user.email);

    HttpResponse::Ok()
        .content_type("text/html; charset=utf-8")
        .body(load_template(
            "admin/pages/settings.html",
            vec![
                ("name", app_name),
                ("user_name", &user.name),
                ("user_avatar", &user_avatar),
            ],
            None,
        ))
}

/// Renders the user profile edit page with the current user's data and a CSRF token.
#[get("/profile")]
pub async fn profile(data: web::Data<AppState>, session: Session) -> impl Responder {
    let user: SessionUser = match get_session_user(&session) {
        Some(u) => u,
        None => {
            return HttpResponse::SeeOther()
                .insert_header(("Location", "/signin"))
                .finish()
        }
    };
    let app_name = &data.app_name.lock().unwrap();
    let user_avatar = gravatar_url(&user.email);
    let csrf_token = get_csrf_token(&session);

    HttpResponse::Ok()
        .content_type("text/html; charset=utf-8")
        .body(load_template(
            "admin/pages/profile.html",
            vec![
                ("name", app_name),
                ("user_name", &user.name),
                ("user_email", &user.email),
                ("user_avatar", &user_avatar),
                ("csrf_token", &csrf_token),
            ],
            None,
        ))
}

#[derive(Serialize, Deserialize, Debug)]
pub struct UserForm {
    csrf_token: String,
    name: String,
    email: String,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct PasswordForm {
    csrf_token: String,
    old_password: String,
    new_password: String,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct UserResponseData {
    name: String,
    email: String,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct ProfileResponse {
    data: UserResponseData,
}

/// Handles profile update form submission. Routes through the `CommandBus`:
/// emits `ProfileUpdated` and/or `EmailChanged` events as needed. Refreshes
/// the cached `SessionUser` from the projection so subsequent requests see
/// the new values without re-signing-in.
#[post("/profile")]
pub async fn profile_post(
    req: HttpRequest,
    form: web::Form<UserForm>,
    session: Session,
    command_bus: web::Data<CommandBus<UserAggregate>>,
    read_model_store: web::Data<dyn ReadModelStore>,
) -> impl Responder {
    // Keep the token valid for both forms and validation retries on this page.
    if !validate_csrf_token(&session, &form.csrf_token) {
        return HttpResponse::Forbidden()
            .json(serde_json::json!({"errors": {"csrf": "Invalid request. Please refresh and try again."}}));
    }

    let user: SessionUser =
        match get_session_user(&session) {
            Some(u) => u,
            None => return HttpResponse::Unauthorized().json(
                serde_json::json!({"errors": {"auth": "Session expired. Please sign in again."}}),
            ),
        };

    let profile_form = UpdateProfileForm {
        name: form.name.clone(),
        email: form.email.clone(),
    };
    if let Err(errors) = profile_form.validate() {
        return HttpResponse::BadRequest().json(serde_json::json!({"errors": errors}));
    }

    let new_name = form.name.clone();
    let new_email = form.email.clone();

    if new_name != user.name {
        let cmd = UserCommand::UpdateProfile {
            id: user.id.clone(),
            name: new_name.clone(),
        };
        if let Err(e) = command_bus
            .dispatch(cmd, audit_context::for_actor(&req, user.id.clone()))
            .await
        {
            tracing::error!(error = ?e, "UpdateProfile dispatch failed");
            return HttpResponse::InternalServerError()
                .json(serde_json::json!({"errors": {"server_error": "Failed to update user"}}));
        }
    }

    if new_email != user.email {
        let cmd = UserCommand::ChangeEmail {
            id: user.id.clone(),
            email: new_email.clone(),
        };
        if let Err(e) = command_bus
            .dispatch(cmd, audit_context::for_actor(&req, user.id.clone()))
            .await
        {
            tracing::error!(error = ?e, "ChangeEmail dispatch failed");
            return HttpResponse::InternalServerError()
                .json(serde_json::json!({"errors": {"server_error": "Failed to update user"}}));
        }
    }

    if let Some(refreshed) = SessionUser::from_projection(
        read_model_store.as_ref(),
        crate::domain::user::projector::USERS_VIEW,
        &user.id,
    )
    .await
    {
        set_session_user(&session, &refreshed);
    }

    HttpResponse::Ok().json(ProfileResponse {
        data: UserResponseData {
            name: new_name,
            email: new_email,
        },
    })
}

/// Handles password change form submission. Validates CSRF, verifies the
/// old password against the projection, dispatches `ChangePassword`.
#[post("/profile-password")]
pub async fn profile_password_post(
    req: HttpRequest,
    form: web::Form<PasswordForm>,
    session: Session,
    command_bus: web::Data<CommandBus<UserAggregate>>,
    read_model_store: web::Data<dyn ReadModelStore>,
) -> impl Responder {
    // Keep the token valid for both forms and validation retries on this page.
    if !validate_csrf_token(&session, &form.csrf_token) {
        return HttpResponse::Forbidden()
            .json(serde_json::json!({"errors": {"csrf": "Invalid request. Please refresh and try again."}}));
    }

    let user: SessionUser =
        match get_session_user(&session) {
            Some(u) => u,
            None => return HttpResponse::Unauthorized().json(
                serde_json::json!({"errors": {"auth": "Session expired. Please sign in again."}}),
            ),
        };

    let (validation, agg_id) =
        validate_user_credentials_es(read_model_store.as_ref(), &user.email, &form.old_password)
            .await;

    if validation != UserValidationResult::Valid || agg_id.as_deref() != Some(user.id.as_str()) {
        return HttpResponse::BadRequest()
            .json(serde_json::json!({"errors": {"server_error": "Invalid credentials"}}));
    }

    let new_hash = prepare_password(&form.new_password);
    let cmd = UserCommand::ChangePassword {
        id: user.id.clone(),
        password_hash: new_hash,
    };

    if let Err(e) = command_bus
        .dispatch(cmd, audit_context::for_actor(&req, user.id.clone()))
        .await
    {
        tracing::error!(error = ?e, "ChangePassword dispatch failed");
        return HttpResponse::InternalServerError()
            .json(serde_json::json!({"errors": {"server_error": "Failed to update password"}}));
    }

    HttpResponse::Ok().json(serde_json::json!({"success": "Password updated"}))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::helpers::test::{es::build_stack_with_default_user, InMemoryTestGuard};
    use crate::http::controllers::auth_controller;
    use crate::http::middlewares::auth_middleware::AuthMiddleware;
    use actix_session::storage::CookieSessionStore;
    use actix_session::SessionMiddleware;
    use actix_web::cookie::{Cookie, Key};
    use actix_web::{http, test, App};
    use serial_test::serial;
    use std::env;
    use std::sync::Mutex;

    fn extract_csrf_token(body_str: &str) -> String {
        body_str
            .split("name=\"csrf_token\" value=\"")
            .nth(1)
            .unwrap()
            .split('"')
            .next()
            .unwrap()
            .to_string()
    }

    fn rate_limiter() -> crate::helpers::rate_limit::LoginRateLimiter {
        crate::helpers::rate_limit::LoginRateLimiter(crate::helpers::rate_limit::RateLimiter::new(
            100,
            std::time::Duration::from_secs(60),
        ))
    }

    macro_rules! build_app {
        ($command_bus:expr, $rm:expr, $secret_key:expr) => {{
            test::init_service(
                App::new()
                    .app_data(web::Data::new(rate_limiter()))
                    .app_data(web::Data::new(AppState {
                        app_name: Mutex::from(env::var("APP_NAME").unwrap_or_default()),
                    }))
                    .app_data($command_bus.clone())
                    .app_data($rm.clone())
                    .wrap(SessionMiddleware::new(
                        CookieSessionStore::default(),
                        $secret_key.clone(),
                    ))
                    .service(auth_controller::signin)
                    .service(auth_controller::signin_post)
                    .service(
                        web::scope("/admin")
                            .service(super::dashboard)
                            .service(super::settings)
                            .service(super::profile)
                            .service(super::profile_post)
                            .service(super::profile_password_post)
                            .wrap(AuthMiddleware),
                    ),
            )
            .await
        }};
    }

    /// Macro: run signin_post end-to-end and bind a cookie variable.
    /// Mirrors what a real browser would have at the start of an admin session.
    macro_rules! login {
        ($app:expr, $email:expr, $password:expr) => {{
            let req_signin = test::TestRequest::get().uri("/signin").to_request();
            let resp = test::call_service(&$app, req_signin).await;
            let cookie =
                Cookie::parse_encoded(resp.headers().get("set-cookie").unwrap().to_str().unwrap())
                    .unwrap()
                    .into_owned();
            let body = test::read_body(resp).await;
            let body_str = String::from_utf8(body.to_vec()).unwrap();
            let csrf_token = extract_csrf_token(&body_str);

            let req_login = test::TestRequest::post()
                .uri("/signin")
                .cookie(cookie)
                .set_form([
                    ("csrf_token", csrf_token.as_str()),
                    ("email", $email),
                    ("password", $password),
                ])
                .to_request();
            let resp_login = test::call_service(&$app, req_login).await;
            Cookie::parse_encoded(
                resp_login
                    .headers()
                    .get("set-cookie")
                    .unwrap()
                    .to_str()
                    .unwrap(),
            )
            .unwrap()
            .into_owned()
        }};
    }

    #[serial]
    #[actix_web::test]
    async fn test_dashboard() {
        let _guard = InMemoryTestGuard;
        let stack = build_stack_with_default_user().await;
        let secret_key = Key::from(env::var("SECRET_KEY").unwrap().as_bytes());
        let app = build_app!(stack.command_bus, stack.read_model_store, secret_key);

        // Unauthenticated → redirect.
        let req = test::TestRequest::get().uri("/admin").to_request();
        let resp = test::call_service(&app, req).await;
        assert_eq!(resp.status(), http::StatusCode::FOUND);

        let cookie = login!(app, "jekyll@example.com", "password");
        let req = test::TestRequest::get()
            .cookie(cookie)
            .uri("/admin")
            .to_request();
        let resp = test::call_service(&app, req).await;
        assert_eq!(resp.status(), http::StatusCode::OK);
        assert_eq!(
            resp.headers().get("content-type").unwrap(),
            "text/html; charset=utf-8"
        );
    }

    #[serial]
    #[actix_web::test]
    async fn test_settings() {
        let _guard = InMemoryTestGuard;
        let stack = build_stack_with_default_user().await;
        let secret_key = Key::from(env::var("SECRET_KEY").unwrap().as_bytes());
        let app = build_app!(stack.command_bus, stack.read_model_store, secret_key);

        let cookie = login!(app, "jekyll@example.com", "password");
        let req = test::TestRequest::get()
            .cookie(cookie)
            .uri("/admin/settings")
            .to_request();
        let resp = test::call_service(&app, req).await;
        assert_eq!(resp.status(), http::StatusCode::OK);
        assert_eq!(
            resp.headers().get("content-type").unwrap(),
            "text/html; charset=utf-8"
        );
    }

    #[serial]
    #[actix_web::test]
    async fn test_profile_data() {
        let _guard = InMemoryTestGuard;
        let stack = build_stack_with_default_user().await;
        let secret_key = Key::from(env::var("SECRET_KEY").unwrap().as_bytes());
        let app = build_app!(stack.command_bus, stack.read_model_store, secret_key);

        let cookie = login!(app, "jekyll@example.com", "password");

        let req = test::TestRequest::get()
            .cookie(cookie.clone())
            .uri("/admin/profile")
            .to_request();
        let resp = test::call_service(&app, req).await;
        assert_eq!(resp.status(), http::StatusCode::OK);
        assert_eq!(
            resp.headers().get("content-type").unwrap(),
            "text/html; charset=utf-8"
        );
        let body = test::read_body(resp).await;
        let body_str = String::from_utf8(body.to_vec()).unwrap();
        let csrf_token = extract_csrf_token(&body_str);

        let new_email = "hyde@example.com";
        let req = test::TestRequest::post()
            .cookie(cookie)
            .uri("/admin/profile")
            .set_form([
                ("csrf_token", csrf_token.as_str()),
                ("name", "Hyde"),
                ("email", new_email),
            ])
            .to_request();
        let resp = test::call_service(&app, req).await;
        assert_eq!(resp.status(), http::StatusCode::OK);

        // Projection reflects the updated email.
        let hits = stack
            .read_model_store
            .as_ref()
            .find_by(
                crate::domain::user::projector::USERS_VIEW,
                "email",
                &serde_json::json!(new_email),
            )
            .await
            .unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0]["name"], "Hyde");
    }

    #[serial]
    #[actix_web::test]
    async fn test_profile_password() {
        let _guard = InMemoryTestGuard;
        let stack = build_stack_with_default_user().await;
        let secret_key = Key::from(env::var("SECRET_KEY").unwrap().as_bytes());
        let app = build_app!(stack.command_bus, stack.read_model_store, secret_key);

        let cookie = login!(app, "jekyll@example.com", "password");

        let req = test::TestRequest::get()
            .cookie(cookie.clone())
            .uri("/admin/profile")
            .to_request();
        let resp = test::call_service(&app, req).await;
        let body = test::read_body(resp).await;
        let body_str = String::from_utf8(body.to_vec()).unwrap();
        let csrf_token = extract_csrf_token(&body_str);

        let req = test::TestRequest::post()
            .cookie(cookie)
            .uri("/admin/profile-password")
            .set_form([
                ("csrf_token", csrf_token.as_str()),
                ("current_email", "jekyll@example.com"),
                ("old_password", "password"),
                ("new_password", "new-password"),
            ])
            .to_request();
        let resp = test::call_service(&app, req).await;
        assert_eq!(resp.status(), http::StatusCode::OK);

        let (validation, _id) = crate::services::user_service::validate_user_credentials_es(
            stack.read_model_store.as_ref(),
            "jekyll@example.com",
            "new-password",
        )
        .await;
        assert_eq!(
            validation,
            crate::services::user_service::UserValidationResult::Valid
        );
    }
    #[serial]
    #[actix_web::test]
    async fn profile_forms_reuse_csrf_and_password_confirmation_is_session_bound() {
        let _guard = InMemoryTestGuard;
        let stack = build_stack_with_default_user().await;
        crate::services::user_service::create_user(
            &stack.command_bus,
            stack.read_model_store.as_ref(),
            arc_core::command_bus::CommandContext::system(),
            "Other".into(),
            "other@example.com".into(),
            "other-password",
        )
        .await
        .unwrap();
        let secret_key = Key::from(env::var("SECRET_KEY").unwrap().as_bytes());
        let app = build_app!(stack.command_bus, stack.read_model_store, secret_key);
        let mut cookie = login!(app, "jekyll@example.com", "password");
        let resp = test::call_service(
            &app,
            test::TestRequest::get()
                .uri("/admin/profile")
                .cookie(cookie.clone())
                .to_request(),
        )
        .await;
        if let Some(value) = resp.headers().get("set-cookie") {
            cookie = Cookie::parse_encoded(value.to_str().unwrap())
                .unwrap()
                .into_owned();
        }
        let body = test::read_body(resp).await;
        let token = extract_csrf_token(std::str::from_utf8(&body).unwrap());

        // Model a browser: retain each response's newest session cookie. The
        // same page's two forms must survive validation failures and updates.
        for (csrf, name, email, expected) in [
            (
                "forged",
                "Jekyll",
                "jekyll@example.com",
                http::StatusCode::FORBIDDEN,
            ),
            (token.as_str(), "", "invalid", http::StatusCode::BAD_REQUEST),
            (
                token.as_str(),
                "Jekyll Updated",
                "updated@example.com",
                http::StatusCode::OK,
            ),
            (
                token.as_str(),
                "Jekyll Again",
                "updated@example.com",
                http::StatusCode::OK,
            ),
        ] {
            let resp = test::call_service(
                &app,
                test::TestRequest::post()
                    .uri("/admin/profile")
                    .cookie(cookie.clone())
                    .set_form([("csrf_token", csrf), ("name", name), ("email", email)])
                    .to_request(),
            )
            .await;
            assert_eq!(resp.status(), expected);
            if let Some(value) = resp.headers().get("set-cookie") {
                cookie = Cookie::parse_encoded(value.to_str().unwrap())
                    .unwrap()
                    .into_owned();
            }
        }
        // Another account's valid password must never authorize this session.
        // Then the old page's stale email must not block the real password.
        for (email, password, expected) in [
            (
                "other@example.com",
                "other-password",
                http::StatusCode::BAD_REQUEST,
            ),
            ("jekyll@example.com", "password", http::StatusCode::OK),
        ] {
            let resp = test::call_service(
                &app,
                test::TestRequest::post()
                    .uri("/admin/profile-password")
                    .cookie(cookie.clone())
                    .set_form([
                        ("csrf_token", token.as_str()),
                        ("current_email", email),
                        ("old_password", password),
                        ("new_password", "changed-password"),
                    ])
                    .to_request(),
            )
            .await;
            assert_eq!(resp.status(), expected);
            if let Some(value) = resp.headers().get("set-cookie") {
                cookie = Cookie::parse_encoded(value.to_str().unwrap())
                    .unwrap()
                    .into_owned();
            }
        }
        let (result, _) = validate_user_credentials_es(
            stack.read_model_store.as_ref(),
            "updated@example.com",
            "changed-password",
        )
        .await;
        assert_eq!(result, UserValidationResult::Valid);
        let (result, _) = validate_user_credentials_es(
            stack.read_model_store.as_ref(),
            "other@example.com",
            "other-password",
        )
        .await;
        assert_eq!(result, UserValidationResult::Valid);
    }
    #[serial]
    #[actix_web::test]
    async fn forwarding_headers_cannot_bypass_html_or_api_login_limits() {
        let _guard = InMemoryTestGuard;
        let stack = build_stack_with_default_user().await;
        let sessions: web::Data<dyn arc_core::session::SessionStore> = web::Data::from(
            std::sync::Arc::new(arc_core::session::InMemorySessionStore::new())
                as std::sync::Arc<dyn arc_core::session::SessionStore>,
        );
        let app = test::init_service(
            App::new()
                .app_data(stack.read_model_store.clone())
                .app_data(sessions)
                .app_data(web::Data::new(
                    crate::helpers::rate_limit::LoginRateLimiter(
                        crate::helpers::rate_limit::RateLimiter::new(
                            1,
                            std::time::Duration::from_secs(60),
                        ),
                    ),
                ))
                .wrap(SessionMiddleware::new(
                    CookieSessionStore::default(),
                    Key::generate(),
                ))
                .service(auth_controller::signin_post)
                .service(crate::http::controllers::api_controller::login),
        )
        .await;
        for index in 0..2 {
            let req = test::TestRequest::post()
                .uri("/signin")
                .peer_addr(format!("192.0.2.1:{}", 1000 + index).parse().unwrap())
                .insert_header(("X-Forwarded-For", format!("198.51.100.{index}")))
                .insert_header(("Forwarded", format!("for=198.51.100.{index}")))
                .set_form([
                    ("csrf_token", "bad"),
                    ("email", "jekyll@example.com"),
                    ("password", "bad"),
                ])
                .to_request();
            let resp = test::call_service(&app, req).await;
            assert_eq!(resp.status(), http::StatusCode::SEE_OTHER);
            assert_eq!(resp.headers().contains_key("Retry-After"), index == 1);
            let req = test::TestRequest::post()
                .uri("/login")
                .peer_addr(format!("192.0.2.1:{}", 2000 + index).parse().unwrap())
                .insert_header(("X-Forwarded-For", format!("198.51.100.{index}")))
                .insert_header(("Forwarded", format!("for=198.51.100.{index}")))
                .set_json(serde_json::json!({"email": "jekyll@example.com", "password": "bad"}))
                .to_request();
            let resp = test::call_service(&app, req).await;
            assert_eq!(
                resp.status(),
                if index == 0 {
                    http::StatusCode::UNAUTHORIZED
                } else {
                    http::StatusCode::TOO_MANY_REQUESTS
                }
            );
        }
    }
}
