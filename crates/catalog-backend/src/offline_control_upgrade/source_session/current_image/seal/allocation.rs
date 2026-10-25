//! Fixed transitive seal-reader preflight, not an authority or peak-RSS claim.
//!
//! 94 history has caller and callee typed copies, an allowed-mapping clone and
//! admission/effect Values. Its canonical roundtrip temporarily retains the
//! parsed Value, consumed clone and roundtrip Value. Six tree representations
//! conservatively dominate these overlapping paths; sequential verifications
//! are not multiplied by their invocation count. Imported frozen outputs are
//! retained across imports, so their input charges are summed, not reset.
//!
//! Per JSON byte: two for SQL-owned string capacity, two for parser scratch,
//! twelve for six decoded/typed string capacities, eight for two encoder
//! buffers. Each actual key/value node additionally reserves 1KiB per tree,
//! including conservative map/vector growth. Existing89/95/96 readers use their
//! established four-tree,32-byte convention. Scalars/rows are separate.
use anyhow::{Context, ensure};
use rusqlite::{Connection, types::ValueRef};
use serde::de::{DeserializeSeed, MapAccess, SeqAccess, Visitor};
use std::{
    sync::atomic::{AtomicBool, Ordering},
    time::Instant,
};

const LIMIT: u64 = 256 * 1024 * 1024;
const NODE: u64 = 1024;
const MAX_ROWS: u64 = 100_000;
const MAX_DEPTH: usize = 128;

#[derive(Clone, Copy, PartialEq)]
enum Role {
    Plain,
    Effect,
    Audit,
    EvidenceArray,
    EvidenceRecord,
    Data,
    LibraryRow,
    Table,
    DeferredExclusions,
    ImportedProof,
    ImportedRow,
    ZeroArray,
    ZeroRecord,
    Embedded,
}
struct Budget<'a> {
    used: u64,
    ceiling: u64,
    rows: u64,
    deadline: Instant,
    cancel: &'a AtomicBool,
}
impl Budget<'_> {
    fn alive(&self) -> anyhow::Result<()> {
        ensure!(
            !self.cancel.load(Ordering::Acquire) && Instant::now() < self.deadline,
            "seal allocation scan cancelled or deadline exhausted"
        );
        Ok(())
    }
    fn add(&mut self, bytes: u64) -> anyhow::Result<()> {
        self.alive()?;
        self.used = self
            .used
            .checked_add(bytes)
            .context("seal allocation overflow")?;
        ensure!(
            self.used <= self.ceiling,
            "seal transitive allocation exceeds remaining ceiling"
        );
        Ok(())
    }
    fn scaled(&mut self, n: usize, factor: u64) -> anyhow::Result<()> {
        self.add(
            u64::try_from(n)?
                .checked_mul(factor)
                .context("seal allocation overflow")?,
        )
    }
    fn json(&mut self, raw: &[u8], role: Role, copies: u64, depth: usize) -> anyhow::Result<()> {
        // Admit raw escaped-string scratch BEFORE entering serde. No Value
        // tree is allocated; only the explicitly deferred library scalar is.
        self.scaled(raw.len(), if copies == 4 { 32 } else { 24 })?;
        let mut d = serde_json::Deserializer::from_slice(raw);
        Count {
            budget: self,
            role,
            copies,
            depth,
        }
        .deserialize(&mut d)?;
        d.end()?;
        self.alive()
    }
}
enum Output {
    Unit,
    Table(bool),
    Exclusions(String),
}
struct Count<'a, 'b> {
    budget: &'a mut Budget<'b>,
    role: Role,
    copies: u64,
    depth: usize,
}
impl<'de> DeserializeSeed<'de> for Count<'_, '_> {
    type Value = Output;
    fn deserialize<D: serde::Deserializer<'de>>(self, d: D) -> Result<Output, D::Error> {
        if self.depth > MAX_DEPTH {
            return Err(serde::de::Error::custom("seal JSON depth exceeded"));
        }
        self.budget
            .add(NODE * self.copies)
            .map_err(serde::de::Error::custom)?;
        d.deserialize_any(self)
    }
}
#[derive(Clone, Copy)]
enum Key {
    Other,
    Resolved,
    Data,
    Table,
    Row,
    Exclusions,
    SourceRow,
    Zero,
    Document,
    NativeJson,
}
struct KeySeed<'a, 'b>(&'a mut Budget<'b>, u64);
impl<'de> DeserializeSeed<'de> for KeySeed<'_, '_> {
    type Value = Key;
    fn deserialize<D: serde::Deserializer<'de>>(self, d: D) -> Result<Key, D::Error> {
        self.0
            .add(NODE * self.1)
            .map_err(serde::de::Error::custom)?;
        d.deserialize_str(self)
    }
}
impl<'de> Visitor<'de> for KeySeed<'_, '_> {
    type Value = Key;
    fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("JSON key")
    }
    fn visit_str<E: serde::de::Error>(self, s: &str) -> Result<Key, E> {
        self.0.alive().map_err(E::custom)?;
        Ok(match s {
            "resolved_evidence" => Key::Resolved,
            "data" => Key::Data,
            "table" => Key::Table,
            "row" => Key::Row,
            "exclusions_json" => Key::Exclusions,
            "source_row" => Key::SourceRow,
            "retained_zero_effect_receipts" => Key::Zero,
            "document_json" => Key::Document,
            "old_binding_json" | "new_binding_json" | "old_mapping_json" | "new_mapping_json"
            | "policy_json" | "result_json" => Key::NativeJson,
            _ => Key::Other,
        })
    }
}
impl<'de> Visitor<'de> for Count<'_, '_> {
    type Value = Output;
    fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("bounded JSON")
    }
    fn visit_bool<E: serde::de::Error>(self, _: bool) -> Result<Output, E> {
        Ok(Output::Unit)
    }
    fn visit_i64<E: serde::de::Error>(self, _: i64) -> Result<Output, E> {
        Ok(Output::Unit)
    }
    fn visit_u64<E: serde::de::Error>(self, _: u64) -> Result<Output, E> {
        Ok(Output::Unit)
    }
    fn visit_f64<E: serde::de::Error>(self, _: f64) -> Result<Output, E> {
        Ok(Output::Unit)
    }
    fn visit_unit<E: serde::de::Error>(self) -> Result<Output, E> {
        Ok(Output::Unit)
    }
    fn visit_str<E: serde::de::Error>(self, s: &str) -> Result<Output, E> {
        self.budget.alive().map_err(E::custom)?;
        match self.role {
            Role::Embedded => self
                .budget
                .json(s.as_bytes(), Role::Plain, self.copies, self.depth + 1)
                .map_err(E::custom)?,
            Role::Table => return Ok(Output::Table(s == "libraries")),
            Role::DeferredExclusions => {
                // Object keys may put row before table. Keep exactly this one
                // admitted scalar, never a Value or arbitrary user JSON tree.
                self.budget.scaled(s.len(), 4).map_err(E::custom)?;
                return Ok(Output::Exclusions(s.to_owned()));
            }
            _ => {}
        }
        Ok(Output::Unit)
    }
    fn visit_seq<A: SeqAccess<'de>>(self, mut a: A) -> Result<Output, A::Error> {
        let role = match self.role {
            Role::EvidenceArray => Role::EvidenceRecord,
            Role::ZeroArray => Role::ZeroRecord,
            _ => Role::Plain,
        };
        while a
            .next_element_seed(Count {
                budget: self.budget,
                role,
                copies: self.copies,
                depth: self.depth + 1,
            })?
            .is_some()
        {}
        Ok(Output::Unit)
    }
    fn visit_map<A: MapAccess<'de>>(self, mut a: A) -> Result<Output, A::Error> {
        let mut library = false;
        let mut exclusions = None;
        while let Some(key) = a.next_key_seed(KeySeed(self.budget, self.copies))? {
            let role = match (self.role, key) {
                (Role::Audit, Key::Resolved) => Role::EvidenceArray,
                (Role::Audit, Key::Zero) => Role::ZeroArray,
                (Role::EvidenceRecord, Key::Data) => Role::Data,
                (Role::Data, Key::Table) => Role::Table,
                (Role::Data, Key::Row) => Role::LibraryRow,
                (Role::LibraryRow, Key::Exclusions) => Role::DeferredExclusions,
                (Role::ImportedProof, Key::SourceRow) => Role::ImportedRow,
                (Role::ImportedRow, Key::Exclusions)
                | (Role::ZeroRecord, Key::Document)
                | (Role::Effect, Key::NativeJson) => Role::Embedded,
                _ => Role::Plain,
            };
            let value = a.next_value_seed(Count {
                budget: self.budget,
                role,
                copies: self.copies,
                depth: self.depth + 1,
            })?;
            // Follow the final value of duplicate keys, as Value does.
            match (self.role, key) {
                (Role::Data, Key::Table) => library = matches!(value, Output::Table(true)),
                (Role::Data, Key::Row) | (Role::LibraryRow, Key::Exclusions) => {
                    exclusions = match value {
                        Output::Exclusions(v) => Some(v),
                        _ => None,
                    };
                }
                _ => {}
            }
        }
        if self.role == Role::LibraryRow {
            return Ok(exclusions.map_or(Output::Unit, Output::Exclusions));
        }
        if self.role == Role::Data && library {
            // Valid native import identifiers are <=256 bytes, digest64.
            // FrozenMotionSource repeats these three fields per actual source,
            // while source rows/membership/map nodes were counted above.
            self.budget
                .add(6 * (2 * (256 + 256 + 64) + NODE))
                .map_err(serde::de::Error::custom)?;
            if let Some(raw) = exclusions {
                self.budget
                    .json(raw.as_bytes(), Role::Plain, self.copies, self.depth + 1)
                    .map_err(serde::de::Error::custom)?;
            }
        }
        Ok(Output::Unit)
    }
}

fn sqlite_bytes(db: &Connection) -> anyhow::Result<u64> {
    crate::motion_snapshot::connection_allocations(&[db])
}
fn scan(
    db: &Connection,
    b: &mut Budget<'_>,
    sql: &str,
    json: &[(usize, Role)],
    copies: u64,
    observed: &mut u64,
) -> anyhow::Result<()> {
    b.alive()?;
    let mut q = db.prepare(sql)?;
    let cols = q.column_count();
    let mut rows = q.query([])?;
    while let Some(row) = rows.next()? {
        b.rows = b.rows.checked_add(1).context("seal row count overflow")?;
        ensure!(b.rows <= MAX_ROWS, "seal transitive row cohort exceeded");
        let now = sqlite_bytes(db)?;
        b.add(now.saturating_sub(*observed))?;
        *observed = (*observed).max(now);
        b.add(NODE * copies)?;
        for col in 0..cols {
            match row.get_ref(col)? {
                ValueRef::Text(raw) => {
                    if let Some((_, role)) = json.iter().find(|(i, _)| *i == col) {
                        b.json(raw, *role, copies, 0)?;
                    } else {
                        b.scaled(raw.len(), copies * 4)?;
                    }
                }
                ValueRef::Null if !json.iter().any(|(i, _)| *i == col) => b.add(8)?,
                ValueRef::Integer(_) if !json.iter().any(|(i, _)| *i == col) => {
                    b.add(8 * copies)?
                }
                _ => anyhow::bail!("seal transitive storage class differs"),
            }
        }
    }
    let now = sqlite_bytes(db)?;
    b.add(now.saturating_sub(*observed))?;
    *observed = (*observed).max(now);
    Ok(())
}

/// SAME retained WRITE, fixed read-only SQL, absolute caller deadline. The
/// caller has already subtracted existing target/native/host/owner retention.
/// Return includes observable SQLite growth during this scan, not initial DB
/// storage. It grants nothing and must precede any transitive owned decode.
pub(super) async fn preflight_in(
    db: &mut sqlx::SqliteConnection,
    remaining: u64,
    deadline: Instant,
    cancel: &AtomicBool,
) -> anyhow::Result<u64> {
    let mut lease = db.lock_handle().await?;
    let handle = lease.as_raw_handle().as_ptr();
    ensure!(
        unsafe { rusqlite::ffi::sqlite3_get_autocommit(handle) } == 0
            && unsafe { rusqlite::ffi::sqlite3_txn_state(handle, c"main".as_ptr()) } == 2,
        "seal allocation scan requires retained WRITE"
    );
    // Existing non-owning wrapper discipline: drop before the SQLx lease.
    let current = unsafe { Connection::from_handle(handle) }?;
    preflight_sync(&current, remaining, deadline, cancel)
}
fn preflight_sync(
    db: &Connection,
    remaining: u64,
    deadline: Instant,
    cancel: &AtomicBool,
) -> anyhow::Result<u64> {
    let mut b = Budget {
        used: 0,
        ceiling: remaining.min(LIMIT),
        rows: 0,
        deadline,
        cancel,
    };
    b.alive()?;
    let mut observed = sqlite_bytes(db)?;
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
        scan(db, &mut b, sql, &[(0, Role::Plain)], 4, &mut observed)?;
    }
    for sql in [
        "SELECT sql FROM sqlite_schema WHERE sql IS NOT NULL",
        "SELECT trigger_sql FROM catabolic_source_guards",
    ] {
        scan(db, &mut b, sql, &[], 4, &mut observed)?;
    }
    scan(
        db,
        &mut b,
        "SELECT operation_id,source_id,catalog_id,incarnation,predecessor_json,old_mapping_json,new_mapping_json,old_mapping_digest,new_mapping_digest,requester_json,commitment_json,admission_digest,created_at FROM catabolic_source_relocation_admissions",
        &[
            (4, Role::Plain),
            (5, Role::Plain),
            (6, Role::Plain),
            (9, Role::Plain),
            (10, Role::Plain),
        ],
        6,
        &mut observed,
    )?;
    scan(
        db,
        &mut b,
        "SELECT operation_id,command_json,command_digest,created_at FROM catabolic_source_relocation_commands",
        &[(1, Role::Plain)],
        6,
        &mut observed,
    )?;
    scan(
        db,
        &mut b,
        "SELECT operation_id,native_effect_json,native_effect_digest,successor_json,created_at FROM catabolic_source_relocation_acks",
        &[(1, Role::Effect), (3, Role::Plain)],
        6,
        &mut observed,
    )?;
    let affected: bool = db.query_row(
        "SELECT EXISTS(SELECT 1 FROM catabolic_source_relocation_admissions)",
        [],
        |r| r.get(0),
    )?;
    if affected {
        scan(
            db,
            &mut b,
            "SELECT source_id,epoch,catalog_id,incarnation,mapping_digest,source_revision,binding_revision,state,operation_id,proof_json,created_at FROM catabolic_source_lifecycle WHERE source_id IN (SELECT source_id FROM catabolic_source_relocation_admissions)",
            &[(9, Role::Plain)],
            6,
            &mut observed,
        )?;
        scan(
            db,
            &mut b,
            "SELECT id,descriptor_json FROM catabolic_source_registry WHERE id IN (SELECT source_id FROM catabolic_source_relocation_admissions)",
            &[(1, Role::Plain)],
            6,
            &mut observed,
        )?;
        scan(
            db,
            &mut b,
            "SELECT library_id,mapping_json FROM catabolic_source_mappings",
            &[(1, Role::Plain)],
            6,
            &mut observed,
        )?;
        scan(
            db,
            &mut b,
            "SELECT source_id,descriptor_json,proof_json,activation_digest FROM catabolic_imported_source_descriptors",
            &[(1, Role::Plain), (2, Role::ImportedProof)],
            6,
            &mut observed,
        )?;
        // Control rows are always loaded, even when there are no descriptors.
        // Their JSON is parsed only if descriptors exist; raw strings are
        // always owned. Separate scans avoid treating opaque inactive strings
        // as parsed JSON or inventing a provenance grant from an import ID.
        let imported: bool = db.query_row(
            "SELECT EXISTS(SELECT 1 FROM catabolic_imported_source_descriptors)",
            [],
            |r| r.get(0),
        )?;
        scan(
            db,
            &mut b,
            "SELECT import_id,checkpoint_json,activation_envelope,phase FROM catabolic_cutover WHERE singleton=1",
            if imported { &[(1, Role::Plain)] } else { &[] },
            6,
            &mut observed,
        )?;
        scan(
            db,
            &mut b,
            "SELECT root_import_id,manifest_digest,manifest_json,freeze_revision,phase FROM catabolic_cutover_sets WHERE singleton=1",
            if imported { &[(2, Role::Plain)] } else { &[] },
            6,
            &mut observed,
        )?;
        scan(
            db,
            &mut b,
            "SELECT import_id,ordinal,checkpoint_json,activation_envelope,phase FROM catabolic_cutover_parts",
            if imported { &[(2, Role::Plain)] } else { &[] },
            6,
            &mut observed,
        )?;
        if imported {
            // Active import IDs come from fixed control rows. The real loader
            // rejects descriptors outside this set before frozen-source loads.
            // Scanning an unused active part is a conservative private cohort.
            macro_rules! selected {($head:literal)=>{concat!($head," AS selected WHERE EXISTS(SELECT 1 FROM catabolic_cutover c WHERE c.singleton=1 AND c.import_id=selected.import_id) OR EXISTS(SELECT 1 FROM catabolic_cutover_parts p WHERE p.import_id=selected.import_id)")};}
            // Frozen outputs retain mappings across actual selected imports.
            // The extra complete map per selected snapshot conservatively
            // includes unused active parts, without an owned import-ID list.
            scan(
                db,
                &mut b,
                "SELECT m.library_id,m.mapping_json FROM catabolic_source_mappings m CROSS JOIN catabolic_import_snapshots selected WHERE EXISTS(SELECT 1 FROM catabolic_cutover c WHERE c.singleton=1 AND c.import_id=selected.import_id) OR EXISTS(SELECT 1 FROM catabolic_cutover_parts p WHERE p.import_id=selected.import_id)",
                &[(1, Role::Plain)],
                6,
                &mut observed,
            )?;
            scan(
                db,
                &mut b,
                selected!(
                    "SELECT import_id,input_digest,snapshot_json FROM catabolic_import_snapshots"
                ),
                &[(2, Role::Plain)],
                6,
                &mut observed,
            )?;
            scan(
                db,
                &mut b,
                selected!(
                    "SELECT import_id,input_digest,format_version FROM catabolic_motion_exports"
                ),
                &[],
                6,
                &mut observed,
            )?;
            scan(
                db,
                &mut b,
                selected!(
                    "SELECT import_id,input_digest,audit_json FROM catabolic_reference_audits"
                ),
                &[(2, Role::Audit)],
                6,
                &mut observed,
            )?;
            scan(
                db,
                &mut b,
                selected!(
                    "SELECT import_id,motion_file_id,native_profile,native_file_id,mapping_json FROM catabolic_occurrence_mappings"
                ),
                &[(4, Role::Plain)],
                6,
                &mut observed,
            )?;
        }
    }
    b.alive()?;
    Ok(b.used)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;
    fn measured(raw: &str, role: Role, ceiling: u64) -> anyhow::Result<u64> {
        let cancel = AtomicBool::new(false);
        let mut b = Budget {
            used: 0,
            ceiling,
            rows: 0,
            deadline: Instant::now() + Duration::from_secs(5),
            cancel: &cancel,
        };
        b.json(raw.as_bytes(), role, 6, 0)?;
        Ok(b.used)
    }
    #[test]
    fn actual_data_not_large_allowed_ceiling_sets_charge() {
        let a = measured(r#"{"a":[1,true,"small"]}"#, Role::Plain, 1024 * 1024).unwrap();
        assert_eq!(
            a,
            measured(r#"{"a":[1,true,"small"]}"#, Role::Plain, LIMIT).unwrap()
        );
        assert!(a < 128 * 1024);
        let large = format!("[{}]", vec!["null"; 100].join(","));
        assert!(measured(&large, Role::Plain, 128 * 1024).is_err());
    }
    #[test]
    fn raw_scratch_refuses_before_invalid_json_is_parsed() {
        let error = measured(&"x".repeat(1024), Role::Plain, 1024).unwrap_err();
        assert!(error.to_string().contains("allocation exceeds"));
    }
    #[test]
    fn exact_effect_and_imported_paths_parse_nested_json_only() {
        let raw = r#"{"result_json":"not JSON"}"#;
        assert!(measured(raw, Role::Plain, LIMIT).is_ok());
        assert!(measured(raw, Role::Effect, LIMIT).is_err());
        assert!(
            measured(
                r#"{"user":{"result_json":"not JSON"}}"#,
                Role::Effect,
                LIMIT
            )
            .is_ok()
        );
        assert!(
            measured(
                r#"{"source_row":{"exclusions_json":"not JSON"}}"#,
                Role::ImportedProof,
                LIMIT
            )
            .is_err()
        );
        assert!(
            measured(
                r#"{"source_row":{"exclusions_json":"not JSON"}}"#,
                Role::Plain,
                LIMIT
            )
            .is_ok()
        );
    }
    #[test]
    fn library_discriminator_is_order_independent_and_not_arbitrary_key_match() {
        let a = r#"{"resolved_evidence":[{"data":{"row":{"exclusions_json":"[\"a\"]"},"table":"libraries"}}]}"#;
        let b = r#"{"resolved_evidence":[{"data":{"table":"libraries","row":{"exclusions_json":"[\"a\"]"}}}]}"#;
        assert_eq!(
            measured(a, Role::Audit, LIMIT).unwrap(),
            measured(b, Role::Audit, LIMIT).unwrap()
        );
        let opaque = r#"{"resolved_evidence":[{"data":{"row":{"exclusions_json":"not JSON"},"table":"items"}}]}"#;
        assert!(measured(opaque, Role::Audit, LIMIT).is_ok());
        let replaced = r#"{"resolved_evidence":[{"data":{"table":"libraries","row":{"exclusions_json":"not JSON"},"table":"items"}}]}"#;
        assert!(measured(replaced, Role::Audit, LIMIT).is_ok());
        assert!(
            measured(
                r#"{"retained_zero_effect_receipts":[{"document_json":"not JSON"}]}"#,
                Role::Audit,
                LIMIT
            )
            .is_err()
        );
    }
    #[test]
    fn cancellation_deadline_and_combined_nested_depth_refuse() {
        let cancel = AtomicBool::new(true);
        let mut b = Budget {
            used: 0,
            ceiling: LIMIT,
            rows: 0,
            deadline: Instant::now() + Duration::from_secs(5),
            cancel: &cancel,
        };
        assert!(b.json(b"{}", Role::Plain, 6, 0).is_err());
        cancel.store(false, Ordering::Release);
        b.deadline = Instant::now();
        assert!(b.json(b"{}", Role::Plain, 6, 0).is_err());
        b.deadline = Instant::now() + Duration::from_secs(5);
        assert!(b.json(b"{}", Role::Plain, 6, MAX_DEPTH + 1).is_err());
    }
    #[test]
    fn borrowed_storage_refuses_blob_without_owning_payload() {
        let db = Connection::open_in_memory().unwrap();
        db.execute_batch("CREATE TABLE sample(raw); INSERT INTO sample VALUES(zeroblob(1048576))")
            .unwrap();
        let cancel = AtomicBool::new(false);
        let mut b = Budget {
            used: 0,
            ceiling: LIMIT,
            rows: 0,
            deadline: Instant::now() + Duration::from_secs(5),
            cancel: &cancel,
        };
        let mut observed = sqlite_bytes(&db).unwrap();
        let error = scan(
            &db,
            &mut b,
            "SELECT raw FROM sample",
            &[(0, Role::Plain)],
            6,
            &mut observed,
        )
        .unwrap_err();
        assert!(error.to_string().contains("storage class"));
    }
    #[test]
    fn borrowed_large_text_and_actual_nested_nodes_refuse_before_owned_decode() {
        let db = Connection::open_in_memory().unwrap();
        db.execute_batch("CREATE TABLE sample(raw TEXT NOT NULL)")
            .unwrap();
        db.execute(
            "INSERT INTO sample VALUES(?)",
            [format!("\"{}\"", "x".repeat(65536))],
        )
        .unwrap();
        let cancel = AtomicBool::new(false);
        let mut b = Budget {
            used: 0,
            ceiling: 65536,
            rows: 0,
            deadline: Instant::now() + Duration::from_secs(5),
            cancel: &cancel,
        };
        let mut observed = sqlite_bytes(&db).unwrap();
        assert!(
            scan(
                &db,
                &mut b,
                "SELECT raw FROM sample",
                &[(0, Role::Plain)],
                6,
                &mut observed
            )
            .unwrap_err()
            .to_string()
            .contains("allocation exceeds")
        );
        let inner = format!("[{}]", vec!["0"; 128].join(","));
        let raw = serde_json::json!({"result_json":inner}).to_string();
        assert!(measured(&raw, Role::Plain, 128 * 1024).is_ok());
        assert!(measured(&raw, Role::Effect, 128 * 1024).is_err());
    }
    #[tokio::test]
    async fn autocommit_and_read_transaction_are_not_a_writer() {
        use sqlx::Connection as _;
        let mut db = sqlx::SqliteConnection::connect("sqlite::memory:")
            .await
            .unwrap();
        let cancel = AtomicBool::new(false);
        let deadline = Instant::now() + Duration::from_secs(5);
        assert!(
            preflight_in(&mut db, LIMIT, deadline, &cancel)
                .await
                .unwrap_err()
                .to_string()
                .contains("retained WRITE")
        );
        sqlx::query("BEGIN").execute(&mut db).await.unwrap();
        assert!(
            preflight_in(&mut db, LIMIT, deadline, &cancel)
                .await
                .unwrap_err()
                .to_string()
                .contains("retained WRITE")
        );
    }
}
