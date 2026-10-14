package cli

import (
	"bufio"
	"fmt"
	"github.com/charmbracelet/x/term"
	"io"
	"os"
	"strings"

	"github.com/Jagalite/motion/apps/terminal/internal/operations"
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
		// Refuse before reading the secret when no credential store is accepted.
		if accepted == "" && c.Config.CredentialStore != store.FileStore || accepted != "" && accepted != store.FileStore {
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
		ctx, cancel := env.context(cmd.Context())
		defer cancel()
		signed, err := operations.RedeemInvitation(ctx, c, code, name, accepted)
		if err != nil {
			return err
		}
		return env.print(map[string]string{"device_id": signed.DeviceID, "server_id": signed.ServerID}, func(w io.Writer) { fmt.Fprintln(w, "Invitation redeemed. This device is paired.") })
	}
	return cmd
}
