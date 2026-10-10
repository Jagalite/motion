package cli

import (
	"bytes"
	"context"
	"encoding/json"
	"fmt"
	"net/http"
	"net/http/httptest"
	"os"
	"path/filepath"
	"testing"
)

func TestJobsRetryCommandUsesFreshOperationIdentityAndReportsConflicts(t *testing.T) {
	keys := []string{}
	conflict := false
	srv := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		switch r.URL.Path {
		case "/api/v2/system/health":
			fmt.Fprint(w, `{"status":"ok","server_id":"mock","server_epoch":"e"}`)
		case "/api/v2/jobs/failed/retry":
			if r.Method != "POST" || r.Header.Get("Authorization") != "Bearer 01234567890123456789012345678901" {
				t.Error("incorrect job retry request")
			}
			keys = append(keys, r.Header.Get("Idempotency-Key"))
			if conflict {
				w.Header().Set("Content-Type", "application/problem+json")
				w.WriteHeader(409)
				fmt.Fprint(w, `{"type":"/problems/not_retryable","title":"Conflict","status":409,"code":"not_retryable","request_id":"r","retryable":false}`)
				return
			}
			fmt.Fprint(w, `{"id":"failed","revision":"2","kind":"scan","phase":"queued","attempt_generation":"1","result_ids":[],"requester_id":"operator"}`)
		default:
			t.Errorf("unexpected request: %s", r.URL)
			w.WriteHeader(404)
		}
	}))
	defer srv.Close()
	dir := t.TempDir()
	token := filepath.Join(dir, "operator")
	if err := os.WriteFile(token, []byte("01234567890123456789012345678901"), 0600); err != nil {
		t.Fatal(err)
	}
	invoke := func() (string, error) {
		var out, stderr bytes.Buffer
		env := &Env{Out: &out, Err: &stderr, HTTP: srv.Client()}
		err := Execute(context.Background(), env, []string{"--server", srv.URL, "--config-dir", dir, "--operator-token-file", token, "--json", "jobs", "retry", "failed"})
		return out.String(), err
	}
	out, err := invoke()
	if err != nil {
		t.Fatal(err)
	}
	var result struct {
		Schema string
		Data   struct{ ID, Phase string }
	}
	if err := json.Unmarshal([]byte(out), &result); err != nil || result.Schema != Schema || result.Data.ID != "failed" || result.Data.Phase != "queued" {
		t.Fatalf("output=%s err=%v", out, err)
	}
	conflict = true
	out, err = invoke()
	if ExitCode(err) != ExitConflict || out != "" {
		t.Fatalf("output=%s err=%v", out, err)
	}
	if len(keys) != 2 || keys[0] == "" || keys[1] == "" || keys[0] == keys[1] {
		t.Fatalf("keys=%v", keys)
	}
}
