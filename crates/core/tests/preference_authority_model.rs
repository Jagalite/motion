use playscale_core::playback_monitor::{ViewingAuthority, prefer_viewing};
use playscale_core::playback_session::{PreferenceApi, PreferenceInput, normalize_preferences};
use serde::{Deserialize, Serialize};
use stateless::{Check, Enumerate, Model, ModelError, ModelMetadata, Transition, TransitionRef};

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
struct Row {
    current: bool,
    time: i64,
    revision: i64,
    id: u8,
}
fn authority(r: &Row) -> ViewingAuthority<'_> {
    ViewingAuthority {
        current: r.current,
        updated_at: r.time,
        revision: r.revision,
        id: if r.id == 0 { "a" } else { "b" },
    }
}
struct Authority;
impl Model for Authority {
    type State = Row;
    type Input = Row;
    type Output = ();
    fn metadata(&self) -> ModelMetadata {
        ModelMetadata {
            name: "motion.monitor_authority".into(),
            model_version: 1,
            properties_version: 1,
            codec_version: 1,
            build: include_str!("../src/playback_monitor.rs").into(),
        }
    }
    fn initial_state(&self) -> Result<Row, ModelError> {
        Ok(Row {
            current: false,
            time: -1,
            revision: -1,
            id: 0,
        })
    }
    fn step(&self, s: &Row, i: &Row) -> Result<Transition<Row, ()>, ModelError> {
        Ok(Transition::accepted(
            if prefer_viewing(authority(i), authority(s)) {
                i.clone()
            } else {
                s.clone()
            },
            vec![],
        ))
    }
    fn check_state(&self, _: &Row) -> Result<Vec<Check>, ModelError> {
        Ok(vec![])
    }
    fn check_transition(
        &self,
        s: &Row,
        i: &Row,
        n: &TransitionRef<'_, Row, ()>,
    ) -> Result<Vec<Check>, ModelError> {
        // Independent ordered decision, not the production tuple comparator.
        let expected = if s.current != i.current {
            if i.current { i } else { s }
        } else if s.time != i.time {
            if i.time > s.time { i } else { s }
        } else if s.revision != i.revision {
            if i.revision > s.revision { i } else { s }
        } else if i.id > s.id {
            i
        } else {
            s
        };
        Ok(vec![if n.state == expected {
            Check::passed("authority_order")
        } else {
            Check::failed("authority_order", "incorrect winner")
        }])
    }
}
impl Enumerate for Authority {
    fn inputs(&self, _: &Row) -> Result<Vec<Row>, ModelError> {
        let mut rows = vec![];
        for current in [false, true] {
            for time in [-1, 0, 1] {
                for revision in [-1, 0, 1] {
                    for id in [0, 1] {
                        rows.push(Row {
                            current,
                            time,
                            revision,
                            id,
                        });
                    }
                }
            }
        }
        Ok(rows)
    }
}
#[test]
fn authoritative_session_precedes_timestamp_revision_and_id() {
    let report = stateless::explore::enumerate(
        &Authority,
        stateless::explore::SearchConfig {
            max_states: 100,
            max_transitions: 5000,
            max_depth: 5,
        },
    )
    .unwrap();
    assert!(report.failure.is_none(), "{:?}", report.failure);
    assert_eq!(report.skipped_checks, 0);
    assert_eq!(
        report.termination,
        stateless::explore::SearchTermination::GraphExhausted
    );
    println!(
        "monitor authority: {} states / {} transitions",
        report.states, report.transitions
    );
}
#[test]
fn preference_compatibility_and_canonical_replacement() {
    let good = vec!["EN-us".into(), "fr".into()];
    let duplicate = vec!["EN".into(), "en".into()];
    for api in [PreferenceApi::Legacy, PreferenceApi::V2] {
        for (languages, mode, completion, quality, cap, expected) in [
            (&good, "off", 90.0, "auto", None, true),
            (&duplicate, "off", 90.0, "auto", None, false),
            (
                &good,
                "forced",
                90.0,
                "auto",
                None,
                api == PreferenceApi::V2,
            ),
            (
                &good,
                "off",
                20.0,
                "auto",
                None,
                api == PreferenceApi::Legacy,
            ),
            (
                &good,
                "off",
                90.0,
                "original",
                Some(8_000_000),
                api == PreferenceApi::Legacy,
            ),
            (&good, "off", 90.0, "auto", Some(0), false),
            (
                &good,
                "off",
                90.0,
                "auto",
                Some(u64::MAX),
                api == PreferenceApi::Legacy,
            ),
        ] {
            let result = normalize_preferences(
                api,
                PreferenceInput {
                    subtitle_mode: mode,
                    quality,
                    conversion_recipe: "h264720p",
                    completion_percent: completion,
                    max_bitrate_bps: cap,
                    audio_languages: languages,
                    subtitle_languages: &[],
                },
            );
            assert_eq!(result.is_ok(), expected);
            if let Ok([audio, _]) = result {
                assert_eq!(audio, vec!["en-us", "fr"]);
            }
        }
    }
    assert!(playscale_core::revision::advance(i64::MAX, i64::MAX).is_err());
    assert!(playscale_core::revision::advance(1, 0).is_err());
}
