package operations

import "github.com/Jagalite/motion/apps/terminal/internal/api"

// UsesNativeJobs identifies the bounded native job contract even when its
// controls are disabled or unsupported. The read leaf never implies writes.
func UsesNativeJobs(caps api.Capabilities) bool {
	_, reads := caps.Feature("processing.native_job_reads")
	_, controls := caps.Feature("processing.native_preparation_controls")
	return reads || controls
}

// JobControlsSupported is a presentation restriction, not a permission grant.
// Servers predating both native leaves retain their existing job behavior.
func JobControlsSupported(caps api.Capabilities) bool {
	if !UsesNativeJobs(caps) {
		return true
	}
	var f api.Feature
	found := false
	for _, feature := range caps.Features {
		if feature.ID == "processing.native_preparation_controls" {
			if found {
				return false // conflicting/duplicate declarations are not support
			}
			f, found = feature, true
		}
	}
	return found && f.Implemented && f.Enabled &&
		f.Limits["contract_version"] == "1" &&
		f.Limits["operations"] == "cancel,retry" &&
		f.Limits["cohort"] == "native_preparation"
}

// JobActionAvailable interprets only the server's advisory NEW-request hints.
// It deliberately does not reconstruct admission from phase, attempts or IDs.
// Exact replay is separate: an already-submitted intent must retain its key
// and reviewed revision even after these hints change or become false.
func JobActionAvailable(caps api.Capabilities, job api.Job, kind JobActionKind) bool {
	if kind != CancelJob && kind != RetryJob {
		return false
	}
	if !UsesNativeJobs(caps) {
		// A descriptor without a recognized capability is not legacy behavior.
		return job.Controls == nil
	}
	if !JobControlsSupported(caps) || job.Kind != "rendition" {
		return false
	}
	c := job.Controls
	if c == nil || !c.RequestHintsComplete() || c.Family != "native_preparation" || c.Version != 1 || !c.Cohort {
		return false
	}
	if kind == CancelJob {
		return c.CancelRequestAvailable
	}
	return c.RetryRequestAvailable
}
