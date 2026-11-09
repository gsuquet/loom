package contexts

import (
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
}

// Inputs are the facts Resolve works from; Remotes is injectable for tests.
type Inputs struct {
	Flag    string
	Env     string
	Dir     string
	Remotes func(dir string) ([]gitx.Remote, error)
}

// ErrNoMatch is returned when no rule selects a context. There is no default.
var ErrNoMatch = errors.New("no context matches")

// Resolve picks the context: flag, then env, then the longest match.paths
// prefix of Dir, then match.hosts against the repository remotes.
func (c *Config) Resolve(in Inputs) (*Resolution, error) {
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
	if res, err := c.matchRemote(in.Dir, in.Remotes); res != nil || err != nil {
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

func (c *Config) matchRemote(dir string, list func(string) ([]gitx.Remote, error)) (*Resolution, error) {
	if dir == "" {
		return nil, nil
	}
	remotes, err := list(dir)
	if errors.Is(err, gitx.ErrNotRepo) {
		return nil, nil
	}
	if err != nil {
		return nil, fmt.Errorf("listing git remotes: %w", err)
	}
	var found *Resolution
	for _, r := range remotes {
		ctx := c.matchURL(r.URL)
		if ctx == nil {
			continue
		}
		if found == nil {
			found = &Resolution{Context: ctx, Source: SourceRemote, Matched: r.Name + " " + r.URL}
		} else if found.Context != ctx {
			return nil, fmt.Errorf("ambiguous: remotes match contexts %q (%s) and %q (%s %s)",
				found.Context.Name, found.Matched, ctx.Name, r.Name, r.URL)
		}
	}
	return found, nil
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
