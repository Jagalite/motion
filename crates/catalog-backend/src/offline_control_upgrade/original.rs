//! Original93 preservation from consumed, independently inspected image bytes.
//! Descriptive evidence only: no path, Connection, migration or admission grant.
use super::*;
use crate::motion_snapshot::image_read::{self, InspectedImage};
use std::io::{Cursor, Read};

const MAX_IMAGE: u64 = 56 * 1024 * 1024;
const MAX_ROWS: i64 = 10_000;
const MAX_CONTROL_BYTES: i64 = 8 * 1024 * 1024;

pub(super) struct Original93Preservation {
    rows: RetainedRows,
    image_sha256: String,
    image_bytes: u64,
    inventory_sha256: String,
    ledger_sha256: String,
    full_binding_sha256: String,
    frozen_binding_sha256: String,
    ledger: Vec<Vec<Value>>,
    guards: Vec<Vec<Value>>,
}
impl Original93Preservation {
    /// Describes these exact retained owners without rereading either image.
    pub(super) fn retained_allocations(&self) -> anyhow::Result<u64> {
        use crate::motion_snapshot::row_preservation::sql_rows_retained_allocations;
        let mut total = (std::mem::size_of::<Self>() as u64)
            .checked_add(self.rows.retained_allocations()?)
            .and_then(|n| n.checked_add(sql_rows_retained_allocations(&self.ledger).ok()?))
            .and_then(|n| n.checked_add(sql_rows_retained_allocations(&self.guards).ok()?))
            .context("original preservation retained capacity overflow")?;
        for text in [&self.image_sha256, &self.inventory_sha256, &self.ledger_sha256,
            &self.full_binding_sha256, &self.frozen_binding_sha256] {
            total = total.checked_add(u64::try_from(text.capacity())?)
                .context("original preservation digest capacity overflow")?;
        }
        Ok(total)
    }
    pub(super) fn verify_rows(
        &self,
        db: &Connection,
        cancel: &AtomicBool,
        additions: &ControlAdditions,
    ) -> anyhow::Result<()> {
        self.rows.verify(db, cancel, additions)
    }
    pub(super) fn image_sha256(&self) -> &str {
        &self.image_sha256
    }
    pub(super) fn image_bytes(&self) -> u64 {
        self.image_bytes
    }
    pub(super) fn inventory_sha256(&self) -> &str {
        &self.inventory_sha256
    }
    pub(super) fn ledger_sha256(&self) -> &str {
        &self.ledger_sha256
    }
    pub(super) fn full_binding_sha256(&self) -> &str {
        &self.full_binding_sha256
    }
    pub(super) fn frozen_binding_sha256(&self) -> &str {
        &self.frozen_binding_sha256
    }
    pub(super) fn ledger(&self) -> &[Vec<Value>] {
        &self.ledger
    }
    pub(super) fn guards(&self) -> &[Vec<Value>] {
        &self.guards
    }
}

/// The caller retains selected-pair custody and validates its manifest against
/// `inspected`. Neither the supplied reader nor the descriptive inspection alone
/// suffices: all bytes are consumed once, hashed, and independently reinspected.
pub(super) fn capture<R: Read>(
    reference: &UpgradeReference,
    mut reader: R,
    inspected: &InspectedImage,
    retained_pair_bytes: u64,
    cancel: &AtomicBool,
) -> anyhow::Result<Original93Preservation> {
    check(cancel)?;
    let size = inspected.bytes();
    ensure!(
        size > 0 && size <= MAX_IMAGE && inspected.schema_version() == 93,
        "original preservation requires bounded inspected93 image"
    );
    // Source Vec, SQLite owned image and serialized comparison can coexist.
    // The parent's 64MiB reserve covers bounded row hashes, control values,
    // schema/selector temporaries and descriptive facts; this is not a hard RSS cap.
    let charge = size
        .checked_mul(3)
        .context("original image accounting overflow")?;
    admit_memory(None, retained_pair_bytes, charge)?;
    let mut bytes = vec![0; usize::try_from(size)?];
    for chunk in bytes.chunks_mut(64 * 1024) {
        check(cancel)?;
        reader.read_exact(chunk)?;
    }
    let mut extra = [0u8; 1];
    ensure!(
        reader.read(&mut extra)? == 0,
        "original image has trailing bytes"
    );
    check(cancel)?;
    ensure!(
        catabolic_core::sha256(&bytes) == inspected.sha256(),
        "original reader differs from independently inspected image"
    );
    let inspection_reserved = retained_pair_bytes
        .checked_add(size)
        .context("original reinspection accounting overflow")?;
    let actual = image_read::inspect(
        Cursor::new(bytes.as_slice()),
        size,
        inspection_reserved,
        cancel,
    )?;
    ensure!(
        actual.sha256() == inspected.sha256()
            && actual.bytes() == size
            && actual.schema_version() == 93
            && actual.inventory_sha256() == inspected.inventory_sha256()
            && actual.migration_ledger_sha256() == inspected.migration_ledger_sha256()
            && actual.binding().evidence_sha256() == inspected.binding().evidence_sha256()
            && actual.binding().effective_identity() == inspected.binding().effective_identity(),
        "original93 reinspection commitments differ"
    );
    let mut db = Connection::open_in_memory()?;
    db.pragma_update(None, "cache_size", -2048)?;
    admit_memory(Some(&db), retained_pair_bytes, charge)?;
    // Mutable isolated image permits the existing retained-WRITE row scanner.
    // No SQL writes occur; this connection is never returned or used for effects.
    db.deserialize_read_exact(
        "main",
        Cursor::new(bytes.as_slice()),
        usize::try_from(size)?,
        false,
    )?;
    db.pragma_update(None, "foreign_keys", true)?;
    db.pragma_update(None, "recursive_triggers", true)?;
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
    db.progress_handler(10_000, Some(move || std::time::Instant::now() >= deadline))?;
    db.execute_batch("BEGIN IMMEDIATE")?;
    ensure!(
        reference.0.verify_in(&db)? == ControlVersion::Retained93,
        "original image is not exact closed93"
    );
    crate::source_freeze::verify_sync(&db)?;
    integrity(&db)?;
    let frozen_size: i64 = db.query_row(
        "SELECT length(CAST(identity_json AS BLOB)) FROM catabolic_binding WHERE id=1",
        [],
        |r| r.get(0),
    )?;
    ensure!(
        (1..=65536).contains(&frozen_size),
        "original binding exceeds bound"
    );
    let frozen: String = db.query_row(
        "SELECT identity_json FROM catabolic_binding WHERE id=1",
        [],
        |r| r.get(0),
    )?;
    let frozen_binding_sha256 = catabolic_core::sha256(frozen.as_bytes());
    verify_binding(&db, &frozen_binding_sha256)?;
    admit_memory(Some(&db), retained_pair_bytes, charge)?;
    let rows = RetainedRows::capture(&db, cancel)?;
    let ledger = control_rows(&db, true, cancel)?;
    let guards = control_rows(&db, false, cancel)?;
    admit_memory(Some(&db), retained_pair_bytes, charge)?;
    check(cancel)?;
    let serialized = db.serialize("main")?;
    ensure!(
        serialized.len() as u64 == size
            && catabolic_core::sha256(&*serialized) == inspected.sha256(),
        "original93 inspection changed source image"
    );
    drop(serialized);
    db.execute_batch("ROLLBACK")?;
    db.progress_handler(0, None::<fn() -> bool>)?;
    check(cancel)?;
    Ok(Original93Preservation {
        rows,
        image_sha256: actual.sha256().into(),
        image_bytes: size,
        inventory_sha256: actual.inventory_sha256().into(),
        ledger_sha256: actual.migration_ledger_sha256().into(),
        full_binding_sha256: actual.binding().evidence_sha256().into(),
        frozen_binding_sha256,
        ledger,
        guards,
    })
}

fn control_rows(
    db: &Connection,
    ledger: bool,
    cancel: &AtomicBool,
) -> anyhow::Result<Vec<Vec<Value>>> {
    let (table, fields, columns) = if ledger {
        (
            "_sqlx_migrations",
            "rowid,version,description,installed_on,success,checksum,execution_time",
            7,
        )
    } else {
        (
            "catabolic_source_guards",
            "rowid,trigger_name,table_name,trigger_sql",
            4,
        )
    };
    let costs = fields
        .split(',')
        .map(|column| format!("coalesce(length(CAST({column} AS BLOB)),0)"))
        .collect::<Vec<_>>()
        .join("+");
    let (count, bytes): (i64, i64) = db.query_row(
        &format!("SELECT count(*),coalesce(sum({costs}),0) FROM {table}"),
        [],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    ensure!(
        (0..=MAX_ROWS).contains(&count) && (0..=MAX_CONTROL_BYTES).contains(&bytes),
        "original control rows exceed bound"
    );
    let mut statement = db.prepare(&format!("SELECT {fields} FROM {table} ORDER BY rowid"))?;
    let mut query = statement.query([])?;
    let mut output = Vec::with_capacity(usize::try_from(count)?);
    while let Some(row) = query.next()? {
        check(cancel)?;
        // Value rejects invalid UTF8 rather than normalizing original bytes.
        output.push(
            (0..columns)
                .map(|index| row.get(index))
                .collect::<rusqlite::Result<Vec<Value>>>()?,
        );
    }
    ensure!(
        output.len() == usize::try_from(count)?,
        "original row inventory changed"
    );
    Ok(output)
}

#[cfg(test)]
pub(super) mod tests {
    use super::*;
    use std::sync::Arc;

    pub(in crate::offline_control_upgrade) async fn original()
    -> (tempfile::TempDir, Vec<u8>, InspectedImage) {
        let (root, path, _) = super::super::tests::sealed().await;
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .after_connect(|db, _| Box::pin(crate::projection::configure_connection(db)))
            .connect_with(
                sqlx::sqlite::SqliteConnectOptions::new()
                    .filename(path)
                    .foreign_keys(true),
            )
            .await
            .unwrap();
        let cancel = Arc::new(AtomicBool::new(false));
        let (writer, image) = crate::motion_snapshot::capture(&pool, cancel.clone())
            .await
            .unwrap();
        let mut bytes = Vec::new();
        image.copy_verified_to(&mut bytes, &cancel).unwrap();
        writer.rollback().await.unwrap();
        pool.close().await;
        let facts =
            image_read::inspect(Cursor::new(&bytes), bytes.len() as u64, 0, &cancel).unwrap();
        (root, bytes, facts)
    }

    #[tokio::test]
    async fn genuine_original93_retains_exact_rows_and_distinct_binding_commitments() {
        let (_root, bytes, facts) = original().await;
        let reference = UpgradeReference::build().await.unwrap();
        let flag = AtomicBool::new(false);
        let saved = capture(&reference, Cursor::new(&bytes), &facts, 0, &flag).unwrap();
        assert_eq!(saved.image_sha256(), facts.sha256());
        assert_eq!(saved.image_bytes(), bytes.len() as u64);
        assert_eq!(saved.inventory_sha256(), facts.inventory_sha256());
        assert_eq!(saved.ledger_sha256(), facts.migration_ledger_sha256());
        assert_eq!(
            saved.full_binding_sha256(),
            facts.binding().evidence_sha256()
        );
        assert_ne!(saved.frozen_binding_sha256(), saved.full_binding_sha256());
        assert!(!saved.ledger().is_empty());
        assert!(!saved.guards().is_empty());
        let mut db = Connection::open_in_memory().unwrap();
        db.deserialize_read_exact("main", Cursor::new(&bytes), bytes.len(), false)
            .unwrap();
        db.execute_batch("BEGIN IMMEDIATE").unwrap();
        saved
            .verify_rows(&db, &flag, &ControlAdditions::default())
            .unwrap();
        // Test-only corrupt image: retain default-deny connection admission.
        // Suppress exactly this row's UPDATE guard and AFTER event, restoring
        // byte-identical SQL before preservation checks. No event may leak.
        set_ledger(&db, false).unwrap();
        assert!(
            db.execute("UPDATE profiles SET name='changed' WHERE id='retained'", [])
                .is_err()
        );
        let names = [
            "motion_source_freeze_update_profiles",
            "event_profiles_update",
        ];
        let triggers: Vec<String> = names
            .iter()
            .map(|name| {
                db.query_row(
                    "SELECT sql FROM sqlite_schema WHERE type='trigger' AND name=?1",
                    [name],
                    |row| row.get(0),
                )
                .unwrap()
            })
            .collect();
        let events: i64 = db
            .query_row("SELECT count(*) FROM change_events", [], |row| row.get(0))
            .unwrap();
        db.execute_batch(
            "DROP TRIGGER motion_source_freeze_update_profiles; DROP TRIGGER event_profiles_update",
        )
        .unwrap();
        assert_eq!(
            db.execute("UPDATE profiles SET name='changed' WHERE id='retained'", [])
                .unwrap(),
            1
        );
        for trigger in &triggers {
            db.execute_batch(trigger).unwrap();
        }
        for (name, expected) in names.iter().zip(&triggers) {
            let actual: String = db
                .query_row(
                    "SELECT sql FROM sqlite_schema WHERE name=?1",
                    [name],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!(&actual, expected);
        }
        assert_eq!(
            db.query_row("SELECT count(*) FROM change_events", [], |row| row
                .get::<_, i64>(0))
                .unwrap(),
            events
        );
        crate::source_freeze::verify_sync(&db).unwrap();
        assert!(
            saved
                .verify_rows(&db, &flag, &ControlAdditions::default())
                .is_err()
        );
        db.execute_batch("ROLLBACK").unwrap();
    }

    #[tokio::test]
    async fn foreign_inspection_truncation_and_extra_bytes_cannot_mint_original_evidence() {
        let (_root, bytes, facts) = original().await;
        let (_foreign_root, foreign, foreign_facts) = original().await;
        let reference = UpgradeReference::build().await.unwrap();
        let flag = AtomicBool::new(false);
        assert_ne!(facts.sha256(), foreign_facts.sha256());
        assert!(capture(&reference, Cursor::new(&foreign), &facts, 0, &flag).is_err());
        assert!(capture(&reference, Cursor::new(&bytes), &foreign_facts, 0, &flag).is_err());
        assert!(
            capture(
                &reference,
                Cursor::new(&bytes[..bytes.len() - 1]),
                &facts,
                0,
                &flag
            )
            .is_err()
        );
        let mut extra = bytes.clone();
        extra.push(0);
        assert!(capture(&reference, Cursor::new(extra), &facts, 0, &flag).is_err());
    }

    #[tokio::test]
    async fn original_capture_uses_held_bytes_not_replaced_path() {
        let (root, bytes, facts) = original().await;
        let reference = UpgradeReference::build().await.unwrap();
        let path = root.path().join("selected-motion-image");
        std::fs::write(&path, &bytes).unwrap();
        let held = File::open(&path).unwrap();
        std::fs::rename(&path, root.path().join("old-selected-motion-image")).unwrap();
        std::fs::write(&path, b"replacement").unwrap();
        let flag = AtomicBool::new(false);
        assert!(capture(&reference, File::open(&path).unwrap(), &facts, 0, &flag).is_err());
        let original = capture(&reference, held, &facts, 0, &flag).unwrap();
        assert_eq!(original.image_sha256(), facts.sha256());
        assert_eq!(std::fs::read(path).unwrap(), b"replacement");
        // Pair pathname refusal belongs to SelectedPairInput::revalidate. This
        // descriptor-only evidence owner neither follows nor blesses that path.
    }

    #[tokio::test]
    async fn original_capture_cancel_and_cumulative_budget_refuse_without_effects() {
        let (_root, bytes, facts) = original().await;
        let reference = UpgradeReference::build().await.unwrap();
        assert!(
            capture(
                &reference,
                Cursor::new(&bytes),
                &facts,
                0,
                &AtomicBool::new(true)
            )
            .is_err()
        );
        assert!(
            capture(
                &reference,
                Cursor::new(&bytes),
                &facts,
                u64::MAX,
                &AtomicBool::new(false)
            )
            .is_err()
        );
        struct CancelDuringRead<'a> {
            source: Cursor<&'a [u8]>,
            flag: &'a AtomicBool,
        }
        impl Read for CancelDuringRead<'_> {
            fn read(&mut self, out: &mut [u8]) -> std::io::Result<usize> {
                let count = self.source.read(out)?;
                self.flag.store(true, Ordering::Release);
                Ok(count)
            }
        }
        let during = AtomicBool::new(false);
        assert!(
            capture(
                &reference,
                CancelDuringRead {
                    source: Cursor::new(bytes.as_slice()),
                    flag: &during,
                },
                &facts,
                0,
                &during
            )
            .is_err()
        );
        assert_eq!(catabolic_core::sha256(&bytes), facts.sha256());
        capture(
            &reference,
            Cursor::new(&bytes),
            &facts,
            0,
            &AtomicBool::new(false),
        )
        .unwrap();
    }
}
