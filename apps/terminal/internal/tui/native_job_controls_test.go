package tui

import (
	"strings"
	"testing"

	"github.com/Jagalite/motion/apps/terminal/internal/api"
	"github.com/Jagalite/motion/apps/terminal/internal/operations"
)

func nativeJobControlFeatures() []api.Feature {
	return append(nativeJobReadFeatures(), api.Feature{
		ID: "processing.native_preparation_controls", Implemented: true, Enabled: true,
		Limits: map[string]string{"contract_version": "1", "operations": "cancel,retry", "cohort": "native_preparation"},
	})
}

func nativeJob(id, phase string, cancel, retry bool) api.Job {
	return api.Job{ID: id, Kind: "rendition", Phase: phase, Controls: &api.JobControls{
		Family: "native_preparation", Version: 1, Cohort: true,
		CancelRequestAvailable: cancel, RetryRequestAvailable: retry,
	}}
}

func assertJobShortcuts(t *testing.T, m Model, cancel, retry bool) {
	t.Helper()
	foundCancel, foundRetry := false, false
	for _, binding := range m.shortHelp() {
		for _, shortcut := range binding.Keys() {
			foundCancel = foundCancel || shortcut == "c"
			foundRetry = foundRetry || shortcut == "R"
		}
	}
	if foundCancel != cancel || foundRetry != retry {
		t.Fatalf("shortcuts cancel=%v retry=%v, want %v %v", foundCancel, foundRetry, cancel, retry)
	}
}

func TestNativeJobControlsOfferOnlySupportedMixedHistoryRows(t *testing.T) {
	history := nativeJob("history", "failed", true, true)
	history.Controls.Cohort = false
	unknown := nativeJob("unknown", "failed", true, true)
	unknown.Controls.Version = 2
	scan := nativeJob("scan", "running", true, true)
	scan.Kind = "scan"
	m, f := processingWithFeatures(t, nativeJobControlFeatures(),
		nativeJob("cancel", "running", true, false),
		nativeJob("retry", "failed", false, true), history, unknown, scan,
		api.Job{ID: "legacy", Kind: "rendition", Phase: "failed"})
	if m.readOnlyJobs() || !strings.Contains(screen(m), "other job history is read-only") {
		t.Fatal("bounded native controls not presented")
	}
	for _, id := range []string{"history", "unknown", "scan", "legacy"} {
		m = selectJob(t, m, id)
		assertJobShortcuts(t, m, false, false)
		m = press(t, m, "c", "R", "y")
		if m.confirm.open() || len(f.Mutations()) != 0 || ops(f.Calls()) != "" {
			t.Fatalf("unsupported %s dispatched: %+v %s", id, m.confirm, ops(f.Calls()))
		}
	}
	m = press(t, m, "g")
	m = selectJob(t, m, "cancel")
	assertJobShortcuts(t, m, true, false)
	m = press(t, m, "c", "y")
	m = selectJob(t, m, "retry")
	assertJobShortcuts(t, m, false, true)
	m = press(t, m, "R", "y")
	if got := ops(f.Calls()); got != `cancel cancel ,get retry ,retry retry "r-1"` {
		t.Fatalf("bounded requests %s", got)
	}
}

func TestNativeJobControlsPreserveExactReplayAcrossHintChanges(t *testing.T) {
	for _, kind := range []operations.JobActionKind{operations.CancelJob, operations.RetryJob} {
		t.Run(string(kind), func(t *testing.T) {
			key, phase := "c", "running"
			if kind == operations.RetryJob {
				key, phase = "R", "failed"
			}
			m, f := processingWithFeatures(t, nativeJobControlFeatures(), nativeJob("j1", phase, true, true))
			m = selectJob(t, m, "j1")
			f.LoseAcks(1)
			m = press(t, m, key, "y")
			held, ok := m.jobs.Pending(kind, "j1")
			if !ok {
				t.Fatal("uncertain native intent discarded")
			}
			f.SetPhase("j1", "completed")
			f.SetControls("j1", nativeJob("j1", "completed", false, false).Controls)
			m = drain(t, m, m.load(keyJobs))
			before := len(f.Calls())
			m = press(t, m, key)
			if !m.confirm.resume || m.confirm.etag != held.IfMatch || len(f.Calls()) != before {
				t.Fatalf("replay re-reviewed or replaced the intent: %+v", m.confirm)
			}
			m = press(t, m, "y")
			calls := f.Mutations()
			if len(calls) != 2 || calls[0] != calls[1] || calls[1].Key != held.Key {
				t.Fatalf("replay identity changed: %+v", calls)
			}
			if _, ok := m.jobs.Pending(kind, "j1"); ok {
				t.Fatal("acknowledged replay remains pending")
			}
			m = press(t, m, key, "y")
			if len(f.Mutations()) != 2 {
				t.Fatal("false hints permitted a fresh action after replay")
			}
		})
	}
}

func TestNativeJobControlsWithdrawalRetainsUncertainIntent(t *testing.T) {
	for _, withdrawal := range []string{"capability", "permission", "all_leaves", "refresh_failure"} {
		t.Run(withdrawal, func(t *testing.T) {
			m, f := processingWithFeatures(t, nativeJobControlFeatures(), nativeJob("j1", "failed", false, true))
			m = selectJob(t, m, "j1")
			f.LoseAcks(1)
			m = press(t, m, "R", "y", "R")
			held, ok := m.jobs.Pending(operations.RetryJob, "j1")
			if !ok || !m.confirm.resume {
				t.Fatal("missing uncertain retry confirmation")
			}
			b := m.backend.(fixtureBackend)
			switch withdrawal {
			case "capability":
				b.fake.features = nativeJobReadFeatures()
			case "permission":
				b.fake.permissions = []string{api.PermCatalogRead}
			case "all_leaves":
				b.fake.features = nil
			case "refresh_failure":
				b.fake.meErr = &api.Problem{Status: 503, Code: "unavailable"}
			}
			m = drain(t, m, m.load(keySession))
			m = press(t, m, "y", "R", "y")
			pending, ok := m.jobs.Pending(operations.RetryJob, "j1")
			if !m.readOnlyJobs() || m.confirm.open() || !ok || pending != held || len(f.Mutations()) != 1 {
				t.Fatalf("withdrawal changed or sent intent: %+v %+v", m.confirm, pending)
			}
			b.fake.features = nativeJobControlFeatures()
			b.fake.permissions = []string{api.PermProcessingRequest}
			b.fake.meErr = nil
			f.SetControls("j1", nativeJob("j1", "queued", false, false).Controls)
			m = drain(t, m, m.load(keySession))
			m = drain(t, m, m.load(keyJobs))
			m = press(t, m, "R", "y")
			calls := f.Mutations()
			if len(calls) != 2 || calls[0] != calls[1] {
				t.Fatalf("restored support lost original retry identity: %+v", calls)
			}
		})
	}
}

func TestNativeJobControlsWithdrawUnsubmittedConfirmationsAndDelayedReviews(t *testing.T) {
	for _, withdrawal := range []string{"capability", "permission"} {
		t.Run(withdrawal, func(t *testing.T) {
			m, f := processingWithFeatures(t, nativeJobControlFeatures(), nativeJob("j1", "failed", false, true))
			m = selectJob(t, m, "j1")
			m = press(t, m, "R")
			if !m.confirm.open() {
				t.Fatal("retry not reviewed")
			}
			replies := messages(t, m.inspectJob("j1", true))
			b := m.backend.(fixtureBackend)
			if withdrawal == "capability" {
				b.fake.features = nativeJobReadFeatures()
			} else {
				b.fake.permissions = []string{api.PermCatalogRead}
			}
			m = drain(t, m, m.load(keySession))
			if m.confirm.open() {
				t.Fatal("unsubmitted confirmation survived withdrawal")
			}
			b.fake.features = nativeJobControlFeatures()
			b.fake.permissions = []string{api.PermProcessingRequest}
			m = drain(t, m, m.load(keySession))
			for _, reply := range replies {
				m, _ = update(t, m, reply)
			}
			m = press(t, m, "y")
			if m.confirm.open() || len(f.Mutations()) != 0 {
				t.Fatal("delayed review reopened a withdrawn action")
			}
		})
	}
}

func TestNativeJobControlsHintsWithdrawAtTheSameJobRevision(t *testing.T) {
	m, f := processingWithFeatures(t, nativeJobControlFeatures(), nativeJob("j1", "failed", false, true))
	m = selectJob(t, m, "j1")
	m = press(t, m, "R")
	replies := messages(t, m.inspectJob("j1", true))
	f.SetControls("j1", nativeJob("j1", "failed", false, false).Controls)
	m = drain(t, m, m.load(keyJobs))
	for _, reply := range replies {
		m, _ = update(t, m, reply)
	}
	m = press(t, m, "y", "R", "y")
	if m.confirm.open() || len(f.Mutations()) != 0 || m.canRetryJob("j1") {
		t.Fatal("stale same-revision hints reopened retry")
	}
}
