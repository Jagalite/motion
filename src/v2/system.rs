//! Contract, liveness and capability reads.
use super::{Problem, auth::Caller};
use crate::App;
use axum::{
    Json,
    extract::State,
    http::{HeaderValue, header},
    response::{IntoResponse, Response},
};
use serde::Serialize;
use std::{collections::BTreeMap, sync::atomic::Ordering};

mod native_diagnostics;

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
    restore_epoch: String,
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
    let (server_id, restore_epoch) = server_identity(&app).await?;
    let schema: i64 =
        sqlx::query_scalar("SELECT coalesce(max(version),0) FROM _sqlx_migrations WHERE success=1")
            .fetch_one(&app.db)
            .await?;
    let mut api = feature("api.v2.identity", true, true);
    api.limits = BTreeMap::from([
        ("json_body_bytes", super::BODY_LIMIT.to_string()),
        ("page_limit_max", super::PAGE_MAX.to_string()),
        (
            "idempotency_retention_seconds",
            playscale_core::access::IDEMPOTENCY_RETENTION_SECONDS.to_string(),
        ),
    ]);
    let filters = crate::processing::filters(&app).await;
    let tone_mapping = filters.zscale && filters.tonemap;
    let mut playback = feature("playback.v2", true, true);
    playback.limits = BTreeMap::from([
        (
            "routes",
            // Live routes are advertised only where the server admits them.
            {
                let mut routes = vec!["original", "prepared"];
                if app.processing.settings.experimental_copy_routes {
                    routes.push("stream_copy_hls");
                }
                routes.push("software_hls");
                if crate::delivery::hardware_live(&app).await
                    != crate::delivery::HardwareState::Unavailable
                {
                    routes.push("hardware_hls");
                }
                routes.join(",")
            },
        ),
        // Intervals and multipart versions are played by conversion only.
        ("timeline_binding", "whole,interval,multipart".into()),
        (
            "subtitles",
            {
                let mut modes = vec![
                    "text_webvtt_on_original:embedded_or_srt_vtt_sidecar",
                    "text_hls_rendition_on_live_routes_for_hls_text_clients",
                    "bitmap_burned_in_conversion",
                ];
                // Only where this server's FFmpeg has libass.
                if filters.subtitles {
                    modes.push("embedded_text_and_styled_burned_in_conversion_with_font_attachments");
                }
                modes.push("automatic_by_profile_language_and_role");
                modes.join(",")
            },
        ),
        ("components", "embedded_and_sidecar_revision_bound,rendition_audio".into()),
        ("bitrate_caps", "conversion_vbv_peak_measured_per_segment".into()),
        ("tickets", "file_or_delivery_generation_playback".into()),
        (
            "adaptive_bitrate",
            if app.processing.settings.experimental_adaptive_bitrate {
                "software_conversion:720p_3000000,360p_800000"
            } else {
                "disabled"
            }
            .into(),
        ),
        (
            "hdr",
            // Tone mapping is advertised only when this server's FFmpeg has
            // the filters; the source facts still decide per title.
            if tone_mapping {
                "preserve_original_for_hdr_clients,tone_map_to_sdr_on_single_file_conversions:pq,hlg,dolby_vision_7_8_with_hdr10_or_hlg_base"
            } else {
                "preserve_original_for_hdr_clients"
            }
            .into(),
        ),
        (
            "unsupported",
            {
                let mut unsupported = vec![];
                if !filters.subtitles {
                    unsupported.extend(["styled_subtitles", "text_burn_in"]);
                }
                unsupported.extend(["sidecar_burn_in", "subtitles_on_multipart", "prepared_track_pins"]);
                if tone_mapping {
                    unsupported.extend(["hdr_conversion_on_multipart", "hdr_preparation"]);
                } else {
                    unsupported.push("hdr_conversion");
                }
                unsupported.push("cast");
                unsupported.join(",")
            },
        ),
        ("playhead", "heartbeat_ordered_observation".into()),
        ("viewing_restart", "recoverable".into()),
    ]);
    let mut sessions = feature("admin.playback_sessions", true, true);
    sessions.limits = BTreeMap::from([(
        "history_retention_days",
        crate::playback_monitor::RETENTION_DAYS.to_string(),
    )]);
    let mut metrics = feature("admin.metrics", true, true);
    metrics.limits = BTreeMap::from([
        ("interval_seconds", "10".into()),
        ("retention_seconds", "86400".into()),
    ]);
    let (oidc_enabled,email_enabled):(bool,bool)=sqlx::query_as("SELECT EXISTS(SELECT 1 FROM signin_providers WHERE enabled=1) AND s.reviewed_epoch=x.restore_epoch AND s.public_origin IS NOT NULL,coalesce(json_extract(s.email_json,'$.enabled'),0) AND s.reviewed_epoch=x.restore_epoch AND s.public_origin IS NOT NULL FROM signin_settings s CROSS JOIN server_identity x").fetch_one(&app.db).await?;
    let mut features = vec![
        api,
        feature("identity.pairing", true, true),
        feature("identity.access_management", true, true),
        feature("identity.invitations", true, true),
        feature("identity.browser_sessions", true, true),
        feature("identity.oidc", true, oidc_enabled),
        feature("identity.email_codes", true, email_enabled),
        feature("identity.action_proofs", true, true),
        feature(
            "identity.trusted_private_ingress",
            true,
            app.access.settings.trusted_ingress.is_some(),
        ),
        // Every live route enforces v2 grants after retirement; the household
        // viewer is itself a narrow v2 grant, never a bypass.
        feature("access.restricted_mode", true, true),
        feature(
            "access.household_viewer",
            true,
            app.access.mode == playscale_core::access::AccessMode::TrustedHousehold,
        ),
        feature("events.scoped_stream", true, true),
        feature("catalog.v2", true, true),
        playback,
        metrics,
        sessions,
    ];
    if app.native_catalog.is_some() {
        // The native backend never advertises legacy route implementations as
        // available. Full catalog.v2 remains off while richer adapters land.
        for feature in &mut features {
            if !matches!(
                feature.id,
                "api.v2.identity"
                    | "identity.browser_sessions"
                    | "identity.oidc"
                    | "identity.email_codes"
                    | "identity.action_proofs"
                    | "identity.trusted_private_ingress"
                    | "access.restricted_mode"
                    | "access.household_viewer"
            ) {
                feature.enabled = false;
            }
        }
        // playback.v2 is the existing player-launch negotiation contract, not
        // a claim that every legacy presentation family is native-admitted.
        // Replace (do not merge) the legacy limits so config cannot advertise
        // HLS/components/subtitle routes which the outer router still refuses.
        let playback = features.iter_mut().find(|f| f.id == "playback.v2").unwrap();
        playback.enabled = true;
        playback.limits = BTreeMap::from([
            ("routes", "original,prepared".into()),
            ("timeline_binding", "whole".into()),
            ("subtitles", "off".into()),
            ("components", "unsupported".into()),
            ("adaptive_bitrate", "disabled".into()),
            ("hdr", "original_passthrough_for_compatible_clients".into()),
            ("tickets", "file_or_delivery_generation_playback".into()),
            ("playhead", "heartbeat_ordered_observation".into()),
            ("viewing_restart", "recoverable".into()),
            ("controls", "seek_whole_original".into()),
            (
                "admission",
                "current_principal_scope_source_and_client_compatibility".into(),
            ),
            (
                "unsupported",
                "replan,hls,interval,multipart,components,subtitles,adaptive_bitrate,cast".into(),
            ),
        ]);
        let mut native = feature("catalog.native_plain_works", true, true);
        native.limits.insert(
            "scope",
            "unrestricted principals; qualified root no-file works only".into(),
        );
        native.limits.insert(
            "serving_contract",
            crate::native_catalog::SERVING_CONTRACT_VERSION.to_string(),
        );
        native
            .limits
            .insert("operations", "create,get,browse,search,replace".into());
        native.limits.insert(
            "unsupported",
            crate::native_catalog::unsupported_serving_routes().join(","),
        );
        features.push(native);
        let mut sources = feature("catalog.native_source_bindings", true, true);
        sources.limits.insert(
            "operations",
            "create,get,list,delete,explicit_scans,scan_status,scan_cancel,scan_schedules".into(),
        );
        features.push(sources);
        let mut diagnostics = feature("admin.native_diagnostics", true, true);
        diagnostics.limits = BTreeMap::from([
            ("operations", "GET /admin/diagnostics".into()),
            ("authorization", "current_system_admin".into()),
            (
                "workload",
                "recorded_native_scan_demands_and_processing_admissions".into(),
            ),
            ("failure_history_max", "100".into()),
            (
                "authority",
                "observations_only_not_execution_or_catalog_grants".into(),
            ),
        ]);
        features.push(diagnostics);
        let mut auth_reads = feature("admin.native_auth_reads", true, true);
        auth_reads.limits = BTreeMap::from([
            ("method", "GET".into()),
            (
                "routes",
                super::admin_reads::AUTH_ROUTES
                    .iter()
                    .copied()
                    .chain(["/admin/auth/users/{id}"])
                    .collect::<Vec<_>>()
                    .join(","),
            ),
            ("authorization", "current_system_admin".into()),
            ("mail_activity_detail", "stable_presence_marker_only".into()),
            (
                "authority",
                "descriptive_application_state_not_library_or_access_grants".into(),
            ),
            (
                "unsupported_operations",
                "mutations,provider_tests,email_tests,email_diagnostics,restore_review_mutation"
                    .into(),
            ),
        ]);
        features.push(auth_reads);
        features.push(feature("profiles.preferences", true, true));
        let mut jobs = feature("processing.native_job_reads", true, true);
        jobs.limits = BTreeMap::from([
            ("operations", "list,get".into()),
            ("kinds", "rendition".into()),
            ("visibility", "current_admin_or_requester".into()),
            ("state", "recorded_phase_and_attempt".into()),
            (
                "results",
                "historical_ids_not_catalog_or_byte_authority".into(),
            ),
            ("unsupported_operations", "cancel,retry".into()),
            ("unsupported_kinds", "scan,metadata,backup,preview".into()),
        ]);
        features.push(jobs);
        let mut controls = feature("processing.native_preparation_controls", true, true);
        controls.limits = BTreeMap::from([
            ("contract_version", "1".into()),
            ("operations", "cancel,retry".into()),
            ("cohort", "native_preparation".into()),
            (
                "authorization",
                "current_owner_or_admin_and_processing_request".into(),
            ),
            (
                "cancellation",
                "cooperative_not_confirmed_termination".into(),
            ),
            (
                "retry",
                "reviewed_original_preparation_current_admission".into(),
            ),
            (
                "hints",
                "per_row_new_requests_not_replay_or_authority".into(),
            ),
            (
                "unsupported_kinds",
                "legacy_history,scan,metadata,backup,preview".into(),
            ),
        ]);
        features.push(controls);
    }
    let mut runtime_hashes = BTreeMap::new();
    if let Some(hash) = app
        .access
        .server_hash
        .get_or_init(|| async {
            let exe = std::env::current_exe().ok()?;
            // Reading and hashing a large executable must both stay off the
            // async worker. The once-cell retains the same process-lifetime
            // artifact identity for subsequent capabilities requests.
            tokio::task::spawn_blocking(move || {
                std::fs::read(exe)
                    .ok()
                    .map(|bytes| format!("sha256:{}", super::sha256(&bytes)))
            })
            .await
            .ok()
            .flatten()
        })
        .await
    {
        runtime_hashes.insert("server", hash.clone());
    }
    Ok(Json(Capabilities {
        server_id,
        server_epoch: app.access.server_epoch.clone(),
        restore_epoch,
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
    // Process measurements are optional: when they cannot be taken (the
    // reader is busy or the platform call failed) the rest of diagnostics
    // still answers, with `process: null`, rather than failing as a whole.
    let process = match app.health.metrics.snapshot().await {
        Ok(process) => Some(process),
        Err(code) => {
            tracing::warn!(code, "Process measurements are temporarily unavailable");
            None
        }
    };
    let mut tx = app.db.begin().await?;
    caller
        .reauthorize(
            &mut tx,
            Some(playscale_core::access::Permission::SystemAdmin),
        )
        .await?;
    let (queued, running, errors) = if app.native_catalog.is_some() {
        native_diagnostics::workload_in(&mut tx).await?
    } else {
        let (queued,running):(i64,i64)=sqlx::query_as("SELECT coalesce(sum(phase='queued'),0),coalesce(sum(phase IN ('admitted','running','validating','publishing','cancelling')),0) FROM (SELECT phase FROM jobs UNION ALL SELECT phase FROM processing_jobs)")
        .fetch_one(&mut *tx).await?;
        // Worker error text can contain physical paths and tool output. Report
        // stable failure identities, never raw subprocess messages.
        let errors:Vec<String>=sqlx::query_scalar("SELECT kind || ':' || id || ':' || phase FROM (SELECT 'scan' AS kind,id,phase,created_at AS changed FROM jobs WHERE phase='failed' UNION ALL SELECT 'processing',id,phase,updated_at FROM processing_jobs WHERE phase='failed') ORDER BY changed DESC,id LIMIT 100")
        .fetch_all(&mut *tx).await?;
        (queued, running, errors)
    };
    tx.commit().await?;
    Ok(Json(serde_json::json!({
        "server_epoch":app.access.server_epoch,
        "uptime_seconds":app.health.started.elapsed().as_secs().to_string(),
        "active_deliveries":app.processing.deliveries.active_sessions().to_string(),
        "queued_jobs":queued.to_string(),"running_jobs":running.to_string(),
        "db_busy_count":DB_BUSY_COUNT.load(Ordering::Relaxed).to_string(),
        "worker_errors":errors,
        "process":process
    }))
    .into_response())
}

pub async fn metrics(
    State(app): State<App>,
    caller: Caller,
    query: Result<
        axum::extract::Query<crate::metrics::history::HistoryQuery>,
        axum::extract::rejection::QueryRejection,
    >,
) -> Result<Response, Problem> {
    use playscale_core::access::Permission::SystemAdmin;
    caller.require(SystemAdmin)?;
    let query = query
        .map_err(|_| Problem::bad("invalid_metrics_query", "Invalid metrics query"))?
        .0;
    let now = crate::now();
    let window = query.validate(now).map_err(|_| {
        Problem::bad(
            "invalid_metrics_query",
            "since must be RFC 3339 and step_seconds an integer of at least 10",
        )
    })?;
    let mut tx = app.db.begin().await?;
    caller.reauthorize(&mut tx, Some(SystemAdmin)).await?;
    tx.commit().await?;
    let history = crate::metrics::history::read(&app, window, now)
        .await
        .map_err(|_| {
            tracing::warn!(
                code = "metrics_read_failed",
                "Metrics history is temporarily unavailable"
            );
            Problem::new(
                axum::http::StatusCode::SERVICE_UNAVAILABLE,
                "metrics_read_failed",
                "Metrics history is temporarily unavailable",
            )
        })?;
    // Read the credential again after the potentially longer history read,
    // without keeping an application DB transaction open across that work.
    let mut tx = app.db.begin().await?;
    caller.reauthorize(&mut tx, Some(SystemAdmin)).await?;
    tx.commit().await?;
    Ok(Json(history).into_response())
}
