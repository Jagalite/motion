package tui

import (
	"context"
	"encoding/json"
	"fmt"
	"strconv"
	"strings"

	"charm.land/bubbles/v2/table"
	"charm.land/bubbles/v2/textinput"
	tea "charm.land/bubbletea/v2"
	"github.com/Jagalite/motion/apps/terminal/internal/api"
)

// Optional extension keeps older/test backends useful; capability-gated pages
// only appear when both the API and server advertise administrator access.
type accessBackend interface {
	ListUsers(context.Context, string, int) (api.Page[api.User], error)
	GetUser(context.Context, string) (api.Tagged[api.User], error)
	CreateUser(context.Context, api.UserInput, string) (api.Tagged[api.User], error)
	ReplaceUser(context.Context, string, api.UserInput, string) (api.Tagged[api.User], error)
	ListPairings(context.Context, string, int) (api.Page[api.PairingSummary], error)
	ApprovePairing(context.Context, string, api.PairingApproval, string) (api.Tagged[api.Device], error)
	DenyPairing(context.Context, string, string) error
	GetDevice(context.Context, string) (api.Tagged[api.Device], error)
	RevokeDevice(context.Context, string, string) error
	ListInvitations(context.Context, string, int) (api.Page[api.Invitation], error)
	CreateInvitation(context.Context, api.InvitationInput, string) (api.Tagged[api.Invitation], error)
	RevokeInvitation(context.Context, string, string) error
}

const (
	keyUsers       = "access-users"
	keyPairings    = "pairings"
	keyInvitations = "invitations"
)

func (m Model) accessFeature() bool {
	if _, ok := m.backend.(accessBackend); !ok {
		return false
	}
	if c := m.caps(); c != nil {
		f, ok := c.Feature("identity.access_management")
		return ok && f.Enabled
	}
	return false
}
func (m Model) userName(id string) string {
	users, _ := data[[]api.User](m, keyUsers)
	for _, u := range users {
		if u.ID == id {
			return clean(u.Name)
		}
	}
	return "Unassigned"
}
func (m Model) grantSummary(profiles, libraries []string) []string {
	ps, _ := data[[]api.Profile](m, keyProfiles)
	ls, _ := data[[]api.Library](m, keyLibraries)
	pnames, lnames := []string{}, []string{}
	for _, id := range profiles {
		name := id
		for _, p := range ps {
			if p.ID == id {
				name = p.Name
				break
			}
		}
		pnames = append(pnames, clean(name))
	}
	for _, id := range libraries {
		name := id
		for _, l := range ls {
			if l.ID == id {
				name = l.Name
				break
			}
		}
		lnames = append(lnames, clean(name))
	}
	if len(pnames) == 0 {
		pnames = []string{"none"}
	}
	if len(lnames) == 0 {
		lnames = []string{"none"}
	}
	return []string{"Profiles: " + strings.Join(pnames, ", "), "Libraries: " + strings.Join(lnames, ", ")}
}
func (m Model) ownedDevices(id string) []string {
	devices, ok := data[[]api.Device](m, keyDevices)
	if !ok {
		return []string{"Owned devices: loading; refresh before applying a user-wide change."}
	}
	lines := []string{}
	for _, d := range devices {
		if d.UserID == nil || *d.UserID != id {
			continue
		}
		status := "enabled"
		if d.Revoked {
			status = "revoked"
		}
		lines = append(lines, "Device: "+clean(d.Name)+" · "+clean(d.ClientName)+" · "+status)
	}
	if len(lines) == 0 {
		lines = []string{"Owned devices: none"}
	}
	return lines
}
func (m Model) accessRows(width int) ([]table.Column, []table.Row, []string) {
	var rows []table.Row
	var ids []string
	switch m.route.page {
	case pagePairings:
		values, _ := data[[]api.PairingSummary](m, keyPairings)
		for _, p := range values {
			ids = append(ids, p.ID)
			rows = append(rows, table.Row{clean(p.DeviceName), clean(p.ClientName), clean(p.Status), clean(p.UserCode), clean(p.ExpiresAt)})
		}
		return project(width, []column{{"Device", 20, false}, {"Client", 18, true}, {"Status", 12, false}, {"Compare code", 12, false}, {"Expires", 22, true}}, rows, ids)
	case pageUsers:
		values, _ := data[[]api.User](m, keyUsers)
		for _, u := range values {
			state := "enabled"
			if u.Disabled {
				state = "disabled"
			}
			ids = append(ids, u.ID)
			rows = append(rows, table.Row{clean(u.Name), state, fmt.Sprintf("%d profiles · %d libraries", len(u.ProfileIDs), len(u.Policy.LibraryIDs)), clean(strings.Join(u.Permissions, ", "))})
		}
		return project(width, []column{{"User", 24, false}, {"Status", 10, false}, {"Scope", 26, true}, {"Permissions", 32, true}}, rows, ids)
	case pageInvitations:
		values, _ := data[[]api.Invitation](m, keyInvitations)
		for _, i := range values {
			ids = append(ids, i.ID)
			rows = append(rows, table.Row{m.userName(i.UserID), clean(i.Status), clean(i.CreatedAt), clean(i.ExpiresAt)})
		}
		return project(width, []column{{"User", 24, false}, {"Status", 12, false}, {"Created", 22, true}, {"Expires", 22, false}}, rows, ids)
	}
	return nil, nil, nil
}
func (m Model) accessDetail(width int) []string {
	id := m.selectedID()
	var lines []string
	switch m.route.page {
	case pagePairings:
		values, _ := data[[]api.PairingSummary](m, keyPairings)
		for _, p := range values {
			if p.ID == id {
				lines = []string{"Names are untrusted. Compare the code with the requesting device.", "Created: " + clean(p.CreatedAt) + " · Expires: " + clean(p.ExpiresAt), "a approve · d deny · r refresh"}
			}
		}
	case pageUsers:
		values, _ := data[[]api.User](m, keyUsers)
		for _, u := range values {
			if u.ID == id {
				lines = []string{"e edit name, grants and enabled state · n new user", "Ratings: " + clean(strings.Join(u.Policy.AllowedRatings, ", ")) + " · Blocked labels: " + clean(strings.Join(u.Policy.BlockedLabels, ", ")), "User edits narrow existing devices; widening requires a new device approval."}
				if u.Policy.DenyRated {
					lines = append(lines, "All rated content denied; unrated access is configured separately.")
				}
				lines = append(lines, m.grantSummary(u.ProfileIDs, u.Policy.LibraryIDs)...)
				lines = append(lines, "Permissions: "+clean(strings.Join(u.Permissions, ", ")))
				if has(u.Permissions, api.PermSystemAdmin) {
					lines = append(lines, "Administrator: unrestricted access.")
				}
				lines = append(lines, m.ownedDevices(u.ID)...)
			}
		}
	case pageInvitations:
		lines = []string{"n create invitation · d revoke unused invitation · r refresh", "Codes are shown only after issuance. Send them privately.", "Claimed invitations: revoke the device on Devices."}
	}
	if len(lines) == 0 {
		lines = []string{"n new user or invitation · r refresh"}
	}
	return panel(m.title(m.route), "", colorInfo, lines, width)
}

type accessField struct {
	label, kind, id string
	input           textinput.Model
	checked         bool
}
type accessDialog struct {
	kind, target, revision, title, key string
	fields                             []accessField
	focus                              int
	users                              []api.User
	user                               int
	review, busy                       bool
	message, secret                    string
	epoch                              int
}
type accessDone struct {
	epoch      int
	invitation *api.Invitation
	err        error
}

func textField(label, id, value string) accessField {
	in := textinput.New()
	in.Prompt = ""
	in.CharLimit = 400
	if id == "labels" || id == "ratings" {
		in.CharLimit = 0
	}
	in.SetValue(value)
	return accessField{label: label, kind: "text", id: id, input: in}
}
func toggle(label, kind, id string, on bool) accessField {
	return accessField{label: label, kind: kind, id: id, checked: on}
}
func (f *accessField) focusInput() tea.Cmd {
	if f.kind == "text" {
		return f.input.Focus()
	}
	return nil
}
func (f *accessField) blurInput() {
	if f.kind == "text" {
		f.input.Blur()
	}
}
func has(values []string, id string) bool {
	for _, v := range values {
		if v == id {
			return true
		}
	}
	return false
}
func (m *Model) grantFields(d *accessDialog, u api.User) {
	if d.kind == "user" {
		d.fields = append(d.fields, textField("Name", "name", u.Name))
	}
	if d.kind == "user" {
		d.fields = append(d.fields, toggle("Access enabled", "enabled", "", !u.Disabled))
	}
	if d.kind == "invite" {
		d.fields = append(d.fields, textField("Expires in seconds (60–86400)", "ttl", "600"))
	}
	d.fields = append(d.fields, toggle("Allow unrated content", "unrated", "", u.Policy.AllowUnrated), toggle("Deny all rated content", "deny_rated", "", u.Policy.DenyRated), textField("Allowed ratings (JSON list; [] = any unless denied)", "ratings", accessList(u.Policy.AllowedRatings)), textField("Blocked labels (JSON list)", "labels", accessList(u.Policy.BlockedLabels)))
	profiles, _ := data[[]api.Profile](*m, keyProfiles)
	for _, p := range profiles {
		d.fields = append(d.fields, toggle("Profile: "+clean(p.Name), "profile", p.ID, has(u.ProfileIDs, p.ID)))
	}
	libraries, _ := data[[]api.Library](*m, keyLibraries)
	for _, l := range libraries {
		d.fields = append(d.fields, toggle("Library: "+clean(l.Name), "library", l.ID, has(u.Policy.LibraryIDs, l.ID)))
	}
	for _, p := range api.AllPermissions {
		d.fields = append(d.fields, toggle("Permission: "+p, "permission", p, has(u.Permissions, p) && (d.kind == "user" || p != api.PermSystemAdmin)))
	}
}
func (m *Model) beginAccess(kind string) tea.Cmd {
	if !m.admin() || !m.accessFeature() {
		return m.say("Administrator access is unavailable.", true)
	}
	m.accessEpoch++
	d := &accessDialog{kind: kind, target: m.selectedID(), key: api.NewIdempotencyKey(), epoch: m.accessEpoch, user: -1}
	switch kind {
	case "user":
		u := api.User{Policy: api.GrantPolicy{AllowUnrated: true}, Permissions: []string{api.PermCatalogRead, api.PermEventsRead, api.PermPlaybackRequest, api.PermViewingWrite}}
		if d.target != "" {
			values, _ := data[[]api.User](*m, keyUsers)
			for _, v := range values {
				if v.ID == d.target {
					u = v
					d.revision = v.Revision
				}
			}
		}
		d.title = "Create access user"
		if d.revision != "" {
			d.title = "Edit access for " + clean(u.Name)
		} else {
			d.target = ""
		}
		m.grantFields(d, u)
	case "approve", "invite":
		d.title = "Choose the intended user"
		users, _ := data[[]api.User](*m, keyUsers)
		for _, u := range users {
			if !u.Disabled {
				d.users = append(d.users, u)
				d.fields = append(d.fields, accessField{label: clean(u.Name) + " · " + shortID(u.ID), kind: "user", id: u.ID})
			}
		}
		if len(d.users) == 0 {
			return m.say("Create an enabled user on Users and access first.", true)
		}
		if kind == "approve" {
			values, _ := data[[]api.PairingSummary](*m, keyPairings)
			found := false
			for _, p := range values {
				if p.ID == d.target && p.Status == "pending" {
					found = true
					d.message = "Compare " + clean(p.UserCode) + " with the requesting device. Names are untrusted."
					d.revision = p.UserCode
				}
			}
			if !found {
				return m.say("Select a pending request and refresh its status.", true)
			}
		}
	case "deny":
		d.title = "Deny pairing request?"
		d.review = true
	case "device":
		values, _ := data[[]api.Device](*m, keyDevices)
		for _, v := range values {
			if v.ID == d.target {
				d.revision = v.Revision
				d.title = "Revoke device “" + clean(v.Name) + "”?"
			}
		}
		d.message = "Only this device is revoked. Already downloaded bytes cannot be erased."
		d.review = true
	case "revoke-invite":
		values, _ := data[[]api.Invitation](*m, keyInvitations)
		for _, v := range values {
			if v.ID == d.target {
				d.revision = v.Revision
				d.title = "Revoke invitation for “" + m.userName(v.UserID) + "”?"
			}
		}
		d.review = true
	}
	m.access = d
	if len(d.fields) > 0 {
		return d.fields[0].focusInput()
	}
	return nil
}
func (m *Model) accessAction(msg tea.KeyPressMsg) (bool, tea.Cmd) {
	if !m.admin() || !m.accessFeature() {
		return false, nil
	}
	switch m.route.page {
	case pagePairings:
		if msg.String() == "a" {
			return true, m.beginAccess("approve")
		}
		if msg.String() == "d" && m.selectedID() != "" {
			return true, m.beginAccess("deny")
		}
	case pageUsers:
		if msg.String() == "e" && m.selectedID() != "" {
			return true, m.beginAccess("user")
		}
		if msg.String() == "n" {
			cmd := m.beginAccess("user")
			if m.access != nil {
				m.access.target = ""
				m.access.revision = ""
				m.access.title = "Create access user"
				m.access.fields = nil
				m.grantFields(m.access, api.User{Policy: api.GrantPolicy{AllowUnrated: true}, Permissions: []string{api.PermCatalogRead, api.PermPlaybackRequest, api.PermViewingWrite, api.PermEventsRead}})
				cmd = m.access.fields[0].focusInput()
			}
			return true, cmd
		}
	case pageInvitations:
		if msg.String() == "n" {
			return true, m.beginAccess("invite")
		}
		if msg.String() == "d" && m.selectedID() != "" {
			return true, m.beginAccess("revoke-invite")
		}
	case pageDevices:
		if msg.String() == "d" && m.selectedID() != "" {
			return true, m.beginAccess("device")
		}
	}
	return false, nil
}
func (m *Model) accessKey(msg tea.KeyPressMsg) tea.Cmd {
	d := m.access
	if msg.String() == "esc" {
		if d.busy {
			return nil
		}
		m.access = nil
		m.accessEpoch++
		return nil
	}
	if d.busy {
		return nil
	}
	if d.secret != "" {
		return nil
	}
	if d.review {
		if msg.String() == "y" {
			return m.submitAccess()
		}
		if msg.String() == "n" {
			d.review = false
		}
		return nil
	}
	if len(d.fields) == 0 {
		return nil
	}
	if msg.String() == "ctrl+s" {
		if d.user < 0 && (d.kind == "approve" || d.kind == "invite") {
			return nil
		}
		d.review = true
		return nil
	}
	f := &d.fields[d.focus]
	switch msg.String() {
	case "up", "shift+tab":
		f.blurInput()
		d.focus = (d.focus + len(d.fields) - 1) % len(d.fields)
		return d.fields[d.focus].focusInput()
	case "down", "tab":
		f.blurInput()
		d.focus = (d.focus + 1) % len(d.fields)
		return d.fields[d.focus].focusInput()
	case "enter", "space":
		if f.kind == "user" {
			d.user = d.focus
			u := d.users[d.user]
			d.fields = nil
			d.focus = 0
			d.title = "Approve access for “" + clean(u.Name) + "”"
			if d.kind == "invite" {
				d.title = "Create invitation for “" + clean(u.Name) + "”"
			}
			m.grantFields(d, u)
			return d.fields[0].focusInput()
		}
		if f.kind != "text" {
			f.checked = !f.checked
			return nil
		}
	}
	if f.kind != "text" {
		return nil
	}
	var cmd tea.Cmd
	f.input, cmd = f.input.Update(msg)
	return cmd
}
func accessList(values []string) string {
	if values == nil {
		values = []string{}
	}
	encoded, _ := json.Marshal(values)
	return string(encoded)
}
func parseAccessList(value string) ([]string, error) {
	var values []string
	if err := json.Unmarshal([]byte(value), &values); err != nil || values == nil {
		return nil, fmt.Errorf("enter a JSON list of strings, for example [\"PG\",\"PG-13\"]")
	}
	return values, nil
}
func (d *accessDialog) input() (api.UserInput, int64, error) {
	in := api.UserInput{ProfileIDs: []string{}, Permissions: []string{}, Policy: api.GrantPolicy{LibraryIDs: []string{}, AllowedRatings: []string{}, BlockedLabels: []string{}}}
	if d.user >= 0 && d.user < len(d.users) {
		in.Name = d.users[d.user].Name
	}
	ttl := int64(600)
	for _, f := range d.fields {
		switch f.kind {
		case "profile":
			if f.checked {
				in.ProfileIDs = append(in.ProfileIDs, f.id)
			}
		case "library":
			if f.checked {
				in.Policy.LibraryIDs = append(in.Policy.LibraryIDs, f.id)
			}
		case "permission":
			if f.checked {
				in.Permissions = append(in.Permissions, f.id)
			}
		case "enabled":
			in.Disabled = !f.checked
		case "unrated":
			in.Policy.AllowUnrated = f.checked
		case "deny_rated":
			in.Policy.DenyRated = f.checked
		case "text":
			switch f.id {
			case "name":
				in.Name = f.input.Value()
			case "ratings":
				values, err := parseAccessList(f.input.Value())
				if err != nil {
					return in, 0, err
				}
				in.Policy.AllowedRatings = values
			case "labels":
				values, err := parseAccessList(f.input.Value())
				if err != nil {
					return in, 0, err
				}
				in.Policy.BlockedLabels = values
			case "ttl":
				var err error
				ttl, err = strconv.ParseInt(f.input.Value(), 10, 64)
				if err != nil {
					return in, 0, fmt.Errorf("enter an invitation lifetime in seconds")
				}
			}
		}
	}
	if in.Policy.DenyRated && len(in.Policy.AllowedRatings) != 0 {
		return in, 0, fmt.Errorf("clear the rating allowlist when denying all rated content")
	}
	return in, ttl, nil
}
func (m *Model) submitAccess() tea.Cmd {
	d := m.access
	in, ttl, err := d.input()
	if err != nil {
		d.message = err.Error()
		d.review = false
		return nil
	}
	b := m.backend.(accessBackend)
	ctx := m.ctx
	// Capture immutable intent and revisions before launching any asynchronous work.
	kind, id, rev, key, epoch := d.kind, d.target, d.revision, d.key, d.epoch
	var user api.User
	if d.user >= 0 && d.user < len(d.users) {
		user = d.users[d.user]
	}
	d.busy = true
	return func() tea.Msg {
		ctx, cancel := context.WithTimeout(ctx, loadTimeout)
		defer cancel()
		result := accessDone{epoch: epoch}
		stale := func() error { return fmt.Errorf("access changed; close this dialog and refresh before trying again") }
		switch kind {
		case "user":
			if id == "" {
				_, result.err = b.CreateUser(ctx, in, key)
			} else {
				var current api.Tagged[api.User]
				current, result.err = b.GetUser(ctx, id)
				if result.err == nil {
					if current.Value.Revision != rev {
						result.err = stale()
					} else {
						_, result.err = b.ReplaceUser(ctx, id, in, current.ETag)
					}
				}
			}
		case "approve":
			_, result.err = b.ApprovePairing(ctx, id, api.PairingApproval{UserCode: rev, UserID: user.ID, UserRevision: user.Revision, ProfileIDs: in.ProfileIDs, Permissions: in.Permissions, Policy: &in.Policy}, key)
		case "deny":
			result.err = b.DenyPairing(ctx, id, key)
		case "device":
			var current api.Tagged[api.Device]
			current, result.err = b.GetDevice(ctx, id)
			if result.err == nil {
				if current.Value.Revision != rev {
					result.err = stale()
				} else {
					result.err = b.RevokeDevice(ctx, id, current.ETag)
				}
			}
		case "invite":
			var invitation api.Tagged[api.Invitation]
			invitation, result.err = b.CreateInvitation(ctx, api.InvitationInput{UserID: user.ID, UserRevision: user.Revision, ProfileIDs: in.ProfileIDs, Permissions: in.Permissions, Policy: in.Policy, TTLSeconds: ttl}, key)
			if result.err == nil {
				result.invitation = &invitation.Value
			}
		case "revoke-invite":
			result.err = b.RevokeInvitation(ctx, id, `"r-`+rev+`"`)
		}
		return result
	}
}

// Withdraw both cached administrator data and in-flight modal results. This
// deliberately leaves the local viewing outbox untouched.
func (m *Model) forgetAccess() {
	m.access = nil
	m.accessEpoch++
	for _, key := range []string{keyUsers, keyPairings, keyInvitations, keyDevices} {
		m.drop(key)
	}
}
func (m *Model) accessFinished(result accessDone) tea.Cmd {
	if m.access == nil || result.epoch != m.access.epoch || result.epoch != m.accessEpoch {
		return nil
	}
	d := m.access
	d.busy = false
	if unauthenticated(result.err) {
		m.signedOut(result.err)
		return nil
	}
	if revoked(result.err) {
		m.forgetAccess()
		return m.load(keySession)
	}
	if result.err != nil {
		d.message = clean(result.err.Error())
		return nil
	}
	if result.invitation != nil {
		d.secret = result.invitation.Code
		d.message = "Expires " + result.invitation.ExpiresAt + ". Share privately; Esc hides this code."
		d.review = false
	} else {
		m.access = nil
	}
	return tea.Batch(m.reloadAll(), m.load(keyDevices), m.load(keyUsers), m.say("Access updated.", false))
}
func (m Model) accessView() string {
	d := m.access
	width, height := m.dims()
	inner := max(width-6, 10)
	lines := []string{headingStyle.Render(d.title), "", clean(d.message)}
	if d.secret != "" {
		lines = append(lines, "", "Invitation code (join the following lines without spaces):")
		for start := 0; start < len(d.secret); start += inner {
			lines = append(lines, d.secret[start:min(start+inner, len(d.secret))])
		}
		lines = append(lines, "", "The code grants only the selected access, once.")
	} else if d.review {
		lines = append(lines, "", "Confirm this access change? y apply · n edit · Esc cancel")
		if in, _, err := d.input(); err == nil && len(d.fields) > 0 {
			state := "enabled"
			if in.Disabled {
				state = "DISABLED across ALL owned devices"
			}
			lines = append(lines, "User: "+clean(in.Name)+" · "+state, "Permissions: "+strings.Join(in.Permissions, ", "))
			lines = append(lines, m.grantSummary(in.ProfileIDs, in.Policy.LibraryIDs)...)
			if d.kind == "user" && d.target != "" {
				lines = append(lines, m.ownedDevices(d.target)...)
			}
			lines = append(lines, "Revocation cannot erase downloaded bytes. Viewing outboxes are preserved.")
		}
	} else {
		lines = append(lines, "", "↑/↓ or Tab choose · Space toggle · Ctrl+S review · Esc cancel", "")
		start := max(0, d.focus-max(height-10, 1)/2)
		end := min(len(d.fields), start+max(height-10, 1))
		for i := start; i < end; i++ {
			f := d.fields[i]
			prefix := "  "
			if i == d.focus {
				prefix = "› "
			}
			value := "[ ] "
			if f.checked {
				value = "[✓] "
			}
			if f.kind == "text" {
				value = clean(f.input.Value())
				if i == d.focus {
					value = f.input.View()
				}
				lines = append(lines, prefix+clean(f.label)+": "+value)
			} else if f.kind == "user" {
				lines = append(lines, prefix+clean(f.label))
			} else {
				lines = append(lines, prefix+value+clean(f.label))
			}
		}
	}
	if d.busy {
		lines = append(lines, "Applying…")
	}
	for i := range lines {
		lines[i] = truncate(lines[i], inner)
	}
	return "\n  " + strings.Join(lines, "\n  ")
}
