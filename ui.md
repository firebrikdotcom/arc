# Add a UI Page

Create the project with UI support:

```bash
arc new my-app --ui
```

The generated app serves Tera-rendered HTML at `/` and static files under `/public/*`.

## Add a page

Create `resources/views/about.html`:

```html
<!doctype html>
<html lang="en">
<head>
  <meta charset="utf-8">
  <meta name="viewport" content="width=device-width, initial-scale=1">
  <title>About {{ app_name }}</title>
  <link rel="stylesheet" href="/public/styles.css">
</head>
<body>
  <main>
    <p class="eyebrow">About</p>
    <h1>{{ app_name }}</h1>
    <p>This page is rendered by Tera.</p>
  </main>
</body>
</html>
```

Add a handler to `src/ui.rs`:

```rust
#[get("/about")]
pub async fn about() -> impl Responder {
    let template = include_str!("../resources/views/about.html");
    let mut context = Context::new();
    context.insert("app_name", env!("CARGO_PKG_NAME"));

    match Tera::one_off(template, &context, true) {
        Ok(html) => HttpResponse::Ok()
            .content_type("text/html; charset=utf-8")
            .body(html),
        Err(error) => HttpResponse::InternalServerError()
            .body(format!("template error: {error}")),
    }
}
```

Register it in `src/routes.rs`:

```rust
cfg.service(health)
    .service(crate::ui::home)
    .service(crate::ui::about)
    .service(actix_files::Files::new("/public", "public"));
```

Open <http://127.0.0.1:8080/about>.

## Current UI scope

The `--ui` starter provides Tera and static-file serving only. It does not generate forms, CSRF integration, authentication, Tailwind, Stimulus, Turbo, Vite, or the repository's admin screens.
