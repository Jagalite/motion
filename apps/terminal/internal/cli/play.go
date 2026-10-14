package cli

import (
	"context"
	"errors"
	"fmt"
	"io"

	"github.com/Jagalite/motion/apps/terminal/internal/operations"
	"github.com/Jagalite/motion/apps/terminal/internal/platform"
	"github.com/spf13/cobra"
)

func playCmd(env *Env) *cobra.Command {
	var printOnly bool
	cmd := &cobra.Command{Use: "play TIMELINE_ID", Short: "Open the authorized browser player (browser signs in separately)", Args: cobra.ExactArgs(1)}
	cmd.Flags().BoolVar(&printOnly, "print-url", false, "print the token-free player URL without opening a browser")
	cmd.RunE = func(cmd *cobra.Command, args []string) error {
		return run(env, func(ctx context.Context, c *Conn) error {
			result, err := operations.LaunchPlayer(ctx, c, operations.PlayInput{Server: c.URL, TimelineID: args[0], ProfileID: env.Profile, Open: !printOnly}, env.Browser)
			if errors.Is(err, platform.ErrBrowserUnsupported) {
				return usage("browser opening is unsupported; use --print-url")
			}
			if err != nil {
				return err
			}
			return env.print(result, func(w io.Writer) { fmt.Fprintln(w, result.URL) })
		})(cmd, args)
	}
	return cmd
}
