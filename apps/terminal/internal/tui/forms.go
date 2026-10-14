package tui

import (
	"context"
	"os"
	"strings"
	"time"

	"charm.land/bubbles/v2/textinput"
	tea "charm.land/bubbletea/v2"
	"github.com/Jagalite/motion/apps/terminal/internal/api"
	"github.com/Jagalite/motion/apps/terminal/internal/operations"
	"github.com/Jagalite/motion/apps/terminal/internal/store"
)

// Forms reuse the access dialog: fields, review, then one shared operation.
const (
	formProfileNew    = "profile-new"
	formProfileRename = "profile-rename"
	formProfileDelete = "profile-delete"
	formSourceNew     = "source-new"
	formLibraryNew    = "library-new"
	formScan          = "scan"
	formDevicePolicy  = "device-policy"
	formPair          = "pair"
	formRedeem        = "redeem"
	formSignOut       = "sign-out"
	formDeviceAssign  = "device-assign"
)

func formKind(kind string) bool {
	switch kind {
	case formProfileNew, formProfileRename, formProfileDelete, formSourceNew, formLibraryNew,
		formScan, formDevicePolicy, formPair, formRedeem, formSignOut, formDeviceAssign:
		return true
	}
	return false
}

// pairWait is sent when a pairing request has been shown and the interface
// should wait for its approval.
type pairWait struct {
	epoch   int
	pending *operations.PendingPairing
}

// policyLoaded answers the read that opens the device policy form.
type policyLoaded struct {
	epoch  int
	req    int
	device string
	policy api.Tagged[api.AccessPolicy]
	err    error
}

func (m *Model) openForm(kind, target, revision, title string) *accessDialog {
	m.accessEpoch++
	d := &accessDialog{kind: kind, target: target, revision: revision, title: title, key: api.NewIdempotencyKey(), epoch: m.accessEpoch, user: -1}
	m.access = d
	return d
}

func (d *accessDialog) focusFirst() tea.Cmd {
	if len(d.fields) == 0 {
		d.review = true
		return nil
	}
	return d.fields[0].focusInput()
}

func (m Model) allows(permission string) bool {
	me := m.me()
	return me != nil && me.Allows(permission)
}

// formAction opens a form for a key on a table page. The permission checks
// only guide; the server decides every request.
func (m *Model) formAction(msg tea.KeyPressMsg) (bool, tea.Cmd) {
	id := m.selectedID()
	switch {
	case m.route.page == pageProfiles && msg.String() == "n" && m.allows(api.PermProfilesManage):
		d := m.openForm(formProfileNew, "", "", "Create viewing profile")
		d.fields = []accessField{textField("Name", "name", "")}
		return true, d.focusFirst()
	case m.route.page == pageProfiles && msg.String() == "e" && id != "" && m.allows(api.PermProfilesManage):
		p, ok := m.profileRow(id)
		if !ok {
			return true, nil
		}
		d := m.openForm(formProfileRename, p.ID, p.Revision, "Rename “"+clean(p.Name)+"”")
		d.fields = []accessField{textField("Name", "name", p.Name)}
		return true, d.focusFirst()
	case m.route.page == pageProfiles && msg.String() == "x" && id != "" && m.allows(api.PermProfilesManage):
		p, ok := m.profileRow(id)
		if !ok {
			return true, nil
		}
		d := m.openForm(formProfileDelete, p.ID, p.Revision, "Delete profile “"+clean(p.Name)+"”?")
		d.message = "Its continue-watching and viewing history are removed. Media is untouched."
		return true, d.focusFirst()
	case m.route.page == pageSources && msg.String() == "n" && m.allows(api.PermSourcesManage):
		d := m.openForm(formSourceNew, "", "", "Register a storage source")
		d.message = "The path is on the server host; the server checks it."
		d.fields = []accessField{textField("Name", "name", ""), textField("Root path", "root", ""), textField("Exclusions (JSON list)", "exclusions", "[]")}
		return true, d.focusFirst()
	case m.route.page == pageSources && msg.String() == "l" && m.allows(api.PermSourcesManage):
		d := m.openForm(formLibraryNew, "", "", "Create a library")
		d.fields = []accessField{textField("Name", "name", ""), textField("Kind ("+strings.Join(operations.LibraryKinds, ", ")+")", "kind", "mixed"), textField("Metadata language", "language", "")}
		sources, _ := data[[]api.Source](*m, keySources)
		for _, s := range sources {
			d.fields = append(d.fields, toggle("Source: "+clean(s.Name), "source", s.ID, false))
		}
		return true, d.focusFirst()
	case m.route.page == pageScans && msg.String() == "n" && m.allows(api.PermSourcesManage) && !m.ready(keySources, keyLibraries):
		return true, tea.Batch(m.ensure(keySources, keyLibraries), m.say("Loading libraries and sources; press n again in a moment.", false))
	case m.route.page == pageScans && msg.String() == "n" && m.allows(api.PermSourcesManage):
		d := m.openForm(formScan, "", "", "Request a scan")
		d.message = "Choose one library. With no source chosen, every source of the library is observed."
		libraries, _ := data[[]api.Library](*m, keyLibraries)
		for _, l := range libraries {
			d.fields = append(d.fields, toggle("Library: "+clean(l.Name), "library", l.ID, len(libraries) == 1))
		}
		d.fields = append(d.fields, toggle("Verify file content", "verify", "", false), toggle("Require complete coverage", "complete", "", false))
		sources, _ := data[[]api.Source](*m, keySources)
		for _, s := range sources {
			d.fields = append(d.fields, toggle("Only source: "+clean(s.Name), "source", s.ID, false))
		}
		if len(libraries) == 0 {
			m.access = nil
			return true, m.say("Create a library first.", true)
		}
		return true, d.focusFirst()
	case m.route.page == pageDevices && msg.String() == "u" && id != "" && m.admin() && m.accessFeature():
		var device api.Device
		devices, _ := data[[]api.Device](*m, keyDevices)
		for _, d := range devices {
			if d.ID == id {
				device = d
			}
		}
		switch {
		case device.ID == "":
			return true, nil
		case device.UserID != nil:
			return true, m.say("This device already belongs to "+m.userName(*device.UserID)+"; ownership is not transferred.", true)
		case device.Revoked:
			return true, m.say("A revoked device cannot be assigned.", true)
		}
		d := m.openForm(formDeviceAssign, device.ID, device.Revision, "Assign “"+clean(device.Name)+"” to a user")
		d.message = "Its grant narrows to the user's. Choose the person this device belongs to."
		users, _ := data[[]api.User](*m, keyUsers)
		for _, u := range users {
			if !u.Disabled {
				d.users = append(d.users, u)
				d.fields = append(d.fields, accessField{label: clean(u.Name) + " · " + shortID(u.ID), kind: "user", id: u.ID})
			}
		}
		if len(d.users) == 0 {
			m.access = nil
			return true, m.say("Create an enabled user on Users and access first.", true)
		}
		return true, d.focusFirst()
	case m.route.page == pageDevices && msg.String() == "p" && id != "" && m.admin():
		// Each read supersedes the previous one.
		m.policyReq++
		backend, ctx, epoch, req := m.backend, m.ctx, m.accessEpoch, m.policyReq
		return true, func() tea.Msg {
			ctx, cancel := context.WithTimeout(ctx, loadTimeout)
			defer cancel()
			p, err := operations.InspectDevicePolicy(ctx, backend, id)
			return policyLoaded{epoch: epoch, req: req, device: id, policy: p, err: err}
		}
	}
	return false, nil
}

// ready reports that every key has data from a completed load.
func (m Model) ready(keys ...string) bool {
	for _, key := range keys {
		if s := m.cache.slots[key]; s == nil || s.loading || s.data == nil {
			return false
		}
	}
	return true
}

func (m Model) profileRow(id string) (api.Profile, bool) {
	profiles, _ := data[[]api.Profile](m, keyProfiles)
	for _, p := range profiles {
		if p.ID == id {
			return p, true
		}
	}
	return api.Profile{}, false
}

// policyForm opens the device policy editor on the policy just read.
func (m *Model) policyForm(msg policyLoaded) tea.Cmd {
	// Only the newest read opens the editor, and only on the Devices page
	// with nothing else open.
	if msg.epoch != m.accessEpoch || msg.req != m.policyReq || m.route.page != pageDevices || m.access != nil {
		return nil
	}
	if unauthenticated(msg.err) {
		m.signedOut(msg.err)
		return nil
	}
	if msg.err != nil {
		return m.say("Couldn't read the device policy: "+refusal(msg.err), true)
	}
	p := msg.policy.Value
	name := msg.device
	devices, _ := data[[]api.Device](*m, keyDevices)
	for _, d := range devices {
		if d.ID == msg.device {
			name = d.Name
		}
	}
	d := m.openForm(formDevicePolicy, msg.device, p.Revision, "Access policy for “"+clean(name)+"”")
	d.message = "The policy is replaced as a whole, at the revision read now."
	d.fields = []accessField{toggle("Allow unrated content", "unrated", "", p.AllowUnrated), toggle("Deny all rated content", "deny_rated", "", p.DenyRated),
		textField("Allowed ratings (JSON list; [] = any unless denied)", "ratings", accessList(p.AllowedRatings)), textField("Blocked labels (JSON list)", "labels", accessList(p.BlockedLabels))}
	libraries, _ := data[[]api.Library](*m, keyLibraries)
	for _, l := range libraries {
		d.fields = append(d.fields, toggle("Library: "+clean(l.Name), "library", l.ID, has(p.LibraryIDs, l.ID)))
	}
	for _, perm := range api.AllPermissions {
		d.fields = append(d.fields, toggle("Permission: "+perm, "permission", perm, has(p.Permissions, perm)))
	}
	return d.focusFirst()
}

// accountAction handles credential actions: pairing and redeeming while
// signed out, and signing out on the Server page.
func (m *Model) accountAction(msg tea.KeyPressMsg) (bool, tea.Cmd) {
	if m.session == nil {
		return false, nil
	}
	// A credential the server rejected is as good as none.
	rejected := false
	if s := m.cache.slots[keySession]; s != nil && unauthenticated(s.err) {
		rejected = true
	}
	signedIn := m.session.SignedIn() && !rejected
	switch {
	case !signedIn && !m.session.Operator && msg.String() == "P":
		host, _ := os.Hostname()
		d := m.openForm(formPair, "", "", "Pair this device")
		d.message = "An administrator approves the request after comparing its code."
		d.fields = []accessField{textField("Device name", "name", host), toggle("Keep the credential in a private file on this computer (0600)", "consent", "", m.session.Config.CredentialStore == store.FileStore)}
		return true, d.focusFirst()
	case !signedIn && !m.session.Operator && msg.String() == "I":
		d := m.openForm(formRedeem, "", "", "Redeem an invitation")
		d.message = "Paste the one-use code. It is not shown or kept."
		code := textField("Invitation code", "code", "")
		code.input.EchoMode = textinput.EchoPassword
		code.input.CharLimit = 0
		d.fields = []accessField{code, textField("Device name", "name", "Terminal"), toggle("Keep the credential in a private file on this computer (0600)", "consent", "", m.session.Config.CredentialStore == store.FileStore)}
		return true, d.focusFirst()
	case signedIn && !m.session.Operator && m.route.page == pageServer && msg.String() == "O":
		d := m.openForm(formSignOut, "", "", "Sign out of this server?")
		d.message = "The stored credential is forgotten here. An administrator revokes the device to invalidate it."
		return true, d.focusFirst()
	}
	return false, nil
}

func (d *accessDialog) text(id string) string {
	for _, f := range d.fields {
		if f.kind == "text" && f.id == id {
			return f.input.Value()
		}
	}
	return ""
}

func (d *accessDialog) checked(kind string) []string {
	var ids []string
	for _, f := range d.fields {
		if f.kind == kind && f.checked {
			ids = append(ids, f.id)
		}
	}
	return ids
}

func (d *accessDialog) on(kind string) bool { return len(d.checked(kind)) > 0 }

// scanRequest reads the scan form.
func (d *accessDialog) scanRequest() (operations.ScanRequest, error) {
	libraries := d.checked("library")
	if len(libraries) != 1 {
		return operations.ScanRequest{}, operations.Invalid("choose exactly one library")
	}
	in := operations.ScanRequest{LibraryID: libraries[0], Verify: d.on("verify"), RequireComplete: d.on("complete")}
	if sources := d.checked("source"); len(sources) > 0 {
		in.SourceIDs = &sources
	}
	return in, nil
}

// devicePolicy reads the device policy form.
func (d *accessDialog) devicePolicy() (api.AccessPolicy, error) {
	in := api.AccessPolicy{LibraryIDs: d.checked("library"), Permissions: d.checked("permission"), AllowUnrated: d.on("unrated"), DenyRated: d.on("deny_rated")}
	var err error
	if in.AllowedRatings, err = parseAccessList(d.text("ratings")); err != nil {
		return in, err
	}
	in.BlockedLabels, err = parseAccessList(d.text("labels"))
	return in, err
}

func consented(d *accessDialog) string {
	if d.on("consent") {
		return store.FileStore
	}
	return ""
}

// submitForm runs the form's shared operation.
func (m *Model) submitForm() tea.Cmd {
	d := m.access
	b, ctx, s := m.backend, m.ctx, m.session
	kind, id, rev, epoch := d.kind, d.target, d.revision, d.epoch
	var run func(context.Context) accessDone
	switch kind {
	case formProfileNew:
		name, intent := d.text("name"), d.intent("creation of profile "+d.text("name"))
		run = func(ctx context.Context) accessDone {
			_, err := operations.CreateProfile(ctx, b, name, intent)
			return accessDone{err: err, note: "Profile created."}
		}
	case formProfileRename:
		name := d.text("name")
		run = func(ctx context.Context) accessDone {
			_, err := operations.RenameProfile(ctx, b, id, name, operations.Reviewed(rev))
			return accessDone{err: err, note: "Profile renamed."}
		}
	case formProfileDelete:
		run = func(ctx context.Context) accessDone {
			return accessDone{err: operations.DeleteProfile(ctx, b, id, operations.Reviewed(rev)), note: "Profile deleted."}
		}
	case formSourceNew:
		exclusions, err := parseAccessList(d.text("exclusions"))
		if err != nil {
			return m.formRefused(err)
		}
		in, intent := api.SourceInput{Name: d.text("name"), RootPath: d.text("root"), Exclusions: exclusions}, d.intent("registration of source "+d.text("name"))
		run = func(ctx context.Context) accessDone {
			_, err := operations.CreateSource(ctx, b, in, intent)
			return accessDone{err: err, note: "Source registered."}
		}
	case formLibraryNew:
		in, intent := api.LibraryInput{Name: d.text("name"), Kind: strings.TrimSpace(d.text("kind")), Language: d.text("language"), SourceIDs: d.checked("source")}, d.intent("creation of library "+d.text("name"))
		run = func(ctx context.Context) accessDone {
			_, err := operations.CreateLibrary(ctx, b, in, intent)
			return accessDone{err: err, note: "Library created."}
		}
	case formScan:
		in, err := d.scanRequest()
		if err != nil {
			return m.formRefused(err)
		}
		intent := d.intent("scan of library " + in.LibraryID)
		run = func(ctx context.Context) accessDone {
			_, err := operations.RequestScan(ctx, b, in, intent)
			return accessDone{err: err, note: "Scan requested."}
		}
	case formDevicePolicy:
		in, err := d.devicePolicy()
		if err != nil {
			return m.formRefused(err)
		}
		run = func(ctx context.Context) accessDone {
			_, err := operations.ReplaceDevicePolicy(ctx, b, id, in, operations.Reviewed(rev))
			return accessDone{err: err, note: "Device policy replaced."}
		}
	case formPair:
		name, accepted := d.text("name"), consented(d)
		run = func(ctx context.Context) accessDone {
			pending, err := operations.StartPairing(ctx, s, name, accepted)
			return accessDone{err: err, pairing: pending}
		}
	case formRedeem:
		code, name, accepted := d.text("code"), d.text("name"), consented(d)
		run = func(ctx context.Context) accessDone {
			_, err := operations.RedeemInvitation(ctx, s, code, name, accepted)
			return accessDone{err: err, signedIn: err == nil, note: "Invitation redeemed; this device is signed in."}
		}
	case formDeviceAssign:
		if d.user < 0 || d.user >= len(d.users) {
			return m.formRefused(operations.Invalid("choose the user the device belongs to"))
		}
		user := d.users[d.user]
		run = func(ctx context.Context) accessDone {
			_, err := operations.AssignDevice(ctx, b, id, user.ID, user.Revision, operations.Reviewed(rev))
			return accessDone{err: err, note: "Device assigned to " + clean(user.Name) + "."}
		}
	case formSignOut:
		run = func(context.Context) accessDone {
			_, err := operations.SignOut(s)
			return accessDone{err: err, signedOut: err == nil, note: "Signed out. Revoke the device on Devices to invalidate it."}
		}
	default:
		return nil
	}
	d.busy = true
	return func() tea.Msg {
		ctx, cancel := context.WithTimeout(ctx, loadTimeout)
		defer cancel()
		result := run(ctx)
		result.epoch = epoch
		return result
	}
}

func (m *Model) formRefused(err error) tea.Cmd {
	m.access.message, m.access.review = clean(err.Error()), false
	return nil
}

// formFinished applies a successful form result.
func (m *Model) formFinished(result accessDone) tea.Cmd {
	d := m.access
	switch {
	case result.pairing != nil:
		d.busy = true
		d.title = "Waiting for approval"
		d.message = "In an administrator’s Motion TUI or `motion access pairings list`, compare code " + clean(result.pairing.UserCode) +
			". Expires " + clean(result.pairing.ExpiresAt) + ". Esc stops waiting."
		ctx, cancel := context.WithTimeout(m.ctx, 30*time.Minute)
		m.pairCancel, m.pairing = cancel, result.pairing
		pending, epoch := result.pairing, d.epoch
		return func() tea.Msg {
			defer cancel()
			_, err := pending.Wait(ctx)
			return accessDone{epoch: epoch, err: err, signedIn: err == nil, note: "Paired; this device is signed in."}
		}
	case result.signedIn:
		m.access, m.pairCancel, m.pairing = nil, nil, nil
		return tea.Batch(m.resync(), m.credentialChanged(true), m.say(result.note, false))
	case result.signedOut:
		m.access = nil
		m.signedOut(&api.Problem{Status: 401, Code: "not_signed_in", Title: "Unauthorized", Detail: "signed out of this server"})
		return tea.Batch(m.credentialChanged(false), m.say(result.note, false))
	}
	m.access = nil
	return tea.Batch(m.reloadAll(), m.reload(func(key string) bool {
		return key == keyProfiles || key == keySources || key == keyLibraries || key == keyScans || key == keyDevices
	}), m.say(result.note, false))
}

// formSummary is what the review step shows.
func (m Model) formSummary(d *accessDialog) []string {
	switch d.kind {
	case formProfileNew, formProfileRename:
		return []string{"Name: " + clean(d.text("name"))}
	case formSourceNew:
		return []string{"Name: " + clean(d.text("name")), "Root: " + clean(d.text("root")), "Exclusions: " + clean(d.text("exclusions"))}
	case formLibraryNew:
		return []string{"Name: " + clean(d.text("name")) + " · " + clean(d.text("kind")), "Sources: " + clean(strings.Join(d.checked("source"), ", "))}
	case formScan:
		in, err := d.scanRequest()
		if err != nil {
			return []string{err.Error()}
		}
		mode := "incremental"
		if in.Verify {
			mode = "verify file content"
		}
		name := in.LibraryID
		libraries, _ := data[[]api.Library](m, keyLibraries)
		for _, l := range libraries {
			if l.ID == in.LibraryID {
				name = l.Name
			}
		}
		return []string{"Library: " + clean(name) + " · " + mode}
	case formDevicePolicy:
		in, err := d.devicePolicy()
		if err != nil {
			return []string{err.Error()}
		}
		return append(m.grantSummary(nil, in.LibraryIDs)[1:], "Permissions: "+strings.Join(in.Permissions, ", "))
	case formDeviceAssign:
		if d.user >= 0 && d.user < len(d.users) {
			return []string{"User: " + clean(d.users[d.user].Name), "The device's grant narrows to this user's; it cannot be reassigned later."}
		}
		return nil
	case formPair, formRedeem:
		if !d.on("consent") {
			return []string{"The credential needs a store: tick the private file option."}
		}
		return []string{"Device name: " + clean(d.text("name"))}
	}
	return nil
}
