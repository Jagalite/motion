//! The native adapter's bounded cohort gate, before any external probe/effect.
use super::*;

#[test]
fn native_replan_only_accepts_whole_byte_claims() {
    let baseline = Claims {
        principal: "principal".into(),
        profile: "profile".into(),
        timeline: "timeline".into(),
        version: "version".into(),
        file: "file".into(),
        file_revision: "revision".into(),
        route: Route::Original,
        audio: None,
        subtitle: None,
        clip: None,
        parts: vec![],
        max_bitrate_bps: None,
        adaptive: false,
        expires_at: 1,
    };
    for route in [Route::Original, Route::Prepared] {
        let mut claims = baseline.clone();
        claims.route = route;
        assert!(native_replan_cohort(&claims).is_ok());
    }
    let mut rejected = Vec::new();
    for route in [
        Route::Remux,
        Route::AudioConvert,
        Route::Transcode,
        Route::HardwareTranscode,
    ] {
        let mut claims = baseline.clone();
        claims.route = route;
        rejected.push(claims);
    }
    for subtitle in [
        core::Subtitle::Text(0),
        core::Subtitle::Bitmap(0),
        core::Subtitle::BurnedText {
            index: 0,
            attachments: 0,
        },
        core::Subtitle::Sidecar {
            name: "captions.vtt".into(),
            fingerprint: "revision".into(),
        },
    ] {
        let mut claims = baseline.clone();
        claims.subtitle = Some(subtitle);
        rejected.push(claims);
    }
    let mut claims = baseline.clone();
    claims.clip = Some(playscale_core::parts::Clip {
        start_ms: 0,
        end_ms: 100,
    });
    rejected.push(claims);
    let mut claims = baseline.clone();
    claims.parts.push(playscale_core::parts::PartPin {
        file: "file".into(),
        revision: "revision".into(),
        duration_ms: 100,
        audio: None,
        subtitle: None,
        burn: None,
    });
    rejected.push(claims);
    let mut claims = baseline.clone();
    claims.max_bitrate_bps = Some(1000);
    rejected.push(claims);
    let mut claims = baseline;
    claims.adaptive = true;
    rejected.push(claims);
    for claims in rejected {
        let problem = native_replan_cohort(&claims).unwrap_err();
        assert_eq!(problem.status, StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(problem.code, "native_replan_unavailable");
    }
}
