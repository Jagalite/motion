// Package platform holds effects on the local machine that interfaces inject
// into shared operations.
package platform

import (
	"context"
	"errors"
	"os/exec"
	"runtime"
)

// ErrBrowserUnsupported means this platform has no known browser opener.
var ErrBrowserUnsupported = errors.New("browser opening is unsupported on this platform")

// OpenBrowser opens target with the platform's fixed browser opener.
func OpenBrowser(ctx context.Context, target string) error {
	var command *exec.Cmd
	switch runtime.GOOS {
	case "darwin":
		command = exec.CommandContext(ctx, "open", target)
	case "linux":
		command = exec.CommandContext(ctx, "xdg-open", target)
	case "windows":
		command = exec.CommandContext(ctx, "rundll32", "url.dll,FileProtocolHandler", target)
	default:
		return ErrBrowserUnsupported
	}
	return command.Run()
}
