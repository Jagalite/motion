package terminal_test

import (
	"encoding/json"
	"fmt"
	"go/ast"
	"go/importer"
	"go/parser"
	"go/token"
	"go/types"
	"io"
	"os"
	"os/exec"
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

// Every inventoried operation is shared, with tests at each layer, or a
// recorded permanent interface-specific exception. Pending migration is no
// longer accepted: a new workflow ships through internal/operations with
// both entry points.
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
		case "interface_specific":
			// A deliberate, permanent exception: one interface only, no
			// shared operation, and a recorded reason.
			if e.Exception == nil || !strings.HasPrefix(e.Exception.Disposition, "permanent") || (e.CLI == nil) == (e.TUI == nil) {
				t.Errorf("%s: an interface-specific entry needs one entry point and a permanent exception", e.ID)
			}
			if len(e.Shared) != 0 || len(e.API) != 0 {
				t.Errorf("%s: an interface-specific entry claims shared code", e.ID)
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

// operationRefs lists the internal/operations names a package's non-test
// code refers to.
func operationRefs(t *testing.T, dir string) map[string]bool {
	t.Helper()
	refs := map[string]bool{}
	for _, f := range parsed(t, dir, false) {
		ast.Inspect(f, func(n ast.Node) bool {
			if sel, ok := n.(*ast.SelectorExpr); ok {
				if pkg, ok := sel.X.(*ast.Ident); ok && pkg.Name == "operations" {
					refs[sel.Sel.Name] = true
				}
			}
			return true
		})
	}
	return refs
}

// clientMethods are the exported methods of *api.Client: the transport.
func clientMethods(t *testing.T) map[string]bool {
	t.Helper()
	methods := map[string]bool{}
	for _, f := range parsed(t, filepath.Join("internal", "api"), false) {
		for _, d := range f.Decls {
			fn, ok := d.(*ast.FuncDecl)
			if !ok || fn.Recv == nil || !fn.Name.IsExported() {
				continue
			}
			if star, ok := fn.Recv.List[0].Type.(*ast.StarExpr); ok {
				if id, ok := star.X.(*ast.Ident); ok && id.Name == "Client" {
					methods[fn.Name.Name] = true
				}
			}
		}
	}
	return methods
}

// typed type-checks a package's non-test files against the compiler's
// export data for its dependencies.
func typed(t *testing.T, dir string) (*types.Info, map[string]*ast.File, *token.FileSet) {
	t.Helper()
	out, err := exec.Command("go", "list", "-export", "-deps", "-f", "{{.ImportPath}}={{.Export}}", "./"+dir).Output()
	if err != nil {
		t.Fatal(err)
	}
	exports := map[string]string{}
	for _, line := range strings.Split(strings.TrimSpace(string(out)), "\n") {
		if path, file, ok := strings.Cut(line, "="); ok && file != "" {
			exports[path] = file
		}
	}
	fset := token.NewFileSet()
	files := map[string]*ast.File{}
	var list []*ast.File
	paths, _ := filepath.Glob(filepath.Join(dir, "*.go"))
	for _, path := range paths {
		if strings.HasSuffix(path, "_test.go") {
			continue
		}
		f, err := parser.ParseFile(fset, path, nil, 0)
		if err != nil {
			t.Fatal(err)
		}
		files[path] = f
		list = append(list, f)
	}
	info := &types.Info{Selections: map[*ast.SelectorExpr]*types.Selection{}, Uses: map[*ast.Ident]types.Object{}}
	conf := types.Config{Importer: importer.ForCompiler(fset, "gc", func(path string) (io.ReadCloser, error) {
		file, ok := exports[path]
		if !ok {
			return nil, fmt.Errorf("no export data for %s", path)
		}
		return os.Open(file)
	})}
	if _, err := conf.Check("github.com/Jagalite/motion/apps/terminal/"+filepath.ToSlash(dir), fset, list, info); err != nil {
		t.Fatal(err)
	}
	return info, files, fset
}

// CLI and TUI code reaches the server only through internal/operations: it
// never calls a method of the API client, whether on the client itself, on
// a session embedding it, or on an interface (such as the TUI's Backend)
// that carries client methods. Shared data types remain usable. Each
// interface also refers to the implementation of every shared operation.
func TestInterfacesDoNotBypassSharedOperations(t *testing.T) {
	transport := clientMethods(t)
	for _, dir := range []string{filepath.Join("internal", "cli"), filepath.Join("internal", "tui")} {
		info, files, fset := typed(t, dir)
		for sel, selection := range info.Selections {
			if selection.Kind() == types.FieldVal {
				continue
			}
			obj := selection.Obj()
			apiMethod := obj.Pkg() != nil && strings.HasSuffix(obj.Pkg().Path(), "/internal/api")
			_, viaInterface := selection.Recv().Underlying().(*types.Interface)
			if p, ok := selection.Recv().(*types.Pointer); ok {
				_, viaInterface = p.Elem().Underlying().(*types.Interface)
			}
			if apiMethod && transport[obj.Name()] || viaInterface && transport[obj.Name()] {
				t.Errorf("%s: %s reaches the API client directly; call the shared operation", fset.Position(sel.Pos()), obj.Name())
			}
		}
		for _, f := range files {
			for id, obj := range info.Uses {
				if obj.Pkg() != nil && strings.HasSuffix(obj.Pkg().Path(), "/internal/api") && obj.Name() == "New" && fset.File(id.Pos()) == fset.File(f.Pos()) {
					t.Errorf("%s: constructs an API client", fset.Position(id.Pos()))
				}
			}
			ast.Inspect(f, func(n ast.Node) bool {
				switch n := n.(type) {
				case *ast.BasicLit:
					// Player URLs are built only by the shared launch.
					if n.Kind == token.STRING && strings.Contains(n.Value, "/play/") {
						t.Errorf("%s: builds a player URL; use operations.PlayerURL", fset.Position(n.Pos()))
					}
				case *ast.Ident:
					// The browser opener is injected by the composition root.
					if n.Name == "OpenBrowser" {
						t.Errorf("%s: refers to the platform browser opener", fset.Position(n.Pos()))
					}
				}
				return true
			})
		}
	}
	refs := map[string]map[string]bool{
		"cli": operationRefs(t, filepath.Join("internal", "cli")),
		"tui": operationRefs(t, filepath.Join("internal", "tui")),
	}
	for _, e := range inventory(t) {
		if e.Status != "shared" {
			continue
		}
		for iface, used := range refs {
			if (iface == "cli" && e.CLI == nil) || (iface == "tui" && e.TUI == nil) {
				continue
			}
			wired := false
			for _, name := range e.Shared {
				wired = wired || used[name]
			}
			if !wired {
				t.Errorf("%s: internal/%s refers to none of %v", e.ID, iface, e.Shared)
			}
		}
		for _, m := range e.API {
			if !transport[m] {
				t.Errorf("%s: %s is not an API client method", e.ID, m)
			}
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
