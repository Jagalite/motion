//! Host-owned durable half of native catalog activation. Receipt contents are
//! observations; the native owner remains responsible for receipt authenticity.
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    Validated,
    Activated,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Checkpoint {
    pub catalog_id: String,
    pub incarnation: String,
    pub import_id: String,
    pub input_digest: String,
    pub mapping_digest: String,
    pub reference_ack_id: String,
    pub freeze_revision: u64,
    pub validation_generation: i64,
    pub phase: Phase,
}
impl Checkpoint {
    pub fn valid(&self) -> bool {
        [
            &self.catalog_id,
            &self.incarnation,
            &self.import_id,
            &self.reference_ack_id,
        ]
        .iter()
        .all(|s| !s.is_empty() && s.len() <= 1024 && !s.chars().any(char::is_control))
            && [&self.input_digest, &self.mapping_digest].iter().all(|s| {
                s.len() == 64
                    && s.bytes()
                        .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
            })
            && self.freeze_revision > 0
            && self.freeze_revision <= i64::MAX as u64
            && self.validation_generation >= 0
    }
    pub fn same_binding(&self, other: &Self) -> bool {
        let mut other = other.clone();
        other.phase = self.phase;
        self == &other
    }
    pub fn acknowledge(&self, observed: &Self) -> Result<Self, &'static str> {
        if !self.valid()
            || !observed.valid()
            || !self.same_binding(observed)
            || observed.phase != Phase::Activated
        {
            return Err("cutover receipt binding mismatch");
        }
        let mut next = self.clone();
        next.phase = Phase::Activated;
        Ok(next)
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    fn checkpoint() -> Checkpoint {
        Checkpoint {
            catalog_id: "c".into(),
            incarnation: "i".into(),
            import_id: "m".into(),
            input_digest: "a".repeat(64),
            mapping_digest: "b".repeat(64),
            reference_ack_id: "r".into(),
            freeze_revision: 1,
            validation_generation: 0,
            phase: Phase::Validated,
        }
    }
    #[test]
    fn acknowledgment_is_exact_and_idempotent() {
        let a = checkpoint();
        let mut b = a.clone();
        b.phase = Phase::Activated;
        let done = a.acknowledge(&b).unwrap();
        assert_eq!(done.acknowledge(&b).unwrap(), done);
        for n in 0..8 {
            let mut changed = b.clone();
            match n {
                0 => changed.catalog_id.push('x'),
                1 => changed.incarnation.push('x'),
                2 => changed.import_id.push('x'),
                3 => changed.input_digest = "c".repeat(64),
                4 => changed.mapping_digest = "d".repeat(64),
                5 => changed.reference_ack_id.push('x'),
                6 => changed.freeze_revision += 1,
                _ => changed.validation_generation += 1,
            };
            assert!(a.acknowledge(&changed).is_err());
        }
        assert!(a.acknowledge(&a).is_err());
    }
}

/// Deterministic host half of aggregate admission. The adapter must obtain each
/// observation from an authenticated native receipt before applying this rule.
pub fn admit_complete_set(
    catalog_id: &str,
    incarnation: &str,
    freeze_revision: u64,
    expected: &[(String, String)],
    observations: &[Checkpoint],
) -> Result<(), &'static str> {
    if expected.is_empty() || expected.len() != observations.len() {
        return Err("partition set is incomplete");
    }
    let mut ids = std::collections::BTreeSet::new();
    for ((id, digest), checkpoint) in expected.iter().zip(observations) {
        if !ids.insert(id)
            || !checkpoint.valid()
            || checkpoint.phase != Phase::Activated
            || checkpoint.catalog_id != catalog_id
            || checkpoint.incarnation != incarnation
            || checkpoint.freeze_revision != freeze_revision
            || checkpoint.import_id != *id
            || checkpoint.input_digest != *digest
        {
            return Err("partition observation does not match complete manifest");
        }
    }
    Ok(())
}

#[cfg(test)]
mod aggregate_tests {
    use super::*;
    #[test]
    fn no_prefix_duplicate_reordering_or_restored_catalog_admits() {
        let one = Checkpoint {
            catalog_id: "catalog".into(),
            incarnation: "epoch".into(),
            import_id: "one".into(),
            input_digest: "a".repeat(64),
            mapping_digest: "b".repeat(64),
            reference_ack_id: "ack".into(),
            freeze_revision: 1,
            validation_generation: 1,
            phase: Phase::Activated,
        };
        let mut two = one.clone();
        two.import_id = "two".into();
        let expected = vec![
            (one.import_id.clone(), one.input_digest.clone()),
            (two.import_id.clone(), two.input_digest.clone()),
        ];
        assert!(
            admit_complete_set(
                "catalog",
                "epoch",
                1,
                &expected,
                &[one.clone(), two.clone()]
            )
            .is_ok()
        );
        assert!(admit_complete_set("catalog", "epoch", 1, &expected, &[one.clone()]).is_err());
        assert!(
            admit_complete_set(
                "catalog",
                "epoch",
                1,
                &expected,
                &[two.clone(), one.clone()]
            )
            .is_err()
        );
        assert!(
            admit_complete_set(
                "catalog",
                "restored",
                1,
                &expected,
                &[one.clone(), two.clone()]
            )
            .is_err()
        );
        assert!(
            admit_complete_set(
                "catalog",
                "epoch",
                2,
                &expected,
                &[one.clone(), two.clone()]
            )
            .is_err()
        );
        two.phase = Phase::Validated;
        assert!(admit_complete_set("catalog", "epoch", 1, &expected, &[one.clone(), two]).is_err());
        assert!(
            admit_complete_set(
                "catalog",
                "epoch",
                1,
                &[expected[0].clone(), expected[0].clone()],
                &[one.clone(), one]
            )
            .is_err()
        );
    }
}
