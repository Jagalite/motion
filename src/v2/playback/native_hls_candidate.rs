//! Private acceptance entry point only. No route, signed HLS planner result or
//! capability advertisement calls this until end-to-end acceptance qualifies it.
use super::*;
use crate::delivery::{AdmissionAuthority, native_hls};
use std::sync::Mutex;

struct Authority {
    plan: PlanAuthority,
    source: native_hls::Slot,
}
impl AdmissionAuthority for Authority {
    fn reauthorize<'a>(
        &'a self,
        db: &'a mut sqlx::SqliteConnection,
        request: &'a delivery::CreateRequest,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<String, ApiError>> + Send + 'a>>
    {
        self.plan.reauthorize(db, request)
    }
    fn prepare_new<'a>(
        &'a self,
        db: &'a mut sqlx::SqliteConnection,
        request: &'a delivery::CreateRequest,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), ApiError>> + Send + 'a>>
    {
        Box::pin(async move {
            let principal = self
                .plan
                .caller
                .reauthorize(db, None)
                .await
                .map_err(|p| self.plan.refuse(p))?;
            let input = native_hls::open_in(
                &self.plan.app,
                db,
                &principal,
                owner_of(&self.plan.claims),
                self.plan.claims.version.clone(),
            )
            .await?;
            // Opening exact native authority can await slow filesystem work.
            // Recheck the real admission credential, including time expiry,
            // before the prepared input becomes eligible for dispatch.
            self.plan.reauthorize(db, request).await?;
            *self.source.lock().unwrap() = Some(input);
            Ok(())
        })
    }
    fn fresh(&self) -> Result<(), ApiError> {
        self.plan.fresh()
    }
}

/// A still-valid, genuinely signed whole-byte plan supplies identity and expiry,
/// not source paths. Conversion is private and always software-only, SDR,
/// single-file MOV/MP4, no subtitles/edits/multipart/copy/adaptive/hardware.
#[allow(dead_code)]
pub(crate) async fn admit(
    app: App,
    caller: Caller,
    token: &str,
    key: String,
    start_ms: u64,
) -> Result<delivery::DeliveryView, Problem> {
    let mut claims = open(&app, token)?;
    native_replan_cohort(&claims)?;
    if claims.principal != caller.principal.id {
        return Err(Problem::not_found());
    }
    let native = app.native_catalog.as_ref().ok_or_else(Problem::not_found)?;
    native
        .rebuild_projection()
        .await
        .map_err(Problem::internal)?;
    claims.route = Route::Transcode;
    let owner = owner_of(&claims);
    let source: native_hls::Slot = Arc::new(Mutex::new(None));
    let request = delivery::CreateRequest {
        file_id: claims.file.clone(),
        file_revision: claims.file_revision.clone(),
        start_ms,
        audio_track: claims.audio,
        subtitle_track: None,
        operation: None,
        backend: None,
    };
    let planned = delivery::Planned {
        native_input: Some(source.clone()),
        owner,
        version: claims.version.clone(),
        route: Route::Transcode,
        network: "unknown",
        sidecar: None,
        burn: None,
        clip: None,
        parts: vec![],
        max_bitrate_bps: None,
        adaptive: false,
    };
    let authority = Arc::new(Authority {
        plan: PlanAuthority {
            app: app.clone(),
            caller,
            claims,
            refusal: Default::default(),
        },
        source,
    });
    delivery::admit_planned(app, authority.clone(), key, request, planned)
        .await
        .map_err(|error| {
            authority
                .plan
                .refusal
                .lock()
                .unwrap()
                .take()
                .unwrap_or_else(|| problem(error))
        })
}

#[cfg(test)]
#[path = "native_hls_candidate_tests.rs"]
mod tests;
