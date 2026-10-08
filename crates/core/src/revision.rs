//! Optimistic revisions never wrap or accept a stale write.
pub fn advance(current: i64, expected: i64) -> Result<i64, &'static str> {
    if current < 0 || current != expected {
        Err("revision_conflict")
    } else {
        current.checked_add(1).ok_or("revision_exhausted")
    }
}
