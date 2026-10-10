package cli

import (
	"context"
	"fmt"
	"github.com/Jagalite/motion/apps/terminal/internal/api"
	"github.com/spf13/cobra"
	"io"
)

func scanCmd(env *Env) *cobra.Command {
	var verify, complete bool
	var sources []string
	cmd := &cobra.Command{Use: "scan LIBRARY_ID", Short: "Request a fresh source observation", Args: cobra.ExactArgs(1)}
	cmd.Flags().BoolVar(&verify, "verify", false, "verify file content")
	cmd.Flags().BoolVar(&complete, "require-complete", false, "require complete source coverage")
	cmd.Flags().StringSliceVar(&sources, "source", nil, "source IDs; omitted means every source in the library")
	cmd.RunE = func(cmd *cobra.Command, args []string) error {
		key := api.NewIdempotencyKey()
		var selection *[]string
		if cmd.Flags().Changed("source") {
			selection = &sources
		}
		return run(env, func(ctx context.Context, c *Conn) error {
			mode := "incremental"
			if verify {
				mode = "verify"
			}
			result, err := c.CreateScan(ctx, args[0], api.ScanInput{Mode: mode, SourceIDs: selection, RequireComplete: complete}, key)
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
func jobsCmd(env *Env) *cobra.Command {
	cmd := &cobra.Command{Use: "jobs", Short: "Inspect, cancel and retry authorized durable jobs"}
	var cursor string
	var limit int
	printJob := func(j api.Job) error {
		return env.print(j, func(w io.Writer) {
			table(w, "ID\tKIND\tPHASE\tATTEMPT", [][]string{{j.ID, j.Kind, j.Phase, j.AttemptGeneration}})
		})
	}
	list := &cobra.Command{Use: "list", Args: cobra.NoArgs, RunE: run(env, func(ctx context.Context, c *Conn) error {
		result, err := c.ListJobs(ctx, cursor, limit)
		if err != nil {
			return err
		}
		return env.print(result, func(w io.Writer) {
			rows := [][]string{}
			for _, j := range result.Items {
				rows = append(rows, []string{j.ID, j.Kind, j.Phase, j.AttemptGeneration})
			}
			table(w, "ID\tKIND\tPHASE\tATTEMPT", rows)
		})
	})}
	pageFlags(list, &cursor, &limit)
	show := &cobra.Command{Use: "show JOB_ID", Args: cobra.ExactArgs(1), RunE: func(cmd *cobra.Command, args []string) error {
		return run(env, func(ctx context.Context, c *Conn) error {
			r, err := c.GetJob(ctx, args[0])
			if err != nil {
				return err
			}
			return printJob(r.Value)
		})(cmd, args)
	}}
	cancel := &cobra.Command{Use: "cancel JOB_ID", Args: cobra.ExactArgs(1), RunE: func(cmd *cobra.Command, args []string) error {
		key := api.NewIdempotencyKey()
		return run(env, func(ctx context.Context, c *Conn) error {
			r, err := c.CancelJob(ctx, args[0], key)
			if err != nil {
				return err
			}
			return printJob(r.Value)
		})(cmd, args)
	}}
	retry := &cobra.Command{Use: "retry JOB_ID", Short: "Retry a failed or cancelled job", Args: cobra.ExactArgs(1), RunE: func(cmd *cobra.Command, args []string) error {
		key := api.NewIdempotencyKey()
		return run(env, func(ctx context.Context, c *Conn) error {
			r, err := c.RetryJob(ctx, args[0], key)
			if err != nil {
				return err
			}
			return printJob(r.Value)
		})(cmd, args)
	}}
	cmd.AddCommand(list, show, cancel, retry)
	return cmd
}
func diagnosticsCmd(env *Env) *cobra.Command {
	return &cobra.Command{Use: "diagnostics", Short: "Read authorized server diagnostics", Args: cobra.NoArgs, RunE: run(env, func(ctx context.Context, c *Conn) error {
		d, err := c.Diagnostics(ctx)
		if err != nil {
			return err
		}
		return env.print(d, func(w io.Writer) {
			table(w, "METRIC\tVALUE", [][]string{{"Uptime seconds", d.UptimeSeconds}, {"Active deliveries", d.ActiveDeliveries}, {"Queued jobs", d.QueuedJobs}, {"Running jobs", d.RunningJobs}, {"Database busy", d.DBBusyCount}})
			for _, e := range d.WorkerErrors {
				fmt.Fprintln(w, sanitize(e))
			}
		})
	})}
}
