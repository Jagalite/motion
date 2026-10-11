package tui

import "github.com/charmbracelet/bubbles/key"

type keyMap struct {
	Up, Down, Left, Right key.Binding
	PageUp, PageDown      key.Binding
	Top, Bottom           key.Binding
	Open, Back, Focus     key.Binding
	Search, Play, Layout  key.Binding
	Cancel, Choose        key.Binding
	Refresh, Help, Quit   key.Binding
	Confirm, Keep         key.Binding
	Submit, Leave         key.Binding
}

var keys = keyMap{
	Up:       key.NewBinding(key.WithKeys("up", "k"), key.WithHelp("↑/k", "up")),
	Down:     key.NewBinding(key.WithKeys("down", "j"), key.WithHelp("↓/j", "down")),
	Left:     key.NewBinding(key.WithKeys("left", "h"), key.WithHelp("←/h", "left")),
	Right:    key.NewBinding(key.WithKeys("right", "l"), key.WithHelp("→/l", "right")),
	PageUp:   key.NewBinding(key.WithKeys("pgup", "ctrl+u"), key.WithHelp("pgup", "page up")),
	PageDown: key.NewBinding(key.WithKeys("pgdown", "ctrl+d"), key.WithHelp("pgdn", "page down")),
	Top:      key.NewBinding(key.WithKeys("home", "g"), key.WithHelp("g", "first")),
	Bottom:   key.NewBinding(key.WithKeys("end", "G"), key.WithHelp("G", "last")),
	Open:     key.NewBinding(key.WithKeys("enter"), key.WithHelp("enter", "open")),
	Back:     key.NewBinding(key.WithKeys("esc", "backspace"), key.WithHelp("esc", "back")),
	Focus:    key.NewBinding(key.WithKeys("tab", "shift+tab"), key.WithHelp("tab", "sidebar")),
	Search:   key.NewBinding(key.WithKeys("/"), key.WithHelp("/", "search")),
	Play:     key.NewBinding(key.WithKeys("p"), key.WithHelp("p", "play")),
	Layout:   key.NewBinding(key.WithKeys("v"), key.WithHelp("v", "grid/list")),
	Cancel:   key.NewBinding(key.WithKeys("c"), key.WithHelp("c", "cancel job")),
	Choose:   key.NewBinding(key.WithKeys("enter"), key.WithHelp("enter", "use profile")),
	Refresh:  key.NewBinding(key.WithKeys("r"), key.WithHelp("r", "refresh")),
	Help:     key.NewBinding(key.WithKeys("?"), key.WithHelp("?", "help")),
	Quit:     key.NewBinding(key.WithKeys("q", "ctrl+c"), key.WithHelp("q", "quit")),
	Confirm:  key.NewBinding(key.WithKeys("y"), key.WithHelp("y", "confirm")),
	Keep:     key.NewBinding(key.WithKeys("n", "esc"), key.WithHelp("n", "keep")),
	Submit:   key.NewBinding(key.WithKeys("enter"), key.WithHelp("enter", "results")),
	Leave:    key.NewBinding(key.WithKeys("esc"), key.WithHelp("esc", "leave search")),
}

// helpKeys adapts a context's bindings to the help bubble.
type helpKeys struct {
	short []key.Binding
	full  [][]key.Binding
}

func (h helpKeys) ShortHelp() []key.Binding  { return h.short }
func (h helpKeys) FullHelp() [][]key.Binding { return h.full }

func move(label string) key.Binding {
	return key.NewBinding(key.WithKeys("up", "down", "left", "right"), key.WithHelp("←↑↓→", label))
}

// fullHelp is the help page: every shortcut, grouped.
func fullHelp() [][]key.Binding {
	return [][]key.Binding{
		{keys.Up, keys.Down, keys.Left, keys.Right, keys.PageUp, keys.PageDown, keys.Top, keys.Bottom},
		{keys.Open, keys.Back, keys.Focus, keys.Search, keys.Refresh, keys.Help, keys.Quit},
		{keys.Play, keys.Layout, keys.Choose, keys.Cancel},
	}
}
