//! Identification workflow: proposals, operator decisions and provider refresh.
//!
//! Parser output and provider candidates are evidence, never write permission.
//! A decision is accepted only against the exact proposal revision and file
//! revision the operator reviewed, and a refresh never overrides a decision.
use serde::{Deserialize, Serialize};

pub type Id = String;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    /// One plausible candidate (or none) awaiting review.
    Pending,
    /// Several plausible candidates; never auto-accepted.
    Review,
    Accepted,
    Rejected,
    Deferred,
    /// The file revision changed; this proposal can no longer be decided.
    Stale,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Candidate {
    pub id: Id,
    /// Existing local work, if the candidate is already cataloged.
    pub item_id: Option<Id>,
    pub title: String,
    /// Normalized `(namespace, value)` provider identity.
    pub external_id: Option<(String, String)>,
    pub reason_codes: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "decision", rename_all = "snake_case")]
pub enum Decision {
    Accept { candidate_id: Id },
    Reject,
    Defer,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Proposal {
    pub id: Id,
    pub revision: u64,
    pub file_id: Id,
    pub file_revision: String,
    pub status: Status,
    pub candidates: Vec<Candidate>,
    pub decision: Option<Decision>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum MatchError {
    StaleProposal,
    FileChanged,
    AlreadyDecided,
    UnknownCandidate(Id),
    RevisionExhausted,
    TooManyCandidates,
}

/// The accepted identification the adapter must apply in the same transaction.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Identification {
    pub file_id: Id,
    pub file_revision: String,
    pub candidate: Candidate,
}

pub const MAX_CANDIDATES: usize = 50;

pub fn initial_status(candidates: &[Candidate]) -> Status {
    if candidates.len() > 1 {
        Status::Review
    } else {
        Status::Pending
    }
}

pub fn propose(
    id: Id,
    file_id: Id,
    file_revision: String,
    candidates: Vec<Candidate>,
) -> Result<Proposal, MatchError> {
    if candidates.len() > MAX_CANDIDATES {
        return Err(MatchError::TooManyCandidates);
    }
    Ok(Proposal {
        id,
        revision: 1,
        file_id,
        file_revision,
        status: initial_status(&candidates),
        candidates,
        decision: None,
    })
}

fn decided(status: Status) -> bool {
    matches!(status, Status::Accepted | Status::Rejected)
}

/// Apply an operator decision. `current_file_revision` is the file's revision
/// observed inside the writer transaction (`None` if the file record is gone).
pub fn decide(
    proposal: &Proposal,
    expected_revision: u64,
    current_file_revision: Option<&str>,
    decision: Decision,
) -> Result<(Proposal, Option<Identification>), MatchError> {
    if decided(proposal.status) {
        return Err(MatchError::AlreadyDecided);
    }
    if proposal.status == Status::Stale || expected_revision != proposal.revision {
        return Err(MatchError::StaleProposal);
    }
    if current_file_revision != Some(proposal.file_revision.as_str()) {
        return Err(MatchError::FileChanged);
    }
    let mut next = proposal.clone();
    next.revision = proposal
        .revision
        .checked_add(1)
        .ok_or(MatchError::RevisionExhausted)?;
    let identification = match &decision {
        Decision::Accept { candidate_id } => {
            let candidate = proposal
                .candidates
                .iter()
                .find(|c| &c.id == candidate_id)
                .ok_or_else(|| MatchError::UnknownCandidate(candidate_id.clone()))?;
            next.status = Status::Accepted;
            Some(Identification {
                file_id: proposal.file_id.clone(),
                file_revision: proposal.file_revision.clone(),
                candidate: candidate.clone(),
            })
        }
        Decision::Reject => {
            next.status = Status::Rejected;
            None
        }
        Decision::Defer => {
            next.status = Status::Deferred;
            None
        }
    };
    next.decision = Some(decision);
    Ok((next, identification))
}

/// Provider refresh or re-parse. Decided proposals are never changed: a fix-match
/// survives refresh. Undecided ones take the new candidates and a new revision,
/// so any decision reviewed against the old candidates is rejected as stale.
/// A changed file revision makes the proposal stale; a new one must be raised.
pub fn refresh(
    proposal: &Proposal,
    current_file_revision: Option<&str>,
    candidates: Vec<Candidate>,
) -> Result<Proposal, MatchError> {
    if decided(proposal.status) || proposal.status == Status::Stale {
        return Ok(proposal.clone());
    }
    if candidates.len() > MAX_CANDIDATES {
        return Err(MatchError::TooManyCandidates);
    }
    let mut next = proposal.clone();
    if current_file_revision != Some(proposal.file_revision.as_str()) {
        next.status = Status::Stale;
    } else if candidates == proposal.candidates {
        return Ok(proposal.clone());
    } else {
        next.status = if proposal.status == Status::Deferred {
            Status::Deferred
        } else {
            initial_status(&candidates)
        };
        next.candidates = candidates;
    }
    next.revision = proposal
        .revision
        .checked_add(1)
        .ok_or(MatchError::RevisionExhausted)?;
    Ok(next)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MatchState {
    Unmatched,
    Ambiguous,
    Matched,
    Manual,
}

/// May an automatic provider contribution set (`Some`) or withdraw (`None`) a
/// work's identity in a namespace? `current` is the work's identity in that
/// namespace. A manual identification pins it: a disagreeing value or a removal
/// is refused (and can be retained as a conflict), never applied. Namespaces
/// without a pinned identity stay open.
pub fn provider_identity_allowed(
    state: MatchState,
    current: Option<&(String, String)>,
    incoming: Option<&(String, String)>,
) -> bool {
    match (state, current) {
        (MatchState::Manual, Some(current)) => incoming == Some(current),
        _ => true,
    }
}

/// Match state of a merged work. Merge carries every source identity to the
/// target (conflicting identities block the merge), so the strongest state,
/// including a manual pin, moves with it.
pub fn merged_state(states: &[MatchState]) -> MatchState {
    let rank = |s: &MatchState| match s {
        MatchState::Unmatched => 0,
        MatchState::Ambiguous => 1,
        MatchState::Matched => 2,
        MatchState::Manual => 3,
    };
    states
        .iter()
        .copied()
        .max_by_key(rank)
        .unwrap_or(MatchState::Unmatched)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProposalAction {
    /// The latest proposal already decided this exact file revision; keep it.
    /// Reopening a decision is a separate explicit operation.
    Keep,
    /// Refresh the open proposal for this file revision.
    Refresh,
    /// Raise a new proposal (none yet, or the file revision changed).
    Raise,
}
pub fn proposal_action(latest: Option<&Proposal>, file_revision: &str) -> ProposalAction {
    match latest {
        Some(p) if p.file_revision == file_revision => match p.status {
            Status::Accepted | Status::Rejected => ProposalAction::Keep,
            Status::Pending | Status::Review | Status::Deferred => ProposalAction::Refresh,
            Status::Stale => ProposalAction::Raise,
        },
        _ => ProposalAction::Raise,
    }
}

// ---------------------------------------------------------------------------
// Deterministic filename parsing (evidence only)

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Parsed {
    pub title: String,
    pub year: Option<u16>,
    pub season: Option<u32>,
    pub episodes: Vec<u32>,
    pub edition: Option<String>,
}

const STOP_TOKENS: &[&str] = &[
    "2160p", "1080p", "1080i", "720p", "576p", "480p", "4k", "uhd", "hdr", "hdr10", "dv", "bluray",
    "blu-ray", "bdrip", "brrip", "webrip", "web-dl", "webdl", "web", "hdtv", "dvdrip", "remux",
    "x264", "x265", "h264", "h265", "hevc", "avc", "aac", "ac3", "dts", "atmos", "truehd", "10bit",
    "proper", "repack",
];
const EDITIONS: &[(&str, &str)] = &[
    ("directors cut", "Director's Cut"),
    ("director's cut", "Director's Cut"),
    ("extended", "Extended"),
    ("theatrical", "Theatrical"),
    ("unrated", "Unrated"),
    ("final cut", "Final Cut"),
    ("remastered", "Remastered"),
];

/// Parse a file stem. Bracketed groups, release tokens and separators are
/// dropped; `SxxEyy[Ezz]` and a plausible year end the title.
pub fn parse_name(stem: &str) -> Parsed {
    // Tokens with their bracket depth; separators are whitespace, '.' and '_'.
    let mut tokens: Vec<(String, bool)> = Vec::new();
    let mut current = String::new();
    let mut depth = 0u32;
    let flush = |current: &mut String, bracketed: bool, tokens: &mut Vec<(String, bool)>| {
        if !current.is_empty() {
            tokens.push((std::mem::take(current), bracketed));
        }
    };
    for ch in stem.chars() {
        match ch {
            '[' | '(' | '{' => {
                flush(&mut current, depth > 0, &mut tokens);
                depth += 1;
            }
            ']' | ')' | '}' => {
                flush(&mut current, depth > 0, &mut tokens);
                depth = depth.saturating_sub(1);
            }
            c if c.is_whitespace() || c == '.' || c == '_' => {
                flush(&mut current, depth > 0, &mut tokens)
            }
            c => current.push(c),
        }
    }
    flush(&mut current, depth > 0, &mut tokens);
    let lower_all = tokens
        .iter()
        .map(|(t, _)| t.to_lowercase())
        .collect::<Vec<_>>()
        .join(" ");
    let mut parsed = Parsed::default();
    for (needle, label) in EDITIONS {
        if lower_all.contains(needle) {
            parsed.edition = Some((*label).into());
            break;
        }
    }
    let mut title: Vec<&str> = Vec::new();
    let mut ended = false;
    for (token, bracketed) in &tokens {
        let lower = token.to_lowercase();
        if let Some((season, episodes)) = episode_token(&lower) {
            if parsed.season.is_none() {
                parsed.season = Some(season);
                parsed.episodes = episodes;
            }
            ended = true;
            continue;
        }
        // A plausible year after some title text ends the title, bracketed or not.
        if let Ok(year) = lower.parse::<u16>()
            && (1880..=2100).contains(&year)
            && lower.len() == 4
            && !title.is_empty()
            && parsed.year.is_none()
            && parsed.season.is_none()
        {
            parsed.year = Some(year);
            ended = true;
            continue;
        }
        if STOP_TOKENS.contains(&lower.as_str()) {
            ended = true;
        }
        if ended || *bracketed || token == "-" {
            continue;
        }
        title.push(token);
    }
    parsed.title = title.join(" ");
    if parsed.title.is_empty() {
        parsed.title = stem.trim().to_string();
    }
    parsed
}

fn episode_token(token: &str) -> Option<(u32, Vec<u32>)> {
    let rest = token.strip_prefix('s')?;
    let e = rest.find('e')?;
    let season: u32 = rest[..e].parse().ok().filter(|_| (1..=4).contains(&e))?;
    let episodes: Option<Vec<u32>> = rest[e + 1..]
        .split(['e', '-'])
        .filter(|p| !p.is_empty())
        .map(|p| p.parse().ok().filter(|_| p.len() <= 4))
        .collect();
    let episodes = episodes.filter(|v| !v.is_empty())?;
    Some((season, episodes))
}

/// Deterministic comparison key: case-folded alphanumerics separated by single
/// spaces. Display text is never replaced by this key.
pub fn normalize_title(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut space = false;
    for ch in text.chars().flat_map(char::to_lowercase) {
        if ch.is_alphanumeric() {
            if space && !out.is_empty() {
                out.push(' ');
            }
            space = false;
            out.push(ch);
        } else if !matches!(ch, '\'' | '\u{2019}' | '\u{2018}' | '\u{02BC}') {
            // Apostrophe variants (straight, curly, modifier) are elisions.
            space = true;
        }
    }
    out
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Attachment {
    /// The file already belongs to the chosen work.
    AlreadyIdentified,
    /// The file's work holds only this file's version: merge it into the target.
    MergeWork,
    /// The file shares its work with other versions; an explicit split must
    /// come first so unrelated versions are never re-identified with it.
    SplitRequired,
}
/// Accepting a local-work candidate re-identifies the file's work only when the
/// file is that work's sole version.
pub fn attachment(file_item: &str, target_item: &str, versions_in_file_item: usize) -> Attachment {
    if file_item == target_item {
        Attachment::AlreadyIdentified
    } else if versions_in_file_item == 1 {
        Attachment::MergeWork
    } else {
        Attachment::SplitRequired
    }
}
