package store

import (
	"fmt"
	"os"
	"path/filepath"
	"testing"
)

func TestRedemptionClaimSurvivesCredentialWriteFailure(t *testing.T) {
	s := Store{Dir: t.TempDir()}
	original := RedemptionClaim{ClaimID: "claim-one", DeviceName: "First name"}
	if _, err := s.BeginRedemption("server", "secret-code", FileStore, original); err != nil {
		t.Fatal(err)
	}
	if err := os.Mkdir(filepath.Join(s.Dir, "credentials", "server.json"), 0700); err != nil {
		t.Fatal(err)
	}
	if err := s.PutCredential(Credential{ServerID: "server", AccessToken: "secret"}, FileStore); err == nil {
		t.Fatal("expected persistence failure")
	}
	replay, err := s.BeginRedemption("server", "secret-code", FileStore, RedemptionClaim{ClaimID: "different", DeviceName: "Changed"})
	if err != nil || replay != original {
		t.Fatalf("lost recoverable claim: %#v %v", replay, err)
	}
	if _, err := s.BeginRedemption("server", "secret-code", "", original); err != ErrNoStore {
		t.Fatal(err)
	}
}
func TestRedemptionRejectsUnusableStoreBeforeRequest(t *testing.T) {
	dir := t.TempDir()
	if err := os.WriteFile(filepath.Join(dir, "credentials"), []byte("block"), 0600); err != nil {
		t.Fatal(err)
	}
	if _, err := (Store{Dir: dir}).BeginRedemption("server", "code", FileStore, RedemptionClaim{ClaimID: "one"}); err == nil {
		t.Fatal("accepted unusable store")
	}
}

func TestConcurrentRedemptionsPublishOneCompleteClaim(t *testing.T) {
	s := Store{Dir: t.TempDir()}
	type result struct {
		claim RedemptionClaim
		err   error
	}
	results := make(chan result, 16)
	for i := 0; i < 16; i++ {
		go func(i int) {
			c, e := s.BeginRedemption("server", "same-code", FileStore, RedemptionClaim{ClaimID: fmt.Sprintf("claim-%d", i), DeviceName: "Device"})
			results <- result{c, e}
		}(i)
	}
	first := <-results
	if first.err != nil {
		t.Fatal(first.err)
	}
	for i := 1; i < 16; i++ {
		r := <-results
		if r.err != nil || r.claim != first.claim {
			t.Fatalf("different or incomplete claim: %#v %v", r.claim, r.err)
		}
	}
}
