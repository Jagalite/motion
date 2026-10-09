package terminal_test

// End-to-end against the real Rust server, not a mock. Run with:
//   cargo build && MOTION_E2E_SERVER_BIN=$PWD/target/debug/playscale go test -run E2E ./apps/terminal
import (
	"bufio"
	"bytes"
	"context"
	"encoding/json"
	"fmt"
	"io"
	"net/http"
	"os"
	"os/exec"
	"path/filepath"
	"strings"
	"sync"
	"syscall"
	"testing"
	"time"

	"github.com/Jagalite/motion/apps/terminal/internal/cli"
)

type server struct {
	url      string
	dataDir  string
	operator string
	cmd      *exec.Cmd
}

func startServer(t *testing.T, bin string) *server {
	t.Helper()
	data := t.TempDir()
	cmd := exec.Command(bin, "--listen", "127.0.0.1:0", "--data-dir", data, "--access-mode", "restricted")
	stdout, err := cmd.StdoutPipe()
	if err != nil {
		t.Fatal(err)
	}
	cmd.Stderr = os.Stderr
	if err := cmd.Start(); err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() {
		_ = cmd.Process.Signal(syscall.SIGTERM)
		done := make(chan struct{})
		go func() { _ = cmd.Wait(); close(done) }()
		select {
		case <-done:
		case <-time.After(15 * time.Second):
			_ = cmd.Process.Kill()
		}
	})
	lines := bufio.NewScanner(stdout)
	url := make(chan string, 1)
	go func() {
		for lines.Scan() {
			if rest, ok := strings.CutPrefix(lines.Text(), "Motion is running at "); ok {
				url <- strings.TrimSpace(rest)
			}
		}
	}()
	select {
	case u := <-url:
		return &server{url: u, dataDir: data, operator: filepath.Join(data, "admin-token"), cmd: cmd}
	// A freshly linked binary can take minutes to pass the OS launch scan.
	case <-time.After(10 * time.Minute):
		t.Fatal("server did not start")
	}
	return nil
}

// syncBuffer is safe for a command writing while the test reads.
type syncBuffer struct {
	mu  sync.Mutex
	buf bytes.Buffer
}

func (b *syncBuffer) Write(p []byte) (int, error) {
	b.mu.Lock()
	defer b.mu.Unlock()
	return b.buf.Write(p)
}

func (b *syncBuffer) String() string {
	b.mu.Lock()
	defer b.mu.Unlock()
	return b.buf.String()
}

type invocation struct {
	code   int
	stdout string
	stderr string
}

func motion(ctx context.Context, srv *server, config string, out, errOut io.Writer, args ...string) int {
	env := &cli.Env{Out: out, Err: errOut}
	err := cli.Execute(ctx, env, append([]string{"--server", srv.url, "--config-dir", config}, args...))
	if err != nil {
		cli.ReportError(errOut, env.JSON, err)
	}
	return cli.ExitCode(err)
}

func run(t *testing.T, srv *server, config string, args ...string) invocation {
	t.Helper()
	var out, errOut bytes.Buffer
	code := motion(context.Background(), srv, config, &out, &errOut, args...)
	return invocation{code, out.String(), errOut.String()}
}

func data(t *testing.T, s string) map[string]any {
	t.Helper()
	var v struct {
		Schema string         `json:"schema"`
		Data   map[string]any `json:"data"`
	}
	if err := json.Unmarshal([]byte(s), &v); err != nil || v.Schema != cli.Schema {
		t.Fatalf("not versioned JSON: %q (%v)", s, err)
	}
	return v.Data
}

func TestE2EPairApproveUseAndRevokeAgainstRealServer(t *testing.T) {
	bin := os.Getenv("MOTION_E2E_SERVER_BIN")
	if bin == "" {
		t.Skip("set MOTION_E2E_SERVER_BIN to the built server binary")
	}
	srv := startServer(t, bin)
	device := t.TempDir()
	admin := t.TempDir()

	status := run(t, srv, device, "--json", "server", "status")
	if status.code != cli.ExitOK {
		t.Fatalf("status: %+v", status)
	}
	health := data(t, status.stdout)["health"].(map[string]any)
	serverID := health["server_id"].(string)

	// Restricted mode: the legacy v1 surface is closed to anonymous callers.
	resp, err := http.Get(srv.url + "/api/v1/libraries")
	if err != nil || resp.StatusCode != http.StatusUnauthorized {
		t.Fatalf("legacy surface open: %v %v", resp, err)
	}
	resp.Body.Close()

	// Pair: the device waits while the operator approves the displayed code.
	pairOut, pairErr := &syncBuffer{}, &syncBuffer{}
	paired := make(chan int, 1)
	go func() {
		paired <- motion(context.Background(), srv, device, pairOut, pairErr,
			"--json", "auth", "pair", "--name", "e2e", "--credential-store", "file", "--wait", "2m")
	}()
	var pending map[string]string
	deadline := time.Now().Add(30 * time.Second)
	for pending == nil && time.Now().Before(deadline) {
		for _, line := range strings.Split(pairErr.String(), "\n") {
			var v struct {
				Notice map[string]string `json:"notice"`
			}
			if json.Unmarshal([]byte(line), &v) == nil && v.Notice["pairing_id"] != "" {
				pending = v.Notice
			}
		}
		time.Sleep(50 * time.Millisecond)
	}
	if pending == nil {
		t.Fatalf("no pairing notice: %s", pairErr.String())
	}
	approve := run(t, srv, admin, "--operator-token-file", srv.operator, "--json", "auth", "approve", pending["pairing_id"],
		"--code", strings.ToLower(pending["user_code"]), "--grant-profile", "default",
		"--permission", "catalog:read", "--permission", "events:read", "--permission", "profiles:manage")
	if approve.code != cli.ExitOK {
		t.Fatalf("approve: %+v", approve)
	}
	deviceID := data(t, approve.stdout)["id"].(string)
	select {
	case code := <-paired:
		if code != cli.ExitOK {
			t.Fatalf("pair exit %d: %s", code, pairErr.String())
		}
	case <-time.After(60 * time.Second):
		t.Fatal("pairing did not complete")
	}
	if strings.Contains(pairOut.String(), "mdv_") || strings.Contains(pairErr.String(), "mdv_") {
		t.Fatal("credential printed")
	}
	cred, err := os.Stat(filepath.Join(device, "credentials", serverID+".json"))
	if err != nil || cred.Mode().Perm() != 0o600 {
		t.Fatalf("credential file: %v %v", cred, err)
	}

	me := run(t, srv, device, "--json", "auth", "status")
	if me.code != cli.ExitOK || data(t, me.stdout)["device_id"] != deviceID {
		t.Fatalf("me: %+v", me)
	}
	// Authorization is enforced: device management is administrative.
	if denied := run(t, srv, device, "devices", "list"); denied.code != cli.ExitAuth {
		t.Fatalf("device listed devices: %+v", denied)
	}

	// Follow events while renaming the granted profile.
	ctx, cancel := context.WithCancel(context.Background())
	defer cancel()
	tailOut, tailErr := &syncBuffer{}, &syncBuffer{}
	tailed := make(chan int, 1)
	go func() {
		tailed <- motion(ctx, srv, device, tailOut, tailErr, "--json", "events", "tail")
	}()
	waitFor(t, tailOut, `"kind":"reset"`)
	rename := run(t, srv, device, "--json", "profiles", "rename", "default", "Household")
	if rename.code != cli.ExitOK || data(t, rename.stdout)["name"] != "Household" {
		t.Fatalf("rename: %+v", rename)
	}
	waitFor(t, tailOut, `"resource_id":"default"`)
	stale := run(t, srv, device, "profiles", "rename", "default", "Again", "--if-match", `"r-0"`)
	if stale.code != cli.ExitConflict {
		t.Fatalf("stale rename: %+v", stale)
	}

	// Revocation ends the stream and every later request.
	revoke := run(t, srv, admin, "--operator-token-file", srv.operator, "devices", "revoke", deviceID)
	if revoke.code != cli.ExitOK {
		t.Fatalf("revoke: %+v", revoke)
	}
	select {
	case code := <-tailed:
		if code != cli.ExitAuth {
			t.Fatalf("tail ended with %d: %s", code, tailErr.String())
		}
	case <-time.After(30 * time.Second):
		t.Fatal("event stream survived revocation")
	}
	if after := run(t, srv, device, "auth", "status"); after.code != cli.ExitAuth {
		t.Fatalf("revoked credential still works: %+v", after)
	}
	fmt.Fprintf(os.Stderr, "e2e: server %s paired device %s, events, conditional rename, revocation verified\n", serverID, deviceID)
}

func waitFor(t *testing.T, b *syncBuffer, needle string) {
	t.Helper()
	deadline := time.Now().Add(30 * time.Second)
	for time.Now().Before(deadline) {
		if strings.Contains(b.String(), needle) {
			return
		}
		time.Sleep(50 * time.Millisecond)
	}
	t.Fatalf("timed out waiting for %s in %q", needle, b.String())
}
