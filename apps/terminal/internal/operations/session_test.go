package operations_test

import (
	"context"
	"errors"
	"net/http"
	"net/http/httptest"
	"testing"

	"github.com/Jagalite/motion/apps/terminal/internal/api"
	"github.com/Jagalite/motion/apps/terminal/internal/operations"
	"github.com/Jagalite/motion/apps/terminal/internal/operations/opstest"
	"github.com/Jagalite/motion/apps/terminal/internal/store"
)

func TestConnectBindsCredentialsToTheVerifiedIdentity(t *testing.T) {
	jobs := opstest.NewJobs(api.Job{ID: "j1", Phase: "running"})
	auth := []string{}
	serverID := "s1"
	srv := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		auth = append(auth, r.Header.Get("Authorization"))
		jobs.Handler(serverID).ServeHTTP(w, r)
	}))
	defer srv.Close()
	dir := t.TempDir()
	st := store.Store{Dir: dir}
	if err := st.Save(store.Config{Default: srv.URL, CredentialStore: store.FileStore, Servers: map[string]store.Server{srv.URL: {URL: srv.URL, ServerID: "s1"}}}); err != nil {
		t.Fatal(err)
	}
	if err := st.PutCredential(store.Credential{ServerID: "s1", DeviceID: "d", AccessToken: "mdv_secret"}, store.FileStore); err != nil {
		t.Fatal(err)
	}
	ctx := context.Background()
	s, err := operations.Connect(ctx, operations.Settings{ConfigDir: dir, HTTP: srv.Client()})
	if err != nil || s.URL != srv.URL || s.Credential == nil {
		t.Fatalf("%+v %v", s, err)
	}
	if _, err := operations.InspectJob(ctx, s, "j1"); err != nil || auth[len(auth)-1] != "Bearer mdv_secret" {
		t.Fatalf("%v %v", err, auth)
	}
	serverID, auth = "impostor", nil
	if _, err := operations.Connect(ctx, operations.Settings{ConfigDir: dir, HTTP: srv.Client()}); !errors.Is(err, operations.ErrServerIdentity) {
		t.Fatalf("changed identity accepted: %v", err)
	}
	for _, a := range auth {
		if a != "" {
			t.Fatalf("credential sent to a changed identity: %q", a)
		}
	}
	var invalid *operations.InvalidInputError
	if _, err := operations.Connect(ctx, operations.Settings{ConfigDir: t.TempDir()}); !errors.As(err, &invalid) {
		t.Fatalf("no server: %v", err)
	}
}
