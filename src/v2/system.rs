//! Contract, liveness and capability reads.
use super::{Problem, auth::Caller};
use crate::App;
use axum::{
    Json,
    extract::State,
    http::{HeaderValue, header},
    response::{IntoResponse, Response},
};
use playscale_core::access::AccessMode;
use serde::Serialize;
use std::{collections::BTreeMap, sync::atomic::Ordering};

/// The reviewed contract, served verbatim. The JSON form is generated from
/// the YAML by `scripts/contract_json.rb`; a test checks they stay in step.
pub const CONTRACT_YAML: &[u8] = include_bytes!("../../contracts/Motion_Server_API_v2.yaml");
pub const CONTRACT_JSON: &str = include_str!("../../contracts/Motion_Server_API_v2.json");

pub fn contract_digest() -> String {
    format!("sha256:{}", super::sha256(CONTRACT_YAML))
}

pub async fn openapi() -> Response {
    let mut response = CONTRACT_JSON.into_response();
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/json"),
    );
    response
}

pub async fn server_identity(app: &App) -> Result<(String, String), Problem> {
    Ok(
        sqlx::query_as("SELECT server_id,restore_epoch FROM server_identity WHERE singleton=1")
            .fetch_one(&app.db)
            .await?,
    )
}

#[derive(Serialize)]
pub struct Health {
    status: &'static str,
    server_id: String,
    server_epoch: String,
}

pub async fn health(State(app): State<App>) -> Result<Json<Health>, Problem> {
    let (server_id, _) = server_identity(&app).await?;
    let status = if app.health.shutting_down.load(Ordering::Relaxed) {
        "draining"
    } else if !app.health.worker_running.load(Ordering::Relaxed) {
        "degraded"
    } else {
        "ok"
    };
    Ok(Json(Health {
        status,
        server_id,
        server_epoch: app.access.server_epoch.clone(),
    }))
}

#[derive(Serialize)]
struct Feature {
    id: &'static str,
    implemented: bool,
    enabled: bool,
    qualification: &'static str,
    limits: BTreeMap<&'static str, String>,
    receipt_ids: Vec<String>,
}

fn feature(id: &'static str, implemented: bool, enabled: bool) -> Feature {
    Feature {
        id,
        implemented,
        enabled,
        qualification: "unqualified",
        limits: BTreeMap::new(),
        receipt_ids: vec![],
    }
}

#[derive(Serialize)]
pub struct Capabilities {
    server_id: String,
    server_epoch: String,
    server_version: &'static str,
    api_version: &'static str,
    schema_version: String,
    contract_digest: String,
    features: Vec<Feature>,
    runtime_hashes: BTreeMap<&'static str, String>,
}

pub async fn capabilities(
    State(app): State<App>,
    _caller: Caller,
) -> Result<Json<Capabilities>, Problem> {
    let (server_id, _) = server_identity(&app).await?;
    let schema: i64 =
        sqlx::query_scalar("SELECT coalesce(max(version),0) FROM _sqlx_migrations WHERE success=1")
            .fetch_one(&app.db)
            .await?;
    let restricted = app.access.mode == AccessMode::Restricted;
    let mut api = feature("api.v2.identity", true, true);
    api.limits = BTreeMap::from([
        ("json_body_bytes", super::BODY_LIMIT.to_string()),
        ("page_limit_max", super::PAGE_MAX.to_string()),
        (
            "idempotency_retention_seconds",
            playscale_core::access::IDEMPOTENCY_RETENTION_SECONDS.to_string(),
        ),
    ]);
    let features = vec![
        api,
        feature("identity.pairing", true, true),
        feature("identity.browser_sessions", true, true),
        feature(
            "identity.trusted_private_ingress",
            true,
            app.access.settings.trusted_ingress.is_some(),
        ),
        // Household mode leaves the legacy v1 surface open: v2 grants are
        // enforced on v2 routes but profile restrictions are not enforceable.
        feature("access.restricted_mode", true, restricted),
        feature("events.scoped_stream", true, true),
        feature("catalog.v2", false, false),
        feature("playback.v2", false, false),
    ];
    let mut runtime_hashes = BTreeMap::new();
    if let Some(hash) = app
        .access
        .server_hash
        .get_or_init(|| async {
            let exe = std::env::current_exe().ok()?;
            tokio::task::spawn_blocking(move || std::fs::read(exe).ok())
                .await
                .ok()
                .flatten()
                .map(|bytes| format!("sha256:{}", super::sha256(&bytes)))
        })
        .await
    {
        runtime_hashes.insert("server", hash.clone());
    }
    Ok(Json(Capabilities {
        server_id,
        server_epoch: app.access.server_epoch.clone(),
        server_version: env!("CARGO_PKG_VERSION"),
        api_version: "2.0.0",
        schema_version: schema.to_string(),
        contract_digest: contract_digest(),
        features,
        runtime_hashes,
    }))
}
