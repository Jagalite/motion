package tui

import "github.com/Jagalite/motion/apps/terminal/internal/api"

type page int

const (
	pageHome page = iota
	pageLibrary
	pageCatalog
	pageItem
	pageSearch
	pageProcessing
	pageScans
	pageDevices
	pageDiagnostics
	pageActivity
	pageProfiles
	pageServer
)

// route names a page; library and item pages carry the resource ID.
type route struct {
	page page
	id   string
}

// keys are the queries the page renders.
func (r route) keys() []string {
	switch r.page {
	case pageHome:
		return []string{keyLibraries, keyContinue}
	case pageLibrary:
		return []string{"library:" + r.id}
	case pageItem:
		return []string{"item:" + r.id}
	case pageCatalog:
		return []string{keyCatalog}
	case pageSearch:
		return []string{keySearch}
	case pageProcessing:
		return []string{keyJobs}
	case pageScans:
		return []string{keyScans, keyLibraries}
	case pageDevices:
		return []string{keyDevices}
	case pageDiagnostics:
		return []string{keyDiagnostics}
	case pageProfiles:
		return []string{keyProfiles}
	case pageServer:
		return []string{keySession}
	}
	return nil
}

// visit is a history entry: where the user was and what was focused.
type visit struct {
	route  route
	sel    int
	offset int
}

type navEntry struct {
	group string
	label string
	icon  string
	route route
}

// navEntries mirrors the Marquee sidebar: Home, the principal's libraries,
// management screens the principal may use, then profiles and the server.
// Gated entries appear once the principal's permissions are known.
func (m Model) navEntries() []navEntry {
	entries := []navEntry{{label: "Home", icon: "⌂", route: route{page: pageHome}}}
	libraries, _ := data[[]api.Library](m, keyLibraries)
	for _, l := range libraries {
		entries = append(entries, navEntry{group: "Libraries", label: clean(l.Name), icon: libraryIcon(l.Kind), route: route{page: pageLibrary, id: l.ID}})
	}
	entries = append(entries,
		navEntry{group: "Libraries", label: "All titles", icon: "▤", route: route{page: pageCatalog}},
		navEntry{group: "Manage", label: "Processing", icon: "◷", route: route{page: pageProcessing}},
	)
	if me := m.me(); me != nil {
		if me.Allows(api.PermSourcesManage) {
			entries = append(entries, navEntry{group: "Manage", label: "Scans", icon: "↻", route: route{page: pageScans}})
		}
		if me.Allows(api.PermSystemAdmin) {
			entries = append(entries,
				navEntry{group: "Manage", label: "Devices", icon: "▢", route: route{page: pageDevices}},
				navEntry{group: "Manage", label: "Diagnostics", icon: "∿", route: route{page: pageDiagnostics}},
			)
		}
		if me.Allows(api.PermEventsRead) {
			entries = append(entries, navEntry{group: "Manage", label: "Activity", icon: "≡", route: route{page: pageActivity}})
		}
	}
	return append(entries,
		navEntry{group: "foot", label: "Profiles", icon: "◉", route: route{page: pageProfiles}},
		navEntry{group: "foot", label: "Server", icon: "◈", route: route{page: pageServer}},
	)
}

// section is the sidebar entry that owns the route: an item page belongs to
// whichever top-level page it was opened from.
func (m Model) section() route {
	if len(m.history) > 0 && (m.route.page == pageItem) {
		return m.history[0].route
	}
	return m.route
}

func (m Model) title(r route) string {
	switch r.page {
	case pageHome:
		return "Home"
	case pageLibrary:
		if p, ok := data[libraryPage](m, "library:"+r.id); ok {
			return clean(p.library.Name)
		}
		for _, e := range m.navEntries() {
			if e.route == r {
				return e.label
			}
		}
		return "Library"
	case pageItem:
		if p, ok := data[itemPage](m, "item:"+r.id); ok {
			return clean(p.item.Title)
		}
		return "Title"
	case pageCatalog:
		return "All titles"
	case pageSearch:
		return "Search"
	case pageProcessing:
		return "Processing"
	case pageScans:
		return "Scans"
	case pageDevices:
		return "Devices"
	case pageDiagnostics:
		return "Diagnostics"
	case pageActivity:
		return "Activity"
	case pageProfiles:
		return "Profiles"
	case pageServer:
		return "Server"
	}
	return ""
}
