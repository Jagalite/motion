//! v2 playback authority: which route a plan selects, whether a plan token may
//! admit a delivery, who may observe or control a delivery, and how a viewing
//! session stays bound to the delivery it was created for.
//!
//! The adapter supplies observations (current principal, file revision, catalog
//! scope, client capability, source streams) and executes effects; it never
//! decides these rules itself.
use crate::access::{FileFacts, Permission, Principal, catalog_scope};
use crate::delivery::Operation;
use crate::playback::{self, Candidate, Mode, Support};
use serde::{Deserialize, Serialize};

/// Lifetime of a playback plan token. Admission rechecks every pinned fact.
pub const PLAN_TTL_SECONDS: i64 = 5 * 60;

/// How one planned candidate is delivered.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Route {
    /// The stored original, byte ranges, browser-selected default streams.
    Original,
    /// A live HLS conversion of the original with exactly the selected streams.
    Transcode,
}
impl Route {
    pub fn operation(self) -> Operation {
        match self {
            Route::Original => Operation::Original,
            Route::Transcode => Operation::VideoTranscode,
        }
    }
    pub fn transport(self) -> &'static str {
        match self {
            Route::Original => "http_range",
            Route::Transcode => "hls",
        }
    }
    /// Candidate identities are stable for one file and route, so a client
    /// can report a failed candidate and never be offered it again.
    /// IDs fit the contract's 128-character identifier: a file ID too long to
    /// embed is replaced by a stable 64-bit digest of it.
    pub fn candidate_id(self, file_id: &str) -> String {
        let prefix = match self {
            Route::Original => "o",
            Route::Transcode => "t",
        };
        if file_id.len() <= 126 {
            format!("{prefix}-{file_id}")
        } else {
            // FNV-1a: deterministic across builds and platforms.
            let digest = file_id.bytes().fold(0xcbf2_9ce4_8422_2325_u64, |h, b| {
                (h ^ u64::from(b)).wrapping_mul(0x0000_0100_0000_01b3)
            });
            format!("{prefix}_{digest:016x}")
        }
    }
}

/// Facts about the one original source a timeline plan may use.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SourceFacts {
    pub available: bool,
    /// Whether the client reports decoding the source's video and default audio.
    pub client_support: Support,
    /// Within the client's max_height / bitrate limits.
    pub within_budget: bool,
    /// The core live-route rule admits a transcode of the selected streams.
    pub transcodable: bool,
}

/// The client's transport capability and explicit selections.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Request {
    pub mode: Mode,
    pub range: bool,
    pub hls: bool,
    /// The client chose a stream other than the browser default (for example a
    /// second audio track or a subtitle). A byte route cannot honor that choice.
    pub explicit_streams: bool,
    pub failed: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Plan {
    Ready(Route),
    Blocked(&'static str),
}

/// Select a route. Original byte delivery is preferred in Auto; a live
/// conversion is used when the original is unsupported, failed on this client,
/// requested explicitly (Convert), or needed to honor an explicit stream choice.
/// Strict Original never converts. Nothing here starts processing.
pub fn plan(file_id: &str, source: &SourceFacts, request: &Request) -> Plan {
    let failed = |route: Route| request.failed.contains(&route.candidate_id(file_id));
    if !source.available {
        return Plan::Blocked("source_unavailable");
    }
    let original = Candidate {
        original: true,
        available: request.range && !request.explicit_streams && !failed(Route::Original),
        support: source.client_support,
        matches_recipe: false,
        within_budget: source.within_budget,
        can_process: source.transcodable,
    };
    let byte = match request.mode {
        // Convert never plays the stored original; prepared renditions are not
        // yet offered by this planner, so the live route serves Convert.
        Mode::Convert => playback::Decision::Prepare(0),
        mode => playback::decide(mode, std::slice::from_ref(&original), None),
    };
    match byte {
        playback::Decision::Play(_) => Plan::Ready(Route::Original),
        _ if request.mode == Mode::Original => Plan::Blocked(if request.explicit_streams {
            "stream_selection_requires_conversion"
        } else {
            "original_unavailable_or_unsupported"
        }),
        _ if !request.hls => Plan::Blocked("client_cannot_play_conversion"),
        _ if !source.transcodable => Plan::Blocked("no_live_route"),
        _ if failed(Route::Transcode) => Plan::Blocked("all_candidates_failed"),
        _ => Plan::Ready(Route::Transcode),
    }
}

/// Everything a plan token pins. Signed by the adapter; never trusted unsigned.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Claims {
    pub principal: String,
    pub profile: String,
    pub timeline: String,
    pub version: String,
    pub file: String,
    pub file_revision: String,
    pub route: Route,
    /// Zero-based audio stream among the source's audio streams; None omits audio.
    pub audio: Option<u32>,
    /// Zero-based text subtitle stream; None means no subtitle.
    pub subtitle: Option<u32>,
    pub expires_at: i64,
}

/// Current facts observed inside the admission transaction.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Observed<'a> {
    /// The caller re-derived from current credential state.
    pub principal: &'a Principal,
    /// The pinned file's current revision and library membership; None when it
    /// is missing or unavailable.
    pub file: Option<&'a FileFacts>,
    /// The pinned version still binds the pinned file to the pinned timeline.
    pub bound: bool,
    pub now: i64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum PlanError {
    Expired,
    /// The token belongs to another principal; never disclose its contents.
    WrongPrincipal,
    Forbidden(Permission),
    ProfileForbidden,
    NotFound,
    SourceChanged,
}

/// Whether `principal` may read a file with these facts: the same catalog
/// scope (library grants, CatalogRead, rating policy) as every catalog read.
pub fn readable(principal: &Principal, file: Option<&FileFacts>) -> bool {
    file.is_some_and(|f| catalog_scope(principal).admits(f.library_ids.iter()))
}

/// Whether a plan token may admit (or replan) a delivery now. Every pinned
/// fact is rechecked: a token never outlives a permission, profile grant,
/// catalog scope, version binding or source revision it was issued under.
pub fn admit(claims: &Claims, observed: &Observed<'_>) -> Result<(), PlanError> {
    if observed.now >= claims.expires_at {
        return Err(PlanError::Expired);
    }
    if observed.principal.id != claims.principal {
        return Err(PlanError::WrongPrincipal);
    }
    if !observed.principal.allows(Permission::PlaybackRequest) {
        return Err(PlanError::Forbidden(Permission::PlaybackRequest));
    }
    if !observed.principal.may_use_profile(&claims.profile) {
        return Err(PlanError::ProfileForbidden);
    }
    if !observed.bound || !readable(observed.principal, observed.file) {
        return Err(PlanError::NotFound);
    }
    if observed
        .file
        .is_some_and(|f| f.revision != claims.file_revision)
    {
        return Err(PlanError::SourceChanged);
    }
    Ok(())
}

/// The principal and context a delivery was admitted for.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Owner {
    pub principal: String,
    pub profile: String,
    pub timeline: String,
    pub file: String,
    pub file_revision: String,
}

/// Observe or control a delivery: only its admitting principal, while that
/// principal may still request playback for the delivery's profile and still
/// read the delivered file. Any other caller learns nothing (the adapter
/// reports not found). Source revision changes are fenced by stream admission;
/// the owner may still read and close such a delivery.
pub fn may_control(owner: &Owner, principal: &Principal, file: Option<&FileFacts>) -> bool {
    owner.principal == principal.id
        && principal.allows(Permission::PlaybackRequest)
        && principal.may_use_profile(&owner.profile)
        && readable(principal, file)
}

/// A replan may change route and streams but never the delivery's timeline,
/// profile or source file revision; those need a new delivery.
pub fn replan_compatible(owner: &Owner, claims: &Claims) -> bool {
    owner.principal == claims.principal
        && owner.profile == claims.profile
        && owner.timeline == claims.timeline
        && owner.file == claims.file
        && owner.file_revision == claims.file_revision
}

/// Viewing sessions write durable progress for the delivery's profile.
pub fn may_view(owner: &Owner, principal: &Principal, file: Option<&FileFacts>) -> bool {
    may_control(owner, principal, file) && principal.allows(Permission::ViewingWrite)
}

/// Record progress into an existing viewing session (also after its delivery
/// ended, so a durable client outbox can drain after a restart): viewing
/// write, the session's profile, and the session's file still readable.
/// Sessions are per profile; their unguessable identity names the session.
pub fn may_record(principal: &Principal, profile: &str, file: Option<&FileFacts>) -> bool {
    principal.allows(Permission::ViewingWrite)
        && principal.may_use_profile(profile)
        && readable(principal, file)
}

/// A viewing session's identity embeds the delivery it was created for, so the
/// binding is durable with the session row itself and cannot drift after a
/// restart. Both parts are server-generated identifiers.
pub fn viewing_id(session: &str, delivery: &str) -> Option<String> {
    let valid = |s: &str| {
        !s.is_empty() && s.len() <= 60 && s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
    };
    (valid(session) && valid(delivery)).then(|| format!("{session}_{delivery}"))
}

/// The delivery a viewing session was created for; None for sessions that
/// were not created through a delivery (v1 sessions).
pub fn viewing_delivery(id: &str) -> Option<&str> {
    let (session, delivery) = id.split_once('_')?;
    viewing_id(session, delivery).map(|_| delivery)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::access::{Grant, Mode as AccessMode, Policy};

    fn principal(id: &str, permissions: &[Permission], profiles: &[&str]) -> Principal {
        let mut permissions: std::collections::BTreeSet<_> = permissions.iter().copied().collect();
        permissions.insert(Permission::CatalogRead);
        Principal {
            id: id.into(),
            device_id: None,
            mode: AccessMode::Paired,
            grant: Grant {
                profile_ids: profiles.iter().map(|p| p.to_string()).collect(),
                permissions,
            },
            policy: Policy {
                library_ids: ["lib".to_string()].into(),
                ..Policy::default()
            },
            policy_revision: 0,
        }
    }
    fn facts(revision: &str, library: &str) -> FileFacts {
        FileFacts {
            library_ids: [library.to_string()].into(),
            revision: revision.into(),
        }
    }
    fn source(support: Support) -> SourceFacts {
        SourceFacts {
            available: true,
            client_support: support,
            within_budget: true,
            transcodable: true,
        }
    }
    fn request(mode: Mode) -> Request {
        Request {
            mode,
            range: true,
            hls: true,
            explicit_streams: false,
            failed: vec![],
        }
    }

    #[test]
    fn auto_prefers_original_then_converts_unsupported_failed_or_explicit_streams() {
        let auto = request(Mode::Auto);
        assert_eq!(
            plan("f", &source(Support::Supported), &auto),
            Plan::Ready(Route::Original)
        );
        assert_eq!(
            plan("f", &source(Support::Unknown), &auto),
            Plan::Ready(Route::Original)
        );
        assert_eq!(
            plan("f", &source(Support::Unsupported), &auto),
            Plan::Ready(Route::Transcode)
        );
        let failed = Request {
            failed: vec![Route::Original.candidate_id("f")],
            ..auto.clone()
        };
        assert_eq!(
            plan("f", &source(Support::Supported), &failed),
            Plan::Ready(Route::Transcode)
        );
        let both = Request {
            failed: vec![
                Route::Original.candidate_id("f"),
                Route::Transcode.candidate_id("f"),
            ],
            ..auto.clone()
        };
        assert_eq!(
            plan("f", &source(Support::Supported), &both),
            Plan::Blocked("all_candidates_failed")
        );
        let no_range = Request {
            range: false,
            ..auto.clone()
        };
        assert_eq!(
            plan("f", &source(Support::Supported), &no_range),
            Plan::Ready(Route::Transcode)
        );
        let explicit = Request {
            explicit_streams: true,
            ..auto
        };
        assert_eq!(
            plan("f", &source(Support::Supported), &explicit),
            Plan::Ready(Route::Transcode)
        );
    }

    #[test]
    fn strict_modes_never_fall_back() {
        assert_eq!(
            plan("f", &source(Support::Unsupported), &request(Mode::Original)),
            Plan::Blocked("original_unavailable_or_unsupported")
        );
        let explicit = Request {
            explicit_streams: true,
            ..request(Mode::Original)
        };
        assert_eq!(
            plan("f", &source(Support::Supported), &explicit),
            Plan::Blocked("stream_selection_requires_conversion")
        );
        assert_eq!(
            plan("f", &source(Support::Supported), &request(Mode::Convert)),
            Plan::Ready(Route::Transcode)
        );
        let no_hls = Request {
            hls: false,
            ..request(Mode::Convert)
        };
        assert_eq!(
            plan("f", &source(Support::Supported), &no_hls),
            Plan::Blocked("client_cannot_play_conversion")
        );
        let mut hdr = source(Support::Unsupported);
        hdr.transcodable = false;
        assert_eq!(
            plan("f", &hdr, &request(Mode::Auto)),
            Plan::Blocked("no_live_route")
        );
        let mut gone = source(Support::Supported);
        gone.available = false;
        assert_eq!(
            plan("f", &gone, &request(Mode::Auto)),
            Plan::Blocked("source_unavailable")
        );
    }

    fn claims() -> Claims {
        Claims {
            principal: "a".into(),
            profile: "p".into(),
            timeline: "t".into(),
            version: "v".into(),
            file: "f".into(),
            file_revision: "r1".into(),
            route: Route::Original,
            audio: Some(0),
            subtitle: None,
            expires_at: 100,
        }
    }

    #[test]
    fn plan_admission_rechecks_every_pinned_fact() {
        let ok = principal("a", &[Permission::PlaybackRequest], &["p"]);
        let r1 = facts("r1", "lib");
        let observed = |p, file, bound, now| Observed {
            principal: p,
            file,
            bound,
            now,
        };
        assert_eq!(
            admit(&claims(), &observed(&ok, Some(&r1), true, 99)),
            Ok(())
        );
        assert_eq!(
            admit(&claims(), &observed(&ok, Some(&r1), true, 100)),
            Err(PlanError::Expired)
        );
        let other = principal("b", &[Permission::PlaybackRequest], &["p"]);
        assert_eq!(
            admit(&claims(), &observed(&other, Some(&r1), true, 1)),
            Err(PlanError::WrongPrincipal)
        );
        let revoked = principal("a", &[], &["p"]);
        assert_eq!(
            admit(&claims(), &observed(&revoked, Some(&r1), true, 1)),
            Err(PlanError::Forbidden(Permission::PlaybackRequest))
        );
        let narrowed = principal("a", &[Permission::PlaybackRequest], &["q"]);
        assert_eq!(
            admit(&claims(), &observed(&narrowed, Some(&r1), true, 1)),
            Err(PlanError::ProfileForbidden)
        );
        assert_eq!(
            admit(&claims(), &observed(&ok, None, true, 1)),
            Err(PlanError::NotFound)
        );
        assert_eq!(
            admit(&claims(), &observed(&ok, Some(&r1), false, 1)),
            Err(PlanError::NotFound)
        );
        // Moved to a library outside the grant, or policy narrowed to nothing.
        let moved = facts("r1", "other");
        let changed = facts("r2", "lib");
        assert_eq!(
            admit(&claims(), &observed(&ok, Some(&moved), true, 1)),
            Err(PlanError::NotFound)
        );
        let mut rated = ok.clone();
        rated.policy.allow_unrated = false;
        assert_eq!(
            admit(&claims(), &observed(&rated, Some(&r1), true, 1)),
            Err(PlanError::NotFound)
        );
        assert_eq!(
            admit(&claims(), &observed(&ok, Some(&changed), true, 1)),
            Err(PlanError::SourceChanged)
        );
    }

    #[test]
    fn delivery_control_and_replans_stay_with_the_admitted_context() {
        let owner = Owner {
            principal: "a".into(),
            profile: "p".into(),
            timeline: "t".into(),
            file: "f".into(),
            file_revision: "r1".into(),
        };
        let viewer = principal(
            "a",
            &[Permission::PlaybackRequest, Permission::ViewingWrite],
            &["p"],
        );
        let file = facts("r1", "lib");
        let f = Some(&file);
        assert!(may_control(&owner, &viewer, f) && may_view(&owner, &viewer, f));
        let player = principal("a", &[Permission::PlaybackRequest], &["p"]);
        assert!(may_control(&owner, &player, f) && !may_view(&owner, &player, f));
        assert!(!may_control(
            &owner,
            &principal("b", &[Permission::PlaybackRequest], &["p"]),
            f
        ));
        assert!(!may_control(
            &owner,
            &principal("a", &[Permission::PlaybackRequest], &["q"]),
            f
        ));
        // Losing the library (or the file) ends control and viewing.
        assert!(!may_control(&owner, &viewer, Some(&facts("r1", "other"))));
        assert!(!may_view(&owner, &viewer, None));
        assert!(may_record(&viewer, "p", f));
        assert!(!may_record(&player, "p", f));
        assert!(!may_record(&viewer, "q", f));
        assert!(!may_record(&viewer, "p", Some(&facts("r1", "other"))));
        let mut c = claims();
        assert!(replan_compatible(&owner, &c));
        c.route = Route::Transcode;
        c.audio = Some(1);
        assert!(replan_compatible(&owner, &c));
        for change in [
            |c: &mut Claims| c.timeline = "u".into(),
            |c: &mut Claims| c.profile = "q".into(),
            |c: &mut Claims| c.file_revision = "r2".into(),
            |c: &mut Claims| c.file = "g".into(),
            |c: &mut Claims| c.principal = "b".into(),
        ] {
            let mut changed = claims();
            change(&mut changed);
            assert!(!replan_compatible(&owner, &changed));
        }
    }

    #[test]
    fn candidate_ids_are_stable_distinct_and_bounded() {
        let long = "x".repeat(128);
        for route in [Route::Original, Route::Transcode] {
            assert_eq!(route.candidate_id(&long), route.candidate_id(&long));
            assert!(route.candidate_id(&long).len() <= 128);
        }
        assert_ne!(
            Route::Original.candidate_id("f"),
            Route::Transcode.candidate_id("f")
        );
        assert_ne!(
            Route::Original.candidate_id(&long),
            Route::Original.candidate_id(&"y".repeat(128))
        );
    }

    #[test]
    fn viewing_ids_carry_their_delivery() {
        let id = viewing_id("s-1", "d-2").unwrap();
        assert_eq!(viewing_delivery(&id), Some("d-2"));
        assert_eq!(viewing_delivery("legacy-session"), None);
        assert_eq!(viewing_id("s_1", "d"), None);
        assert_eq!(viewing_id("", "d"), None);
        assert_eq!(viewing_delivery("a_b_c"), None);
        assert!(id.len() <= 128);
    }
}
