# Build an Event-Sourced Resource

This tutorial turns a fresh generated app into a small Product API:

```text
POST /api/products
GET  /api/products
GET  /api/products/{id}
```

A Product write follows Arc's supported path:

```text
HTTP request → ProductCommand → ProductAggregate → ProductCreated event
             → event store → ProductProjector → products_view
```

The command side is authoritative. The projection is the query model.

## 1. Create the app

```bash
arc new catalog
cd catalog
make setup
```

The generated `AppAggregate` is a placeholder. Generate a complete Product resource beside it:

```bash
arc plugin add auth-db-session
arc plugin add auth-db-jwt
arc generate resource Product --api --ui --api-auth jwt
make migrate
```

The command creates the aggregate, commands, event payloads, projector, read-model migration,
focused tests, and JSON CRUD API, then registers them with the application. It refuses to overwrite
an existing resource. `arc generate aggregate Product --api` is an alias. Installing the session
plugin before generation protects browser routes automatically; API routes require the explicit
`--api-auth jwt` option. See [Authentication Plugins](auth-plugins.md).

## 2. Understand the generated Product aggregate

The generated files live under `src/domain/product/`:

```text
src/domain/product/
├── aggregate.rs
├── commands.rs
├── events.rs
├── mod.rs
└── projector.rs
```

The generated aggregate follows this command/event pattern:

```rust
use arc_core::aggregate::{Aggregate, Command};
use arc_core::event::{Event, NewEvent};
use async_trait::async_trait;
use serde_json::json;
use thiserror::Error;

pub mod projector;

#[derive(Debug)]
pub enum ProductCommand {
    Create { id: String, name: String },
}

impl Command for ProductCommand {
    fn aggregate_id(&self) -> &str {
        match self {
            Self::Create { id, .. } => id,
        }
    }
}

#[derive(Default)]
pub struct ProductAggregate {
    id: Option<String>,
    name: Option<String>,
    version: i64,
}

#[derive(Debug, Error)]
pub enum ProductError {
    #[error("product already exists")]
    AlreadyExists,
}

#[async_trait]
impl Aggregate for ProductAggregate {
    type Command = ProductCommand;
    type Event = ();
    type Error = ProductError;

    fn aggregate_type() -> &'static str {
        "Product"
    }

    fn version(&self) -> i64 {
        self.version
    }

    async fn handle(
        &self,
        command: Self::Command,
    ) -> Result<Vec<Event>, Self::Error> {
        match command {
            ProductCommand::Create { id, name } => {
                if self.id.is_some() {
                    return Err(ProductError::AlreadyExists);
                }

                Ok(vec![Event::new(NewEvent {
                    aggregate_type: Self::aggregate_type(),
                    aggregate_id: id.clone(),
                    sequence: self.version + 1,
                    event_type: "ProductCreated",
                    payload: json!({ "id": id, "name": name }),
                })])
            }
        }
    }

    fn apply(&mut self, event: &Event) {
        if event.event_type == "ProductCreated" {
            self.id = event.payload["id"].as_str().map(str::to_owned);
            self.name = event.payload["name"].as_str().map(str::to_owned);
            self.version = event.sequence;
        }
    }
}
```

`handle` validates intent and produces events. `apply` reconstructs state from stored events. Neither function writes to a database.

## 3. Understand the generated read model

`src/domain/product/projector.rs` contains a version-gated projector like this:

```rust
use arc_core::event::Event;
use arc_core::projection::{
    ProjectionError, ProjectionResult, Projector,
};
use arc_core::read_model_store::{ReadModelStore, Upsert};
use async_trait::async_trait;
use serde_json::json;

pub const PRODUCTS_VIEW: &str = "products_view";

pub struct ProductProjector;

#[async_trait]
impl Projector for ProductProjector {
    fn name(&self) -> &str {
        "ProductProjector"
    }

    fn handles(&self) -> Vec<String> {
        vec!["ProductCreated".to_string()]
    }

    async fn apply(
        &self,
        event: &Event,
        store: &dyn ReadModelStore,
    ) -> ProjectionResult<()> {
        let row = json!({
            "id": event.aggregate_id.clone(),
            "name": event.payload["name"].clone(),
            "version": event.sequence,
        });

        store
            .upsert(Upsert::new(
                PRODUCTS_VIEW,
                &event.aggregate_id,
                row,
            ))
            .await
            .map_err(|error| ProjectionError::handle_failed(
                self.name(),
                &event.event_type,
                event.event_id.to_string(),
                error.to_string(),
            ))
    }
}
```

Every projected row must contain an integer `version`. Arc uses it to make replayed upserts idempotent.

## 4. Review the generated read-model migration

The generator chooses the next migration number and creates:

```text
migrations/00000000000001_products_view/
├── up.sql
└── down.sql
```

`up.sql`:

```sql
CREATE TABLE products_view (
    id TEXT PRIMARY KEY NOT NULL,
    version BIGINT NOT NULL,
    data TEXT NOT NULL
);
```

`down.sql`:

```sql
DROP TABLE products_view;
```

Apply it if you have not already:

```bash
make migrate
```

Arc's SQLite read-model store expects the standard `id`, `version`, and JSON `data` columns.

## 5. Review the generated registration

The generator adds imports for `ProductAggregate`, `ProductProjector`, and `PRODUCT_VIEW`, then
extends the builder without removing the placeholder aggregate:

```rust
ArcApp::builder()
    .register_aggregate::<AppAggregate>()
    .register_aggregate::<ProductAggregate>()
    .register_projector(ProductProjector, PRODUCTS_VIEW)
    .register_routes(routes::config)
    .serve(host.clone(), port)
    .await
    .with_context(|| {
        format!(
            "could not start on {host}:{port}; \
             the port may be busy (change APP_PORT in .env)"
        )
    })
```

`src/domain.rs` also receives `pub mod product;`. The marker-based edits are deterministic, so
future generated resources are appended at the same extension points.

## 6. Use the generated Product handlers

The `--api` flag creates `src/domain/product/api.rs` and registers it in `src/routes.rs`. It exposes
create, list, get, update, and delete endpoints. The generated handlers follow this abbreviated
pattern:

> **Note:** [`web::Data`](project-structure.md#actix-shared-application-data) gives these routes access to the command bus and read-model store that Arc created when the server started.

```rust
use actix_web::{
    get, post, web, HttpResponse, Responder,
};
use arc_core::command_bus::{CommandBus, CommandContext};
use arc_core::read_model_store::ReadModelStore;
use serde::Deserialize;
use serde_json::json;

use crate::domain::product::projector::PRODUCTS_VIEW;
use crate::domain::product::aggregate::ProductAggregate;
use crate::domain::product::commands::ProductCommand;

#[derive(Deserialize)]
struct CreateProduct {
    id: String,
    name: String,
}

#[get("/health")]
async fn health() -> impl Responder {
    HttpResponse::Ok().json(json!({ "status": "healthy" }))
}

#[post("/api/products")]
async fn create_product(
    body: web::Json<CreateProduct>,
    bus: web::Data<CommandBus<ProductAggregate>>,
) -> impl Responder {
    let command = ProductCommand::Create {
        id: body.id.clone(),
        name: body.name.clone(),
    };

    match bus
        .dispatch(command, CommandContext::for_actor("anonymous"))
        .await
    {
        Ok(_) => HttpResponse::Created().json(json!({
            "id": body.id.clone()
        })),
        Err(error) => HttpResponse::BadRequest().json(json!({
            "error": error.to_string()
        })),
    }
}

#[get("/api/products")]
async fn list_products(
    store: web::Data<dyn ReadModelStore>,
) -> impl Responder {
    match store.list(PRODUCTS_VIEW).await {
        Ok(products) => HttpResponse::Ok().json(products),
        Err(error) => HttpResponse::InternalServerError().json(json!({
            "error": error.to_string()
        })),
    }
}

#[get("/api/products/{id}")]
async fn get_product(
    id: web::Path<String>,
    store: web::Data<dyn ReadModelStore>,
) -> impl Responder {
    match store.get(PRODUCTS_VIEW, id.as_str()).await {
        Ok(Some(product)) => HttpResponse::Ok().json(product),
        Ok(None) => HttpResponse::NotFound().finish(),
        Err(error) => HttpResponse::InternalServerError().json(json!({
            "error": error.to_string()
        })),
    }
}

pub fn config(cfg: &mut web::ServiceConfig) {
    cfg.service(health)
        .service(create_product)
        .service(list_products)
        .service(get_product);
}
```

For a real public API, derive the actor from authentication rather than using `"anonymous"`.
Authentication is not generated today, so these routes are intended as a development starting
point rather than a production security boundary.

## 7. Run and verify

```bash
make check
make dev
```

In another terminal:

```bash
curl --fail \
  --request POST \
  --header 'content-type: application/json' \
  --data '{"id":"product-1","name":"Notebook"}' \
  http://127.0.0.1:8080/api/products
```

Then query the projection:

```bash
curl --fail http://127.0.0.1:8080/api/products
curl --fail http://127.0.0.1:8080/api/products/product-1

curl --fail \
  --request PUT \
  --header 'content-type: application/json' \
  --data '{"name":"Field Notebook"}' \
  http://127.0.0.1:8080/api/products/product-1

curl --fail \
  --request DELETE \
  http://127.0.0.1:8080/api/products/product-1
```

Because the default event bus is `inprocess`, the Product projection is updated before the POST returns.

Inspect the persisted event stream directly during development:

```bash
sqlite3 -json database/database.sqlite \
  "SELECT sequence, aggregate_id, event_type, payload
   FROM events
   WHERE aggregate_type = 'Product'
   ORDER BY aggregate_id, sequence;" | jq
```

Inspect the current projected rows:

```bash
sqlite3 -json database/database.sqlite \
  "SELECT id, version, json(data) AS data FROM products_view;" | jq
```

## Generated input limits

Newly generated resources reject blank names and names exceeding **1024 UTF-8 bytes**
in the aggregate, for both create and rename commands. This bounds the name stored in
each event, including commands issued outside HTTP. JSON writes return a bad-request
response for these validation failures; browser forms redisplay the error. Multibyte
characters count by their encoded size. Names are preserved verbatim: Unicode, quotes,
angle brackets, and surrounding whitespace in nonblank names are not stripped. Keep
Tera autoescaping enabled when displaying names; validation does not replace output
escaping. Customize the aggregate limit for your domain when needed.

Generated browser collections treat page zero as page one and use saturating offset
arithmetic. Extremely large page numbers produce an empty page, without integer
overflow or wrapping to earlier records. Collections still load the read model before
paginating; this is not a database-level result limit.

These generator changes apply to newly generated code. Existing applications must
adopt the same validation and pagination changes in their resource modules.

## Customizing the resource

The generated API already includes rename/update and delete behavior. To add another operation:

1. Add variants to `ProductCommand`.
2. Validate them in `ProductAggregate::handle`.
3. produce past-tense events such as `ProductRenamed`.
4. Apply those events in `ProductAggregate::apply`.
5. Add them to `ProductProjector::handles`.
6. Update or delete the read-model row in the projector.
7. Expose handlers that dispatch the new commands.

Never update `products_view` directly from a write endpoint; it is a projection, not the source of truth.
