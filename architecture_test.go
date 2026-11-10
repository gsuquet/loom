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
// under internal/ except cli is a command package (gitcmd, memstore, views, hooks, knowledge).
// TODO(slice 1b): allow the edges views→memstore and hooks→memstore/views (docs/design.md §8).
var shared = []string{"contexts", "forge", "gitx"}

// core packages are the functional core (ADR 0016): they decide, adapters
// (cli, gitx, forge, hooks…) run processes. memstore and views are listed
// before they exist so the rule applies from their first commit.
var core = []string{"contexts", "memstore", "views"}

// coreForbidden are the imports that would let a core package run a subprocess.
var coreForbidden = []string{"os/exec", module + "/internal/gitx", module + "/internal/forge"}

// allowedDeps are the only third-party modules; adding one needs an ADR (ADR 0004).
var allowedDeps = []string{"github.com/BurntSushi/toml", "gopkg.in/yaml.v3"}

// imports maps each package dir (slash-separated, relative) to its imports,
// test files included only when withTests is set.
func imports(t *testing.T, withTests bool) map[string][]string {
	t.Helper()
	out := map[string][]string{}
	err := filepath.WalkDir(".", func(path string, d fs.DirEntry, err error) error {
		if err != nil {
			return err
		}
		if d.IsDir() && (d.Name() == "testdata" || (strings.HasPrefix(d.Name(), ".") && path != ".")) {
			return filepath.SkipDir
		}
		if d.IsDir() || !strings.HasSuffix(path, ".go") || (!withTests && strings.HasSuffix(path, "_test.go")) {
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
	all := imports(t, true)
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
	for dir, imps := range imports(t, true) {
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

// TestCoreRunsNoSubprocess keeps the functional core pure: non-test files of
// core packages never import os/exec, gitx or forge; callers inject the facts.
func TestCoreRunsNoSubprocess(t *testing.T) {
	for dir, imps := range imports(t, false) {
		name, ok := internalName(module + "/" + dir)
		if !ok || !slices.Contains(core, name) {
			continue
		}
		for _, imp := range imps {
			if slices.Contains(coreForbidden, imp) {
				t.Errorf("%s imports %s: core packages %v run no subprocess; inject the result from cli (ADR 0016)", dir, imp, core)
			}
		}
	}
}
