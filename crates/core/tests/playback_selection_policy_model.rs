use playscale_core::{
    delivery::Operation,
    playback_session::{self as core, SelectionConstraints},
};
use serde::{Deserialize, Serialize};
use stateless::{Check, Enumerate, Model, ModelError, ModelMetadata, Transition, TransitionRef};
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct Selection {
    operation: u8,
    flags: u8,
    cap: u8,
}
struct Selections;
fn operation(n: u8) -> Operation {
    match n {
        0 => Operation::Original,
        1 => Operation::Remux,
        _ => Operation::VideoTranscode,
    }
}
fn cap(n: u8) -> Option<u64> {
    match n {
        0 => None,
        1 => Some(8_000_000),
        2 => Some(0),
        _ => Some(u64::MAX),
    }
}
impl Model for Selections {
    type State = ();
    type Input = Selection;
    type Output = bool;
    fn metadata(&self) -> ModelMetadata {
        ModelMetadata {
            name: "motion.selection_composition".into(),
            model_version: 1,
            properties_version: 1,
            codec_version: 1,
            build: include_str!("../src/playback_session.rs").into(),
        }
    }
    fn initial_state(&self) -> Result<(), ModelError> {
        Ok(())
    }
    fn step(&self, _: &(), i: &Selection) -> Result<Transition<(), bool>, ModelError> {
        let f = i.flags;
        Ok(Transition::accepted(
            (),
            vec![
                core::validate_selection(SelectionConstraints {
                    operation: operation(i.operation),
                    interval: f & 1 != 0,
                    multipart: f & 2 != 0,
                    adaptive: f & 4 != 0,
                    adaptive_enabled: f & 8 != 0,
                    hardware: f & 16 != 0,
                    burned_subtitle: f & 32 != 0,
                    cap_bps: cap(i.cap),
                })
                .is_ok(),
            ],
        ))
    }
    fn check_state(&self, _: &()) -> Result<Vec<Check>, ModelError> {
        Ok(vec![])
    }
    fn check_transition(
        &self,
        _: &(),
        i: &Selection,
        n: &TransitionRef<'_, (), bool>,
    ) -> Result<Vec<Check>, ModelError> {
        let convert = i.operation == 2;
        let f = i.flags;
        let expected = match (f & 4 != 0, i.cap) {
            (true, 0) => convert && f & 8 != 0 && f & (2 | 16 | 32) == 0,
            (true, _) => false,
            (false, 0) => convert || f & 3 == 0,
            (false, 1) => convert,
            _ => false,
        };
        Ok(vec![if n.outputs[0] == expected {
            Check::passed("selection_admission_composition")
        } else {
            Check::failed("selection_admission_composition", "invalid composition")
        }])
    }
}
impl Enumerate for Selections {
    fn inputs(&self, _: &()) -> Result<Vec<Selection>, ModelError> {
        Ok((0..3)
            .flat_map(|operation| {
                (0..64).flat_map(move |flags| {
                    (0..4).map(move |cap| Selection {
                        operation,
                        flags,
                        cap,
                    })
                })
            })
            .collect())
    }
}
#[test]
fn planning_admission_and_replan_share_selection_constraints() {
    let r = stateless::explore::enumerate(
        &Selections,
        stateless::explore::SearchConfig {
            max_states: 2,
            max_transitions: 1000,
            max_depth: 2,
        },
    )
    .unwrap();
    assert!(r.failure.is_none(), "{:?}", r.failure);
    assert_eq!(r.skipped_checks, 0);
    assert_eq!(
        r.termination,
        stateless::explore::SearchTermination::GraphExhausted
    );
    println!(
        "selection policy: {} states / {} transitions",
        r.states, r.transitions
    );
}
#[test]
fn browser_observations_and_track_identity_are_not_assumed_supported() {
    use playscale_core::playback::{Support, browser_support};
    let videos = vec!["avc1".into()];
    let audios = vec!["mp4a".into()];
    for (video, audio, hdr, sdr, hdr_client, container, expected) in [
        (
            Some("h264"),
            Some("aac"),
            false,
            false,
            false,
            Some("mp4"),
            Support::Supported,
        ),
        (
            Some("h264"),
            None,
            false,
            false,
            false,
            Some("webm"),
            Support::Supported,
        ),
        (
            Some("h264"),
            Some("ac3"),
            false,
            false,
            false,
            Some("mp4"),
            Support::Unsupported,
        ),
        (
            Some("mpeg2video"),
            None,
            false,
            false,
            false,
            Some("mp4"),
            Support::Unsupported,
        ),
        (
            Some("hevc"),
            None,
            false,
            false,
            false,
            Some("mp4"),
            Support::Unknown,
        ),
        (
            Some("h264"),
            None,
            true,
            false,
            false,
            Some("mp4"),
            Support::Unsupported,
        ),
        (
            Some("h264"),
            None,
            true,
            true,
            true,
            Some("mp4"),
            Support::Unsupported,
        ),
        (
            Some("h264"),
            None,
            true,
            false,
            true,
            Some("mp4"),
            Support::Supported,
        ),
        (None, None, false, false, false, None, Support::Unknown),
        (
            Some("h264"),
            None,
            false,
            false,
            false,
            Some("mkv"),
            Support::Unknown,
        ),
    ] {
        assert_eq!(
            browser_support(
                video, audio, hdr, sdr, hdr_client, container, &videos, &audios
            ),
            expected
        );
    }
    assert_eq!(
        core::compose_tracks(true, false, None, None, false, None, None, Some(0)),
        Err(core::TrackCompositionError::UnavailableComponent)
    );
    assert_eq!(
        core::compose_tracks(true, false, Some(1), None, false, Some(0), None, Some(0)),
        Err(core::TrackCompositionError::Conflict)
    );
    assert_eq!(
        core::compose_tracks(false, true, None, None, true, None, Some(0), Some(0)),
        Err(core::TrackCompositionError::Conflict)
    );
    assert_eq!(
        core::compose_tracks(
            true,
            true,
            Some(1),
            Some(2),
            false,
            Some(1),
            Some(2),
            Some(0)
        ),
        Ok((Some(1), Some(2)))
    );
}
#[test]
fn subtitle_backend_and_provenance_are_revalidated() {
    use playscale_core::delivery::{self as d, BurnSelection, SidecarAdmission};
    assert!(!d::backend_supported(Operation::Original, true, true));
    assert!(!d::backend_supported(
        Operation::VideoTranscode,
        true,
        false
    ));
    assert!(d::burn_selection_supported(
        Operation::VideoTranscode,
        BurnSelection::Text { attachments: 2 },
        "ass",
        true,
        2
    ));
    assert!(!d::burn_selection_supported(
        Operation::VideoTranscode,
        BurnSelection::Text { attachments: 1 },
        "ass",
        true,
        2
    ));
    assert!(!d::burn_selection_supported(
        Operation::Original,
        BurnSelection::Bitmap,
        "dvd_subtitle",
        true,
        0
    ));
    assert_eq!(
        d::admit_sidecar("x.srt", "r", None),
        SidecarAdmission::Changed
    );
    assert_eq!(
        d::admit_sidecar("../x.srt", "r", Some("srt")),
        SidecarAdmission::InvalidIdentity
    );
    assert_eq!(
        d::admit_sidecar("x.ass", "r", Some("ass")),
        SidecarAdmission::Unsupported
    );
    use playscale_core::playback::{LegacyProvenance, local_output_contract};
    for mask in 0..32 {
        let p = LegacyProvenance {
            registration_source: if mask & 1 == 0 {
                "playscale"
            } else {
                "imported"
            },
            external_id: "job",
            file: "out",
            file_revision: "r",
            source_file: "src",
            source_revision: "s",
            job_id: if mask & 2 == 0 { "job" } else { "other" },
            job_source_file: "src",
            job_source_revision: if mask & 4 == 0 { "s" } else { "new" },
            phase: if mask & 8 == 0 {
                "completed"
            } else {
                "running"
            },
            expired: mask & 16 != 0,
        };
        assert_eq!(local_output_contract(&p, "out", "r", "src", "s"), mask == 0);
    }
}

#[test]
fn legacy_profile_match_keeps_codec_in_the_contract() {
    let h264 = serde_json::json!({"version":1,"codec":"h264","max_width":1280});
    let hevc = serde_json::json!({"version":1,"codec":"hevc","max_width":1280});
    assert!(!playscale_core::playback::legacy_recipe_matches(
        Some("video_profile"),
        Some(&hevc),
        "video_profile",
        Some(&h264)
    ));
    assert!(playscale_core::playback::legacy_recipe_matches(
        Some("video_profile"),
        Some(&h264),
        "video_profile",
        Some(&h264)
    ));
}

struct MultipartBurns;
impl Model for MultipartBurns {
    type State = ();
    type Input = (u8, u8, u8, u8);
    type Output = bool;
    fn metadata(&self) -> ModelMetadata {
        ModelMetadata {
            name: "motion.multipart_burn_layout".into(),
            model_version: 1,
            properties_version: 1,
            codec_version: 1,
            build: include_str!("../src/parts.rs").into(),
        }
    }
    fn initial_state(&self) -> Result<(), ModelError> {
        Ok(())
    }
    fn step(&self, _: &(), i: &Self::Input) -> Result<Transition<(), bool>, ModelError> {
        use playscale_core::parts::{BurnLayout, PartPin, burn_layout};
        let selected = match i.0 {
            0 => BurnLayout::Off,
            1 => BurnLayout::Bitmap(0),
            _ => BurnLayout::Text,
        };
        let parts: Vec<_> = [i.1, i.2]
            .into_iter()
            .enumerate()
            .map(|(n, burn)| PartPin {
                file: n.to_string(),
                revision: "r".into(),
                duration_ms: 100,
                audio: None,
                subtitle: None,
                burn: match burn {
                    0 => None,
                    1 => Some(0),
                    _ => Some(1),
                },
            })
            .collect();
        let codec = match i.3 {
            0 => None,
            1 => Some("dvd_subtitle"),
            _ => Some("ass"),
        };
        Ok(Transition::accepted(
            (),
            vec![burn_layout(&parts, selected, codec)],
        ))
    }
    fn check_state(&self, _: &()) -> Result<Vec<Check>, ModelError> {
        Ok(vec![])
    }
    fn check_transition(
        &self,
        _: &(),
        i: &Self::Input,
        n: &TransitionRef<'_, (), bool>,
    ) -> Result<Vec<Check>, ModelError> {
        let expected = match i.0 {
            0 => i.1 == 0 && i.2 == 0,
            1 => i.1 == 1 && i.2 != 0 && i.3 == 1,
            _ => false,
        };
        Ok(vec![if n.outputs[0] == expected {
            Check::passed("every_part_has_exact_burn_kind")
        } else {
            Check::failed("every_part_has_exact_burn_kind", "invalid multipart burn")
        }])
    }
}
impl Enumerate for MultipartBurns {
    fn inputs(&self, _: &()) -> Result<Vec<Self::Input>, ModelError> {
        Ok((0..3)
            .flat_map(|a| {
                (0..3)
                    .flat_map(move |b| (0..3).flat_map(move |c| (0..3).map(move |d| (a, b, c, d))))
            })
            .collect())
    }
}
#[test]
fn multipart_burn_shape_is_an_admission_prerequisite() {
    let r = stateless::explore::enumerate(
        &MultipartBurns,
        stateless::explore::SearchConfig {
            max_states: 2,
            max_transitions: 100,
            max_depth: 2,
        },
    )
    .unwrap();
    assert!(r.failure.is_none(), "{:?}", r.failure);
    assert_eq!(r.skipped_checks, 0);
    assert_eq!(
        r.termination,
        stateless::explore::SearchTermination::GraphExhausted
    );
    println!(
        "multipart burn: {} states / {} transitions",
        r.states, r.transitions
    );
}
