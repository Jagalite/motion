// Package cli implements the `motion` command line over the public API.
// It has no database, scanner, provider or FFmpeg dependency: every
// operation is an authorized request to the selected server.
package cli

import (
	"context"
	"errors"
	"fmt"
	"io"
	"os"
	"os/exec"
	"path/filepath"
	"strings"
	"text/tabwriter"
	"time"

	"github.com/Jagalite/motion/apps/terminal/internal/api"
	"github.com/Jagalite/motion/apps/terminal/internal/store"
	"github.com/spf13/cobra"
)

// TUI is set by main to avoid an import cycle with the tui package.
var TUI func(ctx context.Context, conn *Conn) error

func NewRoot(env *Env) *cobra.Command {
	root := &cobra.Command{
		Use:           "motion",
		Short:         "Motion terminal client",
		SilenceUsage:  true,
		SilenceErrors: true,
	}
	root.SetOut(env.Out)
	root.SetErr(env.Err)
	root.SetFlagErrorFunc(func(_ *cobra.Command, err error) error { return usage("%v", err) })
	f := root.PersistentFlags()
	f.StringVar(&env.Server, "server", "", "server URL (default: $MOTION_SERVER or the last paired server)")
	f.BoolVar(&env.JSON, "json", false, "write versioned JSON ("+Schema+") to stdout")
	f.DurationVar(&env.Timeout, "timeout", 30*time.Second, "overall request timeout; 0 disables")
	f.StringVar(&env.Profile, "profile", os.Getenv("MOTION_PROFILE"), "viewing profile ID")
	f.StringVar(&env.OperatorTokenFile, "operator-token-file", "", "act as the server operator using its 0600 token file")
	f.StringVar(&env.ConfigDir, "config-dir", "", "configuration directory (default: $MOTION_CONFIG_DIR or the user config dir)")
	root.AddCommand(serverCmd(env), authCmd(env), devicesCmd(env), profilesCmd(env),
		librariesCmd(env), sourcesCmd(env), catalogCmd(env), jobsCmd(env), diagnosticsCmd(env), playCmd(env), eventsCmd(env), tuiCmd(env), serveCmd())
	classifyArgs(root)
	return root
}

// classifyArgs makes positional-argument errors invalid input (exit 2).
func classifyArgs(cmd *cobra.Command) {
	if validate := cmd.Args; validate != nil {
		cmd.Args = func(c *cobra.Command, args []string) error {
			if err := validate(c, args); err != nil {
				return usage("%v", err)
			}
			return nil
		}
	}
	for _, child := range cmd.Commands() {
		classifyArgs(child)
	}
}

// Execute runs the command line. Output mode is resolved before command
// lookup so even an unknown command is reported in the requested format.
func Execute(ctx context.Context, env *Env, args []string) error {
	root := NewRoot(env) // binds flags, resetting env.JSON to its default
	for _, a := range args {
		if a == "--" {
			break
		}
		if a == "--json" || a == "--json=true" {
			env.JSON = true
		}
	}
	root.SetArgs(args)
	err := root.ExecuteContext(ctx)
	if err != nil && strings.HasPrefix(err.Error(), "unknown command") {
		err = usage("%v", err)
	}
	return err
}

// run wraps a command body with the global timeout and a verified connection.
func run(env *Env, body func(ctx context.Context, c *Conn) error) func(*cobra.Command, []string) error {
	return func(cmd *cobra.Command, _ []string) error {
		ctx, cancel := env.context(cmd.Context())
		defer cancel()
		conn, err := env.Connect(ctx)
		if err != nil {
			return err
		}
		return body(ctx, conn)
	}
}

func table(w io.Writer, header string, rows [][]string) {
	tw := tabwriter.NewWriter(w, 0, 4, 2, ' ', 0)
	fmt.Fprintln(tw, header)
	for _, r := range rows {
		for i := range r {
			r[i] = sanitize(strings.ReplaceAll(r[i], "\t", " "))
		}
		fmt.Fprintln(tw, strings.Join(r, "\t"))
	}
	tw.Flush()
}

func serverCmd(env *Env) *cobra.Command {
	cmd := &cobra.Command{Use: "server", Short: "Inspect the selected server"}
	cmd.AddCommand(&cobra.Command{
		Use:   "status",
		Short: "Show liveness, identity and (when signed in) capabilities",
		Args:  cobra.NoArgs,
		RunE: run(env, func(ctx context.Context, c *Conn) error {
			out := map[string]any{"url": c.URL, "health": c.Health}
			var caps *api.Capabilities
			if c.Operator || c.Credential != nil {
				v, err := c.Capabilities(ctx)
				if err != nil {
					return err
				}
				caps = &v
				out["capabilities"] = v
			}
			return env.print(out, func(w io.Writer) {
				fmt.Fprintf(w, "Server   %s\nStatus   %s\nID       %s\nEpoch    %s\n", c.URL, c.Health.Status, c.Health.ServerID, c.Health.ServerEpoch)
				if caps == nil {
					fmt.Fprintln(w, "Not signed in: run `motion auth pair` to see capabilities.")
					return
				}
				fmt.Fprintf(w, "Version  %s (API %s, schema %s)\nContract %s\n", caps.ServerVersion, caps.APIVersion, caps.SchemaVersion, caps.ContractDigest)
				rows := [][]string{}
				for _, f := range caps.Features {
					rows = append(rows, []string{f.ID, yes(f.Implemented), yes(f.Enabled), f.Qualification})
				}
				table(w, "FEATURE\tIMPLEMENTED\tENABLED\tQUALIFICATION", rows)
			})
		}),
	})
	return cmd
}

func yes(b bool) string {
	if b {
		return "yes"
	}
	return "no"
}

func authCmd(env *Env) *cobra.Command {
	cmd := &cobra.Command{Use: "auth", Short: "Pair this device and manage credentials"}
	var name, credentialStore string
	var wait time.Duration
	pair := &cobra.Command{
		Use:   "pair",
		Short: "Pair this device; an administrator approves the displayed code",
		Args:  cobra.NoArgs,
		RunE: func(cmd *cobra.Command, _ []string) error {
			ctx, cancel := context.WithTimeout(cmd.Context(), wait)
			defer cancel()
			conn, err := env.Connect(ctx)
			if err != nil {
				return err
			}
			accepted := credentialStore
			if accepted == "" {
				accepted = conn.Config.CredentialStore
			}
			if accepted != store.FileStore {
				return store.ErrNoStore
			}
			if name == "" {
				name, _ = os.Hostname()
			}
			return pairDevice(ctx, env, conn, name, accepted)
		},
	}
	pair.Flags().StringVar(&name, "name", "", "device name shown to the administrator (default: hostname)")
	pair.Flags().StringVar(&credentialStore, "credential-store", "", "where to keep the credential; only \"file\" (0600) is supported")
	pair.Flags().DurationVar(&wait, "wait", 10*time.Minute, "how long to wait for approval")

	var code string
	var profiles, permissions []string
	approve := &cobra.Command{
		Use:   "approve PAIRING_ID",
		Short: "Approve a pairing with explicit profiles and permissions (administrator)",
		Args:  cobra.ExactArgs(1),
		RunE: func(cmd *cobra.Command, args []string) error {
			if code == "" {
				return usage("--code is required: use the code shown on the pairing device")
			}
			for _, p := range permissions {
				if !contains(api.AllPermissions, p) {
					return usage("unknown permission %q; valid: %s", p, strings.Join(api.AllPermissions, ", "))
				}
			}
			// One key for every retry of this approval.
			key := api.NewIdempotencyKey()
			return run(env, func(ctx context.Context, c *Conn) error {
				d, err := c.ApprovePairing(ctx, args[0], api.PairingApproval{
					UserCode: code, ProfileIDs: nonNil(profiles), Permissions: nonNil(permissions)}, key)
				if err != nil {
					return err
				}
				return env.print(d.Value, func(w io.Writer) {
					fmt.Fprintf(w, "Approved device %s (%s) with %s\n", sanitize(d.Value.Name), d.Value.ID, strings.Join(d.Value.Permissions, ", "))
				})
			})(cmd, args)
		},
	}
	approve.Flags().StringVar(&code, "code", "", "user code displayed by the pairing device")
	approve.Flags().StringSliceVar(&profiles, "grant-profile", nil, "profile ID the device may use (repeatable)")
	approve.Flags().StringSliceVar(&permissions, "permission", nil, "permission to grant (repeatable)")

	status := &cobra.Command{
		Use:   "status",
		Short: "Show the authenticated principal",
		Args:  cobra.NoArgs,
		RunE: run(env, func(ctx context.Context, c *Conn) error {
			me, err := c.Me(ctx)
			if err != nil {
				return err
			}
			if env.Profile != "" && !contains(me.ProfileIDs, env.Profile) {
				return &api.Problem{Status: 403, Code: "profile_not_permitted", Title: "Forbidden",
					Detail: fmt.Sprintf("profile %q is not available to this principal", env.Profile)}
			}
			return env.print(me, func(w io.Writer) {
				device := "-"
				if me.DeviceID != nil {
					device = *me.DeviceID
				}
				fmt.Fprintf(w, "Principal %s (%s)\nDevice    %s\nProfiles  %s\nPermissions %s\nPolicy revision %s\n",
					me.ID, me.Mode, device, strings.Join(me.ProfileIDs, ", "), strings.Join(me.Permissions, ", "), me.PolicyRevision)
			})
		}),
	}
	logout := &cobra.Command{
		Use:   "logout",
		Short: "Forget this server's stored credential (an administrator revokes it server-side)",
		Args:  cobra.NoArgs,
		RunE: run(env, func(_ context.Context, c *Conn) error {
			if err := c.Store.DeleteCredential(c.Health.ServerID); err != nil {
				return err
			}
			return env.print(map[string]string{"server_id": c.Health.ServerID}, func(w io.Writer) {
				fmt.Fprintln(w, "Removed the local credential. Revoke the device with `motion devices revoke` to invalidate it.")
			})
		}),
	}
	cmd.AddCommand(pair, approve, status, logout)
	return cmd
}

func contains(list []string, v string) bool {
	for _, x := range list {
		if x == v {
			return true
		}
	}
	return false
}

func nonNil(v []string) []string {
	if v == nil {
		return []string{}
	}
	return v
}

func pairDevice(ctx context.Context, env *Env, c *Conn, name, accepted string) error {
	p, err := c.CreatePairing(ctx, api.PairingRequest{DeviceName: name, ClientName: "motion-terminal"})
	if err != nil {
		return err
	}
	env.notice(map[string]string{"pairing_id": p.ID, "user_code": p.UserCode, "expires_at": p.ExpiresAt},
		fmt.Sprintf("Pairing requested. On an administrator's terminal run:\n  motion auth approve %s --code %s --permission catalog:read --permission events:read\nWaiting for approval (expires %s)...", p.ID, p.UserCode, p.ExpiresAt))
	interval := time.Duration(max(p.PollIntervalSeconds, 1)) * time.Second
	for attempt := 0; ; attempt++ {
		if err := c.Sleep(ctx, interval); err != nil {
			return err
		}
		cred, err := c.ClaimPairing(ctx, p.ID, p.DeviceCode)
		var problem *api.Problem
		var unavailable *api.UnavailableError
		switch {
		case err == nil:
			if err := c.Store.PutCredential(store.Credential{
				ServerID: c.Health.ServerID, DeviceID: cred.DeviceID,
				AccessToken: cred.AccessToken, ExpiresAt: cred.ExpiresAt}, accepted); err != nil {
				return err
			}
			c.Config.CredentialStore = accepted
			if err := c.Remember(); err != nil {
				return err
			}
			result := map[string]string{"server_id": c.Health.ServerID, "device_id": cred.DeviceID, "expires_at": cred.ExpiresAt}
			return env.print(result, func(w io.Writer) {
				fmt.Fprintf(w, "Paired as device %s with server %s (credential expires %s).\n", cred.DeviceID, c.Health.ServerID, cred.ExpiresAt)
			})
		case errors.As(err, &problem) && problem.Code == "pairing_pending":
			if problem.RetryAfter > 0 {
				interval = problem.RetryAfter
			}
		case errors.As(err, &problem) && problem.Status == 429:
			if problem.RetryAfter > 0 {
				interval = problem.RetryAfter
			}
		case errors.As(err, &unavailable):
			interval = min(interval*2, 30*time.Second)
		default:
			return err
		}
	}
}

func pageFlags(cmd *cobra.Command, cursor *string, limit *int) {
	cmd.Flags().StringVar(cursor, "cursor", "", "continue from a previous page's next_cursor")
	cmd.Flags().IntVar(limit, "limit", 50, "page size (1-200)")
}

func devicesCmd(env *Env) *cobra.Command {
	cmd := &cobra.Command{Use: "devices", Short: "Manage paired devices (administrator)"}
	var cursor string
	var limit int
	list := &cobra.Command{
		Use: "list", Short: "List devices", Args: cobra.NoArgs,
		RunE: run(env, func(ctx context.Context, c *Conn) error {
			page, err := c.ListDevices(ctx, cursor, limit)
			if err != nil {
				return err
			}
			return env.print(page, func(w io.Writer) {
				rows := [][]string{}
				for _, d := range page.Items {
					state := "active"
					if d.Revoked {
						state = "revoked"
					}
					rows = append(rows, []string{d.ID, d.Name, state, strings.Join(d.Permissions, ",")})
				}
				table(w, "ID\tNAME\tSTATE\tPERMISSIONS", rows)
				if page.NextCursor != nil {
					fmt.Fprintf(w, "More: --cursor %s\n", *page.NextCursor)
				}
			})
		}),
	}
	pageFlags(list, &cursor, &limit)
	show := &cobra.Command{
		Use: "show DEVICE_ID", Short: "Show a device", Args: cobra.ExactArgs(1),
		RunE: func(cmd *cobra.Command, args []string) error {
			return run(env, func(ctx context.Context, c *Conn) error {
				d, err := c.GetDevice(ctx, args[0])
				if err != nil {
					return err
				}
				return env.print(d.Value, func(w io.Writer) {
					fmt.Fprintf(w, "Device %s (%s)\nRevision %s  Revoked %v\nProfiles %s\nPermissions %s\n",
						sanitize(d.Value.Name), d.Value.ID, d.Value.Revision, d.Value.Revoked,
						strings.Join(d.Value.ProfileIDs, ", "), strings.Join(d.Value.Permissions, ", "))
				})
			})(cmd, args)
		},
	}
	var ifMatch string
	revoke := &cobra.Command{
		Use: "revoke DEVICE_ID", Short: "Revoke a device's credentials", Args: cobra.ExactArgs(1),
		RunE: func(cmd *cobra.Command, args []string) error {
			return run(env, func(ctx context.Context, c *Conn) error {
				etag := ifMatch
				if etag == "" {
					d, err := c.GetDevice(ctx, args[0])
					if err != nil {
						return err
					}
					etag = d.ETag
				}
				if err := c.RevokeDevice(ctx, args[0], etag); err != nil {
					return err
				}
				return env.print(map[string]string{"device_id": args[0], "state": "revoked"}, func(w io.Writer) {
					fmt.Fprintf(w, "Revoked device %s.\n", args[0])
				})
			})(cmd, args)
		},
	}
	revoke.Flags().StringVar(&ifMatch, "if-match", "", "ETag the device must still have (default: read it first)")
	cmd.AddCommand(list, show, revoke, policyCmd(env))
	return cmd
}

func policyCmd(env *Env) *cobra.Command {
	cmd := &cobra.Command{Use: "policy", Short: "Read or replace a device's access policy"}
	show := &cobra.Command{
		Use: "show DEVICE_ID", Args: cobra.ExactArgs(1),
		RunE: func(cmd *cobra.Command, args []string) error {
			return run(env, func(ctx context.Context, c *Conn) error {
				p, err := c.GetDevicePolicy(ctx, args[0])
				if err != nil {
					return err
				}
				return env.print(p.Value, func(w io.Writer) { printPolicy(w, p.Value) })
			})(cmd, args)
		},
	}
	var in api.AccessPolicy
	var ifMatch string
	set := &cobra.Command{
		Use: "set DEVICE_ID", Short: "Replace the whole policy", Args: cobra.ExactArgs(1),
		RunE: func(cmd *cobra.Command, args []string) error {
			for _, p := range in.Permissions {
				if !contains(api.AllPermissions, p) {
					return usage("unknown permission %q", p)
				}
			}
			return run(env, func(ctx context.Context, c *Conn) error {
				etag := ifMatch
				if etag == "" {
					current, err := c.GetDevicePolicy(ctx, args[0])
					if err != nil {
						return err
					}
					etag = current.ETag
				}
				in.LibraryIDs, in.AllowedRatings = nonNil(in.LibraryIDs), nonNil(in.AllowedRatings)
				in.BlockedLabels, in.Permissions = nonNil(in.BlockedLabels), nonNil(in.Permissions)
				p, err := c.ReplaceDevicePolicy(ctx, args[0], in, etag)
				if err != nil {
					return err
				}
				return env.print(p.Value, func(w io.Writer) { printPolicy(w, p.Value) })
			})(cmd, args)
		},
	}
	set.Flags().StringSliceVar(&in.LibraryIDs, "library", nil, "library the device may see (repeatable)")
	set.Flags().BoolVar(&in.AllowUnrated, "allow-unrated", true, "allow unrated titles")
	set.Flags().StringSliceVar(&in.AllowedRatings, "rating", nil, "allowed rating (repeatable; none means no rating filter)")
	set.Flags().StringSliceVar(&in.BlockedLabels, "block-label", nil, "blocked label (repeatable)")
	set.Flags().StringSliceVar(&in.Permissions, "permission", nil, "permission (repeatable; replaces the current set)")
	set.Flags().StringVar(&ifMatch, "if-match", "", "ETag the policy must still have (default: read it first)")
	cmd.AddCommand(show, set)
	return cmd
}

// printPolicy writes through env.print, whose writer strips control characters.
func printPolicy(w io.Writer, p api.AccessPolicy) {
	fmt.Fprintf(w, "Revision %s\nLibraries %s\nAllow unrated %v\nRatings %s\nBlocked labels %s\nPermissions %s\n",
		p.Revision, strings.Join(p.LibraryIDs, ", "), p.AllowUnrated, strings.Join(p.AllowedRatings, ", "),
		strings.Join(p.BlockedLabels, ", "), strings.Join(p.Permissions, ", "))
}

func profilesCmd(env *Env) *cobra.Command {
	cmd := &cobra.Command{Use: "profiles", Short: "List and manage viewing profiles"}
	var cursor string
	var limit int
	list := &cobra.Command{
		Use: "list", Args: cobra.NoArgs,
		RunE: run(env, func(ctx context.Context, c *Conn) error {
			page, err := c.ListProfiles(ctx, cursor, limit)
			if err != nil {
				return err
			}
			return env.print(page, func(w io.Writer) {
				rows := [][]string{}
				for _, p := range page.Items {
					rows = append(rows, []string{p.ID, p.Name, p.Revision})
				}
				table(w, "ID\tNAME\tREVISION", rows)
				if page.NextCursor != nil {
					fmt.Fprintf(w, "More: --cursor %s\n", *page.NextCursor)
				}
			})
		}),
	}
	pageFlags(list, &cursor, &limit)
	show := &cobra.Command{
		Use: "show PROFILE_ID", Args: cobra.ExactArgs(1),
		RunE: func(cmd *cobra.Command, args []string) error {
			return run(env, func(ctx context.Context, c *Conn) error {
				p, err := c.GetProfile(ctx, args[0])
				if err != nil {
					return err
				}
				return env.print(p.Value, func(w io.Writer) {
					fmt.Fprintf(w, "%s  %s  (revision %s)\n", p.Value.ID, sanitize(p.Value.Name), p.Value.Revision)
				})
			})(cmd, args)
		},
	}
	create := &cobra.Command{
		Use: "create NAME", Args: cobra.ExactArgs(1),
		RunE: func(cmd *cobra.Command, args []string) error {
			key := api.NewIdempotencyKey()
			return run(env, func(ctx context.Context, c *Conn) error {
				p, err := c.CreateProfile(ctx, args[0], key)
				if err != nil {
					return err
				}
				return env.print(p.Value, func(w io.Writer) { fmt.Fprintf(w, "Created profile %s (%s)\n", sanitize(p.Value.Name), p.Value.ID) })
			})(cmd, args)
		},
	}
	var ifMatch string
	conditional := func(ctx context.Context, c *Conn, id string) (string, error) {
		if ifMatch != "" {
			return ifMatch, nil
		}
		p, err := c.GetProfile(ctx, id)
		return p.ETag, err
	}
	rename := &cobra.Command{
		Use: "rename PROFILE_ID NAME", Args: cobra.ExactArgs(2),
		RunE: func(cmd *cobra.Command, args []string) error {
			return run(env, func(ctx context.Context, c *Conn) error {
				etag, err := conditional(ctx, c, args[0])
				if err != nil {
					return err
				}
				p, err := c.ReplaceProfile(ctx, args[0], args[1], etag)
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
				etag, err := conditional(ctx, c, args[0])
				if err != nil {
					return err
				}
				if err := c.DeleteProfile(ctx, args[0], etag); err != nil {
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

func librariesCmd(env *Env) *cobra.Command {
	cmd := &cobra.Command{Use: "libraries", Short: "Browse libraries"}
	var cursor string
	var limit int
	list := &cobra.Command{
		Use: "list", Args: cobra.NoArgs,
		RunE: run(env, func(ctx context.Context, c *Conn) error {
			page, err := c.ListLibraries(ctx, cursor, limit)
			if api.IsCode(err, "not_found") {
				return &api.Problem{Status: 503, Code: "feature_unavailable", Title: "Unavailable",
					Detail: "this server does not implement the v2 catalog yet (capability catalog.v2)"}
			}
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
	cmd.AddCommand(list, libraryAddCmd(env))
	return cmd
}

func eventsCmd(env *Env) *cobra.Command {
	cmd := &cobra.Command{Use: "events", Short: "Watch authorized change hints"}
	var after string
	tail := &cobra.Command{
		Use:   "tail",
		Short: "Stream events, reconnecting with the last cursor",
		Args:  cobra.NoArgs,
		RunE: func(cmd *cobra.Command, _ []string) error {
			// The stream is long-lived: the timeout applies to connecting only.
			connectCtx, cancel := env.context(cmd.Context())
			conn, err := env.Connect(connectCtx)
			cancel()
			if err != nil {
				return err
			}
			return conn.Follow(cmd.Context(), after, func(e api.Event) error {
				if env.JSON {
					return writeJSON(env.Out, envelope{Event: e})
				}
				id := "-"
				if e.ResourceID != nil {
					id = *e.ResourceID
				}
				line := fmt.Sprintf("%s %-8s %s %s", time.Now().Format(time.TimeOnly), e.Kind, e.ResourceType, id)
				if e.Reason != nil {
					line += " (" + *e.Reason + ": discard cached views and re-read)"
				}
				_, err := fmt.Fprintln(env.Out, sanitize(line))
				return err
			}, func(err error, wait time.Duration) {
				env.notice(map[string]string{"disconnected": err.Error(), "retry_in": wait.String()},
					fmt.Sprintf("Disconnected (%v); reconnecting in %s", err, wait))
			})
		},
	}
	tail.Flags().StringVar(&after, "after", "", "resume after this event cursor")
	cmd.AddCommand(tail)
	return cmd
}

func tuiCmd(env *Env) *cobra.Command {
	return &cobra.Command{
		Use:   "tui",
		Short: "Interactive terminal interface",
		Args:  cobra.NoArgs,
		RunE: func(cmd *cobra.Command, _ []string) error {
			if env.JSON {
				return usage("tui has no JSON mode")
			}
			connectCtx, cancel := env.context(cmd.Context())
			conn, err := env.Connect(connectCtx)
			cancel()
			if err != nil {
				return err
			}
			return TUI(cmd.Context(), conn)
		},
	}
}

// serveCmd delegates to the bundled Rust server; there is no Go server.
func serveCmd() *cobra.Command {
	return &cobra.Command{
		Use:                "serve [server flags]",
		Short:              "Run the bundled Motion server (arguments pass through)",
		DisableFlagParsing: true,
		RunE: func(cmd *cobra.Command, args []string) error {
			bin, err := serverBinary()
			if err != nil {
				return err
			}
			child := exec.CommandContext(cmd.Context(), bin, args...)
			child.Stdin, child.Stdout, child.Stderr = os.Stdin, os.Stdout, os.Stderr
			child.Cancel = func() error { return child.Process.Signal(os.Interrupt) }
			child.WaitDelay = 10 * time.Second
			if err := child.Run(); err != nil {
				var exit *exec.ExitError
				if errors.As(err, &exit) {
					os.Exit(exit.ExitCode())
				}
				return err
			}
			return nil
		},
	}
}

func serverBinary() (string, error) {
	if bin := os.Getenv("MOTION_SERVER_BIN"); bin != "" {
		return bin, nil
	}
	self, err := os.Executable()
	if err != nil {
		return "", err
	}
	for _, name := range []string{"motion-server", "playscale"} {
		candidate := filepath.Join(filepath.Dir(self), name)
		if info, err := os.Stat(candidate); err == nil && !info.IsDir() {
			return candidate, nil
		}
	}
	return "", usage("bundled server not found next to %s; set MOTION_SERVER_BIN", self)
}
