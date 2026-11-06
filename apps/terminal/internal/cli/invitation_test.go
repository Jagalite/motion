package cli

import (
	"bytes"
	"context"
	"encoding/json"
	"fmt"
	"io"
	"net/http"
	"net/http/httptest"
	"os"
	"path/filepath"
	"strings"
	"testing"
	"testing/synctest"
	"time"

	"github.com/Jagalite/motion/apps/terminal/internal/api"
	"github.com/Jagalite/motion/apps/terminal/internal/store"
)

type delayedInvitation struct {
	reader io.Reader
	delay  time.Duration
}

func (r *delayedInvitation) Read(p []byte) (int, error) {
	time.Sleep(r.delay)
	r.delay = 0
	return r.reader.Read(p)
}

type invitationRoundTripFunc func(*http.Request) (*http.Response, error)

func (f invitationRoundTripFunc) RoundTrip(r *http.Request) (*http.Response, error) {
	return f(r)
}

func TestInvitationInputDoesNotConsumeRequestTimeout(t *testing.T) {
	for _, timeout := range []time.Duration{100 * time.Millisecond, 0} {
		t.Run(timeout.String(), func(t *testing.T) {
			synctest.Test(t, func(t *testing.T) {
				// Advance a fake clock during input and keep HTTP in process:
				// scheduler delays and real sockets must not consume the budget.
				const inputDelay = 150 * time.Millisecond
				start := time.Now()
				healthCalls, redemptionCalls := 0, 0
				client := &http.Client{Transport: invitationRoundTripFunc(func(r *http.Request) (*http.Response, error) {
					if r.Body != nil {
						defer r.Body.Close()
					}
					if err := r.Context().Err(); err != nil {
						return nil, err
					}
					deadline, hasDeadline := r.Context().Deadline()
					if timeout == 0 {
						if hasDeadline {
							t.Fatal("--timeout 0 must not set a request deadline")
						}
					} else if !hasDeadline || time.Until(deadline) != timeout {
						t.Fatalf("%s request must get its full %s timeout; deadline = %v", r.URL.Path, timeout, deadline)
					}
					var body string
					switch r.Method + " " + r.URL.Path {
					case "GET /api/v2/system/health":
						healthCalls++
						body = `{"status":"ok","server_id":"timeout-server","server_epoch":"e1"}`
					case "POST /api/v2/auth/invitations/redeem":
						redemptionCalls++
						if elapsed := time.Since(start); elapsed != inputDelay {
							t.Fatalf("redemption started after %s, want input delay %s", elapsed, inputDelay)
						}
						body = `{"device_id":"dev","access_token":"token","expires_at":"2030-01-01T00:00:00Z"}`
					default:
						t.Fatalf("unexpected request: %s %s", r.Method, r.URL.Path)
					}
					return &http.Response{
						StatusCode: http.StatusOK,
						Header:     http.Header{"Content-Type": {"application/json"}},
						Body:       io.NopCloser(strings.NewReader(body)),
					}, nil
				})}
				var out bytes.Buffer
				root := NewRoot(&Env{Out: &out, Err: &out, HTTP: client})
				root.SetIn(&delayedInvitation{reader: strings.NewReader("min_code\n"), delay: inputDelay})
				root.SetArgs([]string{"--server", "http://invitation.test", "--config-dir", t.TempDir(), "--timeout", timeout.String(), "auth", "redeem", "--credential-store", "file"})
				if err := root.ExecuteContext(context.Background()); err != nil {
					t.Fatal(err)
				}
				if healthCalls != 1 || redemptionCalls != 1 {
					t.Fatalf("requests: health = %d, redemption = %d; want one each", healthCalls, redemptionCalls)
				}
			})
		})
	}
}

func TestInvitationRecoversAcrossCommandRestartAfterPersistenceFailure(t *testing.T) {
	dir := t.TempDir()
	var claims []api.RedemptionInput
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		w.Header().Set("Content-Type", "application/json")
		if r.URL.Path == "/api/v2/system/health" {
			fmt.Fprint(w, `{"status":"ok","server_id":"recovery-server","server_epoch":"e1"}`)
			return
		}
		var in api.RedemptionInput
		if err := json.NewDecoder(r.Body).Decode(&in); err != nil {
			t.Error(err)
		}
		claims = append(claims, in)
		if len(claims) == 1 {
			if err := os.Mkdir(filepath.Join(dir, "credentials", "recovery-server.json"), 0700); err != nil {
				t.Error(err)
			}
		}
		fmt.Fprint(w, `{"device_id":"dev","access_token":"recovered-token","expires_at":"2030-01-01T00:00:00Z"}`)
	}))
	defer server.Close()
	run := func(name string) error {
		var out bytes.Buffer
		root := NewRoot(&Env{Out: &out, Err: &out, HTTP: server.Client()})
		root.SetIn(strings.NewReader("min_code\n"))
		root.SetArgs([]string{"--server", server.URL, "--config-dir", dir, "auth", "redeem", "--name", name, "--credential-store", "file"})
		return root.ExecuteContext(context.Background())
	}
	if err := run("Original"); err == nil {
		t.Fatal("expected storage failure")
	}
	if err := os.Remove(filepath.Join(dir, "credentials", "recovery-server.json")); err != nil {
		t.Fatal(err)
	}
	if err := run("Changed"); err != nil {
		t.Fatal(err)
	}
	if len(claims) != 2 || claims[0] != claims[1] {
		t.Fatal("recovery changed request identity")
	}
	saved, err := (store.Store{Dir: dir}).Credential("recovery-server")
	if err != nil || saved == nil || saved.AccessToken != "recovered-token" {
		t.Fatal("missing recovered credential", err)
	}
}
