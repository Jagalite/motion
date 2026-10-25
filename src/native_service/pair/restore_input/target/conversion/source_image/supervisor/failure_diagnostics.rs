//! Opt-in diagnostics for the genuine service96/94 fixtures, never production.
//! Only safe static classifications survive event visitation. Arbitrary error
//! strings, panic payloads, credentials, and retained owners are never logged.
use super::{Failure, PhaseOwner, Shared, FAILED};
use super::target_reservation::TargetPhase;
use std::fmt::{self, Write};
use std::sync::{Arc, Mutex, TryLockError, atomic::Ordering};
use tracing::{Dispatch, Event, Metadata, Subscriber, field::{Field, Visit}};
use tracing_subscriber::{Layer, layer::Context, prelude::*};

const MAX_EVENTS: usize = 4;
const MAX_ERROR_BYTES: usize = 1024;
const MAX_MESSAGE_BYTES: usize = 64;
const UNKNOWN: &str = "unclassified internal error (text redacted)";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum WorkerKind {
    Source96,
    Source94,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct SafeEvent {
    worker: WorkerKind,
    cause: &'static str,
    truncated: bool,
}
#[derive(Clone, Copy, Debug, Default)]
struct Events {
    entries: [Option<SafeEvent>; MAX_EVENTS],
    length: usize,
    additional_events_omitted: bool,
}

// Neither this value nor the subscriber owns Shared or any service resources.
// The heap sink and dispatcher bookkeeping are fixture-only diagnostic storage,
// not an assertion of the production allocation budget or physical RSS.
pub(super) struct Capture {
    source96: Dispatch,
    source94: Dispatch,
    events: Arc<Mutex<Events>>,
}
impl Capture {
    fn new() -> Self {
        let events = Arc::new(Mutex::new(Events::default()));
        let source96 = Self::make_dispatch(events.clone(), WorkerKind::Source96);
        let source94 = Self::make_dispatch(events.clone(), WorkerKind::Source94);
        Self { source96, source94, events }
    }
    fn make_dispatch(events: Arc<Mutex<Events>>, worker: WorkerKind) -> Dispatch {
        Dispatch::new(tracing_subscriber::registry().with(CaptureLayer { events, worker }))
    }
    fn dispatch(&self, worker: WorkerKind) -> Dispatch {
        match worker {
            WorkerKind::Source96 => self.source96.clone(),
            WorkerKind::Source94 => self.source94.clone(),
        }
    }
    fn snapshot(&self) -> Events {
        *self.events.lock().unwrap_or_else(|p| p.into_inner())
    }
}

pub(super) fn enable(shared: &Shared) {
    let mut capture = shared.test.failure_diagnostics.lock().unwrap_or_else(|p| p.into_inner());
    if capture.is_none() {
        *capture = Some(Capture::new());
    }
}
pub(super) fn dispatch(shared: &Shared, worker: WorkerKind) -> Option<Dispatch> {
    shared.test.failure_diagnostics.lock().unwrap_or_else(|p| p.into_inner())
        .as_ref().map(|capture| capture.dispatch(worker))
}

// There is no fmt writer and no global subscriber. This filter does not consume
// events from another fixture, the HTTP setup, or unrelated service workers.
struct CaptureLayer {
    events: Arc<Mutex<Events>>,
    worker: WorkerKind,
}
fn candidate(metadata: &Metadata<'_>) -> bool {
    metadata.is_event()
        && *metadata.level() == tracing::Level::ERROR
        && metadata.target() == "playscale::v2"
        && metadata.fields().field("error").is_some()
        && metadata.fields().field("message").is_some()
}
impl<S: Subscriber> Layer<S> for CaptureLayer {
    fn enabled(&self, metadata: &Metadata<'_>, _: Context<'_, S>) -> bool {
        candidate(metadata)
    }
    fn on_event(&self, event: &Event<'_>, _: Context<'_, S>) {
        if !candidate(event.metadata()) {
            return;
        }
        // First inspect only message and error-presence. A different message
        // must not affect even the overflow flag, and its error is not formatted.
        let mut header = MessageVisitor::default();
        event.record(&mut header);
        if !header.error_seen || !header.message_seen || header.message.truncated
            || header.message.as_str() != "v2 request failed"
        {
            return;
        }
        {
            let mut events = self.events.lock().unwrap_or_else(|p| p.into_inner());
            if events.length == MAX_EVENTS {
                // Additional accepted events never invoke their error formatter.
                events.additional_events_omitted = true;
                return;
            }
        }
        // Never hold the sink lock while invoking an error's formatter. The
        // visitor keeps raw bytes only in bounded stack buffers, then discards
        // them; the retained sink contains exclusively static safe labels.
        let mut visitor = ErrorVisitor::default();
        event.record(&mut visitor);
        let entry = SafeEvent {
            worker: self.worker,
            cause: safe_cause(&visitor.error),
            truncated: visitor.error.truncated,
        };
        let mut events = self.events.lock().unwrap_or_else(|p| p.into_inner());
        let slot = events.length;
        if slot < MAX_EVENTS {
            events.entries[slot] = Some(entry);
            events.length += 1;
        } else {
            events.additional_events_omitted = true;
        }
    }
}

// Exact whole-field matches only. A prefix, suffix, source-chain addition, or
// truncated value cannot smuggle data through a recognized safe classification.
fn safe_cause(error: &BoundedText<MAX_ERROR_BYTES>) -> &'static str {
    if error.truncated {
        return UNKNOWN;
    }
    match error.as_str() {
        "offline aggregate allocation budget exhausted" => "offline aggregate allocation budget exhausted",
        "native intent preparation cancelled" => "native intent preparation cancelled",
        "native intent inherited deadline expired" => "native intent inherited deadline expired",
        _ => UNKNOWN,
    }
}

struct BoundedText<const N: usize> {
    bytes: [u8; N],
    length: usize,
    truncated: bool,
}
impl<const N: usize> Default for BoundedText<N> {
    fn default() -> Self {
        Self { bytes: [0; N], length: 0, truncated: false }
    }
}
impl<const N: usize> BoundedText<N> {
    fn as_str(&self) -> &str {
        std::str::from_utf8(&self.bytes[..self.length]).expect("bounded formatter preserves UTF-8")
    }
}
impl<const N: usize> Write for BoundedText<N> {
    fn write_str(&mut self, value: &str) -> fmt::Result {
        if self.truncated {
            return Err(fmt::Error);
        }
        let available = N - self.length;
        let mut copied = available.min(value.len());
        while !value.is_char_boundary(copied) {
            copied -= 1;
        }
        self.bytes[self.length..self.length + copied].copy_from_slice(&value.as_bytes()[..copied]);
        self.length += copied;
        if copied != value.len() {
            self.truncated = true;
            Err(fmt::Error)
        } else {
            Ok(())
        }
    }
}
#[derive(Default)]
struct MessageVisitor {
    message: BoundedText<MAX_MESSAGE_BYTES>,
    message_seen: bool,
    error_seen: bool,
}
impl Visit for MessageVisitor {
    fn record_debug(&mut self, field: &Field, value: &dyn fmt::Debug) {
        match field.name() {
            "message" => {
                self.message_seen = true;
                if write!(&mut self.message, "{value:?}").is_err() {
                    self.message.truncated = true;
                }
            }
            "error" => self.error_seen = true,
            _ => {}
        }
    }
    fn record_str(&mut self, field: &Field, value: &str) {
        match field.name() {
            "message" => {
                self.message_seen = true;
                let _ = self.message.write_str(value);
            }
            "error" => self.error_seen = true,
            _ => {}
        }
    }
}
#[derive(Default)]
struct ErrorVisitor {
    error: BoundedText<MAX_ERROR_BYTES>,
}
impl Visit for ErrorVisitor {
    fn record_debug(&mut self, field: &Field, value: &dyn fmt::Debug) {
        if field.name() == "error" && write!(&mut self.error, "{value:?}").is_err() {
            // A formatter may fail before the byte cap. Its partial recognized
            // prefix is not an exact whole-field match and must stay redacted.
            self.error.truncated = true;
        }
    }
    fn record_str(&mut self, field: &Field, value: &str) {
        if field.name() == "error" {
            let _ = self.error.write_str(value);
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
enum RetainedSummary {
    Available { target: Option<TargetPhase>, failure: &'static str },
    Busy,
}
fn failure_kind(failure: Option<&Failure>) -> &'static str {
    match failure {
        Some(Failure::Error(_)) => "error (retained text redacted)",
        Some(Failure::Panic(payload)) if payload.is::<String>() => "panic String payload (redacted)",
        Some(Failure::Panic(payload)) if payload.is::<&str>() => "panic &str payload (redacted)",
        Some(Failure::Panic(_)) => "panic other payload (redacted)",
        None => "no phase failure retained",
    }
}
fn retained_summary(status: u8, phase: &Mutex<PhaseOwner>) -> Option<RetainedSummary> {
    if status != FAILED {
        return None; // In particular, never touch this mutex while RUNNING.
    }
    let phase = match phase.try_lock() {
        Ok(phase) => phase,
        Err(TryLockError::Poisoned(poison)) => poison.into_inner(),
        Err(TryLockError::WouldBlock) => return Some(RetainedSummary::Busy),
    };
    Some(RetainedSummary::Available {
        target: phase.target.as_ref().map(|target| target.phase),
        failure: failure_kind(phase.failure.as_ref()),
    })
}
pub(super) fn report(shared: &Shared, context: &'static str) {
    // FAILED is not a worker join or close acknowledgment. A worker can still
    // hold phase immediately after publishing status, or have uncertain spawn
    // outcome. Reporting must never wait for that mutex or change any custody.
    let Some(retained) = retained_summary(shared.status.load(Ordering::Acquire), &shared.phase) else {
        return;
    };
    // retained_summary has released phase before either diagnostic lock.
    let capture = shared.test.failure_diagnostics.lock().unwrap_or_else(|p| p.into_inner());
    if let Some(capture) = capture.as_ref() {
        let events = capture.snapshot();
        eprintln!("[service fixture failure] {context}; retained={retained:?}; internal={events:?}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn event(error: &str) {
        // Exercise the actual redaction/logging boundary, not a replacement.
        let problem = crate::v2::Problem::internal(error);
        assert_eq!(problem.status, axum::http::StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(problem.code, "internal_error");
        assert_eq!(problem.detail, "The request could not be completed");
    }
    fn empty_phase() -> PhaseOwner {
        PhaseOwner { target: None, workspace: None, selection: None, pair: None,
            image: None, failure: None, close_failure: None }
    }

    #[test]
    fn exact_event_filter_and_unknown_text_are_private() {
        let capture = Capture::new();
        let dispatch = capture.dispatch(WorkerKind::Source96);
        tracing::dispatcher::with_default(&dispatch, || {
            tracing::error!(target: "unrelated", error = "private credential", "v2 request failed");
            tracing::warn!(target: "playscale::v2", error = "private credential", "v2 request failed");
            tracing::error!(target: "playscale::v2", error = "private credential", "other message");
            tracing::error!(target: "playscale::v2", "v2 request failed");
        });
        assert_eq!(capture.snapshot().length, 0);
        tracing::dispatcher::with_default(&dispatch, || {
            event("offline aggregate allocation budget exhausted");
            event("offline aggregate allocation budget exhausted: private credential");
            event("private credential and retained payload");
        });
        let events = capture.snapshot();
        assert_eq!(events.length, 3);
        assert_eq!(events.entries[0].unwrap().cause, "offline aggregate allocation budget exhausted");
        assert_eq!(events.entries[1].unwrap().cause, UNKNOWN);
        assert_eq!(events.entries[2].unwrap().cause, UNKNOWN);
        let report = format!("{events:?}");
        assert!(!report.contains("private credential"));
        assert!(!report.contains("retained payload"));
    }

    #[test]
    fn event_count_and_field_formatting_are_bounded() {
        let capture = Capture::new();
        let dispatch = capture.dispatch(WorkerKind::Source96);
        let oversized = "é".repeat(MAX_ERROR_BYTES);
        tracing::dispatcher::with_default(&dispatch, || {
            event(&oversized);
            for _ in 0..(MAX_EVENTS + 3) {
                event("offline aggregate allocation budget exhausted");
            }
        });
        let events = capture.snapshot();
        assert_eq!(events.length, MAX_EVENTS);
        assert!(events.additional_events_omitted);
        assert_eq!(events.entries[0].unwrap().cause, UNKNOWN);
        assert!(events.entries[0].unwrap().truncated);
        assert!(!format!("{events:?}").contains('é'));
        let mut text = BoundedText::<5>::default();
        assert!(text.write_str("ééé").is_err());
        assert_eq!(text.as_str(), "éé");
        assert_eq!(text.length, 4);
        assert!(text.truncated);
        assert!(text.write_str("x").is_err());
    }

    #[test]
    fn filtered_and_full_events_do_not_format_errors_and_partial_errors_are_redacted() {
        struct MustNotFormat;
        impl fmt::Display for MustNotFormat {
            fn fmt(&self, _: &mut fmt::Formatter<'_>) -> fmt::Result {
                panic!("filtered or full event error was formatted");
            }
        }
        struct Incomplete;
        impl fmt::Display for Incomplete {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("offline aggregate allocation budget exhausted")?;
                Err(fmt::Error)
            }
        }
        let capture = Capture::new();
        let dispatch = capture.dispatch(WorkerKind::Source96);
        tracing::dispatcher::with_default(&dispatch, || {
            let _ = crate::v2::Problem::internal(Incomplete);
            for _ in 1..MAX_EVENTS {
                event("offline aggregate allocation budget exhausted");
            }
            tracing::error!(target: "playscale::v2", error = %MustNotFormat, "other message");
        });
        let events = capture.snapshot();
        assert_eq!(events.length, MAX_EVENTS);
        assert!(!events.additional_events_omitted);
        assert_eq!(events.entries[0].unwrap().cause, UNKNOWN);
        assert!(events.entries[0].unwrap().truncated);
        tracing::dispatcher::with_default(&dispatch, || {
            let _ = crate::v2::Problem::internal(MustNotFormat);
        });
        assert!(capture.snapshot().additional_events_omitted);
    }

    #[test]
    fn recognized_prefix_with_truncated_suffix_is_not_disclosed() {
        let mut text = BoundedText::<MAX_ERROR_BYTES>::default();
        let value = format!("offline aggregate allocation budget exhausted{}", "x".repeat(MAX_ERROR_BYTES));
        assert!(text.write_str(&value).is_err());
        assert_eq!(safe_cause(&text), UNKNOWN);
    }

    #[tokio::test]
    async fn blocking_workers_capture_only_their_own_fixture() {
        let left = Capture::new();
        let right = Capture::new();
        let a = left.dispatch(WorkerKind::Source96);
        let b = right.dispatch(WorkerKind::Source94);
        let barrier = Arc::new(std::sync::Barrier::new(2));
        let other_barrier = barrier.clone();
        let a = tokio::task::spawn_blocking(move || {
            let _guard = tracing::dispatcher::set_default(&a);
            barrier.wait();
            event("offline aggregate allocation budget exhausted");
        });
        let b = tokio::task::spawn_blocking(move || {
            let _guard = tracing::dispatcher::set_default(&b);
            other_barrier.wait();
            event("native intent preparation cancelled");
        });
        let (a, b) = tokio::join!(a, b);
        a.unwrap();
        b.unwrap();
        let a = left.snapshot();
        let b = right.snapshot();
        assert_eq!(a.length, 1);
        assert_eq!(b.length, 1);
        assert_eq!(a.entries[0].unwrap().worker, WorkerKind::Source96);
        assert_eq!(b.entries[0].unwrap().worker, WorkerKind::Source94);
        assert_eq!(a.entries[0].unwrap().cause, "offline aggregate allocation budget exhausted");
        assert_eq!(b.entries[0].unwrap().cause, "native intent preparation cancelled");
    }

    #[test]
    fn dispatcher_restores_on_same_thread_after_success_and_unwind() {
        let outer = Capture::new();
        let inner = Capture::new();
        let a = outer.dispatch(WorkerKind::Source94);
        let b = inner.dispatch(WorkerKind::Source96);
        std::thread::spawn(move || {
            let _outer = tracing::dispatcher::set_default(&a);
            {
                let _inner = tracing::dispatcher::set_default(&b);
                event("offline aggregate allocation budget exhausted");
            }
            event("native intent preparation cancelled");
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                let _inner = tracing::dispatcher::set_default(&b);
                event("offline aggregate allocation budget exhausted");
                panic!("fixture dispatcher unwind");
            }));
            assert!(result.is_err());
            event("native intent inherited deadline expired");
        }).join().unwrap();
        assert_eq!(outer.snapshot().length, 2);
        assert_eq!(inner.snapshot().length, 2);
        assert!(outer.snapshot().entries[..2].iter().all(|entry| entry.unwrap().worker == WorkerKind::Source94));
        assert!(inner.snapshot().entries[..2].iter().all(|entry| entry.unwrap().worker == WorkerKind::Source96));
    }

    #[test]
    fn retained_inspection_is_terminal_only_and_nonblocking() {
        let phase = Mutex::new(empty_phase());
        let held = phase.lock().unwrap();
        for status in [super::super::IDLE, super::super::RUNNING, super::super::COMPLETE, super::super::CLOSED] {
            assert!(retained_summary(status, &phase).is_none());
        }
        assert_eq!(retained_summary(FAILED, &phase), Some(RetainedSummary::Busy));
        drop(held);
        assert_eq!(retained_summary(FAILED, &phase), Some(RetainedSummary::Available {
            target: None, failure: "no phase failure retained",
        }));
    }

    #[test]
    fn retained_panic_and_error_payloads_are_never_formatted_or_consumed() {
        struct PrivatePayload;
        let mut phase = empty_phase();
        for failure in [
            Failure::Panic(Box::new("private panic text".to_owned())),
            Failure::Panic(Box::new("private borrowed panic")),
            Failure::Panic(Box::new(PrivatePayload)),
            Failure::Error(anyhow::anyhow!("private retained error")),
        ] {
            phase.failure = Some(failure);
            let locked = Mutex::new(phase);
            let summary = retained_summary(FAILED, &locked).unwrap();
            assert!(!format!("{summary:?}").contains("private"));
            assert!(locked.lock().unwrap().failure.is_some());
            phase = locked.into_inner().unwrap();
        }
    }

    #[test]
    fn poisoned_terminal_phase_does_not_hide_safe_failure_kind() {
        let phase = Mutex::new(empty_phase());
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _held = phase.lock().unwrap();
            panic!("fixture phase poison");
        }));
        assert!(result.is_err());
        assert_eq!(retained_summary(FAILED, &phase), Some(RetainedSummary::Available {
            target: None, failure: "no phase failure retained",
        }));
    }
}
