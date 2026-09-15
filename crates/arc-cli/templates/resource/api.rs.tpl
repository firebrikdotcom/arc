use super::aggregate::{{Type}}Aggregate;
use super::commands::{{Type}}Command;
use super::projector::{{CONSTANT}}_VIEW;
use actix_web::{delete, get, post, put, web, HttpResponse, Responder};
use arc_core::command_bus::{CommandBus, CommandContext};
use arc_core::read_model_store::{CollectionQuery, ReadModelStore};
use serde::Deserialize;
use serde_json::json;
{{api-auth-import}}{{api-role-import}}
#[derive(Deserialize)]
struct Create{{Type}} {
    id: String,
    name: String,
}

#[derive(Deserialize)]
struct Update{{Type}} {
    name: String,
}

#[post("/{{view}}")]
async fn create_{{module}}(
    body: web::Json<Create{{Type}}>,
    bus: web::Data<CommandBus<{{Type}}Aggregate>>,
) -> impl Responder {
    let command = {{Type}}Command::Create {
        id: body.id.clone(),
        name: body.name.clone(),
    };
    match bus
        .dispatch(command, CommandContext::for_actor("anonymous"))
        .await
    {
        Ok(_) => HttpResponse::Created().json(json!({
            "id": body.id.clone(),
            "name": body.name.clone(),
        })),
        Err(error) => HttpResponse::BadRequest().json(json!({ "error": error.to_string() })),
    }
}

#[derive(Deserialize)]
struct CollectionParams { limit: Option<u64>, offset: Option<u64> }

#[get("/{{view}}")]
async fn list_{{view}}(store: web::Data<dyn ReadModelStore>, params: web::Query<CollectionParams>) -> impl Responder {
    let query = match CollectionQuery::new(params.limit.unwrap_or(20), params.offset.unwrap_or(0)) {
        Ok(query) => query,
        Err(_) => return HttpResponse::BadRequest().json(json!({ "error": "invalid collection window" })),
    };
    match store.collection({{CONSTANT}}_VIEW, &query).await {
        Ok(page) => HttpResponse::Ok().insert_header(("X-Has-Next", page.has_next.to_string())).json(page.rows),
        Err(error) => {
            HttpResponse::InternalServerError().json(json!({ "error": error.to_string() }))
        }
    }
}

#[get("/{{view}}/{id}")]
async fn get_{{module}}(
    id: web::Path<String>,
    store: web::Data<dyn ReadModelStore>,
) -> impl Responder {
    match store.get({{CONSTANT}}_VIEW, id.as_str()).await {
        Ok(Some(row)) => HttpResponse::Ok().json(row),
        Ok(None) => HttpResponse::NotFound().finish(),
        Err(error) => {
            HttpResponse::InternalServerError().json(json!({ "error": error.to_string() }))
        }
    }
}

#[put("/{{view}}/{id}")]
async fn update_{{module}}(
    id: web::Path<String>,
    body: web::Json<Update{{Type}}>,
    bus: web::Data<CommandBus<{{Type}}Aggregate>>,
) -> impl Responder {
    let command = {{Type}}Command::Rename {
        id: id.into_inner(),
        name: body.name.clone(),
    };
    match bus
        .dispatch(command, CommandContext::for_actor("anonymous"))
        .await
    {
        Ok(_) => HttpResponse::Ok().json(json!({ "name": body.name.clone() })),
        Err(error) => HttpResponse::BadRequest().json(json!({ "error": error.to_string() })),
    }
}

#[delete("/{{view}}/{id}")]
async fn delete_{{module}}(
    id: web::Path<String>,
    bus: web::Data<CommandBus<{{Type}}Aggregate>>,
) -> impl Responder {
    match bus
        .dispatch(
            {{Type}}Command::Delete {
                id: id.into_inner(),
            },
            CommandContext::for_actor("anonymous"),
        )
        .await
    {
        Ok(_) => HttpResponse::NoContent().finish(),
        Err(error) => HttpResponse::BadRequest().json(json!({ "error": error.to_string() })),
    }
}

pub fn config(cfg: &mut web::ServiceConfig) {
    cfg.service(
        web::scope(""){{api-role-wrap}}{{api-auth-wrap}}
            .service(create_{{module}})
            .service(list_{{view}})
            .service(get_{{module}})
            .service(update_{{module}})
            .service(delete_{{module}}),
    );
}
