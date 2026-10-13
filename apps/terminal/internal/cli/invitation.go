package cli

import (
	"bufio"
	"fmt"
	"github.com/charmbracelet/x/term"
	"io"
	"os"
	"strings"
	"time"

	"github.com/Jagalite/motion/apps/terminal/internal/api"
	"github.com/Jagalite/motion/apps/terminal/internal/store"
	"github.com/spf13/cobra"
)

// Read from stdin, never an argument (shell history/process listings). The
// recipient can paste into a redirected private input or the terminal prompt.
func redeemCmd(env *Env) *cobra.Command {
	var name, accepted string
	cmd := &cobra.Command{Use: "redeem", Short: "Redeem a one-use invitation read from standard input", Args: cobra.NoArgs}
	cmd.Flags().StringVar(&name, "name", "Terminal", "device name shown to administrators")
	cmd.Flags().StringVar(&accepted, "credential-store", "", "credential storage consent (file)")
	cmd.RunE = func(cmd *cobra.Command, _ []string) error {
		connectCtx, connectCancel := env.context(cmd.Context())
		c, err := env.Connect(connectCtx)
		connectCancel()
		if err != nil {
			return err
		}
		if accepted == "" {
			accepted = c.Config.CredentialStore
		}
		if accepted != store.FileStore {
			return store.ErrNoStore
		}
		fmt.Fprintln(env.Err, "Paste the invitation code, then Enter (keep it private):")
		var code string
		if file, ok := cmd.InOrStdin().(*os.File); ok && term.IsTerminal(file.Fd()) {
			var bytes []byte
			bytes, err = term.ReadPassword(file.Fd())
			code = string(bytes)
			fmt.Fprintln(env.Err)
		} else {
			code, err = bufio.NewReader(io.LimitReader(cmd.InOrStdin(), 130)).ReadString('\n')
		}
		if err != nil && err != io.EOF {
			return err
		}
		code = strings.TrimSpace(code)
		claim, err := c.Store.BeginRedemption(c.Health.ServerID, code, accepted, store.RedemptionClaim{ClaimID: api.NewIdempotencyKey(), DeviceName: name})
		if err != nil {
			return err
		}
		ctx, cancel := env.context(cmd.Context())
		defer cancel()
		input := api.RedemptionInput{Code: code, ClaimID: claim.ClaimID, DeviceName: claim.DeviceName, ClientName: "motion-terminal"}
		var credential api.Credential
		for attempt := 0; attempt < 3; attempt++ {
			credential, err = c.RedeemInvitation(ctx, input)
			if err == nil {
				break
			}
			if ctx.Err() != nil {
				return ctx.Err()
			}
			// Retain the exact claim identity for an ambiguous transport failure.
			if _, ok := err.(*api.UnavailableError); !ok {
				return err
			}
			if sleepErr := c.Sleep(ctx, time.Second); sleepErr != nil {
				return sleepErr
			}
		}
		if err != nil {
			return err
		}
		if err = c.Store.PutCredential(store.Credential{ServerID: c.Health.ServerID, DeviceID: credential.DeviceID, AccessToken: credential.AccessToken, ExpiresAt: credential.ExpiresAt}, accepted); err != nil {
			return err
		}
		c.Config.CredentialStore = accepted
		if err = c.Remember(); err != nil {
			return err
		}
		return env.print(map[string]string{"device_id": credential.DeviceID, "server_id": c.Health.ServerID}, func(w io.Writer) { fmt.Fprintln(w, "Invitation redeemed. This device is paired.") })
	}
	return cmd
}
