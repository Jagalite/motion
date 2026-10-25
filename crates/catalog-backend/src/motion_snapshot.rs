//! Bounded database-image snapshot of a fresh, retained Motion writer.
//!
//! This is data, not stopped-service, pair-publication or restore authority.
//! The returned transaction must remain owned through subsequent pair validation
//! and publication. SQLite serialization is synchronous and not interruptible;
//! cancellation retains its connection lease until that call actually returns.
mod binding;
pub(crate) mod source_reader;
pub use source_reader::{
    LegacyAuthorizationReader, RetainedAuthorizationCloseSlot, RetainedAuthorizationReader,
};
pub mod image_read;
pub(crate) mod row_preservation;
pub use binding::VerifiedBinding;

use anyhow::{Context, ensure};
use rusqlite::{Connection, types::ValueRef};
use sqlx::{Sqlite, SqlitePool, Transaction};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

pub(crate) const MAX_IMAGE: u64 = 56 * 1024 * 1024;
const MAX_INVENTORY: usize = 8 * 1024 * 1024;
const INVENTORY_RESERVE: u64 = 32 * 1024 * 1024;
const VERIFIER_RESERVE: u64 = 16 * 1024 * 1024;
const MAX_ROWS: usize = 100_000;
const MAX_TABLES: usize = 1_000;

pub struct VerifiedMotionImage {
    bytes: Vec<u8>,
    inventory_digest: String,
    image_digest: String,
    binding: Option<VerifiedBinding>,
    schema_version: u32,
    schema_digest: String,
    ledger_digest: String,
}
impl VerifiedMotionImage {
    pub fn schema_version(&self) -> u32 {
        self.schema_version
    }
    pub fn schema_sha256(&self) -> &str {
        &self.schema_digest
    }
    pub fn migration_ledger_sha256(&self) -> &str {
        &self.ledger_digest
    }
    pub fn binding(&self) -> Option<&VerifiedBinding> {
        self.binding.as_ref()
    }
    pub fn len(&self) -> usize {
        self.bytes.len()
    }
    pub fn is_empty(&self) -> bool {
        self.bytes.is_empty()
    }
    pub fn sha256(&self) -> &str {
        &self.image_digest
    }
    pub fn inventory_sha256(&self) -> &str {
        &self.inventory_digest
    }
    /// No publication is implied. The caller must discard its stage on error.
    pub fn copy_verified_to(
        &self,
        out: &mut impl std::io::Write,
        cancel: &AtomicBool,
    ) -> anyhow::Result<()> {
        check(cancel)?;
        ensure!(
            catabolic_core::sha256(&self.bytes) == self.image_digest,
            "Motion image changed"
        );
        for chunk in self.bytes.chunks(64 * 1024) {
            check(cancel)?;
            out.write_all(chunk)?;
        }
        check(cancel)
    }
}
struct CancelOnDrop(Arc<AtomicBool>, bool);
impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        if self.1 {
            self.0.store(true, Ordering::Release);
        }
    }
}
fn nonnegative(row: &rusqlite::Row<'_>, column: usize) -> rusqlite::Result<u64> {
    let value: i64 = row.get(column)?;
    u64::try_from(value).map_err(|_| rusqlite::Error::IntegralValueOutOfRange(column, value))
}
fn check(cancel: &AtomicBool) -> anyhow::Result<()> {
    ensure!(!cancel.load(Ordering::Acquire), "Motion snapshot cancelled");
    Ok(())
}

/// Fresh, unmodified Motion writer custody. No SQL/connection/transaction
/// accessor is exposed, and callers cannot wrap an already-mutated transaction.
/// This is not a service-drain, native-host or publication capability.
pub struct FreshWriter {
    transaction: Transaction<'static, Sqlite>,
    runtime: tokio::runtime::Handle,
    binding: Option<binding::Pending>,
    // Actual readonly-reader custody is independent of temporary auth tokens.
    authorization: source_reader::SourceAuthorizationCustody,
}
/// Settle the caller's exact retained SQLx writer without consuming it. This
/// proves rollback/depth/autocommit only, not identity, source or service custody.
/// The caller retains the transaction across cancellation/error and decides
/// when separately owned native/service resources may be closed.
pub async fn close_retained_writer(
    transaction: &mut Transaction<'static, Sqlite>,
) -> anyhow::Result<()> {
    use sqlx::TransactionManager;
    let depth = sqlx::sqlite::SqliteTransactionManager::get_transaction_depth(transaction);
    ensure!(
        depth <= 1,
        "retained writer close cannot adopt a nested transaction"
    );
    sqlx::sqlite::SqliteTransactionManager::rollback(transaction).await?;
    ensure!(
        sqlx::sqlite::SqliteTransactionManager::get_transaction_depth(transaction) == 0,
        "retained writer close did not settle SQLx transaction"
    );
    let mut lease = transaction.lock_handle().await?;
    // SAFETY: nonowning wrapper remains inside the exclusive handle lease;
    // no await or connection escape occurs after construction.
    let db = unsafe { Connection::from_handle(lease.as_raw_handle().as_ptr()) }?;
    ensure!(
        db.is_autocommit(),
        "retained writer close did not settle SQLite transaction"
    );
    Ok(())
}

impl FreshWriter {
    pub async fn begin(pool: &SqlitePool, cancel: &AtomicBool) -> anyhow::Result<Self> {
        check(cancel)?;
        let mut transaction = pool.begin_with("BEGIN IMMEDIATE").await?;
        let binding = binding::validate(&mut transaction).await?;
        check(cancel)?;
        Ok(Self {
            transaction,
            runtime: tokio::runtime::Handle::current(),
            binding,
            authorization: source_reader::SourceAuthorizationCustody::default(),
        })
    }

    /// Settle only this exact retained writer. Cancellation or failure leaves
    /// the connection in this object; callers may drop it only after success.
    /// This does not close native/service custody or mint another snapshot grant.
    pub async fn close(&mut self) -> anyhow::Result<()> {
        if self.authorization.claim.is_some() && !self.authorization.settled {
            ensure!(
                !self.authorization.transferred,
                "source reader was transferred and requires its exact workspace settlement"
            );
            self.authorization.reader.settle_inline().await?;
            self.authorization.settled = true;
        }
        close_retained_writer(&mut self.transaction).await
    }

    /// Call only on the retained blocking owner, after acquiring native custody.
    /// The writer remains in this object through subsequent verification/copy.
    /// The synchronous call never spawns work or releases custody on cancellation.
    /// Pair-wide allocation accounting and publication remain the composer's duty.
    pub fn capture_blocking(&mut self, cancel: &AtomicBool) -> anyhow::Result<VerifiedMotionImage> {
        self.capture_leased(cancel, || Ok(()))
    }

    /// Refuse a staging parent containing the actual main database pathname
    /// (and therefore its WAL/SHM companions), before any native artifact exists.
    /// This is only overlap refusal; snapshot authority remains the held handle.
    pub fn refuse_destination_overlap_blocking(
        &mut self,
        parent: &std::path::Path,
    ) -> anyhow::Result<()> {
        let parent = parent.canonicalize()?;
        let mut lease = self.runtime.block_on(self.transaction.lock_handle())?;
        // SAFETY: lexical non-owning wrapper enclosed by the SQLx handle lease.
        let db = unsafe { Connection::from_handle(lease.as_raw_handle().as_ptr()) }?;
        let source: String = db.query_row(
            "SELECT file FROM pragma_database_list WHERE name='main'",
            [],
            |r| r.get(0),
        )?;
        ensure!(
            !source.is_empty(),
            "pair snapshot requires file-backed Motion database"
        );
        let source = std::path::Path::new(&source).canonicalize()?;
        ensure!(
            !source.starts_with(parent),
            "pair destination contains active Motion database"
        );
        Ok(())
    }

    /// Observe the actual retained writer connection without releasing its lease.
    /// No other SQL may run on this writer between this measurement and native
    /// export. The private fresh writer exposes no such SQL capability.
    pub fn retained_allocations_blocking(&mut self) -> anyhow::Result<u64> {
        let mut lease = self.runtime.block_on(self.transaction.lock_handle())?;
        // SAFETY: same lexical, non-owning SQLx lease contract as capture_leased.
        let connection = unsafe { Connection::from_handle(lease.as_raw_handle().as_ptr()) }?;
        connection_allocations(&[&connection])
    }

    /// Observe the same retained writer from an already-running async owner.
    /// Only the SQLx handle lease is awaited; the writer stays caller-borrowed.
    /// Once leased, the non-owning SQLite wrapper never crosses another await.
    /// Dropping the future releases no writer and grants no restore authority.
    pub async fn retained_allocations(&mut self) -> anyhow::Result<u64> {
        let mut lease = self.transaction.lock_handle().await?;
        // SAFETY: same lexical, non-owning lease as the blocking observation.
        let connection = unsafe { Connection::from_handle(lease.as_raw_handle().as_ptr()) }?;
        connection_allocations(&[&connection])
    }

    /// An additional reservation only reduces the fixed accounting cohort.
    /// The composer measures retained native allocations under its held owner.
    pub fn capture_reserved_blocking(
        &mut self,
        cancel: &AtomicBool,
        reserved: u64,
    ) -> anyhow::Result<VerifiedMotionImage> {
        self.capture_leased_reserved(cancel, reserved, || Ok(()))
    }

    fn capture_leased(
        &mut self,
        cancel: &AtomicBool,
        after_lease: impl FnOnce() -> anyhow::Result<()>,
    ) -> anyhow::Result<VerifiedMotionImage> {
        self.capture_leased_reserved(cancel, 0, after_lease)
    }
    fn capture_leased_reserved(
        &mut self,
        cancel: &AtomicBool,
        reserved: u64,
        after_lease: impl FnOnce() -> anyhow::Result<()>,
    ) -> anyhow::Result<VerifiedMotionImage> {
        check(cancel)?;
        // Guard acquisition and wrapper lifetime stay on this blocking thread.
        // No SQLx call/await occurs while the non-owning wrapper exists.
        let mut lease = self.runtime.block_on(self.transaction.lock_handle())?;
        // SAFETY: SQLx excludes all worker FFI while lease lives. Both crates
        // resolve pinned libsqlite3-sys 0.38.2. This non-owning wrapper is lexical,
        // never sent/stored/returned, and drops before lease; from_handle neither
        // closes the connection nor removes its hooks.
        let connection = unsafe { Connection::from_handle(lease.as_raw_handle().as_ptr()) }?;
        after_lease()?;
        capture_connection(&connection, cancel, self.binding.as_ref(), reserved)
    }
}

pub async fn capture(
    pool: &SqlitePool,
    cancel: Arc<AtomicBool>,
) -> anyhow::Result<(Transaction<'static, Sqlite>, VerifiedMotionImage)> {
    capture_inner(pool, cancel, || Ok(())).await
}
async fn capture_inner(
    pool: &SqlitePool,
    cancel: Arc<AtomicBool>,
    after_lease: impl FnOnce() -> anyhow::Result<()> + Send + 'static,
) -> anyhow::Result<(Transaction<'static, Sqlite>, VerifiedMotionImage)> {
    let mut writer = FreshWriter::begin(pool, &cancel).await?;
    let mut signal = CancelOnDrop(cancel.clone(), true);
    let result = tokio::task::spawn_blocking(move || {
        let image = writer.capture_leased(&cancel, after_lease)?;
        Ok((writer.transaction, image))
    })
    .await
    .context("Motion snapshot blocking owner failed")?;
    signal.1 = false;
    result
}

/// Fixed current-pair reuse of the same bounded copier, not an arbitrary41
/// snapshot constructor. The original WRITE and exact96 proof stay retained.
pub(crate) fn capture_current_source_pair(
    db: &Connection,
    proof: &crate::native_bootstrap::SourcePairProof,
    cancel: &AtomicBool,
    reserved: u64,
) -> anyhow::Result<VerifiedMotionImage> {
    let pending = binding::current_source_pair(db, proof)?;
    let image = capture_connection(db, cancel, Some(&pending), reserved)?;
    proof.verify_in(db)?;
    Ok(image)
}

fn capture_connection(
    db: &Connection,
    cancel: &AtomicBool,
    binding: Option<&binding::Pending>,
    reserved: u64,
) -> anyhow::Result<VerifiedMotionImage> {
    check(cancel)?;
    ensure!(
        !db.is_autocommit(),
        "Motion snapshot requires retained writer"
    );
    // Read-only introspection of this leased handle; never changes transaction.
    let state = unsafe { rusqlite::ffi::sqlite3_txn_state(db.handle(), c"main".as_ptr()) };
    ensure!(state == 2, "Motion snapshot requires WRITE transaction");
    let migrated: i64 = db.query_row(
        "SELECT count(*) FROM _sqlx_migrations WHERE success=1",
        [],
        |r| r.get(0),
    )?;
    ensure!(
        migrated > 0,
        "Motion snapshot requires initialized migration ledger"
    );
    let pages: u64 = db.query_row("PRAGMA main.page_count", [], |r| nonnegative(r, 0))?;
    let page_size: u64 = db.query_row("PRAGMA main.page_size", [], |r| nonnegative(r, 0))?;
    let size = pages
        .checked_mul(page_size)
        .context("Motion image size overflow")?;
    ensure!(
        size > 0 && size <= MAX_IMAGE,
        "Motion snapshot image cohort exceeded"
    );
    // Conservative 4-image reservation, 32 MiB inventory and 16 MiB
    // verifier precharge, plus observed live DB allocations: <=256 MiB.
    // Not a process-wide RSS limit. Source caches are separately bounded below.
    allocation_reserved(&[db], size, reserved)?;
    let before = inventory(db, cancel)?;
    allocation_reserved(&[db], size, reserved)?;
    check(cancel)?;
    let data = db.serialize("main")?;
    check(cancel)?;
    ensure!(
        data.len() as u64 == size,
        "Motion snapshot image size changed"
    );
    allocation_reserved(&[db], size, reserved)?;
    let bytes = portable_image(&data, page_size, size)?;
    let mut copied = Connection::open_in_memory()?;
    copied.pragma_update(None, "cache_size", -2048)?;
    allocation_reserved(&[db, &copied], size, reserved)?;
    copied.deserialize_read_exact("main", std::io::Cursor::new(&bytes), bytes.len(), true)?;
    allocation_reserved(&[db, &copied], size, reserved)?;
    let integrity: String = copied.query_row("PRAGMA integrity_check", [], |r| r.get(0))?;
    ensure!(integrity == "ok", "Motion image integrity failed");
    allocation_reserved(&[db, &copied], size, reserved)?;
    ensure!(
        !copied
            .prepare("PRAGMA foreign_key_check")?
            .query([])?
            .next()?
            .is_some(),
        "Motion image foreign keys failed"
    );
    allocation_reserved(&[db, &copied], size, reserved)?;
    let copied_inventory = inventory(&copied, cancel)?;
    allocation_reserved(&[db, &copied], size, reserved)?;
    ensure!(
        copied_inventory == before && inventory(db, cancel)? == before,
        "Motion image schema/row inventory mismatch"
    );
    check(cancel)?;
    allocation_reserved(&[db, &copied], size, reserved)?;
    let binding = binding::verify_copy(db, &copied, binding)?;
    let schema_version: u32 = copied.query_row(
        "SELECT max(version) FROM _sqlx_migrations WHERE success=1",
        [],
        |r| r.get(0),
    )?;
    let ledger_digest = before
        .ledger
        .context("Motion migration ledger missing from complete inventory")?;
    Ok(VerifiedMotionImage {
        binding,
        image_digest: catabolic_core::sha256(&bytes),
        inventory_digest: before.complete,
        schema_digest: before.schema,
        schema_version,
        ledger_digest,
        bytes,
    })
}

/// SQLite's documented deserialize workaround: only the owned output's two
/// journal-format bytes change. This is a logically equal portable image, not
/// a byte-identical copy of the live WAL-mode database file.
fn portable_image(data: &[u8], page_size: u64, expected: u64) -> anyhow::Result<Vec<u8>> {
    ensure!(
        data.len() >= 100 && &data[..16] == b"SQLite format 3\0",
        "invalid initialized SQLite image header"
    );
    let encoded = u16::from_be_bytes([data[16], data[17]]);
    let actual_page = if encoded == 1 {
        65536
    } else {
        u64::from(encoded)
    };
    ensure!(
        actual_page == page_size
            && actual_page.is_power_of_two()
            && (512..=65536).contains(&actual_page),
        "SQLite image page size mismatch"
    );
    ensure!(
        data.len() as u64 == expected
            && expected > 0
            && expected <= MAX_IMAGE
            && expected % actual_page == 0,
        "SQLite image length mismatch"
    );
    ensure!(
        matches!((data[18], data[19]), (1, 1) | (2, 2)),
        "unsupported SQLite image journal format"
    );
    let mut owned = data.to_vec();
    owned[18] = 1;
    owned[19] = 1;
    Ok(owned)
}

pub(crate) fn connection_allocations(databases: &[&Connection]) -> anyhow::Result<u64> {
    let mut cache = 0u64;
    for db in databases {
        cache = [
            rusqlite::ffi::SQLITE_DBSTATUS_CACHE_USED,
            rusqlite::ffi::SQLITE_DBSTATUS_SCHEMA_USED,
            rusqlite::ffi::SQLITE_DBSTATUS_STMT_USED,
        ]
        .into_iter()
        .try_fold(cache, |sum, op| {
            let (mut current, mut high) = (0, 0);
            let rc = unsafe {
                rusqlite::ffi::sqlite3_db_status(db.handle(), op, &mut current, &mut high, 0)
            };
            ensure!(
                rc == 0 && current >= 0,
                "Motion allocation observation unavailable"
            );
            sum.checked_add(current as u64)
                .context("Motion allocation overflow")
        })?;
    }
    Ok(cache)
}
#[cfg(test)]
fn allocation(databases: &[&Connection], size: u64) -> anyhow::Result<()> {
    allocation_reserved(databases, size, 0)
}
fn allocation_reserved(databases: &[&Connection], size: u64, reserved: u64) -> anyhow::Result<()> {
    let cache = connection_allocations(databases)?;
    ensure!(
        size.checked_mul(4)
            .and_then(|n| n.checked_add(INVENTORY_RESERVE))
            .and_then(|n| n.checked_add(VERIFIER_RESERVE))
            .and_then(|n| n.checked_add(cache))
            .and_then(|n| n.checked_add(reserved))
            .is_some_and(|n| n <= 256 * 1024 * 1024),
        "Motion snapshot allocation budget exceeded"
    );
    Ok(())
}

fn field(out: &mut Vec<u8>, value: ValueRef<'_>) {
    let (tag, bytes): (u8, Vec<u8>) = match value {
        ValueRef::Null => (0, vec![]),
        ValueRef::Integer(n) => (1, n.to_be_bytes().to_vec()),
        ValueRef::Real(n) => (2, n.to_bits().to_be_bytes().to_vec()),
        ValueRef::Text(b) => (3, b.to_vec()),
        ValueRef::Blob(b) => (4, b.to_vec()),
    };
    out.push(tag);
    out.extend_from_slice(&(bytes.len() as u64).to_be_bytes());
    out.extend_from_slice(&bytes);
}
#[derive(Debug, PartialEq, Eq)]
struct InventoryAudit {
    complete: String,
    schema: String,
    ledger: Option<String>,
}
#[derive(Default)]
struct InventoryBudget {
    used: usize,
    count: usize,
}
impl InventoryBudget {
    fn row(&mut self) -> anyhow::Result<()> {
        self.count = self.count.checked_add(1).context("inventory overflow")?;
        ensure!(
            self.count <= MAX_ROWS,
            "Motion row inventory bound exceeded"
        );
        Ok(())
    }
    fn value(&mut self, encoded: &mut Vec<u8>, value: ValueRef<'_>) -> anyhow::Result<()> {
        let size = match value {
            ValueRef::Text(b) | ValueRef::Blob(b) => b.len(),
            _ => 8,
        };
        self.used = self
            .used
            .checked_add(size.checked_add(9).context("inventory overflow")?)
            .context("inventory overflow")?;
        ensure!(
            self.used <= MAX_INVENTORY,
            "Motion row inventory byte bound exceeded"
        );
        field(encoded, value);
        Ok(())
    }
}
// Shared traversal retains the image inventory's exact encoding and order.
// A budget belongs to the whole pass, never to an individual table.
fn schema_inventory(
    db: &Connection,
    cancel: &AtomicBool,
    budget: &mut InventoryBudget,
) -> anyhow::Result<(Vec<u8>, Vec<String>)> {
    let (schema_rows, schema_bytes): (i64, i64) = db.query_row(
        "SELECT count(*), coalesce(sum(length(CAST(type AS BLOB))+length(CAST(name AS BLOB))+length(CAST(tbl_name AS BLOB))+coalesce(length(CAST(sql AS BLOB)),0)),0) FROM sqlite_schema", [], |r| Ok((r.get(0)?,r.get(1)?)))?;
    ensure!(
        schema_rows <= 10000 && schema_bytes >= 0 && schema_bytes <= MAX_INVENTORY as i64,
        "Motion schema preflight bound exceeded"
    );
    let mut summary = Vec::new();
    let mut schema =
        db.prepare("SELECT type,name,tbl_name,sql FROM sqlite_schema ORDER BY type,name")?;
    let mut rows = schema.query([])?;
    let mut tables = Vec::new();
    while let Some(row) = rows.next()? {
        check(cancel)?;
        let mut encoded = Vec::new();
        for i in 0..4 {
            field(&mut encoded, row.get_ref(i)?);
        }
        budget.used = budget
            .used
            .checked_add(encoded.len())
            .context("inventory overflow")?;
        ensure!(
            budget.used <= MAX_INVENTORY,
            "Motion schema inventory too large"
        );
        summary.extend_from_slice(catabolic_core::sha256(&encoded).as_bytes());
        if row.get::<_, String>(0)? == "table" {
            let sql: Option<String> = row.get(3)?;
            if sql
                .as_deref()
                .unwrap_or_default()
                .to_ascii_uppercase()
                .contains("CREATE VIRTUAL TABLE")
            {
                ensure!(
                    sql.as_deref()
                        == Some(
                            "CREATE VIRTUAL TABLE search_fts USING fts5(item_id UNINDEXED, body, tokenize='unicode61 remove_diacritics 0')"
                        ),
                    "unsupported Motion virtual table"
                );
            }
            tables.push(row.get::<_, String>(1)?);
            ensure!(
                tables.len() <= MAX_TABLES,
                "Motion table inventory too large"
            );
        }
    }
    Ok((summary, tables))
}
fn table_row_hashes(
    db: &Connection,
    table: &str,
    cancel: &AtomicBool,
    budget: &mut InventoryBudget,
) -> anyhow::Result<Vec<String>> {
    check(cancel)?;
    let quoted = table.replace('"', "\"\"");
    let without_rowid: i64 = db.query_row(
        "SELECT wr FROM pragma_table_list WHERE schema='main' AND name=?1",
        [table],
        |r| r.get(0),
    )?;
    let names = db
        .prepare("SELECT lower(name) FROM pragma_table_xinfo(?1,'main')")?
        .query_map([table], |r| r.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let selector = if without_rowid == 1 {
        "*".to_owned()
    } else {
        let alias = ["rowid", "_rowid_", "oid"]
            .into_iter()
            .find(|name| !names.iter().any(|n| n == name))
            .context("all implicit rowid names shadowed")?;
        format!("{alias},*")
    };
    let sql = format!(
        "SELECT {selector} FROM main.\"{quoted}\" LIMIT {}",
        MAX_ROWS + 1
    );
    let mut statement = db.prepare(&sql)?;
    let columns = statement.column_count();
    ensure!(columns <= 512, "Motion table column bound exceeded");
    let mut rows = statement.query([])?;
    let mut hashes = Vec::new();
    while let Some(row) = rows.next()? {
        check(cancel)?;
        budget.row()?;
        let mut encoded = Vec::new();
        for i in 0..columns {
            let value = row.get_ref(i)?;
            budget.value(&mut encoded, value)?;
        }
        hashes.push(catabolic_core::sha256(&encoded));
    }
    hashes.sort_unstable();
    Ok(hashes)
}
fn inventory(db: &Connection, cancel: &AtomicBool) -> anyhow::Result<InventoryAudit> {
    let mut budget = InventoryBudget::default();
    let (mut summary, tables) = schema_inventory(db, cancel, &mut budget)?;
    let schema_digest = catabolic_core::sha256(&summary);
    let mut ledger = None;
    for table in tables {
        let table_start = summary.len();
        summary.extend_from_slice(&(table.len() as u64).to_be_bytes());
        summary.extend_from_slice(table.as_bytes());
        let hashes = table_row_hashes(db, &table, cancel, &mut budget)?;
        summary.extend_from_slice(&(hashes.len() as u64).to_be_bytes());
        for hash in hashes {
            summary.extend_from_slice(hash.as_bytes());
        }
        if table == "_sqlx_migrations" {
            ledger = Some(catabolic_core::sha256(&summary[table_start..]));
        }
    }
    Ok(InventoryAudit {
        complete: catabolic_core::sha256(&summary),
        schema: schema_digest,
        ledger,
    })
}

/// Complete bounded schema-and-row commitment under a caller-retained writer.
/// This is evidence only, not schema, identity, restore, or admission authority.
pub(crate) fn retained_inventory_sha256(
    db: &Connection,
    cancel: &AtomicBool,
) -> anyhow::Result<String> {
    check(cancel)?;
    ensure!(
        !db.is_autocommit(),
        "retained inventory requires active transaction"
    );
    let state = unsafe { rusqlite::ffi::sqlite3_txn_state(db.handle(), c"main".as_ptr()) };
    ensure!(state == 2, "retained inventory requires WRITE transaction");
    let measured = inventory(db, cancel)?;
    check(cancel)?;
    Ok(measured.complete)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn retained_inventory_requires_writer_and_does_not_release_it_on_cancellation() {
        let db = Connection::open_in_memory().unwrap();
        db.execute_batch("CREATE TABLE retained(value); INSERT INTO retained VALUES(7)")
            .unwrap();
        let cancel = AtomicBool::new(false);
        assert!(retained_inventory_sha256(&db, &cancel).is_err());
        db.execute_batch("BEGIN IMMEDIATE").unwrap();
        assert_eq!(
            retained_inventory_sha256(&db, &cancel).unwrap(),
            inventory(&db, &cancel).unwrap().complete
        );
        assert!(retained_inventory_sha256(&db, &AtomicBool::new(true)).is_err());
        assert!(!db.is_autocommit());
        db.execute_batch("ROLLBACK").unwrap();
        assert!(retained_inventory_sha256(&db, &cancel).is_err());
    }

    #[test]
    fn extracted_traversal_retains_complete_schema_and_ledger_digest_format() {
        let db = Connection::open_in_memory().unwrap();
        db.execute_batch(
            "CREATE TABLE _sqlx_migrations(version,success,checksum);
             CREATE TABLE payload(value);
             INSERT INTO _sqlx_migrations VALUES(93,1,x'00ff');
             INSERT INTO payload VALUES(NULL),(7),(7.25),('hi'),(x'00ff');",
        )
        .unwrap();
        // Fixed pre-extraction stream: ordered exact schema row hashes, then
        // length-prefixed table names/counts and sorted typed row hashes.
        let observed = inventory(&db, &AtomicBool::new(false)).unwrap();
        assert_eq!(
            observed.schema,
            "66b5c59b5c1541999cd79838d84b6296b2eea199e1aab819f0ec2fff8fac85df"
        );
        assert_eq!(
            observed.ledger.as_deref(),
            Some("d030162232d2ef33cc3cecaf33fc4f9a594b56954e57f597c454dd4b54cdb1fa")
        );
        assert_eq!(
            observed.complete,
            "cfba2f377218bc3d1925891d48f8a7366f35fbf589e9ac95e6000d5210185d99"
        );
    }

    #[tokio::test]
    async fn actual_native_binding_identity_is_exposed_only_after_copy_evidence_verification() {
        let dir = tempfile::tempdir().unwrap();
        let native = dir.path().join("native.sqlite3");
        crate::initialize(native.clone()).await.unwrap();
        let expected = crate::inspect(native.clone()).await.unwrap().identity;
        let pool = crate::test_journal(&dir.path().join("motion.sqlite3"))
            .await
            .unwrap();
        let _backend = crate::NativeCatalog::bind(pool.clone(), native, expected.clone())
            .await
            .unwrap();
        let (tx, image) = capture(&pool, Arc::new(AtomicBool::new(false)))
            .await
            .unwrap();
        let binding = image.binding().expect("actual validated native binding");
        assert_eq!(binding.effective_identity(), &expected);
        assert_eq!(binding.evidence_sha256().len(), 64);
        assert_eq!(image.migration_ledger_sha256().len(), 64);
        assert_eq!(image.schema_sha256().len(), 64);
        assert!(image.schema_version() >= 89);
        tx.rollback().await.unwrap();
    }

    #[tokio::test]
    async fn fresh_writer_retains_custody_before_capture_after_failure_and_after_copy() {
        let dir = tempfile::tempdir().unwrap();
        let pool = crate::test_journal(&dir.path().join("motion.sqlite3"))
            .await
            .unwrap();
        let writer = FreshWriter::begin(&pool, &AtomicBool::new(false))
            .await
            .unwrap();
        assert!(
            tokio::time::timeout(
                std::time::Duration::from_millis(30),
                pool.begin_with("BEGIN IMMEDIATE")
            )
            .await
            .is_err()
        );
        let (writer, digest) = tokio::task::spawn_blocking(move || {
            let mut writer = writer;
            assert!(writer.capture_blocking(&AtomicBool::new(true)).is_err());
            let image = writer.capture_blocking(&AtomicBool::new(false)).unwrap();
            let mut copy = Vec::new();
            image
                .copy_verified_to(&mut copy, &AtomicBool::new(false))
                .unwrap();
            assert_eq!(catabolic_core::sha256(&copy), image.sha256());
            (writer, image.inventory_sha256().to_owned())
        })
        .await
        .unwrap();
        assert_eq!(digest.len(), 64);
        assert!(
            tokio::time::timeout(
                std::time::Duration::from_millis(30),
                pool.begin_with("BEGIN IMMEDIATE")
            )
            .await
            .is_err()
        );
        drop(writer);
        tokio::time::timeout(
            std::time::Duration::from_secs(5),
            pool.begin_with("BEGIN IMMEDIATE"),
        )
        .await
        .unwrap()
        .unwrap()
        .rollback()
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn actual_migrations_fts_image_retains_writer_and_drop_preserves_sqlx() {
        let dir = tempfile::tempdir().unwrap();
        let pool = crate::test_journal(&dir.path().join("motion.sqlite3"))
            .await
            .unwrap();
        sqlx::query(
            "INSERT INTO search_fts(item_id,body) VALUES('snapshot-fixture','measured text')",
        )
        .execute(&pool)
        .await
        .unwrap();
        let (tx, image) = capture(&pool, Arc::new(AtomicBool::new(false)))
            .await
            .unwrap();
        let mut bytes = Vec::new();
        image
            .copy_verified_to(&mut bytes, &AtomicBool::new(false))
            .unwrap();
        assert_eq!(catabolic_core::sha256(&bytes), image.sha256());
        let mut copy = Connection::open_in_memory().unwrap();
        copy.deserialize_read_exact("main", std::io::Cursor::new(bytes), image.len(), true)
            .unwrap();
        assert_eq!(
            copy.query_row(
                "SELECT body FROM search_fts WHERE item_id='snapshot-fixture'",
                [],
                |r| r.get::<_, String>(0)
            )
            .unwrap(),
            "measured text"
        );
        assert!(
            tokio::time::timeout(
                std::time::Duration::from_millis(30),
                pool.begin_with("BEGIN IMMEDIATE")
            )
            .await
            .is_err()
        );
        tx.rollback().await.unwrap();
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT 1")
                .fetch_one(&pool)
                .await
                .unwrap(),
            1
        );
        pool.begin_with("BEGIN IMMEDIATE")
            .await
            .unwrap()
            .rollback()
            .await
            .unwrap();
    }
    #[tokio::test]
    async fn cancellation_refuses_image_and_releases_actual_writer() {
        let dir = tempfile::tempdir().unwrap();
        let pool = crate::test_journal(&dir.path().join("motion.sqlite3"))
            .await
            .unwrap();
        assert!(
            capture(&pool, Arc::new(AtomicBool::new(true)))
                .await
                .is_err()
        );
        let (tx, image) = capture(&pool, Arc::new(AtomicBool::new(false)))
            .await
            .unwrap();
        let mut bytes = Vec::new();
        assert!(
            image
                .copy_verified_to(&mut bytes, &AtomicBool::new(true))
                .is_err()
        );
        assert!(bytes.is_empty());
        tx.rollback().await.unwrap();
        pool.begin_with("BEGIN IMMEDIATE")
            .await
            .unwrap()
            .rollback()
            .await
            .unwrap();
    }
    #[test]
    fn typed_inventory_detects_rowid_type_and_shadow_content_changes() {
        let db = Connection::open_in_memory().unwrap();
        db.execute_batch(
            "CREATE TABLE values_test(value); INSERT INTO values_test(rowid,value) VALUES(1,1);",
        )
        .unwrap();
        let cancel = AtomicBool::new(false);
        let first = inventory(&db, &cancel).unwrap();
        db.execute("UPDATE values_test SET rowid=2", []).unwrap();
        assert_ne!(first, inventory(&db, &cancel).unwrap());
        let second = inventory(&db, &cancel).unwrap();
        db.execute("UPDATE values_test SET value='1'", []).unwrap();
        assert_ne!(second, inventory(&db, &cancel).unwrap());
        db.execute_batch("CREATE VIRTUAL TABLE other_fts USING fts5(body);")
            .unwrap();
        assert!(inventory(&db, &cancel).is_err());
    }
    #[tokio::test]
    async fn aborted_caller_retains_writer_until_blocking_lease_exits() {
        let dir = tempfile::tempdir().unwrap();
        let pool = crate::test_journal(&dir.path().join("motion.sqlite3"))
            .await
            .unwrap();
        let cancel = Arc::new(AtomicBool::new(false));
        let (started, ready) = tokio::sync::oneshot::channel();
        let (release, wait) = std::sync::mpsc::channel();
        let task_pool = pool.clone();
        let task_cancel = cancel.clone();
        let caller = tokio::spawn(async move {
            capture_inner(&task_pool, task_cancel, move || {
                let _ = started.send(());
                wait.recv().unwrap();
                Ok(())
            })
            .await
        });
        ready.await.unwrap();
        caller.abort();
        assert!(matches!(caller.await, Err(error) if error.is_cancelled()));
        assert!(cancel.load(Ordering::Acquire));
        assert!(
            tokio::time::timeout(
                std::time::Duration::from_millis(30),
                pool.begin_with("BEGIN IMMEDIATE")
            )
            .await
            .is_err()
        );
        release.send(()).unwrap();
        tokio::time::timeout(
            std::time::Duration::from_secs(5),
            pool.begin_with("BEGIN IMMEDIATE"),
        )
        .await
        .unwrap()
        .unwrap()
        .rollback()
        .await
        .unwrap();
    }
    #[tokio::test]
    async fn leased_failure_drops_wrapper_without_closing_sqlx_connection() {
        let dir = tempfile::tempdir().unwrap();
        let pool = crate::test_journal(&dir.path().join("motion.sqlite3"))
            .await
            .unwrap();
        assert!(
            capture_inner(&pool, Arc::new(AtomicBool::new(false)), || anyhow::bail!(
                "injected leased failure"
            ))
            .await
            .is_err()
        );
        let mut tx = pool.begin_with("BEGIN IMMEDIATE").await.unwrap();
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT 1")
                .fetch_one(&mut *tx)
                .await
                .unwrap(),
            1
        );
        tx.rollback().await.unwrap();
    }
    #[tokio::test]
    async fn real_wal_snapshot_is_logically_equal_and_source_stays_wal() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("motion.sqlite3");
        let pool = crate::test_journal(&path).await.unwrap();
        let mode: String = sqlx::query_scalar("PRAGMA journal_mode=WAL")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(mode, "wal");
        sqlx::query(
            "INSERT INTO search_fts(item_id,body) VALUES('wal-fixture','uncheckpointed content')",
        )
        .execute(&pool)
        .await
        .unwrap();
        assert!(path.with_extension("sqlite3-wal").exists());
        let (mut tx, image) = capture(&pool, Arc::new(AtomicBool::new(false)))
            .await
            .unwrap();
        assert_eq!(&image.bytes[18..20], &[1, 1]);
        assert_eq!(
            sqlx::query_scalar::<_, String>("PRAGMA journal_mode")
                .fetch_one(&mut *tx)
                .await
                .unwrap(),
            "wal"
        );
        let mut copied = Connection::open_in_memory().unwrap();
        copied
            .deserialize_read_exact(
                "main",
                std::io::Cursor::new(&image.bytes),
                image.len(),
                true,
            )
            .unwrap();
        assert_eq!(
            copied
                .query_row(
                    "SELECT body FROM search_fts WHERE item_id='wal-fixture'",
                    [],
                    |r| r.get::<_, String>(0)
                )
                .unwrap(),
            "uncheckpointed content"
        );
        tx.rollback().await.unwrap();
    }
    #[test]
    fn portable_header_only_changes_documented_bytes_and_refuses_mismatch() {
        let db = Connection::open_in_memory().unwrap();
        db.execute_batch("CREATE TABLE actual(value); INSERT INTO actual VALUES(42)")
            .unwrap();
        let data = db.serialize("main").unwrap();
        let page_size: u64 = db
            .query_row("PRAGMA page_size", [], |r| nonnegative(r, 0))
            .unwrap();
        let mut wal = data.to_vec();
        wal[18] = 2;
        wal[19] = 2;
        let portable = portable_image(&wal, page_size, wal.len() as u64).unwrap();
        assert_eq!(&portable[..18], &wal[..18]);
        assert_eq!(&portable[20..], &wal[20..]);
        assert_eq!(&wal[18..20], &[2, 2]);
        assert!(portable_image(&wal, page_size * 2, wal.len() as u64).is_err());
        assert!(portable_image(&wal, page_size, wal.len() as u64 - 1).is_err());
        wal[0] = 0;
        assert!(portable_image(&wal, page_size, wal.len() as u64).is_err());
    }
    #[tokio::test]
    async fn retained_native_reservation_refuses_without_releasing_fresh_writer() {
        let dir = tempfile::tempdir().unwrap();
        let pool = crate::test_journal(&dir.path().join("motion.sqlite3"))
            .await
            .unwrap();
        let cancel = Arc::new(AtomicBool::new(false));
        let mut writer = FreshWriter::begin(&pool, &cancel).await.unwrap();
        let writer = tokio::task::spawn_blocking(move || {
            assert!(writer.retained_allocations_blocking().unwrap() > 0);
            assert!(
                writer
                    .capture_reserved_blocking(&cancel, 256 * 1024 * 1024)
                    .is_err()
            );
            assert!(
                !writer
                    .capture_reserved_blocking(&cancel, 1024 * 1024)
                    .unwrap()
                    .is_empty()
            );
            writer
        })
        .await
        .unwrap();
        drop(writer);
        pool.close().await;
    }
    #[tokio::test]
    async fn async_retained_observation_keeps_exact_writer_on_drop_and_accounting_refusal() {
        use sqlx::TransactionManager;
        let dir = tempfile::tempdir().unwrap();
        let pool = crate::test_journal(&dir.path().join("motion.sqlite3"))
            .await
            .unwrap();
        let mut writer = FreshWriter::begin(&pool, &AtomicBool::new(false))
            .await
            .unwrap();
        let (original, expected) = {
            let mut lease = writer.transaction.lock_handle().await.unwrap();
            let original = lease.as_raw_handle().as_ptr() as usize;
            // SAFETY: exact writer handle, lexical under this exclusive lease.
            let db = unsafe { Connection::from_handle(lease.as_raw_handle().as_ptr()) }.unwrap();
            (original, connection_allocations(&[&db]).unwrap())
        };
        // This runs inside Tokio, the context that rejects nested block_on.
        assert!(expected > 0);
        assert_eq!(writer.retained_allocations().await.unwrap(), expected);
        drop(writer.retained_allocations()); // unpolled observation cancellation
        // A real caller-side accounting refusal must not consume its writer.
        let refused: anyhow::Result<u64> = async {
            writer
                .retained_allocations()
                .await?
                .checked_add(u64::MAX)
                .context("external retained cohort overflow")
        }
        .await;
        assert!(refused.is_err());
        assert_eq!(
            sqlx::sqlite::SqliteTransactionManager::get_transaction_depth(&writer.transaction),
            1
        );
        {
            let mut lease = writer.transaction.lock_handle().await.unwrap();
            assert_eq!(lease.as_raw_handle().as_ptr() as usize, original);
            // SAFETY: same lexical exclusive lease, without await below.
            let db = unsafe { Connection::from_handle(lease.as_raw_handle().as_ptr()) }.unwrap();
            assert!(!db.is_autocommit());
            assert_eq!(unsafe { rusqlite::ffi::sqlite3_txn_state(db.handle(), c"main".as_ptr()) }, 2);
        }
        assert_eq!(writer.retained_allocations().await.unwrap(), expected);
        writer.close().await.unwrap();
        drop(writer);
        let fresh = pool.begin_with("BEGIN IMMEDIATE").await.unwrap();
        fresh.rollback().await.unwrap();
        pool.close().await;
    }

    #[tokio::test]
    async fn fresh_writer_unpolled_close_keeps_custody_then_proves_settlement() {
        use sqlx::TransactionManager;
        let dir = tempfile::tempdir().unwrap();
        let pool = crate::test_journal(&dir.path().join("motion.sqlite3"))
            .await
            .unwrap();
        let cancel = AtomicBool::new(false);
        let mut writer = FreshWriter::begin(&pool, &cancel).await.unwrap();
        drop(writer.close());
        assert_eq!(
            sqlx::sqlite::SqliteTransactionManager::get_transaction_depth(&writer.transaction),
            1
        );
        {
            let mut lease = writer.transaction.lock_handle().await.unwrap();
            let db = unsafe { Connection::from_handle(lease.as_raw_handle().as_ptr()) }.unwrap();
            assert!(!db.is_autocommit());
        }
        writer.close().await.unwrap();
        writer.close().await.unwrap();
        assert_eq!(
            sqlx::sqlite::SqliteTransactionManager::get_transaction_depth(&writer.transaction),
            0
        );
        assert!(writer.authorization_reader().await.is_err());
        drop(writer);
        let fresh = pool.begin_with("BEGIN IMMEDIATE").await.unwrap();
        fresh.rollback().await.unwrap();
        pool.close().await;
    }

    #[tokio::test]
    async fn fresh_writer_nested_close_refusal_retains_exact_transaction() {
        use sqlx::TransactionManager;
        let dir = tempfile::tempdir().unwrap();
        let pool = crate::test_journal(&dir.path().join("motion.sqlite3"))
            .await
            .unwrap();
        let mut writer = FreshWriter::begin(&pool, &AtomicBool::new(false))
            .await
            .unwrap();
        sqlx::sqlite::SqliteTransactionManager::begin(&mut writer.transaction, None)
            .await
            .unwrap();
        assert!(writer.close().await.is_err());
        assert_eq!(
            sqlx::sqlite::SqliteTransactionManager::get_transaction_depth(&writer.transaction),
            2
        );
        {
            let mut lease = writer.transaction.lock_handle().await.unwrap();
            let db = unsafe { Connection::from_handle(lease.as_raw_handle().as_ptr()) }.unwrap();
            assert!(!db.is_autocommit());
        }
        sqlx::sqlite::SqliteTransactionManager::rollback(&mut writer.transaction)
            .await
            .unwrap();
        writer.close().await.unwrap();
        drop(writer);
        pool.close().await;
    }

    #[test]
    fn allocation_accounts_for_verifier_not_only_source() {
        let source = Connection::open_in_memory().unwrap();
        let verifier = Connection::open_in_memory().unwrap();
        verifier.execute_batch("CREATE TABLE verifier_rows(value); INSERT INTO verifier_rows VALUES(zeroblob(65536));").unwrap();
        let (mut low, mut high) = (0, MAX_IMAGE);
        while low < high {
            let mid = low + (high - low + 1) / 2;
            if allocation(&[&source], mid).is_ok() {
                low = mid;
            } else {
                high = mid - 1;
            }
        }
        assert!(allocation(&[&source], low).is_ok());
        assert!(allocation(&[&source, &verifier], low).is_err());
        assert!(allocation(&[&source, &verifier], u64::MAX).is_err());
    }
}
