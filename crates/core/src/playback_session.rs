//! v2 playback authority: which route a plan selects, whether a plan token may
//! admit a delivery, who may observe or control a delivery, and who may write
//! progress through it. Viewing order and authority are decided in
//! `timeline_viewing`.
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
    /// A stored, revision-valid generated rendition of the original, byte
    /// ranges, its default streams.
    Prepared,
    /// A live HLS stream copy of the original's video and AAC (or no) audio,
    /// with exactly the selected streams and source timestamps.
    Remux,
    /// A live HLS stream copy of the original's video with the selected audio
    /// converted to AAC.
    AudioConvert,
    /// A live HLS conversion of the original with exactly the selected streams.
    Transcode,
    /// The same conversion on a hardware encoder session. Never falls back to
    /// software within a delivery; a failure is reported and replanned.
    HardwareTranscode,
}
impl Route {
    pub fn operation(self) -> Operation {
        match self {
            Route::Original => Operation::Original,
            Route::Prepared => Operation::Prepared,
            Route::Remux => Operation::Remux,
            Route::AudioConvert => Operation::AudioConvert,
            Route::Transcode | Route::HardwareTranscode => Operation::VideoTranscode,
        }
    }
    /// The stream-copy route for a copy operation.
    pub fn copy(operation: Operation) -> Option<Route> {
        match operation {
            Operation::Remux => Some(Route::Remux),
            Operation::AudioConvert => Some(Route::AudioConvert),
            _ => None,
        }
    }
    pub fn transport(self) -> &'static str {
        match self {
            Route::Original | Route::Prepared => "http_range",
            Route::Remux | Route::AudioConvert | Route::Transcode | Route::HardwareTranscode => {
                "hls"
            }
        }
    }
    /// Candidate identities are stable for one file and route, so a client
    /// can report a failed candidate and never be offered it again.
    /// IDs fit the contract's 128-character identifier: a file ID too long to
    /// embed is replaced by a stable 64-bit digest of it.
    pub fn candidate_id(self, file_id: &str) -> String {
        let prefix = match self {
            Route::Original => "o",
            Route::Prepared => "p",
            Route::Remux => "r",
            Route::AudioConvert => "c",
            Route::Transcode => "t",
            Route::HardwareTranscode => "h",
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
    /// The conversion's output honors the client's height and bitrate limits
    /// (`conversion_fits`). A conversion never silently exceeds them.
    pub conversion_within_budget: bool,
    /// Live stream copy of the selected streams. A copy keeps the source video,
    /// so `within_budget` is also its budget.
    pub copy: StreamCopy,
    /// A hardware encoder for the conversion. Its recipe has the same output
    /// bounds as the software one, so `conversion_within_budget` covers both.
    pub hardware: Hardware,
    /// The timeline is an interval of the original (`parts::Clip`). Byte
    /// delivery, a stream copy and prepared renditions all present the whole
    /// file, so only a conversion of the interval can serve it.
    pub clipped: bool,
    /// The timeline plays several files in order. Byte delivery, a copy and
    /// a rendition each present one file, so only a conversion can serve it.
    pub multipart: bool,
    /// Stored renditions of this original whose output and source revisions
    /// are both still the registered ones (`renditions::available`), in the
    /// adapter's preference order (newest first).
    pub prepared: Vec<PreparedFacts>,
}

/// What is known about a live stream copy of the selected streams.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StreamCopy {
    /// Not enabled on this server, or ruled out by the stream facts
    /// (`delivery::copy_operation`), or by observed bitstream details.
    Ineligible,
    /// The stream facts name this copy route, but the source's bitstream
    /// details have not been observed yet.
    Unobserved(Route),
    /// The core live-route rule admits this copy route with observed details.
    Eligible(Route),
}

/// What the server observes about hardware encoding for a live conversion.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Hardware {
    /// Not enabled here, or no hardware encoder session could be opened.
    Unavailable,
    /// Available, but no interactive hardware session is admissible now.
    Busy,
    Available,
}

/// Facts about one revision-valid prepared rendition.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PreparedFacts {
    pub file: String,
    pub client_support: Support,
    pub within_budget: bool,
    /// Byte delivery of this rendition presents exactly the selected streams:
    /// no subtitle is selected, and its one audio stream was prepared from the
    /// selected audio of the same source revision (or, for a rendition whose
    /// source audio is not recorded, no audio was chosen explicitly).
    pub carries_selection: bool,
}

/// Audio bitrate of every live conversion recipe (AAC stereo).
pub const LIVE_AUDIO_BPS: u64 = 160_000;
/// The lowest video rate a capped conversion is offered at.
pub const MIN_CAPPED_VIDEO_BPS: u64 = 300_000;

/// Encoder rate limits for a conversion capped at a client's peak bitrate.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RateCap {
    /// VBV maximum video rate, bits/s.
    pub video_max_bps: u64,
    /// VBV buffer, bits (one second at the maximum rate).
    pub video_buffer_bits: u64,
}

/// Rate limits that keep a live conversion's peak bitrate within
/// `max_bitrate_bps` over every window of at least `min_window_ms` (HLS
/// measures peak bitrate over sets of segments lasting at least half the
/// target duration). Over a window `w`, a VBV-constrained video stream carries
/// at most `M*w + B` bits, so with `B` one second at `M` its rate is at most
/// `M*(1 + 1000/w)`. The audio rate and a 5% container allowance come first.
/// None when too little remains for video.
pub fn rate_cap(max_bitrate_bps: u64, min_window_ms: u64) -> Option<RateCap> {
    let video = (max_bitrate_bps.checked_mul(100)? / 105).checked_sub(LIVE_AUDIO_BPS)?;
    let max = video.checked_mul(min_window_ms)? / (min_window_ms + 1000);
    (max >= MIN_CAPPED_VIDEO_BPS).then_some(RateCap {
        video_max_bps: max,
        video_buffer_bits: max,
    })
}

/// The adaptive ladder of a conversion: the main variant (at most
/// `delivery::LIVE_MAX_HEIGHT` lines) and a low variant (at most
/// `ADAPTIVE_LOW_HEIGHT`), each capped (`rate_cap`) so its declared HLS
/// BANDWIDTH is a measured bound.
pub const ADAPTIVE_MAIN_BPS: u64 = 3_000_000;
pub const ADAPTIVE_LOW_BPS: u64 = 800_000;
pub const ADAPTIVE_LOW_HEIGHT: u32 = 360;

/// Whether a planned route is delivered as an adaptive (multi-variant)
/// conversion: the server enables it, the route is a software conversion of
/// one file without a burned subtitle, and the client set no bitrate cap
/// (a capped plan keeps its single capped variant). Everything else is one
/// variant, as before.
pub fn adaptive(
    enabled: bool,
    route: Route,
    max_bitrate_bps: Option<u64>,
    burn_in: bool,
    multipart: bool,
) -> bool {
    enabled && route == Route::Transcode && max_bitrate_bps.is_none() && !burn_in && !multipart
}

/// Whether a published segment honors a peak bitrate cap.
pub fn within_cap(bytes: u64, duration_ms: u64, max_bitrate_bps: u64) -> bool {
    bytes.saturating_mul(8_000) <= max_bitrate_bps.saturating_mul(duration_ms)
}

/// The highest bitrate over the runs of contiguous segments ending with the
/// newest one whose total duration is between `min_window_ms` and
/// `max_window_ms` (HLS peak windows); zero when no run qualifies yet.
pub fn peak_bitrate(recent: &[(u64, u64)], min_window_ms: u64, max_window_ms: u64) -> u64 {
    let (mut bytes, mut duration, mut peak) = (0u64, 0u64, 0u64);
    for &(b, d) in recent.iter().rev() {
        bytes = bytes.saturating_add(b);
        duration = duration.saturating_add(d);
        if duration > max_window_ms {
            break;
        }
        if duration >= min_window_ms && duration > 0 {
            peak = peak.max(bytes.saturating_mul(8_000) / duration);
        }
    }
    peak
}

/// Whether every run of contiguous segments ending with the newest one, whose
/// total duration is between `min_window_ms` and `max_window_ms` (HLS: 0.5
/// and 1.5 target durations), stays within the cap. `recent` holds (bytes,
/// duration) of the generation's segments in order, newest last; runs ending
/// earlier were checked when their own last segment was published.
pub fn peak_within_cap(
    recent: &[(u64, u64)],
    max_bitrate_bps: u64,
    min_window_ms: u64,
    max_window_ms: u64,
) -> bool {
    let (mut bytes, mut duration) = (0u64, 0u64);
    for &(b, d) in recent.iter().rev() {
        bytes = bytes.saturating_add(b);
        duration = duration.saturating_add(d);
        if duration > max_window_ms {
            break;
        }
        if duration >= min_window_ms && !within_cap(bytes, duration, max_bitrate_bps) {
            return false;
        }
    }
    true
}

/// Whether a conversion whose output is at most `output_height` lines and, when
/// capped, at most `output_max_bitrate` bits/s honors the requested limits. An
/// uncapped (quality-targeted) encoder cannot promise any bitrate limit.
pub fn conversion_fits(
    max_height: Option<u32>,
    max_bitrate: Option<u64>,
    output_height: Option<u32>,
    output_max_bitrate: Option<u64>,
) -> bool {
    max_height.is_none_or(|limit| output_height.is_some_and(|h| h <= limit))
        && max_bitrate.is_none_or(|limit| output_max_bitrate.is_some_and(|b| b <= limit))
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
    /// The client pinned the prepared rendition's version: nothing else may
    /// serve the plan.
    pub pinned_prepared: bool,
    /// The selected subtitle can only be presented burned into the video (a
    /// bitmap subtitle for a client that accepts burned subtitles). Burn-in
    /// is a video transform: only a conversion can deliver it.
    pub burn_in: bool,
    pub failed: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Plan {
    /// The original (byte ranges) or a live conversion of it.
    Ready(Route),
    /// The prepared rendition at this index of `SourceFacts::prepared`.
    Prepared(usize),
    /// A stream copy would be chosen if the source's bitstream details admit
    /// it: observe them, then plan again with `StreamCopy` decided. Planning
    /// with decided copy facts never returns this.
    Observe,
    Blocked(&'static str),
}

/// Select a route. Original byte delivery is preferred in Auto, then a
/// prepared rendition, then a live stream copy, then a live conversion;
/// Convert prefers a prepared rendition over a live conversion and never plays
/// the original or a copy of its video. A live route is also needed to honor
/// an explicit stream choice. Strict Original never converts, and a pinned
/// prepared version is never widened. A failed copy falls back to conversion.
/// A conversion uses an available hardware encoder first; a failed or busy
/// one falls back to software, and a failed software conversion ends the
/// plan. Nothing here starts processing.
pub fn plan(file_id: &str, source: &SourceFacts, request: &Request) -> Plan {
    let failed = |route: Route, file: &str| request.failed.contains(&route.candidate_id(file));
    let byte = request.range && !request.explicit_streams && !request.burn_in;
    // An unavailable original rules out its own byte route and any live
    // conversion of it, not a stored rendition that is still revision-valid.
    let mut candidates = vec![Candidate {
        original: true,
        available: source.available
            && byte
            && !source.clipped
            && !source.multipart
            && !request.pinned_prepared
            && !failed(Route::Original, file_id),
        support: source.client_support,
        matches_recipe: false,
        within_budget: source.within_budget,
        can_process: source.transcodable,
    }];
    candidates.extend(source.prepared.iter().map(|p| Candidate {
        original: false,
        // A rendition carrying the selected streams honors an explicit choice.
        available: request.range
            && !request.burn_in
            && !source.clipped
            && !source.multipart
            && p.carries_selection
            && !failed(Route::Prepared, &p.file),
        support: p.client_support,
        matches_recipe: true,
        within_budget: p.within_budget,
        can_process: false,
    }));
    match playback::decide(request.mode, &candidates, None) {
        playback::Decision::Play(0) => return Plan::Ready(Route::Original),
        playback::Decision::Play(i) => return Plan::Prepared(i - 1),
        _ => {}
    }
    match () {
        _ if request.pinned_prepared => Plan::Blocked("selected_version_unavailable"),
        _ if !source.available => Plan::Blocked("source_unavailable"),
        _ if request.mode == Mode::Original => Plan::Blocked(if source.clipped {
            "interval_requires_conversion"
        } else if source.multipart {
            "multipart_requires_conversion"
        } else if request.burn_in {
            "subtitle_requires_conversion"
        } else if request.explicit_streams {
            "stream_selection_requires_conversion"
        } else {
            "original_unavailable_or_unsupported"
        }),
        _ if !request.hls => Plan::Blocked("client_cannot_play_conversion"),
        // A copy keeps the source's video at its own size and bitrate. Convert
        // asks for converted video, so only Auto may choose a copy.
        _ if request.mode == Mode::Auto
            && source.within_budget
            && !request.burn_in
            && !source.clipped
            && !source.multipart =>
        {
            match source.copy {
                StreamCopy::Eligible(route) if !failed(route, file_id) => Plan::Ready(route),
                StreamCopy::Unobserved(route) if !failed(route, file_id) => Plan::Observe,
                _ => transcode(file_id, source, request),
            }
        }
        _ => transcode(file_id, source, request),
    }
}

fn transcode(file_id: &str, source: &SourceFacts, request: &Request) -> Plan {
    let failed = |route: Route| request.failed.contains(&route.candidate_id(file_id));
    match () {
        _ if !source.transcodable => Plan::Blocked("no_live_route"),
        _ if !source.conversion_within_budget => Plan::Blocked("conversion_exceeds_quality_limit"),
        _ if source.hardware == Hardware::Available && !failed(Route::HardwareTranscode) => {
            Plan::Ready(Route::HardwareTranscode)
        }
        _ if failed(Route::Transcode) => Plan::Blocked("all_candidates_failed"),
        _ => Plan::Ready(Route::Transcode),
    }
}

/// A selected subtitle as a plan pins it.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Subtitle {
    /// The zero-based embedded text subtitle stream of the pinned file,
    /// delivered as WebVTT.
    Text(u32),
    /// A recognized plain-text sidecar of the pinned file, by name and the
    /// fingerprint observed when it was scanned. A changed sidecar is refused.
    Sidecar { name: String, fingerprint: String },
    /// The zero-based embedded bitmap subtitle stream (PGS, DVD, DVB) of the
    /// pinned file, burned into converted video.
    Bitmap(u32),
    /// The zero-based embedded text or styled (ASS/SSA) subtitle stream of
    /// the pinned file, rendered by libass into converted video with the
    /// file's `attachments` font attachments (`delivery::presentation`).
    BurnedText { index: u32, attachments: u32 },
}

/// A plan's subtitle claim: the current form, or a bare stream number from
/// a plan signed before subtitle kinds (an embedded text stream).
fn subtitle_claim<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Option<Subtitle>, D::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Claim {
        Legacy(u32),
        Current(Subtitle),
    }
    Ok(Option::<Claim>::deserialize(d)?.map(|claim| match claim {
        Claim::Legacy(stream) => Subtitle::Text(stream),
        Claim::Current(subtitle) => subtitle,
    }))
}

/// Whether a route presents a subtitle delivered this way. A burned subtitle
/// is a video transform, so only a conversion. A text subtitle comes from the
/// original's file: byte delivery of the original carries it as a WebVTT
/// sidecar, and a live route (copy or conversion of the original) as an HLS
/// subtitle rendition when the client selects in-manifest live text
/// (`live_text`; an external track on a live presentation is refused by the
/// supported player). A prepared rendition is another file: it presents a
/// text subtitle only when it carries exactly the selected one
/// (`processing::carries`), as a WebVTT sidecar of its bytes.
pub fn presents_subtitle(route: Route, burned: bool, live_text: bool, carried: bool) -> bool {
    match (burned, route) {
        (true, Route::Transcode | Route::HardwareTranscode) => true,
        (true, _) => false,
        (false, Route::Original) => true,
        (
            false,
            Route::Remux | Route::AudioConvert | Route::Transcode | Route::HardwareTranscode,
        ) => live_text,
        (false, Route::Prepared) => carried,
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
    /// The selected subtitle; None means no subtitle. Plans signed before
    /// subtitle kinds carry an embedded text stream as a bare number.
    #[serde(deserialize_with = "subtitle_claim")]
    pub subtitle: Option<Subtitle>,
    /// The interval of the file the version binds, when it binds one. A
    /// changed interval is a changed binding.
    #[serde(default)]
    pub clip: Option<crate::parts::Clip>,
    /// Every part of a multipart version, in order (empty for one file). A
    /// changed part or revision is a changed binding.
    #[serde(default)]
    pub parts: Vec<crate::parts::PartPin>,
    /// The peak bitrate a conversion is capped at (`rate_cap`); None uncapped.
    #[serde(default)]
    pub max_bitrate_bps: Option<u64>,
    /// Delivered as an adaptive ladder (`adaptive`).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub adaptive: bool,
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
    fresh(claims, observed.now)?;
    reauthorize(claims, observed)
}

/// A plan token may start something new only before it expires.
pub fn fresh(claims: &Claims, now: i64) -> Result<(), PlanError> {
    if now >= claims.expires_at {
        Err(PlanError::Expired)
    } else {
        Ok(())
    }
}

/// Every pinned fact except expiry. An exact retry of an already admitted
/// request is reauthorized with this, so a lost acknowledgement can be
/// replayed after the token expired, but never past a revoked permission,
/// grant, scope, binding or source revision.
pub fn reauthorize(claims: &Claims, observed: &Observed<'_>) -> Result<(), PlanError> {
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

/// Revalidate long-lived delivery work against the owner's current device grant.
/// Credential expiry is enforced by request admission and the delivery lease;
/// this policy fences device revocation and scope removal even without requests.
pub fn may_continue(
    owner: &Owner,
    device: Option<&crate::access::Device>,
    file: Option<&FileFacts>,
) -> bool {
    let principal = if owner.principal == "operator" {
        Some(Principal::operator())
    } else {
        device
            .filter(|d| !d.revoked && d.id == owner.principal)
            .map(|d| Principal {
                id: d.id.clone(),
                device_id: Some(d.id.clone()),
                mode: crate::access::Mode::Paired,
                grant: d.grant.clone(),
                policy: d.policy.clone(),
                policy_revision: d.revision,
            })
    };
    principal
        .as_ref()
        .is_some_and(|p| may_control(owner, p, file))
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

/// Read or record into an existing viewing session (also after its delivery
/// ended, so a durable client outbox can drain after a restart): viewing
/// write, the session's profile, and its timeline still readable under the
/// caller's catalog scope (observed by the adapter). Sessions are per profile;
/// their unguessable identity names the session.
pub fn may_record(principal: &Principal, profile: &str, timeline_readable: bool) -> bool {
    principal.allows(Permission::ViewingWrite)
        && principal.may_use_profile(profile)
        && timeline_readable
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
            conversion_within_budget: true,
            copy: StreamCopy::Ineligible,
            hardware: Hardware::Unavailable,
            clipped: false,
            multipart: false,
            prepared: vec![],
        }
    }
    impl SourceFacts {
        fn clone_offline(&self) -> Self {
            Self {
                available: false,
                ..self.clone()
            }
        }
    }
    fn request(mode: Mode) -> Request {
        Request {
            mode,
            range: true,
            hls: true,
            explicit_streams: false,
            pinned_prepared: false,
            burn_in: false,
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
            clip: None,
            parts: vec![],
            max_bitrate_bps: None,
            adaptive: false,
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
        // An admitted request is reauthorized without expiry, never past a revocation.
        assert_eq!(
            reauthorize(&claims(), &observed(&ok, Some(&r1), true, 100)),
            Ok(())
        );
        assert_eq!(
            reauthorize(&claims(), &observed(&ok, Some(&r1), false, 100)),
            Err(PlanError::NotFound)
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
        assert!(may_record(&viewer, "p", true));
        assert!(!may_record(&player, "p", true));
        assert!(!may_record(&viewer, "q", true));
        assert!(!may_record(&viewer, "p", false));
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
        let routes = [
            Route::Original,
            Route::Prepared,
            Route::Remux,
            Route::AudioConvert,
            Route::Transcode,
            Route::HardwareTranscode,
        ];
        for route in routes {
            assert_eq!(route.candidate_id(&long), route.candidate_id(&long));
            assert!(route.candidate_id(&long).len() <= 128);
        }
        for (i, a) in routes.iter().enumerate() {
            for b in &routes[i + 1..] {
                assert_ne!(a.candidate_id("f"), b.candidate_id("f"));
                assert_ne!(a.candidate_id(&long), b.candidate_id(&long));
            }
        }
        assert_ne!(
            Route::Original.candidate_id(&long),
            Route::Original.candidate_id(&"y".repeat(128))
        );
    }

    #[test]
    fn prepared_renditions_serve_convert_and_back_up_an_unsupported_original() {
        let prepared = |support| PreparedFacts {
            file: "g".into(),
            client_support: support,
            within_budget: true,
            carries_selection: true,
        };
        let with = |original, p: Option<PreparedFacts>| SourceFacts {
            prepared: p.into_iter().collect(),
            ..source(original)
        };
        // Convert: a usable rendition beats a live conversion.
        let ready = with(Support::Supported, Some(prepared(Support::Unknown)));
        assert_eq!(
            plan("f", &ready, &request(Mode::Convert)),
            Plan::Prepared(0)
        );
        // Auto: the original first, then the rendition, then live.
        assert_eq!(
            plan("f", &ready, &request(Mode::Auto)),
            Plan::Ready(Route::Original)
        );
        let unsupported = with(Support::Unsupported, Some(prepared(Support::Supported)));
        assert_eq!(
            plan("f", &unsupported, &request(Mode::Auto)),
            Plan::Prepared(0)
        );
        // Strict Original never uses it.
        assert_eq!(
            plan("f", &unsupported, &request(Mode::Original)),
            Plan::Blocked("original_unavailable_or_unsupported")
        );
        // An unusable or failed rendition falls back to the live route.
        let bad = with(Support::Unsupported, Some(prepared(Support::Unsupported)));
        assert_eq!(
            plan("f", &bad, &request(Mode::Convert)),
            Plan::Ready(Route::Transcode)
        );
        let failed = Request {
            failed: vec![Route::Prepared.candidate_id("g")],
            ..request(Mode::Convert)
        };
        assert_eq!(plan("f", &ready, &failed), Plan::Ready(Route::Transcode));
        // The original's byte route cannot honor an explicit stream choice,
        // nor can a rendition that does not carry it.
        let explicit = Request {
            explicit_streams: true,
            ..request(Mode::Convert)
        };
        let mut without = ready.clone();
        without.prepared[0].carries_selection = false;
        assert_eq!(
            plan("f", &without, &explicit),
            Plan::Ready(Route::Transcode)
        );
        // A pinned prepared version is never widened to another route.
        let pinned = Request {
            pinned_prepared: true,
            ..request(Mode::Auto)
        };
        assert_eq!(plan("f", &ready, &pinned), Plan::Prepared(0));
        assert_eq!(
            plan("f", &bad, &pinned),
            Plan::Blocked("selected_version_unavailable")
        );
        assert_eq!(
            plan("f", &with(Support::Supported, None), &pinned),
            Plan::Blocked("selected_version_unavailable")
        );
        // An offline original does not hide a revision-valid rendition, but
        // rules out its own byte route and any live conversion.
        let offline = SourceFacts {
            available: false,
            ..ready.clone()
        };
        assert_eq!(plan("f", &offline, &request(Mode::Auto)), Plan::Prepared(0));
        assert_eq!(
            plan(
                "f",
                &with(Support::Supported, None).clone_offline(),
                &request(Mode::Auto)
            ),
            Plan::Blocked("source_unavailable")
        );
        // Every revision-valid rendition reaches the decision: a newer one
        // beyond the client's limit does not hide an older one within it.
        let two = SourceFacts {
            prepared: vec![
                PreparedFacts {
                    within_budget: false,
                    ..prepared(Support::Supported)
                },
                PreparedFacts {
                    file: "h".into(),
                    ..prepared(Support::Supported)
                },
            ],
            ..source(Support::Unsupported)
        };
        assert_eq!(plan("f", &two, &request(Mode::Convert)), Plan::Prepared(1));
        // A rendition that does not carry the selected streams never serves
        // them; one that does serves an explicit choice by byte delivery.
        let mut other_audio = ready.clone();
        other_audio.prepared[0].carries_selection = false;
        assert_eq!(
            plan("f", &other_audio, &request(Mode::Convert)),
            Plan::Ready(Route::Transcode)
        );
        let explicit = Request {
            explicit_streams: true,
            ..request(Mode::Auto)
        };
        assert_eq!(plan("f", &ready, &explicit), Plan::Prepared(0));
        assert_eq!(
            plan("f", &other_audio, &explicit),
            Plan::Ready(Route::Transcode)
        );
    }

    #[test]
    fn conversions_never_exceed_client_limits() {
        assert!(conversion_fits(None, None, Some(720), None));
        assert!(conversion_fits(Some(720), None, Some(720), None));
        assert!(!conversion_fits(Some(480), None, Some(720), None));
        assert!(!conversion_fits(Some(480), None, None, None));
        assert!(!conversion_fits(None, Some(8_000_000), Some(720), None));
        assert!(conversion_fits(
            None,
            Some(3_000_000),
            Some(720),
            Some(2_500_000)
        ));
        let mut over = source(Support::Unsupported);
        over.conversion_within_budget = false;
        assert_eq!(
            plan("f", &over, &request(Mode::Auto)),
            Plan::Blocked("conversion_exceeds_quality_limit")
        );
        // The original still plays when it alone fits.
        assert_eq!(
            plan(
                "f",
                &SourceFacts {
                    conversion_within_budget: false,
                    ..source(Support::Supported)
                },
                &request(Mode::Auto)
            ),
            Plan::Ready(Route::Original)
        );
    }

    #[test]
    fn auto_prefers_an_eligible_stream_copy_over_conversion_and_falls_back_on_failure() {
        let mut auto = request(Mode::Auto);
        auto.explicit_streams = true;
        let mut s = source(Support::Supported);
        for route in [Route::Remux, Route::AudioConvert] {
            s.copy = StreamCopy::Eligible(route);
            assert_eq!(plan("f", &s, &auto), Plan::Ready(route));
            // Unobserved details are observed first, never assumed.
            s.copy = StreamCopy::Unobserved(route);
            assert_eq!(plan("f", &s, &auto), Plan::Observe);
            // A failed copy is never offered again or observed again.
            let mut failed = auto.clone();
            failed.failed = vec![route.candidate_id("f")];
            for copy in [StreamCopy::Eligible(route), StreamCopy::Unobserved(route)] {
                s.copy = copy;
                assert_eq!(plan("f", &s, &failed), Plan::Ready(Route::Transcode));
            }
            failed.failed.push(Route::Transcode.candidate_id("f"));
            assert_eq!(
                plan("f", &s, &failed),
                Plan::Blocked("all_candidates_failed")
            );
        }
        // Byte delivery of the original still wins when it can honor the request.
        s.copy = StreamCopy::Eligible(Route::Remux);
        assert_eq!(
            plan("f", &s, &request(Mode::Auto)),
            Plan::Ready(Route::Original)
        );
        // A copy needs no conversion capability, but still needs HLS.
        s.transcodable = false;
        assert_eq!(plan("f", &s, &auto), Plan::Ready(Route::Remux));
        let mut range_only = auto.clone();
        range_only.hls = false;
        assert_eq!(
            plan("f", &s, &range_only),
            Plan::Blocked("client_cannot_play_conversion")
        );
        s.copy = StreamCopy::Ineligible;
        assert_eq!(plan("f", &s, &auto), Plan::Blocked("no_live_route"));
    }

    #[test]
    fn stream_copy_keeps_source_limits_and_is_never_chosen_by_strict_modes() {
        let mut s = source(Support::Supported);
        s.copy = StreamCopy::Eligible(Route::Remux);
        // The copied video keeps the source size and unknown peak bitrate: a
        // source outside the client limits converts instead.
        s.within_budget = false;
        let auto = request(Mode::Auto);
        assert_eq!(plan("f", &s, &auto), Plan::Ready(Route::Transcode));
        s.copy = StreamCopy::Unobserved(Route::Remux);
        assert_eq!(plan("f", &s, &auto), Plan::Ready(Route::Transcode));
        s.conversion_within_budget = false;
        assert_eq!(
            plan("f", &s, &auto),
            Plan::Blocked("conversion_exceeds_quality_limit")
        );
        // Convert asks for converted video; Original never converts.
        let mut s = source(Support::Unsupported);
        s.copy = StreamCopy::Eligible(Route::AudioConvert);
        assert_eq!(
            plan("f", &s, &request(Mode::Convert)),
            Plan::Ready(Route::Transcode)
        );
        assert_eq!(
            plan("f", &s, &request(Mode::Original)),
            Plan::Blocked("original_unavailable_or_unsupported")
        );
        s.copy = StreamCopy::Unobserved(Route::AudioConvert);
        assert_eq!(
            plan("f", &s, &request(Mode::Convert)),
            Plan::Ready(Route::Transcode)
        );
        // An unavailable original has no copy either.
        let mut offline = s.clone_offline();
        offline.copy = StreamCopy::Eligible(Route::Remux);
        assert_eq!(
            plan("f", &offline, &request(Mode::Auto)),
            Plan::Blocked("source_unavailable")
        );
        // A pinned prepared version is never widened to a copy.
        let mut pinned = request(Mode::Auto);
        pinned.pinned_prepared = true;
        assert_eq!(
            plan("f", &s, &pinned),
            Plan::Blocked("selected_version_unavailable")
        );
    }

    #[test]
    fn copy_routes_map_to_their_operations_and_hls() {
        for (route, operation) in [
            (Route::Remux, Operation::Remux),
            (Route::AudioConvert, Operation::AudioConvert),
        ] {
            assert_eq!(route.operation(), operation);
            assert_eq!(Route::copy(operation), Some(route));
            assert_eq!(route.transport(), "hls");
        }
        for operation in [
            Operation::Original,
            Operation::Prepared,
            Operation::VideoTranscode,
        ] {
            assert_eq!(Route::copy(operation), None);
        }
    }

    #[test]
    fn conversion_prefers_an_available_hardware_session_and_falls_back_to_software() {
        let mut s = source(Support::Unsupported);
        s.hardware = Hardware::Available;
        for mode in [Mode::Auto, Mode::Convert] {
            assert_eq!(
                plan("f", &s, &request(mode)),
                Plan::Ready(Route::HardwareTranscode)
            );
        }
        // A failed hardware attempt is never offered again; software follows.
        let mut failed = request(Mode::Auto);
        failed.failed = vec![Route::HardwareTranscode.candidate_id("f")];
        assert_eq!(plan("f", &s, &failed), Plan::Ready(Route::Transcode));
        failed.failed.push(Route::Transcode.candidate_id("f"));
        assert_eq!(
            plan("f", &s, &failed),
            Plan::Blocked("all_candidates_failed")
        );
        // Busy or unavailable hardware converts in software.
        for hardware in [Hardware::Busy, Hardware::Unavailable] {
            s.hardware = hardware;
            assert_eq!(
                plan("f", &s, &request(Mode::Auto)),
                Plan::Ready(Route::Transcode)
            );
        }
        // Hardware is still a conversion: limits and capability apply alike.
        s.hardware = Hardware::Available;
        s.conversion_within_budget = false;
        assert_eq!(
            plan("f", &s, &request(Mode::Auto)),
            Plan::Blocked("conversion_exceeds_quality_limit")
        );
        s.conversion_within_budget = true;
        s.transcodable = false;
        assert_eq!(
            plan("f", &s, &request(Mode::Auto)),
            Plan::Blocked("no_live_route")
        );
        // An eligible copy still comes before any conversion.
        s.transcodable = true;
        s.copy = StreamCopy::Eligible(Route::Remux);
        assert_eq!(
            plan("f", &s, &request(Mode::Auto)),
            Plan::Ready(Route::Remux)
        );
        assert_eq!(
            Route::HardwareTranscode.operation(),
            Operation::VideoTranscode
        );
        assert_eq!(Route::HardwareTranscode.transport(), "hls");
    }

    #[test]
    fn burned_subtitles_need_a_conversion_never_bytes_or_a_copy() {
        let mut s = source(Support::Supported);
        s.copy = StreamCopy::Eligible(Route::Remux);
        let mut burn = request(Mode::Auto);
        burn.burn_in = true;
        assert_eq!(plan("f", &s, &burn), Plan::Ready(Route::Transcode));
        s.hardware = Hardware::Available;
        assert_eq!(plan("f", &s, &burn), Plan::Ready(Route::HardwareTranscode));
        let mut original = burn.clone();
        original.mode = Mode::Original;
        assert_eq!(
            plan("f", &s, &original),
            Plan::Blocked("subtitle_requires_conversion")
        );
        let mut range_only = burn.clone();
        range_only.hls = false;
        assert_eq!(
            plan("f", &s, &range_only),
            Plan::Blocked("client_cannot_play_conversion")
        );
        // A prepared rendition never carries it either.
        s.prepared = vec![PreparedFacts {
            file: "p".into(),
            client_support: Support::Supported,
            within_budget: true,
            carries_selection: true,
        }];
        assert_eq!(plan("f", &s, &burn), Plan::Ready(Route::HardwareTranscode));
        s.transcodable = false;
        assert_eq!(plan("f", &s, &burn), Plan::Blocked("no_live_route"));
    }

    #[test]
    fn subtitles_are_presented_only_by_routes_that_can_deliver_them() {
        for route in [
            Route::Original,
            Route::Prepared,
            Route::Remux,
            Route::AudioConvert,
            Route::Transcode,
            Route::HardwareTranscode,
        ] {
            assert_eq!(
                presents_subtitle(route, false, false, false),
                route == Route::Original
            );
            assert_eq!(
                presents_subtitle(route, false, true, false),
                route != Route::Prepared,
                "{route:?}"
            );
            // A rendition carrying exactly the selected text subtitle
            // presents it; carrying changes no other route.
            assert_eq!(
                presents_subtitle(route, false, false, true),
                matches!(route, Route::Original | Route::Prepared)
            );
            for live_text in [false, true] {
                assert_eq!(
                    presents_subtitle(route, true, live_text, true),
                    matches!(route, Route::Transcode | Route::HardwareTranscode)
                );
            }
        }
    }

    #[test]
    fn an_interval_is_served_only_by_a_conversion_of_it() {
        let mut s = source(Support::Supported);
        s.clipped = true;
        s.copy = StreamCopy::Eligible(Route::Remux);
        s.prepared = vec![PreparedFacts {
            file: "p".into(),
            client_support: Support::Supported,
            within_budget: true,
            carries_selection: true,
        }];
        assert_eq!(
            plan("f", &s, &request(Mode::Auto)),
            Plan::Ready(Route::Transcode)
        );
        assert_eq!(
            plan("f", &s, &request(Mode::Convert)),
            Plan::Ready(Route::Transcode)
        );
        s.hardware = Hardware::Available;
        assert_eq!(
            plan("f", &s, &request(Mode::Auto)),
            Plan::Ready(Route::HardwareTranscode)
        );
        assert_eq!(
            plan("f", &s, &request(Mode::Original)),
            Plan::Blocked("interval_requires_conversion")
        );
        let mut range_only = request(Mode::Auto);
        range_only.hls = false;
        assert_eq!(
            plan("f", &s, &range_only),
            Plan::Blocked("client_cannot_play_conversion")
        );
    }

    #[test]
    fn several_parts_are_served_only_by_a_conversion() {
        let mut s = source(Support::Supported);
        s.multipart = true;
        s.copy = StreamCopy::Eligible(Route::Remux);
        assert_eq!(
            plan("f", &s, &request(Mode::Auto)),
            Plan::Ready(Route::Transcode)
        );
        assert_eq!(
            plan("f", &s, &request(Mode::Original)),
            Plan::Blocked("multipart_requires_conversion")
        );
    }

    #[test]
    fn capped_conversions_keep_every_peak_window_within_the_limit() {
        let window = 3_000;
        let cap = rate_cap(2_000_000, window).unwrap();
        // 2 Mb/s less 5% and the audio, then 3/4 for the one-second buffer.
        assert_eq!(
            cap.video_max_bps,
            (2_000_000 * 100 / 105 - LIVE_AUDIO_BPS) * 3 / 4
        );
        assert_eq!(cap.video_buffer_bits, cap.video_max_bps);
        // The worst window: the maximum rate plus the whole buffer, the audio
        // and the container allowance stay within the cap.
        let worst = (cap.video_max_bps * window / 1000 + cap.video_buffer_bits) * 1000 / window
            + LIVE_AUDIO_BPS;
        assert!(worst * 105 / 100 <= 2_000_000, "{worst}");
        // Too little for video is refused, never squeezed.
        assert_eq!(rate_cap(500_000, window), None);
        assert!(rate_cap(700_000, window).is_some());
        assert_eq!(rate_cap(0, window), None);
        assert_eq!(rate_cap(u64::MAX, window), None);
        assert!(within_cap(1_000_000, 4_000, 2_000_000));
        assert!(!within_cap(1_000_001, 4_000, 2_000_000));
    }

    #[test]
    fn peak_bitrate_is_checked_over_every_qualifying_window() {
        let cap = 1_000_000;
        // A compliant 4 s segment then a 2 s tail that alone is too short to
        // count, but whose 6 s window with its predecessor is over the cap.
        assert!(peak_within_cap(&[(450_000, 4_000)], cap, 3_000, 9_000));
        assert!(!peak_within_cap(
            &[(450_000, 4_000), (400_000, 2_000)],
            cap,
            3_000,
            9_000
        ));
        // A short segment alone is not a window.
        assert!(peak_within_cap(&[(400_000, 2_000)], cap, 3_000, 9_000));
        // Windows longer than 1.5 targets are not counted: the oldest segment
        // here only joins a 12 s run.
        assert!(peak_within_cap(
            &[(9_000_000, 4_000), (400_000, 4_000), (400_000, 4_000)],
            cap,
            3_000,
            9_000
        ));
        assert!(!peak_within_cap(&[(500_001, 4_000)], cap, 3_000, 9_000));
    }

    #[test]
    fn peak_bitrate_is_the_highest_qualifying_window() {
        assert_eq!(peak_bitrate(&[(400_000, 2_000)], 3_000, 9_000), 0);
        // 4 s at 0.9 Mb/s, then with a 2 s tail: 6 s at (450k+400k)*8/6 s.
        assert_eq!(peak_bitrate(&[(450_000, 4_000)], 3_000, 9_000), 900_000);
        assert_eq!(
            peak_bitrate(&[(450_000, 4_000), (400_000, 2_000)], 3_000, 9_000),
            850_000 * 8_000 / 6_000
        );
    }

    #[test]
    fn claims_signed_before_subtitle_kinds_still_open() {
        let current = Claims {
            subtitle: Some(Subtitle::Text(3)),
            ..claims()
        };
        let json = serde_json::to_string(&current).unwrap();
        assert!(json.contains("\"subtitle\":{\"text\":3}"), "{json}");
        let legacy = json.replace("\"subtitle\":{\"text\":3}", "\"subtitle\":3");
        assert_eq!(serde_json::from_str::<Claims>(&legacy).unwrap(), current);
        assert_eq!(serde_json::from_str::<Claims>(&json).unwrap(), current);
        let none = serde_json::to_string(&claims()).unwrap();
        assert_eq!(serde_json::from_str::<Claims>(&none).unwrap(), claims());
    }

    #[test]
    fn adaptive_ladders_only_for_uncapped_single_file_software_conversions() {
        assert!(adaptive(true, Route::Transcode, None, false, false));
        assert!(!adaptive(false, Route::Transcode, None, false, false));
        for route in [
            Route::Original,
            Route::Prepared,
            Route::Remux,
            Route::AudioConvert,
            Route::HardwareTranscode,
        ] {
            assert!(!adaptive(true, route, None, false, false), "{route:?}");
        }
        assert!(!adaptive(
            true,
            Route::Transcode,
            Some(5_000_000),
            false,
            false
        ));
        assert!(!adaptive(true, Route::Transcode, None, true, false));
        assert!(!adaptive(true, Route::Transcode, None, false, true));
        // Both rungs leave room for video under their caps.
        assert!(rate_cap(ADAPTIVE_MAIN_BPS, 3_000).is_some());
        assert!(rate_cap(ADAPTIVE_LOW_BPS, 3_000).is_some());
    }
}
