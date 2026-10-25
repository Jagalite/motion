package operations_test

import (
	"encoding/json"
	"testing"

	"github.com/Jagalite/motion/apps/terminal/internal/api"
	"github.com/Jagalite/motion/apps/terminal/internal/operations"
)

func nativeControlCaps() api.Capabilities {
	return api.Capabilities{Features: []api.Feature{
		{ID: "processing.native_job_reads", Implemented: true, Enabled: true},
		{ID: "processing.native_preparation_controls", Implemented: true, Enabled: true,
			Limits: map[string]string{"contract_version": "1", "operations": "cancel,retry", "cohort": "native_preparation"}},
	}}
}

func nativeControlJob() api.Job {
	return api.Job{ID: "native", Kind: "rendition", Phase: "failed", Controls: &api.JobControls{
		Family: "native_preparation", Version: 1, Cohort: true, CancelRequestAvailable: true, RetryRequestAvailable: true,
	}}
}

func TestNativeJobControlCapabilityIsSeparateAndFailClosed(t *testing.T) {
	for _, change := range []struct {
		name string
		edit func(*api.Capabilities)
	}{
		{"read_only", func(c *api.Capabilities) { c.Features = c.Features[:1] }},
		{"disabled", func(c *api.Capabilities) { c.Features[1].Enabled = false }},
		{"unimplemented", func(c *api.Capabilities) { c.Features[1].Implemented = false }},
		{"unknown_version", func(c *api.Capabilities) { c.Features[1].Limits["contract_version"] = "2" }},
		{"missing_limits", func(c *api.Capabilities) { c.Features[1].Limits = nil }},
		{"other_cohort", func(c *api.Capabilities) { c.Features[1].Limits["cohort"] = "all_jobs" }},
		{"other_operations", func(c *api.Capabilities) { c.Features[1].Limits["operations"] = "retry" }},
		{"duplicate_enabled_then_disabled", func(c *api.Capabilities) {
			duplicate := c.Features[1]
			duplicate.Enabled = false
			c.Features = append(c.Features, duplicate)
		}},
		{"duplicate_disabled_then_enabled", func(c *api.Capabilities) {
			duplicate := c.Features[1]
			c.Features[1].Enabled = false
			c.Features = append(c.Features, duplicate)
		}},
		{"duplicate_enabled", func(c *api.Capabilities) { c.Features = append(c.Features, c.Features[1]) }},
	} {
		t.Run(change.name, func(t *testing.T) {
			caps := nativeControlCaps()
			change.edit(&caps)
			if !operations.UsesNativeJobs(caps) || operations.JobControlsSupported(caps) {
				t.Fatal("unsupported native capability treated as legacy or writable")
			}
			for _, kind := range []operations.JobActionKind{operations.CancelJob, operations.RetryJob} {
				if operations.JobActionAvailable(caps, nativeControlJob(), kind) {
					t.Fatalf("unsupported %s offered", kind)
				}
			}
		})
	}
	caps := nativeControlCaps()
	if !operations.JobControlsSupported(caps) {
		t.Fatal("bounded native capability unavailable")
	}
	legacy := api.Capabilities{}
	if operations.UsesNativeJobs(legacy) || !operations.JobControlsSupported(legacy) || !operations.JobActionAvailable(legacy, api.Job{Kind: "scan"}, operations.CancelJob) {
		t.Fatal("older-server behavior changed")
	}
	if operations.JobActionAvailable(legacy, nativeControlJob(), operations.CancelJob) {
		t.Fatal("descriptor without capability treated as legacy")
	}
}

func TestNativeJobControlPartialWireHintsFailClosedForBothActions(t *testing.T) {
	caps := nativeControlCaps()
	for _, hints := range []string{
		``,
		`,"retry_request_available":true`,
		`,"cancel_request_available":true`,
		`,"cancel_request_available":null,"retry_request_available":true`,
		`,"cancel_request_available":true,"retry_request_available":null`,
	} {
		var job api.Job
		body := `{"id":"native","kind":"rendition","controls":{"family":"native_preparation","version":1,"cohort":true` + hints + `}}`
		if err := json.Unmarshal([]byte(body), &job); err != nil {
			t.Fatal(err)
		}
		if operations.JobActionAvailable(caps, job, operations.CancelJob) || operations.JobActionAvailable(caps, job, operations.RetryJob) {
			t.Fatalf("partial wire descriptor offered an action: %s", body)
		}
	}
}

func TestNativeJobControlOffersRequireTheExactRowContract(t *testing.T) {
	caps := nativeControlCaps()
	for _, change := range []struct {
		name string
		edit func(*api.Job)
	}{
		{"missing", func(j *api.Job) { j.Controls = nil }},
		{"history", func(j *api.Job) { j.Controls.Cohort = false }},
		{"unknown_family", func(j *api.Job) { j.Controls.Family = "generic_jobs" }},
		{"unknown_version", func(j *api.Job) { j.Controls.Version = 2 }},
		{"missing_version", func(j *api.Job) { j.Controls.Version = 0 }},
		{"scan", func(j *api.Job) { j.Kind = "scan" }},
		{"preview", func(j *api.Job) { j.Kind = "preview" }},
		{"metadata", func(j *api.Job) { j.Kind = "metadata" }},
		{"backup", func(j *api.Job) { j.Kind = "backup" }},
	} {
		t.Run(change.name, func(t *testing.T) {
			job := nativeControlJob()
			change.edit(&job)
			if operations.JobActionAvailable(caps, job, operations.CancelJob) || operations.JobActionAvailable(caps, job, operations.RetryJob) {
				t.Fatalf("unsupported row offered controls: %+v", job)
			}
		})
	}
	job := nativeControlJob()
	job.Controls.CancelRequestAvailable = false
	if operations.JobActionAvailable(caps, job, operations.CancelJob) || !operations.JobActionAvailable(caps, job, operations.RetryJob) {
		t.Fatal("cancel and retry hints conflated")
	}
	job.Controls.RetryRequestAvailable = false
	if operations.JobActionAvailable(caps, job, operations.RetryJob) {
		t.Fatal("phase alone enabled retry")
	}
	if operations.JobActionAvailable(caps, nativeControlJob(), "publish") {
		t.Fatal("unknown action enabled")
	}
}
