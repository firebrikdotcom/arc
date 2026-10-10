# Application Workflows

This page follows real data through a generated Arc application.

## 1. Simple incoming request

Request:

```http
GET /health HTTP/1.1
Host: 127.0.0.1:8080
```

Lifecycle:

```text
Actix server
  → global Arc middleware
  → routes::health
  → JSON response
```

Response:

```http
HTTP/1.1 200 OK
content-type: application/json

{"status":"healthy","application":"my-app","version":"0.1.0"}
```

This path is ordinary Actix routing. It does not use event sourcing because it does not change or query domain state.

## 2. State-changing request

Consider:

```http
POST /api/products HTTP/1.1
content-type: application/json

{"id":"product-1","name":"Notebook"}
```

The controller accepts transport data:

```rust
#[derive(Deserialize)]
struct CreateProduct {
    id: String,
    name: String,
}
```

This struct is a request data-transfer object. It describes incoming JSON; it is not the domain model.

The controller converts that data into domain intent:

```rust
let command = ProductCommand::Create {
    id: body.id.clone(),
    name: body.name.clone(),
};
```

It dispatches through Arc:

```rust
bus.dispatch(
    command,
    CommandContext::for_actor("anonymous"),
).await
```

Full lifecycle:

```text
JSON request
  → Actix deserializes CreateProduct
  → controller creates ProductCommand
  → CommandBus loads events for product-1
  → ProductAggregate::apply rebuilds current state
  → ProductAggregate::handle validates Create
  → handle returns ProductCreated
  → CommandBus stamps audit metadata
  → SQLite event store appends ProductCreated
  → in-process event bus publishes ProductCreated
  → ProductProjector writes products_view
  → controller returns 201 Created
```

The aggregate is the write model. It protects invariants and produces facts. It is not an ORM row and should not be serialized directly as an HTTP response.

## 3. Query request

Request:

```http
GET /api/products/product-1 HTTP/1.1
```

The controller queries the read model:

> **Note:** [`web::Data`](project-structure.md#actix-shared-application-data) gives this route access to Arc's shared read-model store.

```rust
async fn get_product(
    id: web::Path<String>,
    store: web::Data<dyn ReadModelStore>,
) -> impl Responder {
    match store.get("products_view", id.as_str()).await {
        Ok(Some(product)) => HttpResponse::Ok().json(product),
        Ok(None) => HttpResponse::NotFound().finish(),
        Err(error) => HttpResponse::InternalServerError()
            .json(serde_json::json!({
                "error": error.to_string()
            })),
    }
}
```

Lifecycle:

```text
GET request
  → Actix extracts product-1 from the path
  → controller asks ReadModelStore for products_view/product-1
  → SQLite reads the projected JSON row
  → controller serializes it as JSON
```

The query does not rebuild the aggregate. Reads use projections optimized for the response being built.

## 4. Domain model versus data model

Arc separates several kinds of data:

| Kind | Example | Responsibility |
|---|---|---|
| Request data | `CreateProduct` | Parse and validate HTTP input shape |
| Command | `ProductCommand::Create` | Express domain intent |
| Aggregate | `ProductAggregate` | Enforce rules using reconstructed state |
| Event | `ProductCreated` | Persist an immutable accepted fact |
| Projection row | `products_view` JSON | Answer queries and build views |
| Response data | JSON or Tera context | Present data to the caller |

Do not use one struct for every layer. Their reasons to change are different.

## 5. Controller lifecycle

A controller should:

1. Extract path, query, form, or JSON input.
2. Perform transport-level validation.
3. Create a command for a write, or query a read model for a read.
4. Translate success or failure into an HTTP response.

A controller should not:

- update event-store tables directly;
- update projection tables during a write;
- contain aggregate business rules;
- render an aggregate as though it were a database record.

## 6. View lifecycle

For a server-rendered Product page:

```text
GET /products/product-1
  → controller extracts product-1
  → controller queries products_view
  → controller inserts product into Tera Context
  → Tera renders resources/views/product.html
  → controller returns text/html
```

Controller:

> **Note:** The controller receives Arc's shared read-model store through [`web::Data`](project-structure.md#actix-shared-application-data).

```rust
#[get("/products/{id}")]
async fn show_product(
    id: web::Path<String>,
    store: web::Data<dyn ReadModelStore>,
) -> impl Responder {
    let product = match store
        .get("products_view", id.as_str())
        .await
    {
        Ok(Some(product)) => product,
        Ok(None) => return HttpResponse::NotFound().finish(),
        Err(error) => {
            return HttpResponse::InternalServerError()
                .body(error.to_string());
        }
    };

    let mut context = tera::Context::new();
    context.insert("product", &product);

    match tera::Tera::one_off(
        include_str!("../resources/views/product.html"),
        &context,
        true,
    ) {
        Ok(html) => HttpResponse::Ok()
            .content_type("text/html; charset=utf-8")
            .body(html),
        Err(error) => HttpResponse::InternalServerError()
            .body(error.to_string()),
    }
}
```

View:

```html
<!doctype html>
<html lang="en">
<body>
  <main>
    <h1>{{ product.name }}</h1>
    <p>Product ID: {{ product.id }}</p>
  </main>
</body>
</html>
```

The view receives projection data, not the write-model aggregate.

## 7. HTML form lifecycle

Form:

```html
<form method="post" action="/products">
  <label>
    ID
    <input name="id" required>
  </label>
  <label>
    Name
    <input name="name" required>
  </label>
  <button type="submit">Create product</button>
</form>
```

The handler uses Actix's `web::Form`:

> **Note:** [`web::Data`](project-structure.md#actix-shared-application-data) gives this route access to the shared Product command bus.

```rust
#[derive(serde::Deserialize)]
struct ProductForm {
    id: String,
    name: String,
}

#[post("/products")]
async fn create_product_form(
    form: web::Form<ProductForm>,
    bus: web::Data<CommandBus<ProductAggregate>>,
) -> impl Responder {
    let command = ProductCommand::Create {
        id: form.id.clone(),
        name: form.name.clone(),
    };

    match bus
        .dispatch(
            command,
            CommandContext::for_actor("anonymous"),
        )
        .await
    {
        Ok(_) => HttpResponse::SeeOther()
            .insert_header((
                actix_web::http::header::LOCATION,
                format!("/products/{}", form.id),
            ))
            .finish(),
        Err(error) => HttpResponse::UnprocessableEntity()
            .body(error.to_string()),
    }
}
```

Lifecycle:

```text
Browser submits application/x-www-form-urlencoded
  → Actix creates ProductForm
  → controller creates ProductCommand
  → CommandBus runs the aggregate/event/projection workflow
  → controller returns 303 See Other
  → browser follows Location
  → GET controller reads products_view
  → Tera renders the Product page
```

This is Post/Redirect/Get: refreshing the resulting page does not resubmit the form.

## Form security boundary

The `--ui` starter does not currently generate CSRF tokens or authentication. Before exposing state-changing cookie-authenticated forms:

1. add a supported CSRF mechanism;
2. reject missing or invalid tokens;
3. derive `CommandContext` from the authenticated actor;
4. do not use `"anonymous"` for authenticated writes.

Session middleware being present does not make a form authenticated or CSRF-safe.

## Where to go next

- [Add an Endpoint](endpoints.md) for basic Actix routing.
- [Build a Resource](resources.md) for the complete Product implementation.
- [Add a UI Page](ui.md) for Tera rendering.
