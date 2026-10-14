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
	"slices"
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
	Note         string              `json:"note"`
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

// commands maps every command path to whether it runs (a group may too).
func commands(root *cobra.Command) map[string]bool {
	out := map[string]bool{}
	var walk func(*cobra.Command, string)
	walk = func(c *cobra.Command, prefix string) {
		for _, child := range c.Commands() {
			path := strings.TrimSpace(prefix + " " + child.Name())
			out[path] = child.Runnable()
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
			if _, ok := cmds[*e.CLI]; !ok {
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
	// Every runnable command is inventoried, including groups that also
	// run; the TUI launcher and Cobra's own commands are not operations.
	for path, runnable := range cmds {
		if runnable && !covered[path] && path != "tui" && path != "help" && !strings.HasPrefix(path, "completion") {
			t.Errorf("command %q is not in operations.json", path)
		}
	}
}

// cobraCommand is a &cobra.Command{...} literal in internal/cli.
type cobraCommand struct {
	leaf string
	fn   *ast.FuncDecl
	run  []ast.Node // its RunE: the field value and any later assignments
}

// cliCommands finds the command literals of internal/cli and what runs them.
func cliCommands(t *testing.T) []cobraCommand {
	t.Helper()
	var out []cobraCommand
	for _, f := range parsed(t, filepath.Join("internal", "cli"), false) {
		for _, d := range f.Decls {
			fn, ok := d.(*ast.FuncDecl)
			if !ok || fn.Body == nil {
				continue
			}
			vars := map[string]*cobraCommand{}
			var found []*cobraCommand
			ast.Inspect(fn.Body, func(n ast.Node) bool {
				lit, ok := n.(*ast.CompositeLit)
				if !ok {
					return true
				}
				sel, ok := lit.Type.(*ast.SelectorExpr)
				if !ok || sel.Sel.Name != "Command" {
					return true
				}
				c := &cobraCommand{fn: fn}
				for _, el := range lit.Elts {
					kv, ok := el.(*ast.KeyValueExpr)
					if !ok {
						continue
					}
					switch kv.Key.(*ast.Ident).Name {
					case "Use":
						if b, ok := kv.Value.(*ast.BasicLit); ok {
							use, _ := strconv.Unquote(b.Value)
							c.leaf = strings.Fields(use)[0]
						}
					case "RunE", "Run":
						c.run = append(c.run, kv.Value)
					}
				}
				found = append(found, c)
				return true
			})
			// Bind "x := &cobra.Command{...}" names, then "x.RunE = ...".
			ast.Inspect(fn.Body, func(n ast.Node) bool {
				as, ok := n.(*ast.AssignStmt)
				if !ok || len(as.Lhs) != 1 || len(as.Rhs) != 1 {
					return true
				}
				if id, ok := as.Lhs[0].(*ast.Ident); ok {
					if u, ok := as.Rhs[0].(*ast.UnaryExpr); ok {
						if lit, ok := u.X.(*ast.CompositeLit); ok {
							for _, c := range found {
								for _, el := range lit.Elts {
									if kv, ok := el.(*ast.KeyValueExpr); ok && kv.Key.(*ast.Ident).Name == "Use" {
										if b, ok := kv.Value.(*ast.BasicLit); ok {
											use, _ := strconv.Unquote(b.Value)
											if strings.Fields(use)[0] == c.leaf {
												vars[id.Name] = c
											}
										}
									}
								}
							}
						}
					}
				}
				if sel, ok := as.Lhs[0].(*ast.SelectorExpr); ok && (sel.Sel.Name == "RunE" || sel.Sel.Name == "Run") {
					if id, ok := sel.X.(*ast.Ident); ok && vars[id.Name] != nil {
						vars[id.Name].run = append(vars[id.Name].run, as.Rhs[0])
					}
				}
				return true
			})
			for _, c := range found {
				out = append(out, *c)
			}
		}
	}
	return out
}

// runner finds the command literal for a CLI path: a literal for the leaf
// in a function that also declares the parent, or that the parent's
// function calls.
func runner(commands []cobraCommand, path string) *cobraCommand {
	segs := strings.Fields(path)
	leaf := segs[len(segs)-1]
	declares := func(fn *ast.FuncDecl, use string) bool {
		for _, c := range commands {
			if c.fn == fn && c.leaf == use {
				return true
			}
		}
		return false
	}
	calls := func(caller *ast.FuncDecl, callee string) bool {
		found := false
		ast.Inspect(caller.Body, func(n ast.Node) bool {
			if call, ok := n.(*ast.CallExpr); ok {
				if id, ok := call.Fun.(*ast.Ident); ok && id.Name == callee {
					found = true
				}
			}
			return !found
		})
		return found
	}
	for i := range commands {
		c := &commands[i]
		if c.leaf != leaf {
			continue
		}
		if len(segs) == 1 {
			return c
		}
		parent := segs[len(segs)-2]
		if c.leaf != parent && declares(c.fn, parent) {
			return c
		}
		for _, p := range commands {
			if p.leaf == parent && calls(p.fn, c.fn.Name.Name) {
				return c
			}
		}
	}
	return nil
}

// TestCLIEntryPointsRunTheirSharedOperation ties each ledger CLI entry
// point to its command: what that command runs must call one of the
// entry's shared operations. The session entry is exempt: every command
// connects through Env.Connect, which calls operations.Connect.
func TestCLIEntryPointsRunTheirSharedOperation(t *testing.T) {
	commands := cliCommands(t)
	for _, e := range inventory(t) {
		if e.Status != "shared" || e.CLI == nil || e.ID == "session.connect" {
			continue
		}
		c := runner(commands, *e.CLI)
		if c == nil || len(c.run) == 0 {
			t.Errorf("%s: no command literal runs %q", e.ID, *e.CLI)
			continue
		}
		wired := false
		for _, node := range c.run {
			ast.Inspect(node, func(n ast.Node) bool {
				if sel, ok := n.(*ast.SelectorExpr); ok {
					if pkg, ok := sel.X.(*ast.Ident); ok && pkg.Name == "operations" && slices.Contains(e.Shared, sel.Sel.Name) {
						wired = true
					}
				}
				return !wired
			})
		}
		if !wired {
			t.Errorf("%s: `motion %s` does not run any of %v", e.ID, *e.CLI, e.Shared)
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

// rawHTTP are the net/http functions and *http.Client methods that build or
// send requests; interface code reaches the server only through operations.
var rawHTTP = map[string]bool{"NewRequest": true, "NewRequestWithContext": true, "Do": true, "Get": true,
	"Post": true, "PostForm": true, "Head": true, "DefaultClient": true, "RoundTrip": true}

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
			if obj.Pkg() != nil && obj.Pkg().Path() == "net/http" && rawHTTP[obj.Name()] {
				t.Errorf("%s: sends HTTP directly (%s); call the shared operation", fset.Position(sel.Pos()), obj.Name())
			}
		}
		for _, f := range files {
			for id, obj := range info.Uses {
				if obj.Pkg() != nil && strings.HasSuffix(obj.Pkg().Path(), "/internal/api") && obj.Name() == "New" && fset.File(id.Pos()) == fset.File(f.Pos()) {
					t.Errorf("%s: constructs an API client", fset.Position(id.Pos()))
				}
				if obj.Pkg() != nil && obj.Pkg().Path() == "net/http" && rawHTTP[obj.Name()] && fset.File(id.Pos()) == fset.File(f.Pos()) {
					t.Errorf("%s: builds or sends an HTTP request (%s)", fset.Position(id.Pos()), obj.Name())
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
