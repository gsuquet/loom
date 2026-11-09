package contexts

import (
	"os"
	"path/filepath"
	"slices"
	"strings"
	"testing"
)

const home = "/home/u"

func TestParseExample(t *testing.T) {
	data, err := os.ReadFile("testdata/contexts.toml")
	if err != nil {
		t.Fatal(err)
	}
	cfg, err := Parse(string(data), home)
	if err != nil {
		t.Fatalf("Parse: %v", err)
	}
	if got, want := cfg.Names(), []string{"context1", "context2", "context3"}; !slices.Equal(got, want) {
		t.Errorf("Names = %v, want %v", got, want)
	}
	n := cfg.Contexts["context3"]
	if n.Name != "context3" || n.KB != "vault" || n.Secrets != "bw" || n.KubeContext != "example-cluster" {
		t.Errorf("context3 = %+v", n)
	}
	if got := n.Match.Paths; !slices.Equal(got, []string{"~/context3", "~/context1/nested/context3"}) {
		t.Errorf("context3 paths = %v", got)
	}
	if got := cfg.Expand(cfg.KB["vault"].Path); got != "/home/u/context3/knowledge-vault" {
		t.Errorf("Expand = %q", got)
	}
}

func TestParse(t *testing.T) {
	const minimal = "[contexts.context1]\nmatch.paths = [\"~/context1\"]\n"
	tests := []struct {
		name    string
		text    string
		wantErr []string // substrings; empty means success
	}{
		{name: "minimal", text: minimal},
		{name: "bad toml", text: "[contexts.context1\n", wantErr: []string{"invalid TOML"}},
		{name: "no contexts", text: "[git]\nprotected_branches = [\"main\"]\n", wantErr: []string{"no [contexts.<name>]"}},
		{name: "unknown key", text: minimal + "kube = \"x\"\n", wantErr: []string{"unknown keys: contexts.context1.kube"}},
		{name: "unknown match key", text: minimal + "match.host = [\"github.com\"]\n", wantErr: []string{"contexts.context1.match.host"}},
		{name: "bad name", text: "[contexts.Work]\n", wantErr: []string{"contexts.Work: name must match"}},
		{name: "relative path", text: "[contexts.a]\nmatch.paths = [\"rel/dir\"]\n", wantErr: []string{`"rel/dir" must be absolute or start with ~/`}},
		{name: "tilde user", text: "[contexts.a]\nmatch.paths = [\"~bob/x\"]\n", wantErr: []string{`"~bob/x" must be absolute`}},
		{
			name:    "duplicate path",
			text:    "[contexts.a]\nmatch.paths = [\"~/x\"]\n[contexts.b]\nmatch.paths = [\"~/x/\"]\n",
			wantErr: []string{`contexts.b: match.paths entry "~/x/" is already used by context "a"`},
		},
		{name: "host with scheme", text: "[contexts.a]\nmatch.hosts = [\"https://github.com\"]\n", wantErr: []string{"without scheme or user"}},
		{name: "host with user", text: "[contexts.a]\nmatch.hosts = [\"git@github.com\"]\n", wantErr: []string{"without scheme or user"}},
		{name: "host with scp colon", text: "[contexts.a]\nmatch.hosts = [\"github.com:owner\"]\n", wantErr: []string{"must look like host[/owner]"}},
		{
			name:    "duplicate host case-insensitive",
			text:    "[contexts.a]\nmatch.hosts = [\"GitHub.com/x\"]\n[contexts.b]\nmatch.hosts = [\"github.com/x\"]\n",
			wantErr: []string{`entry "github.com/x" is already used by context "a"`},
		},
		{name: "missing kb", text: minimal + "kb = \"commons\"\n", wantErr: []string{`kb "commons" has no [kb.commons] table`}},
		{name: "kb without path", text: minimal + "[kb.commons]\n", wantErr: []string{`kb.commons: path "" must be set`}},
		{name: "relative git_include", text: minimal + "git_include = \"context1.gitconfig\"\n", wantErr: []string{"git_include"}},
		{
			name:    "all errors reported",
			text:    "[contexts.a]\nmatch.paths = [\"rel\"]\nkb = \"nope\"\n",
			wantErr: []string{"must be absolute", `kb "nope"`},
		},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			cfg, err := Parse(tt.text, home)
			if len(tt.wantErr) == 0 {
				if err != nil {
					t.Fatalf("unexpected error: %v", err)
				}
				if !slices.Equal(cfg.Git.ProtectedBranches, DefaultProtectedBranches) {
					t.Errorf("ProtectedBranches = %v", cfg.Git.ProtectedBranches)
				}
				return
			}
			if err == nil {
				t.Fatalf("expected error containing %q", tt.wantErr)
			}
			for _, w := range tt.wantErr {
				if !strings.Contains(err.Error(), w) {
					t.Errorf("error %q does not contain %q", err, w)
				}
			}
		})
	}
}

func TestDefaultPath(t *testing.T) {
	tests := []struct {
		name string
		env  map[string]string
		want string
	}{
		{"default", nil, "/home/u/.config/loom/contexts.toml"},
		{"xdg", map[string]string{"XDG_CONFIG_HOME": "/xdg"}, "/xdg/loom/contexts.toml"},
		{"env wins", map[string]string{"XDG_CONFIG_HOME": "/xdg", EnvConfig: "/etc/c.toml"}, "/etc/c.toml"},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			got := DefaultPath(func(k string) string { return tt.env[k] }, home)
			if got != filepath.FromSlash(tt.want) {
				t.Errorf("DefaultPath = %q, want %q", got, tt.want)
			}
		})
	}
}

func TestLoad(t *testing.T) {
	dir := t.TempDir()
	if _, err := Load(filepath.Join(dir, "missing.toml"), home); err == nil || !strings.Contains(err.Error(), "contexts config not found") {
		t.Errorf("missing file: err = %v", err)
	}
	bad := filepath.Join(dir, "bad.toml")
	if err := os.WriteFile(bad, []byte("[contexts.a]\nmatch.paths = [\"rel\"]\n"), 0o600); err != nil {
		t.Fatal(err)
	}
	if _, err := Load(bad, home); err == nil || !strings.HasPrefix(err.Error(), bad+": ") {
		t.Errorf("invalid file: err = %v, want prefix %q", err, bad)
	}
	cfg, err := Load("testdata/contexts.toml", home)
	if err != nil || cfg.Path != "testdata/contexts.toml" {
		t.Errorf("Load = %v, %v", cfg, err)
	}
}
