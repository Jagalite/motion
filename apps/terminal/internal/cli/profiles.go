package cli

import (
	"context"
	"fmt"
	"io"

	"github.com/Jagalite/motion/apps/terminal/internal/operations"
	"github.com/spf13/cobra"
)

func profilesCmd(env *Env) *cobra.Command {
	cmd := &cobra.Command{Use: "profiles", Short: "List and manage viewing profiles"}
	var cursor string
	var limit int
	list := &cobra.Command{
		Use: "list", Args: cobra.NoArgs,
		RunE: run(env, func(ctx context.Context, c *Conn) error {
			page, err := operations.ListProfiles(ctx, c, operations.PageRequest{Cursor: cursor, Limit: limit})
			if err != nil {
				return err
			}
			return env.print(page, func(w io.Writer) {
				rows := [][]string{}
				for _, p := range page.Items {
					rows = append(rows, []string{p.ID, p.Name, p.Revision})
				}
				table(w, "ID\tNAME\tREVISION", rows)
				more(w, page.NextCursor)
			})
		}),
	}
	pageFlags(list, &cursor, &limit)
	show := &cobra.Command{
		Use: "show PROFILE_ID", Args: cobra.ExactArgs(1),
		RunE: func(cmd *cobra.Command, args []string) error {
			return run(env, func(ctx context.Context, c *Conn) error {
				p, err := operations.InspectProfile(ctx, c, args[0])
				if err != nil {
					return err
				}
				return env.print(p.Value, func(w io.Writer) {
					fmt.Fprintf(w, "%s  %s  (revision %s)\n", p.Value.ID, sanitize(p.Value.Name), p.Value.Revision)
				})
			})(cmd, args)
		},
	}
	var key string
	create := &cobra.Command{
		Use: "create NAME", Args: cobra.ExactArgs(1),
		RunE: func(cmd *cobra.Command, args []string) error {
			intent, err := operations.ResumeIntent("creation of profile "+args[0], key)
			if err != nil {
				return err
			}
			return run(env, func(ctx context.Context, c *Conn) error {
				p, err := operations.CreateProfile(ctx, c, args[0], intent)
				if err != nil {
					return err
				}
				return env.print(p.Value, func(w io.Writer) { fmt.Fprintf(w, "Created profile %s (%s)\n", sanitize(p.Value.Name), p.Value.ID) })
			})(cmd, args)
		},
	}
	keyFlag(create, &key)
	var ifMatch string
	rename := &cobra.Command{
		Use: "rename PROFILE_ID NAME", Args: cobra.ExactArgs(2),
		RunE: func(cmd *cobra.Command, args []string) error {
			return run(env, func(ctx context.Context, c *Conn) error {
				p, err := operations.RenameProfile(ctx, c, args[0], args[1], operations.ETagOrCurrent(ifMatch))
				if err != nil {
					return err
				}
				return env.print(p.Value, func(w io.Writer) { fmt.Fprintf(w, "Renamed %s to %s\n", p.Value.ID, sanitize(p.Value.Name)) })
			})(cmd, args)
		},
	}
	remove := &cobra.Command{
		Use: "delete PROFILE_ID", Short: "Remove a profile and its viewing state (media is untouched)", Args: cobra.ExactArgs(1),
		RunE: func(cmd *cobra.Command, args []string) error {
			return run(env, func(ctx context.Context, c *Conn) error {
				if err := operations.DeleteProfile(ctx, c, args[0], operations.ETagOrCurrent(ifMatch)); err != nil {
					return err
				}
				return env.print(map[string]string{"profile_id": args[0], "state": "deleted"}, func(w io.Writer) {
					fmt.Fprintf(w, "Deleted profile %s\n", args[0])
				})
			})(cmd, args)
		},
	}
	for _, c := range []*cobra.Command{rename, remove} {
		c.Flags().StringVar(&ifMatch, "if-match", "", "ETag the profile must still have (default: read it first)")
	}
	cmd.AddCommand(list, show, create, rename, remove)
	return cmd
}
