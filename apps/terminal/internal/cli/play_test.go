package cli

import (
	"net/url"
	"strings"
	"testing"
)

func TestPlayerURLHasOnlyEscapedSelection(t *testing.T) {
	value, err := PlayerURL("https://server.example/base?ticket=secret#fragment", "猫/a?#", "profile&one")
	if err != nil {
		t.Fatal(err)
	}
	u, err := url.Parse(value)
	if err != nil || u.Path != "/play/猫/a?#" || u.Query().Get("profile_id") != "profile&one" {
		t.Fatalf("%s %v", value, err)
	}
	if len(u.Query()) != 1 || strings.Contains(value, "secret") || u.Fragment != "" || !strings.Contains(u.EscapedPath(), "%2F") {
		t.Fatal(value)
	}
	for _, server := range []string{"file:///tmp/player", "https://secret@server.example", "javascript:alert(1)"} {
		if _, err := PlayerURL(server, "t", ""); err == nil {
			t.Fatal(server)
		}
	}
}
