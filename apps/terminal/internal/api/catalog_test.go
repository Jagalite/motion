package api

import (
	"context"
	"encoding/json"
	"io"
	"net/http"
	"strings"
	"testing"
)

func TestCatalogEscapesQueryAndPreservesCreateRetry(t *testing.T) {
	var attempts int
	var bodies []string
	c, _ := client(t, http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		switch r.Method + " " + r.URL.Path {
		case "GET /api/v2/catalog/search":
			if r.URL.Query().Get("q") != "猫 & x?#" || r.URL.Query().Get("limit") != "7" {
				t.Errorf("query %v", r.URL.Query())
			}
			io.WriteString(w, `{"items":[],"next_cursor":null,"read_revision":"1","event_cursor":"e"}`)
		case "POST /api/v2/libraries":
			attempts++
			b, _ := io.ReadAll(r.Body)
			bodies = append(bodies, string(b))
			if r.Header.Get("Idempotency-Key") != "create-library" {
				t.Error("missing retry identity")
			}
			if attempts == 1 {
				writeProblem(w, 503, "database_busy", "")
				return
			}
			w.Header().Set("ETag", `"r-1"`)
			w.WriteHeader(201)
			io.WriteString(w, `{"id":"lib","revision":"1","name":"Movies","kind":"movies","language":"en","source_ids":["source"],"availability":"unknown"}`)
		default:
			t.Errorf("unexpected %s %s", r.Method, r.URL)
			w.WriteHeader(404)
		}
	}))
	if _, err := c.SearchCatalog(context.Background(), "猫 & x?#", "", 7); err != nil {
		t.Fatal(err)
	}
	v, err := c.CreateLibrary(context.Background(), LibraryInput{Name: "Movies", Kind: "movies", Language: "en", SourceIDs: []string{"source"}}, "create-library")
	if err != nil || v.Value.ID != "lib" || v.ETag != `"r-1"` {
		t.Fatalf("%+v %v", v, err)
	}
	if attempts != 2 || bodies[0] != bodies[1] {
		t.Fatalf("retry changed operation %v", bodies)
	}
}
func TestOperationsUseContractPathsAndStringCounters(t *testing.T) {
	c, _ := client(t, http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		switch r.URL.Path {
		case "/api/v2/libraries/lib/scans":
			var in map[string]any
			json.NewDecoder(r.Body).Decode(&in)
			if in["mode"] != "verify" || in["require_complete"] != true {
				t.Errorf("input %v", in)
			}
			if _, exists := in["source_ids"]; exists {
				t.Error("omitted scope became empty source selection")
			}
			if r.Header.Get("Idempotency-Key") != "scan-key" {
				t.Error("missing scan key")
			}
			w.WriteHeader(202)
			io.WriteString(w, `{"id":"scan","revision":"1","sources":[],"job_ids":[],"status":"queued"}`)
		case "/api/v2/jobs/job/cancel":
			if r.Method != "POST" || r.Header.Get("Idempotency-Key") != "cancel-key" {
				t.Error("incorrect cancel request")
			}
			io.WriteString(w, `{"id":"job","revision":"2","kind":"scan","phase":"cancelling","attempt_generation":"1","progress":null,"error_code":null,"result_ids":[],"requester_id":"device"}`)
		case "/api/v2/admin/diagnostics":
			io.WriteString(w, `{"server_epoch":"e","uptime_seconds":"18446744073709551615","active_deliveries":"0","queued_jobs":"0","running_jobs":"0","db_busy_count":"0","worker_errors":[]}`)
		default:
			t.Errorf("unexpected %s", r.URL)
			w.WriteHeader(404)
		}
	}))
	if _, err := c.CreateScan(context.Background(), "lib", ScanInput{Mode: "verify", RequireComplete: true}, "scan-key"); err != nil {
		t.Fatal(err)
	}
	if j, err := c.CancelJob(context.Background(), "job", "cancel-key"); err != nil || j.Value.Phase != "cancelling" {
		t.Fatalf("%+v %v", j, err)
	}
	d, err := c.Diagnostics(context.Background())
	if err != nil || !strings.HasPrefix(d.UptimeSeconds, "18446744") {
		t.Fatalf("%+v %v", d, err)
	}
}

func TestScanEmptySelectionIsDifferentFromAllSources(t *testing.T) {
	all, err := json.Marshal(ScanInput{Mode: "incremental"})
	if err != nil {
		t.Fatal(err)
	}
	empty := []string{}
	selected, err := json.Marshal(ScanInput{Mode: "incremental", SourceIDs: &empty})
	if err != nil {
		t.Fatal(err)
	}
	if strings.Contains(string(all), "source_ids") || !strings.Contains(string(selected), `"source_ids":[]`) {
		t.Fatalf("all=%s selected=%s", all, selected)
	}
}
