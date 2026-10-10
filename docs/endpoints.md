# Add an Endpoint

Endpoints are ordinary Actix Web handlers registered through your generated `routes::config`.

## Add a JSON endpoint

Add this to `src/routes.rs`:

```rust
use actix_web::{get, web, HttpResponse, Responder};

#[get("/api/greeting/{name}")]
async fn greeting(name: web::Path<String>) -> impl Responder {
    HttpResponse::Ok().json(serde_json::json!({
        "message": format!("Hello, {}!", name.into_inner())
    }))
}
```

Register it in the same file:

```rust
pub fn config(cfg: &mut web::ServiceConfig) {
    cfg.service(health)
        .service(greeting);
}
```

Run:

```bash
make dev
```

Verify:

```bash
curl --fail http://127.0.0.1:8080/api/greeting/Arc
```

Expected response:

```json
{"message":"Hello, Arc!"}
```

## Split routes into a module

As the application grows, create `src/http/products.rs`:

```rust
use actix_web::{get, HttpResponse, Responder};

#[get("/api/products")]
pub async fn index() -> impl Responder {
    HttpResponse::Ok().json(Vec::<serde_json::Value>::new())
}
```

Create `src/http/mod.rs`:

```rust
pub mod products;
```

Declare it from `src/main.rs`:

```rust
mod http;
```

Then register the handler in `src/routes.rs`:

```rust
pub fn config(cfg: &mut web::ServiceConfig) {
    cfg.service(health)
        .service(crate::http::products::index);
}
```

## What makes an endpoint “Arc”

A read-only or computed endpoint can remain a normal Actix handler. A state-changing endpoint should dispatch a command through `CommandBus`; it should not write directly to the event or read-model tables. See [Build a Resource](resources.md).
