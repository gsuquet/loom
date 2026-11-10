package contexts

import (
	"context"
	"errors"
	"fmt"
	"path/filepath"
	"strings"
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

// Remote is a git remote of the repository being resolved. URL has insteadOf
// rewrites applied; RawURL is the value as written in git config, which is
// what git's includeIf hasconfig:remote.*.url sees. Both are credential-free.
type Remote struct {
	Name   string
	URL    string
	RawURL string
}

// Inputs are the facts Resolve works from. Remotes lists the remotes of the
// repository containing dir, and nil remotes outside a repository; the caller
// supplies it (cli wires git), so this package runs no subprocess. A nil
// Remotes disables remote matching.
type Inputs struct {
	Flag    string
	Env     string
	Dir     string
	Remotes func(ctx context.Context, dir string) ([]Remote, error)
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

func (c *Config) matchRemote(ctx context.Context, dir string, list func(context.Context, string) ([]Remote, error)) (*Resolution, error) {
	if dir == "" || list == nil {
		return nil, nil
	}
	remotes, err := list(ctx, dir)
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
func (c *Config) routedByGit(remotes []Remote, ctx *Context) bool {
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
	key := hostPath(rawURL)
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

// hostPath normalises a remote URL to "host/path" without scheme, user, port
// or ".git" suffix, lowercased, so it can be prefix-matched against
// "host[/owner]" patterns. It returns "" for local paths and unparsable URLs.
func hostPath(rawURL string) string {
	u := strings.TrimSpace(rawURL)
	switch {
	case strings.Contains(u, "://"):
		u = u[strings.Index(u, "://")+3:]
		if at := strings.LastIndex(strings.SplitN(u, "/", 2)[0], "@"); at >= 0 {
			u = u[at+1:]
		}
		host, rest, _ := strings.Cut(u, "/")
		if h, _, ok := strings.Cut(host, ":"); ok {
			host = h // drop port
		}
		u = host + "/" + rest
	case strings.Contains(u, ":") && !strings.HasPrefix(u, "/"):
		// scp-like syntax: [user@]host:path
		host, rest, _ := strings.Cut(u, ":")
		if at := strings.LastIndex(host, "@"); at >= 0 {
			host = host[at+1:]
		}
		u = host + "/" + strings.TrimPrefix(rest, "/")
	default:
		return ""
	}
	u = strings.TrimSuffix(strings.TrimSuffix(u, "/"), ".git")
	if strings.HasPrefix(u, "/") || u == "" {
		return ""
	}
	return strings.ToLower(u)
}
