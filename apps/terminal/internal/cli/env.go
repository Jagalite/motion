package cli

import (
	"context"
	"errors"
	"fmt"
	"io"
	"net"
	"net/http"
	"os"
	"strings"
	"time"

	"github.com/Jagalite/motion/apps/terminal/internal/api"
	"github.com/Jagalite/motion/apps/terminal/internal/store"
)

var errServerIdentity = errors.New("server identity changed")

// Env carries global flags and process I/O for one invocation.
type Env struct {
	Out, Err          io.Writer
	JSON              bool
	Timeout           time.Duration
	Server            string
	Profile           string
	OperatorTokenFile string
	ConfigDir         string
	// HTTP is replaced in tests.
	HTTP *http.Client
}

// Conn is a client bound to a verified server identity.
type Conn struct {
	*api.Client
	URL        string
	Health     api.Health
	Credential *store.Credential
	Operator   bool
	Store      store.Store
	Config     store.Config
}

func (e *Env) store() (store.Store, error) {
	if e.ConfigDir != "" {
		return store.Store{Dir: e.ConfigDir}, nil
	}
	return store.Default()
}

func (e *Env) serverURL(cfg store.Config) (string, error) {
	switch {
	case e.Server != "":
		return strings.TrimRight(e.Server, "/"), nil
	case os.Getenv("MOTION_SERVER") != "":
		return strings.TrimRight(os.Getenv("MOTION_SERVER"), "/"), nil
	case cfg.Default != "":
		return cfg.Servers[cfg.Default].URL, nil
	}
	return "", usage("no server selected; pass --server http://host:port or set MOTION_SERVER")
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
func (e *Env) Connect(ctx context.Context) (*Conn, error) {
	st, err := e.store()
	if err != nil {
		return nil, err
	}
	cfg, err := st.Load()
	if err != nil {
		return nil, err
	}
	url, err := e.serverURL(cfg)
	if err != nil {
		return nil, err
	}
	httpClient := e.HTTP
	if httpClient == nil {
		// Bound connecting and response headers even for long-lived streams;
		// a healthy stream's body is not subject to the timeout.
		transport := http.DefaultTransport.(*http.Transport).Clone()
		if e.Timeout > 0 {
			transport.DialContext = (&net.Dialer{Timeout: e.Timeout, KeepAlive: 30 * time.Second}).DialContext
			transport.TLSHandshakeTimeout = e.Timeout
			transport.ResponseHeaderTimeout = e.Timeout
		}
		httpClient = &http.Client{Transport: transport}
	}
	client, err := api.New(url, httpClient)
	if err != nil {
		return nil, usage("%v", err)
	}
	health, err := client.Health(ctx)
	if err != nil {
		return nil, err
	}
	for _, known := range cfg.Servers {
		if known.URL == url && known.ServerID != health.ServerID {
			return nil, fmt.Errorf("%w: %s was server %s and now reports %s; refusing to send credentials (remove it from %s to re-pair)",
				errServerIdentity, url, known.ServerID, health.ServerID, st.Dir)
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
	conn := &Conn{Client: client, URL: url, Health: health, Store: st, Config: cfg}
	tokenFile := e.OperatorTokenFile
	if tokenFile == "" {
		tokenFile = os.Getenv("MOTION_OPERATOR_TOKEN_FILE")
	}
	var token string
	if tokenFile != "" {
		if token, err = readOperatorToken(tokenFile); err != nil {
			return nil, usage("operator token: %v", err)
		}
		conn.Operator = true
	} else if registered {
		if conn.Credential, err = st.Credential(health.ServerID); err != nil {
			return nil, err
		}
		if conn.Credential != nil {
			token = conn.Credential.AccessToken
		}
	}
	client.Token = func() string { return token }
	return conn, nil
}

// Remember registers this URL for the verified server identity, so later
// connections can verify it and select its credential.
func (c *Conn) Remember() error {
	c.Config.Servers[c.URL] = store.Server{URL: c.URL, ServerID: c.Health.ServerID}
	if c.Config.Default == "" {
		c.Config.Default = c.URL
	}
	return c.Store.Save(c.Config)
}

// context applies the global timeout; zero disables it.
func (e *Env) context(parent context.Context) (context.Context, context.CancelFunc) {
	if e.Timeout <= 0 {
		return context.WithCancel(parent)
	}
	return context.WithTimeout(parent, e.Timeout)
}

func (e *Env) print(data any, human func(io.Writer)) error {
	if e.JSON {
		return writeJSON(e.Out, envelope{Data: data})
	}
	// Every human-readable byte passes through control-character removal.
	human(sanitizer{e.Out})
	return nil
}

type sanitizer struct{ w io.Writer }

func (s sanitizer) Write(p []byte) (int, error) {
	if _, err := io.WriteString(s.w, sanitize(string(p))); err != nil {
		return 0, err
	}
	return len(p), nil
}

// notice reports progress on stderr without polluting machine output.
func (e *Env) notice(data any, human string) {
	if e.JSON {
		_ = writeJSON(e.Err, envelope{Notice: data})
		return
	}
	fmt.Fprintln(e.Err, sanitize(human))
}
