package gitx

import (
	"errors"
	"os"
	"os/exec"
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

func TestRemotes(t *testing.T) {
	t.Setenv("GIT_CONFIG_GLOBAL", os.DevNull)
	t.Setenv("GIT_CONFIG_NOSYSTEM", "1")
	dir := t.TempDir()
	t.Setenv("GIT_CEILING_DIRECTORIES", dir)
	if _, err := Remotes(dir); !errors.Is(err, ErrNotRepo) {
		t.Fatalf("outside repo: err = %v, want ErrNotRepo", err)
	}
	for _, args := range [][]string{
		{"init", "-q"},
		{"config", "url.git@github.com:.insteadOf", "gh:"},
		{"remote", "add", "zeta", "https://example.com/z"},
		{"remote", "add", "alpha", "https://example.com/a"},
		{"remote", "add", "origin", "gh:owner/repo"},
	} {
		if out, err := exec.Command("git", append([]string{"-C", dir}, args...)...).CombinedOutput(); err != nil {
			t.Fatalf("git %v: %v %s", args, err, out)
		}
	}
	got, err := Remotes(dir)
	if err != nil {
		t.Fatal(err)
	}
	want := []Remote{{"origin", "git@github.com:owner/repo"}, {"alpha", "https://example.com/a"}, {"zeta", "https://example.com/z"}}
	if !slices.Equal(got, want) {
		t.Errorf("Remotes = %v, want %v", got, want)
	}
}
