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

func devicesCmd(env *Env) *cobra.Command {
	cmd := &cobra.Command{Use: "devices", Short: "Manage paired devices (administrator)"}
	var cursor string
	var limit int
	list := &cobra.Command{
		Use: "list", Short: "List devices", Args: cobra.NoArgs,
		RunE: run(env, func(ctx context.Context, c *Conn) error {
			page, err := operations.ListDevices(ctx, c, operations.PageRequest{Cursor: cursor, Limit: limit})
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
				more(w, page.NextCursor)
			})
		}),
	}
	pageFlags(list, &cursor, &limit)
	show := &cobra.Command{
		Use: "show DEVICE_ID", Short: "Show a device", Args: cobra.ExactArgs(1),
		RunE: func(cmd *cobra.Command, args []string) error {
			return run(env, func(ctx context.Context, c *Conn) error {
				d, err := operations.InspectDevice(ctx, c, args[0])
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
				if err := operations.RevokeDevice(ctx, c, args[0], operations.ETagOrCurrent(ifMatch)); err != nil {
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
				p, err := operations.InspectDevicePolicy(ctx, c, args[0])
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
			if err := operations.CheckPermissions(in.Permissions); err != nil {
				return err
			}
			return run(env, func(ctx context.Context, c *Conn) error {
				p, err := operations.ReplaceDevicePolicy(ctx, c, args[0], in, operations.ETagOrCurrent(ifMatch))
				if err != nil {
					return err
				}
				return env.print(p.Value, func(w io.Writer) { printPolicy(w, p.Value) })
			})(cmd, args)
		},
	}
	set.Flags().StringSliceVar(&in.LibraryIDs, "library", nil, "library the device may see (repeatable)")
	set.Flags().BoolVar(&in.AllowUnrated, "allow-unrated", true, "allow unrated titles")
	set.Flags().BoolVar(&in.DenyRated, "deny-rated", false, "deny all rated titles")
	set.Flags().StringSliceVar(&in.AllowedRatings, "rating", nil, "allowed rating (repeatable; none means no rating filter)")
	set.Flags().StringSliceVar(&in.BlockedLabels, "block-label", nil, "blocked label (repeatable)")
	set.Flags().StringSliceVar(&in.Permissions, "permission", nil, "permission (repeatable; replaces the current set)")
	set.Flags().StringVar(&ifMatch, "if-match", "", "ETag the policy must still have (default: read it first)")
	cmd.AddCommand(show, set)
	return cmd
}

// printPolicy writes through env.print, whose writer strips control characters.
func printPolicy(w io.Writer, p api.AccessPolicy) {
	fmt.Fprintf(w, "Revision %s\nLibraries %s\nAllow unrated %v\nDeny rated %v\nRatings %s\nBlocked labels %s\nPermissions %s\n",
		p.Revision, strings.Join(p.LibraryIDs, ", "), p.AllowUnrated, p.DenyRated, strings.Join(p.AllowedRatings, ", "),
		strings.Join(p.BlockedLabels, ", "), strings.Join(p.Permissions, ", "))
}

// accessCmd administers users, pairing requests and invitations, as the
// TUI's access pages do.
func accessCmd(env *Env) *cobra.Command {
	cmd := &cobra.Command{Use: "access", Short: "Manage access users, pairing requests and invitations (administrator)"}
	cmd.AddCommand(pairingsCmd(env), usersCmd(env), invitationsCmd(env))
	return cmd
}

func pairingsCmd(env *Env) *cobra.Command {
	cmd := &cobra.Command{Use: "pairings", Short: "Pairing requests"}
	var cursor string
	var limit int
	list := &cobra.Command{Use: "list", Args: cobra.NoArgs, RunE: run(env, func(ctx context.Context, c *Conn) error {
		page, err := operations.ListPairings(ctx, c, operations.PageRequest{Cursor: cursor, Limit: limit})
		if err != nil {
			return err
		}
		return env.print(page, func(w io.Writer) {
			rows := [][]string{}
			for _, p := range page.Items {
				rows = append(rows, []string{p.ID, p.DeviceName, p.ClientName, p.Status, p.UserCode, p.ExpiresAt})
			}
			table(w, "ID\tDEVICE\tCLIENT\tSTATUS\tCODE\tEXPIRES", rows)
			more(w, page.NextCursor)
		})
	})}
	pageFlags(list, &cursor, &limit)
	var key string
	deny := &cobra.Command{Use: "deny PAIRING_ID", Short: "Deny a pairing request", Args: cobra.ExactArgs(1), RunE: func(cmd *cobra.Command, args []string) error {
		intent, err := operations.ResumeIntent("denial of pairing "+args[0], key)
		if err != nil {
			return err
		}
		return run(env, func(ctx context.Context, c *Conn) error {
			if err := operations.DenyPairing(ctx, c, args[0], intent); err != nil {
				return err
			}
			return env.print(map[string]string{"pairing_id": args[0], "status": "denied"}, func(w io.Writer) {
				fmt.Fprintf(w, "Denied pairing %s.\n", sanitize(args[0]))
			})
		})(cmd, args)
	}}
	keyFlag(deny, &key)
	cmd.AddCommand(list, deny)
	return cmd
}

// grantFlags binds the flags of a user grant or invitation.
func grantFlags(cmd *cobra.Command, profiles, permissions *[]string, policy *api.GrantPolicy) {
	cmd.Flags().StringSliceVar(profiles, "grant-profile", nil, "profile ID (repeatable)")
	cmd.Flags().StringSliceVar(permissions, "permission", nil, "permission (repeatable)")
	cmd.Flags().StringSliceVar(&policy.LibraryIDs, "library", nil, "library ID (repeatable; none means no library)")
	cmd.Flags().BoolVar(&policy.AllowUnrated, "allow-unrated", true, "allow unrated content")
	cmd.Flags().BoolVar(&policy.DenyRated, "deny-rated", false, "deny all rated content")
	cmd.Flags().StringSliceVar(&policy.AllowedRatings, "rating", nil, "allowed rating (repeatable; none means any unless denied)")
	cmd.Flags().StringSliceVar(&policy.BlockedLabels, "block-label", nil, "blocked label (repeatable)")
}

// inheritGrant fills grant flags the command line left unset from the
// user's grant, as the TUI pre-fills its form.
func inheritGrant(cmd *cobra.Command, u api.User, profiles, permissions *[]string, policy *api.GrantPolicy) {
	g, f := operations.InheritedGrant(u), cmd.Flags()
	if !f.Changed("grant-profile") {
		*profiles = g.ProfileIDs
	}
	if !f.Changed("permission") {
		*permissions = g.Permissions
	}
	if !f.Changed("library") {
		policy.LibraryIDs = g.Policy.LibraryIDs
	}
	if !f.Changed("allow-unrated") {
		policy.AllowUnrated = g.Policy.AllowUnrated
	}
	if !f.Changed("deny-rated") {
		policy.DenyRated = g.Policy.DenyRated
	}
	if !f.Changed("rating") {
		policy.AllowedRatings = g.Policy.AllowedRatings
	}
	if !f.Changed("block-label") {
		policy.BlockedLabels = g.Policy.BlockedLabels
	}
}

func printUser(w io.Writer, u api.User) {
	state := "enabled"
	if u.Disabled {
		state = "disabled"
	}
	fmt.Fprintf(w, "User %s (%s) · %s · revision %s\nProfiles %s\nLibraries %s\nPermissions %s\nAllow unrated %v  Deny rated %v\nRatings %s\nBlocked labels %s\n",
		u.Name, u.ID, state, u.Revision, strings.Join(u.ProfileIDs, ", "), strings.Join(u.Policy.LibraryIDs, ", "),
		strings.Join(u.Permissions, ", "), u.Policy.AllowUnrated, u.Policy.DenyRated,
		strings.Join(u.Policy.AllowedRatings, ", "), strings.Join(u.Policy.BlockedLabels, ", "))
}

func usersCmd(env *Env) *cobra.Command {
	cmd := &cobra.Command{Use: "users", Short: "Access users"}
	var cursor string
	var limit int
	list := &cobra.Command{Use: "list", Args: cobra.NoArgs, RunE: run(env, func(ctx context.Context, c *Conn) error {
		page, err := operations.ListUsers(ctx, c, operations.PageRequest{Cursor: cursor, Limit: limit})
		if err != nil {
			return err
		}
		return env.print(page, func(w io.Writer) {
			rows := [][]string{}
			for _, u := range page.Items {
				state := "enabled"
				if u.Disabled {
					state = "disabled"
				}
				rows = append(rows, []string{u.ID, u.Name, state, strings.Join(u.Permissions, ",")})
			}
			table(w, "ID\tNAME\tSTATE\tPERMISSIONS", rows)
			more(w, page.NextCursor)
		})
	})}
	pageFlags(list, &cursor, &limit)
	show := &cobra.Command{Use: "show USER_ID", Args: cobra.ExactArgs(1), RunE: func(cmd *cobra.Command, args []string) error {
		return run(env, func(ctx context.Context, c *Conn) error {
			u, err := operations.InspectUser(ctx, c, args[0])
			if err != nil {
				return err
			}
			return env.print(u.Value, func(w io.Writer) { printUser(w, u.Value) })
		})(cmd, args)
	}}
	var in api.UserInput
	var key string
	var disabled bool
	create := &cobra.Command{Use: "create NAME", Args: cobra.ExactArgs(1), RunE: func(cmd *cobra.Command, args []string) error {
		in.Name, in.Disabled = args[0], disabled
		if !cmd.Flags().Changed("permission") {
			in.Permissions = operations.DefaultUserPermissions
		}
		intent, err := operations.ResumeIntent("creation of user "+args[0], key)
		if err != nil {
			return err
		}
		return run(env, func(ctx context.Context, c *Conn) error {
			u, err := operations.CreateUser(ctx, c, in, intent)
			if err != nil {
				return err
			}
			return env.print(u.Value, func(w io.Writer) { printUser(w, u.Value) })
		})(cmd, args)
	}}
	grantFlags(create, &in.ProfileIDs, &in.Permissions, &in.Policy)
	create.Flags().BoolVar(&disabled, "disabled", false, "create the user disabled")
	keyFlag(create, &key)
	var up api.UserInput
	var name, ifMatch string
	var upDisabled bool
	update := &cobra.Command{Use: "update USER_ID", Short: "Replace a user's name, grants and enabled state", Args: cobra.ExactArgs(1), RunE: func(cmd *cobra.Command, args []string) error {
		up.Name, up.Disabled = name, upDisabled
		return run(env, func(ctx context.Context, c *Conn) error {
			u, err := operations.ReplaceUser(ctx, c, args[0], up, operations.ETagOrCurrent(ifMatch))
			if err != nil {
				return err
			}
			return env.print(u.Value, func(w io.Writer) { printUser(w, u.Value) })
		})(cmd, args)
	}}
	grantFlags(update, &up.ProfileIDs, &up.Permissions, &up.Policy)
	update.Flags().StringVar(&name, "name", "", "user name")
	update.Flags().BoolVar(&upDisabled, "disabled", false, "disable the user across all owned devices")
	update.Flags().StringVar(&ifMatch, "if-match", "", "ETag the user must still have (default: read it first)")
	_ = update.MarkFlagRequired("name")
	cmd.AddCommand(list, show, create, update)
	return cmd
}

func invitationsCmd(env *Env) *cobra.Command {
	cmd := &cobra.Command{Use: "invitations", Short: "One-use invitations"}
	var cursor string
	var limit int
	list := &cobra.Command{Use: "list", Args: cobra.NoArgs, RunE: run(env, func(ctx context.Context, c *Conn) error {
		page, err := operations.ListInvitations(ctx, c, operations.PageRequest{Cursor: cursor, Limit: limit})
		if err != nil {
			return err
		}
		return env.print(page, func(w io.Writer) {
			rows := [][]string{}
			for _, i := range page.Items {
				rows = append(rows, []string{i.ID, i.UserID, i.Status, i.CreatedAt, i.ExpiresAt})
			}
			table(w, "ID\tUSER\tSTATUS\tCREATED\tEXPIRES", rows)
			more(w, page.NextCursor)
		})
	})}
	pageFlags(list, &cursor, &limit)
	var in api.InvitationInput
	var key, userRevision string
	create := &cobra.Command{Use: "create USER_ID", Short: "Issue an invitation; the code is printed once", Args: cobra.ExactArgs(1), RunE: func(cmd *cobra.Command, args []string) error {
		intent, err := operations.ResumeIntent("invitation for user "+args[0], key)
		if err != nil {
			return err
		}
		if key != "" && userRevision == "" {
			return usage("--idempotency-key needs the original request's --user-revision")
		}
		return run(env, func(ctx context.Context, c *Conn) error {
			// The invitation grants what the administrator decided on: the
			// given user revision, or the current one, read now.
			user, err := operations.InspectUser(ctx, c, args[0])
			if err != nil {
				return err
			}
			in.UserID, in.UserRevision = user.Value.ID, user.Value.Revision
			if userRevision != "" {
				in.UserRevision = userRevision
			}
			inheritGrant(cmd, user.Value, &in.ProfileIDs, &in.Permissions, &in.Policy)
			v, err := operations.CreateInvitation(ctx, c, in, intent)
			if err != nil {
				return err
			}
			return env.print(v.Value, func(w io.Writer) {
				fmt.Fprintf(w, "Invitation %s for %s expires %s.\nCode (share privately; it is shown once):\n%s\n", v.Value.ID, sanitize(user.Value.Name), v.Value.ExpiresAt, v.Value.Code)
			})
		})(cmd, args)
	}}
	grantFlags(create, &in.ProfileIDs, &in.Permissions, &in.Policy)
	create.Flags().Int64Var(&in.TTLSeconds, "ttl", 600, "lifetime in seconds (60-86400)")
	create.Flags().StringVar(&userRevision, "user-revision", "", "user revision the invitation is decided on (default: read it now)")
	keyFlag(create, &key)
	var ifMatch string
	revoke := &cobra.Command{Use: "revoke INVITATION_ID", Short: "Revoke an unused invitation", Args: cobra.ExactArgs(1), RunE: func(cmd *cobra.Command, args []string) error {
		return run(env, func(ctx context.Context, c *Conn) error {
			if err := operations.RevokeInvitation(ctx, c, args[0], operations.ETagOrCurrent(ifMatch)); err != nil {
				return err
			}
			return env.print(map[string]string{"invitation_id": args[0], "status": "revoked"}, func(w io.Writer) {
				fmt.Fprintf(w, "Revoked invitation %s.\n", sanitize(args[0]))
			})
		})(cmd, args)
	}}
	revoke.Flags().StringVar(&ifMatch, "if-match", "", "ETag the invitation must still have (default: read it first)")
	cmd.AddCommand(list, create, revoke)
	return cmd
}
