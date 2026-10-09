package api

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"net/http"
	"net/http/httptest"
	"strings"
	"sync"
	"testing"
	"time"
)

type recorder struct {
	mu    sync.Mutex
	waits []time.Duration
}

func (r *recorder) sleep(_ context.Context, d time.Duration) error {
	r.mu.Lock()
	defer r.mu.Unlock()
	r.waits = append(r.waits, d)
	return nil
}

func client(t *testing.T, h http.Handler) (*Client, *recorder) {
	t.Helper()
	srv := httptest.NewServer(h)
	t.Cleanup(srv.Close)
	c, err := New(srv.URL, srv.Client())
	if err != nil {
		t.Fatal(err)
	}
	c.Token = func() string { return "mdv_secret" }
	r := &recorder{}
	c.Sleep = r.sleep
	return c, r
}

func writeProblem(w http.ResponseWriter, status int, code string, retryAfter string) {
	w.Header().Set("Content-Type", "application/problem+json")
	if retryAfter != "" {
		w.Header().Set("Retry-After", retryAfter)
	}
	w.WriteHeader(status)
	fmt.Fprintf(w, `{"type":"/problems/%s","title":"t","status":%d,"code":%q,"detail":"d","request_id":"req-1","retryable":true}`, code, status, code)
}

func TestRetriesReuseTheIdempotencyKeyAndBody(t *testing.T) {
	var keys, bodies []string
	c, rec := client(t, http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		b, _ := io.ReadAll(r.Body)
		keys = append(keys, r.Header.Get("Idempotency-Key"))
		bodies = append(bodies, string(b))
		if len(keys) < 3 {
			writeProblem(w, 503, "database_busy", "2")
			return
		}
		w.Header().Set("ETag", `"r-1"`)
		w.WriteHeader(201)
		fmt.Fprint(w, `{"id":"p1","name":"Kids","revision":"1"}`)
	}))
	key := NewIdempotencyKey()
	p, err := c.CreateProfile(context.Background(), "Kids", key)
	if err != nil {
		t.Fatal(err)
	}
	if p.ETag != `"r-1"` || p.Value.ID != "p1" {
		t.Fatalf("unexpected %+v", p)
	}
	if len(keys) != 3 || keys[0] != key || keys[1] != key || keys[2] != key {
		t.Fatalf("retry identity not preserved: %v", keys)
	}
	if bodies[0] != bodies[2] {
		t.Fatalf("retried body changed: %v", bodies)
	}
	if len(rec.waits) != 2 || rec.waits[0] != 2*time.Second {
		t.Fatalf("Retry-After not honored: %v", rec.waits)
	}
}

func TestUnkeyedWritesAndConflictsAreNotRetried(t *testing.T) {
	calls := 0
	c, _ := client(t, http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		calls++
		if r.URL.Path == "/api/v2/auth/pairings" {
			writeProblem(w, 503, "database_busy", "")
			return
		}
		writeProblem(w, 409, "idempotency_key_reused", "")
	}))
	if _, err := c.CreatePairing(context.Background(), PairingRequest{"a", "b"}); err == nil {
		t.Fatal("expected error")
	}
	if calls != 1 {
		t.Fatalf("unkeyed POST retried %d times", calls)
	}
	calls = 0
	_, err := c.CreateProfile(context.Background(), "x", NewIdempotencyKey())
	var p *Problem
	if !errors.As(err, &p) || p.Code != "idempotency_key_reused" || p.Status != 409 || p.RequestID != "req-1" {
		t.Fatalf("problem not decoded: %#v", err)
	}
	if calls != 1 {
		t.Fatalf("conflict retried %d times", calls)
	}
}

func TestNonProblemErrorsAndRequestIDFallback(t *testing.T) {
	c, _ := client(t, http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		w.Header().Set("X-Request-ID", "req-9")
		w.WriteHeader(502)
		fmt.Fprint(w, "<html>bad gateway</html>")
	}))
	c.MaxAttempts = 1
	_, err := c.Me(context.Background())
	var p *Problem
	if !errors.As(err, &p) || p.Code != "http_502" || p.RequestID != "req-9" {
		t.Fatalf("got %#v", err)
	}
}

func TestCredentialsAreNotForwardedAcrossRedirects(t *testing.T) {
	leaked := make(chan string, 1)
	other := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		leaked <- r.Header.Get("Authorization")
	}))
	defer other.Close()
	c, _ := client(t, http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		http.Redirect(w, r, other.URL+"/api/v2/me", http.StatusTemporaryRedirect)
	}))
	c.MaxAttempts = 1
	if _, err := c.Me(context.Background()); err == nil {
		t.Fatal("redirect should not be followed")
	}
	select {
	case got := <-leaked:
		t.Fatalf("redirect followed; Authorization=%q", got)
	default:
	}
}

func TestAuthenticatedCallWithoutCredentialFailsLocally(t *testing.T) {
	called := false
	c, _ := client(t, http.HandlerFunc(func(http.ResponseWriter, *http.Request) { called = true }))
	c.Token = func() string { return "" }
	_, err := c.Me(context.Background())
	if !IsCode(err, "not_signed_in") || called {
		t.Fatalf("err=%v called=%v", err, called)
	}
}

func TestUnavailableIsRetriedThenReported(t *testing.T) {
	c, err := New("http://127.0.0.1:1", nil)
	if err != nil {
		t.Fatal(err)
	}
	rec := &recorder{}
	c.Sleep = rec.sleep
	_, err = c.Health(context.Background())
	var u *UnavailableError
	if !errors.As(err, &u) || len(rec.waits) != 2 {
		t.Fatalf("err=%v waits=%v", err, rec.waits)
	}
}

func TestCancellationStopsRetrying(t *testing.T) {
	ctx, cancel := context.WithCancel(context.Background())
	c, _ := client(t, http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		cancel()
		writeProblem(w, 503, "busy", "")
	}))
	c.Sleep = sleep
	_, err := c.Me(ctx)
	if !errors.Is(err, context.Canceled) {
		t.Fatalf("got %v", err)
	}
}

func TestServerURLValidation(t *testing.T) {
	for _, bad := range []string{"ftp://x", "http://", "http://u:p@host", "http://host/path", "http://host?q=1"} {
		if _, err := New(bad, nil); err == nil {
			t.Errorf("accepted %q", bad)
		}
	}
	if _, err := New("http://127.0.0.1:8787/", nil); err != nil {
		t.Error(err)
	}
}

func TestPagesDecodeContractShape(t *testing.T) {
	c, _ := client(t, http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		if r.URL.Query().Get("limit") != "2" || r.URL.Query().Get("cursor") != "abc" {
			t.Errorf("query %v", r.URL.RawQuery)
		}
		json.NewEncoder(w).Encode(map[string]any{
			"items":       []map[string]any{{"id": "d1", "name": "TV", "revision": "1", "profile_ids": []string{}, "permissions": []string{"catalog:read"}, "revoked": false}},
			"next_cursor": nil, "read_revision": "7", "event_cursor": "e.operator.0.7",
		})
	}))
	page, err := c.ListDevices(context.Background(), "abc", 2)
	if err != nil || len(page.Items) != 1 || page.NextCursor != nil || page.EventCursor != "e.operator.0.7" {
		t.Fatalf("%+v %v", page, err)
	}
	if !strings.Contains(page.Items[0].Permissions[0], "catalog") {
		t.Fatal("permissions")
	}
}
