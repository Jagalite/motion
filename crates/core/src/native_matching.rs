//! Descriptive native observations and pure local matching workflow decisions.
//!
//! Nothing serialized here is a grant, a completeness proof, a native receipt,
//! or byte authority. An adapter must obtain the complete observations from the
//! retained native owner, independently authorize the caller, and retain its
//! writer/native custody through commit. This module creates no catalog effect.
//! Acceptance requires a separate atomic catalog owner and remains unavailable.

use crate::{
    access::{CatalogScope, may_curate},
    matching::{self, Candidate, Decision, MatchError, Proposal, ProposalAction, Status},
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;

pub const CONTRACT_VERSION: u32 = 1;
pub const MAX_CORPUS_ROWS: usize = 10_000;
pub const MAX_BYTES: usize = 2 * 1024 * 1024;
pub const MAX_NODES: usize = 8192;
pub const MAX_WORK: usize = 8192;
pub const MAX_DEPTH: usize = 32;
const MAX_ID_BYTES: usize = 256;
const MAX_TEXT_BYTES: usize = 4096;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CatalogIdentity {
    pub catalog_id: String,
    pub incarnation: String,
    pub profile: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Fence {
    pub identity: CatalogIdentity,
    pub generation: i64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Head {
    pub id: String,
    pub revision: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExactFile {
    pub id: String,
    pub revision: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Completeness {
    Complete,
    Incomplete,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FileOrigin {
    Original,
    Generated,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Presence {
    Present,
    Missing,
    Unknown,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkState {
    Live,
    Retired,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceEvidence {
    pub source_id: String,
    pub mapping_digest: String,
    pub binding_digest: String,
    pub policy_digest: String,
    pub occurrence_digest: String,
}

/// Complete ownership paths, not a selected visible path. This initial cohort
/// requires one unambiguous original-file path; multi-owner files are refused.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OwnerPath {
    pub work: Head,
    pub work_state: WorkState,
    pub context: Head,
    pub timeline: Head,
    pub version: Head,
    pub association: Head,
    pub binding: Head,
    pub work_closure_digest: String,
    pub context_closure_digest: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FileLibrary {
    pub library: Head,
    pub definition_digest: String,
    pub scope_digest: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FileObservation {
    pub file_id: String,
    /// Original Motion wire revision, independently bridged to original_native.
    pub file_revision: String,
    pub original_native: ExactFile,
    pub observed_native: Option<ExactFile>,
    pub relative_path: String,
    pub origin: FileOrigin,
    pub presence: Presence,
    pub source: SourceEvidence,
    pub ownership_complete: Completeness,
    pub owner_paths: Vec<OwnerPath>,
    /// ALL current logical libraries containing this file's source, not all
    /// libraries of its work and not a principal-filtered membership list.
    pub memberships_complete: Completeness,
    pub file_libraries: Vec<FileLibrary>,
}

/// The adapter supplies every source document's observed release_year. Missing
/// is distinct from a later source's year. Noninteger legacy SQL values must
/// fail in the adapter rather than be silently coerced into this contract.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReleaseYearEvidence {
    pub source: String,
    pub revision: u64,
    pub document_digest: String,
    pub release_year: Option<i64>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LocalWork {
    pub work: Head,
    pub state: WorkState,
    pub title: String,
    pub metadata_digest: String,
    pub years_complete: Completeness,
    /// Unique, ascending source names. Selection uses local first, then source.
    pub years: Vec<ReleaseYearEvidence>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CandidateCorpus {
    pub completeness: Completeness,
    /// Descriptive complete selection provenance, not permission to read it.
    pub selection_digest: String,
    /// All current eligible local works in ascending work-ID order, including
    /// the subject if selected. Never the first N matching/visible works.
    pub works: Vec<LocalWork>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Observation {
    pub contract_version: u32,
    pub fence: Fence,
    pub file: FileObservation,
    pub corpus: CandidateCorpus,
}

/// Reject/defer need current file custody, not candidate discovery. In
/// particular, corpus growth cannot prevent rejecting a reviewed proposal.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DecisionObservation {
    pub contract_version: u32,
    pub fence: Fence,
    pub file: FileObservation,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CandidateEvidence {
    pub candidate: Candidate,
    pub work: Head,
    pub metadata_digest: String,
    pub selected_year: Option<ReleaseYearEvidence>,
}

/// Immutable per-review evidence. Global generation and unrelated/nonmatching
/// corpus rows are intentionally absent from its semantic digest.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Evidence {
    pub identity: CatalogIdentity,
    pub file: FileObservation,
    pub candidates: Vec<CandidateEvidence>,
    pub digest: String,
}

/// Descriptive pending work, never a native receipt. Only a separately qualified
/// owner may establish or reconcile it. Neither planner clears or replaces it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PendingIntent {
    pub operation_id: String,
    pub envelope_digest: String,
    pub proposal_revision: u64,
    pub evidence_digest: String,
    pub decision: Decision,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Workflow {
    pub contract_version: u32,
    pub proposal: Proposal,
    pub origin: Evidence,
    pub reviewed: Evidence,
    /// Last persisted review/decision observation; not a semantic digest salt.
    /// A generation-only create no-op does not write a new workflow revision.
    pub reviewed_generation: i64,
    pub pending: Option<PendingIntent>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProposalHead {
    pub id: String,
    pub revision: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CreateRequest {
    /// Used only if a new proposal is raised; cannot recycle the prior ID.
    pub new_proposal_id: String,
    pub expected: Option<ProposalHead>,
    pub file_id: String,
    pub file_revision: String,
    pub library_id: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DecisionRequest {
    pub expected: ProposalHead,
    pub decision: Decision,
}

/// Commit all writes and the HTTP ACK together, or none. A superseded open
/// proposal precedes its replacement in writes. Empty writes means no change.
/// The adapter also fences expected/latest absence and every observed native
/// dependency; serialized plans confer no execution permission.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct WorkflowPlan {
    pub fence: Fence,
    pub expected: Option<ProposalHead>,
    pub writes: Vec<Workflow>,
    pub result: Workflow,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Refusal {
    UnsupportedContract,
    InvalidInput,
    InvalidObservation,
    IncompleteObservation,
    AmbiguousOwnership,
    GeneratedFile,
    RetiredWork,
    FileUnavailable,
    FileChanged,
    OutsideFileScope,
    StaleProposal,
    StaleObservation,
    RecoveryRequired,
    AcceptUnavailable,
    CandidateOverflow,
    ByteBudget,
    NodeBudget,
    DepthBudget,
    WorkBudget,
    Matching(MatchError),
}
impl std::fmt::Display for Refusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "native matching workflow refused: {self:?}")
    }
}
impl std::error::Error for Refusal {}
impl From<MatchError> for Refusal {
    fn from(value: MatchError) -> Self {
        Self::Matching(value)
    }
}
pub type Result<T> = std::result::Result<T, Refusal>;

pub fn plan_create(
    current: Option<&Workflow>,
    observation: &Observation,
    scope: &CatalogScope,
    request: &CreateRequest,
) -> Result<WorkflowPlan> {
    let mut budget = Budget::default();
    budget.charge(&(current, observation, scope, request))?;
    validate_observation(observation, scope)?;
    identifier(&request.new_proposal_id)?;
    if request.file_id != observation.file.file_id
        || request.file_revision != observation.file.file_revision
    {
        return Err(Refusal::FileChanged);
    }
    if !observation
        .file
        .file_libraries
        .iter()
        .any(|v| v.library.id == request.library_id)
    {
        return Err(Refusal::OutsideFileScope);
    }
    if let Some(current) = current {
        validate_workflow(current, &mut budget)?;
    }
    if current.map(workflow_head) != request.expected {
        return Err(Refusal::StaleProposal);
    }
    if let Some(current) = current {
        admit_continuity(current, &observation.fence, &observation.file)?;
        if current.pending.is_some() {
            return Err(Refusal::RecoveryRequired);
        }
        if matching::proposal_action(Some(&current.proposal), &observation.file.file_revision)
            == ProposalAction::Keep
        {
            return finish(
                &observation.fence,
                current,
                vec![],
                request.expected.clone(),
            );
        }
    }
    let evidence = evidence(observation, &mut budget)?;
    let candidates = evidence
        .candidates
        .iter()
        .map(|v| v.candidate.clone())
        .collect();
    let mut writes = Vec::new();
    let next = if let Some(current) = current.filter(|v| {
        matching::proposal_action(Some(&v.proposal), &observation.file.file_revision)
            == ProposalAction::Refresh
    }) {
        let mut next = current.clone();
        next.proposal = matching::refresh(
            &current.proposal,
            Some(&observation.file.file_revision),
            candidates,
        )?;
        if evidence.digest != current.reviewed.digest {
            if next.proposal.revision == current.proposal.revision {
                next.proposal.revision = next
                    .proposal
                    .revision
                    .checked_add(1)
                    .ok_or(MatchError::RevisionExhausted)?;
            }
            next.reviewed = evidence;
            next.reviewed_generation = observation.fence.generation;
        }
        if &next != current {
            writes.push(next.clone());
        }
        next
    } else {
        if let Some(current) = current {
            if request.new_proposal_id == current.proposal.id {
                return Err(Refusal::InvalidInput);
            }
            let mut superseded = current.clone();
            superseded.proposal = matching::refresh(
                &current.proposal,
                Some(&observation.file.file_revision),
                current.proposal.candidates.clone(),
            )?;
            if &superseded != current {
                writes.push(superseded);
            }
        }
        let next = Workflow {
            contract_version: CONTRACT_VERSION,
            proposal: matching::propose(
                request.new_proposal_id.clone(),
                request.file_id.clone(),
                request.file_revision.clone(),
                candidates,
            )?,
            origin: evidence.clone(),
            reviewed: evidence,
            reviewed_generation: observation.fence.generation,
            pending: None,
        };
        writes.push(next.clone());
        next
    };
    finish(&observation.fence, &next, writes, request.expected.clone())
}

pub fn plan_decide(
    current: &Workflow,
    observation: &DecisionObservation,
    scope: &CatalogScope,
    request: &DecisionRequest,
) -> Result<WorkflowPlan> {
    let mut budget = Budget::default();
    budget.charge(&(current, observation, scope, request))?;
    validate_current_file(
        observation.contract_version,
        &observation.fence,
        &observation.file,
        scope,
    )?;
    validate_workflow(current, &mut budget)?;
    if workflow_head(current) != request.expected {
        return Err(Refusal::StaleProposal);
    }
    admit_continuity(current, &observation.fence, &observation.file)?;
    if current.pending.is_some() {
        return Err(Refusal::RecoveryRequired);
    }
    if matches!(request.decision, Decision::Accept { .. }) {
        return Err(Refusal::AcceptUnavailable);
    }
    if current.proposal.file_revision != observation.file.file_revision {
        return Err(Refusal::FileChanged);
    }
    // Reject/defer do not identify a work or apply reviewed candidate evidence.
    // Current complete custody/scope is required, but unrelated work/context
    // heads and even a newly authorized owner path need not match the old review.
    let (proposal, effect) = matching::decide(
        &current.proposal,
        request.expected.revision,
        Some(&observation.file.file_revision),
        request.decision.clone(),
    )?;
    debug_assert!(effect.is_none());
    let mut next = current.clone();
    next.proposal = proposal;
    next.reviewed_generation = observation.fence.generation;
    finish(
        &observation.fence,
        &next,
        vec![next.clone()],
        Some(request.expected.clone()),
    )
}

fn finish(
    fence: &Fence,
    result: &Workflow,
    writes: Vec<Workflow>,
    expected: Option<ProposalHead>,
) -> Result<WorkflowPlan> {
    if writes
        .iter()
        .chain(std::iter::once(result))
        .any(|value| value.proposal.revision > i64::MAX as u64)
    {
        return Err(MatchError::RevisionExhausted.into());
    }
    let plan = WorkflowPlan {
        fence: fence.clone(),
        expected,
        writes,
        result: result.clone(),
    };
    Budget::default().charge(&plan)?;
    Ok(plan)
}
fn workflow_head(workflow: &Workflow) -> ProposalHead {
    ProposalHead {
        id: workflow.proposal.id.clone(),
        revision: workflow.proposal.revision,
    }
}
fn identifier(value: &str) -> Result<()> {
    if value.is_empty()
        || value.len() > MAX_ID_BYTES
        || value.trim() != value
        || value.chars().any(char::is_control)
    {
        Err(Refusal::InvalidInput)
    } else {
        Ok(())
    }
}
fn digest(value: &str) -> Result<()> {
    if value.len() != 64
        || !value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        Err(Refusal::InvalidObservation)
    } else {
        Ok(())
    }
}
fn head(value: &Head) -> Result<()> {
    identifier(&value.id)?;
    if value.revision == 0 || value.revision > i64::MAX as u64 {
        Err(Refusal::InvalidObservation)
    } else {
        Ok(())
    }
}
fn identity(value: &CatalogIdentity) -> Result<()> {
    identifier(&value.catalog_id)?;
    identifier(&value.incarnation)?;
    identifier(&value.profile)
}
fn complete(value: Completeness) -> Result<()> {
    if value == Completeness::Complete {
        Ok(())
    } else {
        Err(Refusal::IncompleteObservation)
    }
}

fn validate_file(file: &FileObservation) -> Result<()> {
    for id in [
        &file.file_id,
        &file.file_revision,
        &file.original_native.id,
        &file.original_native.revision,
        &file.source.source_id,
    ] {
        identifier(id)?;
    }
    if file.relative_path.is_empty()
        || file.relative_path.len() > MAX_TEXT_BYTES
        || file.relative_path.chars().any(char::is_control)
    {
        return Err(Refusal::InvalidObservation);
    }
    if file.origin != FileOrigin::Original {
        return Err(Refusal::GeneratedFile);
    }
    if file.presence != Presence::Present {
        return Err(Refusal::FileUnavailable);
    }
    if file.observed_native.as_ref() != Some(&file.original_native) {
        return Err(Refusal::FileChanged);
    }
    for value in [
        &file.source.mapping_digest,
        &file.source.binding_digest,
        &file.source.policy_digest,
        &file.source.occurrence_digest,
    ] {
        digest(value)?;
    }
    complete(file.ownership_complete)?;
    if file.owner_paths.len() != 1 {
        return Err(Refusal::AmbiguousOwnership);
    }
    for path in &file.owner_paths {
        for value in [
            &path.work,
            &path.context,
            &path.timeline,
            &path.version,
            &path.association,
            &path.binding,
        ] {
            head(value)?;
        }
        if path.work_state != WorkState::Live {
            return Err(Refusal::RetiredWork);
        }
        digest(&path.work_closure_digest)?;
        digest(&path.context_closure_digest)?;
    }
    complete(file.memberships_complete)?;
    if file.file_libraries.is_empty() {
        return Err(Refusal::IncompleteObservation);
    }
    let mut previous = None;
    for library in &file.file_libraries {
        head(&library.library)?;
        digest(&library.definition_digest)?;
        digest(&library.scope_digest)?;
        if previous.is_some_and(|id| id >= library.library.id.as_str()) {
            return Err(Refusal::InvalidObservation);
        }
        previous = Some(library.library.id.as_str());
    }
    Ok(())
}

fn validate_current_file(
    contract_version: u32,
    fence: &Fence,
    file: &FileObservation,
    scope: &CatalogScope,
) -> Result<()> {
    if contract_version != CONTRACT_VERSION {
        return Err(Refusal::UnsupportedContract);
    }
    identity(&fence.identity)?;
    if fence.generation < 0 {
        return Err(Refusal::InvalidObservation);
    }
    validate_file(file)?;
    let libraries = file
        .file_libraries
        .iter()
        .map(|v| v.library.id.clone())
        .collect::<BTreeSet<_>>();
    if !may_curate(scope, &libraries) {
        return Err(Refusal::OutsideFileScope);
    }
    Ok(())
}
fn validate_observation(observation: &Observation, scope: &CatalogScope) -> Result<()> {
    validate_current_file(
        observation.contract_version,
        &observation.fence,
        &observation.file,
        scope,
    )?;
    complete(observation.corpus.completeness)?;
    digest(&observation.corpus.selection_digest)?;
    if observation.corpus.works.len() > MAX_CORPUS_ROWS {
        return Err(Refusal::WorkBudget);
    }
    let mut previous = None;
    for work in &observation.corpus.works {
        head(&work.work)?;
        if previous.is_some_and(|id| id >= work.work.id.as_str()) {
            return Err(Refusal::InvalidObservation);
        }
        previous = Some(work.work.id.as_str());
        if work.state != WorkState::Live {
            return Err(Refusal::RetiredWork);
        }
        if work.title.len() > MAX_TEXT_BYTES {
            return Err(Refusal::InvalidObservation);
        }
        digest(&work.metadata_digest)?;
        complete(work.years_complete)?;
        let mut previous = None;
        for year in &work.years {
            identifier(&year.source)?;
            if year.revision == 0
                || year.revision > i64::MAX as u64
                || previous.is_some_and(|source| source >= year.source.as_str())
            {
                return Err(Refusal::InvalidObservation);
            }
            digest(&year.document_digest)?;
            previous = Some(year.source.as_str());
        }
        let owner = &observation.file.owner_paths[0].work;
        if work.work.id == owner.id && work.work != *owner {
            return Err(Refusal::InvalidObservation);
        }
    }
    Ok(())
}

fn evidence(observation: &Observation, budget: &mut Budget) -> Result<Evidence> {
    let file = &observation.file;
    budget.work(file.relative_path.len())?;
    let stem = std::path::Path::new(&file.relative_path)
        .file_stem()
        .and_then(|v| v.to_str())
        .unwrap_or(&file.relative_path);
    let parsed = matching::parse_name(stem);
    let wanted = matching::normalize_title(&parsed.title);
    let own_work = &file.owner_paths[0].work.id;
    let mut candidates = Vec::new();
    if !wanted.is_empty() {
        for work in &observation.corpus.works {
            budget.work(
                work.title
                    .len()
                    .saturating_add(work.years.len())
                    .saturating_add(1),
            )?;
            if &work.work.id == own_work || matching::normalize_title(&work.title) != wanted {
                continue;
            }
            let selected_year = work
                .years
                .iter()
                .filter(|y| y.release_year.is_some())
                .min_by(|a, b| {
                    (a.source != "local", &a.source).cmp(&(b.source != "local", &b.source))
                })
                .cloned();
            let year = selected_year.as_ref().and_then(|v| v.release_year);
            let mut reasons = vec!["title".to_owned()];
            match (parsed.year, year) {
                (Some(a), Some(b)) if i64::from(a) == b => reasons.push("year".into()),
                (Some(_), Some(_)) => continue,
                _ => {}
            }
            if candidates.len() == matching::MAX_CANDIDATES {
                return Err(Refusal::CandidateOverflow);
            }
            if work.work.id.len() + "local:".len() > MAX_ID_BYTES {
                return Err(Refusal::InvalidObservation);
            }
            candidates.push(CandidateEvidence {
                candidate: Candidate {
                    id: format!("local:{}", work.work.id),
                    item_id: Some(work.work.id.clone()),
                    title: work.title.clone(),
                    external_id: None,
                    reason_codes: reasons,
                },
                work: work.work.clone(),
                metadata_digest: work.metadata_digest.clone(),
                selected_year,
            });
        }
    }
    let mut evidence = Evidence {
        identity: observation.fence.identity.clone(),
        file: file.clone(),
        candidates,
        digest: String::new(),
    };
    evidence.digest = evidence_digest(&evidence)?;
    Ok(evidence)
}

fn evidence_digest(value: &Evidence) -> Result<String> {
    let bytes = serde_json::to_vec(&(
        "motion.native_matching.evidence.v1",
        &value.identity,
        &value.file,
        &value.candidates,
    ))
    .map_err(|_| Refusal::InvalidObservation)?;
    Ok(format!("{:x}", Sha256::digest(bytes)))
}
fn validate_evidence(value: &Evidence, budget: &mut Budget) -> Result<()> {
    identity(&value.identity)?;
    validate_file(&value.file)?;
    if value.candidates.len() > matching::MAX_CANDIDATES || value.digest != evidence_digest(value)?
    {
        return Err(Refusal::InvalidObservation);
    }
    budget.work(value.file.relative_path.len())?;
    let stem = std::path::Path::new(&value.file.relative_path)
        .file_stem()
        .and_then(|v| v.to_str())
        .unwrap_or(&value.file.relative_path);
    let parsed = matching::parse_name(stem);
    let wanted = matching::normalize_title(&parsed.title);
    let mut previous = None;
    for c in &value.candidates {
        head(&c.work)?;
        digest(&c.metadata_digest)?;
        identifier(&c.candidate.id)?;
        if c.candidate.title.len() > MAX_TEXT_BYTES {
            return Err(Refusal::InvalidObservation);
        }
        budget.work(c.candidate.title.len().saturating_add(1))?;
        if c.candidate.id != format!("local:{}", c.work.id)
            || c.candidate.item_id.as_ref() != Some(&c.work.id)
            || c.candidate.external_id.is_some()
            || c.work.id == value.file.owner_paths[0].work.id
            || previous.is_some_and(|id| id >= c.work.id.as_str())
            || wanted.is_empty()
            || matching::normalize_title(&c.candidate.title) != wanted
        {
            return Err(Refusal::InvalidObservation);
        }
        let year = if let Some(year) = &c.selected_year {
            identifier(&year.source)?;
            digest(&year.document_digest)?;
            if year.revision == 0 || year.revision > i64::MAX as u64 || year.release_year.is_none()
            {
                return Err(Refusal::InvalidObservation);
            }
            year.release_year
        } else {
            None
        };
        let mut expected_reasons = vec!["title".to_owned()];
        match (parsed.year, year) {
            (Some(a), Some(b)) if i64::from(a) == b => expected_reasons.push("year".into()),
            (Some(_), Some(_)) => return Err(Refusal::InvalidObservation),
            _ => {}
        }
        if c.candidate.reason_codes != expected_reasons {
            return Err(Refusal::InvalidObservation);
        }
        previous = Some(c.work.id.as_str());
    }
    Ok(())
}
fn validate_workflow(value: &Workflow, budget: &mut Budget) -> Result<()> {
    if value.contract_version != CONTRACT_VERSION {
        return Err(Refusal::UnsupportedContract);
    }
    identifier(&value.proposal.id)?;
    if value.proposal.revision == 0
        || value.proposal.revision > i64::MAX as u64
        || value.reviewed_generation < 0
    {
        return Err(Refusal::InvalidObservation);
    }
    validate_evidence(&value.origin, budget)?;
    validate_evidence(&value.reviewed, budget)?;
    if value.origin.identity != value.reviewed.identity
        || value.origin.file.file_id != value.proposal.file_id
        || value.reviewed.file.file_id != value.proposal.file_id
        || value.origin.file.file_revision != value.proposal.file_revision
        || value.reviewed.file.file_revision != value.proposal.file_revision
        || value.origin.file.original_native != value.reviewed.file.original_native
        || value.proposal.candidates
            != value
                .reviewed
                .candidates
                .iter()
                .map(|c| c.candidate.clone())
                .collect::<Vec<_>>()
    {
        return Err(Refusal::InvalidObservation);
    }
    let consistent = match (&value.proposal.status, &value.proposal.decision) {
        (Status::Accepted, Some(Decision::Accept { candidate_id })) => value
            .proposal
            .candidates
            .iter()
            .any(|c| &c.id == candidate_id),
        (Status::Pending | Status::Review, None) => {
            value.proposal.status == matching::initial_status(&value.proposal.candidates)
        }
        (Status::Rejected, Some(Decision::Reject))
        | (Status::Deferred, Some(Decision::Defer))
        | (Status::Stale, None | Some(Decision::Defer)) => true,
        _ => false,
    };
    if !consistent {
        return Err(Refusal::InvalidObservation);
    }
    if let Some(pending) = &value.pending {
        identifier(&pending.operation_id)?;
        digest(&pending.envelope_digest)?;
        if pending.proposal_revision != value.proposal.revision
            || pending.evidence_digest != value.reviewed.digest
            || matches!(
                value.proposal.status,
                Status::Accepted | Status::Rejected | Status::Stale
            )
        {
            return Err(Refusal::InvalidObservation);
        }
        if let Decision::Accept { candidate_id } = &pending.decision {
            if !value
                .proposal
                .candidates
                .iter()
                .any(|c| &c.id == candidate_id)
            {
                return Err(Refusal::InvalidObservation);
            }
        }
    }
    Ok(())
}
fn admit_continuity(current: &Workflow, fence: &Fence, file: &FileObservation) -> Result<()> {
    if current.reviewed.identity != fence.identity
        || current.proposal.file_id != file.file_id
        || fence.generation < current.reviewed_generation
    {
        return Err(Refusal::StaleObservation);
    }
    if current.proposal.file_revision == file.file_revision
        && current.reviewed.file.original_native != file.original_native
    {
        return Err(Refusal::FileChanged);
    }
    Ok(())
}

#[derive(Default)]
struct Budget {
    work: usize,
}
impl Budget {
    fn work(&mut self, amount: usize) -> Result<()> {
        self.work = self.work.saturating_add(amount);
        if self.work > MAX_WORK {
            Err(Refusal::WorkBudget)
        } else {
            Ok(())
        }
    }
    fn charge(&mut self, value: &impl Serialize) -> Result<()> {
        // Count serialization first, without allocating an unbounded JSON tree.
        struct Count(usize);
        impl std::io::Write for Count {
            fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
                self.0 = self.0.saturating_add(bytes.len());
                if self.0 > MAX_BYTES {
                    return Err(std::io::Error::other("matching byte budget"));
                }
                Ok(bytes.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        let mut bytes = Count(0);
        serde_json::to_writer(&mut bytes, value).map_err(|_| Refusal::ByteBudget)?;
        let value = serde_json::to_value(value).map_err(|_| Refusal::InvalidInput)?;
        let mut stack = vec![(&value, 0)];
        let mut nodes = 0usize;
        while let Some((value, depth)) = stack.pop() {
            nodes += 1;
            if nodes > MAX_NODES {
                return Err(Refusal::NodeBudget);
            }
            if depth > MAX_DEPTH {
                return Err(Refusal::DepthBudget);
            }
            match value {
                serde_json::Value::Array(values) => {
                    stack.extend(values.iter().map(|v| (v, depth + 1)))
                }
                serde_json::Value::Object(values) => {
                    stack.extend(values.values().map(|v| (v, depth + 1)))
                }
                _ => {}
            }
        }
        self.work(nodes.saturating_add(bytes.0 / 64))
    }
}

#[cfg(test)]
mod tests;
