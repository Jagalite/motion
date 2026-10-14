package operations

import (
	"context"
	"fmt"
	"net"
	"net/http"
	"os"
	"strings"
	"sync"
	"time"

	"github.com/Jagalite/motion/apps/terminal/internal/api"
	"github.com/Jagalite/motion/apps/terminal/internal/store"
)

// Settings are the resolved connection choices. Entry points parse flags and
// environment variables; every interface then connects through Connect.
type Settings struct {
	// Server is an explicit server URL; empty selects the stored default.
	Server  string
	Profile string
	// OperatorTokenFile authenticates as the server operator.
	OperatorTokenFile string
	// ConfigDir overrides the credential store location.
	ConfigDir string
	// Timeout bounds connecting and response headers; zero disables it.
	Timeout time.Duration
	// HTTP is replaced in tests.
	HTTP *http.Client
}

// Session is a client bound to a verified server identity.
type Session struct {
	Profile string
	*api.Client
	URL        string
	Health     api.Health
	Credential *store.Credential
	Operator   bool
	Store      store.Store
	Config     store.Config

	mu    sync.Mutex
	token string
}

// SignedIn reports whether requests carry a credential.
func (s *Session) SignedIn() bool {
	s.mu.Lock()
	defer s.mu.Unlock()
	return s.token != ""
}

// useCredential switches the bearer credential for later requests; nil
// signs the session out. Operator sessions keep the operator token.
func (s *Session) useCredential(c *store.Credential) {
	s.mu.Lock()
	defer s.mu.Unlock()
	if s.Operator {
		return
	}
	s.Credential, s.token = c, ""
	if c != nil {
		s.token = c.AccessToken
	}
}

func (s Settings) store() (store.Store, error) {
	if s.ConfigDir != "" {
		return store.Store{Dir: s.ConfigDir}, nil
	}
	return store.Default()
}

func (s Settings) serverURL(cfg store.Config) (string, error) {
	switch {
	case s.Server != "":
		return strings.TrimRight(s.Server, "/"), nil
	case cfg.Default != "":
		return cfg.Servers[cfg.Default].URL, nil
	}
	return "", Invalid("no server selected; pass --server http://host:port or set MOTION_SERVER")
}

func readOperatorToken(path string) (string, error) {
	info, err := os.Stat(path)
	if err != nil {
		return "", err
	}
	if info.Mode().Perm()&0o077 != 0 {
		return "", fmt.Errorf("%s is accessible by other users; it must be mode 0600", path)
	}
	data, err := os.ReadFile(path)
	if err != nil {
		return "", err
	}
	token := strings.TrimSpace(string(data))
	if len(token) < 32 {
		return "", fmt.Errorf("%s does not contain an operator token", path)
	}
	return token, nil
}

// Connect resolves the server, verifies its identity against what this client
// recorded, and selects the credential bound to that identity. A credential
// is never sent before the unauthenticated identity check succeeds.
func Connect(ctx context.Context, s Settings) (*Session, error) {
	st, err := s.store()
	if err != nil {
		return nil, err
	}
	cfg, err := st.Load()
	if err != nil {
		return nil, err
	}
	url, err := s.serverURL(cfg)
	if err != nil {
		return nil, err
	}
	httpClient := s.HTTP
	if httpClient == nil {
		// Bound connecting and response headers even for long-lived streams;
		// a healthy stream's body is not subject to the timeout.
		transport := http.DefaultTransport.(*http.Transport).Clone()
		if s.Timeout > 0 {
			transport.DialContext = (&net.Dialer{Timeout: s.Timeout, KeepAlive: 30 * time.Second}).DialContext
			transport.TLSHandshakeTimeout = s.Timeout
			transport.ResponseHeaderTimeout = s.Timeout
		}
		httpClient = &http.Client{Transport: transport}
	}
	client, err := api.New(url, httpClient)
	if err != nil {
		return nil, Invalid("%v", err)
	}
	health, err := client.Health(ctx)
	if err != nil {
		return nil, err
	}
	for _, known := range cfg.Servers {
		if known.URL == url && known.ServerID != health.ServerID {
			return nil, fmt.Errorf("%w: %s was server %s and now reports %s; refusing to send credentials (remove it from %s to re-pair)",
				ErrServerIdentity, url, known.ServerID, health.ServerID, st.Dir)
		}
	}
	// A credential is selected only for a URL this client registered for the
	// same server identity; a new URL claiming a known server_id gets none.
	registered := false
	for _, known := range cfg.Servers {
		if known.URL == url && known.ServerID == health.ServerID {
			registered = true
		}
	}
	session := &Session{Profile: s.Profile, Client: client, URL: url, Health: health, Store: st, Config: cfg}
	if s.OperatorTokenFile != "" {
		token, err := readOperatorToken(s.OperatorTokenFile)
		if err != nil {
			return nil, Invalid("operator token: %v", err)
		}
		session.token, session.Operator = token, true
	} else if registered {
		credential, err := st.Credential(health.ServerID)
		if err != nil {
			return nil, err
		}
		session.useCredential(credential)
	}
	client.Token = func() string {
		session.mu.Lock()
		defer session.mu.Unlock()
		return session.token
	}
	return session, nil
}

// Remember registers this URL for the verified server identity, so later
// connections can verify it and select its credential.
func (s *Session) Remember() error {
	s.Config.Servers[s.URL] = store.Server{URL: s.URL, ServerID: s.Health.ServerID}
	if s.Config.Default == "" {
		s.Config.Default = s.URL
	}
	return s.Store.Save(s.Config)
}
