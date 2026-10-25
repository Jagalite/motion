//! Exact retained-row comparison for the private offline control upgrade.
//! This is not schema, migration, freeze, or binding admission. The caller must
//! retain the same writer, authenticate both schemas, and verify every new table
//! is empty. No original table is omitted, including the ledger and guard rows.
use super::{Connection, InventoryBudget, ValueRef, check, schema_inventory, table_row_hashes};
use anyhow::{Context, ensure};
use rusqlite::types::Value;
use std::{collections::BTreeMap, sync::atomic::AtomicBool};

/// Exact additions chosen by the reviewed owner, not rows read back and trusted
/// merely because they appeared. Each row contains implicit rowid followed by
/// all table columns in their original order, with exact SQLite storage classes.
/// No other old table has an append exception.
#[derive(Default)]
pub(crate) struct ControlAdditions {
    pub(crate) ledger: Vec<Vec<Value>>,
    pub(crate) guards: Vec<Vec<Value>>,
}

pub(crate) struct RetainedRows {
    tables: BTreeMap<String, Vec<String>>,
}

fn writer(db: &Connection) -> anyhow::Result<()> {
    ensure!(
        !db.is_autocommit(),
        "row preservation requires retained transaction"
    );
    // Read-only observation. This helper neither acquires nor releases custody.
    let state = unsafe { rusqlite::ffi::sqlite3_txn_state(db.handle(), c"main".as_ptr()) };
    ensure!(
        state == 2,
        "row preservation requires retained WRITE transaction"
    );
    Ok(())
}

// Borrowed capacity accounting only. No row encoding, hash, SQLite query or clone.
// The 1KiB map-node allowance matches the existing native logical accounting;
// it includes unobservable BTreeMap spare slots, not an allocator ABI claim.
fn capacity_add(total: &mut u64, bytes: usize) -> anyhow::Result<()> {
    *total = total.checked_add(u64::try_from(bytes)?)
        .context("control retained capacity overflow")?;
    Ok(())
}
pub(crate) fn sql_rows_retained_allocations(rows: &Vec<Vec<Value>>) -> anyhow::Result<u64> {
    let mut total = 0;
    capacity_add(&mut total, rows.capacity().checked_mul(std::mem::size_of::<Vec<Value>>())
        .context("control row capacity overflow")?)?;
    for row in rows {
        capacity_add(&mut total, row.capacity().checked_mul(std::mem::size_of::<Value>())
            .context("control cell capacity overflow")?)?;
        for value in row {
            match value {
                Value::Text(value) => capacity_add(&mut total, value.capacity())?,
                Value::Blob(value) => capacity_add(&mut total, value.capacity())?,
                _ => (),
            }
        }
    }
    Ok(total)
}
impl ControlAdditions {
    pub(crate) fn retained_allocations(&self) -> anyhow::Result<u64> {
        (std::mem::size_of::<Self>() as u64)
            .checked_add(sql_rows_retained_allocations(&self.ledger)?)
            .and_then(|n| n.checked_add(sql_rows_retained_allocations(&self.guards).ok()?))
            .context("control additions capacity overflow")
    }
}
impl RetainedRows {
    pub(crate) fn retained_allocations(&self) -> anyhow::Result<u64> {
        let mut total = std::mem::size_of::<Self>() as u64;
        for (name, rows) in &self.tables {
            capacity_add(&mut total, 1024)?;
            capacity_add(&mut total, name.capacity())?;
            capacity_add(&mut total, rows.capacity().checked_mul(std::mem::size_of::<String>())
                .context("retained hash vector capacity overflow")?)?;
            for hash in rows {
                capacity_add(&mut total, hash.capacity())?;
            }
        }
        Ok(total)
    }
    pub(crate) fn capture(db: &Connection, cancel: &AtomicBool) -> anyhow::Result<Self> {
        check(cancel)?;
        writer(db)?;
        let mut budget = InventoryBudget::default();
        let (_, names) = schema_inventory(db, cancel, &mut budget)?;
        let mut tables = BTreeMap::new();
        for name in names {
            let rows = table_row_hashes(db, &name, cancel, &mut budget)?;
            tables.insert(name, rows);
        }
        check(cancel)?;
        Ok(Self { tables })
    }

    pub(crate) fn verify(
        &self,
        db: &Connection,
        cancel: &AtomicBool,
        additions: &ControlAdditions,
    ) -> anyhow::Result<()> {
        check(cancel)?;
        writer(db)?;
        let mut budget = InventoryBudget::default();
        let (_, names) = schema_inventory(db, cancel, &mut budget)?;
        // Bounds are cumulative across the entire schema, every original table,
        // and the independently supplied additions being compared with it.
        let ledger = added_hashes(&additions.ledger, 7, cancel, &mut budget)?;
        let guards = added_hashes(&additions.guards, 4, cancel, &mut budget)?;
        ensure!(
            ledger.is_empty() || self.tables.contains_key("_sqlx_migrations"),
            "ledger additions require original ledger table"
        );
        ensure!(
            guards.is_empty() || self.tables.contains_key("catabolic_source_guards"),
            "guard additions require original guard table"
        );
        for (name, original) in &self.tables {
            ensure!(names.contains(name), "retained table disappeared: {name}");
            let observed = table_row_hashes(db, name, cancel, &mut budget)?;
            let appended = match name.as_str() {
                "_sqlx_migrations" => ledger.as_slice(),
                "catabolic_source_guards" => guards.as_slice(),
                _ => &[],
            };
            if appended.is_empty() {
                ensure!(observed == *original, "retained table rows changed: {name}");
            } else {
                let mut expected = original.clone();
                expected.extend_from_slice(appended);
                expected.sort_unstable();
                ensure!(
                    observed == expected,
                    "retained table rows or exact additions changed: {name}"
                );
            }
        }
        check(cancel)
    }
}

fn added_hashes(
    rows: &[Vec<Value>],
    columns: usize,
    cancel: &AtomicBool,
    budget: &mut InventoryBudget,
) -> anyhow::Result<Vec<String>> {
    let mut hashes = Vec::new();
    for row in rows {
        check(cancel)?;
        ensure!(
            row.len() == columns && matches!(row.first(), Some(Value::Integer(_))),
            "control addition must contain exact rowid and every column"
        );
        budget.row()?;
        let mut encoded = Vec::new();
        for value in row {
            budget.value(&mut encoded, ValueRef::from(value))?;
        }
        hashes.push(catabolic_core::sha256(&encoded));
    }
    Ok(hashes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retained_capacity_counts_spare_vectors_strings_blobs_and_map_nodes() {
        let mut text = String::with_capacity(4096);
        text.push('x');
        let mut blob = Vec::with_capacity(8192);
        blob.push(1);
        let mut row = Vec::with_capacity(16);
        row.push(Value::Text(text));
        row.push(Value::Blob(blob));
        let mut rows = Vec::with_capacity(8);
        rows.push(row);
        assert!(sql_rows_retained_allocations(&rows).unwrap() >=
            4096 + 8192 + 16 * std::mem::size_of::<Value>() as u64
                + 8 * std::mem::size_of::<Vec<Value>>() as u64);
        let mut hashes = Vec::with_capacity(10);
        hashes.push(String::with_capacity(2048));
        let mut tables = BTreeMap::new();
        tables.insert(String::with_capacity(256), hashes);
        let retained = RetainedRows { tables };
        assert!(retained.retained_allocations().unwrap() >=
            1024 + 256 + 2048 + 10 * std::mem::size_of::<String>() as u64);
    }

    fn fixture() -> Connection {
        let db = Connection::open_in_memory().unwrap();
        db.execute_batch(
            "CREATE TABLE _sqlx_migrations (
                version BIGINT PRIMARY KEY, description TEXT NOT NULL,
                installed_on TIMESTAMP NOT NULL, success BOOLEAN NOT NULL,
                checksum BLOB NOT NULL, execution_time BIGINT NOT NULL);
             CREATE TABLE catabolic_source_guards (
                trigger_name TEXT PRIMARY KEY, table_name TEXT NOT NULL, trigger_sql TEXT NOT NULL);
             CREATE TABLE retained(value);
             INSERT INTO _sqlx_migrations VALUES(93,'retained','2026-01-01',1,x'010203',7);
             INSERT INTO catabolic_source_guards VALUES('old','retained','exact old SQL');
             INSERT INTO retained VALUES(NULL),(3),(3.0),('3'),(x'33'),(CAST(x'ff00' AS TEXT));
             BEGIN IMMEDIATE;",
        )
        .unwrap();
        db
    }

    fn append(db: &Connection) -> ControlAdditions {
        db.execute_batch(
            "INSERT INTO _sqlx_migrations(rowid,version,description,installed_on,success,checksum,execution_time)
                VALUES(2,94,'reviewed','2026-02-02',1,x'040506',0);
             INSERT INTO catabolic_source_guards(rowid,trigger_name,table_name,trigger_sql)
                VALUES(2,'new','preview_sheet_artifacts','exact new SQL');",
        )
        .unwrap();
        ControlAdditions {
            ledger: vec![vec![
                Value::Integer(2),
                Value::Integer(94),
                Value::Text("reviewed".into()),
                Value::Text("2026-02-02".into()),
                Value::Integer(1),
                Value::Blob(vec![4, 5, 6]),
                Value::Integer(0),
            ]],
            guards: vec![vec![
                Value::Integer(2),
                Value::Text("new".into()),
                Value::Text("preview_sheet_artifacts".into()),
                Value::Text("exact new SQL".into()),
            ]],
        }
    }

    #[test]
    fn exact_append_preserves_all_old_control_and_application_rows() {
        let db = fixture();
        let cancel = AtomicBool::new(false);
        let before = RetainedRows::capture(&db, &cancel).unwrap();
        let additions = append(&db);
        before.verify(&db, &cancel, &additions).unwrap();
        assert!(
            before
                .verify(&db, &cancel, &ControlAdditions::default())
                .is_err()
        );
        assert!(!db.is_autocommit());
    }

    #[test]
    fn each_old_ledger_field_and_guard_row_is_retained_not_exempted() {
        for sql in [
            "UPDATE _sqlx_migrations SET version=92 WHERE version=93",
            "UPDATE _sqlx_migrations SET description='changed' WHERE version=93",
            "UPDATE _sqlx_migrations SET installed_on='changed' WHERE version=93",
            "UPDATE _sqlx_migrations SET success=0 WHERE version=93",
            "UPDATE _sqlx_migrations SET checksum=x'00' WHERE version=93",
            "UPDATE _sqlx_migrations SET execution_time=8 WHERE version=93",
            "UPDATE _sqlx_migrations SET rowid=3 WHERE version=93",
            "DELETE FROM _sqlx_migrations WHERE version=93",
            "UPDATE catabolic_source_guards SET trigger_sql='changed' WHERE trigger_name='old'",
            "DELETE FROM catabolic_source_guards WHERE trigger_name='old'",
        ] {
            let db = fixture();
            let cancel = AtomicBool::new(false);
            let before = RetainedRows::capture(&db, &cancel).unwrap();
            let additions = append(&db);
            db.execute_batch(sql).unwrap();
            assert!(before.verify(&db, &cancel, &additions).is_err(), "{sql}");
        }
    }

    #[test]
    fn unlisted_additions_replacements_rowids_and_storage_classes_fail() {
        for sql in [
            "INSERT INTO retained VALUES('unexpected')",
            "DELETE FROM retained WHERE rowid=1",
            "UPDATE retained SET rowid=100 WHERE rowid=1",
            "UPDATE retained SET value=x'33' WHERE rowid=4",
            "UPDATE retained SET value=3.0 WHERE rowid=2",
            "UPDATE retained SET value='null' WHERE rowid=1",
            "DROP TABLE retained",
            "INSERT INTO catabolic_source_guards VALUES('extra','retained','not approved')",
        ] {
            let db = fixture();
            let cancel = AtomicBool::new(false);
            let before = RetainedRows::capture(&db, &cancel).unwrap();
            let additions = append(&db);
            db.execute_batch(sql).unwrap();
            assert!(before.verify(&db, &cancel, &additions).is_err(), "{sql}");
        }
    }

    #[test]
    fn supplied_control_additions_must_match_every_typed_value_and_rowid() {
        let db = fixture();
        let cancel = AtomicBool::new(false);
        let before = RetainedRows::capture(&db, &cancel).unwrap();
        let mut additions = append(&db);
        additions.ledger[0][5] = Value::Text("040506".into());
        assert!(before.verify(&db, &cancel, &additions).is_err());
        additions.ledger[0][5] = Value::Blob(vec![4, 5, 6]);
        additions.guards[0][0] = Value::Integer(3);
        assert!(before.verify(&db, &cancel, &additions).is_err());
        additions.guards[0].remove(0);
        assert!(before.verify(&db, &cancel, &additions).is_err());
    }

    #[test]
    fn without_rowid_generated_columns_rowid_alias_and_sequence_are_preserved() {
        let db = fixture();
        db.execute_batch(
            "CREATE TABLE keyed(k TEXT PRIMARY KEY,v BLOB) WITHOUT ROWID;
             INSERT INTO keyed VALUES('k',x'00ff');
             CREATE TABLE aliased(rowid TEXT,v INTEGER,g INTEGER GENERATED ALWAYS AS(v+1) STORED);
             INSERT INTO aliased VALUES('shadow',7);
             CREATE TABLE counted(id INTEGER PRIMARY KEY AUTOINCREMENT,v);
             INSERT INTO counted(v) VALUES('retained');",
        )
        .unwrap();
        let cancel = AtomicBool::new(false);
        let before = RetainedRows::capture(&db, &cancel).unwrap();
        before
            .verify(&db, &cancel, &ControlAdditions::default())
            .unwrap();
        db.execute_batch("UPDATE sqlite_sequence SET seq=99 WHERE name='counted'")
            .unwrap();
        assert!(
            before
                .verify(&db, &cancel, &ControlAdditions::default())
                .is_err()
        );
    }

    #[test]
    fn shared_table_traversal_cannot_be_redirected_by_temp_shadow() {
        let db = fixture();
        let cancel = AtomicBool::new(false);
        let before = RetainedRows::capture(&db, &cancel).unwrap();
        db.execute_batch("CREATE TEMP TABLE retained(rowid,_rowid_,oid); UPDATE main.retained SET value=42 WHERE rowid=1").unwrap();
        assert!(
            before
                .verify(&db, &cancel, &ControlAdditions::default())
                .is_err()
        );
    }

    #[test]
    fn cumulative_row_and_byte_budgets_are_not_reset_per_table() {
        let db = Connection::open_in_memory().unwrap();
        db.execute_batch("CREATE TABLE a(v); CREATE TABLE b(v); INSERT INTO a VALUES(1); INSERT INTO b VALUES(2)").unwrap();
        let cancel = AtomicBool::new(false);
        let mut budget = InventoryBudget {
            used: 0,
            count: super::super::MAX_ROWS - 1,
        };
        table_row_hashes(&db, "a", &cancel, &mut budget).unwrap();
        assert!(table_row_hashes(&db, "b", &cancel, &mut budget).is_err());
        let mut budget = InventoryBudget {
            used: super::super::MAX_INVENTORY - 40,
            count: 0,
        };
        table_row_hashes(&db, "a", &cancel, &mut budget).unwrap();
        assert!(table_row_hashes(&db, "b", &cancel, &mut budget).is_err());
    }

    #[test]
    fn cancellation_and_missing_writer_refuse_without_releasing_custody() {
        let db = fixture();
        let before = RetainedRows::capture(&db, &AtomicBool::new(false)).unwrap();
        assert!(RetainedRows::capture(&db, &AtomicBool::new(true)).is_err());
        assert!(
            before
                .verify(&db, &AtomicBool::new(true), &ControlAdditions::default())
                .is_err()
        );
        assert!(!db.is_autocommit());
        db.execute_batch("ROLLBACK").unwrap();
        assert!(RetainedRows::capture(&db, &AtomicBool::new(false)).is_err());
        assert!(
            before
                .verify(&db, &AtomicBool::new(false), &ControlAdditions::default())
                .is_err()
        );
    }
}
