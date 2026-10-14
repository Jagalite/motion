package cli

import (
	"context"
	"fmt"
	"io"
	"net/http"
	"os"
	"time"

	"github.com/Jagalite/motion/apps/terminal/internal/operations"
)

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
	// Browser opens the player; main injects the platform opener.
	Browser operations.Browser
}

// Conn is a client bound to a verified server identity.
type Conn = operations.Session

// settings resolves flags and environment variables for the shared session
// pathway.
func (e *Env) settings() operations.Settings {
	server := e.Server
	if server == "" {
		server = os.Getenv("MOTION_SERVER")
	}
	tokenFile := e.OperatorTokenFile
	if tokenFile == "" {
		tokenFile = os.Getenv("MOTION_OPERATOR_TOKEN_FILE")
	}
	return operations.Settings{Server: server, Profile: e.Profile, OperatorTokenFile: tokenFile,
		ConfigDir: e.ConfigDir, Timeout: e.Timeout, HTTP: e.HTTP}
}

// Connect opens the shared verified session for this invocation.
func (e *Env) Connect(ctx context.Context) (*Conn, error) {
	return operations.Connect(ctx, e.settings())
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
