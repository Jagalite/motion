// Command motion is the Motion terminal client: CLI and TUI over the public API.
package main

import (
	"context"
	"os"
	"os/signal"
	"syscall"

	"github.com/Jagalite/motion/apps/terminal/internal/cli"
	"github.com/Jagalite/motion/apps/terminal/internal/tui"
)

func main() {
	ctx, stop := signal.NotifyContext(context.Background(), os.Interrupt, syscall.SIGTERM)
	defer stop()
	cli.TUI = tui.Run
	env := &cli.Env{Out: os.Stdout, Err: os.Stderr}
	err := cli.Execute(ctx, env, os.Args[1:])
	if err != nil {
		cli.ReportError(os.Stderr, env.JSON, err)
	}
	os.Exit(cli.ExitCode(err))
}
