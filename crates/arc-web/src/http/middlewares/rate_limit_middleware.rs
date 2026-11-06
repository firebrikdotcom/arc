use crate::helpers::rate_limit::RateLimiter;
use actix_web::{
    body::MessageBody,
    dev::{forward_ready, Service, ServiceRequest, ServiceResponse, Transform},
    web, Error,
};
use futures_util::future::LocalBoxFuture;
use std::future::{ready, Ready};

/// Middleware factory for global rate limiting
pub struct GlobalRateLimit;

impl<S, B> Transform<S, ServiceRequest> for GlobalRateLimit
where
    S: Service<ServiceRequest, Response = ServiceResponse<B>, Error = Error>,
    S::Future: 'static,
    B: MessageBody + 'static,
{
    type Response = ServiceResponse<B>;
    type Error = Error;
    type InitError = ();
    type Transform = GlobalRateLimitMiddleware<S>;
    type Future = Ready<Result<Self::Transform, Self::InitError>>;

    fn new_transform(&self, service: S) -> Self::Future {
        ready(Ok(GlobalRateLimitMiddleware { service }))
    }
}

pub struct GlobalRateLimitMiddleware<S> {
    service: S,
}

impl<S, B> Service<ServiceRequest> for GlobalRateLimitMiddleware<S>
where
    S: Service<ServiceRequest, Response = ServiceResponse<B>, Error = Error>,
    S::Future: 'static,
    B: MessageBody + 'static,
{
    type Response = ServiceResponse<B>;
    type Error = Error;
    type Future = LocalBoxFuture<'static, Result<Self::Response, Self::Error>>;

    forward_ready!(service);

    fn call(&self, req: ServiceRequest) -> Self::Future {
        let path = req.path().to_string();

        // Skip rate limiting for health check and static files
        if path == "/health" || path.starts_with("/public/") {
            let fut = self.service.call(req);
            return Box::pin(fut);
        }

        let ip = crate::helpers::rate_limit::client_ip(req.request());
        let key = format!("global:{}", ip);

        // Clone the limiter reference
        let limiter = req.app_data::<web::Data<RateLimiter>>().cloned();

        if let Some(limiter) = limiter {
            if let Err(retry_after) = limiter.check(key) {
                tracing::warn!(ip = %ip, path = %path, "Global rate limit exceeded");
                return Box::pin(async move {
                    Err(actix_web::error::ErrorTooManyRequests(format!(
                        "Rate limit exceeded. Retry after {} seconds",
                        retry_after.as_secs()
                    )))
                });
            }
        }
        Box::pin(self.service.call(req))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use actix_web::{test, App, HttpResponse};
    use std::time::Duration;

    #[actix_web::test]
    async fn forwarding_headers_and_source_ports_cannot_bypass_global_limit() {
        let app = test::init_service(
            App::new()
                .app_data(web::Data::new(RateLimiter::new(1, Duration::from_secs(60))))
                .wrap(GlobalRateLimit)
                .default_service(web::to(|| async { HttpResponse::Ok().finish() })),
        )
        .await;
        let first = test::TestRequest::get()
            .uri("/protected")
            .peer_addr("192.0.2.1:1000".parse().unwrap())
            .insert_header(("X-Forwarded-For", "198.51.100.1"))
            .insert_header(("Forwarded", "for=198.51.100.1"))
            .to_request();
        assert!(test::try_call_service(&app, first)
            .await
            .unwrap()
            .status()
            .is_success());
        let second = test::TestRequest::get()
            .uri("/protected")
            .peer_addr("192.0.2.1:2000".parse().unwrap())
            .insert_header(("X-Forwarded-For", "198.51.100.2"))
            .insert_header(("Forwarded", "for=198.51.100.2"))
            .to_request();
        let error = test::try_call_service(&app, second).await.unwrap_err();
        assert_eq!(
            error.as_response_error().status_code(),
            actix_web::http::StatusCode::TOO_MANY_REQUESTS
        );
        let other = test::TestRequest::get()
            .uri("/protected")
            .peer_addr("192.0.2.2:1000".parse().unwrap())
            .to_request();
        assert!(test::try_call_service(&app, other)
            .await
            .unwrap()
            .status()
            .is_success());
    }
}
