// Package gitx holds thin helpers around the git executable. It is shared by
// command packages and must not depend on any of them.
package gitx

import (
	"bytes"
	"context"
	"errors"
	"fmt"
	"os"
	"os/exec"
	"slices"
	"sort"
	"strings"
	"time"
)

// ErrNotRepo is returned when dir is not inside a git work tree.
var ErrNotRepo = errors.New("not inside a git repository")

// waitDelay bounds how long Run waits for git's output pipes after ctx is done.
const waitDelay = 5 * time.Second

// droppedEnv are inherited variables that would make `git -C dir` act on
// another repository (`git rev-parse --local-env-vars`, plus GIT_NAMESPACE),
// or that Run sets itself. Like git's own prepare_other_repo_env, the
// GIT_CONFIG_PARAMETERS/GIT_CONFIG_COUNT pair (`git -c`) is kept.
var droppedEnv = []string{
	"GIT_ALTERNATE_OBJECT_DIRECTORIES", "GIT_COMMON_DIR", "GIT_CONFIG", "GIT_DIR",
	"GIT_GRAFT_FILE", "GIT_IMPLICIT_WORK_TREE", "GIT_INDEX_FILE", "GIT_NAMESPACE",
	"GIT_NO_REPLACE_OBJECTS", "GIT_OBJECT_DIRECTORY", "GIT_PREFIX", "GIT_REPLACE_REF_BASE",
	"GIT_SHALLOW_FILE", "GIT_WORK_TREE",
	"LC_ALL", "GIT_TERMINAL_PROMPT",
}

// env returns the process environment for git: repository-local variables
// removed, messages in English (ErrNotRepo matches them) and no prompts.
func env() []string {
	return append(slices.DeleteFunc(os.Environ(), func(kv string) bool {
		k, _, _ := strings.Cut(kv, "=")
		return slices.Contains(droppedEnv, k)
	}), "LC_ALL=C", "GIT_TERMINAL_PROMPT=0")
}

// Run executes git with args in dir and returns trimmed stdout. Cancelling
// ctx kills git.
func Run(ctx context.Context, dir string, args ...string) (string, error) {
	cmd := exec.CommandContext(ctx, "git", append([]string{"-C", dir}, args...)...)
	cmd.Env = env()
	cmd.WaitDelay = waitDelay
	var stdout, stderr bytes.Buffer
	cmd.Stdout, cmd.Stderr = &stdout, &stderr
	if err := cmd.Run(); err != nil {
		msg := strings.TrimSpace(stderr.String())
		if strings.Contains(msg, "not a git repository") {
			return "", ErrNotRepo
		}
		return "", fmt.Errorf("git %s: %w: %s", strings.Join(args, " "), err, msg)
	}
	return strings.TrimSpace(stdout.String()), nil
}

// Remote is a named remote and its fetch URL with insteadOf rewrites applied.
type Remote struct {
	Name string
	URL  string
}

// Remotes lists the remotes of the repository containing dir, origin first,
// then by name. A directory outside any repository yields ErrNotRepo.
func Remotes(ctx context.Context, dir string) ([]Remote, error) {
	// `git remote -v` prints URLs after insteadOf expansion, unlike reading .git/config.
	out, err := Run(ctx, dir, "remote", "-v")
	if err != nil {
		return nil, err
	}
	var remotes []Remote
	for line := range strings.SplitSeq(out, "\n") {
		fields := strings.Fields(line)
		if len(fields) == 3 && fields[2] == "(fetch)" {
			remotes = append(remotes, Remote{Name: fields[0], URL: fields[1]})
		}
	}
	sort.SliceStable(remotes, func(i, j int) bool {
		if (remotes[i].Name == "origin") != (remotes[j].Name == "origin") {
			return remotes[i].Name == "origin"
		}
		return remotes[i].Name < remotes[j].Name
	})
	return remotes, nil
}

// HostPath normalises a remote URL to "host/path" without scheme, user, port
// or ".git" suffix, lowercased, so it can be prefix-matched against
// "host[/owner]" patterns. It returns "" for local paths and unparsable URLs.
func HostPath(rawURL string) string {
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
