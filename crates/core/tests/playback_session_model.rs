//! Plan tokens, delivery ownership and replans under permission revocation,
//! profile narrowing, library revocation, file moves, source changes, expiry,
//! incompatible replan contexts and foreign principals. Expected outcomes are
//! stated independently in the checks; the step function only calls the
//! production decisions.
use playscale_core::access::{FileFacts, Grant, Mode as AccessMode, Permission, Policy, Principal};
use playscale_core::playback_session::{self as core, Claims, Observed, Owner, PlanError, Route};
use serde::{Deserialize, Serialize};
use stateless::{
    Check, Enumerate, Model, ModelCodec, ModelError, ModelMetadata, Transition, TransitionRef,
};

const TTL: i64 = 2;

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
struct Grants {
    playback: bool,
    viewing: bool,
    profile: bool,
    library: bool,
}
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
struct State {
    now: i64,
    /// Principal "a"'s current grant; "b" always has full grants.
    a: Grants,
    revision: u8,
    /// The file moved to a library outside every grant.
    moved: bool,
    /// Plan tokens issued so far (at most two), each pinning a revision.
    tokens: Vec<Claims>,
    owner: Option<Owner>,
}
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
enum Input {
    /// Principal "a" plans the current revision on `route` for timeline "t",
    /// or (`other`) for timeline "u": an incompatible replan context.
    Plan {
        route: Route,
        other: bool,
    },
    /// `who` presents token `token` to admit a delivery or to replan it.
    Admit {
        who: u8,
        token: u8,
    },
    Replan {
        who: u8,
        token: u8,
    },
    Control {
        who: u8,
    },
    View {
        who: u8,
    },
    RevokePlayback,
    RevokeViewing,
    NarrowProfile,
    RevokeLibrary,
    MoveFile,
    ChangeSource,
    Tick,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
enum Effect {
    Planned,
    Admitted,
    Replanned,
    Allowed,
    Refused(String),
}

fn principal(id: &str, g: &Grants) -> Principal {
    let mut permissions = std::collections::BTreeSet::from([Permission::CatalogRead]);
    if g.playback {
        permissions.insert(Permission::PlaybackRequest);
    }
    if g.viewing {
        permissions.insert(Permission::ViewingWrite);
    }
    let set = |on: bool, value: &str| {
        if on {
            [value.to_string()].into()
        } else {
            Default::default()
        }
    };
    Principal {
        id: id.into(),
        device_id: None,
        mode: AccessMode::Paired,
        grant: Grant {
            profile_ids: set(g.profile, "p"),
            permissions,
        },
        policy: Policy {
            library_ids: set(g.library, "lib"),
            ..Policy::default()
        },
        policy_revision: 0,
    }
}
fn full() -> Grants {
    Grants {
        playback: true,
        viewing: true,
        profile: true,
        library: true,
    }
}
fn caller(s: &State, who: u8) -> Principal {
    if who == 0 {
        principal("a", &s.a)
    } else {
        principal("b", &full())
    }
}
fn facts(s: &State) -> FileFacts {
    FileFacts {
        library_ids: [if s.moved { "elsewhere" } else { "lib" }.to_string()].into(),
        revision: s.revision.to_string(),
    }
}
fn owner_of(c: &Claims) -> Owner {
    Owner {
        principal: c.principal.clone(),
        profile: c.profile.clone(),
        timeline: c.timeline.clone(),
        file: c.file.clone(),
        file_revision: c.file_revision.clone(),
    }
}

struct Playback;
impl Model for Playback {
    type State = State;
    type Input = Input;
    type Output = Effect;
    fn metadata(&self) -> ModelMetadata {
        ModelMetadata {
            name: "playscale.playback-session".into(),
            model_version: 2,
            properties_version: 2,
            codec_version: 1,
            build: include_str!("../src/playback_session.rs").into(),
        }
    }
    fn initial_state(&self) -> Result<State, ModelError> {
        Ok(State {
            now: 0,
            a: full(),
            revision: 0,
            moved: false,
            tokens: vec![],
            owner: None,
        })
    }
    fn step(&self, s: &State, input: &Input) -> Result<Transition<State, Effect>, ModelError> {
        let mut n = s.clone();
        let mut out = vec![];
        let file = facts(s);
        match input {
            Input::Plan { route, other } => {
                n.tokens.push(Claims {
                    principal: "a".into(),
                    profile: "p".into(),
                    timeline: if *other { "u" } else { "t" }.into(),
                    version: "v".into(),
                    file: "f".into(),
                    file_revision: s.revision.to_string(),
                    route: *route,
                    audio: Some(0),
                    subtitle: None,
                    expires_at: s.now + TTL,
                });
                out.push(Effect::Planned);
            }
            Input::Admit { who, token } | Input::Replan { who, token } => {
                let claims = &s.tokens[*token as usize];
                let p = caller(s, *who);
                let replan = matches!(input, Input::Replan { .. });
                let observed = Observed {
                    principal: &p,
                    file: Some(&file),
                    bound: true,
                    now: s.now,
                };
                let decision = core::admit(claims, &observed).and_then(|_| match &s.owner {
                    None if !replan => Ok(()),
                    Some(o) if replan && core::may_control(o, &p, Some(&file)) => {
                        if core::replan_compatible(o, claims) {
                            Ok(())
                        } else {
                            Err(PlanError::SourceChanged)
                        }
                    }
                    _ => Err(PlanError::NotFound),
                });
                match decision {
                    Ok(()) if replan => out.push(Effect::Replanned),
                    Ok(()) => {
                        n.owner = Some(owner_of(claims));
                        out.push(Effect::Admitted);
                    }
                    Err(e) => out.push(Effect::Refused(format!("{e:?}"))),
                }
            }
            Input::Control { who } | Input::View { who } => {
                let p = caller(s, *who);
                let allowed = s.owner.as_ref().is_some_and(|o| {
                    if matches!(input, Input::View { .. }) {
                        core::may_view(o, &p, Some(&file))
                    } else {
                        core::may_control(o, &p, Some(&file))
                    }
                });
                out.push(if allowed {
                    Effect::Allowed
                } else {
                    Effect::Refused("NotFound".into())
                });
            }
            Input::RevokePlayback => n.a.playback = false,
            Input::RevokeViewing => n.a.viewing = false,
            Input::NarrowProfile => n.a.profile = false,
            Input::RevokeLibrary => n.a.library = false,
            Input::MoveFile => n.moved = true,
            Input::ChangeSource => n.revision += 1,
            Input::Tick => n.now += 1,
        }
        Ok(Transition::accepted(n, out))
    }
    fn check_state(&self, s: &State) -> Result<Vec<Check>, ModelError> {
        Ok(vec![check(
            "a_delivery_is_only_ever_owned_by_its_planning_principal",
            s.owner.as_ref().is_none_or(|o| o.principal == "a"),
        )])
    }
    fn check_transition(
        &self,
        s: &State,
        i: &Input,
        t: &TransitionRef<'_, State, Effect>,
    ) -> Result<Vec<Check>, ModelError> {
        // Stated independently of the production rules: "a" may act while it
        // keeps playback, the profile, and a granted library containing the file.
        let a_ok = s.a.playback && s.a.profile && s.a.library && !s.moved;
        let current =
            |c: &Claims| s.now < c.expires_at && c.file_revision == s.revision.to_string();
        let mut checks = vec![];
        match i {
            Input::Admit { who, token } => {
                let c = &s.tokens[*token as usize];
                let expected = s.owner.is_none() && *who == 0 && a_ok && current(c);
                checks.push(check(
                    "admission_exactly_when_owner_permission_profile_scope_expiry_and_revision_hold",
                    (t.outputs == [Effect::Admitted]) == expected,
                ));
                checks.push(check(
                    "admission_installs_exactly_the_token_context",
                    t.state.owner
                        == if expected {
                            Some(owner_of(c))
                        } else {
                            s.owner.clone()
                        },
                ));
            }
            Input::Replan { who, token } => {
                let c = &s.tokens[*token as usize];
                let expected = s.owner.as_ref().is_some_and(|o| {
                    o.file_revision == c.file_revision && o.timeline == c.timeline
                }) && *who == 0
                    && a_ok
                    && current(c);
                checks.push(check(
                    "replan_only_by_owner_with_current_token_for_the_same_timeline_and_source",
                    (t.outputs == [Effect::Replanned]) == expected && t.state.owner == s.owner,
                ));
            }
            Input::Control { who } => checks.push(check(
                "control_only_by_owner_with_current_playback_profile_and_scope",
                (t.outputs == [Effect::Allowed]) == (s.owner.is_some() && *who == 0 && a_ok),
            )),
            Input::View { who } => checks.push(check(
                "viewing_also_needs_viewing_write",
                (t.outputs == [Effect::Allowed])
                    == (s.owner.is_some() && *who == 0 && a_ok && s.a.viewing),
            )),
            _ => {}
        }
        Ok(checks)
    }
}
fn check(id: &'static str, ok: bool) -> Check {
    if ok {
        Check::passed(id)
    } else {
        Check::failed(id, "playback session contract violated")
    }
}
impl Enumerate for Playback {
    fn inputs(&self, s: &State) -> Result<Vec<Input>, ModelError> {
        let mut inputs = vec![];
        if s.tokens.len() < 2 {
            for other in [false, true] {
                for route in [Route::Original, Route::Transcode] {
                    inputs.push(Input::Plan { route, other });
                }
            }
        }
        for token in 0..s.tokens.len() as u8 {
            for who in 0..2 {
                inputs.push(Input::Admit { who, token });
                inputs.push(Input::Replan { who, token });
            }
        }
        for who in 0..2 {
            inputs.extend([Input::Control { who }, Input::View { who }]);
        }
        let toggles = [
            (s.a.playback, Input::RevokePlayback),
            (s.a.viewing, Input::RevokeViewing),
            (s.a.profile, Input::NarrowProfile),
            (s.a.library, Input::RevokeLibrary),
            (!s.moved, Input::MoveFile),
            (s.revision < 1, Input::ChangeSource),
            (s.now <= TTL, Input::Tick),
        ];
        inputs.extend(toggles.into_iter().filter(|(on, _)| *on).map(|(_, i)| i));
        Ok(inputs)
    }
}
impl ModelCodec for Playback {
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
    fn encode_output(&self, v: &Effect) -> Result<Vec<u8>, ModelError> {
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
fn plan_admission_ownership_and_replans_survive_revocation_expiry_and_source_change() {
    let report = stateless::explore::enumerate(
        &Playback,
        stateless::explore::SearchConfig {
            max_states: 2_000_000,
            max_transitions: 60_000_000,
            max_depth: 100,
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
        "playback-session model: {} states, {} transitions; two principals, two timelines, \
         revocation of playback/viewing/profile/library, file move, source change, expiry",
        report.states, report.transitions
    );
}
