//! Administrator playback monitoring: what each live delivery is sending and
//! how its playback is going, and a durable history of finished sessions.
//!
//! These are observations. Admission, lease renewal, fencing and teardown are
//! decided by the delivery reducer in `crates/core`; this module records what
//! it decided and measures what was transferred, where the bytes are written.
//! Nothing here feeds back into a playback decision.

use std::{
    net::{IpAddr, SocketAddr},
    pin::Pin,
    sync::{
        Arc, Mutex, OnceLock,
        atomic::{AtomicU64, AtomicUsize, Ordering},
    },
    task::{Context, Poll},
};

use axum::body::{Body, Bytes};
use http_body::Body as HttpBody;
use playscale_core::playback_monitor::{
    EndReason, Observation, Persisted, Snapshot, Viewing, ViewingRow, Watch,
};

/// Seconds of transfer averaged into the current throughput.
pub const WINDOW_SECONDS: u64 = 10;
/// Finished sessions are retained this long.
pub const RETENTION_DAYS: i64 = 90;
const RETENTION_ROWS: i64 = 100_000;
const PRUNE_LIMIT: i64 = 1_000;

/// Where the client that requested a delivery connected from. Never the
/// address itself: only whether it was this machine, a private network or
/// anywhere else.
pub fn classify(peer: Option<IpAddr>, trusted_ingress: bool) -> &'static str {
    if trusted_ingress {
        return "remote"; // a configured ingress proxy, e.g. a tailnet
    }
    let Some(ip) = peer else {
        return "unknown";
    };
    let ip = match ip {
        IpAddr::V6(v6) => v6
            .to_ipv4_mapped()
            .map(IpAddr::V4)
            .unwrap_or(IpAddr::V6(v6)),
        other => other,
    };
    match ip {
        IpAddr::V4(v4) if v4.is_loopback() => "local",
        IpAddr::V6(v6) if v6.is_loopback() => "local",
        IpAddr::V4(v4) if v4.is_private() || v4.is_link_local() => "private",
        // Unique local (fc00::/7) and link-local (fe80::/10) IPv6.
        IpAddr::V6(v6)
            if (v6.segments()[0] & 0xfe00) == 0xfc00 || (v6.segments()[0] & 0xffc0) == 0xfe80 =>
        {
            "private"
        }
        _ => "remote",
    }
}

/// The client network of a request, from the listener's peer address.
#[derive(Clone, Copy, Debug)]
pub struct ClientNetwork(pub &'static str);

impl<S: Send + Sync> axum::extract::FromRequestParts<S> for ClientNetwork {
    type Rejection = std::convert::Infallible;
    async fn from_request_parts(
        parts: &mut axum::http::request::Parts,
        _: &S,
    ) -> Result<Self, Self::Rejection> {
        let peer = parts
            .extensions
            .get::<axum::extract::ConnectInfo<SocketAddr>>()
            .map(|c| c.0.ip());
        let ingress = parts
            .extensions
            .get::<crate::v2::auth::TrustedIngress>()
            .is_some();
        Ok(Self(classify(peer, ingress)))
    }
}

/// How a delivery's bytes are produced, by the operation of its generation.
pub fn mode(operation: playscale_core::delivery::Operation) -> &'static str {
    use playscale_core::delivery::Operation;
    match operation {
        Operation::Original | Operation::Prepared => "direct",
        Operation::Remux | Operation::AudioConvert => "remux",
        Operation::VideoTranscode => "transcode",
    }
}

pub(crate) fn wall_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64)
}

/// Slots in the ring: the window's whole seconds plus the second in progress,
/// so the current second never overwrites one still being averaged.
const SLOTS: u64 = WINDOW_SECONDS + 1;

/// Bytes per second in the last `WINDOW_SECONDS` whole seconds.
#[derive(Default)]
struct Window {
    second: u64,
    buckets: [u64; SLOTS as usize],
}

impl Window {
    fn advance(&mut self, second: u64) {
        if second <= self.second {
            return;
        }
        let gap = (second - self.second).min(SLOTS);
        for step in 1..=gap {
            self.buckets[((self.second + step) % SLOTS) as usize] = 0;
        }
        self.second = second;
    }
    fn add(&mut self, second: u64, bytes: u64) {
        self.advance(second);
        let slot = &mut self.buckets[(second % SLOTS) as usize];
        *slot = slot.saturating_add(bytes);
    }
    /// The average over the window of completed seconds before `second`.
    fn rate(&mut self, second: u64) -> u64 {
        self.advance(second);
        let total: u64 = (1..=WINDOW_SECONDS)
            .map(|back| self.buckets[((second + SLOTS - back) % SLOTS) as usize])
            .fold(0, u64::saturating_add);
        total / WINDOW_SECONDS
    }
}

/// One delivery's observed playback and transfer. What observations mean
/// (ordering, watched time, stall episodes, what a history row still needs)
/// is decided by `playscale_core::playback_monitor`; this holds the state
/// and supplies the clock.
pub struct Telemetry {
    pub started_ms: u64,
    pub network: &'static str,
    bytes: AtomicU64,
    peak_bps: AtomicU64,
    last_activity_ms: AtomicU64,
    window: Mutex<Window>,
    watch: Mutex<Watch>,
    end: OnceLock<EndReason>,
    /// What the history row is known to hold.
    persisted: Mutex<Persisted>,
    /// Counted responses still being sent.
    open: AtomicUsize,
    /// How the bytes are produced, as of the last reducer state with a
    /// generation (kept after generations are cleaned up).
    mode: Mutex<&'static str>,
    /// Bytes counted into the process-wide total.
    total: Arc<AtomicU64>,
}

/// A point-in-time view for the monitor.
#[derive(Clone, Debug)]
pub struct Reading {
    pub bytes_sent: u64,
    pub throughput_bps: u64,
    pub average_bps: u64,
    pub peak_bps: u64,
    pub position_ms: Option<u64>,
    pub start_position_ms: Option<u64>,
    pub watched_ms: u64,
    pub stalls: u64,
    pub stalled: bool,
    pub advanced_recently: bool,
    pub viewing: Option<Viewing>,
    pub last_activity_ms: u64,
}

impl Telemetry {
    pub fn new(network: &'static str, start_position: Option<u64>, total: Arc<AtomicU64>) -> Self {
        let now = wall_ms();
        Self {
            started_ms: now,
            network,
            bytes: AtomicU64::new(0),
            peak_bps: AtomicU64::new(0),
            last_activity_ms: AtomicU64::new(now),
            window: Mutex::new(Window {
                second: now / 1000,
                ..Default::default()
            }),
            watch: Mutex::new(Watch::new(start_position)),
            end: OnceLock::new(),
            persisted: Mutex::new(Persisted::default()),
            open: AtomicUsize::new(0),
            mode: Mutex::new("direct"),
            total,
        }
    }

    /// Bytes written to the client for this delivery.
    pub fn sent(&self, n: u64) {
        if n == 0 {
            return;
        }
        let now = wall_ms();
        self.bytes.fetch_add(n, Ordering::Relaxed);
        self.total.fetch_add(n, Ordering::Relaxed);
        self.last_activity_ms.store(now, Ordering::Relaxed);
        self.window.lock().unwrap().add(now / 1000, n);
    }

    /// A playhead the delivery reducer accepted.
    pub fn playhead(&self, generation: u64, sequence: u64, position_ms: u64) {
        let now = wall_ms();
        if self.watch.lock().unwrap().observe(Observation::Playhead {
            generation,
            sequence,
            position_ms,
            now_ms: now,
        }) {
            self.last_activity_ms.store(now, Ordering::Relaxed);
        }
    }

    /// The delivery's authoritative viewing row, read with the periodic flush.
    pub fn viewing(&self, row: ViewingRow, status: Viewing, position_ms: u64) {
        let now = wall_ms();
        if self.watch.lock().unwrap().observe(Observation::Viewing {
            row,
            status,
            position_ms,
            now_ms: now,
        }) {
            self.last_activity_ms.store(now, Ordering::Relaxed);
        }
    }

    /// Advance once a second: roll the throughput window, track the peak and
    /// detect stalls.
    pub fn tick(&self) {
        let now = wall_ms();
        let rate = self.window.lock().unwrap().rate(now / 1000);
        self.peak_bps.fetch_max(rate, Ordering::Relaxed);
        self.watch
            .lock()
            .unwrap()
            .observe(Observation::Tick { now_ms: now });
    }

    /// Record why the delivery ended, as the reducer step that fenced it
    /// decided. Only that step has a reason; the first is kept regardless.
    pub fn end(&self, reason: EndReason) {
        let _ = self.end.set(reason);
    }
    pub fn ended(&self) -> Option<EndReason> {
        self.end.get().copied()
    }

    pub fn set_mode(&self, mode: &'static str) {
        *self.mode.lock().unwrap() = mode;
    }
    pub fn mode(&self) -> &'static str {
        *self.mode.lock().unwrap()
    }

    /// Counted responses still being sent.
    pub fn open_responses(&self) -> usize {
        self.open.load(Ordering::Acquire)
    }

    pub fn read(&self) -> Reading {
        let now = wall_ms();
        let bytes = self.bytes.load(Ordering::Relaxed);
        let throughput = self.window.lock().unwrap().rate(now / 1000);
        let elapsed_ms = now.saturating_sub(self.started_ms);
        let w = self.watch.lock().unwrap();
        Reading {
            bytes_sent: bytes,
            throughput_bps: throughput,
            average_bps: average_bps(bytes, elapsed_ms),
            peak_bps: self.peak_bps.load(Ordering::Relaxed).max(throughput),
            position_ms: w.position_ms,
            start_position_ms: w.start_position_ms,
            watched_ms: w.watched_ms,
            stalls: w.stalls,
            stalled: w.stalled,
            advanced_recently: w.advanced_recently(now),
            viewing: w.viewing,
            last_activity_ms: self.last_activity_ms.load(Ordering::Relaxed),
        }
    }

    /// What the history row should hold now, with the delivery's own state
    /// (so reducer-driven status changes are written and announced) and how
    /// its bytes are produced now.
    fn snapshot(
        r: &Reading,
        delivery: Option<playscale_core::delivery::Status>,
        mode: &str,
    ) -> Snapshot {
        Snapshot {
            bytes_sent: r.bytes_sent,
            peak_bps: r.peak_bps,
            watched_ms: r.watched_ms,
            stalls: r.stalls,
            position_ms: r.position_ms,
            last_activity_ms: r.last_activity_ms,
            status: status(delivery, r).to_string(),
            mode: mode.to_string(),
        }
    }
}

/// Bytes per second over a duration in milliseconds, without truncating it to
/// whole seconds; durations under a second count as one.
pub fn average_bps(bytes: u64, elapsed_ms: u64) -> u64 {
    (u128::from(bytes) * 1000 / u128::from(elapsed_ms.max(1000))) as u64
}

/// A position as stored: positions beyond the API's range are never
/// observed (see `MAX_POSITION_MS`), so this only guards the conversion.
fn stored(position: Option<u64>) -> Option<i64> {
    position.and_then(|p| i64::try_from(p).ok())
}

/// The monitor's status for a session: the delivery's own state first, then
/// what the player says, then what the position shows.
pub fn status(delivery: Option<playscale_core::delivery::Status>, r: &Reading) -> &'static str {
    use playscale_core::delivery::Status;
    match delivery {
        Some(Status::Starting) => return "starting",
        Some(Status::Transitioning) => return "buffering",
        Some(s) if s.fenced() => return "closing",
        _ => {}
    }
    if r.stalled {
        return "stalled";
    }
    match r.viewing {
        Some(Viewing::Playing) => "playing",
        Some(_) => "paused",
        None if r.advanced_recently => "playing",
        None => "paused",
    }
}

/// A response reserved against a delivery's telemetry from the moment its
/// request is admitted: until it is dropped (sent, abandoned, or never built
/// because preparation failed) the delivery's history waits for it.
pub struct Transfer {
    telemetry: Arc<Telemetry>,
}

impl Transfer {
    pub fn begin(telemetry: Arc<Telemetry>) -> Self {
        telemetry.open.fetch_add(1, Ordering::AcqRel);
        Self { telemetry }
    }
    /// Count the response's bytes as they are sent; the transfer ends with
    /// the body.
    pub fn wrap(self, response: axum::response::Response) -> axum::response::Response {
        response.map(|body| {
            Body::new(Counted {
                body,
                transfer: self,
            })
        })
    }
}

impl Drop for Transfer {
    fn drop(&mut self) {
        self.telemetry.open.fetch_sub(1, Ordering::AcqRel);
    }
}

/// A response body that counts the bytes it yields into a delivery's
/// telemetry. Bytes are counted as frames are handed to the connection, so a
/// cancelled download counts only what was sent.
pub struct Counted {
    body: Body,
    transfer: Transfer,
}

impl HttpBody for Counted {
    type Data = Bytes;
    type Error = axum::Error;

    fn poll_frame(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<http_body::Frame<Self::Data>, Self::Error>>> {
        let result = Pin::new(&mut self.body).poll_frame(cx);
        if let Poll::Ready(Some(Ok(frame))) = &result
            && let Some(data) = frame.data_ref()
        {
            self.transfer.telemetry.sent(data.len() as u64);
        }
        result
    }
    fn is_end_stream(&self) -> bool {
        self.body.is_end_stream()
    }
    fn size_hint(&self) -> http_body::SizeHint {
        self.body.size_hint()
    }
}

/// The identity a history row is started with.
pub struct Started<'a> {
    pub delivery_id: &'a str,
    pub principal_id: &'a str,
    pub profile_id: &'a str,
    pub timeline_id: &'a str,
    pub version_id: &'a str,
    pub file_id: &'a str,
    pub mode: &'static str,
}

/// Start a history row (in the admitting transaction) and announce it.
pub async fn start(
    conn: &mut sqlx::SqliteConnection,
    s: Started<'_>,
    telemetry: &Telemetry,
) -> Result<(), sqlx::Error> {
    let w = telemetry.read();
    sqlx::query("INSERT INTO playback_monitor_sessions(delivery_id,principal_id,profile_id,timeline_id,version_id,file_id,mode,client_network,started_at,last_activity_at,start_position_ms) VALUES (?,?,?,?,?,?,?,?,?,?,?)")
        .bind(s.delivery_id).bind(s.principal_id).bind(s.profile_id).bind(s.timeline_id)
        .bind(s.version_id).bind(s.file_id).bind(s.mode).bind(telemetry.network)
        .bind(telemetry.started_ms as i64).bind(telemetry.started_ms as i64)
        .bind(stored(w.start_position_ms))
        .execute(&mut *conn).await?;
    announce(conn, s.delivery_id, "changed").await
}

/// A change hint for administrators. An unknown topic is administrative, so
/// the event stream discloses it only to `system:admin` principals.
async fn announce(
    conn: &mut sqlx::SqliteConnection,
    id: &str,
    kind: &str,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO change_events(topic,resource_id,kind) VALUES ('playback_sessions',?,?)",
    )
    .bind(id)
    .bind(kind)
    .execute(&mut *conn)
    .await
    .map(|_| ())
}

/// The viewing row a delivery's monitoring follows: the session its timeline
/// viewing state names, if that session is on this delivery, otherwise the
/// newest. Timestamps have one-second precision, so a tie between a
/// superseded and a replacing session is broken by that recorded fact.
pub const AUTHORITATIVE_VIEWING: &str = "SELECT vs.id,vs.revision,vs.status,vs.position_ms,vs.updated_at FROM viewing_sessions vs \
    LEFT JOIN timeline_viewing tv ON tv.profile_id=vs.profile_id AND tv.timeline_id=vs.timeline_id AND tv.session_id=vs.id \
    WHERE vs.delivery_id=? ORDER BY tv.session_id IS NULL, vs.updated_at DESC, vs.revision DESC, vs.id DESC LIMIT 1";

/// A live delivery as a flush sees it.
pub struct Live {
    pub id: String,
    pub telemetry: Arc<Telemetry>,
    /// The reducer's status and how the bytes are produced now.
    pub status: playscale_core::delivery::Status,
    pub mode: &'static str,
}

/// Fold the players' viewing rows into live telemetry, then write every
/// session whose history row is behind. A snapshot counts as written only
/// once its transaction commits; after a failure the rows stay due and the
/// next flush retries them. Hints go out only when a session's status changed.
pub async fn flush(db: &sqlx::SqlitePool, live: &[Live]) -> Result<(), sqlx::Error> {
    if live.is_empty() {
        return Ok(());
    }
    let mut conn = db.acquire().await?;
    for session in live {
        let row: Option<(String, i64, String, i64, i64)> = sqlx::query_as(AUTHORITATIVE_VIEWING)
            .bind(&session.id)
            .fetch_optional(&mut *conn)
            .await?;
        if let Some((id, revision, status, position, updated)) = row
            && let Some(status) = Viewing::parse(&status)
        {
            let row = ViewingRow {
                id,
                revision: u64::try_from(revision).unwrap_or(0),
                updated_ms: u64::try_from(updated).unwrap_or(0).saturating_mul(1000),
            };
            session
                .telemetry
                .viewing(row, status, u64::try_from(position).unwrap_or(0));
        }
    }
    drop(conn);
    let due: Vec<_> = live
        .iter()
        .filter_map(|session| {
            let reading = session.telemetry.read();
            let snapshot = Telemetry::snapshot(&reading, Some(session.status), session.mode);
            let due = session.telemetry.persisted.lock().unwrap().due(&snapshot)?;
            Some((session, due))
        })
        .collect();
    if due.is_empty() {
        return Ok(());
    }
    let mut tx = crate::db::begin_write(db).await?;
    for (session, due) in &due {
        let s = &due.snapshot;
        sqlx::query("UPDATE playback_monitor_sessions SET bytes_sent=?,peak_bps=?,watched_ms=?,stalls=?,end_position_ms=?,last_activity_at=?,mode=? WHERE delivery_id=? AND ended_at IS NULL")
            .bind(s.bytes_sent as i64).bind(s.peak_bps as i64).bind(s.watched_ms as i64)
            .bind(s.stalls as i64).bind(stored(s.position_ms))
            .bind(s.last_activity_ms as i64).bind(&s.mode).bind(&session.id)
            .execute(&mut *tx).await?;
        if due.status_changed {
            announce(&mut tx, &session.id, "changed").await?;
        }
    }
    tx.commit().await?;
    for (session, due) in due {
        session
            .telemetry
            .persisted
            .lock()
            .unwrap()
            .acknowledge(due.snapshot);
    }
    Ok(())
}

/// Durably note why a delivery ended, in the transaction that records the
/// decision, so recovery after a crash keeps it. The first reason stays.
pub async fn record_end(
    conn: &mut sqlx::SqliteConnection,
    id: &str,
    reason: EndReason,
) -> Result<(), sqlx::Error> {
    sqlx::query("UPDATE playback_monitor_sessions SET end_reason=? WHERE delivery_id=? AND ended_at IS NULL AND end_reason IS NULL")
        .bind(reason.as_str())
        .bind(id)
        .execute(conn)
        .await
        .map(|_| ())
}

/// Finish a session's history row with its final counters. The reason
/// already recorded with the ending decision wins over `fallback`.
pub async fn finish(
    conn: &mut sqlx::SqliteConnection,
    id: &str,
    telemetry: &Telemetry,
    mode: &str,
    fallback: EndReason,
) -> Result<(), sqlx::Error> {
    let r = telemetry.read();
    let reason = telemetry.ended().unwrap_or(fallback);
    let now = wall_ms() as i64;
    let done = sqlx::query("UPDATE playback_monitor_sessions SET bytes_sent=?,peak_bps=?,watched_ms=?,stalls=?,end_position_ms=?,last_activity_at=?,mode=?,ended_at=?,end_reason=coalesce(end_reason,?) WHERE delivery_id=? AND ended_at IS NULL")
        .bind(r.bytes_sent as i64).bind(r.peak_bps as i64).bind(r.watched_ms as i64)
        .bind(r.stalls as i64).bind(stored(r.position_ms))
        .bind(r.last_activity_ms as i64).bind(mode).bind(now).bind(reason.as_str()).bind(id)
        .execute(&mut *conn).await?;
    if done.rows_affected() > 0 {
        announce(conn, id, "deleted").await?;
    }
    Ok(())
}

/// Sessions of a previous process can no longer be live: finish them at
/// their last recorded activity with their last counters, keeping a reason
/// recorded with an ending decision and otherwise as interrupted.
pub async fn recover(conn: &mut sqlx::SqliteConnection) -> Result<u64, sqlx::Error> {
    sqlx::query("UPDATE playback_monitor_sessions SET ended_at=last_activity_at,end_reason=coalesce(end_reason,'interrupted') WHERE ended_at IS NULL")
        .execute(conn)
        .await
        .map(|r| r.rows_affected())
}

/// Bounded retention: finished sessions older than RETENTION_DAYS, then the
/// oldest beyond RETENTION_ROWS, a batch at a time.
pub async fn prune(db: &sqlx::SqlitePool) -> Result<(), sqlx::Error> {
    let cutoff = (wall_ms() as i64).saturating_sub(RETENTION_DAYS * 86_400_000);
    let mut tx = crate::db::begin_write(db).await?;
    sqlx::query("DELETE FROM playback_monitor_sessions WHERE delivery_id IN (SELECT delivery_id FROM playback_monitor_sessions WHERE ended_at IS NOT NULL AND ended_at<? ORDER BY ended_at LIMIT ?)")
        .bind(cutoff).bind(PRUNE_LIMIT).execute(&mut *tx).await?;
    sqlx::query("DELETE FROM playback_monitor_sessions WHERE delivery_id IN (SELECT delivery_id FROM playback_monitor_sessions WHERE ended_at IS NOT NULL ORDER BY ended_at DESC, delivery_id DESC LIMIT ? OFFSET ?)")
        .bind(PRUNE_LIMIT).bind(RETENTION_ROWS).execute(&mut *tx).await?;
    tx.commit().await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_networks_without_exposing_addresses() {
        let ip = |s: &str| Some(s.parse::<IpAddr>().unwrap());
        assert_eq!(classify(ip("127.0.0.1"), false), "local");
        assert_eq!(classify(ip("::1"), false), "local");
        assert_eq!(classify(ip("::ffff:192.168.1.4"), false), "private");
        assert_eq!(classify(ip("10.2.3.4"), false), "private");
        assert_eq!(classify(ip("fd12::1"), false), "private");
        assert_eq!(classify(ip("8.8.8.8"), false), "remote");
        assert_eq!(classify(ip("127.0.0.1"), true), "remote");
        assert_eq!(classify(None, false), "unknown");
    }

    #[test]
    fn window_averages_the_last_whole_seconds() {
        let mut w = Window {
            second: 100,
            ..Default::default()
        };
        w.add(100, 1_000);
        w.add(101, 3_000);
        assert_eq!(w.rate(102), 400); // 4 000 bytes over 10 s
        assert_eq!(w.rate(111), 300); // second 100 left the window
        assert_eq!(w.rate(200), 0);
    }

    #[test]
    fn telemetry_follows_core_ordering() {
        let t = Telemetry::new("local", None, Arc::default());
        t.playhead(1, 2, 10_000);
        t.playhead(1, 1, 5_000); // stale: ignored
        let r = t.read();
        assert_eq!(r.position_ms, Some(10_000));
        assert_eq!(r.start_position_ms, Some(10_000));
        t.viewing(
            ViewingRow {
                id: "v".into(),
                revision: 1,
                updated_ms: 1_000,
            },
            Viewing::Playing,
            2_000,
        );
        let r = t.read();
        assert_eq!(
            r.position_ms,
            Some(10_000),
            "a viewing row never rewinds a reducer playhead"
        );
        assert_eq!(status(None, &r), "playing");
    }

    #[test]
    fn first_end_reason_wins() {
        let t = Telemetry::new("unknown", None, Arc::default());
        t.end(EndReason::Error);
        t.end(EndReason::Closed);
        assert_eq!(t.ended(), Some(EndReason::Error));
    }
}
