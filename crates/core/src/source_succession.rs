//! Effect-free permanent succession/ACK protocol decisions.
//! All inputs are descriptive facts. Neither a struct nor a successful decision
//! proves native receipt provenance, requester authorization or descriptor/hash
//! custody. The eventual owners must establish those independently under writer.

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Domain {
    pub catalog: String,
    pub incarnation: String,
    pub profile: String,
    pub source: String,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Command {
    pub host: String,
    pub operation: String,
    pub payload_digest: String,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Head {
    pub revision: u64,
    pub record_digest: String,
    pub mapping_digest: String,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Disposition {
    VerifiedWholeOriginals,
    UnavailableOrExcluded,
    EquivalenceRenewal,
    UnsupportedGraph,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Provenance {
    pub domain: Domain,
    pub command: Command,
    pub expected_generation: u64,
    pub committed_generation: u64,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Effect {
    pub provenance: Provenance,
    pub predecessor: Head,
    pub successor: Head,
    /// Exact retained typed effect/result commitment, not a byte-custody grant.
    pub result_digest: String,
    pub closure_digest: String,
    pub disposition: Disposition,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MotionHead {
    pub native: Head,
    pub source_revision: u64,
    pub binding_revision: u64,
    pub lifecycle_epoch: u64,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Intent {
    pub domain: Domain,
    pub command: Command,
    pub expected_generation: u64,
    pub expected: MotionHead,
    pub new_mapping_digest: String,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Ack {
    pub intent: Intent,
    pub effect: Effect,
    pub resulting: MotionHead,
}
/// `Compacted` still requires the exact permanent command provenance and typed
/// effect below. Absence is not an invitation to execute or replace an intent.
#[derive(Clone, Copy)]
pub enum Receipt<'a> {
    Absent,
    Full(&'a Effect),
    Compacted,
}
pub struct Evidence<'a> {
    pub receipt: Receipt<'a>,
    pub permanent_provenance: Option<&'a Provenance>,
    pub permanent_effect: Option<&'a Effect>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Refusal {
    MissingEvidence,
    Conflict,
    BrokenChain,
    UnsupportedDisposition,
    Exhausted,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NativeDecision {
    Append(Head),
    HistoricalReplay,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AckDecision {
    Append(Ack),
    /// No head update, including when this is an older exact ACK.
    HistoricalReplay,
}
fn next(value: u64) -> Result<u64, Refusal> {
    value.checked_add(1).ok_or(Refusal::Exhausted)
}
fn same_key(a: &Command, b: &Command) -> bool {
    a.host == b.host && a.operation == b.operation
}
fn valid_effect(effect: &Effect) -> Result<(), Refusal> {
    if effect.disposition != Disposition::VerifiedWholeOriginals {
        return Err(Refusal::UnsupportedDisposition);
    }
    if effect.successor.revision != next(effect.predecessor.revision)?
        || effect.provenance.committed_generation < effect.provenance.expected_generation
        || effect.predecessor.record_digest == effect.successor.record_digest
    {
        return Err(Refusal::BrokenChain);
    }
    Ok(())
}
fn matches_intent(intent: &Intent, effect: &Effect) -> Result<(), Refusal> {
    valid_effect(effect)?;
    if effect.provenance.domain != intent.domain
        || effect.provenance.command != intent.command
        || effect.provenance.expected_generation != intent.expected_generation
        || effect.predecessor != intent.expected.native
        || effect.successor.mapping_digest != intent.new_mapping_digest
    {
        return Err(Refusal::Conflict);
    }
    Ok(())
}
/// This intentionally does not interpret a generic expired receipt as replay.
/// Only the typed permanent evidence comparison establishes a recoverable effect.
pub fn recover_effect<'a>(intent: &Intent, facts: &'a Evidence<'a>) -> Result<&'a Effect, Refusal> {
    if matches!(facts.receipt, Receipt::Absent) {
        return Err(Refusal::MissingEvidence);
    }
    let effect = facts.permanent_effect.ok_or(Refusal::MissingEvidence)?;
    let provenance = facts.permanent_provenance.ok_or(Refusal::MissingEvidence)?;
    if provenance != &effect.provenance {
        return Err(Refusal::Conflict);
    }
    if let Receipt::Full(full) = facts.receipt {
        if full != effect {
            return Err(Refusal::Conflict);
        }
    }
    matches_intent(intent, effect)?;
    Ok(effect)
}
/// Check the complete supplied native chain, not only its greatest revision.
/// A validated input chain is still not a database or filesystem authority.
pub fn append_native(
    domain: &Domain,
    baseline: &Head,
    history: &[Effect],
    proposed: &Effect,
) -> Result<NativeDecision, Refusal> {
    let mut current = baseline;
    let mut previous_committed = None;
    for (index, effect) in history.iter().enumerate() {
        valid_effect(effect)?;
        if &effect.provenance.domain != domain
            || &effect.predecessor != current
            || previous_committed
                .is_some_and(|generation| effect.provenance.expected_generation < generation)
            || history[..index]
                .iter()
                .any(|old| same_key(&old.provenance.command, &effect.provenance.command))
        {
            return Err(Refusal::BrokenChain);
        }
        current = &effect.successor;
        previous_committed = Some(effect.provenance.committed_generation);
    }
    valid_effect(proposed)?;
    if &proposed.provenance.domain != domain {
        return Err(Refusal::Conflict);
    }
    if let Some(old) = history
        .iter()
        .find(|old| same_key(&old.provenance.command, &proposed.provenance.command))
    {
        return if old == proposed {
            Ok(NativeDecision::HistoricalReplay)
        } else {
            Err(Refusal::Conflict)
        };
    }
    if &proposed.predecessor != current
        || previous_committed
            .is_some_and(|generation| proposed.provenance.expected_generation < generation)
    {
        return Err(Refusal::BrokenChain);
    }
    Ok(NativeDecision::Append(proposed.successor.clone()))
}
fn resulting(intent: &Intent, effect: &Effect) -> Result<MotionHead, Refusal> {
    matches_intent(intent, effect)?;
    Ok(MotionHead {
        native: effect.successor.clone(),
        source_revision: next(intent.expected.source_revision)?,
        binding_revision: next(intent.expected.binding_revision)?,
        lifecycle_epoch: next(intent.expected.lifecycle_epoch)?,
    })
}
pub fn acknowledge(
    domain: &Domain,
    baseline: &MotionHead,
    history: &[Ack],
    native_current: &Head,
    intent: &Intent,
    evidence: &Evidence<'_>,
) -> Result<AckDecision, Refusal> {
    let mut current = baseline;
    let mut previous_committed = None;
    for (index, ack) in history.iter().enumerate() {
        if &ack.intent.domain != domain
            || &ack.intent.expected != current
            || previous_committed
                .is_some_and(|generation| ack.effect.provenance.expected_generation < generation)
            || resulting(&ack.intent, &ack.effect)? != ack.resulting
            || history[..index]
                .iter()
                .any(|old| same_key(&old.intent.command, &ack.intent.command))
        {
            return Err(Refusal::BrokenChain);
        }
        current = &ack.resulting;
        previous_committed = Some(ack.effect.provenance.committed_generation);
    }
    let effect = recover_effect(intent, evidence)?;
    if &intent.domain != domain {
        return Err(Refusal::Conflict);
    }
    if let Some(old) = history
        .iter()
        .find(|old| same_key(&old.intent.command, &intent.command))
    {
        return if &old.intent == intent && &old.effect == effect {
            Ok(AckDecision::HistoricalReplay)
        } else {
            Err(Refusal::Conflict)
        };
    }
    // A delayed unacknowledged effect cannot overwrite a newer native/Motion
    // head. Recovery must resolve the exact predecessor sequence explicitly.
    if &intent.expected != current
        || native_current != &effect.successor
        || previous_committed
            .is_some_and(|generation| effect.provenance.expected_generation < generation)
    {
        return Err(Refusal::BrokenChain);
    }
    Ok(AckDecision::Append(Ack {
        intent: intent.clone(),
        effect: effect.clone(),
        resulting: resulting(intent, effect)?,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn domain() -> Domain {
        Domain {
            catalog: "catalog".into(),
            incarnation: "incarnation".into(),
            profile: "profile".into(),
            source: "source".into(),
        }
    }
    fn baseline() -> MotionHead {
        MotionHead {
            native: Head {
                revision: 0,
                record_digest: "genesis".into(),
                mapping_digest: "mapping0".into(),
            },
            source_revision: 7,
            binding_revision: 3,
            lifecycle_epoch: 11,
        }
    }
    fn chain() -> (Vec<Effect>, Vec<Ack>) {
        let mut head = baseline();
        let mut effects = Vec::new();
        let mut acks = Vec::new();
        for n in 1..=4 {
            let command = Command {
                host: "motion".into(),
                operation: format!("move{n}"),
                payload_digest: format!("payload{n}"),
            };
            let intent = Intent {
                domain: domain(),
                command: command.clone(),
                expected_generation: n * 10,
                expected: head.clone(),
                new_mapping_digest: format!("mapping{n}"),
            };
            let effect = Effect {
                provenance: Provenance {
                    domain: domain(),
                    command,
                    expected_generation: n * 10,
                    committed_generation: n * 10 + 1,
                },
                predecessor: head.native.clone(),
                successor: Head {
                    revision: n,
                    record_digest: format!("record{n}"),
                    mapping_digest: intent.new_mapping_digest.clone(),
                },
                result_digest: format!("result{n}"),
                closure_digest: format!("closure{n}"),
                disposition: Disposition::VerifiedWholeOriginals,
            };
            head = resulting(&intent, &effect).unwrap();
            acks.push(Ack {
                intent,
                effect: effect.clone(),
                resulting: head.clone(),
            });
            effects.push(effect);
        }
        (effects, acks)
    }
    #[test]
    fn exhaustive_small_prefix_replay_compaction_and_ack_matrix() {
        let (effects, acks) = chain();
        let initial = baseline();
        let mut examined = 0;
        // Exhaust every native prefix, ACK prefix no greater than it, target
        // operation, receipt state and independent permanent-evidence presence.
        // This finite model claims this entire domain, not an unbounded proof.
        for native_len in 0..=4 {
            let native_head = if native_len == 0 {
                &initial.native
            } else {
                &effects[native_len - 1].successor
            };
            for ack_len in 0..=native_len {
                for target in 0..4 {
                    for state in 0..3 {
                        for typed in [false, true] {
                            for provenance in [false, true] {
                                let effect = &effects[target];
                                let facts = Evidence {
                                    receipt: match state {
                                        0 => Receipt::Absent,
                                        1 => Receipt::Full(effect),
                                        _ => Receipt::Compacted,
                                    },
                                    permanent_provenance: provenance.then_some(&effect.provenance),
                                    permanent_effect: typed.then_some(effect),
                                };
                                let result = acknowledge(
                                    &domain(),
                                    &initial,
                                    &acks[..ack_len],
                                    native_head,
                                    &acks[target].intent,
                                    &facts,
                                );
                                let recoverable = state != 0 && typed && provenance;
                                if !recoverable {
                                    assert_eq!(result, Err(Refusal::MissingEvidence));
                                } else if target < ack_len {
                                    assert_eq!(result, Ok(AckDecision::HistoricalReplay));
                                } else if target == ack_len && native_len == target + 1 {
                                    assert_eq!(
                                        result,
                                        Ok(AckDecision::Append(acks[target].clone()))
                                    );
                                } else {
                                    assert_eq!(result, Err(Refusal::BrokenChain));
                                }
                                examined += 1;
                            }
                        }
                    }
                }
            }
        }
        assert_eq!(examined, 720);
    }
    #[test]
    fn every_native_prefix_replays_exactly_and_rejects_forks_or_gaps() {
        let (effects, _) = chain();
        for len in 0..=4 {
            for (index, effect) in effects.iter().enumerate() {
                let result = append_native(&domain(), &baseline().native, &effects[..len], effect);
                if index < len {
                    assert_eq!(result, Ok(NativeDecision::HistoricalReplay));
                } else if index == len {
                    assert_eq!(result, Ok(NativeDecision::Append(effect.successor.clone())));
                } else {
                    assert_eq!(result, Err(Refusal::BrokenChain));
                }
                if index < len {
                    for field in 0..3 {
                        let mut changed = effect.clone();
                        match field {
                            0 => changed.provenance.command.payload_digest.push('x'),
                            1 => changed.result_digest.push('x'),
                            _ => changed.closure_digest.push('x'),
                        }
                        assert_eq!(
                            append_native(&domain(), &baseline().native, &effects[..len], &changed),
                            Err(Refusal::Conflict)
                        );
                    }
                }
            }
        }
        for omitted in 0..3 {
            let mut broken = effects.clone();
            broken.remove(omitted);
            assert!(append_native(&domain(), &baseline().native, &broken, &effects[3]).is_err());
        }
    }
    #[test]
    fn stale_ack_never_moves_mapping_backward_and_evidence_cannot_disagree() {
        let (effects, acks) = chain();
        let full = Evidence {
            receipt: Receipt::Full(&effects[0]),
            permanent_provenance: Some(&effects[0].provenance),
            permanent_effect: Some(&effects[0]),
        };
        assert_eq!(
            acknowledge(
                &domain(),
                &baseline(),
                &acks,
                &effects[3].successor,
                &acks[0].intent,
                &full
            ),
            Ok(AckDecision::HistoricalReplay)
        );
        // A missing old ACK cannot overwrite a later native head.
        assert_eq!(
            acknowledge(
                &domain(),
                &baseline(),
                &[],
                &effects[3].successor,
                &acks[0].intent,
                &full
            ),
            Err(Refusal::BrokenChain)
        );
        let mut other = effects[0].clone();
        other.closure_digest.push('x');
        let wrong = Evidence {
            receipt: Receipt::Full(&other),
            ..full
        };
        assert_eq!(
            recover_effect(&acks[0].intent, &wrong),
            Err(Refusal::Conflict)
        );
        for field in 0..5 {
            let mut changed = acks[0].intent.clone();
            match field {
                0 => changed.domain.incarnation.push('x'),
                1 => changed.command.payload_digest.push('x'),
                2 => changed.expected.native.mapping_digest.push('x'),
                3 => changed.new_mapping_digest.push('x'),
                _ => changed.expected_generation += 1,
            }
            let compacted = Evidence {
                receipt: Receipt::Compacted,
                permanent_provenance: Some(&effects[0].provenance),
                permanent_effect: Some(&effects[0]),
            };
            assert_eq!(recover_effect(&changed, &compacted), Err(Refusal::Conflict));
        }
    }
    #[test]
    fn unsupported_dispositions_and_revision_exhaustion_fail_closed() {
        let (effects, acks) = chain();
        for disposition in [
            Disposition::UnavailableOrExcluded,
            Disposition::EquivalenceRenewal,
            Disposition::UnsupportedGraph,
        ] {
            let mut effect = effects[0].clone();
            effect.disposition = disposition;
            assert_eq!(
                append_native(&domain(), &baseline().native, &[], &effect),
                Err(Refusal::UnsupportedDisposition)
            );
        }
        for field in 0..3 {
            let mut intent = acks[0].intent.clone();
            match field {
                0 => intent.expected.source_revision = u64::MAX,
                1 => intent.expected.binding_revision = u64::MAX,
                _ => intent.expected.lifecycle_epoch = u64::MAX,
            }
            assert_eq!(resulting(&intent, &effects[0]), Err(Refusal::Exhausted));
        }
        let mut effect = effects[0].clone();
        effect.predecessor.revision = u64::MAX;
        assert_eq!(valid_effect(&effect), Err(Refusal::Exhausted));
    }
    #[test]
    fn generation_chains_never_go_backward_but_allow_equal_boundary_and_old_replay() {
        let (effects, acks) = chain();
        for expected in [
            0,
            effects[0].provenance.committed_generation - 1,
            effects[0].provenance.committed_generation,
        ] {
            let mut second = effects[1].clone();
            second.provenance.expected_generation = expected;
            // Each effect remains internally valid; only the cross-step fence
            // distinguishes the impossible backwards histories.
            valid_effect(&second).unwrap();
            let allowed = expected == effects[0].provenance.committed_generation;
            let result = append_native(&domain(), &baseline().native, &effects[..1], &second);
            assert_eq!(result.is_ok(), allowed);
            let history = vec![effects[0].clone(), second.clone()];
            let replay = append_native(&domain(), &baseline().native, &history, &effects[0]);
            assert_eq!(replay.is_ok(), allowed);
            if allowed {
                assert_eq!(replay, Ok(NativeDecision::HistoricalReplay));
            }

            let mut second_ack = acks[1].clone();
            second_ack.intent.expected_generation = expected;
            second_ack.effect = second.clone();
            let proof = Evidence {
                receipt: Receipt::Compacted,
                permanent_provenance: Some(&second.provenance),
                permanent_effect: Some(&second),
            };
            let result = acknowledge(
                &domain(),
                &baseline(),
                &acks[..1],
                &second.successor,
                &second_ack.intent,
                &proof,
            );
            assert_eq!(result.is_ok(), allowed);
            let history = vec![acks[0].clone(), second_ack];
            let old = Evidence {
                receipt: Receipt::Full(&effects[0]),
                permanent_provenance: Some(&effects[0].provenance),
                permanent_effect: Some(&effects[0]),
            };
            let replay = acknowledge(
                &domain(),
                &baseline(),
                &history,
                &second.successor,
                &acks[0].intent,
                &old,
            );
            assert_eq!(replay.is_ok(), allowed);
            if allowed {
                assert_eq!(replay, Ok(AckDecision::HistoricalReplay));
            }
        }
    }
}
