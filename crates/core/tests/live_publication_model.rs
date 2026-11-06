use playscale_core::delivery::publication::{
    Decision, MAX_SEGMENT_BYTES, Policy, Publication, Segment,
};
use serde::{Deserialize, Serialize};
use stateless::{Check, Enumerate, Model, ModelError, ModelMetadata, Transition, TransitionRef};
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
struct State {
    main: Publication,
    low: Publication,
    main_bytes: Vec<u64>,
    low_bytes: Vec<u64>,
}
#[derive(Clone, Debug, PartialEq, Eq)]
enum Receipt {
    Qualified(Decision),
    Finished(bool),
    Begun(bool),
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
enum Input {
    Observe { low: bool, kind: u8 },
    Finish { low: bool },
    Gap { low: bool },
}
struct ModelCase {
    adaptive: bool,
    copied: bool,
    cap: bool,
}
impl Model for ModelCase {
    type State = State;
    type Input = Input;
    type Output = Receipt;
    fn metadata(&self) -> ModelMetadata {
        ModelMetadata {
            name: format!(
                "motion.live_publication.{}{}{}",
                self.adaptive, self.copied, self.cap
            ),
            model_version: 1,
            properties_version: 1,
            codec_version: 1,
            build: include_str!("../src/delivery/publication.rs").into(),
        }
    }
    fn initial_state(&self) -> Result<State, ModelError> {
        Ok(State::default())
    }
    fn step(&self, s: &State, i: &Input) -> Result<Transition<State, Receipt>, ModelError> {
        let mut n = s.clone();
        let mut outputs = vec![];
        match *i {
            Input::Observe { low, kind } => {
                let index = if low { n.low.verified } else { n.main.verified };
                let policy = Policy {
                    target_seconds: 2,
                    cap_bps: self.cap.then_some(1_000_000),
                    copied_video: self.copied && !low,
                    expected_start_us: Some(1_000),
                    requested_start_ms: 50,
                };
                let segment = Segment {
                    index,
                    bytes: match kind {
                        1 => MAX_SEGMENT_BYTES + 1,
                        2 => 200_000,
                        _ => 100_000,
                    },
                    duration_ms: if kind == 3 { 0 } else { 1000 },
                    independent: kind != 4,
                    start_us: Some(if kind == 5 { 1_002 } else { 1_000 }),
                };
                let decision = if low {
                    n.low.qualify(policy, segment, None)
                } else {
                    n.main
                        .qualify(policy, segment, self.adaptive.then_some(&n.low))
                };
                if matches!(decision, Decision::Ready { .. } | Decision::Segment { .. }) {
                    if low {
                        n.low_bytes.push(segment.bytes);
                    } else {
                        n.main_bytes.push(segment.bytes);
                    }
                }
                outputs.push(Receipt::Qualified(decision));
            }
            Input::Finish { low } => {
                let finished = if low {
                    n.low.finish(true, n.low.verified.checked_sub(1), None)
                } else {
                    n.main.finish(
                        true,
                        n.main.verified.checked_sub(1),
                        self.adaptive.then_some(&n.low),
                    )
                };
                outputs.push(Receipt::Finished(finished));
            }
            Input::Gap { low } => {
                let p = if low { &mut n.low } else { &mut n.main };
                outputs.push(Receipt::Begun(p.begin(Some(p.verified + 1))));
            }
        }
        Ok(Transition::accepted(n, outputs))
    }
    fn check_state(&self, s: &State) -> Result<Vec<Check>, ModelError> {
        Ok(vec![
            if !self.adaptive || s.main.verified <= s.low.verified {
                Check::passed("paired_publication")
            } else {
                Check::failed("paired_publication", "main ahead")
            },
            if !s.main.ended || !self.adaptive || s.low.ended && s.low.verified == s.main.verified {
                Check::passed("paired_completion")
            } else {
                Check::failed("paired_completion", "unpaired end")
            },
        ])
    }
    fn check_transition(
        &self,
        s: &State,
        i: &Input,
        n: &TransitionRef<'_, State, Receipt>,
    ) -> Result<Vec<Check>, ModelError> {
        let mut valid = true;
        if let Input::Observe { low, kind } = *i {
            let before = if low { &s.low } else { &s.main };
            let after = if low { &n.state.low } else { &n.state.main };
            let ready = !before.failed
                && !before.ended
                && (low || !self.adaptive || !s.low.failed && before.verified < s.low.verified);
            let bad = kind == 1
                || kind == 3
                || kind == 4
                || kind == 2 && self.cap
                || kind == 5 && self.copied && !low && before.verified == 0;
            let expected = if !ready {
                if before.failed {
                    Decision::Failed
                } else {
                    Decision::Wait
                }
            } else if bad {
                Decision::Failed
            } else {
                // Independent suffix-window oracle; the core retains/trims
                // its own window, while this history never evicts anything.
                let mut history = if low {
                    s.low_bytes.clone()
                } else {
                    s.main_bytes.clone()
                };
                history.push(if kind == 2 { 200_000 } else { 100_000 });
                let peak = (1..=history.len().min(3))
                    .map(|count| {
                        history[history.len() - count..].iter().sum::<u64>() * 8 / count as u64
                    })
                    .max()
                    .unwrap();
                if before.verified == 0 {
                    Decision::Ready {
                        media_time_origin_ms: if self.copied && !low { 0 } else { 50 },
                        first_segment_start_ms: if self.copied && !low { 1 } else { 0 },
                        peak_bps: peak,
                    }
                } else {
                    Decision::Segment { peak_bps: peak }
                }
            };
            valid = n.outputs == [Receipt::Qualified(expected)]
                && if !ready {
                    after == before
                } else if bad {
                    after.failed && after.verified == before.verified
                } else {
                    after.verified == before.verified + 1 && !after.failed
                };
        } else if let Input::Finish { low } = *i {
            let before = if low { &s.low } else { &s.main };
            let expected = !before.failed
                && before.verified > 0
                && (low
                    || !self.adaptive
                    || !s.low.failed && s.low.ended && s.low.verified == before.verified);
            valid = n.outputs == [Receipt::Finished(expected)];
        } else if let Input::Gap { low } = *i {
            let before = if low { &s.low } else { &s.main };
            valid = n.outputs == [Receipt::Begun(before.ended)];
        }
        Ok(vec![if valid {
            Check::passed("qualification_before_publication")
        } else {
            Check::failed("qualification_before_publication", "invalid qualification")
        }])
    }
}
impl Enumerate for ModelCase {
    fn inputs(&self, s: &State) -> Result<Vec<Input>, ModelError> {
        let mut v = vec![];
        for low in [false, true] {
            if low && !self.adaptive {
                continue;
            }
            let p = if low { &s.low } else { &s.main };
            if p.verified < 4 && !p.failed && !p.ended {
                for kind in 0..6 {
                    v.push(Input::Observe { low, kind });
                }
                v.push(Input::Gap { low });
            }
            v.push(Input::Finish { low });
        }
        Ok(v)
    }
}
#[test]
fn live_qualification_covers_adaptive_capped_and_copied_output() {
    for model in [
        ModelCase {
            adaptive: false,
            copied: false,
            cap: false,
        },
        ModelCase {
            adaptive: false,
            copied: true,
            cap: true,
        },
        ModelCase {
            adaptive: true,
            copied: false,
            cap: true,
        },
    ] {
        let r = stateless::explore::enumerate(
            &model,
            stateless::explore::SearchConfig {
                max_states: 10000,
                max_transitions: 100000,
                max_depth: 20,
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
            "publication adaptive={} copied={} cap={}: {} states / {} transitions",
            model.adaptive, model.copied, model.cap, r.states, r.transitions
        );
    }
}

#[test]
fn short_segments_combine_into_an_over_cap_window_without_partial_publication() {
    let mut p = Publication::default();
    let policy = Policy {
        target_seconds: 6,
        cap_bps: Some(1_000_000),
        copied_video: false,
        expected_start_us: None,
        requested_start_ms: 123,
    };
    assert_eq!(
        p.qualify(
            policy,
            Segment {
                index: 0,
                bytes: 200_000,
                duration_ms: 2000,
                independent: true,
                start_us: None
            },
            None
        ),
        Decision::Ready {
            media_time_origin_ms: 123,
            first_segment_start_ms: 0,
            peak_bps: 0
        }
    );
    // Neither 2-second fragment alone qualifies (minimum window is 3s),
    // but the joined 4-second window is 1.2 Mb/s and must not publish.
    assert_eq!(
        p.qualify(
            policy,
            Segment {
                index: 1,
                bytes: 400_000,
                duration_ms: 2000,
                independent: true,
                start_us: None
            },
            None
        ),
        Decision::Failed
    );
    assert_eq!(p.verified, 1);
    assert!(!p.finish(true, Some(1), None));
}

#[test]
fn copied_start_and_fragment_bounds_keep_exact_boundary_contracts() {
    let policy = Policy {
        target_seconds: 6,
        cap_bps: None,
        copied_video: true,
        expected_start_us: Some(1000),
        requested_start_ms: 9999,
    };
    for actual in [
        None,
        Some(998),
        Some(999),
        Some(1000),
        Some(1001),
        Some(1002),
    ] {
        let mut p = Publication::default();
        let output = p.qualify(
            policy,
            Segment {
                index: 0,
                bytes: 100,
                duration_ms: 4000,
                independent: true,
                start_us: actual,
            },
            None,
        );
        let expected = match actual {
            Some(999..=1001) => Decision::Ready {
                media_time_origin_ms: 0,
                first_segment_start_ms: actual.unwrap() / 1000,
                peak_bps: 200,
            },
            _ => Decision::Failed,
        };
        assert_eq!(output, expected);
    }
    for (bytes, duration, valid) in [
        (MAX_SEGMENT_BYTES, 6499, true),
        (MAX_SEGMENT_BYTES + 1, 6499, false),
        (100, 6500, false),
        (100, 0, false),
    ] {
        let mut p = Publication::default();
        let output = p.qualify(
            policy,
            Segment {
                index: 0,
                bytes,
                duration_ms: duration,
                independent: true,
                start_us: Some(1000),
            },
            None,
        );
        assert_eq!(matches!(output, Decision::Ready { .. }), valid);
        assert_eq!(p.verified, u32::from(valid));
    }
}
