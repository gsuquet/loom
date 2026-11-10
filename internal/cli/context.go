package cli

import (
	"context"
	"encoding/json"
	"errors"
	"flag"
	"fmt"
	"strings"

	"github.com/gsuquet/loom/internal/contexts"
	"github.com/gsuquet/loom/internal/gitx"
)

// configFlags are shared by every command that reads contexts.toml.
type configFlags struct {
	config  string
	context string
}

func (f *configFlags) register(fs *flag.FlagSet, withContext bool) {
	fs.StringVar(&f.config, "config", "", "contexts file (default $"+contexts.EnvConfig+" or ~/.config/loom/contexts.toml)")
	if withContext {
		fs.StringVar(&f.context, "context", "", "force the context (overrides $"+contexts.EnvContext+")")
		fs.StringVar(&f.context, "c", "", "shorthand for --context")
	}
}

func (f *configFlags) load(env Env) (*contexts.Config, error) {
	home, err := env.HomeDir()
	if err != nil {
		return nil, fmt.Errorf("home directory: %w", err)
	}
	path := f.config
	if path == "" {
		path = contexts.DefaultPath(env.Getenv, home)
	}
	return contexts.Load(path, home)
}

func (f *configFlags) resolve(ctx context.Context, env Env) (*contexts.Config, *contexts.Resolution, error) {
	cfg, err := f.load(env)
	if err != nil {
		return nil, nil, err
	}
	wd, err := env.Getwd()
	if err != nil {
		return nil, nil, fmt.Errorf("working directory: %w", err)
	}
	res, err := cfg.Resolve(ctx, contexts.Inputs{
		Flag: f.context, Env: env.Getenv(contexts.EnvContext), Dir: wd, Remotes: gitRemotes,
	})
	return cfg, res, err
}

// gitRemotes adapts gitx.Remotes for contexts.Resolve, which runs no git
// itself (ADR 0016): outside a repository there are no remotes.
func gitRemotes(ctx context.Context, dir string) ([]contexts.Remote, error) {
	list, err := gitx.Remotes(ctx, dir)
	if errors.Is(err, gitx.ErrNotRepo) {
		return nil, nil
	}
	if err != nil {
		return nil, err
	}
	out := make([]contexts.Remote, len(list))
	for i, r := range list {
		out[i] = contexts.Remote(r)
	}
	return out, nil
}

func runContext(ctx context.Context, env Env, args []string) error {
	if len(args) > 0 && args[0] == "gitconfig" {
		return runContextGitconfig(ctx, env, args[1:])
	}
	var cf configFlags
	var asJSON bool
	fs := newFlagSet("context", "context [--json] [-c name] [--config file]\n  loom context gitconfig [--config file]")
	cf.register(fs, true)
	fs.BoolVar(&asJSON, "json", false, "print JSON")
	if err := parse(env, fs, args); err != nil {
		return err
	}
	if fs.NArg() > 0 {
		return usageErr(env, fs, "unexpected arguments: %s", strings.Join(fs.Args(), " "))
	}
	cfg, res, err := cf.resolve(ctx, env)
	if err != nil {
		return err
	}
	out := contextOutput(cfg, res)
	if out.Warning != "" {
		fmt.Fprintf(env.Stderr, "loom: warning: %s\n", out.Warning)
	}
	if asJSON {
		enc := json.NewEncoder(env.Stdout)
		enc.SetIndent("", "  ")
		enc.SetEscapeHTML(false)
		return enc.Encode(out)
	}
	var b strings.Builder
	row := func(k, v string) {
		if v != "" {
			fmt.Fprintf(&b, "%-14s%s\n", k+":", v) // fixed width: stable for scripts and tests
		}
	}
	row("context", out.Name)
	row("source", fmt.Sprintf("%s (%s)", out.Source, out.Matched))
	if out.KB != nil {
		row("kb", fmt.Sprintf("%s (%s)", out.KB.Name, out.KB.Path))
	}
	row("git_include", out.GitInclude)
	row("secrets", out.Secrets)
	row("kube_context", out.KubeContext)
	row("protected", strings.Join(out.ProtectedBranches, ", "))
	row("config", out.Config)
	_, err = fmt.Fprint(env.Stdout, b.String())
	return err
}

type kbOutput struct {
	Name string `json:"name"`
	Path string `json:"path"`
}

type contextJSON struct {
	Name              string    `json:"name"`
	Source            string    `json:"source"`
	Matched           string    `json:"matched"`
	KB                *kbOutput `json:"kb,omitempty"`
	GitInclude        string    `json:"git_include,omitempty"`
	Secrets           string    `json:"secrets,omitempty"`
	KubeContext       string    `json:"kube_context,omitempty"`
	ProtectedBranches []string  `json:"protected_branches"`
	Config            string    `json:"config"`
	Warning           string    `json:"warning,omitempty"`
}

// contextOutput expands "~" in paths so callers get usable absolute paths.
func contextOutput(cfg *contexts.Config, res *contexts.Resolution) contextJSON {
	c := res.Context
	out := contextJSON{
		Name: c.Name, Source: string(res.Source), Matched: res.Matched,
		Secrets: c.Secrets, KubeContext: c.KubeContext,
		ProtectedBranches: cfg.Git.ProtectedBranches, Config: cfg.Path, Warning: res.Warning,
	}
	if c.GitInclude != "" {
		out.GitInclude = cfg.Expand(c.GitInclude)
	}
	if c.KB != "" {
		out.KB = &kbOutput{Name: c.KB, Path: cfg.Expand(cfg.KB[c.KB].Path)}
	}
	return out
}

func runContextGitconfig(_ context.Context, env Env, args []string) error {
	var cf configFlags
	fs := newFlagSet("context gitconfig", "context gitconfig [--config file]")
	cf.register(fs, false)
	if err := parse(env, fs, args); err != nil {
		return err
	}
	if fs.NArg() > 0 {
		return usageErr(env, fs, "unexpected arguments: %s", strings.Join(fs.Args(), " "))
	}
	cfg, err := cf.load(env)
	if err != nil {
		return err
	}
	return cfg.WriteGitConfig(env.Stdout)
}
