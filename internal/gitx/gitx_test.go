package gitx

import (
	"context"
	"errors"
	"os"
	"os/exec"
	"path/filepath"
	"slices"
	"testing"
)

func TestHostPath(t *testing.T) {
	tests := []struct{ in, want string }{
		{"git@github.com:Owner/Repo.git", "github.com/owner/repo"},
		{"github.com:owner/repo", "github.com/owner/repo"},
		{"https://github.com/owner/repo.git", "github.com/owner/repo"},
		{"https://token@github.com/owner/repo/", "github.com/owner/repo"},
		{"ssh://git@gitlab.example.com:2222/group/sub/app.git", "gitlab.example.com/group/sub/app"},
		{"ssh://git@gitlab.example.com/group/app", "gitlab.example.com/group/app"},
		{"/srv/git/repo.git", ""},
		{"file:///srv/git/repo.git", ""},
		{"../relative", ""},
		{"", ""},
	}
	for _, tt := range tests {
		if got := HostPath(tt.in); got != tt.want {
			t.Errorf("HostPath(%q) = %q, want %q", tt.in, got, tt.want)
		}
	}
}

// gitInit creates a repository in dir and runs each git command in it.
func gitInit(t *testing.T, dir string, cmds ...[]string) {
	t.Helper()
	for _, args := range append([][]string{{"init", "-q"}}, cmds...) {
		if out, err := exec.Command("git", append([]string{"-C", dir}, args...)...).CombinedOutput(); err != nil {
			t.Fatalf("git %v: %v %s", args, err, out)
		}
	}
}

func TestRemotes(t *testing.T) {
	t.Setenv("GIT_CONFIG_GLOBAL", os.DevNull)
	t.Setenv("GIT_CONFIG_NOSYSTEM", "1")
	dir := t.TempDir()
	t.Setenv("GIT_CEILING_DIRECTORIES", dir)
	gitInit(t, dir,
		[]string{"config", "url.git@github.com:.insteadOf", "gh:"},
		[]string{"remote", "add", "zeta", "https://example.com/z"},
		[]string{"remote", "add", "alpha", "https://example.com/a"},
		[]string{"remote", "add", "origin", "gh:owner/repo"},
	)
	got, err := Remotes(t.Context(), dir)
	if err != nil {
		t.Fatal(err)
	}
	want := []Remote{{"origin", "git@github.com:owner/repo"}, {"alpha", "https://example.com/a"}, {"zeta", "https://example.com/z"}}
	if !slices.Equal(got, want) {
		t.Errorf("Remotes = %v, want %v", got, want)
	}
}

// TestRunEnv checks that the caller's environment cannot change what Run
// reports: a translated locale or a GIT_DIR exported by a git hook or alias.
func TestRunEnv(t *testing.T) {
	t.Setenv("GIT_CONFIG_GLOBAL", os.DevNull)
	t.Setenv("GIT_CONFIG_NOSYSTEM", "1")
	root := t.TempDir()
	t.Setenv("GIT_CEILING_DIRECTORIES", root)
	plain, repo, other := filepath.Join(root, "plain"), filepath.Join(root, "repo"), filepath.Join(root, "other")
	for _, d := range []string{plain, repo, other} {
		if err := os.Mkdir(d, 0o755); err != nil {
			t.Fatal(err)
		}
	}
	gitInit(t, repo, []string{"remote", "add", "origin", "https://example.com/repo"})
	gitInit(t, other, []string{"remote", "add", "origin", "https://example.com/other"})
	tests := []struct {
		name    string
		env     map[string]string
		dir     string
		want    []Remote
		wantErr error
	}{
		// git falls back to English when the locale is not installed, so this case
		// only catches a regression on machines that have it; it never fails spuriously.
		{name: "translated locale", env: map[string]string{"LC_ALL": "fr_FR.UTF-8", "LANG": "fr_FR.UTF-8", "LANGUAGE": "fr"}, dir: plain, wantErr: ErrNotRepo},
		{name: "GIT_DIR outside repo", env: map[string]string{"GIT_DIR": filepath.Join(other, ".git")}, dir: plain, wantErr: ErrNotRepo},
		{
			name: "GIT_DIR and GIT_WORK_TREE in repo",
			env:  map[string]string{"GIT_DIR": filepath.Join(other, ".git"), "GIT_WORK_TREE": other},
			dir:  repo, want: []Remote{{"origin", "https://example.com/repo"}},
		},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			for k, v := range tt.env {
				t.Setenv(k, v)
			}
			got, err := Remotes(t.Context(), tt.dir)
			if !errors.Is(err, tt.wantErr) || !slices.Equal(got, tt.want) {
				t.Errorf("Remotes = %v, %v; want %v, %v", got, err, tt.want, tt.wantErr)
			}
		})
	}
}

func TestRunCancelled(t *testing.T) {
	ctx, cancel := context.WithCancel(t.Context())
	cancel()
	if _, err := Run(ctx, t.TempDir(), "version"); !errors.Is(err, context.Canceled) {
		t.Errorf("err = %v, want context.Canceled", err)
	}
}
