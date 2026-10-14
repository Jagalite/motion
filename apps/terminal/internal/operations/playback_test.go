package operations

import (
	"context"
	"errors"
	"net/url"
	"strings"
	"testing"

	"github.com/Jagalite/motion/apps/terminal/internal/api"
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

type player struct {
	permissions []string
	features    []api.Feature
	calls       []string
	profileErr  error
}

func (p *player) Me(context.Context) (api.Principal, error) {
	p.calls = append(p.calls, "me")
	return api.Principal{ID: "d", Permissions: p.permissions}, nil
}
func (p *player) Capabilities(context.Context) (api.Capabilities, error) {
	p.calls = append(p.calls, "capabilities")
	return api.Capabilities{Features: p.features}, nil
}
func (p *player) GetTimeline(_ context.Context, id string) (api.Tagged[api.Timeline], error) {
	p.calls = append(p.calls, "timeline "+id)
	return api.Tagged[api.Timeline]{Value: api.Timeline{ID: id}}, nil
}
func (p *player) GetProfile(_ context.Context, id string) (api.Tagged[api.Profile], error) {
	p.calls = append(p.calls, "profile "+id)
	return api.Tagged[api.Profile]{Value: api.Profile{ID: id}}, p.profileErr
}

func TestLaunchPlayerChecksCapabilityAndOpensOneTokenFreeURL(t *testing.T) {
	ready := []api.Feature{{ID: PlaybackFeature, Implemented: true, Enabled: true}}
	opened := []string{}
	browser := func(_ context.Context, u string) error { opened = append(opened, u); return nil }
	in := PlayInput{Server: "https://srv", TimelineID: "t/1", ProfileID: "p", Open: true}
	for _, c := range []struct {
		name string
		p    *player
		code string
	}{
		{"no permission", &player{features: ready}, "forbidden"},
		{"player disabled", &player{permissions: []string{api.PermPlaybackRequest}, features: []api.Feature{{ID: PlaybackFeature, Implemented: true}}}, "feature_unavailable"},
		{"profile gone", &player{permissions: []string{api.PermPlaybackRequest}, features: ready, profileErr: &api.Problem{Status: 404, Code: "not_found"}}, "not_found"},
	} {
		if _, err := LaunchPlayer(context.Background(), c.p, in, browser); !api.IsCode(err, c.code) {
			t.Errorf("%s: %v", c.name, err)
		}
	}
	if len(opened) != 0 {
		t.Fatalf("opened %v after a refusal", opened)
	}
	p := &player{permissions: []string{api.PermSystemAdmin}, features: ready}
	result, err := LaunchPlayer(context.Background(), p, in, browser)
	want := "https://srv/play/t%2F1?profile_id=p"
	if err != nil || result != (PlayResult{URL: want, Opened: true, TimelineID: "t/1"}) || len(opened) != 1 || opened[0] != want {
		t.Fatalf("%+v %v %v", result, err, opened)
	}
	if strings.Join(p.calls, ",") != "me,capabilities,timeline t/1,profile p" {
		t.Fatalf("calls %v", p.calls)
	}
	in.Open = false
	if result, err := LaunchPlayer(context.Background(), p, in, nil); err != nil || result.Opened || len(opened) != 1 {
		t.Fatalf("print-only launch: %+v %v", result, err)
	}
	in.Open = true
	var invalid *InvalidInputError
	if _, err := LaunchPlayer(context.Background(), p, in, nil); !errors.As(err, &invalid) {
		t.Fatalf("no browser: %v", err)
	}
}
