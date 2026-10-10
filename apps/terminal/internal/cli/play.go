package cli

import (
	"context"
	"fmt"
	"io"
	"net/url"
	"os/exec"
	"runtime"
	"strings"

	"github.com/Jagalite/motion/apps/terminal/internal/api"
	"github.com/spf13/cobra"
)

// PlayerURL contains selection only. The browser authenticates independently;
// a CLI credential is never transferred to a URL, process argument or page.
func PlayerURL(server, timeline, profile string) (string, error) {
	u, err := url.Parse(server)
	if err != nil || u.Host == "" || u.User != nil || (u.Scheme != "http" && u.Scheme != "https") {
		return "", usage("invalid player server URL")
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
func OpenBrowser(ctx context.Context, target string) error {
	var command *exec.Cmd
	switch runtime.GOOS {
	case "darwin":
		command = exec.CommandContext(ctx, "open", target)
	case "linux":
		command = exec.CommandContext(ctx, "xdg-open", target)
	case "windows":
		command = exec.CommandContext(ctx, "rundll32", "url.dll,FileProtocolHandler", target)
	default:
		return usage("browser opening is unsupported; use --print-url")
	}
	return command.Run()
}
func playCmd(env *Env) *cobra.Command {
	var printOnly bool
	cmd := &cobra.Command{Use: "play TIMELINE_ID", Short: "Open the authorized browser player (browser signs in separately)", Args: cobra.ExactArgs(1)}
	cmd.Flags().BoolVar(&printOnly, "print-url", false, "print the token-free player URL without opening a browser")
	cmd.RunE = func(cmd *cobra.Command, args []string) error {
		return run(env, func(ctx context.Context, c *Conn) error {
			who, err := c.Me(ctx)
			if err != nil {
				return err
			}
			if !who.Allows(api.PermPlaybackRequest) {
				return &api.Problem{Status: 403, Code: "forbidden", Detail: "playback:request permission is required"}
			}
			caps, err := c.Capabilities(ctx)
			if err != nil {
				return err
			}
			ready := false
			for _, f := range caps.Features {
				if f.ID == "playback.v2" && f.Implemented && f.Enabled {
					ready = true
				}
			}
			if !ready {
				return &api.Problem{Status: 503, Code: "feature_unavailable", Detail: "this server has not enabled the v2 player"}
			}
			timeline, err := c.GetTimeline(ctx, args[0])
			if err != nil {
				return err
			}
			if strings.TrimSpace(timeline.Value.ID) == "" {
				return fmt.Errorf("server returned an invalid timeline")
			}
			if env.Profile != "" {
				if _, err := c.GetProfile(ctx, env.Profile); err != nil {
					return err
				}
			}
			target, err := PlayerURL(c.URL, timeline.Value.ID, env.Profile)
			if err != nil {
				return err
			}
			if !printOnly {
				if err := OpenBrowser(ctx, target); err != nil {
					return err
				}
			}
			return env.print(map[string]any{"url": target, "opened": !printOnly, "timeline_id": timeline.Value.ID}, func(w io.Writer) { fmt.Fprintln(w, target) })
		})(cmd, args)
	}
	return cmd
}
