package contexts

import (
	"bytes"
	"flag"
	"os"
	"os/exec"
	"path/filepath"
	"strings"
	"testing"
)

var update = flag.Bool("update", false, "rewrite golden files")

func TestWriteGitConfigGolden(t *testing.T) {
	cfg, err := Load("testdata/contexts.toml", home)
	if err != nil {
		t.Fatal(err)
	}
	var buf bytes.Buffer
	if err := cfg.WriteGitConfig(&buf); err != nil {
		t.Fatal(err)
	}
	assertGolden(t, "testdata/gitconfig.golden", buf.Bytes())
}

func TestWriteGitConfigSkipsContextsWithoutInclude(t *testing.T) {
	cfg, err := Parse("[contexts.a]\nmatch.paths = [\"~/a\"]\nmatch.hosts = [\"example.com\"]\n", home)
	if err != nil {
		t.Fatal(err)
	}
	var buf bytes.Buffer
	if err := cfg.WriteGitConfig(&buf); err != nil {
		t.Fatal(err)
	}
	if bytes.Contains(buf.Bytes(), []byte("[includeIf")) {
		t.Errorf("unexpected includeIf:\n%s", buf.String())
	}
}

// TestWriteGitConfigQuotes checks with real git that quotes, backslashes and
// comment characters in paths survive as data.
func TestWriteGitConfigQuotes(t *testing.T) {
	tmp := isolateGit(t)
	cfg, err := Parse("[contexts.a]\nmatch.paths = [\"/my \\\"dir\\\" #1; x\"]\ngit_include = \"/inc \\\\ #2.gitconfig\"\n", home)
	if err != nil {
		t.Fatal(err)
	}
	var buf bytes.Buffer
	if err := cfg.WriteGitConfig(&buf); err != nil {
		t.Fatal(err)
	}
	file := filepath.Join(tmp, "gitconfig")
	if err := os.WriteFile(file, buf.Bytes(), 0o600); err != nil {
		t.Fatal(err)
	}
	out, err := exec.Command("git", "config", "--file", file, "--list").CombinedOutput()
	want := `includeif.gitdir:/my "dir" #1; x/.path=/inc \ #2.gitconfig`
	if got := strings.TrimSpace(string(out)); err != nil || got != want {
		t.Errorf("git config --list = %q (%v), want %q", got, err, want)
	}
}

func assertGolden(t *testing.T, path string, got []byte) {
	t.Helper()
	if *update {
		if err := os.WriteFile(path, got, 0o600); err != nil {
			t.Fatal(err)
		}
	}
	want, err := os.ReadFile(path)
	if err != nil {
		t.Fatalf("%v (run `mise run golden` to create it)", err)
	}
	if !bytes.Equal(got, want) {
		t.Errorf("output differs from %s (`mise run golden` to accept)\n--- got\n%s\n--- want\n%s", path, got, want)
	}
}

// TestGitConfigRoutesIdentity runs real git against the generated config in
// every repo of the TestResolve table: git must apply the git_include of the
// context Resolve picks, unless Resolve warned that it cannot.
func TestGitConfigRoutesIdentity(t *testing.T) {
	cfg, h := fixture(t)
	for _, name := range cfg.Names() {
		inc := filepath.Join(h, ".config", "git", name+".gitconfig")
		mkdir(t, filepath.Dir(inc))
		if err := os.WriteFile(inc, []byte("[user]\n\tname = "+name+"\n"), 0o600); err != nil {
			t.Fatal(err)
		}
	}
	var buf bytes.Buffer
	if err := cfg.WriteGitConfig(&buf); err != nil {
		t.Fatal(err)
	}
	global := filepath.Join(h, "generated.gitconfig")
	if err := os.WriteFile(global, buf.Bytes(), 0o600); err != nil {
		t.Fatal(err)
	}
	t.Setenv("HOME", h) // git expands "~" in gitdir: and path = with $HOME
	t.Setenv("GIT_CONFIG_GLOBAL", global)

	for _, tt := range resolveCases(t, h) {
		if tt.wantErr != "" || tt.in.Flag != "" || tt.in.Env != "" {
			continue // only path and remote resolutions are mirrored by git
		}
		t.Run(tt.name, func(t *testing.T) {
			res, err := cfg.Resolve(t.Context(), tt.in)
			if err != nil {
				t.Fatalf("Resolve: %v", err)
			}
			out, _ := exec.Command("git", "-C", tt.in.Dir, "config", "user.name").Output()
			got := strings.TrimSpace(string(out))
			switch {
			case res.Warning == "" && got != res.Context.Name:
				t.Errorf("git user.name = %q, Resolve = %q", got, res.Context.Name)
			case res.Warning != "" && got == res.Context.Name:
				t.Errorf("git routes to %q but Resolve warns %q", got, res.Warning)
			}
		})
	}
}
