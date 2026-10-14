package operations

import (
	"context"
	"fmt"
	"net/url"
	"strings"

	"github.com/Jagalite/motion/apps/terminal/internal/api"
)

// PlaybackFeature is the capability that offers the v2 browser player.
const PlaybackFeature = "playback.v2"

// PlaybackAPI is what a player launch reads.
type PlaybackAPI interface {
	Me(context.Context) (api.Principal, error)
	Capabilities(context.Context) (api.Capabilities, error)
	GetTimeline(context.Context, string) (api.Tagged[api.Timeline], error)
	GetProfile(context.Context, string) (api.Tagged[api.Profile], error)
}

// Browser opens a URL in the platform browser. It is injected by the
// executable's composition root.
type Browser func(ctx context.Context, url string) error

// PlayInput selects what to play. Server is the verified session URL.
type PlayInput struct {
	Server     string
	TimelineID string
	ProfileID  string
	// Open asks for the URL to be opened with the Browser; otherwise the
	// token-free URL is only returned.
	Open bool
}

// PlayResult is the launched or printable player URL.
type PlayResult struct {
	URL        string `json:"url"`
	Opened     bool   `json:"opened"`
	TimelineID string `json:"timeline_id"`
}

// PlaybackReady interprets the principal and capabilities for discovery. It
// grants nothing: the server authorizes the player's own requests.
func PlaybackReady(me api.Principal, caps api.Capabilities) error {
	if !me.Allows(api.PermPlaybackRequest) {
		return &api.Problem{Status: 403, Code: "forbidden", Detail: "playback:request permission is required"}
	}
	if f, ok := caps.Feature(PlaybackFeature); !ok || !f.Implemented || !f.Enabled {
		return &api.Problem{Status: 503, Code: "feature_unavailable", Detail: "this server has not enabled the v2 player"}
	}
	return nil
}

// PlayerURL contains selection only. The browser authenticates independently;
// a client credential is never transferred to a URL, process argument or page.
func PlayerURL(server, timeline, profile string) (string, error) {
	u, err := url.Parse(server)
	if err != nil || u.Host == "" || u.User != nil || (u.Scheme != "http" && u.Scheme != "https") {
		return "", Invalid("invalid player server URL")
	}
	u.Path = "/play/" + timeline
	u.RawPath = "/play/" + url.PathEscape(timeline)
	u.RawQuery = ""
	u.Fragment = ""
	if profile != "" {
		q := url.Values{}
		q.Set("profile_id", profile)
		u.RawQuery = q.Encode()
	}
	return u.String(), nil
}

// LaunchPlayer checks that the server offers the player to this principal,
// that the timeline and profile resolve, and then builds the token-free
// player URL and opens it when asked.
func LaunchPlayer(ctx context.Context, a PlaybackAPI, in PlayInput, open Browser) (PlayResult, error) {
	me, err := a.Me(ctx)
	if err != nil {
		return PlayResult{}, err
	}
	caps, err := a.Capabilities(ctx)
	if err != nil {
		return PlayResult{}, err
	}
	if err := PlaybackReady(me, caps); err != nil {
		return PlayResult{}, err
	}
	timeline, err := a.GetTimeline(ctx, in.TimelineID)
	if err != nil {
		return PlayResult{}, err
	}
	if strings.TrimSpace(timeline.Value.ID) == "" {
		return PlayResult{}, fmt.Errorf("server returned an invalid timeline")
	}
	if in.ProfileID != "" {
		if _, err := a.GetProfile(ctx, in.ProfileID); err != nil {
			return PlayResult{}, err
		}
	}
	target, err := PlayerURL(in.Server, timeline.Value.ID, in.ProfileID)
	if err != nil {
		return PlayResult{}, err
	}
	result := PlayResult{URL: target, TimelineID: timeline.Value.ID}
	if in.Open {
		if open == nil {
			return PlayResult{}, Invalid("no browser is available; print the URL instead")
		}
		// A launch cancelled meanwhile (for example by sign-out) opens nothing.
		if err := ctx.Err(); err != nil {
			return PlayResult{}, err
		}
		if err := open(ctx, target); err != nil {
			return PlayResult{}, err
		}
		result.Opened = true
	}
	return result, nil
}
