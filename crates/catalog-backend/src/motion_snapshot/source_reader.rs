//! Read-only current-source authorization under an exclusively borrowed writer.
//! Trusted host consumers use only existing Caller reads. They must not replace
//! the returned connection or alter its raw hooks. This is not an adversarial
//! in-process sandbox, an arbitrary writer, or target historical authorization.
use super::{FreshWriter, binding};
mod legacy;
mod retained_close;
pub use legacy::LegacyAuthorizationReader;
mod restricted_owner;
use anyhow::{Context, ensure};
pub(crate) use restricted_owner::RestrictedReaderOwner;
#[cfg(test)]
pub(crate) use restricted_owner::SetupFault;
pub use retained_close::RetainedAuthorizationCloseSlot;
use rusqlite::ffi;
use sqlx::{Connection as _, SqliteConnection, sqlite::SqliteConnectOptions};
use std::{
    ffi::{CStr, c_char, c_int, c_void},
    path::PathBuf,
};

/// Group the exact new inline source fields so accounting includes their
/// actual padding/alignment, not an additive field-size approximation.
#[derive(Default)]
pub(super) struct SourceAuthorizationCustody {
    pub(super) reader: RestrictedReaderOwner,
    pub(super) claim: Option<std::sync::Arc<()>>,
    pub(super) transferred: bool,
    pub(super) settled: bool,
}

// Five bounded pathname payloads conservatively cover both acquisition and
// verification. Acquisition owns the slot path and connect-options filename,
// while SQLx builds a String then moves it into its worker's CString. Verification
// can retain the slot path, that worker CString, an expected-path clone and both
// main-path expression temporaries. Include the CString's trailing NUL. This is
// not an allocator-capacity/RSS claim or a bound on all opaque SQLx internals.
const fn source_path_storage_reservation(path_bytes: u64) -> Option<u64> {
    match path_bytes.checked_mul(5) {
        Some(bytes) => bytes.checked_add(1),
        None => None,
    }
}

/// A borrowing authorization token, never an owner of the reader connection,
/// its FD, or its opening/closing future. Abandonment retains them in FreshWriter.
pub struct RetainedAuthorizationReader<'writer> {
    writer: &'writer mut FreshWriter,
}

// No context pointer or borrowed callback state. Only SELECT/read/function
// opcodes are admitted. The separately opened database is also OS/SQLite readonly.
unsafe extern "C" fn authorize(
    _: *mut c_void,
    action: c_int,
    _: *const c_char,
    _: *const c_char,
    _: *const c_char,
    _: *const c_char,
) -> c_int {
    match action {
        ffi::SQLITE_SELECT | ffi::SQLITE_READ | ffi::SQLITE_FUNCTION => ffi::SQLITE_OK,
        _ => ffi::SQLITE_DENY,
    }
}
async fn main_path(db: &mut SqliteConnection, write: bool) -> anyhow::Result<PathBuf> {
    let mut lease = db.lock_handle().await?;
    let handle = lease.as_raw_handle().as_ptr();
    // SAFETY: lexical inspection under the exclusive SQLx lease.
    unsafe { checked_main(handle, write) }
}
/// Internal shared readonly-reader mechanics, not an authorization capability.
pub(crate) async fn verify_reader_main(db: &mut SqliteConnection) -> anyhow::Result<PathBuf> {
    main_path(db, false).await
}
pub(crate) fn verify_rusqlite_writer_main(db: &rusqlite::Connection) -> anyhow::Result<PathBuf> {
    // SAFETY: only lexical inspection of this retained connection; no SQL,
    // reentry, handle transfer, or hook mutation.
    unsafe { checked_main(db.handle(), true) }
}
async fn bounded_main_path(db: &mut SqliteConnection, write: bool) -> anyhow::Result<PathBuf> {
    let mut lease = db.lock_handle().await?;
    // SAFETY: the same exclusive, lexical SQLx handle inspection.
    unsafe {
        checked_main_inner(
            lease.as_raw_handle().as_ptr(),
            write,
            Some(restricted_owner::MAX_PATH_BYTES),
        )
    }
}
unsafe fn checked_main(handle: *mut ffi::sqlite3, write: bool) -> anyhow::Result<PathBuf> {
    // SAFETY: forwarded unchanged lexical handle contract.
    unsafe { checked_main_inner(handle, write, None) }
}
unsafe fn checked_main_inner(
    handle: *mut ffi::sqlite3,
    write: bool,
    path_limit: Option<usize>,
) -> anyhow::Result<PathBuf> {
    // SAFETY: all handle introspection is lexical, without reentry/await, under
    // the exclusive SQLx lease. No handle or pointer escapes that lease.
    unsafe {
        if !write {
            let mut bytes = 0u64;
            for op in [
                ffi::SQLITE_DBSTATUS_CACHE_USED,
                ffi::SQLITE_DBSTATUS_SCHEMA_USED,
                ffi::SQLITE_DBSTATUS_STMT_USED,
            ] {
                let (mut current, mut high) = (0, 0);
                ensure!(
                    ffi::sqlite3_db_status(handle, op, &mut current, &mut high, 0)
                        == ffi::SQLITE_OK
                        && current >= 0,
                    "authorization allocation observation unavailable"
                );
                bytes = bytes
                    .checked_add(u64::try_from(current)?)
                    .context("authorization allocation overflow")?;
            }
            verify_reader_allocation(bytes)?;
        }
        ensure!(
            ffi::sqlite3_get_autocommit(handle) == 0
                && ffi::sqlite3_txn_state(handle, c"main".as_ptr()) == if write { 2 } else { 1 },
            "retained authorization transaction ended or changed"
        );
        ensure!(
            ffi::sqlite3_db_readonly(handle, c"main".as_ptr()) == if write { 0 } else { 1 },
            "retained authorization connection mode differs"
        );
        let mut moved: c_int = 0;
        ensure!(
            ffi::sqlite3_file_control(
                handle,
                c"main".as_ptr(),
                ffi::SQLITE_FCNTL_HAS_MOVED,
                (&mut moved as *mut c_int).cast()
            ) == ffi::SQLITE_OK
                && moved == 0,
            "retained authorization database moved"
        );
        let name = ffi::sqlite3_db_filename(handle, c"main".as_ptr());
        ensure!(!name.is_null(), "retained authorization main file missing");
        let name = CStr::from_ptr(name);
        ensure!(
            path_limit.is_none_or(|limit| name.to_bytes().len() <= limit),
            "retained authorization pathname exceeds supported bounded cohort"
        );
        let name = name.to_str()?;
        ensure!(
            !name.is_empty(),
            "retained authorization needs actual file-backed source"
        );
        Ok(PathBuf::from(name))
    }
}
fn verify_reader_allocation(bytes: u64) -> anyhow::Result<()> {
    ensure!(
        bytes <= 16 * 1024 * 1024,
        "authorization reader logical allocation cohort exceeded"
    );
    Ok(())
}
/// Opens a fresh read-only snapshot with a static default-deny authorizer.
/// Only fixed borrowed authorization owners consume this internal connection.
pub(crate) async fn open_restricted_reader(
    path: &std::path::Path,
) -> anyhow::Result<SqliteConnection> {
    let mut reader = SqliteConnection::connect_with(
        &SqliteConnectOptions::new()
            .filename(path)
            .read_only(true)
            .create_if_missing(false)
            .pragma("query_only", "ON")
            .pragma("cache_size", "-2048"),
    )
    .await?;
    sqlx::query("BEGIN").execute(&mut reader).await?;
    let _: i64 = sqlx::query_scalar("SELECT count(*) FROM sqlite_schema")
        .fetch_one(&mut reader)
        .await?;
    {
        let mut lease = reader.lock_handle().await?;
        // SAFETY: static callback, no captured references, connection held
        // exclusively; no source baseline authorizer is changed.
        ensure!(
            unsafe {
                ffi::sqlite3_set_authorizer(
                    lease.as_raw_handle().as_ptr(),
                    Some(authorize),
                    std::ptr::null_mut(),
                )
            } == ffi::SQLITE_OK,
            "retained authorization authorizer installation failed"
        );
    }
    verify_reader_main(&mut reader).await?;
    Ok(reader)
}
impl FreshWriter {
    /// The call itself claims the acquisition before returning its future. An
    /// unpolled/dropped future cannot make the original owner reopen a reader.
    /// The original writer, reader and opening future remain on this owner.
    pub fn authorization_reader(
        &mut self,
    ) -> impl std::future::Future<Output = anyhow::Result<RetainedAuthorizationReader<'_>>> + '_
    {
        let claimed = self.claim_authorization_reader();
        async move {
            claimed?;
            let path = bounded_main_path(&mut self.transaction, true).await?;
            self.authorization.reader.install_path(path)?;
            self.authorization.reader.acquire().await?;
            let mut token = RetainedAuthorizationReader { writer: self };
            token.verify().await?;
            Ok(token)
        }
    }

    fn claim_authorization_reader(&mut self) -> anyhow::Result<()> {
        ensure!(
            self.authorization.claim.is_none() && !self.authorization.transferred,
            "retained source authorization acquisition is already claimed"
        );
        self.authorization.reader.claim()?;
        // This private identity is never caller-supplied or an accounting proof.
        // Its allocation must be charged before entering this source seam.
        self.authorization.claim = Some(std::sync::Arc::new(()));
        Ok(())
    }

    /// Additional source custody cost inside the existing inherited workspace,
    /// not an independently admitted cohort. The Arc share is conservative.
    pub const fn authorization_storage_reservation() -> u64 {
        (std::mem::size_of::<SourceAuthorizationCustody>() as u64)
            .checked_add(RestrictedReaderOwner::opening_frame_reservation())
            .expect("source opening storage overflow")
            .checked_add(restricted_owner::MAX_CLOSE_FRAME as u64)
            .expect("source close storage overflow")
            .checked_add(retained_close::SOURCE_IDENTITY_RESERVE)
            .expect("source identity storage overflow")
            .checked_add(
                source_path_storage_reservation(restricted_owner::MAX_PATH_BYTES as u64)
                    .expect("source pathname storage overflow"),
            )
            .expect("source custody storage overflow")
    }
}
impl RetainedAuthorizationReader<'_> {
    /// Descriptive current SQLite allocation counters under this exact borrowed
    /// writer and readonly reader. Does not release custody or grant SQL access.
    /// A composer's conservative auth reserve may deliberately double-count the
    /// reader; caller-owned native/stage/plan allocations remain separate.
    pub async fn retained_allocations(&mut self) -> anyhow::Result<u64> {
        self.verify().await?;
        async fn observed(db: &mut SqliteConnection) -> anyhow::Result<u64> {
            let mut lease = db.lock_handle().await?;
            // SAFETY: nonowning wrapper stays lexical under exclusive SQLx
            // lease, with no await/reentry/thread escape after construction.
            let connection =
                unsafe { rusqlite::Connection::from_handle(lease.as_raw_handle().as_ptr()) }?;
            super::connection_allocations(&[&connection])
        }
        let source = observed(&mut self.writer.transaction).await?;
        let reader = observed(self.writer.authorization.reader.reader_mut()?).await?;
        verify_reader_allocation(reader)?;
        source
            .checked_add(reader)
            .context("authorization retained allocation overflow")
    }
    pub async fn authorize_connection(&mut self) -> anyhow::Result<&mut SqliteConnection> {
        self.verify().await?;
        self.writer.authorization.reader.reader_mut()
    }
    pub async fn verify(&mut self) -> anyhow::Result<()> {
        // Refuse before inspecting even the original writer. A closed token is
        // not generic authentication proof, regardless of close acknowledgment.
        let source = &mut self.writer.authorization.reader;
        let path = source.path()?.to_path_buf();
        source.verify_identity()?;
        let reader = source.reader_mut()?;
        ensure!(
            bounded_main_path(&mut self.writer.transaction, true).await? == path
                && bounded_main_path(reader, false).await? == path,
            "retained authorization foreign main file"
        );
        let current = binding::validate(&mut self.writer.transaction)
            .await?
            .context("retained authorization requires native source binding")?;
        ensure!(
            Some(&current) == self.writer.binding.as_ref(),
            "retained authorization writer binding changed"
        );
        ensure!(
            binding::validate(reader).await?.as_ref() == Some(&current),
            "retained authorization reader binding differs"
        );
        Ok(())
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicBool;
    #[test]
    fn source_path_storage_has_exact_cohort_nul_and_checked_arithmetic() {
        assert_eq!(restricted_owner::MAX_PATH_BYTES, 4096);
        assert_eq!(source_path_storage_reservation(0), Some(1));
        assert_eq!(source_path_storage_reservation(4096), Some(20_481));
        // Check multiplication and the otherwise-successful product's final
        // NUL addition independently. u64::MAX is exactly divisible by five.
        assert_eq!(source_path_storage_reservation(u64::MAX), None);
        assert_eq!(source_path_storage_reservation(u64::MAX / 5), None);
        assert_eq!(
            FreshWriter::authorization_storage_reservation(),
            std::mem::size_of::<SourceAuthorizationCustody>() as u64
                + RestrictedReaderOwner::opening_frame_reservation()
                + restricted_owner::MAX_CLOSE_FRAME as u64
                + retained_close::SOURCE_IDENTITY_RESERVE
                + 20_481
        );
    }
    #[test]
    fn source_path_limit_precedes_install_and_keeps_exact_boundary() {
        let mut boundary = RestrictedReaderOwner::default();
        boundary.claim().unwrap();
        // OS filename-length failure is immaterial: exactly 4096 bytes passes
        // the logical cohort gate and is retained before attempting File::open.
        let _ = boundary.install_path(PathBuf::from("x".repeat(4096)));
        assert_eq!(boundary.path().unwrap().as_os_str().len(), 4096);

        let mut excessive = RestrictedReaderOwner::default();
        excessive.claim().unwrap();
        assert!(
            excessive
                .install_path(PathBuf::from("x".repeat(4097)))
                .is_err()
        );
        assert!(excessive.path().is_err());
        assert!(!excessive.has_file());
        assert!(!excessive.retains_acquisition());
        excessive.begin_close().unwrap();
        assert!(excessive.is_settled());
        assert!(!excessive.close_joined());
    }
    #[test]
    fn observed_reader_allocation_limit_refuses_overflow_without_relaxation() {
        assert!(verify_reader_allocation(16 * 1024 * 1024).is_ok());
        assert!(verify_reader_allocation(16 * 1024 * 1024 + 1).is_err());
        assert!(verify_reader_allocation(u64::MAX).is_err());
    }
    pub(super) async fn fixture() -> (tempfile::TempDir, sqlx::SqlitePool, FreshWriter) {
        let root = tempfile::tempdir().unwrap();
        let native = root.path().join("native.sqlite3");
        crate::initialize(native.clone()).await.unwrap();
        let identity = crate::inspect(native.clone()).await.unwrap().identity;
        let pool = crate::test_journal(&root.path().join("motion.sqlite3"))
            .await
            .unwrap();
        let backend = crate::NativeCatalog::bind(pool.clone(), native, identity)
            .await
            .unwrap();
        drop(backend);
        let writer = FreshWriter::begin(&pool, &AtomicBool::new(false))
            .await
            .unwrap();
        (root, pool, writer)
    }
    #[tokio::test]
    async fn actual_reader_denies_mutation_transaction_pragma_and_attachment() {
        let (_root, pool, mut writer) = fixture().await;
        let mut reader = writer.authorization_reader().await.unwrap();
        for sql in [
            "CREATE TABLE forbidden(value)",
            "DELETE FROM profiles",
            "COMMIT",
            "ROLLBACK",
            "SAVEPOINT escaped",
            "PRAGMA query_only=OFF",
            "ATTACH ':memory:' AS foreign_db",
        ] {
            assert!(
                sqlx::query(sql)
                    .execute(reader.authorize_connection().await.unwrap())
                    .await
                    .is_err(),
                "{sql}"
            );
            reader.verify().await.unwrap();
        }
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT count(*) FROM catabolic_binding")
                .fetch_one(reader.authorize_connection().await.unwrap())
                .await
                .unwrap(),
            1
        );
        drop(reader);
        drop(writer);
        pool.close().await;
    }
    #[tokio::test]
    async fn retained_writer_end_and_foreign_reader_are_not_authentication_proof() {
        let (_a, pa, mut wa) = fixture().await;
        let (_b, pb, mut wb) = fixture().await;
        let mut a = wa.authorization_reader().await.unwrap();
        let mut b = wb.authorization_reader().await.unwrap();
        std::mem::swap(
            &mut a.writer.authorization.reader,
            &mut b.writer.authorization.reader,
        );
        assert!(a.verify().await.is_err());
        std::mem::swap(
            &mut a.writer.authorization.reader,
            &mut b.writer.authorization.reader,
        );
        a.verify().await.unwrap();
        assert!(a.retained_allocations().await.unwrap() > 0);
        sqlx::query("ROLLBACK")
            .execute(&mut *a.writer.transaction)
            .await
            .unwrap();
        assert!(a.authorize_connection().await.is_err());
        assert!(a.retained_allocations().await.is_err());
        drop(a);
        drop(b);
        drop(wa);
        drop(wb);
        pa.close().await;
        pb.close().await;
    }
    #[tokio::test]
    async fn persistent_source_replacement_refuses_without_adopting_new_database() {
        let (root, pool, mut writer) = fixture().await;
        let mut token = writer.authorization_reader().await.unwrap();
        let original = token
            .writer
            .authorization
            .reader
            .path()
            .unwrap()
            .to_path_buf();
        std::fs::rename(&original, root.path().join("original.sqlite3")).unwrap();
        std::fs::write(&original, b"not the retained source").unwrap();
        assert!(token.verify().await.is_err());
        assert_eq!(
            std::fs::read(&original).unwrap(),
            b"not the retained source"
        );
        drop(token);
        drop(writer);
        pool.close().await;
    }
    #[tokio::test]
    async fn read_capability_keeps_the_original_writer_reserved() {
        let (_root, pool, mut writer) = fixture().await;
        let token = writer.authorization_reader().await.unwrap();
        assert!(
            tokio::time::timeout(
                std::time::Duration::from_millis(30),
                pool.begin_with("BEGIN IMMEDIATE")
            )
            .await
            .is_err()
        );
        drop(token);
        assert!(
            tokio::time::timeout(
                std::time::Duration::from_millis(30),
                pool.begin_with("BEGIN IMMEDIATE")
            )
            .await
            .is_err()
        );
        drop(writer);
        let tx = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            pool.begin_with("BEGIN IMMEDIATE"),
        )
        .await
        .unwrap()
        .unwrap();
        tx.rollback().await.unwrap();
        pool.close().await;
    }
}
