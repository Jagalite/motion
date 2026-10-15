package tui

import "github.com/Jagalite/motion/apps/terminal/internal/api"

type page int

const (
	pageOverview page = iota
	pageHome
	pageLibrary
	pageCatalog
	pageItem
	pageSearch
	pageProcessing
	pageScans
	pageDevices
	pageSources
	pageActivity
	pageProfiles
	pageServer
	pageNowPlaying
	pageHistory
	pagePairings
	pageUsers
	pageInvitations
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
		return []string{keyLibraries, keyContinue, keyCatalog}
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
		return []string{keyScans, keyLibraries, keySources}
	case pageOverview:
		return []string{keyJobs}
	case pageDevices:
		return []string{keyDevices, keyUsers}
	case pagePairings:
		return []string{keyPairings, keyUsers, keyProfiles, keyLibraries, keySetup}
	case pageUsers:
		return []string{keyUsers, keyProfiles, keyLibraries, keyDevices, keySetup}
	case pageInvitations:
		return []string{keyInvitations, keyUsers, keyProfiles, keyLibraries}
	case pageSources:
		return []string{keySources}
	case pageNowPlaying:
		return []string{keyPlayback}
	case pageHistory:
		return []string{keyHistory}
	case pageProfiles:
		return []string{keyProfiles}
	case pageServer:
		return []string{keySession, keyServer}
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

// navEntries is the sidebar: monitoring first, then the libraries, then
// administration. Gated entries appear once the principal's permissions are
// known; the server still decides every request.
func (m Model) navEntries() []navEntry {
	me := m.me()
	if me == nil && m.sessionProblem() == nil {
		me = m.cache.shape // reloading: keep the sidebar steady
	}
	allows := func(p string) bool { return me != nil && me.Allows(p) }
	entries := []navEntry{{group: "Monitor", label: "Overview", icon: "◉", route: route{page: pageOverview}}}
	if allows(api.PermSystemAdmin) && m.playbackFeature() {
		entries = append(entries,
			navEntry{group: "Monitor", label: "Now playing", icon: "▶", route: route{page: pageNowPlaying}},
			navEntry{group: "Monitor", label: "History", icon: "◔", route: route{page: pageHistory}},
		)
	}
	entries = append(entries, navEntry{group: "Monitor", label: "Processing", icon: "◷", route: route{page: pageProcessing}})
	if allows(api.PermSourcesManage) {
		entries = append(entries, navEntry{group: "Monitor", label: "Scans", icon: "↻", route: route{page: pageScans}})
	}
	if allows(api.PermEventsRead) {
		entries = append(entries, navEntry{group: "Monitor", label: "Activity", icon: "≡", route: route{page: pageActivity}})
	}
	entries = append(entries,
		navEntry{group: "Monitor", label: "Server", icon: "◈", route: route{page: pageServer}},
		navEntry{group: "Library", label: "Browse", icon: "⌂", route: route{page: pageHome}},
	)
	libraries, ok := data[[]api.Library](m, keyLibraries)
	if !ok && m.sessionProblem() == nil {
		libraries = m.cache.shelves // reloading: keep the sidebar steady
	}
	for _, l := range libraries {
		entries = append(entries, navEntry{group: "Library", label: clean(l.Name), icon: libraryIcon(l.Kind), route: route{page: pageLibrary, id: l.ID}})
	}
	entries = append(entries, navEntry{group: "Library", label: "All titles", icon: "▤", route: route{page: pageCatalog}})
	if allows(api.PermSourcesManage) {
		entries = append(entries, navEntry{group: "Admin", label: "Sources", icon: "▥", route: route{page: pageSources}})
	}
	if allows(api.PermSystemAdmin) && m.accessFeature() {
		entries = append(entries, navEntry{group: "Admin", label: "Pairing requests", icon: "⇄", route: route{page: pagePairings}}, navEntry{group: "Admin", label: "Users and access", icon: "♙", route: route{page: pageUsers}}, navEntry{group: "Admin", label: "Invitations", icon: "✉", route: route{page: pageInvitations}})
	}
	if allows(api.PermSystemAdmin) {
		entries = append(entries, navEntry{group: "Admin", label: "Devices", icon: "▢", route: route{page: pageDevices}})
	}
	return append(entries, navEntry{group: "Admin", label: "Profiles", icon: "☺", route: route{page: pageProfiles}})
}

// section is the sidebar entry that owns the route: an item page belongs to
// whichever top-level page it was opened from.
func (m Model) section() route {
	if len(m.history) > 0 && m.route.page == pageItem {
		return m.history[0].route
	}
	return m.route
}

func (m Model) title(r route) string {
	switch r.page {
	case pageOverview:
		return "Overview"
	case pageHome:
		return "Browse"
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
	case pagePairings:
		return "Pairing requests"
	case pageUsers:
		return "Users and access"
	case pageInvitations:
		return "Access invitations"
	case pageSources:
		return "Sources"
	case pageActivity:
		return "Activity"
	case pageProfiles:
		return "Profiles"
	case pageServer:
		return "Server"
	case pageNowPlaying:
		return "Now playing"
	case pageHistory:
		return "Playback history"
	}
	return ""
}
