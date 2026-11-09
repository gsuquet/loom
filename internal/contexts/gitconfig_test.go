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

func assertGolden(t *testing.T, path string, got []byte) {
	t.Helper()
	if *update {
		if err := os.WriteFile(path, got, 0o600); err != nil {
			t.Fatal(err)
		}
	}
	want, err := os.ReadFile(path)
	if err != nil {
		t.Fatalf("%v (run go test -update to create it)", err)
	}
	if !bytes.Equal(got, want) {
		t.Errorf("output differs from %s (go test -update to accept)\n--- got\n%s\n--- want\n%s", path, got, want)
	}
}

// TestGitConfigRoutesIdentity runs real git against the generated config and
// checks every repo gets the identity of the context Resolve picks.
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

	repo := func(remote string, parts ...string) string {
		dir := mkdir(t, append([]string{h}, parts...)...)
		git(t, dir, "init", "-q")
		if remote != "" {
			git(t, dir, "remote", "add", "origin", remote)
		}
		return dir
	}
	for _, dir := range []string{
		repo("", "context1", "plain"),
		repo("", "context1", "nested", "context3", "deep"),                      // longest path wins
		repo("https://gitlab.example.com/g/r", "context1", "path-beats-remote"), // path beats remote
		repo("", "context2", "p"),
		repo("ssh://git@gitlab.example.com:2222/g/r.git", "src", "ssh-port"),
		repo("https://token@gitlab.com/example-group/sub/r", "src", "https-user"),
		repo("git@github.com:your-user/r.git", "src", "scp"),
	} {
		res, err := cfg.Resolve(Inputs{Dir: dir})
		if err != nil {
			t.Fatalf("%s: Resolve: %v", dir, err)
		}
		out, err := exec.Command("git", "-C", dir, "config", "user.name").Output()
		if got := strings.TrimSpace(string(out)); err != nil || got != res.Context.Name {
			t.Errorf("%s: git user.name = %q (%v), Resolve = %q", dir, got, err, res.Context.Name)
		}
	}
}
