//! Checkpointed logical retention, not peak RSS or a caller-provided grant.
//! Counts actual JSON nodes/lengths and observable capacities, never configured
//! operation-budget maxima. Map nodes use the native owner's conservative 1KiB
//! convention. Four simultaneous representations cover retained, decoded,
//! unsigned-fingerprint, and encoder/journal copies.
use super::*;
use serde::de::{DeserializeSeed, MapAccess, SeqAccess, Visitor};
const LIMIT: u64 = 256 * 1024 * 1024;
const NODE: u64 = 1024;
const FRAMING: u64 = 256 * 1024;
// Preserve 64KiB for bounded owner/path/key fields even when growth uses slack.
const HEADROOM: u64 = FRAMING - 64 * 1024;

pub(super) struct Allocation {
    external: u64,
    forwarded: u64,
    target_highwater: u64,
    retained_raw: u64,
    // Actual immutable archive witness retention, owned by this phase, not H.
    retained_archive: u64,
}
fn add(a: u64, b: u64) -> anyhow::Result<u64> {
    let n = a
        .checked_add(b)
        .context("native41 host allocation overflow")?;
    ensure!(n <= LIMIT, "native41 host retained allocation exceeded");
    Ok(n)
}
fn mul(a: u64, b: u64) -> anyhow::Result<u64> {
    add(
        0,
        a.checked_mul(b)
            .context("native41 host allocation overflow")?,
    )
}
pub(super) fn value_bytes(value: &Value) -> anyhow::Result<u64> {
    let mut n = NODE;
    match value {
        Value::String(s) => n = add(n, s.capacity() as u64)?,
        Value::Array(a) => {
            n = add(
                n,
                mul(a.capacity() as u64, std::mem::size_of::<Value>() as u64)?,
            )?;
            for v in a {
                n = add(n, value_bytes(v)?)?;
            }
        }
        Value::Object(o) => {
            for (k, v) in o {
                n = add(n, add(k.capacity() as u64, value_bytes(v)?)?)?;
            }
        }
        _ => {}
    }
    Ok(n)
}
struct Count<'a>(&'a mut u64);
impl<'de> DeserializeSeed<'de> for Count<'_> {
    type Value = ();
    fn deserialize<D: serde::Deserializer<'de>>(self, d: D) -> Result<(), D::Error> {
        *self.0 = self
            .0
            .checked_add(1)
            .filter(|n| *n <= LIMIT / (4 * NODE))
            .ok_or_else(|| serde::de::Error::custom("native41 JSON node retention exceeded"))?;
        d.deserialize_any(self)
    }
}
impl<'de> Visitor<'de> for Count<'_> {
    type Value = ();
    fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("bounded JSON")
    }
    fn visit_bool<E: serde::de::Error>(self, _: bool) -> Result<(), E> {
        Ok(())
    }
    fn visit_i64<E: serde::de::Error>(self, _: i64) -> Result<(), E> {
        Ok(())
    }
    fn visit_u64<E: serde::de::Error>(self, _: u64) -> Result<(), E> {
        Ok(())
    }
    fn visit_f64<E: serde::de::Error>(self, _: f64) -> Result<(), E> {
        Ok(())
    }
    fn visit_str<E: serde::de::Error>(self, _: &str) -> Result<(), E> {
        Ok(())
    }
    fn visit_unit<E: serde::de::Error>(self) -> Result<(), E> {
        Ok(())
    }
    fn visit_seq<A: SeqAccess<'de>>(self, mut a: A) -> Result<(), A::Error> {
        while a.next_element_seed(Count(self.0))?.is_some() {}
        Ok(())
    }
    fn visit_map<A: MapAccess<'de>>(self, mut a: A) -> Result<(), A::Error> {
        while a.next_key_seed(Count(self.0))?.is_some() {
            a.next_value_seed(Count(self.0))?;
        }
        Ok(())
    }
}
fn json_bytes(raw: &[u8]) -> anyhow::Result<u64> {
    ensure!(
        raw.len() <= MAX_RECORD,
        "native41 host journal exceeds record bound"
    );
    // Parser escape scratch is bounded by the admitted raw bytes. No Value or
    // owned key/string tree is constructed by this pass.
    let mut nodes = 0;
    let mut d = serde_json::Deserializer::from_slice(raw);
    Count(&mut nodes).deserialize(&mut d)?;
    d.end()?;
    add(mul(nodes, NODE)?, mul(raw.len() as u64, 8)?)
}
fn plan_copies(value: &Value) -> anyhow::Result<u64> {
    let mut encoded = EncodedBudget { bytes: 0 };
    serde_json::to_writer(&mut encoded, value)?;
    add(mul(value_bytes(value)?, 4)?, mul(encoded.bytes as u64, 8)?)
}
fn sqlite_bytes(db: &Connection) -> anyhow::Result<u64> {
    let mut n = 0;
    for op in [
        rusqlite::ffi::SQLITE_DBSTATUS_CACHE_USED,
        rusqlite::ffi::SQLITE_DBSTATUS_SCHEMA_USED,
        rusqlite::ffi::SQLITE_DBSTATUS_STMT_USED,
    ] {
        let (mut current, mut high) = (0, 0);
        let rc = unsafe {
            rusqlite::ffi::sqlite3_db_status(db.handle(), op, &mut current, &mut high, 0)
        };
        ensure!(
            rc == rusqlite::ffi::SQLITE_OK && current >= 0,
            "native41 target allocation unavailable"
        );
        n = add(n, current as u64)?;
    }
    Ok(n)
}
impl Allocation {
    pub(super) fn new(external: u64) -> anyhow::Result<Self> {
        add(0, external)?;
        Ok(Self {
            external,
            forwarded: 0,
            target_highwater: 0,
            retained_raw: 0,
            retained_archive: 0,
        })
    }
    /// Initial structured admission only. Independently accumulated non-cohort
    /// H plus the fixed named64 produce the same full H/K as ordinary accounting.
    /// The sole extraction below is of the component THIS call just composed,
    /// not a retrospective credit against arbitrary caller H or accepted K.
    pub(super) fn with_control_workspace(
        db: &Connection,
        external_without_workspace: u64,
    ) -> anyhow::Result<(Self, catabolic_store::migration::offline_bootstrap::OfflineAllocationReservation)> {
        use catabolic_store::migration::offline_bootstrap::{
            OfflineAllocationReservation, OfflineControlWorkspaceBacking,
        };
        let mut full = Self::new(add(external_without_workspace, OfflineControlWorkspaceBacking::BYTES)?)?;
        full.reservation(db, None, None)?;
        let residual = full.forwarded.checked_sub(OfflineControlWorkspaceBacking::BYTES)
            .context("initial named control component missing")?;
        Ok((full, OfflineAllocationReservation::new(residual)?))
    }
    pub(super) fn check(
        &self,
        db: &Connection,
        plan: Option<&Value>,
        command: Option<&CommandEnvelope>,
    ) -> anyhow::Result<()> {
        let current = sqlite_bytes(db)?;
        ensure!(
            current.saturating_sub(self.target_highwater) <= HEADROOM,
            "native41 target grew; refresh before verification"
        );
        let mut measured = Self::new(self.external)?;
        measured.retained_raw = self.retained_raw;
        measured.retained_archive = self.retained_archive;
        measured.reservation(db, plan, command)?;
        // Only the growth portion may be consumed by new measured metadata;
        // the fixed owner/path/key allowance remains reserved throughout.
        ensure!(
            measured.forwarded.saturating_sub(HEADROOM) <= self.forwarded,
            "native41 host allocation grew; refresh retained reservation"
        );
        Ok(())
    }
    /// Reserve the measured output/permission/reread cohort before encoding.
    pub(super) fn admit_raw_storage(&mut self, bytes: u64) -> anyhow::Result<()> {
        add(0, bytes)?;
        self.retained_raw = self.retained_raw.max(bytes);
        Ok(())
    }
    pub(super) fn retain_raw(&mut self, raw: &String) -> anyhow::Result<()> {
        add(0, raw.capacity() as u64)?;
        self.retained_raw = self.retained_raw.max(raw.capacity() as u64);
        Ok(())
    }
    /// Admit immutable archive ownership separately from caller retention H.
    /// K is only a candidate until the enclosing phase successfully forwards it
    /// to the native owner. Repeated admission of this same witness never adds
    /// the whole archive again; its high-water cannot be reclaimed by shrinkage.
    pub(super) fn retain_archive(&mut self, bytes: u64) -> anyhow::Result<()> {
        let retained = self.retained_archive.max(bytes);
        add(add(self.external, self.retained_raw)?, retained)?;
        let forwarded = add(self.forwarded, retained - self.retained_archive)?;
        self.retained_archive = retained;
        self.forwarded = forwarded;
        Ok(())
    }
    /// Fixed prepare/recovery use ONLY before acquiring the native owner.
    /// The capture scanner receives the remainder after existing Motion owner,
    /// journal, parser and SQLite costs, not another full aggregate allowance.
    pub(super) fn initial_archive_remaining(&mut self, db: &Connection) -> anyhow::Result<u64> {
        ensure!(
            self.retained_archive == 0,
            "initial archive admission already consumed"
        );
        self.reservation(db, None, None)?;
        LIMIT
            .checked_sub(self.forwarded)
            .context("archive initial allocation remainder exceeded")
    }
    /// The archive verifier admits original witness PLUS reread itself. Its
    /// allowance therefore excludes exactly this known archive-owned category
    /// from our combined K, while retaining all other host/phase/native costs.
    /// After native acquisition K already includes this category. This does NOT
    /// subtract from an unknown direct-native observation or reconcile one.
    pub(super) fn archive_validation_remaining(&self, native_retained: u64) -> anyhow::Result<u64> {
        let without_archive = self
            .forwarded
            .checked_sub(self.retained_archive)
            .context("archive allocation provenance differs")?;
        LIMIT
            .checked_sub(native_retained)
            .and_then(|remaining| remaining.checked_sub(without_archive))
            .context("archive validation allocation remainder exceeded")
    }
    pub(super) fn external(&self) -> u64 {
        self.external
    }
    /// Host-only retention and the candidate combined high-water. Reservation
    /// computation precedes native admission, so a consuming transfer must
    /// successfully forward the second value before treating it as accepted.
    pub(super) fn source_pair_reservation(&self) -> (u64, u64) {
        (self.external, self.forwarded)
    }
    /// Known old Motion SQLite category, already included in accepted K, not H.
    /// The retained81 transfer calls this only after its final settlement SQL
    /// and final actual DBSTATUS forwarding. The opaque tail is then inert.
    pub(super) fn source_pair_target_highwater(&self) -> anyhow::Result<u64> {
        ensure!(
            add(self.external, self.target_highwater)? <= self.forwarded,
            "source target category exceeds known forwarding"
        );
        Ok(self.target_highwater)
    }
    pub(super) fn set_external(&mut self, external: u64) -> anyhow::Result<()> {
        add(0, external)?;
        self.external = self.external.max(external);
        Ok(())
    }
    pub(super) fn advance(
        &mut self,
        db: &Connection,
        external: u64,
    ) -> anyhow::Result<catabolic_store::migration::offline_bootstrap::OfflineAllocationReservation>
    {
        let old = self.external;
        self.set_external(external)?;
        let target = sqlite_bytes(db)?;
        let growth = add(
            self.external - old,
            target.saturating_sub(self.target_highwater),
        )?;
        self.forwarded = add(self.forwarded, growth)?;
        self.target_highwater = self.target_highwater.max(target);
        Ok(
            catabolic_store::migration::offline_bootstrap::OfflineAllocationReservation::new(
                self.forwarded,
            )?,
        )
    }
    pub(super) fn with_extra(
        &mut self,
        db: &Connection,
        plan: &Value,
        extra: &Value,
    ) -> anyhow::Result<catabolic_store::migration::offline_bootstrap::OfflineAllocationReservation>
    {
        // Compute this phase from the external scalar, not the previous high
        // water: an unchanged refresh cannot repeatedly add the same scratch.
        let mut current = Self::new(self.external)?;
        current.retained_raw = self.retained_raw;
        current.retained_archive = self.retained_archive;
        current.reservation(db, Some(plan), None)?;
        let total = add(current.forwarded, plan_copies(extra)?)?;
        self.forwarded = self.forwarded.max(total);
        self.target_highwater = self.target_highwater.max(current.target_highwater);
        Ok(
            catabolic_store::migration::offline_bootstrap::OfflineAllocationReservation::new(
                self.forwarded,
            )?,
        )
    }
    pub(super) fn reservation(
        &mut self,
        db: &Connection,
        plan: Option<&Value>,
        command: Option<&CommandEnvelope>,
    ) -> anyhow::Result<catabolic_store::migration::offline_bootstrap::OfflineAllocationReservation>
    {
        let mut observed_sqlite = sqlite_bytes(db)?;
        let mut n = add(
            add(add(self.external, FRAMING)?, observed_sqlite)?,
            add(self.retained_raw, self.retained_archive)?,
        )?;
        // Fixed queries only. Borrowed SQLite cells are inspected before any
        // recovery decoder/retained clone; both original89 and mixed95/96
        // ancestry are included in the simultaneously retained reader allowance.
        for sql in [
            "SELECT identity_json FROM catabolic_binding",
            "SELECT intent_json FROM catabolic_native_upgrade_intent",
            "SELECT proof_json FROM catabolic_native_upgrade_ack",
            "SELECT intent_json FROM catabolic_paired_restore_intents",
            "SELECT proof_json FROM catabolic_paired_restore_acks",
            "SELECT intent_json FROM catabolic_native_bootstrap_intents",
            "SELECT command_json FROM catabolic_native_bootstrap_commands",
            "SELECT proof_json FROM catabolic_native_bootstrap_acks",
            "SELECT state_json FROM catabolic_source_state",
        ] {
            let mut q = db.prepare(sql)?;
            let mut rows = q.query([])?;
            let mut count = 0usize;
            while let Some(row) = rows.next()? {
                count += 1;
                ensure!(count <= 10000, "native41 host journal row bound exceeded");
                let raw = match row.get_ref(0)? {
                    rusqlite::types::ValueRef::Text(raw) => raw,
                    _ => anyhow::bail!("native41 host journal storage class differs"),
                };
                let current_sqlite = sqlite_bytes(db)?;
                n = add(n, current_sqlite.saturating_sub(observed_sqlite))?;
                observed_sqlite = observed_sqlite.max(current_sqlite);
                // Admit parser scratch before parsing even a borrowed cell.
                add(n, mul(raw.len() as u64, 8)?)?;
                n = add(n, mul(json_bytes(raw)?, 4)?)?;
            }
        }
        // Freeze verification copies exact schema/guard strings. Account actual
        // bytes and per-row collection overhead, not a maximum schema allowance.
        for sql in [
            "SELECT sql FROM sqlite_schema WHERE sql IS NOT NULL",
            "SELECT trigger_sql FROM catabolic_source_guards",
        ] {
            let mut q = db.prepare(sql)?;
            let mut rows = q.query([])?;
            let mut count = 0;
            while let Some(row) = rows.next()? {
                count += 1;
                ensure!(count <= 10000, "native41 schema row bound exceeded");
                let raw = row.get_ref(0)?.as_str()?;
                n = add(n, add(mul(raw.len() as u64, 8)?, NODE)?)?;
            }
        }
        if let Some(plan) = plan {
            n = add(n, plan_copies(plan)?)?;
        }
        if let Some(command) = command {
            add(n, mul(command.canonical_payload().len() as u64, 8)?)?;
            n = add(
                n,
                mul(json_bytes(command.canonical_payload().as_bytes())?, 4)?,
            )?;
        }
        let final_sqlite = sqlite_bytes(db)?;
        n = add(n, final_sqlite.saturating_sub(observed_sqlite))?;
        self.target_highwater = self.target_highwater.max(observed_sqlite.max(final_sqlite));
        self.forwarded = self.forwarded.max(n);
        Ok(
            catabolic_store::migration::offline_bootstrap::OfflineAllocationReservation::new(
                self.forwarded,
            )?,
        )
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn tiny_actual_json_does_not_reserve_record_allowance() {
        // This measurement deliberately has no operation-limit input.
        assert!(json_bytes(br#"{"budget":18446744073709551615,"actual":1}"#).unwrap() < 65536);
        assert!(value_bytes(&serde_json::json!({"tiny":1})).unwrap() < 65536);
    }
    #[test]
    fn actual_retained_capacity_and_simultaneous_copies_refuse() {
        let mut text = String::with_capacity(70 * 1024 * 1024);
        text.push('x');
        assert!(mul(value_bytes(&Value::String(text)).unwrap(), 4).is_err());
        assert!(json_bytes(&vec![b' '; MAX_RECORD + 1]).is_err());
        let mut a = Allocation::new(128).unwrap();
        a.set_external(127).unwrap();
        assert_eq!(a.external(), 128);
        assert!(Allocation::new(u64::MAX).is_err());
    }
    // These tables exercise arithmetic only, never admission or fake authority.
    fn accounting_db() -> Connection {
        let db = Connection::open_in_memory().unwrap();
        for (table, column) in [
            ("catabolic_binding", "identity_json"),
            ("catabolic_native_upgrade_intent", "intent_json"),
            ("catabolic_native_upgrade_ack", "proof_json"),
            ("catabolic_paired_restore_intents", "intent_json"),
            ("catabolic_paired_restore_acks", "proof_json"),
            ("catabolic_native_bootstrap_intents", "intent_json"),
            ("catabolic_native_bootstrap_commands", "command_json"),
            ("catabolic_native_bootstrap_acks", "proof_json"),
            ("catabolic_source_state", "state_json"),
            ("catabolic_source_guards", "trigger_sql"),
        ] {
            db.execute_batch(&format!("CREATE TABLE {table}({column})"))
                .unwrap();
        }
        db
    }
    #[test]
    fn named_control_composition_preserves_full_host_totals() {
        let db = accounting_db();
        let other = 1024 * 1024;
        let full_h = other + catabolic_store::migration::offline_bootstrap::OfflineControlWorkspaceBacking::BYTES;
        let mut ordinary = Allocation::new(full_h).unwrap();
        ordinary.reservation(&db, None, None).unwrap();
        let (mut named, _) = Allocation::with_control_workspace(&db, other).unwrap();
        assert_eq!(named.source_pair_reservation(), ordinary.source_pair_reservation());
        let first = named.source_pair_reservation();
        named.advance(&db, full_h).unwrap();
        named.reservation(&db, None, None).unwrap();
        assert_eq!(named.source_pair_reservation(), first);
    }
    #[test]
    fn repeated_measured_refresh_does_not_readd_prior_total() {
        let db = accounting_db();
        let mut a = Allocation::new(1024 * 1024).unwrap();
        a.reservation(&db, Some(&serde_json::json!({"n":1})), None)
            .unwrap();
        let first = a.forwarded;
        assert_eq!(a.source_pair_reservation(), (1024 * 1024, first));
        assert!(
            first > 1024 * 1024,
            "combined reservation includes owned costs"
        );
        for _ in 0..10 {
            a.set_external(512 * 1024).unwrap();
            a.reservation(&db, Some(&serde_json::json!({"n":1})), None)
                .unwrap();
        }
        assert_eq!(a.external, 1024 * 1024);
        assert_eq!(a.forwarded, first);
        assert_eq!(a.source_pair_reservation(), (1024 * 1024, first));
    }
    #[test]
    fn actual_journal_copy_overflow_and_storage_class_refuse() {
        let db = accounting_db();
        let raw = serde_json::to_string(&vec![0; 20000]).unwrap();
        db.execute(
            "INSERT INTO catabolic_native_bootstrap_intents VALUES(?)",
            [&raw],
        )
        .unwrap();
        let mut a = Allocation::new(220 * 1024 * 1024).unwrap();
        assert!(a.reservation(&db, None, None).is_err());
        db.execute("DELETE FROM catabolic_native_bootstrap_intents", [])
            .unwrap();
        db.execute(
            "INSERT INTO catabolic_native_bootstrap_intents VALUES(?)",
            [b"{}".as_slice()],
        )
        .unwrap();
        assert!(
            Allocation::new(0)
                .unwrap()
                .reservation(&db, None, None)
                .is_err()
        );
    }
    #[test]
    fn archive_retention_is_additive_owned_cost_not_host_or_repeated_forward() {
        let db = accounting_db();
        let mut allocation = Allocation::new(1024).unwrap();
        let remaining = allocation.initial_archive_remaining(&db).unwrap();
        let before = allocation.forwarded;
        assert_eq!(remaining, LIMIT - before);
        allocation.retain_archive(4096).unwrap();
        assert_eq!(allocation.source_pair_reservation(), (1024, before + 4096));
        allocation.retain_archive(4096).unwrap();
        allocation.retain_archive(1).unwrap();
        assert_eq!(allocation.source_pair_reservation(), (1024, before + 4096));
        allocation.reservation(&db, None, None).unwrap();
        allocation.check(&db, None, None).unwrap();
        assert!(allocation.forwarded >= before + 4096);
        assert_eq!(allocation.external(), 1024);
        assert!(allocation.initial_archive_remaining(&db).is_err());
        let old = (allocation.retained_archive, allocation.forwarded);
        assert!(allocation.retain_archive(LIMIT).is_err());
        assert_eq!((allocation.retained_archive, allocation.forwarded), old);
    }
    #[test]
    fn archive_validation_accounts_original_plus_reread_under_one_remainder() {
        // Arithmetic only: no synthetic native authority or reconstructed grant.
        let mut allocation = Allocation::new(100).unwrap();
        allocation.forwarded = 1000;
        allocation.retain_archive(200).unwrap();
        assert_eq!(allocation.source_pair_reservation(), (100, 1200));
        assert_eq!(
            allocation.archive_validation_remaining(300).unwrap(),
            LIMIT - 1300
        );
        // Verifier must debit its own original 200 before allocating the reread.
        assert_eq!(
            allocation.archive_validation_remaining(300).unwrap() - 200,
            LIMIT - 300 - allocation.forwarded
        );
        assert!(allocation.archive_validation_remaining(LIMIT).is_err());
        allocation.forwarded = 199;
        assert!(allocation.archive_validation_remaining(0).is_err());
    }
}
