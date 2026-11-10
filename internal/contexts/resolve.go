package contexts

import (
	"context"
	"errors"
	"fmt"
	"path/filepath"
	"strings"

	"github.com/gsuquet/loom/internal/gitx"
)

// Source records which rule selected the context.
type Source string

// Resolution sources, in priority order.
const (
	SourceFlag   Source = "flag"
	SourceEnv    Source = "env"
	SourcePath   Source = "path"
	SourceRemote Source = "remote"
)

// Resolution is the outcome of Resolve.
type Resolution struct {
	Context *Context
	Source  Source
	// Matched is the rule that matched: the flag/env value, the matched
	// directory, or "<remote> <url>".
	Matched string
	// Warning is set when git's generated includeIf routing cannot agree with
	// this resolution, so the context's git_include is not applied by git.
	Warning string
}

// Inputs are the facts Resolve works from; Remotes is injectable for tests.
type Inputs struct {
	Flag    string
	Env     string
	Dir     string
	Remotes func(ctx context.Context, dir string) ([]gitx.Remote, error)
}

// ErrNoMatch is returned when no rule selects a context. There is no default.
var ErrNoMatch = errors.New("no context matches")

// Resolve picks the context: flag, then env, then the longest match.paths
// prefix of Dir, then match.hosts against the repository remotes.
func (c *Config) Resolve(ctx context.Context, in Inputs) (*Resolution, error) {
	if in.Flag != "" {
		return c.byName(in.Flag, SourceFlag, "--context")
	}
	if in.Env != "" {
		return c.byName(in.Env, SourceEnv, EnvContext)
	}
	if res, err := c.matchPath(in.Dir); res != nil || err != nil {
		return res, err
	}
	if in.Remotes == nil {
		in.Remotes = gitx.Remotes
	}
	if res, err := c.matchRemote(ctx, in.Dir, in.Remotes); res != nil || err != nil {
		return res, err
	}
	return nil, fmt.Errorf("%w %s (no match.paths prefix, no match.hosts remote); pass --context or set %s, known: %s",
		ErrNoMatch, in.Dir, EnvContext, strings.Join(c.Names(), ", "))
}

func (c *Config) byName(name string, src Source, origin string) (*Resolution, error) {
	ctx, ok := c.Contexts[name]
	if !ok {
		return nil, fmt.Errorf("unknown context %q from %s; known: %s", name, origin, strings.Join(c.Names(), ", "))
	}
	return &Resolution{Context: ctx, Source: src, Matched: name}, nil
}

func (c *Config) matchPath(dir string) (*Resolution, error) {
	if dir == "" {
		return nil, nil
	}
	dir = realpath(dir)
	var best *Resolution
	bestLen := -1
	for _, ctx := range c.Sorted() {
		for _, p := range ctx.Match.Paths {
			prefix := realpath(c.Expand(p))
			if !within(dir, prefix) {
				continue
			}
			switch {
			case len(prefix) > bestLen:
				best, bestLen = &Resolution{Context: ctx, Source: SourcePath, Matched: prefix}, len(prefix)
			case len(prefix) == bestLen && best.Context != ctx:
				// Two entries resolving to the same directory (e.g. via symlinks).
				return nil, fmt.Errorf("ambiguous: %s matches contexts %q and %q", dir, best.Context.Name, ctx.Name)
			}
		}
	}
	return best, nil
}

func (c *Config) matchRemote(ctx context.Context, dir string, list func(context.Context, string) ([]gitx.Remote, error)) (*Resolution, error) {
	if dir == "" {
		return nil, nil
	}
	remotes, err := list(ctx, dir)
	if errors.Is(err, gitx.ErrNotRepo) {
		return nil, nil
	}
	if err != nil {
		return nil, fmt.Errorf("listing git remotes: %w", err)
	}
	var found *Resolution
	for _, r := range remotes {
		matched := c.matchURL(r.URL)
		if matched == nil {
			continue
		}
		if found == nil {
			found = &Resolution{Context: matched, Source: SourceRemote, Matched: r.Name + " " + r.URL}
		} else if found.Context != matched {
			return nil, fmt.Errorf("ambiguous: remotes match contexts %q (%s) and %q (%s %s)",
				found.Context.Name, found.Matched, matched.Name, r.Name, r.URL)
		}
	}
	if found != nil && found.Context.GitInclude != "" && !c.routedByGit(remotes, found.Context) {
		found.Warning = fmt.Sprintf("git will not apply git_include of %q here: the remote matched only after "+
			"url.<base>.insteadOf rewriting, which git's includeIf cannot see (write the full URL in the remote, "+
			"or add a match.paths entry)", found.Context.Name)
	}
	return found, nil
}

// routedByGit reports whether the generated hasconfig:remote.*.url blocks
// select ctx: git matches the URLs as written in config (no insteadOf), and
// the last matching include wins, so no raw URL may point elsewhere.
func (c *Config) routedByGit(remotes []gitx.Remote, ctx *Context) bool {
	routed := false
	for _, r := range remotes {
		switch m := c.matchURL(r.RawURL); {
		case m == ctx:
			routed = true
		case m != nil:
			return false
		}
	}
	return routed
}

// matchURL returns the context whose match.hosts entry is the longest
// segment-aligned prefix of the URL's host/path, or nil.
func (c *Config) matchURL(rawURL string) *Context {
	key := gitx.HostPath(rawURL)
	if key == "" {
		return nil
	}
	var best *Context
	bestLen := -1
	for _, ctx := range c.Sorted() {
		for _, h := range ctx.Match.Hosts {
			h = strings.ToLower(strings.TrimSuffix(h, "/"))
			if (key == h || strings.HasPrefix(key, h+"/")) && len(h) > bestLen {
				best, bestLen = ctx, len(h)
			}
		}
	}
	return best
}

// within reports whether dir is prefix or below it, on a path-segment boundary.
func within(dir, prefix string) bool {
	if dir == prefix {
		return true
	}
	if !strings.HasSuffix(prefix, string(filepath.Separator)) {
		prefix += string(filepath.Separator)
	}
	return strings.HasPrefix(dir, prefix)
}

// realpath resolves symlinks when the path exists, else just cleans it.
func realpath(p string) string {
	abs, err := filepath.Abs(p)
	if err != nil {
		return filepath.Clean(p)
	}
	if r, err := filepath.EvalSymlinks(abs); err == nil {
		return r
	}
	return abs
}
