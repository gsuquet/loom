// Package contexts loads ~/.config/loom/contexts.toml, the single source of
// "who am I working for here", and resolves the active context.
package contexts

import (
	"errors"
	"fmt"
	"os"
	"path/filepath"
	"regexp"
	"slices"
	"sort"
	"strings"
	"unicode"

	"github.com/BurntSushi/toml"
)

// EnvConfig overrides the config file location; EnvContext forces a context.
const (
	EnvConfig  = "LOOM_CONTEXTS"
	EnvContext = "LOOM_CONTEXT"
)

// DefaultProtectedBranches applies when [git].protected_branches is unset.
var DefaultProtectedBranches = []string{"main", "master", "trunk", "production"}

// Match selects a context by working directory or remote URL.
type Match struct {
	Paths []string `toml:"paths"` // directory prefixes; "~" is expanded
	Hosts []string `toml:"hosts"` // "host[/owner...]" prefixes of remote URLs
}

// Context is one [contexts.<name>] table.
type Context struct {
	Name        string `toml:"-"`
	Match       Match  `toml:"match"`
	KB          string `toml:"kb"`
	GitInclude  string `toml:"git_include"`
	Secrets     string `toml:"secrets"`
	KubeContext string `toml:"kube_context"` // consumed by mise per-dir setup, not loom
}

// KB is one [kb.<name>] table.
type KB struct {
	Path string `toml:"path"`
}

// Git is the [git] table.
type Git struct {
	ProtectedBranches []string `toml:"protected_branches"`
}

// Config is the parsed and validated contexts file.
type Config struct {
	Contexts map[string]*Context `toml:"contexts"`
	KB       map[string]KB       `toml:"kb"`
	Git      Git                 `toml:"git"`

	Path string `toml:"-"` // file the config was loaded from
	Home string `toml:"-"` // home directory used to expand "~"
}

// DefaultPath returns the config path: $LOOM_CONTEXTS, else
// $XDG_CONFIG_HOME/loom/contexts.toml, else ~/.config/loom/contexts.toml.
func DefaultPath(getenv func(string) string, home string) string {
	if p := getenv(EnvConfig); p != "" {
		return p
	}
	if x := getenv("XDG_CONFIG_HOME"); x != "" {
		return filepath.Join(x, "loom", "contexts.toml")
	}
	return filepath.Join(home, ".config", "loom", "contexts.toml")
}

// Load reads and validates the config at path, expanding "~" against home.
func Load(path, home string) (*Config, error) {
	if hasControl(path) {
		return nil, fmt.Errorf("contexts config path %q contains a control character", path)
	}
	data, err := os.ReadFile(path)
	if errors.Is(err, os.ErrNotExist) {
		return nil, fmt.Errorf("contexts config not found at %s (create it, or point %s / --config at one)", path, EnvConfig)
	}
	if err != nil {
		return nil, fmt.Errorf("reading contexts config: %w", err)
	}
	cfg, err := Parse(string(data), home)
	if err != nil {
		return nil, fmt.Errorf("%s: %w", path, err)
	}
	cfg.Path = path
	return cfg, nil
}

// Parse decodes and validates config text. Unknown keys are errors so typos
// fail loudly instead of silently dropping a matcher.
func Parse(text, home string) (*Config, error) {
	var cfg Config
	md, err := toml.Decode(text, &cfg)
	if err != nil {
		return nil, fmt.Errorf("invalid TOML: %w", err)
	}
	if undecoded := md.Undecoded(); len(undecoded) > 0 {
		keys := make([]string, len(undecoded))
		for i, k := range undecoded {
			keys[i] = k.String()
		}
		return nil, fmt.Errorf("unknown keys: %s", strings.Join(keys, ", "))
	}
	cfg.Home = home
	if len(cfg.Git.ProtectedBranches) == 0 {
		cfg.Git.ProtectedBranches = slices.Clone(DefaultProtectedBranches)
	}
	for name, c := range cfg.Contexts {
		if c == nil {
			return nil, fmt.Errorf("context %q is empty", name)
		}
		c.Name = name
	}
	if err := cfg.validate(); err != nil {
		return nil, err
	}
	return &cfg, nil
}

var (
	nameRe = regexp.MustCompile(`^[a-z][a-z0-9_-]*$`)
	hostRe = regexp.MustCompile(`^[a-z0-9.-]+(/[A-Za-z0-9._~-]+)*$`)
)

func (c *Config) validate() error {
	if len(c.Contexts) == 0 {
		return errors.New("no [contexts.<name>] defined")
	}
	var errs []error
	paths := map[string]string{} // normalised path -> context
	hosts := map[string]string{} // lowercased host -> context
	for _, ctx := range c.Sorted() {
		where := "contexts." + ctx.Name
		if !nameRe.MatchString(ctx.Name) {
			errs = append(errs, fmt.Errorf("%s: name must match %s", where, nameRe))
		}
		for _, p := range ctx.Match.Paths {
			if err := checkPath(p); err != nil {
				errs = append(errs, fmt.Errorf("%s: match.paths entry %q %w", where, p, err))
				continue
			}
			if strings.ContainsAny(p, `*?[\`) { // gitdir: would treat them as glob syntax
				errs = append(errs, fmt.Errorf("%s: match.paths entry %q must not contain *, ?, [ or \\", where, p))
				continue
			}
			key := filepath.Clean(c.Expand(p))
			if other, dup := paths[key]; dup {
				errs = append(errs, fmt.Errorf("%s: match.paths entry %q is already used by context %q", where, p, other))
			}
			paths[key] = ctx.Name
		}
		for _, h := range ctx.Match.Hosts {
			key := strings.ToLower(strings.TrimSuffix(h, "/"))
			if strings.Contains(h, "://") || strings.Contains(h, "@") || !hostRe.MatchString(key) {
				errs = append(errs, fmt.Errorf("%s: match.hosts entry %q must look like host[/owner], without scheme or user", where, h))
				continue
			}
			if other, dup := hosts[key]; dup {
				errs = append(errs, fmt.Errorf("%s: match.hosts entry %q is already used by context %q", where, h, other))
			}
			hosts[key] = ctx.Name
		}
		if ctx.KB != "" {
			if _, ok := c.KB[ctx.KB]; !ok {
				errs = append(errs, fmt.Errorf("%s: kb %q has no [kb.%s] table", where, ctx.KB, ctx.KB))
			}
		}
		if ctx.GitInclude != "" {
			if err := checkPath(ctx.GitInclude); err != nil {
				errs = append(errs, fmt.Errorf("%s: git_include %q %w", where, ctx.GitInclude, err))
			}
		}
	}
	for _, name := range sortedKeys(c.KB) {
		if p := c.KB[name].Path; p == "" || checkPath(p) != nil {
			errs = append(errs, fmt.Errorf("kb.%s: path %q must be set, absolute or start with ~/, without control characters", name, p))
		}
	}
	return errors.Join(errs...)
}

// Sorted returns the contexts ordered by name.
func (c *Config) Sorted() []*Context {
	out := make([]*Context, 0, len(c.Contexts))
	for _, name := range sortedKeys(c.Contexts) {
		out = append(out, c.Contexts[name])
	}
	return out
}

// Names returns the context names, sorted.
func (c *Config) Names() []string { return sortedKeys(c.Contexts) }

// Expand replaces a leading "~" with the home directory.
func (c *Config) Expand(p string) string {
	if p == "~" {
		return c.Home
	}
	if rest, ok := strings.CutPrefix(p, "~/"); ok {
		return filepath.Join(c.Home, rest)
	}
	return p
}

// checkPath rejects relative paths and control characters: paths end up in
// the generated gitconfig, where a newline would start a new section.
func checkPath(p string) error {
	switch {
	case !isAbsOrHome(p):
		return errors.New("must be absolute or start with ~/")
	case hasControl(p):
		return errors.New("must not contain control characters")
	}
	return nil
}

func hasControl(s string) bool { return strings.ContainsFunc(s, unicode.IsControl) }

func isAbsOrHome(p string) bool {
	return p == "~" || strings.HasPrefix(p, "~/") || filepath.IsAbs(p)
}

func sortedKeys[V any](m map[string]V) []string {
	keys := make([]string, 0, len(m))
	for k := range m {
		keys = append(keys, k)
	}
	sort.Strings(keys)
	return keys
}
