package terminal_test

import (
	"bytes"
	"encoding/json"
	"io"
	"net/http"
	"os"
	"strings"
	"testing"

	"github.com/Jagalite/motion/apps/terminal/internal/cli"
)

// call sends a JSON request as a device (bearer token) or anonymously.
func call(t *testing.T, srv *server, method, path, token string, body any, headers map[string]string) (int, map[string]any) {
	t.Helper()
	var payload io.Reader
	if body != nil {
		encoded, err := json.Marshal(body)
		if err != nil {
			t.Fatal(err)
		}
		payload = bytes.NewReader(encoded)
	}
	req, err := http.NewRequest(method, srv.url+path, payload)
	if err != nil {
		t.Fatal(err)
	}
	req.Header.Set("Content-Type", "application/json")
	req.Header.Set("Origin", srv.url)
	if token != "" {
		req.Header.Set("Authorization", "Bearer "+token)
	}
	for k, v := range headers {
		req.Header.Set(k, v)
	}
	resp, err := http.DefaultClient.Do(req)
	if err != nil {
		t.Fatal(err)
	}
	defer resp.Body.Close()
	var out map[string]any
	_ = json.NewDecoder(resp.Body).Decode(&out)
	return resp.StatusCode, out
}

// First run against the real server: the operator creates the Owner, the only
// enabled user is preselected for approval, and a paired-only administrator
// device's sensitive change is approved through Action approvals.
func TestE2EOwnerSetupAndActionApprovalAgainstRealServer(t *testing.T) {
	bin := os.Getenv("MOTION_E2E_SERVER_BIN")
	if bin == "" {
		t.Skip("set MOTION_E2E_SERVER_BIN to the built server binary")
	}
	srv := startServer(t, bin)
	admin := t.TempDir()
	op := func(args ...string) invocation {
		return run(t, srv, admin, append([]string{"--operator-token-file", srv.operator, "--json"}, args...)...)
	}
	if status, _ := call(t, srv, "POST", "/api/v2/access/setup", "", map[string]any{}, nil); status != http.StatusUnauthorized {
		t.Fatalf("loopback without the operator token: %d", status)
	}
	first := op("access", "setup")
	if first.code != cli.ExitOK {
		t.Fatalf("setup: %+v", first)
	}
	owner := data(t, first.stdout)["owner"].(map[string]any)
	again := op("access", "setup", "--name", "Someone else")
	if again.code != cli.ExitOK || data(t, again.stdout)["owner"].(map[string]any)["id"] != owner["id"] {
		t.Fatalf("repeat: %+v", again)
	}

	// A device pairs; approval preselects the Owner and grants administration
	// only because it is requested explicitly.
	_, pairing := call(t, srv, "POST", "/api/v2/auth/pairings", "", map[string]string{"device_name": "Owner laptop", "client_name": "e2e"}, nil)
	approve := op("auth", "approve", pairing["id"].(string), "--code", pairing["user_code"].(string), "--permission", "catalog:read", "--permission", "system:admin")
	if approve.code != cli.ExitOK || data(t, approve.stdout)["user_id"] != owner["id"] {
		t.Fatalf("approve: %+v", approve)
	}
	_, claimed := call(t, srv, "POST", "/api/v2/auth/pairings/"+pairing["id"].(string)+"/claim", "", map[string]any{"device_code": pairing["device_code"]}, nil)
	device := claimed["access_token"].(string)

	// The paired-only device is not freshly signed in: creating a user needs
	// an approval from another principal, here the operator's CLI.
	user := map[string]any{"name": "Sam", "disabled": false, "profile_ids": []string{}, "permissions": []string{"catalog:read"},
		"policy": map[string]any{"library_ids": []string{}, "allow_unrated": true, "allowed_ratings": []string{}, "blocked_labels": []string{}}}
	if status, body := call(t, srv, "POST", "/api/v2/users", device, user, map[string]string{"Idempotency-Key": "e2e-owner-create-sam-0001"}); status < 400 || body["code"] != "recent_auth_required" {
		t.Fatalf("unapproved admin change: %d %v", status, body)
	}
	status, proof := call(t, srv, "POST", "/api/v2/auth/proofs", device, map[string]string{"action": "user.admin", "target": "new"}, nil)
	if status != http.StatusOK {
		t.Fatalf("proof request: %d %v", status, proof)
	}
	listed := op("access", "approvals", "list")
	if listed.code != cli.ExitOK || !strings.Contains(listed.stdout, proof["id"].(string)) {
		t.Fatalf("approvals list: %+v", listed)
	}
	if wrong := op("access", "approvals", "approve", proof["id"].(string), "--code", "WRONGCODE"); wrong.code == cli.ExitOK {
		t.Fatalf("wrong code approved: %+v", wrong)
	}
	if ok := op("access", "approvals", "approve", proof["id"].(string), "--code", strings.ToLower(proof["code"].(string))); ok.code != cli.ExitOK {
		t.Fatalf("approve request: %+v", ok)
	}
	// Until the device uses it, the approved request stays listed as approved.
	if after := op("access", "approvals", "list"); !strings.Contains(after.stdout, `"status": "approved"`) && !strings.Contains(after.stdout, `"status":"approved"`) {
		t.Fatalf("approved request status: %+v", after)
	}
	status, created := call(t, srv, "POST", "/api/v2/users", device, user, map[string]string{"Idempotency-Key": "e2e-owner-create-sam-0002", "X-Motion-Action-Proof": proof["id"].(string)})
	if status != http.StatusCreated || created["name"] != "Sam" {
		t.Fatalf("approved admin change: %d %v", status, created)
	}

	// With two enabled users the CLI no longer selects one.
	_, second := call(t, srv, "POST", "/api/v2/auth/pairings", "", map[string]string{"device_name": "Den TV", "client_name": "e2e"}, nil)
	if refused := op("auth", "approve", second["id"].(string), "--code", second["user_code"].(string)); refused.code != cli.ExitInvalid {
		t.Fatalf("approval without --user and two users: %+v", refused)
	}
}
