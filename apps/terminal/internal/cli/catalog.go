package cli

import (
	"context"
	"fmt"
	"io"

	"github.com/Jagalite/motion/apps/terminal/internal/api"
	"github.com/spf13/cobra"
)

func libraryAddCmd(env *Env) *cobra.Command {
	var kind, language string
	var sources []string
	cmd := &cobra.Command{Use: "add NAME", Short: "Create a logical library from registered sources", Args: cobra.ExactArgs(1)}
	cmd.Flags().StringVar(&kind, "kind", "mixed", "movies, television, music, photos, personal_video or mixed")
	cmd.Flags().StringVar(&language, "language", "", "metadata language")
	cmd.Flags().StringSliceVar(&sources, "source", []string{}, "registered source IDs (repeatable)")
	cmd.RunE = func(cmd *cobra.Command, args []string) error {
		key := api.NewIdempotencyKey()
		return run(env, func(ctx context.Context, c *Conn) error {
			result, err := c.CreateLibrary(ctx, api.LibraryInput{Name: args[0], Kind: kind, Language: language, SourceIDs: sources}, key)
			if err != nil {
				return err
			}
			return env.print(result.Value, func(w io.Writer) {
				fmt.Fprintf(w, "Created library %s (%s)\n", sanitize(result.Value.Name), sanitize(result.Value.ID))
			})
		})(cmd, args)
	}
	return cmd
}
func sourcesCmd(env *Env) *cobra.Command {
	cmd := &cobra.Command{Use: "sources", Short: "Manage registered server-local storage sources"}
	var cursor string
	var limit int
	list := &cobra.Command{Use: "list", Args: cobra.NoArgs, RunE: run(env, func(ctx context.Context, c *Conn) error {
		result, err := c.ListSources(ctx, cursor, limit)
		if err != nil {
			return err
		}
		return env.print(result, func(w io.Writer) {
			rows := [][]string{}
			for _, s := range result.Items {
				rows = append(rows, []string{s.ID, s.Name, s.RootPath, s.Availability})
			}
			table(w, "ID\tNAME\tROOT\tAVAILABILITY", rows)
		})
	})}
	pageFlags(list, &cursor, &limit)
	var root string
	var exclusions []string
	add := &cobra.Command{Use: "add NAME", Args: cobra.ExactArgs(1), Short: "Register a path on the server host"}
	add.Flags().StringVar(&root, "root", "", "absolute source path on the server host")
	add.Flags().StringSliceVar(&exclusions, "exclude", []string{}, "source-relative exclusions")
	_ = add.MarkFlagRequired("root")
	add.RunE = func(cmd *cobra.Command, args []string) error {
		key := api.NewIdempotencyKey()
		return run(env, func(ctx context.Context, c *Conn) error {
			result, err := c.CreateSource(ctx, api.SourceInput{Name: args[0], RootPath: root, Exclusions: exclusions}, key)
			if err != nil {
				return err
			}
			return env.print(result.Value, func(w io.Writer) {
				fmt.Fprintf(w, "Registered source %s (%s)\n", sanitize(result.Value.Name), sanitize(result.Value.ID))
			})
		})(cmd, args)
	}
	show := &cobra.Command{Use: "show SOURCE_ID", Args: cobra.ExactArgs(1), RunE: func(cmd *cobra.Command, args []string) error {
		return run(env, func(ctx context.Context, c *Conn) error {
			result, err := c.GetSource(ctx, args[0])
			if err != nil {
				return err
			}
			return env.print(result.Value, func(w io.Writer) {
				table(w, "ID\tNAME\tROOT\tAVAILABILITY", [][]string{{result.Value.ID, result.Value.Name, result.Value.RootPath, result.Value.Availability}})
			})
		})(cmd, args)
	}}
	cmd.AddCommand(list, add, show)
	return cmd
}
func catalogCmd(env *Env) *cobra.Command {
	cmd := &cobra.Command{Use: "catalog", Short: "Browse the authorized logical catalog"}
	var cursor, library string
	var limit int
	printPage := func(result api.Page[api.CatalogItem]) error {
		return env.print(result, func(w io.Writer) {
			rows := [][]string{}
			for _, item := range result.Items {
				rows = append(rows, []string{item.ID, item.Title, item.Kind, item.Availability})
			}
			table(w, "ID\tTITLE\tKIND\tAVAILABILITY", rows)
		})
	}
	list := &cobra.Command{Use: "list", Args: cobra.NoArgs, RunE: run(env, func(ctx context.Context, c *Conn) error {
		result, err := c.ListCatalog(ctx, cursor, limit, library)
		if err != nil {
			return err
		}
		return printPage(result)
	})}
	pageFlags(list, &cursor, &limit)
	list.Flags().StringVar(&library, "library", "", "logical library ID")
	var searchCursor string
	var searchLimit int
	search := &cobra.Command{Use: "search TEXT", Args: cobra.ExactArgs(1), RunE: func(cmd *cobra.Command, args []string) error {
		return run(env, func(ctx context.Context, c *Conn) error {
			result, err := c.SearchCatalog(ctx, args[0], searchCursor, searchLimit)
			if err != nil {
				return err
			}
			return printPage(result)
		})(cmd, args)
	}}
	pageFlags(search, &searchCursor, &searchLimit)
	show := &cobra.Command{Use: "show ITEM_ID", Args: cobra.ExactArgs(1), RunE: func(cmd *cobra.Command, args []string) error {
		return run(env, func(ctx context.Context, c *Conn) error {
			result, err := c.GetCatalogItem(ctx, args[0])
			if err != nil {
				return err
			}
			return env.print(result.Value, func(w io.Writer) {
				table(w, "ID\tTITLE\tKIND\tAVAILABILITY", [][]string{{result.Value.ID, result.Value.Title, result.Value.Kind, result.Value.Availability}})
			})
		})(cmd, args)
	}}
	cmd.AddCommand(list, search, show)
	return cmd
}
