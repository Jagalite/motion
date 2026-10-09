use playscale_core::access::{
    AccessError, Claim, Credential, CredentialKind, DEVICE_CREDENTIAL_TTL_SECONDS, Device,
    EventCursor, Grant, IdempotencyRecord, Idempotent, Issue, Pairing, PairingPhase, Permission,
    Principal, Recheck, ResetReason, Resume, apply_issue, approve, authenticate, claim, derive,
    idempotency, recheck, replace_policy, resume, revoke,
};
use serde::{Deserialize, Serialize};
use stateless::{
    Check, Enumerate, Model, ModelCodec, ModelError, ModelMetadata, Transition, TransitionRef,
};
use std::collections::BTreeSet;

const TICK: i64 = 400;
const HORIZON: i64 = 1_200;
const PAIRING_EXPIRY: i64 = 600;
const MAX_ROWS: usize = 3;
const MAX_REVISION: u64 = 3;
const KEY_RETENTION: i64 = 800;
const SESSION_MAX: i64 = 12 * 3_600;
const EPOCH: &str = "e1";

/// A stored credential row. `None` in `State::rows` is a deleted row.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
struct Row {
    credential: Credential,
    parent: Option<usize>,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
struct Stream {
    row: usize,
    policy_revision: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
struct State {
    now: i64,
    /// The initial pairing and, after `Renew`, a renewal pairing for `d1`.
    pairings: Vec<Pairing>,
    device: Option<Device>,
    /// Credential rows are retained on revocation, so authentication itself
    /// must deny them.
    rows: Vec<Option<Row>>,
    /// The single modelled idempotency key and the row it acknowledged.
    key: Option<(IdempotencyRecord, usize)>,
    stream: Option<Stream>,
    /// Last cursor a subscriber received, for reconnect.
    cursor: Option<EventCursor>,
    /// The last applied issue, for delayed/duplicate re-application.
    last_issue: Option<Issue>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
enum Input {
    Approve {
        pairing: usize,
        admin: bool,
        code_ok: bool,
    },
    Claim {
        pairing: usize,
    },
    /// Start a renewal pairing for the existing device.
    Renew,
    /// A delayed or duplicated application of the last issue.
    ReapplyIssue,
    Derive {
        parent: usize,
        kind: CredentialKind,
        ttl: i64,
        keyed: bool,
    },
    /// The adapter removes one credential row (logout, cleanup).
    Delete {
        row: usize,
    },
    Revoke {
        stale: bool,
    },
    /// Toggle library access, or `events:read` when `events` is set.
    Policy {
        stale: bool,
        events: bool,
    },
    Subscribe {
        row: usize,
    },
    /// Reconnect with the last cursor as the device (`Some`) or operator.
    Reconnect {
        row: Option<usize>,
    },
    Tick,
    /// Jump to one second before (`before`) or exactly at a row's expiry.
    ExpireRow {
        row: usize,
        before: bool,
    },
    /// Jump to the device credential's expiry boundary.
    ExpireDevice,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
enum Effect {
    Approved,
    Issued { issue: Issue, row: usize },
    ClaimReplayed { issue: Issue },
    Replayed { row: usize },
    Derived { row: usize },
    Deleted,
    Revoked,
    PolicyChanged { revision: u64 },
    Unchanged,
    Subscribed { cursor: EventCursor },
    Resumed(Resume),
    Ticked,
    Rejected(AccessError),
}

fn grant() -> Grant {
    Grant {
        profile_ids: ["p1".to_string()].into(),
        permissions: [Permission::CatalogRead, Permission::EventsRead].into(),
    }
}

fn viewer() -> Principal {
    Principal {
        grant: Grant {
            profile_ids: BTreeSet::new(),
            permissions: [Permission::ProfilesManage].into(),
        },
        ..Principal::operator()
    }
}

fn row(s: &State, i: usize) -> Option<&Row> {
    s.rows.get(i).and_then(Option::as_ref)
}

/// Production authentication over the stored rows, as the adapter performs it.
fn auth(s: &State, i: usize) -> Result<Principal, AccessError> {
    let r = row(s, i).ok_or(AccessError::Unauthenticated)?;
    let parent = r.parent.and_then(|p| row(s, p)).map(|p| &p.credential);
    authenticate(&r.credential, parent, s.device.as_ref(), s.now)
}

/// Independent specification of when a stored credential must authenticate.
fn should_live(s: &State, i: usize) -> bool {
    let (Some(r), Some(d)) = (row(s, i), s.device.as_ref()) else {
        return false;
    };
    let c = &r.credential;
    let own = !d.revoked
        && c.device_id == d.id
        && c.generation == d.generation
        && s.now < c.expires_at
        && d.credential_expires_at.is_some_and(|e| s.now < e);
    let lineage = match (c.kind, r.parent.and_then(|p| row(s, p))) {
        (CredentialKind::Device, _) => r.parent.is_none(),
        (_, Some(p)) => {
            p.credential.kind == CredentialKind::Device
                && p.credential.generation == d.generation
                && c.expires_at <= p.credential.expires_at
        }
        (_, None) => false,
    };
    own && lineage
}

fn device_row(s: &State, generation: u64) -> Option<usize> {
    (0..s.rows.len()).find(|i| {
        row(s, *i).is_some_and(|r| {
            r.credential.kind == CredentialKind::Device && r.credential.generation == generation
        })
    })
}

fn digest(kind: CredentialKind, ttl: i64, parent: usize) -> String {
    format!("{kind:?}/{ttl}/{parent}")
}

struct Access;
impl Model for Access {
    type State = State;
    type Input = Input;
    type Output = Effect;
    fn metadata(&self) -> ModelMetadata {
        ModelMetadata {
            name: "playscale.access".into(),
            model_version: 3,
            properties_version: 3,
            codec_version: 3,
            build: include_str!("../src/access.rs").into(),
        }
    }
    fn initial_state(&self) -> Result<State, ModelError> {
        Ok(State {
            now: 0,
            pairings: vec![pending("pair")],
            device: None,
            rows: vec![],
            key: None,
            stream: None,
            cursor: None,
            last_issue: None,
        })
    }
    fn step(&self, s: &State, input: &Input) -> Result<Transition<State, Effect>, ModelError> {
        let mut next = s.clone();
        let effect = match input {
            Input::Approve {
                pairing,
                admin,
                code_ok,
            } => {
                let approver = if *admin {
                    Principal::operator()
                } else {
                    viewer()
                };
                let code = if *code_ok { "abcdefgh" } else { "ABCD-EFGX" };
                let known = ["p1".to_string()].into();
                match approve(
                    &s.pairings[*pairing],
                    s.now,
                    &approver,
                    code,
                    &grant(),
                    &known,
                ) {
                    Ok(()) => {
                        // A renewal binds the existing device; the first creates it.
                        if s.device.is_none() {
                            next.device = Some(Device::approved("d1".into(), grant()));
                        }
                        next.pairings[*pairing].phase = PairingPhase::Approved {
                            device_id: "d1".into(),
                        };
                        Effect::Approved
                    }
                    Err(e) => Effect::Rejected(e),
                }
            }
            Input::Claim { pairing } => {
                let p = &s.pairings[*pairing];
                let issued = match &p.phase {
                    PairingPhase::Claimed { generation, .. } => device_row(s, *generation)
                        .and_then(|i| row(s, i))
                        .map(|r| &r.credential),
                    _ => None,
                };
                match claim(p, s.device.as_ref(), issued, s.now) {
                    Ok(Claim::Issue(issue)) => {
                        match apply_issue(s.device.as_ref().unwrap(), &issue) {
                            Ok(device) => {
                                next.device = Some(device);
                                next.pairings[*pairing].phase = PairingPhase::Claimed {
                                    device_id: issue.device_id.clone(),
                                    generation: issue.generation,
                                };
                                next.rows.push(Some(Row {
                                    credential: Credential {
                                        device_id: issue.device_id.clone(),
                                        kind: CredentialKind::Device,
                                        generation: issue.generation,
                                        expires_at: issue.expires_at,
                                    },
                                    parent: None,
                                }));
                                next.last_issue = Some(issue.clone());
                                Effect::Issued {
                                    issue,
                                    row: s.rows.len(),
                                }
                            }
                            Err(e) => Effect::Rejected(e),
                        }
                    }
                    Ok(Claim::Replay(issue)) => Effect::ClaimReplayed { issue },
                    Err(e) => Effect::Rejected(e),
                }
            }
            Input::Renew => {
                next.pairings.push(pending("renew"));
                Effect::Unchanged
            }
            Input::ReapplyIssue => {
                let issue = s.last_issue.as_ref().unwrap();
                match apply_issue(s.device.as_ref().unwrap(), issue) {
                    Ok(_) => return Err(ModelError::new("a stale issue applied")),
                    Err(e) => Effect::Rejected(e),
                }
            }
            Input::Derive {
                parent,
                kind,
                ttl,
                keyed,
            } => {
                let digest = digest(*kind, *ttl, *parent);
                // Authenticate before consulting the idempotency record.
                let decision = auth(s, *parent).and_then(|_| {
                    if *keyed {
                        idempotency(s.key.as_ref().map(|k| &k.0), &digest, s.now)
                    } else {
                        Ok(Idempotent::Execute)
                    }
                });
                match decision {
                    Err(e) => Effect::Rejected(e),
                    Ok(Idempotent::Replay) => {
                        let acknowledged = s.key.as_ref().unwrap().1;
                        if auth(s, acknowledged).is_ok() {
                            Effect::Replayed { row: acknowledged }
                        } else {
                            Effect::Rejected(AccessError::ReplayUnavailable)
                        }
                    }
                    Ok(Idempotent::Execute) => {
                        let device = s.device.as_ref().unwrap();
                        let parent_row = &row(s, *parent).unwrap().credential;
                        match derive(parent_row, device, *kind, *ttl, s.now) {
                            Err(e) => Effect::Rejected(e),
                            Ok(credential) => {
                                let index = next.rows.len();
                                next.rows.push(Some(Row {
                                    credential,
                                    parent: Some(*parent),
                                }));
                                if *keyed {
                                    next.key = Some((
                                        IdempotencyRecord {
                                            digest,
                                            expires_at: s.now + KEY_RETENTION,
                                        },
                                        index,
                                    ));
                                }
                                Effect::Derived { row: index }
                            }
                        }
                    }
                }
            }
            Input::Delete { row } => {
                next.rows[*row] = None;
                Effect::Deleted
            }
            Input::Revoke { stale } => {
                let device = s.device.as_ref().unwrap();
                match revoke(device, device.revision - u64::from(*stale)) {
                    Ok(Some(d)) => {
                        next.device = Some(d);
                        Effect::Revoked
                    }
                    Ok(None) => Effect::Unchanged,
                    Err(e) => Effect::Rejected(e),
                }
            }
            Input::Policy { stale, events } => {
                let device = s.device.as_ref().unwrap();
                let mut permissions = device.grant.permissions.clone();
                let mut policy = device.policy.clone();
                if *events {
                    if !permissions.remove(&Permission::EventsRead) {
                        permissions.insert(Permission::EventsRead);
                    }
                } else if !policy.library_ids.remove("l1") {
                    policy.library_ids.insert("l1".into());
                }
                match replace_policy(
                    device,
                    device.revision - u64::from(*stale),
                    permissions,
                    policy,
                ) {
                    Ok(Some(d)) => {
                        let revision = d.revision;
                        next.device = Some(d);
                        Effect::PolicyChanged { revision }
                    }
                    Ok(None) => Effect::Unchanged,
                    Err(e) => Effect::Rejected(e),
                }
            }
            Input::Subscribe { row } => match auth(s, *row) {
                Ok(p) if p.allows(Permission::EventsRead) => {
                    let Resume::Reset { position, .. } = resume(None, EPOCH, &p, 0, 0) else {
                        return Err(ModelError::new("a missing cursor must reset"));
                    };
                    let cursor = EventCursor {
                        epoch: EPOCH.into(),
                        principal: p.id.clone(),
                        policy_revision: p.policy_revision,
                        position,
                    };
                    next.stream = Some(Stream {
                        row: *row,
                        policy_revision: p.policy_revision,
                    });
                    next.cursor = Some(cursor.clone());
                    Effect::Subscribed { cursor }
                }
                Ok(_) => Effect::Rejected(AccessError::Forbidden(Permission::EventsRead)),
                Err(e) => Effect::Rejected(e),
            },
            Input::Reconnect { row } => {
                let principal = match row {
                    Some(i) => auth(s, *i),
                    None => Ok(Principal::operator()),
                };
                match principal {
                    Ok(p) => Effect::Resumed(resume(s.cursor.as_ref(), EPOCH, &p, 0, 0)),
                    Err(e) => Effect::Rejected(e),
                }
            }
            Input::Tick => {
                next.now += TICK;
                Effect::Ticked
            }
            Input::ExpireRow { row: i, before } => {
                next.now = row(s, *i).unwrap().credential.expires_at - i64::from(*before);
                Effect::Ticked
            }
            Input::ExpireDevice => {
                next.now = s.device.as_ref().unwrap().credential_expires_at.unwrap();
                Effect::Ticked
            }
        };
        // A live stream re-applies the production recheck after every event.
        if let Some(stream) = &next.stream {
            match recheck(stream.policy_revision, &auth(&next, stream.row)) {
                Recheck::Continue => {}
                Recheck::Reset { policy_revision } => {
                    next.stream.as_mut().unwrap().policy_revision = policy_revision;
                    if let Some(c) = &mut next.cursor {
                        c.policy_revision = policy_revision;
                    }
                }
                Recheck::Close(_) => next.stream = None,
            }
        }
        Ok(Transition::accepted(next, vec![effect]))
    }
    fn check_state(&self, s: &State) -> Result<Vec<Check>, ModelError> {
        let rows = 0..s.rows.len();
        let matches_spec = rows
            .clone()
            .all(|i| auth(s, i).is_ok() == should_live(s, i));
        let revoked_denies = !s.device.as_ref().is_some_and(|d| d.revoked)
            || rows.clone().all(|i| auth(s, i).is_err());
        let superseded_denied = rows.clone().all(|i| {
            row(s, i).is_none_or(|r| {
                Some(r.credential.generation) == s.device.as_ref().map(|d| d.generation)
                    || auth(s, i) == Err(AccessError::CredentialSuperseded)
                    || s.device.as_ref().is_some_and(|d| d.revoked)
            })
        });
        let stream_scoped = s.stream.as_ref().is_none_or(|st| {
            auth(s, st.row).is_ok_and(|p| {
                p.allows(Permission::EventsRead) && p.policy_revision == st.policy_revision
            })
        });
        Ok(vec![
            check("access.authentication_matches_spec", matches_spec),
            check("access.revoked_denies_retained_rows", revoked_denies),
            check("access.superseded_generation_denied", superseded_denied),
            check("access.stream_follows_policy_and_revocation", stream_scoped),
        ])
    }
    fn check_transition(
        &self,
        before: &State,
        input: &Input,
        next: &TransitionRef<'_, State, Effect>,
    ) -> Result<Vec<Check>, ModelError> {
        let after = next.state;
        let [effect] = next.outputs else {
            return Ok(vec![check("access.exact_effect", false)]);
        };
        let exact = match (expected(before, input), effect) {
            (Expect::Reject(e), Effect::Rejected(actual)) => {
                e == *actual && unchanged(before, after)
            }
            (Expect::Approved, Effect::Approved) => match &before.device {
                // A renewal binds the existing device without changing it.
                Some(d) => after.device.as_ref() == Some(d),
                None => after
                    .device
                    .as_ref()
                    .is_some_and(|d| d.revision == 1 && d.generation == 0 && !d.revoked),
            },
            (Expect::Issued(generation), Effect::Issued { issue, row: r }) => {
                *r == before.rows.len()
                    && issue.generation == generation
                    && issue.expires_at == before.now + DEVICE_CREDENTIAL_TTL_SECONDS
                    && auth(after, *r).is_ok()
                    && (0..before.rows.len()).all(|i| auth(after, i).is_err())
            }
            (Expect::ClaimReplayed(original), Effect::ClaimReplayed { issue }) => {
                original == *issue && unchanged(before, after)
            }
            (Expect::Replayed(r0), Effect::Replayed { row: r }) => {
                r0 == *r && unchanged(before, after)
            }
            (Expect::Derived(credential), Effect::Derived { row: r }) => {
                *r == before.rows.len()
                    && row(after, *r).is_some_and(|x| x.credential == credential)
                    && auth(after, *r).is_ok()
            }
            (Expect::Deleted, Effect::Deleted) => {
                matches!(input, Input::Delete { row: i } if row(after, *i).is_none())
            }
            (Expect::Revoked, Effect::Revoked) => after
                .device
                .as_ref()
                .zip(before.device.as_ref())
                .is_some_and(|(a, b)| a.revoked && a.revision == b.revision + 1),
            (Expect::PolicyChanged, Effect::PolicyChanged { revision }) => {
                before.device.as_ref().map(|d| d.revision + 1) == Some(*revision)
                    && after.device.as_ref().map(|d| d.revision) == Some(*revision)
            }
            (Expect::Unchanged, Effect::Unchanged) => {
                before.device == after.device && before.rows == after.rows
            }
            (Expect::Subscribed(p), Effect::Subscribed { cursor }) => {
                cursor.principal == p.id && cursor.policy_revision == p.policy_revision
            }
            (Expect::Resumed(continues), Effect::Resumed(r)) => {
                continues == matches!(r, Resume::Continue(_))
            }
            (Expect::Ticked, Effect::Ticked) => after.now > before.now,
            _ => false,
        };
        Ok(vec![check("access.exact_effect", exact)])
    }
}

fn pending(id: &str) -> Pairing {
    Pairing {
        id: id.into(),
        user_code: "ABCD-EFGH".into(),
        expires_at: PAIRING_EXPIRY,
        phase: PairingPhase::Pending,
    }
}

fn unchanged(before: &State, after: &State) -> bool {
    before.pairings == after.pairings
        && before.device == after.device
        && before.rows == after.rows
        && before.key == after.key
}

enum Expect {
    Reject(AccessError),
    Approved,
    Issued(u64),
    ClaimReplayed(Issue),
    Replayed(usize),
    Derived(Credential),
    Deleted,
    Revoked,
    PolicyChanged,
    Unchanged,
    Subscribed(Principal),
    Resumed(bool),
    Ticked,
}

/// The intended outcome of each input, stated from the specification rather
/// than by calling the decision under test.
fn expected(s: &State, input: &Input) -> Expect {
    let device = s.device.as_ref();
    let live = |i: usize| should_live(s, i);
    // Rejection reasons for a credential that should not authenticate.
    let denial = |i: usize| -> AccessError {
        match (row(s, i), device) {
            (None, _) => AccessError::Unauthenticated,
            (Some(_), None) => AccessError::CredentialRevoked,
            (Some(_), Some(d)) if d.revoked => AccessError::CredentialRevoked,
            (Some(r), Some(d)) if r.credential.generation != d.generation => {
                AccessError::CredentialSuperseded
            }
            (Some(r), Some(_))
                if r.credential.kind != CredentialKind::Device
                    && r.parent.and_then(|p| row(s, p)).is_none() =>
            {
                AccessError::CredentialRevoked
            }
            _ => AccessError::CredentialExpired,
        }
    };
    match input {
        Input::Approve {
            pairing,
            admin,
            code_ok,
        } => {
            let p = &s.pairings[*pairing];
            if !admin {
                Expect::Reject(AccessError::Forbidden(Permission::SystemAdmin))
            } else if s.now >= p.expires_at {
                Expect::Reject(AccessError::PairingExpired)
            } else if p.phase != PairingPhase::Pending {
                Expect::Reject(AccessError::PairingDecided)
            } else if !code_ok {
                Expect::Reject(AccessError::UserCodeMismatch)
            } else {
                Expect::Approved
            }
        }
        Input::Claim { pairing } => {
            let p = &s.pairings[*pairing];
            match (&p.phase, device) {
                _ if s.now >= p.expires_at => Expect::Reject(AccessError::PairingExpired),
                (PairingPhase::Pending, _) => Expect::Reject(AccessError::PairingPending),
                (_, Some(d)) if d.revoked => Expect::Reject(AccessError::DeviceRevoked),
                (PairingPhase::Approved { .. }, Some(d)) => Expect::Issued(d.generation + 1),
                (PairingPhase::Claimed { generation, .. }, Some(d)) => {
                    match device_row(s, *generation).and_then(|i| row(s, i)) {
                        Some(r) if d.generation == *generation => Expect::ClaimReplayed(Issue {
                            device_id: d.id.clone(),
                            generation: *generation,
                            expires_at: r.credential.expires_at,
                        }),
                        _ => Expect::Reject(AccessError::ReplayUnavailable),
                    }
                }
                (_, None) => Expect::Reject(AccessError::DeviceRevoked),
            }
        }
        Input::Renew => Expect::Unchanged,
        Input::ReapplyIssue => Expect::Reject(AccessError::StaleRevision),
        Input::Derive {
            parent,
            kind,
            ttl,
            keyed,
        } => {
            if !live(*parent) {
                return Expect::Reject(denial(*parent));
            }
            let key = s
                .key
                .as_ref()
                .filter(|(k, _)| *keyed && s.now < k.expires_at);
            if let Some((k, acknowledged)) = key {
                return if k.digest != digest(*kind, *ttl, *parent) {
                    Expect::Reject(AccessError::IdempotencyMismatch)
                } else if live(*acknowledged) {
                    Expect::Replayed(*acknowledged)
                } else {
                    Expect::Reject(AccessError::ReplayUnavailable)
                };
            }
            let p = &row(s, *parent).unwrap().credential;
            let ttl_ok = match kind {
                CredentialKind::Access => (60..=3_600).contains(ttl),
                CredentialKind::Session => (60..=SESSION_MAX).contains(ttl),
                CredentialKind::Device => false,
            };
            if p.kind != CredentialKind::Device || *kind == CredentialKind::Device {
                Expect::Reject(AccessError::ParentCredentialRequired)
            } else if !ttl_ok {
                Expect::Reject(AccessError::InvalidTtl)
            } else {
                Expect::Derived(Credential {
                    device_id: p.device_id.clone(),
                    kind: *kind,
                    generation: p.generation,
                    expires_at: (s.now + ttl).min(p.expires_at),
                })
            }
        }
        Input::Delete { .. } => Expect::Deleted,
        Input::Revoke { stale: true } | Input::Policy { stale: true, .. } => {
            Expect::Reject(AccessError::StaleRevision)
        }
        Input::Revoke { stale: false } => match device {
            Some(d) if d.revoked => Expect::Unchanged,
            _ => Expect::Revoked,
        },
        Input::Policy { stale: false, .. } => match device {
            Some(d) if d.revoked => Expect::Reject(AccessError::DeviceRevoked),
            _ => Expect::PolicyChanged,
        },
        Input::Subscribe { row: i } => {
            if !live(*i) {
                return Expect::Reject(denial(*i));
            }
            let p = auth(s, *i).unwrap();
            if p.allows(Permission::EventsRead) {
                Expect::Subscribed(p)
            } else {
                Expect::Reject(AccessError::Forbidden(Permission::EventsRead))
            }
        }
        Input::Reconnect { row } => {
            let (id, revision) = match row {
                Some(i) if !live(*i) => return Expect::Reject(denial(*i)),
                Some(_) => ("d1".to_string(), device.unwrap().revision),
                None => ("operator".to_string(), 0),
            };
            Expect::Resumed(
                s.cursor
                    .as_ref()
                    .is_some_and(|c| c.principal == id && c.policy_revision == revision),
            )
        }
        Input::Tick | Input::ExpireRow { .. } | Input::ExpireDevice => Expect::Ticked,
    }
}

fn check(id: &'static str, ok: bool) -> Check {
    if ok {
        Check::passed(id)
    } else {
        Check::failed(id, "access invariant violated")
    }
}

impl Enumerate for Access {
    fn inputs(&self, s: &State) -> Result<Vec<Input>, ModelError> {
        let mut inputs = vec![];
        let room = s.rows.len() < MAX_ROWS;
        for pairing in 0..s.pairings.len() {
            for admin in [false, true] {
                for code_ok in [false, true] {
                    inputs.push(Input::Approve {
                        pairing,
                        admin,
                        code_ok,
                    });
                }
            }
            if room {
                inputs.push(Input::Claim { pairing });
            }
        }
        if s.pairings.len() == 1 && s.device.is_some() {
            inputs.push(Input::Renew);
        }
        if s.last_issue.is_some() {
            inputs.push(Input::ReapplyIssue);
        }
        for i in 0..s.rows.len() {
            let Some(r) = &s.rows[i] else { continue };
            inputs.push(Input::Delete { row: i });
            inputs.push(Input::Subscribe { row: i });
            inputs.push(Input::Reconnect { row: Some(i) });
            for before in [true, false] {
                if r.credential.expires_at - i64::from(before) > s.now
                    && r.credential.kind != CredentialKind::Device
                {
                    inputs.push(Input::ExpireRow { row: i, before });
                }
            }
            if room {
                for (kind, ttl, keyed) in [
                    (CredentialKind::Access, 59, false),
                    (CredentialKind::Access, 3_600, false),
                    (CredentialKind::Access, 3_600, true),
                    (CredentialKind::Session, SESSION_MAX + 1, false),
                    (CredentialKind::Session, 600, true),
                ] {
                    inputs.push(Input::Derive {
                        parent: i,
                        kind,
                        ttl,
                        keyed,
                    });
                }
            }
        }
        if s.cursor.is_some() {
            inputs.push(Input::Reconnect { row: None });
        }
        if s.device.as_ref().is_some_and(|d| d.revision < MAX_REVISION) {
            for stale in [false, true] {
                inputs.push(Input::Revoke { stale });
                for events in [false, true] {
                    inputs.push(Input::Policy { stale, events });
                }
            }
        }
        if s.now < HORIZON {
            inputs.push(Input::Tick);
        }
        if s.device
            .as_ref()
            .and_then(|d| d.credential_expires_at)
            .is_some_and(|e| s.now < e)
        {
            inputs.push(Input::ExpireDevice);
        }
        Ok(inputs)
    }
}

impl stateless::Generate for Access {
    fn generate(&self, s: &State, rng: &mut stateless::Rng) -> Result<Option<Input>, ModelError> {
        let inputs = self.inputs(s)?;
        Ok(rng.index(inputs.len()).map(|i| inputs[i].clone()))
    }
}

impl ModelCodec for Access {
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
fn decode<T: serde::de::DeserializeOwned>(v: &[u8]) -> Result<T, ModelError> {
    serde_json::from_slice(v).map_err(|e| ModelError::new(e.to_string()))
}

#[test]
fn bounded_access_graph_checks_production_decisions() {
    let config = stateless::explore::SearchConfig {
        max_states: 4_000_000,
        max_transitions: 100_000_000,
        max_depth: 100,
    };
    let report = stateless::explore::enumerate(&Access, config).unwrap();
    assert!(report.failure.is_none(), "{:?}", report.failure);
    assert_eq!(report.skipped_checks, 0);
    assert_eq!(
        report.termination,
        stateless::explore::SearchTermination::GraphExhausted
    );
    assert!(report.transitions > 1000, "{}", report.transitions);
    println!(
        "Stateless: {} states, {} edges; initial + renewal pairing, <={MAX_ROWS} credential rows (device/access/session, deletions, retained on revoke), device revision <={MAX_REVISION}, one idempotency key, one stream + reconnects, time 0..={HORIZON}s plus child e-1/e and the {DEVICE_CREDENTIAL_TTL_SECONDS}s device-expiry boundaries",
        report.states, report.transitions
    );
}

fn run(inputs: &[Input]) -> (State, Vec<Effect>) {
    let mut s = Access.initial_state().unwrap();
    let mut effects = vec![];
    for input in inputs {
        let t = Access.step(&s, input).unwrap();
        effects.extend(t.outputs);
        s = t.state;
    }
    (s, effects)
}

fn paired() -> Vec<Input> {
    vec![
        Input::Approve {
            pairing: 0,
            admin: true,
            code_ok: true,
        },
        Input::Claim { pairing: 0 },
    ]
}

#[test]
fn trace_policy_change_then_revocation_replays_exactly() {
    let mut inputs = paired();
    inputs.extend([
        Input::Derive {
            parent: 0,
            kind: CredentialKind::Session,
            ttl: 600,
            keyed: false,
        },
        Input::Subscribe { row: 1 },
        Input::Policy {
            stale: false,
            events: false,
        },
        Input::Revoke { stale: false },
    ]);
    let trace = stateless::execution::record(&Access, inputs, Default::default(), 10).unwrap();
    let replay = stateless::execution::replay(&Access, &trace, Default::default()).unwrap();
    assert_eq!(replay.outcome, stateless::execution::ReplayOutcome::Exact);
}

#[test]
fn renewal_supersedes_and_old_pairing_cannot_replay_newer_credential() {
    let mut inputs = paired();
    inputs.extend([
        Input::Renew,
        Input::Approve {
            pairing: 1,
            admin: true,
            code_ok: true,
        },
        Input::Claim { pairing: 1 },
        Input::Claim { pairing: 0 },
        Input::Claim { pairing: 1 },
        Input::ReapplyIssue,
    ]);
    let (s, effects) = run(&inputs);
    assert_eq!(auth(&s, 0), Err(AccessError::CredentialSuperseded));
    assert!(auth(&s, 1).is_ok());
    assert_eq!(effects[5], Effect::Rejected(AccessError::ReplayUnavailable));
    assert!(matches!(&effects[6], Effect::ClaimReplayed { issue } if issue.generation == 2));
    assert_eq!(effects[7], Effect::Rejected(AccessError::StaleRevision));
}

#[test]
fn deleting_parent_revokes_children_and_replay_cannot_resurrect() {
    let derive = Input::Derive {
        parent: 0,
        kind: CredentialKind::Access,
        ttl: 3_600,
        keyed: true,
    };
    let mut inputs = paired();
    inputs.extend([derive.clone(), Input::Delete { row: 0 }]);
    let (s, _) = run(&inputs);
    assert_eq!(auth(&s, 1), Err(AccessError::CredentialRevoked));

    let mut inputs = paired();
    inputs.extend([derive.clone(), Input::Delete { row: 1 }, derive]);
    let (s, effects) = run(&inputs);
    assert_eq!(
        effects.last(),
        Some(&Effect::Rejected(AccessError::ReplayUnavailable))
    );
    assert_eq!(s.rows.len(), 2);
}

#[test]
fn keyed_retry_replays_and_other_body_conflicts() {
    let derive = Input::Derive {
        parent: 0,
        kind: CredentialKind::Access,
        ttl: 3_600,
        keyed: true,
    };
    let mut inputs = paired();
    inputs.extend([
        derive.clone(),
        derive,
        Input::Derive {
            parent: 0,
            kind: CredentialKind::Access,
            ttl: 120,
            keyed: true,
        },
    ]);
    let (s, effects) = run(&inputs);
    assert_eq!(
        effects[2..],
        [
            Effect::Derived { row: 1 },
            Effect::Replayed { row: 1 },
            Effect::Rejected(AccessError::IdempotencyMismatch)
        ]
    );
    assert_eq!(s.rows.len(), 2);
}

#[test]
fn child_expires_exactly_at_its_deadline() {
    let mut inputs = paired();
    inputs.extend([
        Input::Derive {
            parent: 0,
            kind: CredentialKind::Session,
            ttl: 600,
            keyed: false,
        },
        Input::ExpireRow {
            row: 1,
            before: true,
        },
    ]);
    let (s, _) = run(&inputs);
    assert!(auth(&s, 1).is_ok());
    let (s, _) = run(&[
        inputs,
        vec![Input::ExpireRow {
            row: 1,
            before: false,
        }],
    ]
    .concat());
    assert_eq!(auth(&s, 1), Err(AccessError::CredentialExpired));
}

#[test]
fn lost_events_permission_closes_stream_and_cursor_is_principal_scoped() {
    let mut inputs = paired();
    inputs.extend([
        Input::Subscribe { row: 0 },
        Input::Reconnect { row: None },
        Input::Reconnect { row: Some(0) },
        Input::Policy {
            stale: false,
            events: true,
        },
        Input::Reconnect { row: Some(0) },
    ]);
    let (s, effects) = run(&inputs);
    assert!(matches!(
        effects[3],
        Effect::Resumed(Resume::Reset {
            reason: ResetReason::PrincipalChanged,
            ..
        })
    ));
    assert_eq!(effects[4], Effect::Resumed(Resume::Continue(0)));
    assert!(s.stream.is_none());
    assert!(matches!(
        effects[6],
        Effect::Resumed(Resume::Reset {
            reason: ResetReason::PolicyChanged,
            ..
        })
    ));
}
