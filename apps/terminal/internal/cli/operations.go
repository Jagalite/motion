package cli

import (
	"context"
	"fmt"
	"io"
	"time"

	"github.com/Jagalite/motion/apps/terminal/internal/api"
	"github.com/Jagalite/motion/apps/terminal/internal/operations"
	"github.com/spf13/cobra"
)

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
		result, err := operations.ListJobs(ctx, c, operations.PageRequest{Cursor: cursor, Limit: limit})
		if err != nil {
			return err
		}
		return env.print(result, func(w io.Writer) {
			rows := [][]string{}
			for _, j := range result.Items {
				rows = append(rows, []string{j.ID, j.Kind, j.Phase, j.AttemptGeneration})
			}
			table(w, "ID\tKIND\tPHASE\tATTEMPT", rows)
			if result.NextCursor != nil {
				fmt.Fprintf(w, "More: --cursor %s\n", sanitize(*result.NextCursor))
			}
		})
	})}
	pageFlags(list, &cursor, &limit)
	show := &cobra.Command{Use: "show JOB_ID", Args: cobra.ExactArgs(1), RunE: func(cmd *cobra.Command, args []string) error {
		return run(env, func(ctx context.Context, c *Conn) error {
			r, err := operations.InspectJob(ctx, c, args[0])
			if err != nil {
				return err
			}
			return printJob(r.Value)
		})(cmd, args)
	}}
	// action resolves the intent before connecting: one key per invocation,
	// or the recorded key of an earlier attempt whose outcome is unknown.
	action := func(kind operations.JobActionKind, job, key, ifMatch string) (operations.JobAction, error) {
		if key != "" {
			return operations.ResumeJobAction(kind, job, key, ifMatch)
		}
		return operations.NewJobAction(kind, job, ifMatch)
	}
	var cancelKey string
	cancel := &cobra.Command{Use: "cancel JOB_ID", Short: "Request cancellation of a job", Args: cobra.ExactArgs(1), RunE: func(cmd *cobra.Command, args []string) error {
		a, err := action(operations.CancelJob, args[0], cancelKey, "")
		if err != nil {
			return err
		}
		return run(env, func(ctx context.Context, c *Conn) error {
			r, err := operations.SubmitJobAction(ctx, c, a)
			if err != nil {
				return err
			}
			return printJob(r.Value)
		})(cmd, args)
	}}
	cancel.Flags().StringVar(&cancelKey, "idempotency-key", "", "resubmit an earlier request whose outcome is unknown")
	var retryKey, ifMatch string
	retry := &cobra.Command{Use: "retry JOB_ID", Short: "Retry a failed or cancelled job", Args: cobra.ExactArgs(1), RunE: func(cmd *cobra.Command, args []string) error {
		if retryKey != "" {
			if err := operations.CheckIdempotencyKey(retryKey); err != nil {
				return err
			}
			// A resubmission repeats the original request exactly; it never
			// reviews the job again.
			if ifMatch == "" {
				return usage("--idempotency-key needs the original request's --if-match")
			}
		}
		return run(env, func(ctx context.Context, c *Conn) error {
			etag := ifMatch
			if etag == "" {
				// Without --if-match the command reviews the current revision.
				current, err := operations.InspectJob(ctx, c, args[0])
				if err != nil {
					return err
				}
				etag = current.ETag
			}
			a, err := action(operations.RetryJob, args[0], retryKey, etag)
			if err != nil {
				return err
			}
			r, err := operations.SubmitJobAction(ctx, c, a)
			if err != nil {
				return err
			}
			return printJob(r.Value)
		})(cmd, args)
	}}
	retry.Flags().StringVar(&retryKey, "idempotency-key", "", "resubmit an earlier request whose outcome is unknown")
	retry.Flags().StringVar(&ifMatch, "if-match", "", "ETag the job must still have (default: read it first)")
	cmd.AddCommand(list, show, cancel, retry)
	return cmd
}
func diagnosticsCmd(env *Env) *cobra.Command {
	cmd := &cobra.Command{Use: "diagnostics", Short: "Read authorized server diagnostics", Args: cobra.NoArgs, RunE: run(env, func(ctx context.Context, c *Conn) error {
		d, err := operations.ReadDiagnostics(ctx, c)
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
	storage := &cobra.Command{Use: "storage", Short: "Read the server's storage use", Args: cobra.NoArgs, RunE: run(env, func(ctx context.Context, c *Conn) error {
		s, err := operations.ReadStorage(ctx, c)
		if err != nil {
			return err
		}
		return env.print(s, func(w io.Writer) {
			free := "unknown"
			if s.FreeBytes != nil {
				free = *s.FreeBytes
			}
			table(w, "STORAGE\tBYTES", [][]string{{"Database", s.DatabaseBytes}, {"Assets", s.AssetBytes}, {"Temporary", s.TemporaryBytes}, {"Reserved", s.ReservedBytes}, {"Free", free}})
		})
	})}
	var window time.Duration
	metrics := &cobra.Command{Use: "metrics", Short: "Read sampled server history (admin.metrics)", Args: cobra.NoArgs, RunE: run(env, func(ctx context.Context, c *Conn) error {
		h, err := operations.ReadMetrics(ctx, c, operations.MetricsWindow{Window: window, Now: time.Now()})
		if err != nil {
			return err
		}
		return env.print(h, func(w io.Writer) {
			fmt.Fprintf(w, "Sampled %s every %ds (kept %ds)\n", h.SampledAt, h.IntervalSeconds, h.RetentionSeconds)
			rows := [][]string{}
			for _, s := range h.Series {
				latest := "-"
				if n := len(s.Points); n > 0 {
					latest = s.Points[n-1].V
				}
				rows = append(rows, []string{s.ID, s.Kind, s.Unit, fmt.Sprint(len(s.Points)), latest})
			}
			table(w, "SERIES\tKIND\tUNIT\tPOINTS\tLATEST", rows)
		})
	})}
	metrics.Flags().DurationVar(&window, "window", time.Hour, "history to read, ending now")
	cmd.AddCommand(storage, metrics)
	return cmd
}

func playbackCmd(env *Env) *cobra.Command {
	cmd := &cobra.Command{Use: "playback", Short: "Monitor live and finished playback (administrator)"}
	sessions := &cobra.Command{Use: "sessions", Short: "Live deliveries", Args: cobra.NoArgs, RunE: run(env, func(ctx context.Context, c *Conn) error {
		list, err := operations.ReadPlaybackSessions(ctx, c)
		if err != nil {
			return err
		}
		return env.print(list, func(w io.Writer) {
			rows := [][]string{}
			for _, s := range list.Sessions {
				title := "-"
				if s.Item != nil {
					title = s.Item.Title
				}
				rows = append(rows, []string{s.DeliveryID, party(s.Device), party(s.Profile), title, s.Status, s.Mode, s.ThroughputBPS})
			}
			table(w, "DELIVERY\tDEVICE\tPROFILE\tTITLE\tSTATUS\tMODE\tBPS", rows)
			if list.Truncated {
				fmt.Fprintln(w, "More sessions exist than the server returned.")
			}
		})
	})}
	var cursor string
	var limit int
	var filter api.HistoryFilter
	history := &cobra.Command{Use: "history", Short: "Finished playback sessions", Args: cobra.NoArgs, RunE: run(env, func(ctx context.Context, c *Conn) error {
		page, err := operations.ListPlaybackHistory(ctx, c, filter, operations.PageRequest{Cursor: cursor, Limit: limit})
		if err != nil {
			return err
		}
		return env.print(page, func(w io.Writer) {
			rows := [][]string{}
			for _, h := range page.Items {
				title := "-"
				if h.Item != nil {
					title = h.Item.Title
				}
				rows = append(rows, []string{h.DeliveryID, party(h.Device), party(h.Profile), title, h.EndedAt, h.EndReason})
			}
			table(w, "DELIVERY\tDEVICE\tPROFILE\tTITLE\tENDED\tREASON", rows)
			more(w, page.NextCursor)
		})
	})}
	pageFlags(history, &cursor, &limit)
	history.Flags().StringVar(&filter.DeviceID, "device", "", "only this device")
	history.Flags().StringVar(&filter.ProfileID, "profile-id", "", "only this profile")
	history.Flags().StringVar(&filter.ItemID, "item", "", "only this title")
	cmd.AddCommand(sessions, history)
	return cmd
}

func party(p api.PlaybackParty) string {
	if p.Name != nil {
		return *p.Name
	}
	return p.ID + " (removed)"
}
