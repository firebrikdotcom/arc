# Testing

## Run project checks

```bash
make check
make test
```

## Test an endpoint

Actix can test handlers without opening a network port:

```rust
#[cfg(test)]
mod tests {
    use actix_web::{test, App};

    #[actix_web::test]
    async fn health_is_available() {
        let app = test::init_service(
            App::new().configure(crate::routes::config)
        )
        .await;

        let request = test::TestRequest::get()
            .uri("/health")
            .to_request();
        let response = test::call_service(&app, request).await;

        assert!(response.status().is_success());
    }
}
```

Place this at the bottom of `src/routes.rs` or under a dedicated test module.

## Test aggregate rules directly

Aggregate tests do not require a database:

```rust
use arc_core::aggregate::Aggregate;

#[actix_web::test]
async fn product_cannot_be_created_twice() {
    let mut product = ProductAggregate::default();
    let created = product
        .handle(ProductCommand::Create {
            id: "p1".into(),
            name: "Notebook".into(),
        })
        .await
        .unwrap();

    product.apply(&created[0]);

    let duplicate = product
        .handle(ProductCommand::Create {
            id: "p1".into(),
            name: "Notebook".into(),
        })
        .await;

    assert!(matches!(duplicate, Err(ProductError::AlreadyExists)));
}
```

Prefer testing business rules at the aggregate level, projector transformations separately, and only then the complete HTTP path.
