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
	"github.com/Jagalite/motion/apps/terminal/internal/operations"
	"github.com/spf13/cobra"
)

// TUI is set by main, which composes the interfaces; cli does not import tui.
var TUI func(ctx context.Context, session *operations.Session, browser operations.Browser) error

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
	root.AddCommand(serverCmd(env), authCmd(env), accessCmd(env), devicesCmd(env), profilesCmd(env),
		librariesCmd(env), sourcesCmd(env), scansCmd(env), catalogCmd(env), jobsCmd(env), diagnosticsCmd(env),
		playbackCmd(env), playCmd(env), eventsCmd(env), tuiCmd(env), serveCmd())
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

// keyFlag adds --idempotency-key, which resubmits a keyed write whose
// outcome was unknown.
func keyFlag(cmd *cobra.Command, key *string) {
	cmd.Flags().StringVar(key, "idempotency-key", "", "resubmit an earlier request whose outcome is unknown")
}

// pageFlags adds --cursor and --limit.
func pageFlags(cmd *cobra.Command, cursor *string, limit *int) {
	cmd.Flags().StringVar(cursor, "cursor", "", "continue from a previous page's next_cursor")
	cmd.Flags().IntVar(limit, "limit", 50, "page size (1-200)")
}

// more prints the cursor of the next page.
func more(w io.Writer, next *string) {
	if next != nil {
		fmt.Fprintf(w, "More: --cursor %s\n", sanitize(*next))
	}
}

func yes(b bool) string {
	if b {
		return "yes"
	}
	return "no"
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
			status, err := operations.ReadServerStatus(ctx, c, c.URL, c.Health, c.SignedIn())
			if err != nil {
				return err
			}
			return env.print(status, func(w io.Writer) {
				fmt.Fprintf(w, "Server   %s\nStatus   %s\nID       %s\nEpoch    %s\n", c.URL, c.Health.Status, c.Health.ServerID, c.Health.ServerEpoch)
				caps := status.Capabilities
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
			if name == "" {
				name, _ = os.Hostname()
			}
			pending, err := operations.StartPairing(ctx, conn, name, credentialStore)
			if err != nil {
				return err
			}
			env.notice(pending, fmt.Sprintf("Pairing requested. In an administrator’s Motion TUI, open Pairing requests and compare code %s.\nWaiting for approval (expires %s)...", pending.UserCode, pending.ExpiresAt))
			signed, err := pending.Wait(ctx)
			if err != nil {
				return err
			}
			return env.print(signed, func(w io.Writer) {
				fmt.Fprintf(w, "Paired as device %s with server %s (credential expires %s).\n", signed.DeviceID, signed.ServerID, signed.ExpiresAt)
			})
		},
	}
	pair.Flags().StringVar(&name, "name", "", "device name shown to the administrator (default: hostname)")
	pair.Flags().StringVar(&credentialStore, "credential-store", "", "where to keep the credential; only \"file\" (0600) is supported")
	pair.Flags().DurationVar(&wait, "wait", 10*time.Minute, "how long to wait for approval")

	var code, approveKey, approveUser, approveRevision, approveResubmit string
	var profiles, permissions []string
	var policy api.GrantPolicy
	approve := &cobra.Command{
		Use:   "approve PAIRING_ID",
		Short: "Approve a pairing with explicit profiles and permissions (administrator)",
		Args:  cobra.ExactArgs(1),
		RunE: func(cmd *cobra.Command, args []string) error {
			if code == "" {
				return usage("--code is required: use the code shown on the pairing device")
			}
			if err := operations.CheckPermissions(permissions); err != nil {
				return err
			}
			// One key for every retry of this approval.
			intent, err := operations.ResumeIntent("approval of pairing "+args[0], approveKey)
			if err != nil {
				return err
			}
			var original *operations.GrantRequest
			if approveUser != "" {
				if original, err = resubmission(approveKey, approveResubmit); err != nil {
					return err
				}
			}
			return run(env, func(ctx context.Context, c *Conn) error {
				in := operations.Approval{PairingID: args[0], UserCode: code, ProfileIDs: profiles, Permissions: permissions}
				if approveUser != "" {
					// The device joins the user at the revision read now, with
					// the grant given here.
					user, err := operations.InspectUser(ctx, c, approveUser)
					if err != nil {
						return err
					}
					in.UserID, in.UserRevision, in.Policy = user.Value.ID, user.Value.Revision, &policy
					if approveRevision != "" {
						in.UserRevision = approveRevision
					}
					inheritGrant(cmd, user.Value, &in.ProfileIDs, &in.Permissions, &policy)
					if original != nil {
						in.UserRevision, in.ProfileIDs, in.Permissions, policy = original.UserRevision, original.ProfileIDs, original.Permissions, original.Policy
					}
				}
				d, err := operations.ApprovePairing(ctx, c, in, intent)
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
	grantFlags(approve, &profiles, &permissions, &policy)
	approve.Flags().StringVar(&approveUser, "user", "", "access user the device belongs to; the library and rating flags then set its grant")
	approve.Flags().StringVar(&approveRevision, "user-revision", "", "user revision the approval is decided on (default: read it now)")
	approve.Flags().StringVar(&approveResubmit, "resubmit-request", "", "with --idempotency-key and --user: the reported request, which replaces the grant flags")
	keyFlag(approve, &approveKey)

	status := &cobra.Command{
		Use:   "status",
		Short: "Show the authenticated principal",
		Args:  cobra.NoArgs,
		RunE: run(env, func(ctx context.Context, c *Conn) error {
			me, err := operations.WhoAmI(ctx, c, env.Profile)
			if err != nil {
				return err
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
			serverID, err := operations.SignOut(c)
			if err != nil {
				return err
			}
			return env.print(map[string]string{"server_id": serverID}, func(w io.Writer) {
				fmt.Fprintln(w, "Removed the local credential. Revoke the device with `motion devices revoke` to invalidate it.")
			})
		}),
	}
	cmd.AddCommand(pair, approve, status, logout, redeemCmd(env))
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
			return operations.FollowEvents(cmd.Context(), conn, after, func(e api.Event) error {
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
			return TUI(cmd.Context(), conn, env.Browser)
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
