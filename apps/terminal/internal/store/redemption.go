package store

import (
	"crypto/sha256"
	"encoding/hex"
	"encoding/json"
	"errors"
	"fmt"
	"os"
	"path/filepath"
	"runtime"
)

// RedemptionClaim is written before a request can consume an invitation. The
// invitation itself is never stored. Retrying with the same code reloads the
// exact claim and device name even after credential persistence failed.
type RedemptionClaim struct {
	ClaimID    string `json:"claim_id"`
	DeviceName string `json:"device_name"`
}

func (s Store) redemptionPath(serverID, code string) (string, error) {
	if _, err := s.credentialPath(serverID); err != nil {
		return "", err
	}
	digest := sha256.Sum256([]byte(code))
	return filepath.Join(s.Dir, "credentials", "redemptions", serverID+"-"+hex.EncodeToString(digest[:])+".json"), nil
}
func (s Store) BeginRedemption(serverID, code, accepted string, proposed RedemptionClaim) (RedemptionClaim, error) {
	if accepted != FileStore {
		return RedemptionClaim{}, ErrNoStore
	}
	if runtime.GOOS == "windows" {
		return RedemptionClaim{}, ErrUnsupportedPlatform
	}
	path, err := s.redemptionPath(serverID, code)
	if err != nil {
		return RedemptionClaim{}, err
	}
	if err = os.MkdirAll(filepath.Dir(path), 0700); err != nil {
		return RedemptionClaim{}, err
	}
	// Publish a fully written claim with a no-replace link. Concurrent callers
	// can only observe the same complete identity, never a partially written file.
	f, err := os.CreateTemp(filepath.Dir(path), ".claim-*")
	if err != nil {
		return RedemptionClaim{}, err
	}
	defer os.Remove(f.Name())
	defer f.Close()
	if err = f.Chmod(0600); err != nil {
		return RedemptionClaim{}, err
	}
	data, err := json.Marshal(proposed)
	if err != nil {
		return RedemptionClaim{}, err
	}
	if _, err = f.Write(data); err != nil {
		return RedemptionClaim{}, err
	}
	if err = f.Sync(); err != nil {
		return RedemptionClaim{}, err
	}
	if err = f.Close(); err != nil {
		return RedemptionClaim{}, err
	}
	if err = os.Link(f.Name(), path); errors.Is(err, os.ErrExist) {
		if err = checkPrivate(path); err != nil {
			return RedemptionClaim{}, err
		}
		data, err := os.ReadFile(path)
		if err != nil {
			return RedemptionClaim{}, err
		}
		if err = json.Unmarshal(data, &proposed); err != nil {
			return RedemptionClaim{}, fmt.Errorf("cannot read pending invitation claim: %w", err)
		}
		if proposed.ClaimID == "" {
			return RedemptionClaim{}, errors.New("pending invitation claim has no identity")
		}
	} else if err != nil {
		return RedemptionClaim{}, err
	}

	// Sync each newly created directory entry before any network effect.
	for _, directory := range []string{filepath.Dir(path), filepath.Join(s.Dir, "credentials"), s.Dir, filepath.Dir(s.Dir)} {
		dir, err := os.Open(directory)
		if err != nil {
			return RedemptionClaim{}, err
		}
		err = dir.Sync()
		closeErr := dir.Close()
		if err != nil {
			return RedemptionClaim{}, err
		}
		if closeErr != nil {
			return RedemptionClaim{}, closeErr
		}
	}
	return proposed, nil
}

// Keep successful claims too: a concurrent retry must never generate a fresh
// claim while the first process is persisting its credential. They contain no
// invitation or access token and may be retained as recovery metadata.
