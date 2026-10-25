//! Fixed service94 transitive-reader preflight, never authority or a peak-RSS claim.
//!
//! Scans borrowed SQLite cells BEFORE the journal/lifecycle/import/pair readers
//! allocate their owned strings or decoded graphs. It deliberately includes all
//! mappings, registry rows and full lifecycle history, including a selected
//! source with no relocation admission yet. No blanket scratch reserve is used.
//!
//! Six trees cover94 caller/callee typed copies, the allowed-mapping clone,
//! admission/effect Values, and canonical roundtrip overlap. Sequential calls
//! reuse this reservation; concurrently retained inputs are summed. Existing
//! 89/95/96 controls retain their established four-tree,32-byte convention.
//! Future preparation, command, receipt, ACK and insertion graphs which do not
//! exist in these rows are a separate BEFORE-allocation caller obligation.
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

/// Named conservative logical charges. These are inputs to the whole-owner
/// admission calculation, not observations of Rust allocator or SQLx-worker RSS.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct TransitiveBytes {
    pub(crate) sql_text: u64,
    pub(crate) parser_scratch: u64,
    pub(crate) decoded_strings: u64,
    pub(crate) graph_nodes: u64,
    pub(crate) canonical_encoding: u64,
    pub(crate) row_scalars: u64,
    pub(crate) frozen_source_scalars: u64,
    pub(crate) deferred_embedded_text: u64,
    pub(crate) sqlite_growth: u64,
}
impl TransitiveBytes {
    pub(crate) fn total(&self) -> anyhow::Result<u64> {
        [self.sql_text, self.parser_scratch, self.decoded_strings,
            self.graph_nodes, self.canonical_encoding, self.row_scalars,
            self.frozen_source_scalars, self.deferred_embedded_text,
            self.sqlite_growth].into_iter().try_fold(0u64, |total, bytes| {
                total.checked_add(bytes).context("owned94 allocation overflow")
            })
    }
}
#[derive(Clone, Copy)]
enum Charge {
    SqlText,
    ParserScratch,
    DecodedStrings,
    GraphNodes,
    CanonicalEncoding,
    RowScalars,
    FrozenSourceScalars,
    DeferredEmbeddedText,
    SqliteGrowth,
}

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
    named: TransitiveBytes,
    ceiling: u64,
    rows: u64,
    deadline: Instant,
    cancel: &'a AtomicBool,
}
impl Budget<'_> {
    fn alive(&self) -> anyhow::Result<()> {
        ensure!(
            !self.cancel.load(Ordering::Acquire) && Instant::now() < self.deadline,
            "owned94 allocation scan cancelled or deadline exhausted"
        );
        Ok(())
    }
    fn add(&mut self, charge: Charge, bytes: u64) -> anyhow::Result<()> {
        self.alive()?;
        self.used = self
            .used
            .checked_add(bytes)
            .context("owned94 allocation overflow")?;
        ensure!(
            self.used <= self.ceiling,
            "owned94 transitive allocation exceeds remaining ceiling"
        );
        let field = match charge {
            Charge::SqlText => &mut self.named.sql_text,
            Charge::ParserScratch => &mut self.named.parser_scratch,
            Charge::DecodedStrings => &mut self.named.decoded_strings,
            Charge::GraphNodes => &mut self.named.graph_nodes,
            Charge::CanonicalEncoding => &mut self.named.canonical_encoding,
            Charge::RowScalars => &mut self.named.row_scalars,
            Charge::FrozenSourceScalars => &mut self.named.frozen_source_scalars,
            Charge::DeferredEmbeddedText => &mut self.named.deferred_embedded_text,
            Charge::SqliteGrowth => &mut self.named.sqlite_growth,
        };
        *field = field.checked_add(bytes).context("owned94 allocation overflow")?;
        Ok(())
    }
    fn scaled(&mut self, charge: Charge, n: usize, factor: u64) -> anyhow::Result<()> {
        self.add(
            charge,
            u64::try_from(n)?
                .checked_mul(factor)
                .context("owned94 allocation overflow")?,
        )
    }
    fn json_bytes(&mut self, bytes: usize, copies: u64) -> anyhow::Result<()> {
        self.scaled(Charge::SqlText, bytes, 2)?;
        self.scaled(Charge::ParserScratch, bytes, 2)?;
        self.scaled(Charge::DecodedStrings, bytes, 2 * copies)?;
        //94: two encoders with growth and string escaping headroom. The older
        // four-tree paths keep their broader32-byte total convention, with
        // the remaining20 bytes explicitly assigned to encoder overlap.
        self.scaled(Charge::CanonicalEncoding, bytes, if copies == 4 { 20 } else { 8 })
    }
    fn json(&mut self, raw: &[u8], role: Role, copies: u64, depth: usize) -> anyhow::Result<()> {
        // Admit raw escaped-string scratch BEFORE entering serde. No Value
        // tree is allocated; only the explicitly deferred library scalar is.
        self.json_bytes(raw.len(), copies)?;
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
            return Err(serde::de::Error::custom("owned94 JSON depth exceeded"));
        }
        self.budget
            .add(Charge::GraphNodes, NODE * self.copies)
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
            .add(Charge::GraphNodes, NODE * self.1)
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
                self.budget.scaled(Charge::DeferredEmbeddedText, s.len(), 4).map_err(E::custom)?;
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
                .add(Charge::FrozenSourceScalars, 6 * (2 * (256 + 256 + 64) + NODE))
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
        b.rows = b.rows.checked_add(1).context("owned94 row count overflow")?;
        ensure!(b.rows <= MAX_ROWS, "owned94 transitive row cohort exceeded");
        let now = sqlite_bytes(db)?;
        b.add(Charge::SqliteGrowth, now.saturating_sub(*observed))?;
        *observed = (*observed).max(now);
        b.add(Charge::RowScalars, NODE * copies)?;
        for col in 0..cols {
            match row.get_ref(col)? {
                ValueRef::Text(raw) => {
                    if let Some((_, role)) = json.iter().find(|(i, _)| *i == col) {
                        b.json(raw, *role, copies, 0)?;
                    } else {
                        b.scaled(Charge::SqlText, raw.len(), copies * 4)?;
                    }
                }
                ValueRef::Null if !json.iter().any(|(i, _)| *i == col) => b.add(Charge::RowScalars, 8)?,
                ValueRef::Integer(_) if !json.iter().any(|(i, _)| *i == col) => {
                    b.add(Charge::RowScalars, 8 * copies)?
                }
                _ => anyhow::bail!("owned94 transitive storage class differs"),
            }
        }
    }
    let now = sqlite_bytes(db)?;
    b.add(Charge::SqliteGrowth, now.saturating_sub(*observed))?;
    *observed = (*observed).max(now);
    Ok(())
}

/// SAME retained WRITE, fixed read-only SQL, absolute caller deadline. The
/// caller has already subtracted existing target/native/host/owner retention.
/// Return includes observable SQLite growth during this scan, not initial DB
/// storage. It grants nothing and must precede EVERY transitive owned decode
/// cohort under this exact transaction; a new transaction needs a new scan.
/// The caller must reserve prospective new94 outputs separately BEFORE building
/// them. Accounting an object only after construction is not pre-admission.
pub(crate) async fn preflight_in(
    db: &mut sqlx::SqliteConnection,
    remaining: u64,
    deadline: Instant,
    cancel: &AtomicBool,
) -> anyhow::Result<TransitiveBytes> {
    ensure!(!cancel.load(Ordering::Acquire) && Instant::now() < deadline,
        "owned94 allocation scan cancelled or deadline exhausted");
    let mut lease = db.lock_handle().await?;
    ensure!(!cancel.load(Ordering::Acquire) && Instant::now() < deadline,
        "owned94 allocation scan cancelled or deadline exhausted");
    let handle = lease.as_raw_handle().as_ptr();
    ensure!(
        unsafe { rusqlite::ffi::sqlite3_get_autocommit(handle) } == 0
            && unsafe { rusqlite::ffi::sqlite3_txn_state(handle, c"main".as_ptr()) } == 2,
        "owned94 allocation scan requires retained WRITE"
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
) -> anyhow::Result<TransitiveBytes> {
    let mut b = Budget {
        used: 0,
        named: TransitiveBytes::default(),
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
    // A first94 operation has no admission row. Its baseline still loads all
    // imported descriptors and frozen imports, while renewal reads complete
    // selected-source lifecycle/history. Scan a conservative superset without
    // constructing an owned list of source IDs or relying on saved intents.
    scan(
        db,
        &mut b,
        "SELECT source_id,epoch,catalog_id,incarnation,mapping_digest,source_revision,binding_revision,state,operation_id,proof_json,created_at FROM catabolic_source_lifecycle",
        &[(9, Role::Plain)],
        6,
        &mut observed,
    )?;
    scan(
        db,
        &mut b,
        "SELECT id,descriptor_json FROM catabolic_source_registry",
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
    b.alive()?;
    ensure!(b.named.total()? == b.used, "owned94 named allocation mismatch");
    Ok(b.named)
}

/// Admit clone, canonical serialization and roundtrip decode overlap for an
/// already-retained typed input, BEFORE constructing any of those new outputs.
/// Inputs must use stable ordinary Serialize implementations (the known94
/// structs and serde_json::Value), not callbacks with allocating/stateful custom
/// serializers. A prospective object which does not yet exist still requires
/// its caller's structural/input envelope before its initial construction.
/// Embedded native JSON strings need the separate persisted Effect scan or
/// an explicit caller-side prospective embedded graph charge.
pub(crate) fn serialized(
    value: &impl serde::Serialize,
    remaining: u64,
    deadline: Instant,
    cancel: &AtomicBool,
) -> anyhow::Result<TransitiveBytes> {
    let mut b = Budget {
        used: 0,
        named: TransitiveBytes::default(),
        ceiling: remaining.min(LIMIT),
        rows: 0,
        deadline,
        cancel,
    };
    // First enforce node count and nesting without allocating a Value or an
    // output buffer. The second traversal can then stream the exact JSON byte
    // count, including escaping, into a writer that retains no output bytes.
    value.serialize(NodeSerializer { budget: &mut b, depth: 0 })?;
    struct Bytes<'a, 'b>(&'a mut Budget<'b>);
    impl std::io::Write for Bytes<'_, '_> {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0.json_bytes(bytes.len(), 6).map_err(std::io::Error::other)?;
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> { Ok(()) }
    }
    serde_json::to_writer(Bytes(&mut b), value)?;
    b.alive()?;
    ensure!(b.named.total()? == b.used, "owned94 named allocation mismatch");
    Ok(b.named)
}

struct NodeSerializer<'a, 'b> {
    budget: &'a mut Budget<'b>,
    depth: usize,
}
struct NodeCompound<'a, 'b> {
    budget: &'a mut Budget<'b>,
    depth: usize,
}
// Pinned serde_json 1.0.151 uses this SerializeStruct protocol for Number,
// never SerializeMap. A user object with the same key remains an ordinary map.
const NUMBER_PROTOCOL: &str = "$serde_json::private::Number";
enum NodeStruct<'a, 'b> {
    Ordinary(NodeCompound<'a, 'b>),
    Number { budget: &'a mut Budget<'b>, depth: usize, seen: bool },
}
struct NumberLexical<'a, 'b> {
    budget: &'a mut Budget<'b>,
    depth: usize,
}
type SerResult<T = ()> = Result<T, serde_json::Error>;
fn invalid_number_protocol() -> serde_json::Error {
    serde::ser::Error::custom("owned94 invalid serialized number protocol")
}
fn number_digits(bytes: &[u8], at: &mut usize, budget: &Budget<'_>) -> SerResult {
    let start = *at;
    while matches!(bytes.get(*at), Some(b'0'..=b'9')) {
        if *at % 64 == 0 { budget.alive().map_err(serde::ser::Error::custom)?; }
        *at += 1;
    }
    if *at == start { return Err(invalid_number_protocol()); }
    Ok(())
}
impl NumberLexical<'_, '_> {
    fn string(self, value: &str) -> SerResult {
        self.budget.alive().map_err(serde::ser::Error::custom)?;
        // Validate borrowed JSON-number spelling without parsing into an owned
        // Number/Value or formatting a temporary string. Check long scans too.
        let bytes = value.as_bytes();
        let mut at = usize::from(bytes.first() == Some(&b'-'));
        match bytes.get(at) {
            Some(b'0') => at += 1,
            Some(b'1'..=b'9') => number_digits(bytes, &mut at, self.budget)?,
            _ => return Err(invalid_number_protocol()),
        }
        if bytes.get(at) == Some(&b'.') {
            at += 1;
            number_digits(bytes, &mut at, self.budget)?;
        }
        if matches!(bytes.get(at), Some(b'e' | b'E')) {
            at += 1;
            if matches!(bytes.get(at), Some(b'+' | b'-')) { at += 1; }
            number_digits(bytes, &mut at, self.budget)?;
        }
        if at != bytes.len() { return Err(invalid_number_protocol()); }
        // Match pinned deserialize_any: i64/u64-fitting integer spellings visit
        // one scalar; other numbers visit NumberDeserializer's key/string bridge.
        // The number's root node was already charged by serialize_struct. Keep
        // that bridge's existing two nodes and child-depth check, not a refund.
        let small = if value.starts_with('-') {
            value.parse::<i64>().is_ok()
        } else {
            value.parse::<u64>().is_ok()
        };
        if !small {
            self.budget.add(Charge::GraphNodes, NODE * 6).map_err(serde::ser::Error::custom)?;
            NodeSerializer { budget: &mut *self.budget, depth: self.depth + 1 }.scalar()?;
        }
        // The unchanged second serialization charges all lexical bytes to SQL,
        // parser scratch, decoded strings (including Number's owned spelling),
        // and canonical encoding. No real text/storage charge is removed here.
        self.budget.alive().map_err(serde::ser::Error::custom)
    }
}
macro_rules! reject_number_scalar {
    ($($name:ident($kind:ty)),* $(,)?) => {$(
        fn $name(self, _: $kind) -> SerResult { Err(invalid_number_protocol()) }
    )*};
}
impl serde::Serializer for NumberLexical<'_, '_> {
    type Ok = ();
    type Error = serde_json::Error;
    type SerializeSeq = serde::ser::Impossible<(), serde_json::Error>;
    type SerializeTuple = Self::SerializeSeq;
    type SerializeTupleStruct = Self::SerializeSeq;
    type SerializeTupleVariant = Self::SerializeSeq;
    type SerializeMap = Self::SerializeSeq;
    type SerializeStruct = Self::SerializeSeq;
    type SerializeStructVariant = Self::SerializeSeq;
    reject_number_scalar! {
        serialize_bool(bool), serialize_i8(i8), serialize_i16(i16),
        serialize_i32(i32), serialize_i64(i64), serialize_i128(i128),
        serialize_u8(u8), serialize_u16(u16), serialize_u32(u32),
        serialize_u64(u64), serialize_u128(u128), serialize_f32(f32),
        serialize_f64(f64), serialize_char(char), serialize_bytes(&[u8]),
    }
    fn serialize_str(self, value: &str) -> SerResult { self.string(value) }
    fn serialize_none(self) -> SerResult { Err(invalid_number_protocol()) }
    fn serialize_some<T: ?Sized + serde::Serialize>(self, _: &T) -> SerResult {
        Err(invalid_number_protocol())
    }
    fn serialize_unit(self) -> SerResult { Err(invalid_number_protocol()) }
    fn serialize_unit_struct(self, _: &'static str) -> SerResult { Err(invalid_number_protocol()) }
    fn serialize_unit_variant(self, _: &'static str, _: u32, _: &'static str) -> SerResult {
        Err(invalid_number_protocol())
    }
    fn serialize_newtype_struct<T: ?Sized + serde::Serialize>(self, _: &'static str, _: &T) -> SerResult {
        Err(invalid_number_protocol())
    }
    fn serialize_newtype_variant<T: ?Sized + serde::Serialize>(
        self, _: &'static str, _: u32, _: &'static str, _: &T,
    ) -> SerResult { Err(invalid_number_protocol()) }
    fn serialize_seq(self, _: Option<usize>) -> SerResult<Self::SerializeSeq> {
        Err(invalid_number_protocol())
    }
    fn serialize_tuple(self, _: usize) -> SerResult<Self::SerializeTuple> {
        Err(invalid_number_protocol())
    }
    fn serialize_tuple_struct(self, _: &'static str, _: usize) -> SerResult<Self::SerializeTupleStruct> {
        Err(invalid_number_protocol())
    }
    fn serialize_tuple_variant(
        self, _: &'static str, _: u32, _: &'static str, _: usize,
    ) -> SerResult<Self::SerializeTupleVariant> { Err(invalid_number_protocol()) }
    fn serialize_map(self, _: Option<usize>) -> SerResult<Self::SerializeMap> {
        Err(invalid_number_protocol())
    }
    fn serialize_struct(self, _: &'static str, _: usize) -> SerResult<Self::SerializeStruct> {
        Err(invalid_number_protocol())
    }
    fn serialize_struct_variant(
        self, _: &'static str, _: u32, _: &'static str, _: usize,
    ) -> SerResult<Self::SerializeStructVariant> { Err(invalid_number_protocol()) }
    fn collect_str<T: ?Sized + std::fmt::Display>(self, _: &T) -> SerResult {
        Err(invalid_number_protocol())
    }
}
impl<'a, 'b> NodeSerializer<'a, 'b> {
    fn scalar(self) -> SerResult {
        if self.depth > MAX_DEPTH {
            return Err(serde::ser::Error::custom("owned94 serialized depth exceeded"));
        }
        self.budget.add(Charge::GraphNodes, NODE * 6)
            .map_err(serde::ser::Error::custom)
    }
    fn compound(self) -> SerResult<NodeCompound<'a, 'b>> {
        let Self { budget, depth } = self;
        NodeSerializer { budget: &mut *budget, depth }.scalar()?;
        Ok(NodeCompound { budget, depth: depth + 1 })
    }
    fn variant(self, variant: &'static str) -> SerResult<NodeCompound<'a, 'b>> {
        let mut outer = self.compound()?;
        outer.value(variant)?;
        Ok(outer)
    }
}
impl NodeCompound<'_, '_> {
    fn value<T: ?Sized + serde::Serialize>(&mut self, value: &T) -> SerResult {
        value.serialize(NodeSerializer { budget: self.budget, depth: self.depth })
    }
    fn field<T: ?Sized + serde::Serialize>(&mut self, key: &'static str, value: &T) -> SerResult {
        self.value(key)?;
        self.value(value)
    }
}
macro_rules! scalar_serializer {
    ($($name:ident($kind:ty)),* $(,)?) => {$(
        fn $name(self, _: $kind) -> SerResult { self.scalar() }
    )*};
}
impl<'a, 'b> serde::Serializer for NodeSerializer<'a, 'b> {
    type Ok = ();
    type Error = serde_json::Error;
    type SerializeSeq = NodeCompound<'a, 'b>;
    type SerializeTuple = NodeCompound<'a, 'b>;
    type SerializeTupleStruct = NodeCompound<'a, 'b>;
    type SerializeTupleVariant = NodeCompound<'a, 'b>;
    type SerializeMap = NodeCompound<'a, 'b>;
    type SerializeStruct = NodeStruct<'a, 'b>;
    type SerializeStructVariant = NodeCompound<'a, 'b>;

    scalar_serializer! {
        serialize_bool(bool), serialize_i8(i8), serialize_i16(i16),
        serialize_i32(i32), serialize_i64(i64), serialize_i128(i128),
        serialize_u8(u8), serialize_u16(u16), serialize_u32(u32),
        serialize_u64(u64), serialize_u128(u128), serialize_f32(f32),
        serialize_f64(f64), serialize_char(char), serialize_str(&str),
    }
    fn serialize_bytes(self, value: &[u8]) -> SerResult {
        let mut seq = self.compound()?;
        for byte in value { seq.value(byte)?; }
        Ok(())
    }
    fn serialize_none(self) -> SerResult { self.scalar() }
    fn serialize_some<T: ?Sized + serde::Serialize>(self, value: &T) -> SerResult {
        value.serialize(self)
    }
    fn serialize_unit(self) -> SerResult { self.scalar() }
    fn serialize_unit_struct(self, _: &'static str) -> SerResult { self.scalar() }
    fn serialize_unit_variant(self, _: &'static str, _: u32, _: &'static str) -> SerResult {
        self.scalar()
    }
    fn serialize_newtype_struct<T: ?Sized + serde::Serialize>(
        self, _: &'static str, value: &T,
    ) -> SerResult { value.serialize(self) }
    fn serialize_newtype_variant<T: ?Sized + serde::Serialize>(
        self, _: &'static str, _: u32, variant: &'static str, value: &T,
    ) -> SerResult {
        self.variant(variant)?.value(value)
    }
    fn serialize_seq(self, _: Option<usize>) -> SerResult<Self::SerializeSeq> { self.compound() }
    fn serialize_tuple(self, _: usize) -> SerResult<Self::SerializeTuple> { self.compound() }
    fn serialize_tuple_struct(self, _: &'static str, _: usize) -> SerResult<Self::SerializeTupleStruct> {
        self.compound()
    }
    fn serialize_tuple_variant(
        self, _: &'static str, _: u32, variant: &'static str, _: usize,
    ) -> SerResult<Self::SerializeTupleVariant> {
        let outer = self.variant(variant)?;
        NodeSerializer { budget: outer.budget, depth: outer.depth }.compound()
    }
    fn serialize_map(self, _: Option<usize>) -> SerResult<Self::SerializeMap> { self.compound() }
    fn serialize_struct(self, name: &'static str, len: usize) -> SerResult<Self::SerializeStruct> {
        if name != NUMBER_PROTOCOL { return self.compound().map(NodeStruct::Ordinary); }
        if len != 1 { return Err(invalid_number_protocol()); }
        let Self { budget, depth } = self;
        NodeSerializer { budget: &mut *budget, depth }.scalar()?;
        Ok(NodeStruct::Number { budget, depth, seen: false })
    }
    fn serialize_struct_variant(
        self, _: &'static str, _: u32, variant: &'static str, _: usize,
    ) -> SerResult<Self::SerializeStructVariant> {
        let outer = self.variant(variant)?;
        NodeSerializer { budget: outer.budget, depth: outer.depth }.compound()
    }
    fn collect_str<T: ?Sized + std::fmt::Display>(self, _: &T) -> SerResult { self.scalar() }
}
macro_rules! sequence_counter {
    ($trait:ident, $element:ident) => {
        impl serde::ser::$trait for NodeCompound<'_, '_> {
            type Ok = ();
            type Error = serde_json::Error;
            fn $element<T: ?Sized + serde::Serialize>(&mut self, value: &T) -> SerResult {
                self.value(value)
            }
            fn end(self) -> SerResult { Ok(()) }
        }
    };
}
sequence_counter!(SerializeSeq, serialize_element);
sequence_counter!(SerializeTuple, serialize_element);
sequence_counter!(SerializeTupleStruct, serialize_field);
sequence_counter!(SerializeTupleVariant, serialize_field);
impl serde::ser::SerializeMap for NodeCompound<'_, '_> {
    type Ok = ();
    type Error = serde_json::Error;
    fn serialize_key<T: ?Sized + serde::Serialize>(&mut self, key: &T) -> SerResult {
        self.value(key)
    }
    fn serialize_value<T: ?Sized + serde::Serialize>(&mut self, value: &T) -> SerResult {
        self.value(value)
    }
    fn end(self) -> SerResult { Ok(()) }
}
macro_rules! struct_counter {
    ($trait:ident) => {
        impl serde::ser::$trait for NodeCompound<'_, '_> {
            type Ok = ();
            type Error = serde_json::Error;
            fn serialize_field<T: ?Sized + serde::Serialize>(
                &mut self, key: &'static str, value: &T,
            ) -> SerResult { self.field(key, value) }
            fn end(self) -> SerResult { Ok(()) }
        }
    };
}
impl serde::ser::SerializeStruct for NodeStruct<'_, '_> {
    type Ok = ();
    type Error = serde_json::Error;
    fn serialize_field<T: ?Sized + serde::Serialize>(
        &mut self, key: &'static str, value: &T,
    ) -> SerResult {
        match self {
            Self::Ordinary(compound) => compound.field(key, value),
            Self::Number { budget, depth, seen } => {
                if *seen || key != NUMBER_PROTOCOL { return Err(invalid_number_protocol()); }
                value.serialize(NumberLexical { budget: &mut **budget, depth: *depth })?;
                *seen = true;
                Ok(())
            }
        }
    }
    fn end(self) -> SerResult {
        match self {
            Self::Number { seen: false, .. } => Err(invalid_number_protocol()),
            _ => Ok(()),
        }
    }
}
struct_counter!(SerializeStructVariant);

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;
    fn measured(raw: &str, role: Role, ceiling: u64) -> anyhow::Result<u64> {
        let cancel = AtomicBool::new(false);
        let mut b = Budget {
            used: 0,
            named: TransitiveBytes::default(),
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
            named: TransitiveBytes::default(),
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
            named: TransitiveBytes::default(),
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
            named: TransitiveBytes::default(),
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
    #[test]
    fn named_json_charges_reconcile_with_actual_nodes_and_bytes() {
        let cancel = AtomicBool::new(false);
        let deadline = Instant::now() + Duration::from_secs(5);
        let value = serde_json::json!({"a": [1, true, "escaped\nvalue"]});
        let raw = serde_json::to_string(&value).unwrap();
        let typed = serialized(&value, LIMIT, deadline, &cancel).unwrap();
        assert_eq!(typed.sql_text, raw.len() as u64 * 2);
        assert_eq!(typed.parser_scratch, raw.len() as u64 * 2);
        assert_eq!(typed.decoded_strings, raw.len() as u64 * 12);
        assert_eq!(typed.canonical_encoding, raw.len() as u64 * 8);
        // Object + key + array + three scalar values.
        assert_eq!(typed.graph_nodes, 6 * 6 * NODE);
        assert_eq!(typed.total().unwrap(), measured(&raw, Role::Plain, LIMIT).unwrap());
        assert!(serialized(&value, typed.total().unwrap() - 1, deadline, &cancel).is_err());
    }
    #[test]
    fn pinned_number_protocol_matches_raw_integer_and_large_number_charges() {
        let cancel = AtomicBool::new(false);
        let deadline = Instant::now() + Duration::from_secs(5);
        for (text, nodes) in [
            ("0", 1), ("-0", 1), ("17", 1), ("-17", 1),
            ("18446744073709551615", 1), ("-9223372036854775808", 1),
            ("18446744073709551616", 3), ("-9223372036854775809", 3),
            ("1208925819614629174706176", 3), ("1.5", 3), ("-0.0", 3),
            ("1e400", 3), ("-1E-400", 3), ("1e+0002", 3),
        ] {
            let number: serde_json::Number = serde_json::from_str(text).unwrap();
            let value = serde_json::Value::Number(number);
            let raw = serde_json::to_string(&value).unwrap();
            let charge = serialized(&value, LIMIT, deadline, &cancel).unwrap();
            assert_eq!(charge.graph_nodes, nodes * 6 * NODE, "{text}");
            assert_eq!(charge.sql_text, raw.len() as u64 * 2, "{text}");
            assert_eq!(charge.parser_scratch, raw.len() as u64 * 2, "{text}");
            assert_eq!(charge.decoded_strings, raw.len() as u64 * 12, "{text}");
            assert_eq!(charge.canonical_encoding, raw.len() as u64 * 8, "{text}");
            let total = charge.total().unwrap();
            assert_eq!(total, measured(&raw, Role::Plain, LIMIT).unwrap(), "{text}");
            assert_eq!(serialized(&value, total, deadline, &cancel).unwrap().total().unwrap(), total);
            assert!(serialized(&value, total - 1, deadline, &cancel).is_err(), "{text}");
            let nested = serde_json::json!({"a": [value, true, "escaped\nvalue"]});
            let nested_raw = serde_json::to_string(&nested).unwrap();
            let nested_charge = serialized(&nested, LIMIT, deadline, &cancel).unwrap();
            assert_eq!(nested_charge.graph_nodes, (5 + nodes) * 6 * NODE, "{text}");
            assert_eq!(nested_charge.total().unwrap(), measured(&nested_raw, Role::Plain, LIMIT).unwrap());
        }
        // Exercise the complete borrowed lexical scan and retained spelling,
        // rather than only machine-sized examples or changing the budget.
        let text = "9".repeat(4096);
        let number: serde_json::Number = serde_json::from_str(&text).unwrap();
        let raw = serde_json::to_string(&number).unwrap();
        let charge = serialized(&number, LIMIT, deadline, &cancel).unwrap();
        assert_eq!(charge.graph_nodes, 3 * 6 * NODE);
        assert_eq!(charge.decoded_strings, raw.len() as u64 * 12);
        assert_eq!(charge.total().unwrap(), measured(&raw, Role::Plain, LIMIT).unwrap());
    }
    #[test]
    fn number_sentinel_user_maps_and_ordinary_structs_keep_real_nodes() {
        #[derive(serde::Serialize)]
        struct Ordinary {
            #[serde(rename = "$serde_json::private::Number")]
            spelling: &'static str,
        }
        let cancel = AtomicBool::new(false);
        let deadline = Instant::now() + Duration::from_secs(5);
        for spelling in ["17", "1e400", "not a number"] {
            let value = serde_json::json!({"$serde_json::private::Number": spelling});
            let raw = serde_json::to_string(&value).unwrap();
            let charge = serialized(&value, LIMIT, deadline, &cancel).unwrap();
            assert_eq!(charge.graph_nodes, 3 * 6 * NODE);
            assert_eq!(charge.total().unwrap(), measured(&raw, Role::Plain, LIMIT).unwrap());
            assert_eq!(serialized(&Ordinary { spelling }, LIMIT, deadline, &cancel).unwrap().total().unwrap(),
                charge.total().unwrap());
        }
    }
    #[test]
    fn invalid_number_protocol_refuses_before_encoding() {
        struct Protocol<T> { len: usize, key: &'static str, fields: usize, value: T }
        impl<T: serde::Serialize> serde::Serialize for Protocol<T> {
            fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
                use serde::ser::SerializeStruct;
                let mut out = serializer.serialize_struct(NUMBER_PROTOCOL, self.len)?;
                for _ in 0..self.fields { out.serialize_field(self.key, &self.value)?; }
                out.end()
            }
        }
        fn refuses(value: impl serde::Serialize) {
            let cancel = AtomicBool::new(false);
            let mut b = Budget { used: 0, named: TransitiveBytes::default(), ceiling: LIMIT,
                rows: 0, deadline: Instant::now() + Duration::from_secs(5), cancel: &cancel };
            assert!(value.serialize(NodeSerializer { budget: &mut b, depth: 0 }).is_err());
        }
        fn protocol<T>(value: T) -> Protocol<T> {
            Protocol { len: 1, key: NUMBER_PROTOCOL, fields: 1, value }
        }
        for text in ["", "+1", "01", "-01", "--1", "NaN", "inf", "1.",
            "1e", "1e+", " 1", "1 ", "1,2", "1\n", "1é"] {
            refuses(protocol(text));
        }
        refuses(Protocol { len: 0, ..protocol("1") });
        refuses(Protocol { len: 2, ..protocol("1") });
        refuses(Protocol { key: "not the protocol key", ..protocol("1") });
        refuses(Protocol { fields: 0, ..protocol("1") });
        refuses(Protocol { fields: 2, ..protocol("1") });
        refuses(protocol(true));
        refuses(protocol(1u64));
        refuses(protocol(Some("1")));
        refuses(protocol(["1"]));
        #[derive(serde::Serialize)]
        struct Wrapped(&'static str);
        refuses(protocol(Wrapped("1")));
        struct Collected;
        impl serde::Serialize for Collected {
            fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
                serializer.collect_str(&1)
            }
        }
        refuses(protocol(Collected));
    }
    #[test]
    fn number_protocol_preserves_parser_bridge_depth_and_liveness() {
        let cancel = AtomicBool::new(false);
        let deadline = Instant::now() + Duration::from_secs(5);
        for text in ["17", "1e400"] {
            let number: serde_json::Number = serde_json::from_str(text).unwrap();
            let mut b = Budget { used: 0, named: TransitiveBytes::default(), ceiling: LIMIT,
                rows: 0, deadline, cancel: &cancel };
            let typed = serde::Serialize::serialize(&number,
                NodeSerializer { budget: &mut b, depth: MAX_DEPTH });
            let mut raw = Budget { used: 0, named: TransitiveBytes::default(), ceiling: LIMIT,
                rows: 0, deadline, cancel: &cancel };
            let decoded = raw.json(text.as_bytes(), Role::Plain, 6, MAX_DEPTH);
            assert_eq!(typed.is_ok(), text == "17");
            assert_eq!(typed.is_ok(), decoded.is_ok());
            assert!(serialized(&number, LIMIT, Instant::now(), &cancel).is_err());
            cancel.store(true, Ordering::Release);
            assert!(serialized(&number, LIMIT, deadline, &cancel).is_err());
            cancel.store(false, Ordering::Release);
        }
    }
    #[test]
    fn typed_enums_bytes_and_nested_depth_count_without_value_conversion() {
        #[derive(serde::Serialize)]
        enum Example { Unit, New(Vec<u8>), Tuple(u8, u8), Struct { x: bool } }
        let cancel = AtomicBool::new(false);
        let deadline = Instant::now() + Duration::from_secs(5);
        for value in [Example::Unit, Example::New(vec![1, 2]), Example::Tuple(1, 2),
            Example::Struct { x: true }] {
            let raw = serde_json::to_string(&value).unwrap();
            assert_eq!(serialized(&value, LIMIT, deadline, &cancel).unwrap().total().unwrap(),
                measured(&raw, Role::Plain, LIMIT).unwrap());
        }
        let mut deep = serde_json::Value::Null;
        for _ in 0..=MAX_DEPTH { deep = serde_json::Value::Array(vec![deep]); }
        assert!(serialized(&deep, LIMIT, deadline, &cancel).unwrap_err().to_string().contains("depth"));
        cancel.store(true, Ordering::Release);
        assert!(serialized(&(), LIMIT, deadline, &cancel).is_err());
    }
    fn empty_control_fixture(db: &Connection) {
        db.execute_batch("
            CREATE TABLE catabolic_binding(identity_json);
            CREATE TABLE catabolic_native_upgrade_intent(intent_json);
            CREATE TABLE catabolic_native_upgrade_ack(proof_json);
            CREATE TABLE catabolic_paired_restore_intents(intent_json);
            CREATE TABLE catabolic_paired_restore_acks(proof_json);
            CREATE TABLE catabolic_native_bootstrap_intents(intent_json);
            CREATE TABLE catabolic_native_bootstrap_commands(command_json);
            CREATE TABLE catabolic_native_bootstrap_acks(proof_json);
            CREATE TABLE catabolic_source_state(state_json);
            CREATE TABLE catabolic_source_guards(trigger_sql);
            CREATE TABLE catabolic_source_relocation_admissions(operation_id,source_id,catalog_id,incarnation,predecessor_json,old_mapping_json,new_mapping_json,old_mapping_digest,new_mapping_digest,requester_json,commitment_json,admission_digest,created_at);
            CREATE TABLE catabolic_source_relocation_commands(operation_id,command_json,command_digest,created_at);
            CREATE TABLE catabolic_source_relocation_acks(operation_id,native_effect_json,native_effect_digest,successor_json,created_at);
            CREATE TABLE catabolic_source_lifecycle(source_id,epoch,catalog_id,incarnation,mapping_digest,source_revision,binding_revision,state,operation_id,proof_json,created_at);
            CREATE TABLE catabolic_source_registry(id,descriptor_json);
            CREATE TABLE catabolic_source_mappings(library_id,mapping_json);
            CREATE TABLE catabolic_imported_source_descriptors(source_id,descriptor_json,proof_json,activation_digest);
            CREATE TABLE catabolic_cutover(singleton,import_id,checkpoint_json,activation_envelope,phase);
            CREATE TABLE catabolic_cutover_sets(singleton,root_import_id,manifest_digest,manifest_json,freeze_revision,phase);
            CREATE TABLE catabolic_cutover_parts(import_id,ordinal,checkpoint_json,activation_envelope,phase);
        ").unwrap();
    }
    #[test]
    fn pending_source_without_any_saved94_admission_is_still_scanned() {
        let cancel = AtomicBool::new(false);
        for insert in [
            "INSERT INTO catabolic_source_mappings VALUES('pending','not JSON')",
            "INSERT INTO catabolic_source_registry VALUES('pending','not JSON')",
            "INSERT INTO catabolic_source_lifecycle VALUES('pending',0,'c','i','d',0,0,'active','initial','not JSON',0)",
            "INSERT INTO catabolic_imported_source_descriptors VALUES('other','{}','not JSON','d')",
        ] {
            let db = Connection::open_in_memory().unwrap();
            empty_control_fixture(&db);
            db.execute_batch(insert).unwrap();
            assert!(preflight_sync(&db, LIMIT, Instant::now() + Duration::from_secs(5), &cancel).is_err());
        }
    }
    #[test]
    fn empty_imports_keep_inactive_control_strings_opaque_but_charged() {
        let db = Connection::open_in_memory().unwrap();
        empty_control_fixture(&db);
        let cancel = AtomicBool::new(false);
        let deadline = Instant::now() + Duration::from_secs(5);
        let before = preflight_sync(&db, LIMIT, deadline, &cancel).unwrap();
        db.execute_batch("INSERT INTO catabolic_cutover VALUES(1,'unused','not JSON','opaque envelope','preparing')").unwrap();
        let after = preflight_sync(&db, LIMIT, deadline, &cancel).unwrap();
        assert!(after.sql_text > before.sql_text);
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
