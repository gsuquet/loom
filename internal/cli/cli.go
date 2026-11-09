// Package cli dispatches `loom <command> [args]` to command handlers using
// the standard library flag package. One file per command group.
package cli

import (
	"errors"
	"flag"
	"fmt"
	"io"
	"os"
	"strings"
	"text/tabwriter"
)

// Exit codes.
const (
	ExitOK    = 0
	ExitError = 1
	ExitUsage = 2
)

// Env is everything a command may touch in the outside world, so tests can
// run commands in-process.
type Env struct {
	Stdout, Stderr io.Writer
	Getenv         func(string) string
	Getwd          func() (string, error)
	HomeDir        func() (string, error)
	Version        string
}

// OSEnv returns an Env bound to the real process.
func OSEnv(version string) Env {
	return Env{
		Stdout: os.Stdout, Stderr: os.Stderr,
		Getenv: os.Getenv, Getwd: os.Getwd, HomeDir: os.UserHomeDir,
		Version: version,
	}
}

type command struct {
	name    string
	summary string
	run     func(env Env, args []string) error
}

// errUsage marks errors whose message has already been printed with usage.
var errUsage = errors.New("usage error")

func commands() []command {
	return []command{
		{"context", "Print the resolved context, or generate git includeIf blocks", runContext},
		{"version", "Print the loom version", runVersion},
		{"help", "Show help for loom or a command", nil}, // handled in Run
	}
}

// Run executes loom with args (without the program name) and returns the exit code.
func Run(args []string, env Env) int {
	if len(args) == 0 {
		usage(env.Stderr)
		return ExitUsage
	}
	name, rest := args[0], args[1:]
	switch name {
	case "-h", "--help", "-help":
		usage(env.Stdout)
		return ExitOK
	case "--version", "-version":
		name = "version"
	case "help":
		if len(rest) == 0 {
			usage(env.Stdout)
			return ExitOK
		}
		name, rest = rest[0], []string{"-h"}
	}
	for _, c := range commands() {
		switch {
		case c.name != name:
		case c.run == nil: // `loom help help`
			usage(env.Stdout)
			return ExitOK
		default:
			return exitCode(env, c.run(env, rest))
		}
	}
	fmt.Fprintf(env.Stderr, "loom: unknown command %q\n\n", name)
	usage(env.Stderr)
	return ExitUsage
}

func exitCode(env Env, err error) int {
	switch {
	case err == nil, errors.Is(err, flag.ErrHelp):
		return ExitOK
	case errors.Is(err, errUsage):
		return ExitUsage
	default:
		fmt.Fprintf(env.Stderr, "loom: %v\n", err)
		return ExitError
	}
}

func usage(w io.Writer) {
	fmt.Fprint(w, "loom - knowledge base and git workflow CLI\n\nUsage:\n  loom <command> [flags] [args]\n\nCommands:\n")
	tw := tabwriter.NewWriter(w, 0, 0, 2, ' ', 0)
	for _, c := range commands() {
		fmt.Fprintf(tw, "  %s\t%s\n", c.name, c.summary)
	}
	_ = tw.Flush()
	fmt.Fprint(w, "\nRun 'loom help <command>' for command flags.\n")
}

// newFlagSet builds a silent FlagSet; parse prints usage to the right stream.
func newFlagSet(name, synopsis string) *flag.FlagSet {
	fs := flag.NewFlagSet("loom "+name, flag.ContinueOnError)
	fs.SetOutput(io.Discard)
	fs.Usage = func() {
		w := fs.Output()
		fmt.Fprintf(w, "Usage:\n  loom %s\n\nFlags:\n", synopsis)
		fs.PrintDefaults()
	}
	return fs
}

// parse parses args and prints usage to the right stream on -h or error.
func parse(env Env, fs *flag.FlagSet, args []string) error {
	err := fs.Parse(args)
	switch {
	case err == nil:
		return nil
	case errors.Is(err, flag.ErrHelp):
		fs.SetOutput(env.Stdout)
		fs.Usage()
		return flag.ErrHelp
	default:
		fs.SetOutput(env.Stderr)
		fmt.Fprintf(env.Stderr, "loom: %v\n", err)
		fs.Usage()
		return errUsage
	}
}

// usageErr prints msg plus usage and returns errUsage.
func usageErr(env Env, fs *flag.FlagSet, format string, a ...any) error {
	fmt.Fprintf(env.Stderr, "loom: "+format+"\n", a...)
	fs.SetOutput(env.Stderr)
	fs.Usage()
	return errUsage
}

func runVersion(env Env, args []string) error {
	fs := newFlagSet("version", "version")
	if err := parse(env, fs, args); err != nil {
		return err
	}
	if fs.NArg() > 0 {
		return usageErr(env, fs, "unexpected arguments: %s", strings.Join(fs.Args(), " "))
	}
	_, err := fmt.Fprintf(env.Stdout, "loom %s\n", env.Version)
	return err
}
