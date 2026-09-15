use actix_web::{
    body::EitherBody,
    dev::{forward_ready, Service, ServiceRequest, ServiceResponse, Transform},
    web, Error, HttpMessage, HttpResponse,
};
use arc_auth_core::{AuthorizationPolicy, Identity};
use arc_web::{ArcAppBuilder, ArcPlugin};
use futures_util::future::LocalBoxFuture;
use std::{
    future::{ready, Ready},
    sync::Arc,
};

pub const ADMIN: &str = "admin";
pub const USER: &str = "user";

pub struct SimpleRbac;

pub struct RbacPlugin;
#[async_trait::async_trait]
impl ArcPlugin for RbacPlugin {
    fn name(&self) -> &'static str {
        "auth-rbac"
    }
    fn register(&self, builder: ArcAppBuilder) -> ArcAppBuilder {
        builder
    }
}

impl AuthorizationPolicy for SimpleRbac {
    fn permits(&self, identity: &Identity, required_roles: &[&str]) -> bool {
        identity.active
            && (required_roles.is_empty()
                || required_roles
                    .iter()
                    .any(|required| identity.has_role(required)))
    }
}

/// Role guard used after a transport authenticator. It accepts session identities
/// or resolves the actor id installed by Arc's JWT middleware.
pub struct RequireRoles {
    roles: Arc<[String]>,
}
impl RequireRoles {
    pub fn new(roles: &[&str]) -> Self {
        Self {
            roles: roles.iter().map(|r| (*r).to_owned()).collect(),
        }
    }
}
impl<S, B> Transform<S, ServiceRequest> for RequireRoles
where
    S: Service<ServiceRequest, Response = ServiceResponse<B>, Error = Error> + 'static,
    S::Future: 'static,
    B: 'static,
{
    type Response = ServiceResponse<EitherBody<B>>;
    type Error = Error;
    type InitError = ();
    type Transform = RoleCheck<S>;
    type Future = Ready<Result<Self::Transform, ()>>;
    fn new_transform(&self, service: S) -> Self::Future {
        ready(Ok(RoleCheck {
            service: Arc::new(service),
            roles: self.roles.clone(),
        }))
    }
}
pub struct RoleCheck<S> {
    service: Arc<S>,
    roles: Arc<[String]>,
}
impl<S, B> Service<ServiceRequest> for RoleCheck<S>
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
        let roles = self.roles.clone();
        let service = self.service.clone();
        Box::pin(async move {
            let actor_id = { req.extensions().get::<String>().cloned() };
            let identity = if let Some(id) = actor_id {
                match req.app_data::<web::Data<dyn arc_auth_core::IdentityStore>>() {
                    Some(store) => store.get(&id).await.ok().flatten(),
                    None => None,
                }
            } else {
                // Only accept the identity validated by RequireSession on this request.
                req.extensions().get::<Identity>().cloned()
            };
            let required = roles.iter().map(String::as_str).collect::<Vec<_>>();
            if identity
                .as_ref()
                .is_some_and(|i| SimpleRbac.permits(i, &required))
            {
                return service
                    .call(req)
                    .await
                    .map(ServiceResponse::map_into_left_body);
            }
            Ok(req.into_response(HttpResponse::Forbidden().finish().map_into_right_body()))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn permits_any_matching_role() {
        let identity = Identity {
            id: "1".into(),
            name: "A".into(),
            email: "a@b.c".into(),
            active: true,
            roles: vec![ADMIN.into()],
        };
        assert!(SimpleRbac.permits(&identity, &[ADMIN]));
        assert!(!SimpleRbac.permits(&identity, &[USER]));
    }
}
