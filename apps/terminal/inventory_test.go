package terminal_test

import (
	"encoding/json"
	"go/ast"
	"go/parser"
	"go/token"
	"os"
	"path/filepath"
	"regexp"
	"strconv"
	"strings"
	"testing"

	"github.com/Jagalite/motion/apps/terminal/internal/cli"
	"github.com/spf13/cobra"
)

type inventoryEntry struct {
	ID           string              `json:"id"`
	Workflow     string              `json:"workflow"`
	Status       string              `json:"status"`
	Shared       []string            `json:"shared"`
	API          []string            `json:"api"`
	CLI          *string             `json:"cli"`
	TUI          *string             `json:"tui"`
	Capabilities []string            `json:"capabilities"`
	Tests        map[string][]string `json:"tests"`
	Gap          string              `json:"gap"`
	Exception    *struct {
		Reason      string `json:"reason"`
		Disposition string `json:"disposition"`
	} `json:"exception"`
}

func inventory(t *testing.T) []inventoryEntry {
	t.Helper()
	data, err := os.ReadFile("operations.json")
	if err != nil {
		t.Fatal(err)
	}
	var doc struct {
		Schema     string           `json:"schema"`
		Note       string           `json:"note"`
		Operations []inventoryEntry `json:"operations"`
	}
	dec := json.NewDecoder(strings.NewReader(string(data)))
	dec.DisallowUnknownFields()
	if err := dec.Decode(&doc); err != nil || doc.Schema != "motion.terminal.operations.v1" {
		t.Fatalf("operations.json: %v", err)
	}
	return doc.Operations
}

// parsed returns the non-test or test files of one package directory,
// keyed by path.
func parsed(t *testing.T, dir string, tests bool) map[string]*ast.File {
	t.Helper()
	paths, _ := filepath.Glob(filepath.Join(dir, "*.go"))
	files := map[string]*ast.File{}
	for _, path := range paths {
		if strings.HasSuffix(path, "_test.go") != tests {
			continue
		}
		f, err := parser.ParseFile(token.NewFileSet(), path, nil, 0)
		if err != nil {
			t.Fatal(err)
		}
		files[path] = f
	}
	return files
}

func declared(files map[string]*ast.File) map[string]bool {
	names := map[string]bool{}
	for _, f := range files {
		for _, d := range f.Decls {
			switch d := d.(type) {
			case *ast.FuncDecl:
				if d.Recv == nil {
					names[d.Name.Name] = true
				}
			case *ast.GenDecl:
				for _, s := range d.Specs {
					if s, ok := s.(*ast.TypeSpec); ok {
						names[s.Name.Name] = true
					}
				}
			}
		}
	}
	return names
}

func commands(root *cobra.Command) map[string]bool {
	out := map[string]bool{}
	var walk func(*cobra.Command, string)
	walk = func(c *cobra.Command, prefix string) {
		for _, child := range c.Commands() {
			path := strings.TrimSpace(prefix + " " + child.Name())
			out[path] = true
			walk(child, path)
		}
	}
	walk(root, "")
	return out
}

// Every inventoried operation is either shared with tests at each layer or
// a tracked gap; a missing entry point always has a recorded exception.
func TestOperationInventoryIsCompleteAndTruthful(t *testing.T) {
	entries := inventory(t)
	matrix, err := os.ReadFile("PARITY.md")
	if err != nil {
		t.Fatal(err)
	}
	cmds := commands(cli.NewRoot(&cli.Env{}))
	shared := declared(parsed(t, filepath.Join("internal", "operations"), false))
	tests := map[string]map[string]bool{}
	testsIn := func(dir string) map[string]bool {
		if tests[dir] == nil {
			tests[dir] = declared(parsed(t, dir, true))
		}
		return tests[dir]
	}
	id := regexp.MustCompile(`^[a-z]+(\.[a-z]+)+$`)
	seen := map[string]bool{}
	covered := map[string]bool{}
	for _, e := range entries {
		if !id.MatchString(e.ID) || seen[e.ID] || e.Workflow == "" {
			t.Errorf("%q: missing, malformed or duplicate identifier or workflow", e.ID)
		}
		seen[e.ID] = true
		if !strings.Contains(string(matrix), "| `"+e.ID+"` |") {
			t.Errorf("%s: missing from PARITY.md", e.ID)
		}
		if e.CLI != nil {
			if !cmds[*e.CLI] {
				t.Errorf("%s: CLI entry %q is not a command", e.ID, *e.CLI)
			}
			covered[*e.CLI] = true
		}
		if (e.CLI == nil || e.TUI == nil) && (e.Exception == nil || e.Exception.Reason == "" || e.Exception.Disposition == "") {
			t.Errorf("%s: an entry point is missing without a recorded exception", e.ID)
		}
		switch e.Status {
		case "shared":
			if len(e.Shared) == 0 || e.CLI == nil || e.TUI == nil {
				t.Errorf("%s: a shared operation needs its implementation and both entry points", e.ID)
			}
			for _, name := range e.Shared {
				if !shared[name] {
					t.Errorf("%s: internal/operations does not declare %s", e.ID, name)
				}
			}
			for _, layer := range []string{"operations", "cli", "tui"} {
				if len(e.Tests[layer]) == 0 {
					t.Errorf("%s: no %s tests", e.ID, layer)
				}
				for _, ref := range e.Tests[layer] {
					dir, name, _ := strings.Cut(ref, ":")
					if !testsIn(filepath.FromSlash(dir))[name] {
						t.Errorf("%s: test %s does not exist", e.ID, ref)
					}
				}
			}
		case "pending":
			if e.Gap == "" && e.Exception == nil {
				t.Errorf("%s: a pending operation must record its gap", e.ID)
			}
			if len(e.Shared) != 0 || len(e.API) != 0 {
				t.Errorf("%s: a pending operation claims shared code", e.ID)
			}
		default:
			t.Errorf("%s: unknown status %q", e.ID, e.Status)
		}
	}
	// Every runnable command is inventoried; grouping commands, the TUI
	// launcher and Cobra's own commands are not operations.
	for path := range cmds {
		group := false
		for other := range cmds {
			group = group || strings.HasPrefix(other, path+" ")
		}
		if !covered[path] && !group && path != "tui" && path != "help" && !strings.HasPrefix(path, "completion") {
			t.Errorf("command %q is not in operations.json", path)
		}
	}
}

// CLI and TUI code reaches migrated workflows only through internal/operations:
// a method a shared operation owns is never called on the API client or on a
// backend from interface code.
func TestInterfacesDoNotBypassSharedOperations(t *testing.T) {
	owned := map[string]string{}
	for _, e := range inventory(t) {
		for _, m := range e.API {
			owned[m] = e.ID
		}
	}
	for _, dir := range []string{"cli", "tui"} {
		for path, f := range parsed(t, filepath.Join("internal", dir), false) {
			ast.Inspect(f, func(n ast.Node) bool {
				call, ok := n.(*ast.CallExpr)
				if !ok {
					return true
				}
				sel, ok := call.Fun.(*ast.SelectorExpr)
				if !ok {
					return true
				}
				if pkg, ok := sel.X.(*ast.Ident); ok && pkg.Name == "operations" {
					return true
				}
				if id, ok := owned[sel.Sel.Name]; ok {
					t.Errorf("%s calls %s directly; use the shared %s operation", path, sel.Sel.Name, id)
				}
				return true
			})
		}
	}
}

// Interfaces do not depend on each other, and shared operations depend on
// neither interface nor on terminal, process or framework effects.
func TestSharedOperationsStayIndependentOfInterfaces(t *testing.T) {
	forbidden := map[string][]string{
		"cli":        {"/internal/tui"},
		"tui":        {"/internal/cli"},
		"operations": {"/internal/cli", "/internal/tui", "/internal/platform", "github.com/spf13/cobra", "charm.land/", "os/exec"},
	}
	effects := map[string]bool{"os.Exit": true, "os.Stdout": true, "os.Stderr": true, "os.Stdin": true,
		"fmt.Print": true, "fmt.Printf": true, "fmt.Println": true, "fmt.Scan": true, "fmt.Scanln": true}
	for dir, bans := range forbidden {
		for path, f := range parsed(t, filepath.Join("internal", dir), false) {
			for _, spec := range f.Imports {
				imported, _ := strconv.Unquote(spec.Path.Value)
				for _, ban := range bans {
					if strings.HasSuffix(imported, ban) || strings.HasPrefix(imported, ban) {
						t.Errorf("%s imports %s", path, imported)
					}
				}
			}
			if dir != "operations" {
				continue
			}
			ast.Inspect(f, func(n ast.Node) bool {
				if sel, ok := n.(*ast.SelectorExpr); ok {
					if pkg, ok := sel.X.(*ast.Ident); ok && effects[pkg.Name+"."+sel.Sel.Name] {
						t.Errorf("%s uses %s.%s", path, pkg.Name, sel.Sel.Name)
					}
				}
				return true
			})
		}
	}
}
