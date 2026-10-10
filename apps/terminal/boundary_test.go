// Package terminal_test proves the client's dependency boundary: it talks to
// Motion only through the public HTTP API.
package terminal_test

import (
	"go/ast"
	"go/parser"
	"go/token"
	"io/fs"
	"os/exec"
	"path/filepath"
	"regexp"
	"strconv"
	"strings"
	"testing"
)

func TestNoDatabaseScannerProviderOrMediaToolDependencies(t *testing.T) {
	out, err := exec.Command("go", "list", "-deps", "./...").Output()
	if err != nil {
		t.Fatal(err)
	}
	// database/sql/driver alone is allowed: lipgloss's color package
	// implements its Scanner/Valuer interfaces without a database.
	forbidden := regexp.MustCompile(`(?i)(^database/sql$|sqlite|ffmpeg|ffprobe|\bav(codec|format)\b|tmdb|tvdb|fsnotify)`)
	for _, pkg := range strings.Fields(string(out)) {
		if forbidden.MatchString(pkg) {
			t.Errorf("forbidden dependency %s", pkg)
		}
	}
}

// Process launches are limited to the bundled Rust server and the platform
// browser opener. Inspects every non-test source file's syntax tree.
func TestOnlyServerDelegationAndBrowserOpenerStartProcesses(t *testing.T) {
	launch := map[string]bool{
		"os/exec.Command": true, "os/exec.CommandContext": true,
		"os.StartProcess": true, "syscall.Exec": true, "syscall.ForkExec": true,
	}
	mediaLiteral := regexp.MustCompile(`(?i)(ffmpeg|ffprobe|\.sqlite)`)
	fset := token.NewFileSet()
	err := filepath.WalkDir(".", func(path string, d fs.DirEntry, err error) error {
		if err != nil || d.IsDir() || !strings.HasSuffix(path, ".go") || strings.HasSuffix(path, "_test.go") {
			return err
		}
		file, err := parser.ParseFile(fset, path, nil, 0)
		if err != nil {
			return err
		}
		imports := map[string]string{}
		for _, spec := range file.Imports {
			p, _ := strconv.Unquote(spec.Path.Value)
			name := p[strings.LastIndex(p, "/")+1:]
			if spec.Name != nil {
				name = spec.Name.Name
			}
			imports[name] = p
		}
		for _, decl := range file.Decls {
			fn, _ := decl.(*ast.FuncDecl)
			ast.Inspect(decl, func(n ast.Node) bool {
				switch n := n.(type) {
				case *ast.SelectorExpr:
					if id, ok := n.X.(*ast.Ident); ok && launch[imports[id.Name]+"."+n.Sel.Name] {
						serve := fn != nil && fn.Name.Name == "serveCmd" && path == filepath.Join("internal", "cli", "root.go")
						browser := fn != nil && fn.Name.Name == "OpenBrowser" && path == filepath.Join("internal", "cli", "play.go")
						if !serve && !browser {
							t.Errorf("%s: process launch outside server/browser delegation", fset.Position(n.Pos()))
						}
					}
				case *ast.CallExpr:
					if fn != nil && fn.Name.Name == "OpenBrowser" {
						if selector, ok := n.Fun.(*ast.SelectorExpr); ok && selector.Sel.Name == "CommandContext" {
							if len(n.Args) < 2 {
								t.Error("browser command has no executable")
								break
							}
							literal, ok := n.Args[1].(*ast.BasicLit)
							if !ok {
								t.Error("browser executable is not fixed")
								break
							}
							executable, _ := strconv.Unquote(literal.Value)
							if executable != "open" && executable != "xdg-open" && executable != "rundll32" {
								t.Errorf("unexpected browser executable %s", executable)
							}
						}
					}
				case *ast.BasicLit:
					if n.Kind == token.STRING && mediaLiteral.MatchString(n.Value) {
						t.Errorf("%s: references a media tool or database file", fset.Position(n.Pos()))
					}
				}
				return true
			})
		}
		return nil
	})
	if err != nil {
		t.Fatal(err)
	}
}
