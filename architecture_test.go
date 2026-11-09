// Package loom_test holds repository-wide rules that are not tied to one package.
package loom_test

import (
	"go/parser"
	"go/token"
	"io/fs"
	"path/filepath"
	"slices"
	"strconv"
	"strings"
	"testing"
)

const module = "github.com/gsuquet/loom"

// shared packages may be imported by any command package; everything else
// under internal/ except cli is a command package (gitcmd, ctxstore, views, hooks, knowledge).
// TODO(slice 1b): allow the edges views→ctxstore and hooks→ctxstore/views (docs/design.md §8).
var shared = []string{"contexts", "forge", "gitx"}

// allowedDeps are the only third-party modules; adding one needs an ADR (ADR 0004).
var allowedDeps = []string{"github.com/BurntSushi/toml", "gopkg.in/yaml.v3"}

// imports maps each package dir (slash-separated, relative) to its imports,
// test files included.
func imports(t *testing.T) map[string][]string {
	t.Helper()
	out := map[string][]string{}
	err := filepath.WalkDir(".", func(path string, d fs.DirEntry, err error) error {
		if err != nil {
			return err
		}
		if d.IsDir() && (d.Name() == "testdata" || (strings.HasPrefix(d.Name(), ".") && path != ".")) {
			return filepath.SkipDir
		}
		if d.IsDir() || !strings.HasSuffix(path, ".go") {
			return nil
		}
		f, err := parser.ParseFile(token.NewFileSet(), path, nil, parser.ImportsOnly)
		if err != nil {
			return err
		}
		dir := filepath.ToSlash(filepath.Dir(path))
		for _, imp := range f.Imports {
			p, _ := strconv.Unquote(imp.Path.Value)
			out[dir] = append(out[dir], p)
		}
		return nil
	})
	if err != nil {
		t.Fatal(err)
	}
	return out
}

// internalName returns "x" for "github.com/gsuquet/loom/internal/x/...".
func internalName(importPath string) (string, bool) {
	rest, ok := strings.CutPrefix(importPath, module+"/internal/")
	if !ok {
		return "", false
	}
	name, _, _ := strings.Cut(rest, "/")
	return name, true
}

func TestPackageBoundaries(t *testing.T) {
	all := imports(t)
	if _, ok := all["internal/contexts"]; !ok {
		t.Fatal("walker found no internal/contexts package; is the test running from the repo root?")
	}
	for dir, imps := range all {
		self, isInternal := internalName(module + "/" + dir)
		for _, imp := range imps {
			dep, ok := internalName(imp)
			switch {
			case !ok, dep == self:
			case dir == "cmd/loom" && dep != "cli":
				t.Errorf("%s imports %s: main must only wire internal/cli", dir, imp)
			case !isInternal, self == "cli":
			case dep == "cli":
				t.Errorf("%s imports %s: only cmd/loom may import cli", dir, imp)
			case !slices.Contains(shared, dep):
				t.Errorf("%s imports %s: packages may only share %v, never import a command package", dir, imp, shared)
			}
		}
	}
}

func TestThirdPartyDeps(t *testing.T) {
	for dir, imps := range imports(t) {
		for _, imp := range imps {
			first, _, _ := strings.Cut(imp, "/")
			if !strings.Contains(first, ".") || strings.HasPrefix(imp, module) {
				continue // stdlib or this module
			}
			if !slices.ContainsFunc(allowedDeps, func(d string) bool { return imp == d || strings.HasPrefix(imp, d+"/") }) {
				t.Errorf("%s imports %s: not in the allowed dependency list (needs an ADR)", dir, imp)
			}
		}
	}
}
