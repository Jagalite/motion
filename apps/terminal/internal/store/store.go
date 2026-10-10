// Package store keeps client configuration and device credentials.
//
// Credentials are namespaced by server_id: a credential is only ever sent to
// the server whose unauthenticated health check reports the same server_id,
// so pointing a URL at a different server never leaks it. No OS keychain
// integration exists yet; the only store is a 0600 file, which the user must
// accept explicitly (`--credential-store file`).
package store

import (
	"encoding/json"
	"errors"
	"fmt"
	"os"
	"path/filepath"
	"regexp"
	"runtime"
)

const FileStore = "file"

// ErrNoStore means the user has not accepted a credential store.
var ErrNoStore = errors.New("no supported OS credential store; rerun with --credential-store file to keep the credential in a 0600 file")

var validID = regexp.MustCompile(`^[A-Za-z0-9_-]{1,128}$`)

type Server struct {
	URL      string `json:"url"`
	ServerID string `json:"server_id"`
}

type Config struct {
	Default         string            `json:"default,omitempty"`
	Servers         map[string]Server `json:"servers,omitempty"`
	CredentialStore string            `json:"credential_store,omitempty"`
}

type Credential struct {
	ServerID    string `json:"server_id"`
	DeviceID    string `json:"device_id"`
	AccessToken string `json:"access_token"`
	ExpiresAt   string `json:"expires_at"`
}

// Store is rooted at a directory, normally the user's config directory.
type Store struct{ Dir string }

func Default() (Store, error) {
	if dir := os.Getenv("MOTION_CONFIG_DIR"); dir != "" {
		return Store{Dir: dir}, nil
	}
	base, err := os.UserConfigDir()
	if err != nil {
		return Store{}, err
	}
	return Store{Dir: filepath.Join(base, "motion")}, nil
}

func (s Store) configPath() string { return filepath.Join(s.Dir, "config.json") }

func (s Store) credentialPath(serverID string) (string, error) {
	if !validID.MatchString(serverID) {
		return "", fmt.Errorf("invalid server_id %q", serverID)
	}
	return filepath.Join(s.Dir, "credentials", serverID+".json"), nil
}

func (s Store) Load() (Config, error) {
	var c Config
	data, err := os.ReadFile(s.configPath())
	if errors.Is(err, os.ErrNotExist) {
		return Config{Servers: map[string]Server{}}, nil
	}
	if err != nil {
		return c, err
	}
	if err := json.Unmarshal(data, &c); err != nil {
		return c, fmt.Errorf("%s: %w", s.configPath(), err)
	}
	if c.Servers == nil {
		c.Servers = map[string]Server{}
	}
	return c, nil
}

func (s Store) Save(c Config) error {
	data, err := json.MarshalIndent(c, "", "  ")
	if err != nil {
		return err
	}
	return writePrivate(s.configPath(), append(data, '\n'))
}

// Credential returns the stored credential for a server, if any.
func (s Store) Credential(serverID string) (*Credential, error) {
	if runtime.GOOS == "windows" {
		return nil, ErrUnsupportedPlatform
	}
	path, err := s.credentialPath(serverID)
	if err != nil {
		return nil, err
	}
	if err := checkPrivate(path); err != nil {
		return nil, err
	}
	data, err := os.ReadFile(path)
	if errors.Is(err, os.ErrNotExist) {
		return nil, nil
	}
	if err != nil {
		return nil, err
	}
	var c Credential
	if err := json.Unmarshal(data, &c); err != nil {
		return nil, fmt.Errorf("%s: %w", path, err)
	}
	if c.ServerID != serverID {
		return nil, fmt.Errorf("%s belongs to server %q, not %q", path, c.ServerID, serverID)
	}
	return &c, nil
}

// ErrUnsupportedPlatform: a 0600 mode does not make a file private on Windows.
var ErrUnsupportedPlatform = errors.New("the file credential store is not supported on Windows: it cannot guarantee a user-only ACL")

func (s Store) PutCredential(c Credential, accepted string) error {
	if accepted != FileStore {
		return ErrNoStore
	}
	if runtime.GOOS == "windows" {
		return ErrUnsupportedPlatform
	}
	path, err := s.credentialPath(c.ServerID)
	if err != nil {
		return err
	}
	data, err := json.MarshalIndent(c, "", "  ")
	if err != nil {
		return err
	}
	return writePrivate(path, append(data, '\n'))
}

func (s Store) DeleteCredential(serverID string) error {
	path, err := s.credentialPath(serverID)
	if err != nil {
		return err
	}
	if err := os.Remove(path); err != nil && !errors.Is(err, os.ErrNotExist) {
		return err
	}
	return nil
}

// checkPrivate refuses credential files readable by other users.
func checkPrivate(path string) error {
	info, err := os.Stat(path)
	if errors.Is(err, os.ErrNotExist) {
		return nil
	}
	if err != nil {
		return err
	}
	if runtime.GOOS != "windows" && info.Mode().Perm()&0o077 != 0 {
		return fmt.Errorf("%s is accessible by other users (mode %v); run chmod 600", path, info.Mode().Perm())
	}
	return nil
}

// writePrivate replaces a file atomically with mode 0600 in a 0700 directory.
func writePrivate(path string, data []byte) error {
	if err := os.MkdirAll(filepath.Dir(path), 0o700); err != nil {
		return err
	}
	tmp, err := os.CreateTemp(filepath.Dir(path), ".tmp-*")
	if err != nil {
		return err
	}
	defer os.Remove(tmp.Name())
	if err := tmp.Chmod(0o600); err != nil {
		tmp.Close()
		return err
	}
	if _, err := tmp.Write(data); err != nil {
		tmp.Close()
		return err
	}
	if err := tmp.Sync(); err != nil {
		tmp.Close()
		return err
	}
	if err := tmp.Close(); err != nil {
		return err
	}
	return os.Rename(tmp.Name(), path)
}
