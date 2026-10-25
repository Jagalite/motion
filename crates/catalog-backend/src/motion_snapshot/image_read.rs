//! Bounded independent inspection of extracted Motion bytes. No live binding,
//! restored deployment, SQL writer or startup admission authority is returned.
use super::*;
use std::io::Read;

pub struct InspectedImage {
    bytes: u64,
    sha256: String,
    schema_version: u32,
    inventory: InventoryAudit,
    binding: VerifiedBinding,
}
impl InspectedImage {
    pub fn bytes(&self) -> u64 {
        self.bytes
    }
    pub fn sha256(&self) -> &str {
        &self.sha256
    }
    pub fn schema_version(&self) -> u32 {
        self.schema_version
    }
    pub fn inventory_sha256(&self) -> &str {
        &self.inventory.complete
    }
    pub fn migration_ledger_sha256(&self) -> &str {
        self.inventory.ledger.as_deref().expect("verified ledger")
    }
    pub fn binding(&self) -> &VerifiedBinding {
        &self.binding
    }
}
pub fn inspect<R: Read>(
    input: R,
    bytes: u64,
    reserved: u64,
    cancel: &AtomicBool,
) -> anyhow::Result<InspectedImage> {
    check(cancel)?;
    ensure!(
        bytes > 0 && bytes <= MAX_IMAGE,
        "Motion inspection image cohort exceeded"
    );
    let mut db = Connection::open_in_memory()?;
    db.pragma_update(None, "cache_size", -2048)?;
    allocation_reserved(&[&db], bytes, reserved)?;
    db.deserialize_read_exact("main", input, usize::try_from(bytes)?, true)?;
    allocation_reserved(&[&db], bytes, reserved)?;
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    db.progress_handler(10000, Some(move || std::time::Instant::now() >= deadline))?;
    let integrity: String = db.query_row("PRAGMA integrity_check", [], |r| r.get(0))?;
    ensure!(integrity == "ok", "Motion inspected image integrity failed");
    ensure!(
        !db.prepare("PRAGMA foreign_key_check")?
            .query([])?
            .next()?
            .is_some(),
        "Motion inspected image foreign keys failed"
    );
    allocation_reserved(&[&db], bytes, reserved)?;
    let inventory = inventory(&db, cancel)?;
    ensure!(
        inventory.ledger.is_some(),
        "Motion inspected migration ledger missing"
    );
    allocation_reserved(&[&db], bytes, reserved)?;
    let schema_version: u32 = db.query_row(
        "SELECT max(version) FROM _sqlx_migrations WHERE success=1",
        [],
        |r| r.get(0),
    )?;
    let binding = binding::inspect(&db)?;
    allocation_reserved(&[&db], bytes, reserved)?;
    let image = db.serialize("main")?;
    ensure!(
        image.len() as u64 == bytes,
        "Motion inspected image length changed"
    );
    allocation_reserved(&[&db], bytes, reserved)?;
    let sha256 = catabolic_core::sha256(&*image);
    check(cancel)?;
    Ok(InspectedImage {
        bytes,
        sha256,
        schema_version,
        inventory,
        binding,
    })
}
