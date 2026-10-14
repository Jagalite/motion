package operations

import (
	"context"
	"errors"
	"fmt"
	"slices"
	"strings"
	"sync"
	"time"

	"github.com/Jagalite/motion/apps/terminal/internal/api"
	"github.com/Jagalite/motion/apps/terminal/internal/store"
)

// PrincipalReader reads the authenticated principal.
type PrincipalReader interface {
	Me(context.Context) (api.Principal, error)
}

// CapabilityReader reads the server's capabilities.
type CapabilityReader interface {
	Capabilities(context.Context) (api.Capabilities, error)
}

// ServerStatus is the verified server identity and, when signed in, its
// capabilities.
type ServerStatus struct {
	URL          string            `json:"url"`
	Health       api.Health        `json:"health"`
	Capabilities *api.Capabilities `json:"capabilities,omitempty"`
}

// ReadServerStatus reports the identity verified when the session was
// opened, and reads capabilities when the session carries a credential.
func ReadServerStatus(ctx context.Context, a CapabilityReader, url string, health api.Health, signedIn bool) (ServerStatus, error) {
	status := ServerStatus{URL: url, Health: health}
	if !signedIn {
		return status, nil
	}
	caps, err := a.Capabilities(ctx)
	if err != nil {
		return ServerStatus{}, err
	}
	status.Capabilities = &caps
	return status, nil
}

// SessionState is what an interface shows about the signed-in principal.
type SessionState struct {
	Me           api.Principal
	Capabilities api.Capabilities
}

// ReadSession reads the principal and the server's capabilities.
func ReadSession(ctx context.Context, a interface {
	PrincipalReader
	CapabilityReader
}) (SessionState, error) {
	me, err := WhoAmI(ctx, a, "")
	if err != nil {
		return SessionState{}, err
	}
	caps, err := a.Capabilities(ctx)
	return SessionState{Me: me, Capabilities: caps}, err
}

// WhoAmI reads the principal and, when a viewing profile is selected,
// refuses a profile the principal may not use.
func WhoAmI(ctx context.Context, a PrincipalReader, profile string) (api.Principal, error) {
	me, err := a.Me(ctx)
	if err != nil {
		return api.Principal{}, err
	}
	if profile != "" && !slices.Contains(me.ProfileIDs, profile) {
		return api.Principal{}, &api.Problem{Status: 403, Code: "profile_not_permitted", Title: "Forbidden",
			Detail: fmt.Sprintf("profile %q is not available to this principal", profile)}
	}
	return me, nil
}

// consent checks the credential-store choice: only the private file store
// exists, and it must be accepted explicitly once.
func consent(s *Session, accepted string) (string, error) {
	if accepted == "" {
		accepted = s.Config.CredentialStore
	}
	if accepted != store.FileStore {
		return "", store.ErrNoStore
	}
	return accepted, nil
}

// Signed is a credential this client stored for the session's server.
type Signed struct {
	ServerID  string `json:"server_id"`
	DeviceID  string `json:"device_id"`
	ExpiresAt string `json:"expires_at,omitempty"`
}

// keep stores the credential, registers the URL for the verified identity
// and switches the session to it.
func keep(s *Session, c api.Credential, accepted string) (Signed, error) {
	credential := store.Credential{ServerID: s.Health.ServerID, DeviceID: c.DeviceID, AccessToken: c.AccessToken, ExpiresAt: c.ExpiresAt}
	if err := s.Store.PutCredential(credential, accepted); err != nil {
		return Signed{}, err
	}
	s.Config.CredentialStore = accepted
	if err := s.Remember(); err != nil {
		return Signed{}, err
	}
	s.useCredential(&credential)
	return Signed{ServerID: s.Health.ServerID, DeviceID: c.DeviceID, ExpiresAt: c.ExpiresAt}, nil
}

// PendingPairing is a pairing request awaiting an administrator. Its device
// code is a secret held only here.
type PendingPairing struct {
	ID        string `json:"pairing_id"`
	UserCode  string `json:"user_code"`
	ExpiresAt string `json:"expires_at"`

	session    *Session
	accepted   string
	deviceCode string
	interval   time.Duration

	mu        sync.Mutex
	abandoned bool
	installed bool
}

// ErrPairingAbandoned is returned by Wait after Abandon.
var ErrPairingAbandoned = errors.New("pairing abandoned")

// Abandon stops the pairing from installing a credential. It reports
// whether the credential had already been installed, in which case the
// session is signed in.
func (p *PendingPairing) Abandon() (installed bool) {
	p.mu.Lock()
	defer p.mu.Unlock()
	p.abandoned = true
	return p.installed
}

// StartPairing asks the server to pair this device. The administrator
// approves it after comparing UserCode. Each call is a new pairing.
func StartPairing(ctx context.Context, s *Session, deviceName, accepted string) (*PendingPairing, error) {
	accepted, err := consent(s, accepted)
	if err != nil {
		return nil, err
	}
	if strings.TrimSpace(deviceName) == "" {
		return nil, Invalid("a device name is required")
	}
	p, err := s.CreatePairing(ctx, api.PairingRequest{DeviceName: deviceName, ClientName: "motion-terminal"})
	if err != nil {
		return nil, err
	}
	return &PendingPairing{ID: p.ID, UserCode: p.UserCode, ExpiresAt: p.ExpiresAt, session: s, accepted: accepted,
		deviceCode: p.DeviceCode, interval: time.Duration(max(p.PollIntervalSeconds, 1)) * time.Second}, nil
}

// Wait polls until the pairing is approved, then stores the credential and
// signs the session in. It honours the server's pacing and stops on denial,
// expiry or cancellation.
func (p *PendingPairing) Wait(ctx context.Context) (Signed, error) {
	interval := p.interval
	for {
		if err := p.session.Sleep(ctx, interval); err != nil {
			return Signed{}, err
		}
		p.mu.Lock()
		abandoned := p.abandoned
		p.mu.Unlock()
		if abandoned {
			return Signed{}, ErrPairingAbandoned
		}
		credential, err := p.session.ClaimPairing(ctx, p.ID, p.deviceCode)
		var problem *api.Problem
		var unavailable *api.UnavailableError
		switch {
		case err == nil:
			// Installation and abandonment exclude each other.
			p.mu.Lock()
			defer p.mu.Unlock()
			if p.abandoned {
				return Signed{}, ErrPairingAbandoned
			}
			signed, err := keep(p.session, credential, p.accepted)
			p.installed = err == nil
			return signed, err
		case errors.As(err, &problem) && (problem.Code == "pairing_pending" || problem.Status == 429):
			if problem.RetryAfter > 0 {
				interval = problem.RetryAfter
			}
		case errors.As(err, &unavailable):
			interval = min(interval*2, 30*time.Second)
		default:
			return Signed{}, err
		}
	}
}

// RedeemInvitation exchanges a one-use invitation code for a credential.
// The claim identity is recorded before the request, so a retry after a lost
// response, even from a new process, reuses it instead of consuming the
// invitation twice.
func RedeemInvitation(ctx context.Context, s *Session, code, deviceName, accepted string) (Signed, error) {
	accepted, err := consent(s, accepted)
	if err != nil {
		return Signed{}, err
	}
	code = strings.TrimSpace(code)
	if code == "" {
		return Signed{}, Invalid("an invitation code is required")
	}
	claim, err := s.Store.BeginRedemption(s.Health.ServerID, code, accepted, store.RedemptionClaim{ClaimID: api.NewIdempotencyKey(), DeviceName: deviceName})
	if err != nil {
		return Signed{}, err
	}
	input := api.RedemptionInput{Code: code, ClaimID: claim.ClaimID, DeviceName: claim.DeviceName, ClientName: "motion-terminal"}
	var credential api.Credential
	for attempt := 0; attempt < 3; attempt++ {
		credential, err = s.RedeemInvitation(ctx, input)
		if err == nil {
			break
		}
		if ctx.Err() != nil {
			return Signed{}, ctx.Err()
		}
		// Retain the exact claim identity for an ambiguous transport failure.
		var unavailable *api.UnavailableError
		if !errors.As(err, &unavailable) {
			return Signed{}, err
		}
		if err := s.Sleep(ctx, time.Second); err != nil {
			return Signed{}, err
		}
	}
	if err != nil {
		return Signed{}, err
	}
	return keep(s, credential, accepted)
}

// SignOut forgets this server's stored credential and stops sending it. The
// device stays valid server-side until an administrator revokes it.
func SignOut(s *Session) (string, error) {
	if err := s.Store.DeleteCredential(s.Health.ServerID); err != nil {
		return "", err
	}
	s.useCredential(nil)
	return s.Health.ServerID, nil
}
