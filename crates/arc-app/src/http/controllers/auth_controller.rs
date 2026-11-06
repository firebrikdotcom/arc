use crate::helpers::csrf::{get_csrf_token, validate_and_regenerate_csrf_token};
use crate::helpers::rate_limit::LoginRateLimiter;
use crate::helpers::session::{
    clear_session_user, get_session_message, is_authenticated, set_session_user, SessionUser,
};
use crate::helpers::template::load_template;
use crate::services::user_service::{validate_user_credentials_es, UserValidationResult};
use crate::validation::user_validation::LoginForm as LoginValidation;
use crate::AppState;
use actix_session::Session;
use actix_web::{get, post, web, HttpRequest, HttpResponse, Responder};
use arc_core::read_model_store::ReadModelStore;
use serde::Deserialize;
use tracing::warn;
use validator::Validate;

/// Form data for the sign-in POST request.
#[derive(Deserialize)]
pub struct SigninForm {
    csrf_token: String,
    email: String,
    password: String,
}

/// Renders the sign-in page. Redirects to `/admin` if already authenticated.
#[get("/signin")]
pub async fn signin(data: web::Data<AppState>, session: Session) -> impl Responder {
    let app_name = &data.app_name.lock().unwrap();

    if is_authenticated(&session) {
        return HttpResponse::Found()
            .insert_header(("Location", "/admin"))
            .finish();
    }

    let session_message: (String, String) = get_session_message(&session, true);
    let csrf_token = get_csrf_token(&session);

    HttpResponse::Ok()
        .content_type("text/html; charset=utf-8")
        .body(load_template(
            "signin.html",
            vec![
                ("name", app_name),
                ("session_message", &*session_message.1),
                ("csrf_token", &csrf_token),
            ],
            None,
        ))
}

/// Signs the user out by clearing session data and redirecting to the home page.
#[get("/signout")]
pub async fn signout(session: Session) -> impl Responder {
    clear_session_user(&session);
    session
        .insert("message", "You have been signed out")
        .unwrap();

    HttpResponse::Found()
        .insert_header(("Location", "/"))
        .finish()
}

/// Handles sign-in form submission. Enforces rate limiting, validates CSRF
/// token and input, then authenticates against the `users_view` projection.
#[post("/signin")]
pub async fn signin_post(
    req: HttpRequest,
    form: web::Form<SigninForm>,
    session: Session,
    limiter: web::Data<LoginRateLimiter>,
    read_model_store: web::Data<dyn ReadModelStore>,
) -> impl Responder {
    let ip = crate::helpers::rate_limit::client_ip(&req);
    let key = format!("login:{}", ip);

    if let Err(retry_after) = limiter.check(key.clone()) {
        warn!(
            ip = ip,
            path = req.path(),
            retry_after_secs = retry_after.as_secs(),
            "Rate limit exceeded on login attempt"
        );

        session
            .insert(
                "message",
                serde_json::json!({
                    "error": "Too many login attempts. Please try again later.",
                    "success": ""
                }),
            )
            .unwrap();

        return HttpResponse::SeeOther()
            .insert_header(("Location", "/signin"))
            .insert_header(("Retry-After", retry_after.as_secs().to_string()))
            .finish();
    }

    if !validate_and_regenerate_csrf_token(&session, &form.csrf_token) {
        session
            .insert(
                "message",
                serde_json::json!({
                    "error": "Invalid request. Please try again.",
                    "success": ""
                }),
            )
            .unwrap();

        return HttpResponse::SeeOther()
            .insert_header(("Location", "/signin"))
            .finish();
    }

    let login_validation = LoginValidation {
        email: form.email.clone(),
        password: form.password.clone(),
    };
    if login_validation.validate().is_err() {
        session
            .insert(
                "message",
                serde_json::json!({
                    "error": "Please enter a valid email and password.",
                    "success": ""
                }),
            )
            .unwrap();

        return HttpResponse::SeeOther()
            .insert_header(("Location", "/signin"))
            .finish();
    }

    let (validation, agg_id) =
        validate_user_credentials_es(read_model_store.as_ref(), &form.email, &form.password).await;

    let invalid_credentials = || {
        session
            .insert(
                "message",
                serde_json::json!({"error": "Invalid credentials", "success": ""}),
            )
            .ok();
        HttpResponse::SeeOther()
            .insert_header(("Location", "/signin"))
            .finish()
    };

    let agg_id = match (validation, agg_id) {
        (UserValidationResult::Valid, Some(id)) => id,
        _ => return invalid_credentials(),
    };

    // Pull the same projection row to populate the cached SessionUser. Falling
    // through to "invalid credentials" if the row vanished between validation
    // and read keeps the response shape consistent — we do not 500 on a race
    // here, the user can simply retry.
    let user = match SessionUser::from_projection(
        read_model_store.as_ref(),
        crate::domain::user::projector::USERS_VIEW,
        &agg_id,
    )
    .await
    {
        Some(u) => u,
        None => return invalid_credentials(),
    };

    set_session_user(&session, &user);

    HttpResponse::SeeOther()
        .insert_header(("Location", "/admin"))
        .finish()
}
