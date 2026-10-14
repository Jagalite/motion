package cli

import (
	"context"
	"fmt"
	"io"
	"strings"

	"github.com/Jagalite/motion/apps/terminal/internal/api"
	"github.com/Jagalite/motion/apps/terminal/internal/operations"
	"github.com/spf13/cobra"
)

func librariesCmd(env *Env) *cobra.Command {
	cmd := &cobra.Command{Use: "libraries", Short: "Browse and manage libraries"}
	var cursor string
	var limit int
	list := &cobra.Command{
		Use: "list", Args: cobra.NoArgs,
		RunE: run(env, func(ctx context.Context, c *Conn) error {
			page, err := operations.ListLibraries(ctx, c, operations.PageRequest{Cursor: cursor, Limit: limit})
			if err != nil {
				return err
			}
			return env.print(page, func(w io.Writer) {
				rows := [][]string{}
				for _, l := range page.Items {
					rows = append(rows, []string{l.ID, l.Name, l.Kind, l.Availability})
				}
				table(w, "ID\tNAME\tKIND\tAVAILABILITY", rows)
			})
		}),
	}
	pageFlags(list, &cursor, &limit)
	var items int
	show := &cobra.Command{Use: "show LIBRARY_ID", Short: "Show a library and its titles", Args: cobra.ExactArgs(1), RunE: func(cmd *cobra.Command, args []string) error {
		return run(env, func(ctx context.Context, c *Conn) error {
			contents, truncated, err := operations.ReadLibraryContents(ctx, c, args[0], items)
			if err != nil {
				return err
			}
			out := map[string]any{"library": contents.Library, "items": contents.Items, "truncated": truncated}
			return env.print(out, func(w io.Writer) {
				l := contents.Library
				fmt.Fprintf(w, "Library %s (%s) · %s · %s\nSources %s\n", l.Name, l.ID, l.Kind, l.Availability, strings.Join(l.Sources, ", "))
				printItems(w, contents.Items)
				if truncated {
					fmt.Fprintf(w, "Showing the first %d titles; use `motion catalog list --library %s` to page through all.\n", len(contents.Items), l.ID)
				}
			})
		})(cmd, args)
	}}
	show.Flags().IntVar(&items, "items", 2000, "read at most this many titles")
	cmd.AddCommand(list, show, libraryAddCmd(env), scanCmd(env))
	return cmd
}

func libraryAddCmd(env *Env) *cobra.Command {
	var kind, language, key string
	var sources []string
	cmd := &cobra.Command{Use: "add NAME", Short: "Create a logical library from registered sources", Args: cobra.ExactArgs(1)}
	cmd.Flags().StringVar(&kind, "kind", "mixed", "movies, television, music, photos, personal_video or mixed")
	cmd.Flags().StringVar(&language, "language", "", "metadata language")
	cmd.Flags().StringSliceVar(&sources, "source", []string{}, "registered source IDs (repeatable)")
	keyFlag(cmd, &key)
	cmd.RunE = func(cmd *cobra.Command, args []string) error {
		intent, err := operations.ResumeIntent("creation of library "+args[0], key)
		if err != nil {
			return err
		}
		return run(env, func(ctx context.Context, c *Conn) error {
			result, err := operations.CreateLibrary(ctx, c, api.LibraryInput{Name: args[0], Kind: kind, Language: language, SourceIDs: sources}, intent)
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

func scanCmd(env *Env) *cobra.Command {
	var verify, complete bool
	var sources []string
	var key string
	cmd := &cobra.Command{Use: "scan LIBRARY_ID", Short: "Request a fresh source observation", Args: cobra.ExactArgs(1)}
	cmd.Flags().BoolVar(&verify, "verify", false, "verify file content")
	cmd.Flags().BoolVar(&complete, "require-complete", false, "require complete source coverage")
	cmd.Flags().StringSliceVar(&sources, "source", nil, "source IDs; omitted means every source in the library")
	keyFlag(cmd, &key)
	cmd.RunE = func(cmd *cobra.Command, args []string) error {
		intent, err := operations.ResumeIntent("scan of library "+args[0], key)
		if err != nil {
			return err
		}
		var selection *[]string
		if cmd.Flags().Changed("source") {
			selection = &sources
		}
		return run(env, func(ctx context.Context, c *Conn) error {
			result, err := operations.RequestScan(ctx, c, operations.ScanRequest{LibraryID: args[0], Verify: verify, SourceIDs: selection, RequireComplete: complete}, intent)
			if err != nil {
				return err
			}
			return env.print(result.Value, func(w io.Writer) {
				fmt.Fprintf(w, "Scan %s: %s\n", sanitize(result.Value.ID), sanitize(result.Value.Status))
			})
		})(cmd, args)
	}
	return cmd
}

func printScan(w io.Writer, s api.Scan) {
	fmt.Fprintf(w, "Scan %s of library %s · %s · complete %v\nJobs %s\n", s.ID, s.LibraryID, s.Status, s.Complete, strings.Join(s.JobIDs, ", "))
	rows := [][]string{}
	for _, src := range s.Sources {
		rows = append(rows, []string{src.SourceID, src.Status, src.ObservedFiles, src.VerifiedFiles, strings.Join(src.ErrorCodes, ",")})
	}
	if len(rows) > 0 {
		table(w, "SOURCE\tSTATUS\tOBSERVED\tVERIFIED\tERRORS", rows)
	}
}

func scansCmd(env *Env) *cobra.Command {
	cmd := &cobra.Command{Use: "scans", Short: "Inspect scan requests (request one with `libraries scan`)"}
	var cursor string
	var limit int
	list := &cobra.Command{Use: "list", Args: cobra.NoArgs, RunE: run(env, func(ctx context.Context, c *Conn) error {
		page, err := operations.ListScans(ctx, c, operations.PageRequest{Cursor: cursor, Limit: limit})
		if err != nil {
			return err
		}
		return env.print(page, func(w io.Writer) {
			rows := [][]string{}
			for _, s := range page.Items {
				rows = append(rows, []string{s.ID, s.LibraryID, s.Status, yes(s.Complete)})
			}
			table(w, "ID\tLIBRARY\tSTATUS\tCOMPLETE", rows)
			more(w, page.NextCursor)
		})
	})}
	pageFlags(list, &cursor, &limit)
	show := &cobra.Command{Use: "show SCAN_ID", Args: cobra.ExactArgs(1), RunE: func(cmd *cobra.Command, args []string) error {
		return run(env, func(ctx context.Context, c *Conn) error {
			s, err := operations.InspectScan(ctx, c, args[0])
			if err != nil {
				return err
			}
			return env.print(s.Value, func(w io.Writer) { printScan(w, s.Value) })
		})(cmd, args)
	}}
	cmd.AddCommand(list, show)
	return cmd
}

func sourcesCmd(env *Env) *cobra.Command {
	cmd := &cobra.Command{Use: "sources", Short: "Manage registered server-local storage sources"}
	var cursor string
	var limit int
	list := &cobra.Command{Use: "list", Args: cobra.NoArgs, RunE: run(env, func(ctx context.Context, c *Conn) error {
		result, err := operations.ListSources(ctx, c, operations.PageRequest{Cursor: cursor, Limit: limit})
		if err != nil {
			return err
		}
		return env.print(result, func(w io.Writer) {
			rows := [][]string{}
			for _, s := range result.Items {
				rows = append(rows, []string{s.ID, s.Name, s.RootPath, s.Availability})
			}
			table(w, "ID\tNAME\tROOT\tAVAILABILITY", rows)
			more(w, result.NextCursor)
		})
	})}
	pageFlags(list, &cursor, &limit)
	var root, key string
	var exclusions []string
	add := &cobra.Command{Use: "add NAME", Args: cobra.ExactArgs(1), Short: "Register a path on the server host"}
	add.Flags().StringVar(&root, "root", "", "absolute source path on the server host")
	add.Flags().StringSliceVar(&exclusions, "exclude", []string{}, "source-relative exclusions")
	_ = add.MarkFlagRequired("root")
	keyFlag(add, &key)
	add.RunE = func(cmd *cobra.Command, args []string) error {
		intent, err := operations.ResumeIntent("registration of source "+args[0], key)
		if err != nil {
			return err
		}
		return run(env, func(ctx context.Context, c *Conn) error {
			result, err := operations.CreateSource(ctx, c, api.SourceInput{Name: args[0], RootPath: root, Exclusions: exclusions}, intent)
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
			result, err := operations.InspectSource(ctx, c, args[0])
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

func printItems(w io.Writer, items []api.CatalogItem) {
	rows := [][]string{}
	for _, item := range items {
		rows = append(rows, []string{item.ID, item.Title, item.Kind, item.Availability})
	}
	table(w, "ID\tTITLE\tKIND\tAVAILABILITY", rows)
}

func catalogCmd(env *Env) *cobra.Command {
	cmd := &cobra.Command{Use: "catalog", Short: "Browse the authorized logical catalog"}
	var cursor, library string
	var limit int
	printPage := func(result api.Page[api.CatalogItem]) error {
		return env.print(result, func(w io.Writer) {
			printItems(w, result.Items)
			more(w, result.NextCursor)
		})
	}
	list := &cobra.Command{Use: "list", Args: cobra.NoArgs, RunE: run(env, func(ctx context.Context, c *Conn) error {
		result, err := operations.ListCatalog(ctx, c, library, operations.PageRequest{Cursor: cursor, Limit: limit})
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
			result, err := operations.SearchCatalog(ctx, c, args[0], operations.PageRequest{Cursor: searchCursor, Limit: searchLimit})
			if err != nil {
				return err
			}
			return printPage(result)
		})(cmd, args)
	}}
	pageFlags(search, &searchCursor, &searchLimit)
	var details bool
	show := &cobra.Command{Use: "show ITEM_ID", Args: cobra.ExactArgs(1), RunE: func(cmd *cobra.Command, args []string) error {
		return run(env, func(ctx context.Context, c *Conn) error {
			if details {
				d, err := operations.ReadItemDetails(ctx, c, args[0], env.Profile, 2000)
				if err != nil {
					return err
				}
				return env.print(d, func(w io.Writer) { printDetails(w, d) })
			}
			result, err := operations.InspectItem(ctx, c, args[0])
			if err != nil {
				return err
			}
			return env.print(result.Value, func(w io.Writer) {
				printItems(w, []api.CatalogItem{result.Value})
			})
		})(cmd, args)
	}}
	show.Flags().BoolVar(&details, "details", false, "include editions, timelines, versions, children and the --profile's viewing state, as the item page shows")
	resume := &cobra.Command{Use: "continue", Short: "Continue watching for the --profile", Args: cobra.NoArgs, RunE: run(env, func(ctx context.Context, c *Conn) error {
		if env.Profile == "" {
			return usage("--profile (or $MOTION_PROFILE) selects whose continue-watching list to read")
		}
		entries, truncated, err := operations.ContinueWatching(ctx, c, env.Profile)
		if err != nil {
			return err
		}
		return env.print(map[string]any{"profile_id": env.Profile, "items": entries, "truncated": truncated}, func(w io.Writer) {
			rows := [][]string{}
			for _, e := range entries {
				rows = append(rows, []string{e.Item.ID, e.Item.Title, e.Timeline.ID, clock(e.Viewing.PositionMS)})
			}
			table(w, "ITEM\tTITLE\tTIMELINE\tPOSITION", rows)
		})
	})}
	cmd.AddCommand(list, search, show, resume)
	return cmd
}

func clock(ms int64) string {
	s := ms / 1000
	if s >= 3600 {
		return fmt.Sprintf("%d:%02d:%02d", s/3600, s/60%60, s%60)
	}
	return fmt.Sprintf("%d:%02d", s/60, s%60)
}

func printDetails(w io.Writer, d operations.ItemDetails) {
	printItems(w, []api.CatalogItem{d.Item})
	for _, t := range d.Timelines {
		viewing := "viewing unavailable"
		if t.Viewing != nil {
			viewing = "at " + clock(t.Viewing.PositionMS)
			if t.Viewing.Watched {
				viewing = "watched"
			}
		}
		fmt.Fprintf(w, "Timeline %s · %s · %d versions · %s\n", t.Timeline.ID, t.Edition, len(t.Versions), viewing)
	}
	if len(d.Children) > 0 {
		fmt.Fprintln(w, "Children:")
		printItems(w, d.Children)
	}
	if d.ChildrenTruncated {
		fmt.Fprintln(w, "More children exist than shown.")
	}
}
