use std::path::PathBuf;

#[derive(
    serde::Serialize,
    serde::Deserialize,
    utoipa::ToSchema,
    poem_openapi::Object,
    salvo::oapi::ToSchema,
)]
struct Item {
    id: String,
    title: String,
    duration_seconds: Option<f64>,
    tags: Vec<String>,
}

fn item() -> Item {
    Item {
        id: "item-1".into(),
        title: "Example".into(),
        duration_seconds: None,
        tags: vec![],
    }
}

#[utoipa::path(get, path = "/api/v1/items", responses((status = 200, body = Item)))]
async fn axum_item() -> axum::Json<Item> {
    axum::Json(item())
}

struct PoemApi;
#[poem_openapi::OpenApi]
impl PoemApi {
    #[oai(path = "/api/v1/items", method = "get")]
    async fn get_item(&self) -> poem_openapi::payload::Json<Item> {
        poem_openapi::payload::Json(item())
    }
}

#[salvo::oapi::endpoint]
async fn salvo_item() -> salvo::prelude::Json<Item> {
    salvo::prelude::Json(item())
}

fn main() {
    let directory = PathBuf::from(std::env::args().nth(1).expect("output directory"));
    std::fs::create_dir_all(&directory).unwrap();
    let payloads = serde_json::json!({
        "serde_axum_salvo": serde_json::to_value(item()).unwrap(),
        "poem_object": poem_openapi::types::ToJSON::to_json(&item()).unwrap(),
    });
    std::fs::write(
        directory.join("payloads.json"),
        serde_json::to_string_pretty(&payloads).unwrap(),
    )
    .unwrap();
    let (_, axum_spec) = utoipa_axum::router::OpenApiRouter::<()>::new()
        .routes(utoipa_axum::routes!(axum_item))
        .split_for_parts();
    let poem_api = poem_openapi::OpenApiService::new(PoemApi, "Probe", "1");
    let salvo_router =
        salvo::Router::new().push(salvo::Router::with_path("api/v1/items").get(salvo_item));
    let salvo_spec = salvo::oapi::OpenApi::new("Probe", "1").merge_router(&salvo_router);
    for (name, text) in [
        ("axum", serde_json::to_string_pretty(&axum_spec).unwrap()),
        ("poem", poem_api.spec()),
        ("salvo", serde_json::to_string_pretty(&salvo_spec).unwrap()),
    ] {
        let spec: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert!(spec["paths"]["/api/v1/items"]["get"]["responses"]["200"].is_object());
        std::fs::write(directory.join(format!("{name}.json")), text).unwrap();
        println!(
            "{name}: OpenAPI {}, documented GET /api/v1/items",
            spec["openapi"]
        );
    }
}
