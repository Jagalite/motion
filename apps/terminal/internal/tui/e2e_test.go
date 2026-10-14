package tui

import (
	"bufio"
	"context"
	"fmt"
	"os"
	"os/exec"
	"path/filepath"
	"reflect"
	"strings"
	"syscall"
	"testing"
	"time"

	tea "charm.land/bubbletea/v2"
	"github.com/Jagalite/motion/apps/terminal/internal/api"
	"github.com/Jagalite/motion/apps/terminal/internal/operations"
	"github.com/Jagalite/motion/apps/terminal/internal/operations/opstest"
)

// realServer starts the built server with isolated data, as the e2e tests
// of the terminal module do, and returns its URL and operator token file.
func realServer(t *testing.T, bin string) (string, string) {
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
	url := make(chan string, 1)
	go func() {
		lines := bufio.NewScanner(stdout)
		for lines.Scan() {
			if rest, ok := strings.CutPrefix(lines.Text(), "Motion is running at "); ok {
				url <- strings.TrimSpace(rest)
			}
		}
	}()
	select {
	case u := <-url:
		return u, filepath.Join(data, "admin-token")
	case <-time.After(10 * time.Minute):
		t.Fatal("server did not start")
	}
	return "", ""
}

// The TUI half of the real-server parity qualification; the CLI half is
// TestE2ECLIReachesTheSharedRealServerState in the terminal module root.
func TestE2ETUIReachesTheSharedRealServerState(t *testing.T) {
	bin := os.Getenv("MOTION_E2E_SERVER_BIN")
	if bin == "" {
		t.Skip("set MOTION_E2E_SERVER_BIN to the built server binary")
	}
	url, token := realServer(t, bin)
	// Real requests take longer than the fakes; notices (5 s timers) are
	// then waited out too, which only slows the test.
	defer func(saved time.Duration) { drainWait = saved }(drainWait)
	drainWait = 6 * time.Second
	ctx := context.Background()
	s, err := operations.Connect(ctx, operations.Settings{Server: url, OperatorTokenFile: token, ConfigDir: t.TempDir()})
	if err != nil {
		t.Fatal(err)
	}
	m := New(ctx, s, s.URL, nil)
	m.session = s
	m, _ = update(t, m, tea.WindowSizeMsg{Width: 140, Height: 45})
	m = drain(t, m, m.reloadAll())
	m.focus = focusContent

	m = goTo(t, m, route{page: pageProfiles})
	for _, name := range []string{opstest.RealProfile, opstest.RealTemporary} {
		m = press(t, m, "n")
		setText(t, m, "name", name)
		m = apply(t, m)
	}
	kids := rowNamed(t, m, keyProfiles, func(p api.Profile) (string, string) { return p.Name, p.ID }, opstest.RealProfile)
	m = selectRow(t, m, kids)
	m = press(t, m, "e")
	setText(t, m, "name", opstest.RealRenamed)
	m = apply(t, m)
	temp := rowNamed(t, m, keyProfiles, func(p api.Profile) (string, string) { return p.Name, p.ID }, opstest.RealTemporary)
	m = selectRow(t, m, temp)
	m = press(t, m, "x")
	m = apply(t, m)

	root := filepath.Join(t.TempDir(), opstest.RealSourceDir)
	if err := os.Mkdir(root, 0o755); err != nil {
		t.Fatal(err)
	}
	m = goTo(t, m, route{page: pageSources})
	m = press(t, m, "n")
	setText(t, m, "name", opstest.RealSource)
	setText(t, m, "root", root)
	m = apply(t, m)
	films := rowNamed(t, m, keySources, func(v api.Source) (string, string) { return v.Name, v.ID }, opstest.RealSource)
	m = press(t, m, "l")
	setText(t, m, "name", opstest.RealLibrary)
	setText(t, m, "kind", opstest.RealLibraryKind)
	setToggle(t, m, "source", films, true)
	m = apply(t, m)

	m = goTo(t, m, route{page: pageScans})
	movies := rowNamed(t, m, keyLibraries, func(l api.Library) (string, string) { return l.Name, l.ID }, opstest.RealLibrary)
	m = press(t, m, "n")
	for _, f := range m.access.fields {
		if f.kind == "library" {
			setToggle(t, m, "library", f.id, f.id == movies)
		}
	}
	m = apply(t, m)

	m = goTo(t, m, route{page: pageUsers})
	m = press(t, m, "n")
	setText(t, m, "name", opstest.RealUser)
	for _, f := range m.access.fields {
		if f.kind == "permission" {
			setToggle(t, m, "permission", f.id, f.id == opstest.RealUserPermission)
		}
	}
	m = apply(t, m)
	m = goTo(t, m, route{page: pageInvitations})
	m = press(t, m, "n")
	m = chooseUser(t, m, opstest.RealUser)
	setText(t, m, "ttl", fmt.Sprint(opstest.RealInvitationTTL))
	m = apply(t, m)
	if m.access == nil || m.access.secret == "" {
		t.Fatal("invitation code not shown once")
	}
	m = press(t, m, "esc")
	invitations, _ := data[[]api.Invitation](m, keyInvitations)
	if len(invitations) != 1 {
		t.Fatalf("invitations %+v", invitations)
	}
	m = selectRow(t, m, invitations[0].ID)
	m = press(t, m, "d")
	m = apply(t, m)
	// Issue a second invitation and redeem it from a signed-out session.
	m = press(t, m, "n")
	m = chooseUser(t, m, opstest.RealUser)
	setText(t, m, "ttl", fmt.Sprint(opstest.RealInvitationTTL))
	m = apply(t, m)
	code := m.access.secret
	m = press(t, m, "esc")
	guest, err := operations.Connect(ctx, operations.Settings{Server: url, ConfigDir: t.TempDir()})
	if err != nil {
		t.Fatal(err)
	}
	r := New(ctx, guest, guest.URL, nil)
	r.session = guest
	r, _ = update(t, r, tea.WindowSizeMsg{Width: 140, Height: 45})
	r = drain(t, r, r.reloadAll())
	r.focus = focusContent
	r = press(t, r, "I")
	setText(t, r, "code", code)
	setText(t, r, "name", opstest.RealDevice)
	setToggle(t, r, "consent", "", true)
	r = apply(t, r)
	if !guest.SignedIn() || r.me() == nil {
		t.Fatal("redemption did not sign the guest session in")
	}

	got, err := opstest.Observe(ctx, s)
	if err != nil {
		t.Fatal(err)
	}
	if !reflect.DeepEqual(got, opstest.ExpectedObservation) {
		t.Fatalf("TUI left %+v, want %+v", got, opstest.ExpectedObservation)
	}
}
