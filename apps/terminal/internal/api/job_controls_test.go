package api

import (
	"encoding/json"
	"testing"
)

func TestJobControlsAreAdditiveVersionedObservations(t *testing.T) {
	for _, body := range []string{`{"id":"old"}`, `{"id":"old","controls":null}`} {
		var job Job
		if err := json.Unmarshal([]byte(body), &job); err != nil || job.Controls != nil {
			t.Fatalf("older job response: %+v %v", job, err)
		}
	}
	var job Job
	body := `{"id":"native","kind":"rendition","controls":{"family":"native_preparation","version":1,"cohort":true,"cancel_request_available":false,"retry_request_available":true}}`
	if err := json.Unmarshal([]byte(body), &job); err != nil {
		t.Fatal(err)
	}
	c := job.Controls
	if c == nil || !c.RequestHintsComplete() || c.Family != "native_preparation" || c.Version != 1 || !c.Cohort || c.CancelRequestAvailable || !c.RetryRequestAvailable {
		t.Fatalf("controls lost: %+v", c)
	}
	for _, controls := range []string{`{"version":"1"}`, `{"cohort":"true"}`, `{"retry_request_available":1}`} {
		if err := json.Unmarshal([]byte(`{"controls":`+controls+`}`), &job); err == nil {
			t.Fatalf("malformed controls accepted: %s", controls)
		}
	}
}

func TestJobControlsRequireBothExplicitWireHintBooleans(t *testing.T) {
	for _, hints := range []string{
		``,
		`,"retry_request_available":true`,
		`,"cancel_request_available":true`,
		`,"cancel_request_available":null,"retry_request_available":true`,
		`,"cancel_request_available":true,"retry_request_available":null`,
	} {
		var job Job
		body := `{"controls":{"family":"native_preparation","version":1,"cohort":true` + hints + `}}`
		if err := json.Unmarshal([]byte(body), &job); err != nil {
			t.Fatal(err)
		}
		if job.Controls == nil || job.Controls.RequestHintsComplete() {
			t.Fatalf("partial controls considered complete: %s", body)
		}
		encoded, err := json.Marshal(job)
		if err != nil {
			t.Fatal(err)
		}
		var roundTrip Job
		if err := json.Unmarshal(encoded, &roundTrip); err != nil || roundTrip.Controls != nil {
			t.Fatalf("serialization manufactured valid hints: %s %v", encoded, err)
		}
	}
	local := JobControls{Family: "native_preparation", Version: 1, Cohort: true}
	if !local.RequestHintsComplete() {
		t.Fatal("typed false hints treated as incomplete")
	}
	encoded, err := json.Marshal(local)
	if err != nil {
		t.Fatal(err)
	}
	var decoded JobControls
	if err := json.Unmarshal(encoded, &decoded); err != nil || !decoded.RequestHintsComplete() || decoded.CancelRequestAvailable || decoded.RetryRequestAvailable {
		t.Fatalf("explicit false hints lost: %s %v", encoded, err)
	}
}
