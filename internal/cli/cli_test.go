package cli

import (
	"bytes"
	"encoding/json"
	"errors"
	"flag"
	"os"
	"path/filepath"
	"strings"
	"testing"
)

var update = flag.Bool("update", false, "rewrite golden files")

type result struct {
	code           int
	stdout, stderr string
}

func run(t *testing.T, env map[string]string, wd string, args ...string) result {
	t.Helper()
	var out, errb bytes.Buffer
	code := Run(args, Env{
		Stdout: &out, Stderr: &errb,
		Getenv:  func(k string) string { return env[k] },
		Getwd:   func() (string, error) { return wd, nil },
		HomeDir: func() (string, error) { return "/home/u", nil },
		Version: "v1.2.3-test",
	})
	return result{code, out.String(), errb.String()}
}

func TestDispatch(t *testing.T) {
	cfg := []string{"--config", "testdata/contexts.toml"}
	outside := t.TempDir() // neither under a match.paths entry nor in a git repo
	t.Setenv("GIT_CEILING_DIRECTORIES", outside)
	tests := []struct {
		name       string
		args       []string
		env        map[string]string
		wd         string
		wantCode   int
		wantStdout string // substring
		wantStderr string // substring
	}{
		{name: "no args", wantCode: ExitUsage, wantStderr: "Usage:\n  loom <command>"},
		{name: "-h", args: []string{"-h"}, wantStdout: "Commands:\n  context"},
		{name: "help", args: []string{"help"}, wantStdout: "  version  Print the loom version"},
		{name: "help help", args: []string{"help", "help"}, wantStdout: "Commands:"},
		{name: "help context", args: []string{"help", "context"}, wantStdout: "loom context gitconfig [--config file]"},
		{name: "help unknown", args: []string{"help", "nope"}, wantCode: ExitUsage, wantStderr: `unknown command "nope"`},
		{name: "version", args: []string{"version"}, wantStdout: "loom v1.2.3-test\n"},
		{name: "--version", args: []string{"--version"}, wantStdout: "loom v1.2.3-test\n"},
		{name: "version extra arg", args: []string{"version", "x"}, wantCode: ExitUsage, wantStderr: "unexpected arguments: x"},
		{name: "unknown command", args: []string{"frobnicate"}, wantCode: ExitUsage, wantStderr: `unknown command "frobnicate"`},
		{name: "bad flag", args: []string{"context", "--nope"}, wantCode: ExitUsage, wantStderr: "flag provided but not defined: -nope"},
		{name: "context extra arg", args: []string{"context", "extra"}, wantCode: ExitUsage, wantStderr: "unexpected arguments: extra"},
		{name: "gitconfig extra arg", args: []string{"context", "gitconfig", "x"}, wantCode: ExitUsage, wantStderr: "unexpected arguments: x"},
		{name: "gitconfig -h", args: []string{"context", "gitconfig", "-h"}, wantStdout: "loom context gitconfig [--config file]"},
		{
			name: "missing config", args: []string{"context"}, env: map[string]string{"LOOM_CONTEXTS": "/nonexistent/c.toml"},
			wantCode: ExitError, wantStderr: "loom: contexts config not found at /nonexistent/c.toml",
		},
		{
			name: "text output via path", args: append([]string{"context"}, cfg...), wd: "/home/u/context1/nested/context3/app",
			wantStdout: "context:      context3\nsource:       path (/home/u/context1/nested/context3)\nkb:           vault (/home/u/context3/knowledge-vault)\n",
		},
		{name: "-c flag", args: append([]string{"context", "-c", "context2"}, cfg...), wantStdout: "source:       flag (context2)"},
		{name: "env context", args: append([]string{"context"}, cfg...), env: map[string]string{"LOOM_CONTEXT": "context1"}, wantStdout: "source:       env (context1)"},
		{name: "unknown context", args: append([]string{"context", "--context", "work"}, cfg...), wantCode: ExitError, wantStderr: `unknown context "work"`},
		{name: "no match is an error", args: append([]string{"context"}, cfg...), wd: outside, wantCode: ExitError, wantStderr: "no context matches " + outside},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			r := run(t, tt.env, tt.wd, tt.args...)
			if r.code != tt.wantCode {
				t.Errorf("exit = %d, want %d\nstdout: %s\nstderr: %s", r.code, tt.wantCode, r.stdout, r.stderr)
			}
			if !strings.Contains(r.stdout, tt.wantStdout) {
				t.Errorf("stdout %q does not contain %q", r.stdout, tt.wantStdout)
			}
			if !strings.Contains(r.stderr, tt.wantStderr) {
				t.Errorf("stderr %q does not contain %q", r.stderr, tt.wantStderr)
			}
		})
	}
}

func TestContextJSON(t *testing.T) {
	r := run(t, nil, "/", "context", "--json", "-c", "context3", "--config", "testdata/contexts.toml")
	if r.code != ExitOK {
		t.Fatalf("exit %d: %s", r.code, r.stderr)
	}
	var got contextJSON
	if err := json.Unmarshal([]byte(r.stdout), &got); err != nil {
		t.Fatalf("invalid JSON %q: %v", r.stdout, err)
	}
	want := contextJSON{
		Name: "context3", Source: "flag", Matched: "context3",
		KB:                &kbOutput{Name: "vault", Path: "/home/u/context3/knowledge-vault"},
		GitInclude:        "/home/u/.config/git/context3.gitconfig",
		Secrets:           "bw",
		KubeContext:       "example-cluster",
		ProtectedBranches: []string{"main", "master", "trunk", "production"},
		Config:            "testdata/contexts.toml",
	}
	gotJSON, _ := json.Marshal(got)
	wantJSON, _ := json.Marshal(want)
	if !bytes.Equal(gotJSON, wantJSON) {
		t.Errorf("got  %s\nwant %s", gotJSON, wantJSON)
	}
}

func TestContextGitconfigGolden(t *testing.T) {
	r := run(t, nil, "/", "context", "gitconfig", "--config", "testdata/contexts.toml")
	if r.code != ExitOK {
		t.Fatalf("exit %d: %s", r.code, r.stderr)
	}
	path := filepath.Join("testdata", "gitconfig.golden")
	if *update {
		if err := os.WriteFile(path, []byte(r.stdout), 0o600); err != nil {
			t.Fatal(err)
		}
	}
	want, err := os.ReadFile(path)
	if err != nil {
		t.Fatalf("%v (run go test -update to create it)", err)
	}
	if r.stdout != string(want) {
		t.Errorf("output differs from %s (go test -update to accept)\n%s", path, r.stdout)
	}
}

func TestExitCodeWrapsErrors(t *testing.T) {
	var errb bytes.Buffer
	env := Env{Stderr: &errb}
	if got := exitCode(env, flag.ErrHelp); got != ExitOK {
		t.Errorf("ErrHelp -> %d", got)
	}
	if got := exitCode(env, errUsage); got != ExitUsage || errb.Len() != 0 {
		t.Errorf("errUsage -> %d, stderr %q", got, errb.String())
	}
	if got := exitCode(env, errors.New("boom")); got != ExitError || errb.String() != "loom: boom\n" {
		t.Errorf("error -> %d, stderr %q", got, errb.String())
	}
}
