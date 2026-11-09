// Command loom is the entry point; all behaviour lives in internal/cli.
package main

import (
	"os"

	"github.com/gsuquet/loom/internal/cli"
)

// version is set at build time with -ldflags "-X main.version=...".
var version = "dev"

func main() {
	os.Exit(cli.Run(os.Args[1:], cli.OSEnv(version)))
}
