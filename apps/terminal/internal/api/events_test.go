package api

import (
	"context"
	"errors"
	"fmt"
	"net/http"
	"sync"
	"testing"
	"time"
)

func TestFollowResumesFromLastCursorAndStopsOnRevocation(t *testing.T) {
	var mu sync.Mutex
	var resumed []string
	connections := 0
	c, rec := client(t, http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		mu.Lock()
		defer mu.Unlock()
		if r.URL.Path == "/api/v2/me" {
			writeProblem(w, 401, "credential_revoked", "")
			return
		}
		connections++
		resumed = append(resumed, r.Header.Get("Last-Event-ID"))
		if connections == 3 {
			writeProblem(w, 401, "credential_revoked", "")
			return
		}
		w.Header().Set("Content-Type", "text/event-stream")
		if connections == 1 {
			fmt.Fprint(w, ": heartbeat\n\n")
			fmt.Fprint(w, "event: reset\nid: e.d1.1.5\ndata: {\"cursor\":\"e.d1.1.5\",\"kind\":\"reset\",\"resource_type\":\"stream\",\"resource_id\":null,\"resource_revision\":null,\"policy_revision\":\"1\",\"reason\":\"initial_snapshot\"}\n\n")
			fmt.Fprint(w, "event: changed\nid: e.d1.1.6\ndata: {\"cursor\":\"e.d1.1.6\",\"kind\":\"changed\",\"resource_type\":\"profile\",\n")
			fmt.Fprint(w, "data: \"resource_id\":\"default\",\"resource_revision\":null,\"policy_revision\":\"1\",\"reason\":null}\n\n")
			// Disconnect: a server-side failure, not a revocation.
			return
		}
		writeProblem(w, 503, "event_limit", "3")
	}))
	// The first clean close re-checks the credential; let it succeed once.
	var got []Event
	var disconnects []error
	meCalls := 0
	c.HTTP.Transport = roundTripFunc(func(r *http.Request) (*http.Response, error) {
		if r.URL.Path == "/api/v2/me" {
			meCalls++
			if meCalls == 1 {
				return &http.Response{StatusCode: 200, Body: http.NoBody, Header: http.Header{}, Request: r}, nil
			}
		}
		return http.DefaultTransport.RoundTrip(r)
	})
	err := c.Follow(context.Background(), "", func(e Event) error {
		got = append(got, e)
		return nil
	}, func(err error, _ time.Duration) { disconnects = append(disconnects, err) })
	if !IsCode(err, "credential_revoked") {
		t.Fatalf("Follow returned %v", err)
	}
	if len(got) != 2 || got[0].Kind != "reset" || *got[1].ResourceID != "default" {
		t.Fatalf("events %+v", got)
	}
	if resumed[0] != "" || resumed[1] != "e.d1.1.6" || resumed[2] != "e.d1.1.6" {
		t.Fatalf("did not resume from the last cursor: %q", resumed)
	}
	if len(disconnects) != 2 || !errors.Is(disconnects[0], ErrStreamEnded) {
		t.Fatalf("disconnects %v", disconnects)
	}
	if rec.waits[1] != 3*time.Second {
		t.Fatalf("Retry-After ignored: %v", rec.waits)
	}
}

func TestFollowDropsAnInvalidCursorAndStartsFresh(t *testing.T) {
	var seen []string
	c, _ := client(t, http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		seen = append(seen, r.Header.Get("Last-Event-ID"))
		if len(seen) == 1 {
			writeProblem(w, 400, "invalid_cursor", "")
			return
		}
		writeProblem(w, 403, "permission_denied", "")
	}))
	err := c.Follow(context.Background(), "stale", func(Event) error { return nil }, nil)
	if !IsCode(err, "permission_denied") || len(seen) != 2 || seen[1] != "" {
		t.Fatalf("err=%v seen=%q", err, seen)
	}
}

type roundTripFunc func(*http.Request) (*http.Response, error)

func (f roundTripFunc) RoundTrip(r *http.Request) (*http.Response, error) { return f(r) }

func TestFollowReturnsConsumerFailuresWithoutReconnecting(t *testing.T) {
	connections := 0
	c, _ := client(t, http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		connections++
		w.Header().Set("Content-Type", "text/event-stream")
		fmt.Fprint(w, "data: {\"cursor\":\"e.p.0.1\",\"kind\":\"changed\",\"resource_type\":\"profile\",\"resource_id\":\"a\",\"resource_revision\":null,\"policy_revision\":\"0\",\"reason\":null}\n\n")
	}))
	broken := errors.New("stdout closed")
	err := c.Follow(context.Background(), "", func(Event) error { return broken }, nil)
	if !errors.Is(err, broken) || connections != 1 {
		t.Fatalf("err=%v connections=%d", err, connections)
	}
}
