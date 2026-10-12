//! Route selection across a client's failure reports and the lazy observation
//! of stream-copy details. A configuration fixes the request and source facts;
//! the client then plans, the adapter observes copy details when asked, and
//! the client reports offered candidates as failed. Expected outcomes are
//! stated in the checks from the configuration and the ghost history; the step
//! function only calls the production `playback_session::plan`. Prepared
//! renditions are covered by unit tests and are not part of this model.
use playscale_core::playback::{Mode, Support};
use playscale_core::playback_session::{self as core, Hardware, Plan, Route, StreamCopy};
use serde::{Deserialize, Serialize};
use stateless::{
    Check, Enumerate, Model, ModelCodec, ModelError, ModelMetadata, Transition, TransitionRef,
};

const FILE: &str = "f";

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
enum ModeFact {
    Auto,
    Convert,
    Original,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
struct Config {
    mode: ModeFact,
    /// The client reports decoding the original (otherwise: unsupported).
    original_supported: bool,
    /// A non-default stream was chosen: no byte route can honor it.
    explicit_streams: bool,
    /// The selected subtitle must be burned into the video.
    burn_in: bool,
    /// The timeline is an interval of the original file.
    clipped: bool,
    /// The timeline plays several files in order.
    multipart: bool,
    hls: bool,
    /// The source fits the client's limits (copy keeps the source's size).
    within_budget: bool,
    transcodable: bool,
    /// The copy route named by stream facts, if any.
    copy: Option<Route>,
    /// Ground truth the observation reveals: bitstream details qualify the copy.
    details_qualify: bool,
    /// The observed hardware encoder state.
    hardware: HardwareFact,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
enum HardwareFact {
    Unavailable,
    Busy,
    Available,
}
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
enum Outcome {
    Ready(Route),
    Observe,
    Blocked(String),
}
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
struct State {
    config: Option<Config>,
    /// What the adapter knows about the copy route now.
    copy: Option<Observed>,
    failed: Vec<Route>,
    last: Option<Outcome>,
    probes: u8,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
enum Observed {
    Unobserved,
    Eligible,
    Ineligible,
}
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
enum Input {
    Configure(Config),
    Plan,
    /// The adapter observes the source's bitstream details.
    Probe,
    /// The client reports the last offered candidate failed.
    Fail,
}

fn facts(c: &Config, copy: Option<Observed>) -> core::SourceFacts {
    core::SourceFacts {
        available: true,
        client_support: if c.original_supported {
            Support::Supported
        } else {
            Support::Unsupported
        },
        within_budget: c.within_budget,
        transcodable: c.transcodable,
        // Conversion limits are unit-tested; here a conversion always fits,
        // so only the copy is bounded by the source's own limits.
        conversion_within_budget: true,
        copy: match (c.copy, copy) {
            (Some(route), Some(Observed::Unobserved)) => StreamCopy::Unobserved(route),
            (Some(route), Some(Observed::Eligible)) => StreamCopy::Eligible(route),
            _ => StreamCopy::Ineligible,
        },
        hardware: match c.hardware {
            HardwareFact::Unavailable => Hardware::Unavailable,
            HardwareFact::Busy => Hardware::Busy,
            HardwareFact::Available => Hardware::Available,
        },
        clipped: c.clipped,
        multipart: c.multipart,
        prepared: vec![],
    }
}
fn request(c: &Config, failed: &[Route]) -> core::Request {
    core::Request {
        mode: match c.mode {
            ModeFact::Auto => Mode::Auto,
            ModeFact::Convert => Mode::Convert,
            ModeFact::Original => Mode::Original,
        },
        range: true,
        hls: c.hls,
        explicit_streams: c.explicit_streams,
        pinned_prepared: false,
        burn_in: c.burn_in,
        failed: failed.iter().map(|r| r.candidate_id(FILE)).collect(),
    }
}

struct Selection;
impl Model for Selection {
    type State = State;
    type Input = Input;
    type Output = Outcome;
    fn metadata(&self) -> ModelMetadata {
        ModelMetadata {
            name: "playscale.route-selection".into(),
            model_version: 5,
            properties_version: 5,
            codec_version: 1,
            build: include_str!("../src/playback_session.rs").into(),
        }
    }
    fn initial_state(&self) -> Result<State, ModelError> {
        Ok(State {
            config: None,
            copy: None,
            failed: vec![],
            last: None,
            probes: 0,
        })
    }
    fn step(&self, s: &State, input: &Input) -> Result<Transition<State, Outcome>, ModelError> {
        let mut n = s.clone();
        let mut out = vec![];
        match input {
            Input::Configure(c) => {
                n.config = Some(*c);
                n.copy = c.copy.map(|_| Observed::Unobserved);
            }
            Input::Plan => {
                let c = s
                    .config
                    .as_ref()
                    .ok_or_else(|| ModelError::new("unconfigured"))?;
                let outcome = match core::plan(FILE, &facts(c, s.copy), &request(c, &s.failed)) {
                    Plan::Ready(route) => Outcome::Ready(route),
                    Plan::Observe => Outcome::Observe,
                    Plan::Blocked(reason) => Outcome::Blocked(reason.into()),
                    Plan::Prepared(_) => return Err(ModelError::new("no renditions modelled")),
                };
                n.last = Some(outcome.clone());
                out.push(outcome);
            }
            Input::Probe => {
                let c = s
                    .config
                    .as_ref()
                    .ok_or_else(|| ModelError::new("unconfigured"))?;
                n.copy = Some(if c.details_qualify {
                    Observed::Eligible
                } else {
                    Observed::Ineligible
                });
                n.probes += 1;
                n.last = None;
            }
            Input::Fail => {
                if let Some(Outcome::Ready(route)) = &s.last {
                    n.failed.push(*route);
                }
                n.last = None;
            }
        }
        Ok(Transition::accepted(n, out))
    }
    fn check_state(&self, s: &State) -> Result<Vec<Check>, ModelError> {
        let mut unique = s.failed.clone();
        unique.sort_by_key(|r| r.candidate_id(FILE));
        unique.dedup();
        Ok(vec![
            check(
                "a_failure_is_recorded_once_per_candidate",
                unique.len() == s.failed.len(),
            ),
            check("details_are_observed_at_most_once", s.probes <= 1),
        ])
    }
    fn check_transition(
        &self,
        s: &State,
        i: &Input,
        t: &TransitionRef<'_, State, Outcome>,
    ) -> Result<Vec<Check>, ModelError> {
        let (Input::Plan, Some(c)) = (i, s.config.as_ref()) else {
            return Ok(vec![]);
        };
        let outcome = &t.outputs[0];
        let failed = |r: Route| s.failed.contains(&r);
        // Stated independently of the production rules.
        let original_usable = c.mode != ModeFact::Convert
            && !c.explicit_streams
            && !c.burn_in
            && !c.clipped
            && !c.multipart
            && c.original_supported
            && c.within_budget
            && !failed(Route::Original);
        let copy_open = c.mode == ModeFact::Auto
            && !c.burn_in
            && !c.clipped
            && !c.multipart
            && c.hls
            && c.within_budget
            && c.copy.is_some_and(|r| !failed(r));
        let expected = if original_usable {
            Outcome::Ready(Route::Original)
        } else if c.mode == ModeFact::Original {
            Outcome::Blocked(
                if c.clipped {
                    "interval_requires_conversion"
                } else if c.multipart {
                    "multipart_requires_conversion"
                } else if c.burn_in {
                    "subtitle_requires_conversion"
                } else if c.explicit_streams {
                    "stream_selection_requires_conversion"
                } else {
                    "original_unavailable_or_unsupported"
                }
                .into(),
            )
        } else if !c.hls {
            Outcome::Blocked("client_cannot_play_conversion".into())
        } else if copy_open && s.copy == Some(Observed::Unobserved) {
            Outcome::Observe
        } else if copy_open && s.copy == Some(Observed::Eligible) {
            Outcome::Ready(c.copy.unwrap())
        } else if !c.transcodable {
            Outcome::Blocked("no_live_route".into())
        } else if c.hardware == HardwareFact::Available && !failed(Route::HardwareTranscode) {
            Outcome::Ready(Route::HardwareTranscode)
        } else if failed(Route::Transcode) {
            Outcome::Blocked("all_candidates_failed".into())
        } else {
            Outcome::Ready(Route::Transcode)
        };
        let copy = matches!(outcome, Outcome::Ready(Route::Remux | Route::AudioConvert));
        Ok(vec![
            check(
                "plan_matches_the_stated_preference_order",
                *outcome == expected,
            ),
            check(
                "a_failed_candidate_is_never_offered_again",
                !matches!(outcome, Outcome::Ready(r) if failed(*r)),
            ),
            check(
                "a_copy_is_offered_only_in_auto_with_hls_within_limits_and_observed_qualified_details",
                !copy
                    || (c.mode == ModeFact::Auto
                        && c.hls
                        && c.within_budget
                        && c.details_qualify
                        && s.copy == Some(Observed::Eligible)
                        && matches!(outcome, Outcome::Ready(r) if Some(*r) == c.copy)),
            ),
            check(
                "conversion_waits_for_an_open_copy_to_be_observed_or_fail",
                !(matches!(outcome, Outcome::Ready(Route::Transcode)) && copy_open)
                    || s.copy == Some(Observed::Ineligible),
            ),
            check(
                "software_conversion_only_without_an_available_unfailed_hardware_session",
                !matches!(outcome, Outcome::Ready(Route::Transcode))
                    || c.hardware != HardwareFact::Available
                    || failed(Route::HardwareTranscode),
            ),
            check(
                "hardware_only_when_observed_available",
                !matches!(outcome, Outcome::Ready(Route::HardwareTranscode))
                    || c.hardware == HardwareFact::Available,
            ),
            check(
                "burned_subtitles_only_on_a_conversion",
                !c.burn_in
                    || matches!(
                        outcome,
                        Outcome::Ready(Route::Transcode | Route::HardwareTranscode)
                            | Outcome::Blocked(_)
                    ),
            ),
            check(
                "an_interval_or_several_parts_are_only_ever_converted",
                !(c.clipped || c.multipart)
                    || matches!(
                        outcome,
                        Outcome::Ready(Route::Transcode | Route::HardwareTranscode)
                            | Outcome::Blocked(_)
                    ),
            ),
            check(
                "strict_original_never_streams_live",
                c.mode != ModeFact::Original
                    || matches!(
                        outcome,
                        Outcome::Ready(Route::Original) | Outcome::Blocked(_)
                    ),
            ),
        ])
    }
}
fn check(id: &'static str, ok: bool) -> Check {
    if ok {
        Check::passed(id)
    } else {
        Check::failed(id, "route selection contract violated")
    }
}
impl Enumerate for Selection {
    fn inputs(&self, s: &State) -> Result<Vec<Input>, ModelError> {
        let Some(_) = s.config else {
            let mut configs = vec![];
            for mode in [ModeFact::Auto, ModeFact::Convert, ModeFact::Original] {
                for copy in [None, Some(Route::Remux), Some(Route::AudioConvert)] {
                    for hardware in [
                        HardwareFact::Unavailable,
                        HardwareFact::Busy,
                        HardwareFact::Available,
                    ] {
                        for bits in 0..512u16 {
                            let bit = |i: u16| bits & (1 << i) != 0;
                            // Details only matter when a copy is named.
                            if copy.is_none() && bit(5) {
                                continue;
                            }
                            configs.push(Input::Configure(Config {
                                mode,
                                original_supported: bit(0),
                                explicit_streams: bit(1),
                                hls: bit(2),
                                within_budget: bit(3),
                                transcodable: bit(4),
                                copy,
                                details_qualify: bit(5),
                                burn_in: bit(6),
                                clipped: bit(7),
                                multipart: bit(8),
                                hardware,
                            }));
                        }
                    }
                }
            }
            return Ok(configs);
        };
        let mut inputs = vec![Input::Plan];
        // The adapter observes only when the plan asked for it.
        if s.last == Some(Outcome::Observe) {
            inputs.push(Input::Probe);
        }
        if let Some(Outcome::Ready(route)) = &s.last
            && !s.failed.contains(route)
        {
            inputs.push(Input::Fail);
        }
        Ok(inputs)
    }
}
impl ModelCodec for Selection {
    fn encode_state(&self, v: &State) -> Result<Vec<u8>, ModelError> {
        encode(v)
    }
    fn decode_state(&self, b: &[u8]) -> Result<State, ModelError> {
        decode(b)
    }
    fn encode_input(&self, v: &Input) -> Result<Vec<u8>, ModelError> {
        encode(v)
    }
    fn decode_input(&self, b: &[u8]) -> Result<Input, ModelError> {
        decode(b)
    }
    fn encode_output(&self, v: &Outcome) -> Result<Vec<u8>, ModelError> {
        encode(v)
    }
}
fn encode<T: Serialize>(v: &T) -> Result<Vec<u8>, ModelError> {
    serde_json::to_vec(v).map_err(|e| ModelError::new(e.to_string()))
}
fn decode<T: serde::de::DeserializeOwned>(b: &[u8]) -> Result<T, ModelError> {
    serde_json::from_slice(b).map_err(|e| ModelError::new(e.to_string()))
}

#[test]
fn route_selection_prefers_qualified_copies_observes_lazily_and_never_reoffers_failures() {
    let report = stateless::explore::enumerate(
        &Selection,
        stateless::explore::SearchConfig {
            max_states: 1_000_000,
            max_transitions: 10_000_000,
            max_depth: 64,
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
        "route-selection model: {} states, {} transitions; three modes, remux/audio-convert/no \
         copy, qualified or unqualified details, unavailable/busy/available hardware, burned \
         subtitles, intervals, multipart, HLS, limits, conversion capability, explicit streams and client support, with repeated \
         failure reports",
        report.states, report.transitions
    );
}
