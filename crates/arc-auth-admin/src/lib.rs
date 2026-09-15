//! Host-rendered authentication administration pages.

use actix_session::Session;
use actix_web::{http::StatusCode, web, HttpRequest, HttpResponse};
use arc_auth_core::{Identity, IdentityStore};
use arc_auth_session::{identity, RequireSession};
use arc_web::ui::{
    ActionMethod, AdminAction, AdminNavItem, Audience, Breadcrumb, TemplateBundle, TemplateDef,
    TemplateName, UiContribution, UiPage,
};
use arc_web::{ArcAppBuilder, ArcPlugin, UiRegistry};
use serde::Deserialize;
use tera::Context;

const TEMPLATES: &[TemplateDef] = &[
    TemplateDef {
        name: TemplateName("capabilities/auth-admin/signin.html"),
        source: include_str!("../templates/signin.html"),
    },
    TemplateDef {
        name: TemplateName("capabilities/auth-admin/profile.html"),
        source: include_str!("../templates/profile.html"),
    },
    TemplateDef {
        name: TemplateName("capabilities/auth-admin/users/index.html"),
        source: include_str!("../templates/users/index.html"),
    },
    TemplateDef {
        name: TemplateName("capabilities/auth-admin/users/form.html"),
        source: include_str!("../templates/users/form.html"),
    },
    TemplateDef {
        name: TemplateName("capabilities/auth-admin/users/detail.html"),
        source: include_str!("../templates/users/detail.html"),
    },
];
const FLASH_NOTICE_KEY: &str = "arc_auth_admin_notice";

fn set_notice(session: &Session, message: &str) {
    let _ = session.insert(FLASH_NOTICE_KEY, message);
}

fn take_notice(session: &Session) -> Option<String> {
    let notice = session.get(FLASH_NOTICE_KEY).ok().flatten();
    session.remove(FLASH_NOTICE_KEY);
    notice
}

fn home_breadcrumb(current: impl Into<String>) -> Vec<Breadcrumb> {
    vec![
        Breadcrumb::link("Home", "/admin"),
        Breadcrumb::current(current),
    ]
}

fn users_breadcrumb(current: impl Into<String>) -> Vec<Breadcrumb> {
    vec![
        Breadcrumb::link("Home", "/admin"),
        Breadcrumb::link("Users", "/admin/users"),
        Breadcrumb::current(current),
    ]
}

fn user_breadcrumb(user: &Identity, current: impl Into<String>) -> Vec<Breadcrumb> {
    vec![
        Breadcrumb::link("Home", "/admin"),
        Breadcrumb::link("Users", "/admin/users"),
        Breadcrumb::link(&user.name, format!("/admin/users/{}", user.id)),
        Breadcrumb::current(current),
    ]
}

pub struct AuthAdminPlugin;
#[async_trait::async_trait]
impl ArcPlugin for AuthAdminPlugin {
    fn name(&self) -> &'static str {
        "auth-admin"
    }
    fn register(&self, builder: ArcAppBuilder) -> ArcAppBuilder {
        builder
            .register_ui(UiContribution {
                owner: "auth-admin",
                templates: TemplateBundle {
                    templates: TEMPLATES,
                },
                navigation: vec![
                    AdminNavItem {
                        id: "auth-profile",
                        label: "Profile",
                        href: "/admin/profile",
                        order: 800,
                        audience: Audience::Authenticated,
                    },
                    AdminNavItem {
                        id: "auth-users",
                        label: "Users",
                        href: "/admin/users",
                        order: 810,
                        audience: Audience::AnyRole(&["admin"]),
                    },
                ],
                actions: vec![AdminAction {
                    id: "auth-signout",
                    label: "Sign out",
                    href: "/signout",
                    method: ActionMethod::PostWithCsrf,
                    audience: Audience::Authenticated,
                }],
                ..UiContribution::default()
            })
            .register_routes(routes)
    }
}

struct PageRender {
    title: &'static str,
    context: Context,
    status: StatusCode,
    breadcrumbs: Vec<Breadcrumb>,
}

fn render(
    registry: &UiRegistry,
    request: &HttpRequest,
    session: &Session,
    name: &'static str,
    mut page: PageRender,
) -> HttpResponse {
    page.context.insert("title", page.title);
    registry.render(
        UiPage {
            template: TemplateName(name),
            title: page.title.into(),
            context: page.context,
            status: page.status,
            breadcrumbs: page.breadcrumbs,
        },
        request,
        session,
    )
}
fn csrf(session: &Session, token: &str) -> bool {
    arc_web::helpers::csrf::validate_and_regenerate_csrf_token(session, token)
}
fn admin(session: &Session) -> Result<Identity, Box<HttpResponse>> {
    match identity(session) {
        Some(v) if v.has_role("admin") => Ok(v),
        Some(_) => Err(Box::new(HttpResponse::Forbidden().finish())),
        None => Err(Box::new(HttpResponse::Unauthorized().finish())),
    }
}

#[derive(Deserialize)]
struct SignInForm {
    email: String,
    password: String,
    csrf_token: String,
}
async fn signin_page(
    req: HttpRequest,
    session: Session,
    registry: web::Data<UiRegistry>,
) -> HttpResponse {
    signin_response(&req, &session, &registry, "", None, StatusCode::OK)
}
fn signin_response(
    req: &HttpRequest,
    session: &Session,
    registry: &UiRegistry,
    email: &str,
    error: Option<&str>,
    status: StatusCode,
) -> HttpResponse {
    let mut c = Context::new();
    c.insert("email", email);
    c.insert("error", &error);
    render(
        registry,
        req,
        session,
        "capabilities/auth-admin/signin.html",
        PageRender {
            title: "Sign in",
            context: c,
            status,
            breadcrumbs: vec![],
        },
    )
}
async fn signin(
    req: HttpRequest,
    form: web::Form<SignInForm>,
    session: Session,
    store: web::Data<dyn IdentityStore>,
    registry: web::Data<UiRegistry>,
) -> HttpResponse {
    if !csrf(&session, &form.csrf_token) {
        return HttpResponse::Forbidden().finish();
    }
    match arc_auth_session::authenticate(&session, store.get_ref(), &form.email, &form.password)
        .await
    {
        Ok(_) => HttpResponse::SeeOther()
            .insert_header(("Location", "/admin"))
            .finish(),
        Err(_) => signin_response(
            &req,
            &session,
            &registry,
            &form.email,
            Some("Email or password was not recognized."),
            StatusCode::UNAUTHORIZED,
        ),
    }
}
#[derive(Deserialize)]
struct CsrfForm {
    csrf_token: String,
}
async fn signout(form: web::Form<CsrfForm>, session: Session) -> HttpResponse {
    if !csrf(&session, &form.csrf_token) {
        return HttpResponse::Forbidden().finish();
    }
    arc_auth_session::sign_out(&session);
    HttpResponse::SeeOther()
        .insert_header(("Location", "/"))
        .finish()
}

async fn profile(
    req: HttpRequest,
    session: Session,
    registry: web::Data<UiRegistry>,
) -> HttpResponse {
    let Some(user) = identity(&session) else {
        return HttpResponse::Unauthorized().finish();
    };
    profile_response(&req, &session, &registry, &user, None, StatusCode::OK)
}
fn profile_response(
    req: &HttpRequest,
    session: &Session,
    registry: &UiRegistry,
    user: &Identity,
    error: Option<&str>,
    status: StatusCode,
) -> HttpResponse {
    let mut c = Context::new();
    c.insert("user", user);
    c.insert("error", &error);
    c.insert("notice", &take_notice(session));
    render(
        registry,
        req,
        session,
        "capabilities/auth-admin/profile.html",
        PageRender {
            title: "Profile",
            context: c,
            status,
            breadcrumbs: home_breadcrumb("Profile"),
        },
    )
}
#[derive(Deserialize)]
struct ProfileForm {
    name: String,
    email: String,
    csrf_token: String,
}
async fn profile_save(
    req: HttpRequest,
    form: web::Form<ProfileForm>,
    session: Session,
    store: web::Data<dyn IdentityStore>,
    registry: web::Data<UiRegistry>,
) -> HttpResponse {
    if !csrf(&session, &form.csrf_token) {
        return HttpResponse::Forbidden().finish();
    }
    let Some(user) = identity(&session) else {
        return HttpResponse::Unauthorized().finish();
    };
    match store
        .update_profile(&user.id, &form.name, &form.email)
        .await
    {
        Ok(updated) => {
            arc_auth_session::cache_identity(&session, &updated);
            set_notice(&session, "Profile saved.");
            HttpResponse::SeeOther()
                .insert_header(("Location", "/admin/profile"))
                .finish()
        }
        Err(e) => profile_response(
            &req,
            &session,
            &registry,
            &user,
            Some(&e.to_string()),
            StatusCode::UNPROCESSABLE_ENTITY,
        ),
    }
}
#[derive(Deserialize)]
struct PasswordForm {
    current_password: String,
    new_password: String,
    csrf_token: String,
}
async fn password_save(
    req: HttpRequest,
    form: web::Form<PasswordForm>,
    session: Session,
    store: web::Data<dyn IdentityStore>,
    registry: web::Data<UiRegistry>,
) -> HttpResponse {
    if !csrf(&session, &form.csrf_token) {
        return HttpResponse::Forbidden().finish();
    }
    let Some(user) = identity(&session) else {
        return HttpResponse::Unauthorized().finish();
    };
    if store
        .authenticate(&user.email, &form.current_password)
        .await
        .is_err()
    {
        return profile_response(
            &req,
            &session,
            &registry,
            &user,
            Some("Current password is incorrect."),
            StatusCode::UNAUTHORIZED,
        );
    }
    match store.change_password(&user.id, &form.new_password).await {
        Ok(()) => {
            set_notice(&session, "Password changed.");
            HttpResponse::SeeOther()
                .insert_header(("Location", "/admin/profile"))
                .finish()
        }
        Err(e) => profile_response(
            &req,
            &session,
            &registry,
            &user,
            Some(&e.to_string()),
            StatusCode::UNPROCESSABLE_ENTITY,
        ),
    }
}

#[derive(Deserialize)]
struct UsersQuery {
    filter: Option<String>,
    page: Option<u64>,
}
async fn users(
    req: HttpRequest,
    query: web::Query<UsersQuery>,
    session: Session,
    store: web::Data<dyn IdentityStore>,
    registry: web::Data<UiRegistry>,
) -> HttpResponse {
    if let Err(r) = admin(&session) {
        return *r;
    }
    let page = query.page.unwrap_or(1).max(1);
    let mut window = match arc_auth_core::CollectionQuery::for_page(page, 20) {
        Ok(window) => window,
        Err(_) => return HttpResponse::BadRequest().body("Invalid page"),
    };
    window.filter = query.filter.clone().unwrap_or_default();
    if window.validate().is_err() {
        return HttpResponse::BadRequest().body("Invalid filter");
    }
    let filter = window.filter.trim().to_ascii_lowercase();
    match store.collection(&window).await {
        Ok(result) => {
            let users = result.rows;
            let mut c = Context::new();
            c.insert("users", &users);
            c.insert("filter", &filter);
            c.insert("page", &page);
            c.insert("has_next", &result.has_next);
            c.insert("notice", &take_notice(&session));
            render(
                &registry,
                &req,
                &session,
                "capabilities/auth-admin/users/index.html",
                PageRender {
                    title: "Users",
                    context: c,
                    status: StatusCode::OK,
                    breadcrumbs: home_breadcrumb("Users"),
                },
            )
        }
        Err(e) => HttpResponse::InternalServerError().body(e.to_string()),
    }
}
async fn user_new(
    req: HttpRequest,
    session: Session,
    registry: web::Data<UiRegistry>,
) -> HttpResponse {
    if let Err(r) = admin(&session) {
        return *r;
    }
    user_form(&req, &session, &registry, None, None, StatusCode::OK)
}
fn user_form(
    req: &HttpRequest,
    session: &Session,
    registry: &UiRegistry,
    user: Option<&Identity>,
    error: Option<&str>,
    status: StatusCode,
) -> HttpResponse {
    let mut c = Context::new();
    c.insert("user", &user);
    c.insert("error", &error);
    c.insert("creating", &user.is_none());
    c.insert("notice", &take_notice(session));
    render(
        registry,
        req,
        session,
        "capabilities/auth-admin/users/form.html",
        PageRender {
            title: if user.is_some() {
                "Edit user"
            } else {
                "Create user"
            },
            context: c,
            status,
            breadcrumbs: match user {
                Some(user) => user_breadcrumb(user, "Edit"),
                None => users_breadcrumb("Create user"),
            },
        },
    )
}
#[derive(Deserialize)]
struct CreateForm {
    name: String,
    email: String,
    password: String,
    roles: String,
    csrf_token: String,
}
fn roles(value: &str) -> Vec<String> {
    value
        .split(',')
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(str::to_owned)
        .collect()
}
async fn user_create(
    req: HttpRequest,
    form: web::Form<CreateForm>,
    session: Session,
    store: web::Data<dyn IdentityStore>,
    registry: web::Data<UiRegistry>,
) -> HttpResponse {
    if !csrf(&session, &form.csrf_token) {
        return HttpResponse::Forbidden().finish();
    }
    if let Err(r) = admin(&session) {
        return *r;
    }
    match store
        .create_user(&form.name, &form.email, &form.password, &roles(&form.roles))
        .await
    {
        Ok(user) => {
            set_notice(&session, "User created.");
            HttpResponse::SeeOther()
                .insert_header(("Location", format!("/admin/users/{}", user.id)))
                .finish()
        }
        Err(e) => user_form(
            &req,
            &session,
            &registry,
            None,
            Some(&e.to_string()),
            StatusCode::UNPROCESSABLE_ENTITY,
        ),
    }
}
async fn user_detail(
    req: HttpRequest,
    id: web::Path<String>,
    session: Session,
    store: web::Data<dyn IdentityStore>,
    registry: web::Data<UiRegistry>,
) -> HttpResponse {
    if let Err(r) = admin(&session) {
        return *r;
    }
    match store.get(&id).await {
        Ok(Some(user)) => {
            let mut c = Context::new();
            c.insert("user", &user);
            c.insert("notice", &take_notice(&session));
            render(
                &registry,
                &req,
                &session,
                "capabilities/auth-admin/users/detail.html",
                PageRender {
                    title: "User detail",
                    context: c,
                    status: StatusCode::OK,
                    breadcrumbs: users_breadcrumb(&user.name),
                },
            )
        }
        Ok(None) => HttpResponse::NotFound().finish(),
        Err(e) => HttpResponse::InternalServerError().body(e.to_string()),
    }
}
async fn user_edit(
    req: HttpRequest,
    id: web::Path<String>,
    session: Session,
    store: web::Data<dyn IdentityStore>,
    registry: web::Data<UiRegistry>,
) -> HttpResponse {
    if let Err(r) = admin(&session) {
        return *r;
    }
    match store.get(&id).await {
        Ok(Some(user)) => user_form(&req, &session, &registry, Some(&user), None, StatusCode::OK),
        Ok(None) => HttpResponse::NotFound().finish(),
        Err(e) => HttpResponse::InternalServerError().body(e.to_string()),
    }
}
#[derive(Deserialize)]
struct EditForm {
    name: String,
    email: String,
    csrf_token: String,
}
async fn user_update(
    req: HttpRequest,
    id: web::Path<String>,
    form: web::Form<EditForm>,
    session: Session,
    store: web::Data<dyn IdentityStore>,
    registry: web::Data<UiRegistry>,
) -> HttpResponse {
    if !csrf(&session, &form.csrf_token) {
        return HttpResponse::Forbidden().finish();
    }
    if let Err(r) = admin(&session) {
        return *r;
    }
    match store.update_profile(&id, &form.name, &form.email).await {
        Ok(_) => {
            set_notice(&session, "User saved.");
            HttpResponse::SeeOther()
                .insert_header(("Location", format!("/admin/users/{id}")))
                .finish()
        }
        Err(e) => {
            let user = store.get(&id).await.ok().flatten();
            user_form(
                &req,
                &session,
                &registry,
                user.as_ref(),
                Some(&e.to_string()),
                StatusCode::UNPROCESSABLE_ENTITY,
            )
        }
    }
}
#[derive(Deserialize)]
struct RolesForm {
    roles: String,
    csrf_token: String,
}
async fn roles_save(
    id: web::Path<String>,
    form: web::Form<RolesForm>,
    session: Session,
    store: web::Data<dyn IdentityStore>,
) -> HttpResponse {
    if !csrf(&session, &form.csrf_token) {
        return HttpResponse::Forbidden().finish();
    }
    if let Err(r) = admin(&session) {
        return *r;
    }
    match store.set_roles(&id, &roles(&form.roles)).await {
        Ok(_) => {
            set_notice(&session, "Roles updated.");
            HttpResponse::SeeOther()
                .insert_header(("Location", format!("/admin/users/{id}")))
                .finish()
        }
        Err(e) => HttpResponse::BadRequest().body(e.to_string()),
    }
}
#[derive(Deserialize)]
struct ActivationForm {
    active: bool,
    csrf_token: String,
}
async fn activation(
    id: web::Path<String>,
    form: web::Form<ActivationForm>,
    session: Session,
    store: web::Data<dyn IdentityStore>,
) -> HttpResponse {
    if !csrf(&session, &form.csrf_token) {
        return HttpResponse::Forbidden().finish();
    }
    if let Err(r) = admin(&session) {
        return *r;
    }
    match store.set_active(&id, form.active).await {
        Ok(_) => {
            set_notice(&session, "User status updated.");
            HttpResponse::SeeOther()
                .insert_header(("Location", format!("/admin/users/{id}")))
                .finish()
        }
        Err(e) => HttpResponse::BadRequest().body(e.to_string()),
    }
}

pub fn routes(cfg: &mut web::ServiceConfig) {
    let idle =
        || arc_web::http::middlewares::idle_timeout_middleware::IdleTimeoutMiddleware::from_env();
    cfg.route("/signin", web::get().to(signin_page))
        .route("/signin", web::post().to(signin))
        .route("/signout", web::post().to(signout))
        .service(
            web::scope("/admin/profile")
                .wrap(RequireSession)
                .wrap(idle())
                .route("", web::get().to(profile))
                .route("", web::post().to(profile_save))
                .route("/password", web::post().to(password_save)),
        )
        .service(
            web::scope("/admin/users")
                .wrap(RequireSession)
                .wrap(idle())
                .route("", web::get().to(users))
                .route("/new", web::get().to(user_new))
                .route("/new", web::post().to(user_create))
                .route("/{id}", web::get().to(user_detail))
                .route("/{id}/edit", web::get().to(user_edit))
                .route("/{id}/edit", web::post().to(user_update))
                .route("/{id}/roles", web::post().to(roles_save))
                .route("/{id}/activation", web::post().to(activation)),
        );
}

#[cfg(test)]
mod tests {
    use super::{home_breadcrumb, user_breadcrumb, TEMPLATES};
    use arc_auth_core::Identity;

    #[test]
    fn success_notice_regions_are_available_on_mutation_destinations() {
        for template in TEMPLATES {
            if matches!(
                template.name.0,
                "capabilities/auth-admin/profile.html"
                    | "capabilities/auth-admin/users/index.html"
                    | "capabilities/auth-admin/users/detail.html"
            ) {
                assert!(template.source.contains("alert--success"));
                assert!(template.source.contains("role=\"status\""));
                assert!(template.source.contains("notice"));
            }
        }
    }

    #[test]
    fn breadcrumb_trails_start_at_home_and_mark_the_current_page() {
        let profile = home_breadcrumb("Profile");
        assert_eq!(profile[0].label, "Home");
        assert_eq!(profile[0].href.as_deref(), Some("/admin"));
        assert_eq!(profile[1].label, "Profile");
        assert!(profile[1].href.is_none());

        let user = Identity {
            id: "user-1".into(),
            name: "Ada Lovelace".into(),
            email: "ada@example.com".into(),
            active: true,
            roles: vec!["admin".into()],
        };
        let edit = user_breadcrumb(&user, "Edit");
        assert_eq!(edit[2].href.as_deref(), Some("/admin/users/user-1"));
        assert_eq!(edit[3].label, "Edit");
        assert!(edit[3].href.is_none());
    }
}
