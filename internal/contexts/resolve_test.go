package contexts

import (
	"context"
	"errors"
	"os"
	"os/exec"
	"path/filepath"
	"strings"
	"testing"

	"github.com/gsuquet/loom/internal/gitx"
)

// isolateGit keeps the developer's git config out of tests.
func isolateGit(t *testing.T) {
	t.Helper()
	t.Setenv("GIT_CONFIG_GLOBAL", os.DevNull)
	t.Setenv("GIT_CONFIG_NOSYSTEM", "1")
}

func mkdir(t *testing.T, parts ...string) string {
	t.Helper()
	p := filepath.Join(parts...)
	if err := os.MkdirAll(p, 0o755); err != nil {
		t.Fatal(err)
	}
	return p
}

func git(t *testing.T, dir string, args ...string) {
	t.Helper()
	out, err := exec.Command("git", append([]string{"-C", dir}, args...)...).CombinedOutput()
	if err != nil {
		t.Fatalf("git %v: %v\n%s", args, err, out)
	}
}

// fixture builds a fake home with context directories and git repos. Every
// directory a resolveCases entry resolves by path or remote is a repo, so the
// same table also drives the git routing test.
func fixture(t *testing.T) (*Config, string) {
	t.Helper()
	isolateGit(t)
	h := t.TempDir()
	t.Setenv("GIT_CEILING_DIRECTORIES", h) // never discover a repo above the fixture
	mkdir(t, h, "context1", "github", "dotfiles")
	mkdir(t, h, "context1", "nested", "context3", "app")
	mkdir(t, h, "context2-real", "proj")
	// ~/context1 is named by its real path; ~/context2 is a symlink in match.paths.
	for link, target := range map[string]string{"gh": filepath.Join("context1", "github"), "context2": "context2-real"} {
		if err := os.Symlink(filepath.Join(h, target), filepath.Join(h, link)); err != nil {
			t.Fatal(err)
		}
	}
	for _, dir := range []string{
		filepath.Join("context1", "github", "dotfiles"), filepath.Join("context1", "nested", "context3", "app"),
		"context2-real", filepath.Join("context2-real", "proj"),
	} {
		git(t, filepath.Join(h, dir), "init", "-q")
	}
	path := mkdir(t, h, "context1", "path-beats-remote")
	git(t, path, "init", "-q")
	git(t, path, "remote", "add", "origin", "https://gitlab.example.com/g/r")
	for name, remotes := range map[string][][2]string{
		"context1-repo": {{"origin", "git@github.com:your-user/x.git"}},
		"mixed-case":    {{"origin", "git@GitHub.com:Your-User/x.git"}},
		"aliased-repo":  {{"origin", "gl:example-group/app.git"}},
		"https-repo":    {{"upstream", "https://gitlab.example.com/team/app"}},
		"https-port":    {{"origin", "https://gitlab.example.com:8443/team/app"}},
		"ssh-port":      {{"origin", "ssh://git@gitlab.example.com:2222/g/r.git"}},
		"https-user":    {{"origin", "https://token@gitlab.com/example-group/sub/r"}},
		"unknown-repo":  {{"origin", "git@github.com:someone-else/x.git"}},
		"token-repo":    {{"origin", "https://user:token@gitlab.example.com/team/app"}},
		"ambiguous":     {{"origin", "git@github.com:your-user/x.git"}, {"fork", "https://gitlab.example.com/a/b"}},
		"same-ctx":      {{"origin", "git@github.com:your-user/x.git"}, {"mirror", "https://github.com/your-user/y"}},
		"no-remote":     nil,
	} {
		dir := mkdir(t, h, "src", name)
		git(t, dir, "init", "-q")
		git(t, dir, "config", "url.git@gitlab.com:.insteadOf", "gl:")
		for _, r := range remotes {
			git(t, dir, "remote", "add", r[0], r[1])
		}
	}
	mkdir(t, h, "src", "plain", "sub")
	data, err := os.ReadFile("testdata/contexts.toml")
	if err != nil {
		t.Fatal(err)
	}
	cfg, err := Parse(string(data), h)
	if err != nil {
		t.Fatal(err)
	}
	return cfg, h
}

type resolveCase struct {
	name        string
	in          Inputs
	wantCtx     string
	wantSrc     Source
	wantMatched string // substring
	wantWarning string // substring; empty means no warning
	wantErr     string // substring
}

// resolveCases is shared by TestResolve and TestGitConfigRoutesIdentity so
// loom's resolution and git's routing are checked against the same repos.
func resolveCases(t *testing.T, h string) []resolveCase {
	src := func(name string) string { return filepath.Join(h, "src", name) }
	return []resolveCase{
		{name: "flag wins over env and path", in: Inputs{Flag: "context2", Env: "context3", Dir: filepath.Join(h, "context1")}, wantCtx: "context2", wantSrc: SourceFlag},
		{name: "env wins over path", in: Inputs{Env: "context3", Dir: filepath.Join(h, "context1")}, wantCtx: "context3", wantSrc: SourceEnv},
		{name: "unknown flag", in: Inputs{Flag: "nope"}, wantErr: `unknown context "nope" from --context; known: context1, context2, context3`},
		{name: "unknown env", in: Inputs{Env: "other"}, wantErr: `unknown context "other" from LOOM_CONTEXT`},
		{name: "path exact", in: Inputs{Dir: filepath.Join(h, "context2")}, wantCtx: "context2", wantSrc: SourcePath},
		{name: "path below", in: Inputs{Dir: filepath.Join(h, "context1", "github", "dotfiles")}, wantCtx: "context1", wantSrc: SourcePath},
		{name: "longest prefix wins", in: Inputs{Dir: filepath.Join(h, "context1", "nested", "context3", "app")}, wantCtx: "context3", wantSrc: SourcePath, wantMatched: filepath.Join("context1", "nested", "context3")},
		{name: "symlinked cwd resolved", in: Inputs{Dir: filepath.Join(h, "gh", "dotfiles")}, wantCtx: "context1", wantSrc: SourcePath},
		{name: "symlinked match.paths reached by real path", in: Inputs{Dir: filepath.Join(h, "context2-real", "proj")}, wantCtx: "context2", wantSrc: SourcePath},
		{name: "path beats remote", in: Inputs{Dir: filepath.Join(h, "context1", "path-beats-remote")}, wantCtx: "context1", wantSrc: SourcePath},
		{name: "segment boundary", in: Inputs{Dir: mkdir(t, h, "context2x")}, wantErr: "no context matches"},
		{name: "remote scp", in: Inputs{Dir: src("context1-repo")}, wantCtx: "context1", wantSrc: SourceRemote, wantMatched: "origin git@github.com:your-user/x.git"},
		{name: "remote mixed case", in: Inputs{Dir: src("mixed-case")}, wantCtx: "context1", wantSrc: SourceRemote},
		{
			name: "remote insteadOf expanded", in: Inputs{Dir: src("aliased-repo")}, wantCtx: "context3", wantSrc: SourceRemote,
			wantMatched: "git@gitlab.com:example-group/app.git", wantWarning: "insteadOf",
		},
		{name: "remote https non-origin", in: Inputs{Dir: src("https-repo")}, wantCtx: "context2", wantSrc: SourceRemote, wantMatched: "upstream "},
		{name: "remote https port", in: Inputs{Dir: src("https-port")}, wantCtx: "context2", wantSrc: SourceRemote},
		{name: "remote ssh user and port", in: Inputs{Dir: src("ssh-port")}, wantCtx: "context2", wantSrc: SourceRemote},
		{name: "remote https user", in: Inputs{Dir: src("https-user")}, wantCtx: "context3", wantSrc: SourceRemote, wantMatched: "origin https://gitlab.com/"},
		{name: "remote credentials redacted", in: Inputs{Dir: src("token-repo")}, wantCtx: "context2", wantSrc: SourceRemote, wantMatched: "origin https://gitlab.example.com/team/app"},
		{name: "remote two remotes same context", in: Inputs{Dir: src("same-ctx")}, wantCtx: "context1", wantSrc: SourceRemote, wantMatched: "origin "},
		{name: "remote ambiguous", in: Inputs{Dir: src("ambiguous")}, wantErr: `ambiguous: remotes match contexts "context1"`},
		{name: "remote owner mismatch", in: Inputs{Dir: src("unknown-repo")}, wantErr: "no context matches"},
		{name: "repo without remotes", in: Inputs{Dir: src("no-remote")}, wantErr: "no context matches"},
		{name: "not a repo", in: Inputs{Dir: filepath.Join(h, "src", "plain", "sub")}, wantErr: "pass --context or set LOOM_CONTEXT"},
	}
}

func TestResolve(t *testing.T) {
	cfg, h := fixture(t)
	for _, tt := range resolveCases(t, h) {
		t.Run(tt.name, func(t *testing.T) {
			res, err := cfg.Resolve(t.Context(), tt.in)
			if tt.wantErr != "" {
				if err == nil || !strings.Contains(err.Error(), tt.wantErr) {
					t.Fatalf("err = %v, want %q", err, tt.wantErr)
				}
				return
			}
			if err != nil {
				t.Fatalf("unexpected error: %v", err)
			}
			if res.Context.Name != tt.wantCtx || res.Source != tt.wantSrc || !strings.Contains(res.Matched, tt.wantMatched) {
				t.Errorf("got %s/%s/%q, want %s/%s/~%q", res.Context.Name, res.Source, res.Matched, tt.wantCtx, tt.wantSrc, tt.wantMatched)
			}
			if (tt.wantWarning == "") != (res.Warning == "") || !strings.Contains(res.Warning, tt.wantWarning) {
				t.Errorf("warning = %q, want ~%q", res.Warning, tt.wantWarning)
			}
		})
	}
}

func TestResolveNoMatchIsErrNoMatch(t *testing.T) {
	cfg, h := fixture(t)
	_, err := cfg.Resolve(t.Context(), Inputs{Dir: filepath.Join(h, "src", "plain")})
	if !errors.Is(err, ErrNoMatch) {
		t.Fatalf("err = %v, want ErrNoMatch", err)
	}
}

func TestResolveSymlinkedMatchPath(t *testing.T) {
	isolateGit(t)
	h := t.TempDir()
	real := mkdir(t, h, "real")
	if err := os.Symlink(real, filepath.Join(h, "alias")); err != nil {
		t.Fatal(err)
	}
	cfg, err := Parse("[contexts.a]\nmatch.paths = [\"~/alias\"]\n", h)
	if err != nil {
		t.Fatal(err)
	}
	res, err := cfg.Resolve(t.Context(), Inputs{Dir: mkdir(t, real, "x")})
	if err != nil || res.Context.Name != "a" {
		t.Fatalf("Resolve = %v, %v", res, err)
	}
}

func TestResolveRemoteListerError(t *testing.T) {
	cfg, h := fixture(t)
	boom := errors.New("boom")
	_, err := cfg.Resolve(t.Context(), Inputs{Dir: h, Remotes: func(context.Context, string) ([]gitx.Remote, error) { return nil, boom }})
	if !errors.Is(err, boom) {
		t.Fatalf("err = %v, want wrapped boom", err)
	}
}
