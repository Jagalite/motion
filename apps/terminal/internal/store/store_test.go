package store

import (
	"errors"
	"os"
	"path/filepath"
	"testing"
)

func TestCredentialsArePrivateAndServerScoped(t *testing.T) {
	s := Store{Dir: t.TempDir()}
	c := Credential{ServerID: "srv1", DeviceID: "d1", AccessToken: "mdv_x", ExpiresAt: "2027-01-01T00:00:00Z"}
	if err := s.PutCredential(c, ""); !errors.Is(err, ErrNoStore) {
		t.Fatalf("stored without acceptance: %v", err)
	}
	if err := s.PutCredential(c, FileStore); err != nil {
		t.Fatal(err)
	}
	path := filepath.Join(s.Dir, "credentials", "srv1.json")
	info, err := os.Stat(path)
	if err != nil || info.Mode().Perm() != 0o600 {
		t.Fatalf("mode %v %v", info.Mode(), err)
	}
	dir, _ := os.Stat(filepath.Dir(path))
	if dir.Mode().Perm() != 0o700 {
		t.Fatalf("directory mode %v", dir.Mode())
	}
	got, err := s.Credential("srv1")
	if err != nil || *got != c {
		t.Fatalf("%+v %v", got, err)
	}
	if other, err := s.Credential("srv2"); other != nil || err != nil {
		t.Fatalf("credential leaked across servers: %+v %v", other, err)
	}
	if err := os.Chmod(path, 0o644); err != nil {
		t.Fatal(err)
	}
	if _, err := s.Credential("srv1"); err == nil {
		t.Fatal("world-readable credential accepted")
	}
	for _, bad := range []string{"../x", "a/b", ""} {
		if _, err := s.Credential(bad); err == nil {
			t.Errorf("accepted server id %q", bad)
		}
	}
	if err := s.DeleteCredential("srv1"); err != nil {
		t.Fatal(err)
	}
	if got, _ := s.Credential("srv1"); got != nil {
		t.Fatal("not deleted")
	}
}

func TestMismatchedCredentialFileIsRejected(t *testing.T) {
	s := Store{Dir: t.TempDir()}
	if err := s.PutCredential(Credential{ServerID: "srv1", AccessToken: "x"}, FileStore); err != nil {
		t.Fatal(err)
	}
	if err := os.Rename(filepath.Join(s.Dir, "credentials", "srv1.json"), filepath.Join(s.Dir, "credentials", "srv2.json")); err != nil {
		t.Fatal(err)
	}
	if _, err := s.Credential("srv2"); err == nil {
		t.Fatal("credential for another server accepted")
	}
}
