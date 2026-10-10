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

/// Process-lifetime count of busy errors returned through the public adapter.
/// It measures observed request failures, not SQLite's internal retry attempts.
pub(crate) static DB_BUSY_COUNT: std::sync::atomic::AtomicU64 =
    std::sync::atomic::AtomicU64::new(0);

pub async fn diagnostics(State(app): State<App>, caller: Caller) -> Result<Response, Problem> {
    caller.require(playscale_core::access::Permission::SystemAdmin)?;
    let mut tx = app.db.begin().await?;
    caller
        .reauthorize(
            &mut tx,
            Some(playscale_core::access::Permission::SystemAdmin),
        )
        .await?;
    let (queued,running):(i64,i64)=sqlx::query_as("SELECT coalesce(sum(phase='queued'),0),coalesce(sum(phase IN ('admitted','running','validating','publishing','cancelling')),0) FROM (SELECT phase FROM jobs UNION ALL SELECT phase FROM processing_jobs)")
        .fetch_one(&mut *tx).await?;
    // Worker error text can contain physical paths and tool output. Report
    // stable failure identities, never raw subprocess messages.
    let errors:Vec<String>=sqlx::query_scalar("SELECT kind || ':' || id || ':' || phase FROM (SELECT 'scan' AS kind,id,phase,created_at AS changed FROM jobs WHERE phase='failed' UNION ALL SELECT 'processing',id,phase,updated_at FROM processing_jobs WHERE phase='failed') ORDER BY changed DESC,id LIMIT 100")
        .fetch_all(&mut *tx).await?;
    tx.commit().await?;
    Ok(Json(serde_json::json!({
        "server_epoch":app.access.server_epoch,
        "uptime_seconds":app.health.started.elapsed().as_secs().to_string(),
        "active_deliveries":app.processing.deliveries.active_sessions().to_string(),
        "queued_jobs":queued.to_string(),"running_jobs":running.to_string(),
        "db_busy_count":DB_BUSY_COUNT.load(Ordering::Relaxed).to_string(),
        "worker_errors":errors
    }))
    .into_response())
}
