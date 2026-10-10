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
	"net/http/cookiejar"
	"os"
	"os/exec"
	"path/filepath"
	"strings"
	"sync"
	"syscall"
	"testing"
	"time"

	"github.com/Jagalite/motion/apps/terminal/internal/cli"
	"github.com/Jagalite/motion/apps/terminal/internal/store"
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
	// A real cookie jar enforces Path, unlike router tests that inject Cookie.
	// The browser session must reach both SSR and API routes.
	credential, err := (store.Store{Dir: device}).Credential(serverID)
	if err != nil || credential == nil {
		t.Fatal("cannot load paired test credential", err)
	}
	jar, err := cookiejar.New(nil)
	if err != nil {
		t.Fatal(err)
	}
	browser := &http.Client{Jar: jar, Timeout: 20 * time.Second}
	exchange, err := json.Marshal(map[string]string{"kind": "credential", "credential": credential.AccessToken})
	if err != nil {
		t.Fatal(err)
	}
	sessionRequest, err := http.NewRequest("POST", srv.url+"/api/v2/auth/session", bytes.NewReader(exchange))
	if err != nil {
		t.Fatal(err)
	}
	sessionRequest.Header.Set("Origin", srv.url)
	sessionRequest.Header.Set("Content-Type", "application/json")
	sessionResponse, err := browser.Do(sessionRequest)
	if err != nil {
		t.Fatal(err)
	}
	sessionResponse.Body.Close()
	if sessionResponse.StatusCode != http.StatusOK {
		t.Fatalf("session exchange: %d", sessionResponse.StatusCode)
	}
	checkBrowser := func(path string, status int, contains string) {
		t.Helper()
		r, err := browser.Get(srv.url + path)
		if err != nil {
			t.Fatal(err)
		}
		body, err := io.ReadAll(io.LimitReader(r.Body, 2<<20))
		r.Body.Close()
		if err != nil || r.StatusCode != status || !strings.Contains(string(body), contains) {
			t.Fatalf("browser %s: status %d, expected %d and %q (%v)", path, r.StatusCode, status, contains, err)
		}
		if strings.Contains(string(body), credential.AccessToken) {
			t.Fatal("SSR exposed credential")
		}
	}
	checkBrowser("/", http.StatusOK, "Continue watching")
	checkBrowser("/api/v2/auth/session", http.StatusOK, "csrf_token")
	checkBrowser("/?profile_id=not-granted", http.StatusForbidden, "")
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

	// Registered sources and logical libraries use the real v2 services.
	mediaRoot := t.TempDir()
	source := run(t, srv, admin, "--operator-token-file", srv.operator, "--json", "sources", "add", "Test source", "--root", mediaRoot)
	if source.code != cli.ExitOK {
		t.Fatalf("source: %+v", source)
	}
	sourceID := data(t, source.stdout)["id"].(string)
	library := run(t, srv, admin, "--operator-token-file", srv.operator, "--json", "libraries", "add", "Test library", "--source", sourceID)
	if library.code != cli.ExitOK {
		t.Fatalf("library: %+v", library)
	}
	libraryID := data(t, library.stdout)["id"].(string)
	if libraryID == sourceID {
		t.Fatal("logical library reused source identity")
	}
	libraries := run(t, srv, admin, "--operator-token-file", srv.operator, "--json", "libraries", "list")
	if libraries.code != cli.ExitOK || len(data(t, libraries.stdout)["items"].([]any)) != 1 {
		t.Fatalf("libraries: %+v", libraries)
	}
	hiddenLibraries := run(t, srv, device, "--json", "libraries", "list")
	if hiddenLibraries.code != cli.ExitOK || len(data(t, hiddenLibraries.stdout)["items"].([]any)) != 0 {
		t.Fatalf("hidden libraries: %+v", hiddenLibraries)
	}
	checkBrowser("/", http.StatusOK, "No libraries")
	// Catalog fixtures are created through HTTP, never by touching SQLite.
	token, err := os.ReadFile(srv.operator)
	if err != nil {
		t.Fatal(err)
	}
	request, err := http.NewRequest("POST", srv.url+"/api/v2/catalog/items", strings.NewReader(`{"kind":"video","title":"Unicode 猫 Film","library_ids":[]}`))
	if err != nil {
		t.Fatal(err)
	}
	request.Header.Set("Authorization", "Bearer "+strings.TrimSpace(string(token)))
	request.Header.Set("Content-Type", "application/json")
	request.Header.Set("Idempotency-Key", "e2e-create-catalog-item")
	response, err := http.DefaultClient.Do(request)
	if err != nil {
		t.Fatal(err)
	}
	var created map[string]any
	err = json.NewDecoder(response.Body).Decode(&created)
	response.Body.Close()
	if err != nil || response.StatusCode != 201 {
		t.Fatalf("catalog creation %d %v %v", response.StatusCode, created, err)
	}
	found := run(t, srv, admin, "--operator-token-file", srv.operator, "--json", "catalog", "search", "猫")
	if found.code != cli.ExitOK || len(data(t, found.stdout)["items"].([]any)) != 1 {
		t.Fatalf("search: %+v", found)
	}
	shown := run(t, srv, admin, "--operator-token-file", srv.operator, "--json", "catalog", "show", created["id"].(string))
	if shown.code != cli.ExitOK || data(t, shown.stdout)["title"] != "Unicode 猫 Film" {
		t.Fatalf("show: %+v", shown)
	}
	diagnostics := run(t, srv, admin, "--operator-token-file", srv.operator, "--json", "diagnostics")
	if diagnostics.code != cli.ExitOK || data(t, diagnostics.stdout)["server_epoch"] == "" {
		t.Fatalf("diagnostics: %+v", diagnostics)
	}
	jobs := run(t, srv, admin, "--operator-token-file", srv.operator, "--json", "jobs", "list")
	if jobs.code != cli.ExitOK {
		t.Fatalf("jobs: %+v", jobs)
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
	checkBrowser("/", http.StatusUnauthorized, "Sign in to Motion")
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
