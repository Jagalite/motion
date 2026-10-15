package tui

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"strconv"
	"strings"
	"time"
	"unicode/utf8"

	"charm.land/bubbles/v2/table"
	"charm.land/bubbles/v2/textinput"
	tea "charm.land/bubbletea/v2"
	"github.com/Jagalite/motion/apps/terminal/internal/api"
	"github.com/Jagalite/motion/apps/terminal/internal/operations"
)

// Optional extension keeps older/test backends useful; capability-gated pages
// only appear when both the API and server advertise administrator access.
type accessBackend interface {
	operations.UserAPI
	operations.PairingAPI
	operations.InvitationAPI
}

const (
	keyUsers       = "access-users"
	keyPairings    = "pairings"
	keyInvitations = "invitations"
	keySetup       = "access-setup"
	keyApprovals   = "action-approvals"
)

// approvalBackend is optional: it lists and approves other devices'
// requests to make one sensitive change without recent sign-in.
type approvalBackend interface {
	operations.ApprovalAPI
}

// setupBackend is optional: without it the TUI never offers owner setup
// or preselects an approval user.
type setupBackend interface {
	operations.SetupAPI
}

// accessSetup is the server's owner setup state and approval suggestion.
func (m Model) accessSetup() api.AccessSetup {
	v, _ := data[api.AccessSetup](m, keySetup)
	return v
}

// setupRequired reports a server without users, where an administrator or
// the local operator may create the Owner.
func (m Model) setupRequired() bool {
	_, ok := m.backend.(setupBackend)
	return ok && m.admin() && m.accessFeature() && m.accessSetup().OwnerSetup == api.SetupRequired
}

const firstRunHint = "First run: press s to create the Owner, then approve the browser's request with a."

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

// deviceName names a requesting device; its name is untrusted display text.
func (m Model) deviceName(id string) string {
	devices, _ := data[[]api.Device](m, keyDevices)
	for _, d := range devices {
		if d.ID == id {
			return "“" + clean(d.Name) + "” (" + shortID(id) + ")"
		}
	}
	return shortID(id)
}
func unixTime(seconds int64) string {
	return time.Unix(seconds, 0).UTC().Format("2006-01-02 15:04 UTC")
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
	case pageApprovals:
		values, _ := data[[]api.ActionApproval](m, keyApprovals)
		for _, a := range values {
			ids = append(ids, a.ID)
			rows = append(rows, table.Row{clean(a.Action), clean(a.Target), m.deviceName(a.Actor), unixTime(a.ExpiresAt)})
		}
		return project(width, []column{{"Action", 22, false}, {"Target", 28, true}, {"Requested by", 24, false}, {"Expires", 22, true}}, rows, ids)
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
		if m.setupRequired() {
			lines = append([]string{firstRunHint}, lines...)
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
	case pageApprovals:
		lines = []string{"a approve the selected request after comparing its code · r refresh", "Approving lets that device make exactly this change once. Approve only requests you expect."}
	case pageInvitations:
		lines = []string{"n create invitation · d revoke unused invitation · r refresh", "Codes are shown only after issuance. Send them privately.", "Claimed invitations: revoke the device on Devices."}
	}
	if len(lines) == 0 {
		lines = []string{"n new user or invitation · r refresh"}
		if m.setupRequired() {
			lines = []string{firstRunHint, "n creates a user manually instead · r refresh"}
		}
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
	// resubmit marks a dialog whose last submission had an unknown
	// outcome: applying again resubmits the same request and key.
	resubmit bool
}
type accessDone struct {
	epoch      int
	invitation *api.Invitation
	// owner is the name of the owner that setup created or found.
	owner string
	err   error
	// Form results: a pairing awaiting approval, a credential change and
	// the confirmation to show.
	pairing   *operations.PendingPairing
	signedIn  bool
	signedOut bool
	note      string
}

// intent is the dialog's logical mutation: one key for every submission.
func (d *accessDialog) intent(operation string) operations.Intent {
	return operations.Intent{Operation: operation, Key: d.key, Resubmission: d.resubmit}
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
	granted := u.Permissions
	if d.kind != "user" {
		granted = operations.InheritedGrant(u).Permissions
	}
	for _, p := range api.AllPermissions {
		d.fields = append(d.fields, toggle("Permission: "+p, "permission", p, has(granted, p)))
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
		u := api.User{Policy: operations.DefaultGrantPolicy(), Permissions: operations.DefaultUserPermissions}
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
	case "setup":
		if !m.setupRequired() {
			return m.say("Owner setup is available only on a server without users.", true)
		}
		d.title = "Set up Motion's owner"
		d.message = "Creates one enabled Owner whose access includes administration of this server. No device gets access yet: approve each device, including your browser, after comparing its code. Existing unassigned devices stay unassigned."
		d.fields = []accessField{textField("Owner name", "name", "Owner")}
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
			if m.setupRequired() {
				return m.beginAccess("setup")
			}
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
			// The server's suggestion and this list agree on the only
			// enabled user: preselect it. The code and grant are still
			// reviewed and confirmed explicitly.
			if only := m.accessSetup().PairingUser; only != nil && len(d.users) == 1 && d.users[0].ID == only.ID {
				d.user, d.fields = 0, nil
				d.title = "Approve access for “" + clean(d.users[0].Name) + "”"
				d.message += " Preselected: the only enabled user."
				m.grantFields(d, d.users[0])
			}
		}
	case "proof":
		values, _ := data[[]api.ActionApproval](*m, keyApprovals)
		found := false
		for _, a := range values {
			if a.ID == d.target && a.Status == "pending" {
				found = true
				d.title = "Approve “" + clean(a.Action) + "” for " + m.deviceName(a.Actor) + "?"
				d.message = "Target: " + clean(a.Target) + ". Enter the code shown on the requesting device. That device then submits its reviewed change once."
			}
		}
		if !found {
			return m.say("Select a pending request and refresh.", true)
		}
		d.fields = []accessField{textField("Code shown on the requesting device", "code", "")}
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
		if msg.String() == "s" {
			return true, m.beginAccess("setup")
		}
		if msg.String() == "a" {
			return true, m.beginAccess("approve")
		}
		if msg.String() == "d" && m.selectedID() != "" {
			return true, m.beginAccess("deny")
		}
	case pageUsers:
		if msg.String() == "s" {
			return true, m.beginAccess("setup")
		}
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
				m.grantFields(m.access, api.User{Policy: operations.DefaultGrantPolicy(), Permissions: operations.DefaultUserPermissions})
				cmd = m.access.fields[0].focusInput()
			}
			return true, cmd
		}
	case pageApprovals:
		if msg.String() == "a" && m.selectedID() != "" {
			return true, m.beginAccess("proof")
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
		if d.busy && d.kind == formPair && m.pairCancel != nil {
			// Stop waiting; an unclaimed request expires on its own. A
			// credential installed just before is kept and shown.
			installed := m.pairing != nil && m.pairing.Abandon()
			m.pairCancel()
			m.access, m.pairCancel, m.pairing = nil, nil, nil
			m.accessEpoch++
			if installed {
				return tea.Batch(m.resync(), m.credentialChanged(true), m.say("The pairing completed before it was stopped; this device is signed in.", false))
			}
			return m.say("Stopped waiting for approval.", false)
		}
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
		// After an unknown outcome the request is fixed: only the same
		// request may be resubmitted.
		if msg.String() == "n" && !d.resubmit {
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
			if d.kind == formDeviceAssign {
				// The chooser stays: n returns to it, as does a refusal.
				d.review = true
				return nil
			}
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
	if err := operations.CheckGrantPolicy(in.Policy); err != nil {
		return in, 0, err
	}
	return in, ttl, nil
}
func (m *Model) submitAccess() tea.Cmd {
	d := m.access
	if formKind(d.kind) {
		return m.submitForm()
	}
	in, ttl, err := d.input()
	if err != nil {
		d.message = err.Error()
		d.review = false
		return nil
	}
	b, devices := m.backend.(accessBackend), m.backend
	setup, _ := m.backend.(setupBackend)
	approvals, _ := m.backend.(approvalBackend)
	var code string
	for _, f := range d.fields {
		if f.kind == "text" && f.id == "code" {
			code = f.input.Value()
		}
	}
	ctx := m.ctx
	// Capture immutable intent and revisions before launching any asynchronous work.
	kind, id, rev, epoch := d.kind, d.target, d.revision, d.epoch
	var user api.User
	if d.user >= 0 && d.user < len(d.users) {
		user = d.users[d.user]
	}
	d.busy = true
	return func() tea.Msg {
		ctx, cancel := context.WithTimeout(ctx, loadTimeout)
		defer cancel()
		result := accessDone{epoch: epoch}
		switch kind {
		case "proof":
			result.err = operations.ApproveAction(ctx, approvals, id, code)
		case "setup":
			var s api.AccessSetup
			s, result.err = operations.SetupOwner(ctx, setup, in.Name)
			if result.err == nil && s.Owner != nil {
				result.owner = s.Owner.Name
			}
		case "user":
			if id == "" {
				_, result.err = operations.CreateUser(ctx, b, in, d.intent("creation of user "+in.Name))
			} else {
				_, result.err = operations.ReplaceUser(ctx, b, id, in, operations.Reviewed(rev))
			}
		case "approve":
			policy := in.Policy
			_, result.err = operations.ApprovePairing(ctx, b, operations.Approval{PairingID: id, UserCode: rev, UserID: user.ID, UserRevision: user.Revision,
				ProfileIDs: in.ProfileIDs, Permissions: in.Permissions, Policy: &policy}, d.intent("approval of pairing "+id))
		case "deny":
			result.err = operations.DenyPairing(ctx, b, id, d.intent("denial of pairing "+id))
		case "device":
			result.err = operations.RevokeDevice(ctx, devices, id, operations.Reviewed(rev))
		case "invite":
			var invitation api.Tagged[api.Invitation]
			invitation, result.err = operations.CreateInvitation(ctx, b, api.InvitationInput{UserID: user.ID, UserRevision: user.Revision,
				ProfileIDs: in.ProfileIDs, Permissions: in.Permissions, Policy: in.Policy, TTLSeconds: ttl}, d.intent("invitation for user "+user.ID))
			if result.err == nil {
				result.invitation = &invitation.Value
			}
		case "revoke-invite":
			result.err = operations.RevokeInvitation(ctx, b, id, operations.Reviewed(rev))
		}
		return result
	}
}

// Withdraw both cached administrator data and in-flight modal results. This
// deliberately leaves the local viewing outbox untouched.
func (m *Model) forgetAccess() {
	m.access = nil
	m.accessEpoch++
	for _, key := range []string{keyUsers, keyPairings, keyInvitations, keyDevices, keySetup, keyApprovals} {
		m.drop(key)
	}
}
func (m *Model) accessFinished(result accessDone) tea.Cmd {
	if m.access == nil || result.epoch != m.access.epoch || result.epoch != m.accessEpoch {
		return nil
	}
	d := m.access
	d.busy = false
	// A rejected credential ends every form except those that sign in;
	// withdrawn administrator access ends the administrator dialogs.
	if unauthenticated(result.err) && d.kind != formPair && d.kind != formRedeem {
		m.signedOut(result.err)
		return nil
	}
	if revoked(result.err) && (!formKind(d.kind) || d.kind == formDeviceAssign || d.kind == formDevicePolicy) {
		m.forgetAccess()
		return m.load(keySession)
	}
	var unknown *operations.UncertainError
	d.resubmit = d.resubmit || errors.As(result.err, &unknown) && unknown.Key != ""
	if result.err != nil && d.kind == "setup" {
		// Setup is safe to repeat: the server returns the recorded owner.
		d.review = false
		d.message = clean(refusal(result.err)) + " Setup is safe to repeat: review and apply again, or Esc and refresh."
		return nil
	}
	if result.err != nil {
		d.review = d.resubmit
		d.message = clean(refusal(result.err))
		switch {
		case unknown != nil && unknown.Key != "":
			d.message = clean(unknown.Error()) + " Apply again to resubmit the same request."
		case unknown != nil:
			d.message = clean(unknown.Error()) + " Close this and refresh to see the current state before trying again."
		}
		return nil
	}
	if formKind(d.kind) {
		return m.formFinished(result)
	}
	done := "Access updated."
	if d.kind == "proof" {
		done = "Request approved. The requesting device can now submit its change."
	}
	if d.kind == "setup" {
		done = "Owner “" + clean(result.owner) + "” is ready. On Pairing requests select the browser's request and press a: compare its code, review the grant, then confirm."
	}
	if result.invitation != nil {
		d.secret = result.invitation.Code
		d.message = "Expires " + result.invitation.ExpiresAt + ". Share privately; Esc hides this code."
		d.review = false
	} else {
		m.access = nil
	}
	return tea.Batch(m.reloadAll(), m.load(keyDevices), m.load(keyUsers), m.load(keySetup), m.say(done, false))
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
	} else if d.review && formKind(d.kind) {
		lines = append(lines, "", "Confirm? y apply · n edit · Esc cancel")
		if d.resubmit {
			lines[len(lines)-1] = "Resubmit the same request? y apply · Esc cancel"
		}
		lines = append(lines, m.formSummary(d)...)
	} else if d.review && d.kind == "proof" {
		code := ""
		for _, f := range d.fields {
			if f.id == "code" {
				code = f.input.Value()
			}
		}
		lines = append(lines, "", "Approve this request? y apply · n edit · Esc cancel",
			"Code entered: "+clean(code)+" — it must match the requesting device.",
			"Only this action on this target, once, for that device.")
	} else if d.review && d.kind == "setup" {
		in, _, _ := d.input()
		name := strings.TrimSpace(in.Name)
		if name == "" {
			name = "Owner"
		}
		lines = append(lines, "", "Create the Owner? y apply · n edit · Esc cancel",
			"Owner: "+clean(name)+" · enabled · administrator of this server",
			"Devices: none approved or assigned by this step.",
			"Next: compare the browser's code and approve it on Pairing requests.")
	} else if d.review {
		lines = append(lines, "", "Confirm this access change? y apply · n edit · Esc cancel")
		if d.kind == "approve" {
			lines = append(lines, "Compare code: "+clean(d.revision)+" must match the requesting device.")
		}
		if in, _, err := d.input(); err == nil && len(d.fields) > 0 {
			state := "enabled"
			if in.Disabled {
				state = "DISABLED across ALL owned devices"
			}
			lines = append(lines, "User: "+clean(in.Name)+" · "+state, "Permissions: "+strings.Join(in.Permissions, ", "))
			lines = append(lines, m.grantSummary(in.ProfileIDs, in.Policy.LibraryIDs)...)
			if d.kind == "approve" || d.kind == "invite" {
				if has(in.Permissions, api.PermSystemAdmin) {
					lines = append(lines, "Administration: GRANTED. This device can manage users, devices, access and all content.")
				} else {
					lines = append(lines, "Administration: not granted. Select Permission: "+api.PermSystemAdmin+" to grant it deliberately.")
				}
			}
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
				if f.input.EchoMode != textinput.EchoNormal {
					// A secret is never rendered, focused or not.
					value = strings.Repeat("•", min(utf8.RuneCountInString(f.input.Value()), 12))
				}
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
