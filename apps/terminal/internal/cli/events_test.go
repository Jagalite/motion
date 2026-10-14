package cli

import (
	"bytes"
	"context"
	"fmt"
	"net/http"
	"net/http/httptest"
	"os"
	"path/filepath"
	"strings"
	"sync"
	"testing"
	"time"
)

type lockedBuffer struct {
	mu sync.Mutex
	b  bytes.Buffer
}

func (l *lockedBuffer) Write(p []byte) (int, error) {
	l.mu.Lock()
	defer l.mu.Unlock()
	return l.b.Write(p)
}

func (l *lockedBuffer) String() string {
	l.mu.Lock()
	defer l.mu.Unlock()
	return l.b.String()
}

func TestEventsTailWritesVersionedEvents(t *testing.T) {
	resumed := make(chan string, 4)
	srv := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		switch r.URL.Path {
		case "/api/v2/system/health":
			fmt.Fprint(w, `{"status":"ok","server_id":"s1","server_epoch":"e"}`)
		case "/api/v2/events":
			resumed <- r.Header.Get("Last-Event-ID")
			w.Header().Set("Content-Type", "text/event-stream")
			fmt.Fprint(w, "id: c2\ndata: {\"cursor\":\"c2\",\"kind\":\"changed\",\"resource_type\":\"profile\",\"resource_id\":\"p1\",\"policy_revision\":\"1\"}\n\n")
			w.(http.Flusher).Flush()
			<-r.Context().Done()
		default:
			w.WriteHeader(404)
		}
	}))
	defer srv.Close()
	dir := t.TempDir()
	token := filepath.Join(dir, "operator")
	if err := os.WriteFile(token, []byte(fixtureToken), 0600); err != nil {
		t.Fatal(err)
	}
	out := &lockedBuffer{}
	ctx, cancel := context.WithCancel(context.Background())
	done := make(chan error, 1)
	go func() {
		env := &Env{Out: out, Err: &bytes.Buffer{}, HTTP: srv.Client()}
		done <- Execute(ctx, env, []string{"--server", srv.URL, "--config-dir", dir, "--operator-token-file", token, "--json", "events", "tail", "--after", "c1"})
	}()
	if after := <-resumed; after != "c1" {
		t.Fatalf("resumed after %q", after)
	}
	for i := 0; i < 100 && !strings.Contains(out.String(), `"cursor":"c2"`); i++ {
		time.Sleep(10 * time.Millisecond)
	}
	cancel()
	err := <-done
	if ExitCode(err) != ExitInterrupted || !strings.Contains(out.String(), `{"schema":"motion.cli.v1","event":{"cursor":"c2"`) {
		t.Fatalf("exit %d output %s", ExitCode(err), out.String())
	}
}
