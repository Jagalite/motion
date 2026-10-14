package tui

import "charm.land/bubbles/v2/key"

type keyMap struct {
	Up, Down, Left, Right key.Binding
	PageUp, PageDown      key.Binding
	Top, Bottom           key.Binding
	Open, Back, Focus     key.Binding
	Search, Play, Layout  key.Binding
	Cancel, Choose        key.Binding
	Inspect, Retry        key.Binding
	New, Rename, Delete   key.Binding
	NewLibrary, Policy    key.Binding
	Assign                key.Binding
	Pair, Redeem, SignOut key.Binding
	Refresh, Help, Quit   key.Binding
	Confirm, Keep         key.Binding
	Submit, Leave         key.Binding
	Shorter, Longer       key.Binding
	History, ClearFilter  key.Binding
}

var keys = keyMap{
	Up:          key.NewBinding(key.WithKeys("up", "k"), key.WithHelp("↑/k", "up")),
	Down:        key.NewBinding(key.WithKeys("down", "j"), key.WithHelp("↓/j", "down")),
	Left:        key.NewBinding(key.WithKeys("left", "h"), key.WithHelp("←/h", "left")),
	Right:       key.NewBinding(key.WithKeys("right", "l"), key.WithHelp("→/l", "right")),
	PageUp:      key.NewBinding(key.WithKeys("pgup", "ctrl+u"), key.WithHelp("pgup", "page up")),
	PageDown:    key.NewBinding(key.WithKeys("pgdown", "ctrl+d"), key.WithHelp("pgdn", "page down")),
	Top:         key.NewBinding(key.WithKeys("home", "g"), key.WithHelp("g", "first")),
	Bottom:      key.NewBinding(key.WithKeys("end", "G"), key.WithHelp("G", "last")),
	Open:        key.NewBinding(key.WithKeys("enter"), key.WithHelp("enter", "open")),
	Back:        key.NewBinding(key.WithKeys("esc", "backspace"), key.WithHelp("esc", "back")),
	Focus:       key.NewBinding(key.WithKeys("tab", "shift+tab"), key.WithHelp("tab", "sidebar")),
	Search:      key.NewBinding(key.WithKeys("/"), key.WithHelp("/", "search")),
	Play:        key.NewBinding(key.WithKeys("p"), key.WithHelp("p", "play")),
	Layout:      key.NewBinding(key.WithKeys("v"), key.WithHelp("v", "grid/list")),
	Cancel:      key.NewBinding(key.WithKeys("c"), key.WithHelp("c", "cancel job")),
	Inspect:     key.NewBinding(key.WithKeys("enter"), key.WithHelp("enter", "inspect job")),
	Retry:       key.NewBinding(key.WithKeys("R"), key.WithHelp("R", "retry job")),
	New:         key.NewBinding(key.WithKeys("n"), key.WithHelp("n", "new")),
	Rename:      key.NewBinding(key.WithKeys("e"), key.WithHelp("e", "rename")),
	Delete:      key.NewBinding(key.WithKeys("x"), key.WithHelp("x", "delete")),
	NewLibrary:  key.NewBinding(key.WithKeys("l"), key.WithHelp("l", "new library")),
	Policy:      key.NewBinding(key.WithKeys("p"), key.WithHelp("p", "device policy")),
	Assign:      key.NewBinding(key.WithKeys("u"), key.WithHelp("u", "assign to user")),
	Pair:        key.NewBinding(key.WithKeys("P"), key.WithHelp("P", "pair this device")),
	Redeem:      key.NewBinding(key.WithKeys("I"), key.WithHelp("I", "redeem invitation")),
	SignOut:     key.NewBinding(key.WithKeys("O"), key.WithHelp("O", "sign out")),
	Choose:      key.NewBinding(key.WithKeys("enter"), key.WithHelp("enter", "use profile")),
	Refresh:     key.NewBinding(key.WithKeys("r"), key.WithHelp("r", "refresh")),
	Help:        key.NewBinding(key.WithKeys("?"), key.WithHelp("?", "help")),
	Quit:        key.NewBinding(key.WithKeys("q", "ctrl+c"), key.WithHelp("q", "quit")),
	Confirm:     key.NewBinding(key.WithKeys("y"), key.WithHelp("y", "confirm")),
	Keep:        key.NewBinding(key.WithKeys("n", "esc"), key.WithHelp("n", "keep")),
	Submit:      key.NewBinding(key.WithKeys("enter"), key.WithHelp("enter", "results")),
	Leave:       key.NewBinding(key.WithKeys("esc"), key.WithHelp("esc", "leave search")),
	Shorter:     key.NewBinding(key.WithKeys("["), key.WithHelp("[", "shorter window")),
	Longer:      key.NewBinding(key.WithKeys("]"), key.WithHelp("]", "longer window")),
	History:     key.NewBinding(key.WithKeys("d"), key.WithHelp("d", "device history")),
	ClearFilter: key.NewBinding(key.WithKeys("x"), key.WithHelp("x", "all devices")),
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
		{keys.Shorter, keys.Longer, keys.Play, keys.Layout, keys.Choose, keys.Inspect, keys.Cancel, keys.Retry, keys.History, keys.ClearFilter},
		{keys.New, keys.Rename, keys.Delete, keys.NewLibrary, keys.Policy, keys.Assign, keys.Pair, keys.Redeem, keys.SignOut},
	}
}
