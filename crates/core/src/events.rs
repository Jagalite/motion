//! Reconnect policy for durable event-log cursors.
pub fn reset_cursor(cursor: i64, oldest: i64, newest: i64) -> Option<i64> {
    (cursor < oldest.saturating_sub(1) || cursor > newest).then_some(newest)
}
