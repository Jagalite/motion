package opstest

import (
	"context"
	"encoding/json"
	"fmt"
	"io"
	"net/http"
	"net/url"
	"slices"
	"sort"
	"strconv"
	"strings"
	"sync"
	"time"

	"github.com/Jagalite/motion/apps/terminal/internal/api"
)

// Server is a contract-shaped stand-in for the management API: profiles,
// devices and their policies, access users, pairings, invitations, sources,
// libraries, scans, catalog reads and monitoring, plus the jobs of an
// embedded Jobs fixture. It serves the same state in process and over
// HTTP, records every request, replays keyed writes by key, enforces strong
// If-Match preconditions and can lose acknowledgements. It is not evidence
// of server behavior.
type Server struct {
	*Jobs

	mu       sync.Mutex
	serverID string
	token    string
	me       api.Principal
	features []api.Feature
	seq      int
	calls    []Call
	acks     map[string]any
	lost     int

	profiles    []*api.Profile
	devices     []*api.Device
	policies    map[string]*api.AccessPolicy
	users       []*api.User
	owner       string
	pairings    []*pairing
	invitations []*api.Invitation
	redeemed    map[string]api.Credential
	// credentials maps issued device tokens to their devices.
	credentials map[string]string
	sources     []*api.Source
	libraries   []*api.Library
	scans       []*api.Scan
	items       []api.CatalogItem
}

type pairing struct {
	api.PairingSummary
	deviceCode string
	deviceID   string
}

// NewServer returns a fixture whose principal holds every permission and
// whose bearer token is token.
func NewServer(serverID, token string) *Server {
	return &Server{
		Jobs:     NewJobs(),
		serverID: serverID, token: token,
		me:       api.Principal{ID: "operator", ProfileIDs: []string{}, Permissions: []string{api.PermSystemAdmin}, Mode: "operator"},
		features: []api.Feature{{ID: "identity.access_management", Implemented: true, Enabled: true}},
		acks:     map[string]any{}, policies: map[string]*api.AccessPolicy{}, redeemed: map[string]api.Credential{}, credentials: map[string]string{},
	}
}

// SetPermissions replaces the principal's permissions.
func (s *Server) SetPermissions(permissions ...string) {
	s.mu.Lock()
	defer s.mu.Unlock()
	s.me.Permissions = permissions
}

// EnableFeatures adds enabled, implemented capabilities.
func (s *Server) EnableFeatures(ids ...string) {
	s.mu.Lock()
	defer s.mu.Unlock()
	for _, id := range ids {
		s.features = append(s.features, api.Feature{ID: id, Implemented: true, Enabled: true})
	}
}

// DisableFeature removes a capability, as an older server would lack it.
func (s *Server) DisableFeature(id string) {
	s.mu.Lock()
	defer s.mu.Unlock()
	s.features = slices.DeleteFunc(s.features, func(f api.Feature) bool { return f.ID == id })
}

// LoseAcks applies the next n keyed or conditional writes but loses their
// acknowledgements.
func (s *Server) LoseAcks(n int) {
	s.mu.Lock()
	defer s.mu.Unlock()
	s.lost = n
}

// Calls returns the management requests received so far.
func (s *Server) Calls() []Call {
	s.mu.Lock()
	defer s.mu.Unlock()
	return append([]Call(nil), s.calls...)
}

// Writes returns the management write requests received so far.
func (s *Server) Writes() []Call {
	var out []Call
	for _, c := range s.Calls() {
		if !strings.HasPrefix(c.Op, "list") && !strings.HasPrefix(c.Op, "get") {
			out = append(out, c)
		}
	}
	return out
}

func (s *Server) id(prefix string) string {
	s.seq++
	return fmt.Sprintf("%s%d", prefix, s.seq)
}

func (s *Server) record(op, id, key, ifMatch string) {
	s.calls = append(s.calls, Call{Op: op, JobID: id, Key: key, IfMatch: ifMatch})
}

func tag(revision string) string { return `"r-` + revision + `"` }

func bump(revision *string) {
	n, _ := strconv.Atoi(*revision)
	*revision = strconv.Itoa(n + 1)
}

func problem(status int, code string) *api.Problem {
	return &api.Problem{Status: status, Code: code, Title: http.StatusText(status)}
}

// precondition checks a strong If-Match against the current revision.
func precondition(ifMatch, revision string) error {
	if ifMatch == "" {
		return problem(428, "precondition_required")
	}
	if ifMatch != tag(revision) {
		return problem(412, "precondition_failed")
	}
	return nil
}

// replay answers a keyed write from its receipt, or runs apply once and
// records the receipt. Lost acknowledgements apply but fail.
func replay[T any](s *Server, op, key string, request any, apply func() (T, error)) (T, error) {
	var zero T
	if key == "" {
		return zero, problem(400, "idempotency_key_required")
	}
	body, _ := json.Marshal(request)
	r, ok := s.acks[op+"\x00"+key].(receipt)
	if ok && r.body != string(body) {
		// The contract: a different request under the same key is 409.
		return zero, problem(409, "idempotency_key_reused")
	}
	if !ok {
		v, err := apply()
		if err != nil {
			return zero, err
		}
		r = receipt{body: string(body), ack: v}
		s.acks[op+"\x00"+key] = r
	}
	_, err := s.deliver(r.ack)
	return r.ack.(T), err
}

// receipt is a saved acknowledgement and the request it answered.
type receipt struct {
	body string
	ack  any
}

func (s *Server) deliver(v any) (_ any, err error) {
	if s.lost > 0 {
		s.lost--
		return v, errLostAck
	}
	return v, nil
}

func page[T any](all []T, cursor string, limit int) api.Page[T] {
	start, _ := strconv.Atoi(cursor)
	if limit <= 0 {
		limit = 50
	}
	start = min(start, len(all))
	end := min(start+limit, len(all))
	p := api.Page[T]{Items: append([]T{}, all[start:end]...), ReadRevision: "1"}
	if end < len(all) {
		next := strconv.Itoa(end)
		p.NextCursor = &next
	}
	return p
}

func values[T any](rows []*T) []T {
	out := make([]T, len(rows))
	for i, r := range rows {
		out[i] = *r
	}
	return out
}

func find[T any](rows []*T, id func(*T) string, want string) *T {
	for _, r := range rows {
		if id(r) == want {
			return r
		}
	}
	return nil
}

// issue returns a new credential for a device.
func (s *Server) issue(device string) api.Credential {
	token := "device-token-" + device + "-0123456789abcdef0123456789"
	s.credentials[token] = device
	return api.Credential{DeviceID: device, AccessToken: token, ExpiresAt: "2027-01-01T00:00:00Z"}
}

// principal is who a bearer token authenticates, if anyone.
func (s *Server) principal(token string) (api.Principal, bool) {
	s.mu.Lock()
	defer s.mu.Unlock()
	if token == s.token {
		return s.me, true
	}
	id, ok := s.credentials[token]
	if !ok {
		return api.Principal{}, false
	}
	d := find(s.devices, deviceID, id)
	if d == nil || d.Revoked {
		return api.Principal{}, false
	}
	return api.Principal{ID: d.ID, DeviceID: &d.ID, ProfileIDs: d.ProfileIDs, Permissions: d.Permissions, PolicyRevision: s.policies[d.ID].Revision, Mode: "paired"}, true
}

// Principal and capabilities ------------------------------------------------

func (s *Server) Me(context.Context) (api.Principal, error) {
	s.mu.Lock()
	defer s.mu.Unlock()
	return s.me, nil
}

func (s *Server) Capabilities(context.Context) (api.Capabilities, error) {
	s.mu.Lock()
	defer s.mu.Unlock()
	return api.Capabilities{ServerID: s.serverID, ServerVersion: "fixture", APIVersion: "2", Features: s.features}, nil
}

// Profiles ------------------------------------------------------------------

func profileID(p *api.Profile) string { return p.ID }

// AddProfile seeds a profile.
func (s *Server) AddProfile(name string) api.Profile {
	s.mu.Lock()
	defer s.mu.Unlock()
	p := &api.Profile{ID: s.id("p"), Name: name, Revision: "1"}
	s.profiles = append(s.profiles, p)
	return *p
}

func (s *Server) ListProfiles(_ context.Context, cursor string, limit int) (api.Page[api.Profile], error) {
	s.mu.Lock()
	defer s.mu.Unlock()
	s.record("listProfiles", "", "", "")
	return page(values(s.profiles), cursor, limit), nil
}

func (s *Server) GetProfile(_ context.Context, id string) (api.Tagged[api.Profile], error) {
	s.mu.Lock()
	defer s.mu.Unlock()
	s.record("getProfile", id, "", "")
	if p := find(s.profiles, profileID, id); p != nil {
		return api.Tagged[api.Profile]{Value: *p, ETag: tag(p.Revision)}, nil
	}
	return api.Tagged[api.Profile]{}, problem(404, "not_found")
}

func (s *Server) CreateProfile(_ context.Context, name, key string) (api.Tagged[api.Profile], error) {
	s.mu.Lock()
	defer s.mu.Unlock()
	s.record("createProfile", "", key, "")
	return replay(s, "createProfile", key, name, func() (api.Tagged[api.Profile], error) {
		p := &api.Profile{ID: s.id("p"), Name: name, Revision: "1"}
		s.profiles = append(s.profiles, p)
		return api.Tagged[api.Profile]{Value: *p, ETag: tag(p.Revision)}, nil
	})
}

func (s *Server) ReplaceProfile(_ context.Context, id, name, etag string) (api.Tagged[api.Profile], error) {
	s.mu.Lock()
	defer s.mu.Unlock()
	s.record("replaceProfile", id, "", etag)
	p := find(s.profiles, profileID, id)
	if p == nil {
		return api.Tagged[api.Profile]{}, problem(404, "not_found")
	}
	if err := precondition(etag, p.Revision); err != nil {
		return api.Tagged[api.Profile]{}, err
	}
	p.Name = name
	bump(&p.Revision)
	v, err := s.deliver(api.Tagged[api.Profile]{Value: *p, ETag: tag(p.Revision)})
	return v.(api.Tagged[api.Profile]), err
}

func (s *Server) DeleteProfile(_ context.Context, id, etag string) error {
	s.mu.Lock()
	defer s.mu.Unlock()
	s.record("deleteProfile", id, "", etag)
	p := find(s.profiles, profileID, id)
	if p == nil {
		return problem(404, "not_found")
	}
	if err := precondition(etag, p.Revision); err != nil {
		return err
	}
	s.profiles = slices.DeleteFunc(s.profiles, func(q *api.Profile) bool { return q.ID == id })
	_, err := s.deliver(nil)
	return err
}

// Devices and policies ------------------------------------------------------

func deviceID(d *api.Device) string { return d.ID }

// AddDevice seeds a paired device with an open policy.
func (s *Server) AddDevice(name string) api.Device {
	s.mu.Lock()
	defer s.mu.Unlock()
	d := &api.Device{ID: s.id("d"), Name: name, ClientName: "fixture", Revision: "1", ProfileIDs: []string{}, Permissions: []string{api.PermCatalogRead}}
	s.devices = append(s.devices, d)
	s.policies[d.ID] = &api.AccessPolicy{Revision: "1", LibraryIDs: []string{}, AllowUnrated: true, AllowedRatings: []string{}, BlockedLabels: []string{}, Permissions: d.Permissions}
	return *d
}

func (s *Server) ListDevices(_ context.Context, cursor string, limit int) (api.Page[api.Device], error) {
	s.mu.Lock()
	defer s.mu.Unlock()
	s.record("listDevices", "", "", "")
	return page(values(s.devices), cursor, limit), nil
}

func (s *Server) GetDevice(_ context.Context, id string) (api.Tagged[api.Device], error) {
	s.mu.Lock()
	defer s.mu.Unlock()
	s.record("getDevice", id, "", "")
	if d := find(s.devices, deviceID, id); d != nil {
		return api.Tagged[api.Device]{Value: *d, ETag: tag(d.Revision)}, nil
	}
	return api.Tagged[api.Device]{}, problem(404, "not_found")
}

func (s *Server) RevokeDevice(_ context.Context, id, etag string) error {
	s.mu.Lock()
	defer s.mu.Unlock()
	s.record("revokeDevice", id, "", etag)
	d := find(s.devices, deviceID, id)
	if d == nil {
		return problem(404, "not_found")
	}
	if err := precondition(etag, d.Revision); err != nil {
		return err
	}
	d.Revoked = true
	bump(&d.Revision)
	_, err := s.deliver(nil)
	return err
}

// AssignDeviceUser follows the server's assign_owner policy: the device
// revision first, then an owned device or a changed user revision is a
// failed precondition, a revoked device or disabled user a conflict; the
// device's grant then narrows to the user's ceiling.
func (s *Server) AssignDeviceUser(_ context.Context, id, user, userRevision, etag string) (api.Tagged[api.Device], error) {
	s.mu.Lock()
	defer s.mu.Unlock()
	s.record("assignDeviceUser", id, "", etag)
	d := find(s.devices, deviceID, id)
	u := find(s.users, userID, user)
	if d == nil || u == nil {
		return api.Tagged[api.Device]{}, problem(404, "not_found")
	}
	if err := precondition(etag, d.Revision); err != nil {
		return api.Tagged[api.Device]{}, err
	}
	switch {
	case d.UserID != nil || u.Revision != userRevision:
		return api.Tagged[api.Device]{}, problem(412, "precondition_failed")
	case d.Revoked:
		return api.Tagged[api.Device]{}, problem(409, "device_revoked")
	case u.Disabled:
		return api.Tagged[api.Device]{}, problem(409, "user_disabled")
	}
	// Compute the narrowed grant first; refuse it without any change.
	p := *s.policies[d.ID]
	permissions, profiles := d.Permissions, d.ProfileIDs
	switch {
	case slices.Contains(u.Permissions, api.PermSystemAdmin):
	case slices.Contains(d.Permissions, api.PermSystemAdmin):
		permissions, profiles = append([]string{}, u.Permissions...), append([]string{}, u.ProfileIDs...)
		p.LibraryIDs, p.AllowUnrated, p.DenyRated = u.Policy.LibraryIDs, u.Policy.AllowUnrated, u.Policy.DenyRated
		p.AllowedRatings, p.BlockedLabels = u.Policy.AllowedRatings, u.Policy.BlockedLabels
	default:
		permissions, profiles = intersect(d.Permissions, u.Permissions), intersect(d.ProfileIDs, u.ProfileIDs)
		p.LibraryIDs, p.AllowUnrated = intersect(p.LibraryIDs, u.Policy.LibraryIDs), p.AllowUnrated && u.Policy.AllowUnrated
		// Labels are a set, as in the server: count distinct ones.
		labels := []string{}
		for _, l := range append(append([]string{}, p.BlockedLabels...), u.Policy.BlockedLabels...) {
			if !slices.Contains(labels, l) {
				labels = append(labels, l)
			}
		}
		p.BlockedLabels = labels
		p.DenyRated, p.AllowedRatings = ratingIntersection(p.DenyRated, p.AllowedRatings, u.Policy.DenyRated, u.Policy.AllowedRatings)
	}
	if len(p.BlockedLabels) > maxBlockedLabels {
		return api.Tagged[api.Device]{}, problem(422, "policy_limit_exceeded")
	}
	d.Permissions, d.ProfileIDs = permissions, profiles
	p.Permissions = permissions
	*s.policies[d.ID] = p
	owner := user
	d.UserID = &owner
	bump(&d.Revision)
	bump(&s.policies[d.ID].Revision)
	v, err := s.deliver(api.Tagged[api.Device]{Value: *d, ETag: tag(d.Revision)})
	return v.(api.Tagged[api.Device]), err
}

// maxBlockedLabels is the server's MAX_BLOCKED_LABELS.
const maxBlockedLabels = 200

// ratingIntersection follows the server's RatingEligibility: deny-rated is
// none, an empty list is any rating, and two lists intersect; an empty
// intersection denies rated content.
func ratingIntersection(denyA bool, a []string, denyB bool, b []string) (bool, []string) {
	switch {
	case denyA || denyB:
		return true, []string{}
	case len(a) == 0:
		return false, append([]string{}, b...)
	case len(b) == 0:
		return false, append([]string{}, a...)
	}
	both := intersect(a, b)
	return len(both) == 0, both
}

func intersect(a, b []string) []string {
	out := []string{}
	for _, v := range a {
		if slices.Contains(b, v) {
			out = append(out, v)
		}
	}
	return out
}

func (s *Server) GetDevicePolicy(_ context.Context, id string) (api.Tagged[api.AccessPolicy], error) {
	s.mu.Lock()
	defer s.mu.Unlock()
	s.record("getDevicePolicy", id, "", "")
	if p, ok := s.policies[id]; ok {
		return api.Tagged[api.AccessPolicy]{Value: *p, ETag: tag(p.Revision)}, nil
	}
	return api.Tagged[api.AccessPolicy]{}, problem(404, "not_found")
}

func (s *Server) ReplaceDevicePolicy(_ context.Context, id string, in api.AccessPolicy, etag string) (api.Tagged[api.AccessPolicy], error) {
	s.mu.Lock()
	defer s.mu.Unlock()
	s.record("replaceDevicePolicy", id, "", etag)
	p, ok := s.policies[id]
	if !ok {
		return api.Tagged[api.AccessPolicy]{}, problem(404, "not_found")
	}
	if err := precondition(etag, p.Revision); err != nil {
		return api.Tagged[api.AccessPolicy]{}, err
	}
	in.Revision = p.Revision
	bump(&in.Revision)
	*p = in
	if d := find(s.devices, deviceID, id); d != nil {
		d.Permissions = in.Permissions
	}
	v, err := s.deliver(api.Tagged[api.AccessPolicy]{Value: *p, ETag: tag(p.Revision)})
	return v.(api.Tagged[api.AccessPolicy]), err
}

// Users, pairings and invitations -------------------------------------------

func userID(u *api.User) string { return u.ID }

// AddUser seeds an enabled access user.
func (s *Server) AddUser(name string) api.User {
	s.mu.Lock()
	defer s.mu.Unlock()
	u := &api.User{ID: s.id("u"), Name: name, Revision: "1", ProfileIDs: []string{}, Permissions: []string{api.PermCatalogRead},
		Policy: api.GrantPolicy{LibraryIDs: []string{}, AllowUnrated: true, AllowedRatings: []string{}, BlockedLabels: []string{}}}
	s.users = append(s.users, u)
	return *u
}

func (s *Server) ListUsers(_ context.Context, cursor string, limit int) (api.Page[api.User], error) {
	s.mu.Lock()
	defer s.mu.Unlock()
	s.record("listUsers", "", "", "")
	return page(values(s.users), cursor, limit), nil
}

func (s *Server) GetUser(_ context.Context, id string) (api.Tagged[api.User], error) {
	s.mu.Lock()
	defer s.mu.Unlock()
	s.record("getUser", id, "", "")
	if u := find(s.users, userID, id); u != nil {
		return api.Tagged[api.User]{Value: *u, ETag: tag(u.Revision)}, nil
	}
	return api.Tagged[api.User]{}, problem(404, "not_found")
}

func (s *Server) CreateUser(_ context.Context, in api.UserInput, key string) (api.Tagged[api.User], error) {
	s.mu.Lock()
	defer s.mu.Unlock()
	s.record("createUser", "", key, "")
	return replay(s, "createUser", key, in, func() (api.Tagged[api.User], error) {
		u := &api.User{ID: s.id("u"), Name: in.Name, Revision: "1", Disabled: in.Disabled, ProfileIDs: in.ProfileIDs, Permissions: in.Permissions, Policy: in.Policy}
		s.users = append(s.users, u)
		return api.Tagged[api.User]{Value: *u, ETag: tag(u.Revision)}, nil
	})
}

// GetAccessSetup mirrors the server's owner setup state.
func (s *Server) GetAccessSetup(_ context.Context) (api.AccessSetup, error) {
	s.mu.Lock()
	defer s.mu.Unlock()
	s.record("getAccessSetup", "", "", "")
	return s.setup(), nil
}

func (s *Server) setup() api.AccessSetup {
	v := api.AccessSetup{OwnerSetup: api.SetupExistingUsers}
	switch {
	case s.owner != "":
		v.OwnerSetup = api.SetupComplete
	case len(s.users) == 0:
		v.OwnerSetup = api.SetupRequired
	}
	var eligible []*api.User
	for _, u := range s.users {
		if u.ID == s.owner {
			c := *u
			v.Owner = &c
		}
		if !u.Disabled {
			eligible = append(eligible, u)
		}
	}
	v.EligibleUsers = len(eligible)
	if len(eligible) == 1 {
		c := *eligible[0]
		v.PairingUser = &c
	}
	return v
}

// SetupOwner creates the owner only when no user exists; a repeat returns it.
func (s *Server) SetupOwner(_ context.Context, in api.SetupInput) (api.AccessSetup, error) {
	s.mu.Lock()
	defer s.mu.Unlock()
	s.record("setupAccessOwner", "", "", "")
	if s.owner == "" {
		if len(s.users) != 0 {
			return api.AccessSetup{}, problem(409, "owner_setup_unavailable")
		}
		name := in.Name
		if name == "" {
			name = "Owner"
		}
		u := &api.User{ID: s.id("u"), Name: name, Revision: "1", ProfileIDs: []string{}, Permissions: slices.Clone(api.AllPermissions), Policy: api.GrantPolicy{LibraryIDs: []string{}, AllowUnrated: true, AllowedRatings: []string{}, BlockedLabels: []string{}}}
		s.users = append(s.users, u)
		s.owner = u.ID
	}
	v, err := s.deliver(s.setup())
	return v.(api.AccessSetup), err
}

func (s *Server) ReplaceUser(_ context.Context, id string, in api.UserInput, etag string) (api.Tagged[api.User], error) {
	s.mu.Lock()
	defer s.mu.Unlock()
	s.record("replaceUser", id, "", etag)
	u := find(s.users, userID, id)
	if u == nil {
		return api.Tagged[api.User]{}, problem(404, "not_found")
	}
	if err := precondition(etag, u.Revision); err != nil {
		return api.Tagged[api.User]{}, err
	}
	u.Name, u.Disabled, u.ProfileIDs, u.Permissions, u.Policy = in.Name, in.Disabled, in.ProfileIDs, in.Permissions, in.Policy
	bump(&u.Revision)
	v, err := s.deliver(api.Tagged[api.User]{Value: *u, ETag: tag(u.Revision)})
	return v.(api.Tagged[api.User]), err
}

func (s *Server) CreatePairing(_ context.Context, in api.PairingRequest) (api.Pairing, error) {
	s.mu.Lock()
	defer s.mu.Unlock()
	s.record("createPairing", "", "", "")
	p := &pairing{PairingSummary: api.PairingSummary{ID: s.id("pair"), DeviceName: in.DeviceName, ClientName: in.ClientName,
		UserCode: fmt.Sprintf("CODE-%04d", s.seq), Status: "pending", CreatedAt: "2026-10-14T00:00:00Z", ExpiresAt: "2026-10-14T00:10:00Z"},
		deviceCode: fmt.Sprintf("mdc_fixture_secret_%d", s.seq)}
	s.pairings = append(s.pairings, p)
	return api.Pairing{ID: p.ID, DeviceCode: p.deviceCode, UserCode: p.UserCode, ExpiresAt: p.ExpiresAt, PollIntervalSeconds: 1}, nil
}

func (s *Server) ClaimPairing(_ context.Context, id, deviceCode string) (api.Credential, error) {
	s.mu.Lock()
	defer s.mu.Unlock()
	s.record("claimPairing", id, "", "")
	for _, p := range s.pairings {
		if p.ID != id || p.deviceCode != deviceCode {
			continue
		}
		switch p.Status {
		case "approved":
			return s.issue(p.deviceID), nil
		case "pending":
			return api.Credential{}, &api.Problem{Status: 409, Code: "pairing_pending", Title: "Conflict", RetryAfter: time.Millisecond}
		}
		return api.Credential{}, problem(403, "pairing_denied")
	}
	return api.Credential{}, problem(404, "not_found")
}

func (s *Server) ListPairings(_ context.Context, cursor string, limit int) (api.Page[api.PairingSummary], error) {
	s.mu.Lock()
	defer s.mu.Unlock()
	s.record("listPairings", "", "", "")
	all := []api.PairingSummary{}
	for _, p := range s.pairings {
		all = append(all, p.PairingSummary)
	}
	return page(all, cursor, limit), nil
}

func (s *Server) ApprovePairing(_ context.Context, id string, in api.PairingApproval, key string) (api.Tagged[api.Device], error) {
	s.mu.Lock()
	defer s.mu.Unlock()
	s.record("approvePairing", id, key, "")
	return replay(s, "approvePairing", key, []any{id, in}, func() (api.Tagged[api.Device], error) {
		for _, p := range s.pairings {
			if p.ID != id {
				continue
			}
			if p.Status != "pending" {
				return api.Tagged[api.Device]{}, problem(409, "pairing_not_pending")
			}
			if p.UserCode != in.UserCode {
				return api.Tagged[api.Device]{}, problem(409, "pairing_code_mismatch")
			}
			d := &api.Device{ID: s.id("d"), Name: p.DeviceName, ClientName: p.ClientName, Revision: "1", ProfileIDs: in.ProfileIDs, Permissions: in.Permissions}
			if in.UserID != "" {
				u := find(s.users, userID, in.UserID)
				if u == nil {
					return api.Tagged[api.Device]{}, problem(404, "not_found")
				}
				if u.Revision != in.UserRevision {
					return api.Tagged[api.Device]{}, problem(409, "user_changed")
				}
				owner := in.UserID
				d.UserID = &owner
			}
			policy := &api.AccessPolicy{Revision: "1", LibraryIDs: []string{}, AllowUnrated: true, AllowedRatings: []string{}, BlockedLabels: []string{}, Permissions: in.Permissions}
			if in.Policy != nil {
				policy.LibraryIDs, policy.AllowUnrated, policy.DenyRated = in.Policy.LibraryIDs, in.Policy.AllowUnrated, in.Policy.DenyRated
				policy.AllowedRatings, policy.BlockedLabels = in.Policy.AllowedRatings, in.Policy.BlockedLabels
			}
			s.devices = append(s.devices, d)
			s.policies[d.ID] = policy
			p.Status, p.deviceID = "approved", d.ID
			return api.Tagged[api.Device]{Value: *d, ETag: tag(d.Revision)}, nil
		}
		return api.Tagged[api.Device]{}, problem(404, "not_found")
	})
}

func (s *Server) DenyPairing(_ context.Context, id, key string) error {
	s.mu.Lock()
	defer s.mu.Unlock()
	s.record("denyPairing", id, key, "")
	_, err := replay(s, "denyPairing", key, id, func() (struct{}, error) {
		for _, p := range s.pairings {
			if p.ID == id && p.Status == "pending" {
				p.Status = "denied"
				return struct{}{}, nil
			}
		}
		return struct{}{}, problem(409, "pairing_not_pending")
	})
	return err
}

func invitationID(i *api.Invitation) string { return i.ID }

func (s *Server) ListInvitations(_ context.Context, cursor string, limit int) (api.Page[api.Invitation], error) {
	s.mu.Lock()
	defer s.mu.Unlock()
	s.record("listInvitations", "", "", "")
	all := values(s.invitations)
	for i := range all {
		all[i].Code = "" // shown only at issuance
	}
	return page(all, cursor, limit), nil
}

func (s *Server) CreateInvitation(_ context.Context, in api.InvitationInput, key string) (api.Tagged[api.Invitation], error) {
	s.mu.Lock()
	defer s.mu.Unlock()
	s.record("createInvitation", in.UserID, key, "")
	return replay(s, "createInvitation", key, in, func() (api.Tagged[api.Invitation], error) {
		u := find(s.users, userID, in.UserID)
		if u == nil {
			return api.Tagged[api.Invitation]{}, problem(404, "not_found")
		}
		if u.Revision != in.UserRevision {
			return api.Tagged[api.Invitation]{}, problem(409, "user_changed")
		}
		i := &api.Invitation{ID: s.id("inv"), UserID: in.UserID, Revision: "1", Status: "pending", CreatedAt: "2026-10-14T00:00:00Z",
			ExpiresAt: "2026-10-14T00:10:00Z", ProfileIDs: in.ProfileIDs, Permissions: in.Permissions, Policy: in.Policy,
			Code: fmt.Sprintf("mi_fixture_code_%d_0123456789abcdef", s.seq)}
		s.invitations = append(s.invitations, i)
		return api.Tagged[api.Invitation]{Value: *i, ETag: tag(i.Revision)}, nil
	})
}

func (s *Server) RevokeInvitation(_ context.Context, id, etag string) error {
	s.mu.Lock()
	defer s.mu.Unlock()
	s.record("revokeInvitation", id, "", etag)
	i := find(s.invitations, invitationID, id)
	if i == nil {
		return problem(404, "not_found")
	}
	if err := precondition(etag, i.Revision); err != nil {
		return err
	}
	i.Status = "revoked"
	bump(&i.Revision)
	_, err := s.deliver(nil)
	return err
}

func (s *Server) RedeemInvitation(_ context.Context, in api.RedemptionInput) (api.Credential, error) {
	s.mu.Lock()
	defer s.mu.Unlock()
	s.record("redeemInvitation", "", in.ClaimID, "")
	if c, ok := s.redeemed[in.Code+"\x00"+in.ClaimID]; ok {
		return c, nil // the same claim replays
	}
	for _, i := range s.invitations {
		if i.Code == in.Code && i.Status == "pending" {
			i.Status = "claimed"
			bump(&i.Revision)
			d := &api.Device{ID: s.id("d"), Name: in.DeviceName, ClientName: in.ClientName, Revision: "1", UserID: &i.UserID, ProfileIDs: i.ProfileIDs, Permissions: i.Permissions}
			s.devices = append(s.devices, d)
			s.policies[d.ID] = &api.AccessPolicy{Revision: "1", LibraryIDs: []string{}, AllowUnrated: true, AllowedRatings: []string{}, BlockedLabels: []string{}, Permissions: i.Permissions}
			c := s.issue(d.ID)
			s.redeemed[in.Code+"\x00"+in.ClaimID] = c
			return c, nil
		}
	}
	return api.Credential{}, problem(403, "invitation_invalid")
}

// Sources, libraries and scans ----------------------------------------------

func sourceID(v *api.Source) string   { return v.ID }
func libraryID(v *api.Library) string { return v.ID }
func scanID(v *api.Scan) string       { return v.ID }

// AddSource seeds a storage source.
func (s *Server) AddSource(name, root string) api.Source {
	s.mu.Lock()
	defer s.mu.Unlock()
	v := &api.Source{ID: s.id("s"), Name: name, RootPath: root, Revision: "1", Availability: "available", Exclusions: []string{}}
	s.sources = append(s.sources, v)
	return *v
}

// AddLibrary seeds a library over sources.
func (s *Server) AddLibrary(name string, sources ...string) api.Library {
	s.mu.Lock()
	defer s.mu.Unlock()
	v := &api.Library{ID: s.id("l"), Name: name, Kind: "mixed", Revision: "1", Sources: append([]string{}, sources...), Availability: "available"}
	s.libraries = append(s.libraries, v)
	return *v
}

func (s *Server) ListSources(_ context.Context, cursor string, limit int) (api.Page[api.Source], error) {
	s.mu.Lock()
	defer s.mu.Unlock()
	s.record("listSources", "", "", "")
	return page(values(s.sources), cursor, limit), nil
}

func (s *Server) GetSource(_ context.Context, id string) (api.Tagged[api.Source], error) {
	s.mu.Lock()
	defer s.mu.Unlock()
	s.record("getSource", id, "", "")
	if v := find(s.sources, sourceID, id); v != nil {
		return api.Tagged[api.Source]{Value: *v, ETag: tag(v.Revision)}, nil
	}
	return api.Tagged[api.Source]{}, problem(404, "not_found")
}

func (s *Server) CreateSource(_ context.Context, in api.SourceInput, key string) (api.Tagged[api.Source], error) {
	s.mu.Lock()
	defer s.mu.Unlock()
	s.record("createSource", "", key, "")
	return replay(s, "createSource", key, in, func() (api.Tagged[api.Source], error) {
		if !strings.HasPrefix(in.RootPath, "/") {
			return api.Tagged[api.Source]{}, problem(422, "invalid_root")
		}
		v := &api.Source{ID: s.id("s"), Name: in.Name, RootPath: in.RootPath, Revision: "1", Availability: "available", Exclusions: in.Exclusions}
		s.sources = append(s.sources, v)
		return api.Tagged[api.Source]{Value: *v, ETag: tag(v.Revision)}, nil
	})
}

func (s *Server) ListLibraries(_ context.Context, cursor string, limit int) (api.Page[api.Library], error) {
	s.mu.Lock()
	defer s.mu.Unlock()
	s.record("listLibraries", "", "", "")
	return page(values(s.libraries), cursor, limit), nil
}

func (s *Server) GetLibrary(_ context.Context, id string) (api.Tagged[api.Library], error) {
	s.mu.Lock()
	defer s.mu.Unlock()
	s.record("getLibrary", id, "", "")
	if v := find(s.libraries, libraryID, id); v != nil {
		return api.Tagged[api.Library]{Value: *v, ETag: tag(v.Revision)}, nil
	}
	return api.Tagged[api.Library]{}, problem(404, "not_found")
}

func (s *Server) CreateLibrary(_ context.Context, in api.LibraryInput, key string) (api.Tagged[api.Library], error) {
	s.mu.Lock()
	defer s.mu.Unlock()
	s.record("createLibrary", "", key, "")
	return replay(s, "createLibrary", key, in, func() (api.Tagged[api.Library], error) {
		for _, id := range in.SourceIDs {
			if find(s.sources, sourceID, id) == nil {
				return api.Tagged[api.Library]{}, problem(422, "unknown_source")
			}
		}
		v := &api.Library{ID: s.id("l"), Name: in.Name, Kind: in.Kind, Language: in.Language, Revision: "1", Sources: in.SourceIDs, Availability: "available"}
		s.libraries = append(s.libraries, v)
		return api.Tagged[api.Library]{Value: *v, ETag: tag(v.Revision)}, nil
	})
}

func (s *Server) CreateScan(_ context.Context, library string, in api.ScanInput, key string) (api.Tagged[api.Scan], error) {
	s.mu.Lock()
	defer s.mu.Unlock()
	s.record("createScan", library, key, "")
	return replay(s, "createScan", key, []any{library, in}, func() (api.Tagged[api.Scan], error) {
		l := find(s.libraries, libraryID, library)
		if l == nil {
			return api.Tagged[api.Scan]{}, problem(404, "not_found")
		}
		sources := l.Sources
		if in.SourceIDs != nil {
			if len(*in.SourceIDs) == 0 {
				return api.Tagged[api.Scan]{}, problem(422, "empty_selection")
			}
			sources = *in.SourceIDs
		}
		v := &api.Scan{ID: s.id("scan"), Revision: "1", LibraryID: library, Status: "queued:" + in.Mode, RequireComplete: in.RequireComplete, JobIDs: []string{}}
		for _, id := range sources {
			v.Sources = append(v.Sources, api.ScanSourceResult{SourceID: id, Status: "queued", ErrorCodes: []string{}})
		}
		s.scans = append(s.scans, v)
		return api.Tagged[api.Scan]{Value: *v, ETag: tag(v.Revision)}, nil
	})
}

func (s *Server) ListScans(_ context.Context, cursor string, limit int) (api.Page[api.Scan], error) {
	s.mu.Lock()
	defer s.mu.Unlock()
	s.record("listScans", "", "", "")
	return page(values(s.scans), cursor, limit), nil
}

func (s *Server) GetScan(_ context.Context, id string) (api.Tagged[api.Scan], error) {
	s.mu.Lock()
	defer s.mu.Unlock()
	s.record("getScan", id, "", "")
	if v := find(s.scans, scanID, id); v != nil {
		return api.Tagged[api.Scan]{Value: *v, ETag: tag(v.Revision)}, nil
	}
	return api.Tagged[api.Scan]{}, problem(404, "not_found")
}

// Snapshot ------------------------------------------------------------------

// Snapshot is the fixture's domain state without generated identifiers, so
// runs through different interfaces can be compared.
type Snapshot struct {
	Profiles    []string
	Devices     []string
	Users       []string
	Pairings    []string
	Invitations []string
	Sources     []string
	Libraries   []string
	Scans       []string
}

// Snapshot returns the comparable domain state.
func (s *Server) Snapshot() Snapshot {
	s.mu.Lock()
	defer s.mu.Unlock()
	name := func(id string) string {
		if v := find(s.sources, sourceID, id); v != nil {
			return v.Name
		}
		if v := find(s.libraries, libraryID, id); v != nil {
			return v.Name
		}
		if v := find(s.users, userID, id); v != nil {
			return v.Name
		}
		return id
	}
	names := func(ids []string) string {
		out := []string{}
		for _, id := range ids {
			out = append(out, name(id))
		}
		sort.Strings(out)
		return strings.Join(out, "+")
	}
	var snap Snapshot
	for _, p := range s.profiles {
		snap.Profiles = append(snap.Profiles, fmt.Sprintf("%s@%s", p.Name, p.Revision))
	}
	for _, d := range s.devices {
		pol := s.policies[d.ID]
		owner := "-"
		if d.UserID != nil {
			owner = name(*d.UserID)
		}
		snap.Devices = append(snap.Devices, fmt.Sprintf("%s owner=%s revoked=%v@%s policy=%s/%s/unrated=%v/denyrated=%v/ratings=%s/labels=%s@%s", d.Name, owner, d.Revoked, d.Revision,
			names(pol.LibraryIDs), strings.Join(pol.Permissions, "+"), pol.AllowUnrated, pol.DenyRated, strings.Join(pol.AllowedRatings, "+"), strings.Join(pol.BlockedLabels, "+"), pol.Revision))
	}
	for _, u := range s.users {
		snap.Users = append(snap.Users, fmt.Sprintf("%s disabled=%v perms=%s libraries=%s@%s", u.Name, u.Disabled, strings.Join(u.Permissions, "+"), names(u.Policy.LibraryIDs), u.Revision))
	}
	for _, p := range s.pairings {
		snap.Pairings = append(snap.Pairings, p.DeviceName+" "+p.Status)
	}
	for _, i := range s.invitations {
		snap.Invitations = append(snap.Invitations, fmt.Sprintf("%s %s ttl-perms=%s@%s", name(i.UserID), i.Status, strings.Join(i.Permissions, "+"), i.Revision))
	}
	for _, v := range s.sources {
		snap.Sources = append(snap.Sources, v.Name+" "+v.RootPath+" excl="+strings.Join(v.Exclusions, "+"))
	}
	for _, v := range s.libraries {
		snap.Libraries = append(snap.Libraries, v.Name+" "+v.Kind+" "+v.Language+" sources="+names(v.Sources))
	}
	for _, v := range s.scans {
		var sources []string
		for _, src := range v.Sources {
			sources = append(sources, src.SourceID)
		}
		snap.Scans = append(snap.Scans, fmt.Sprintf("%s %s complete=%v sources=%s", name(v.LibraryID), v.Status, v.RequireComplete, names(sources)))
	}
	return snap
}

// HTTP ----------------------------------------------------------------------

// Handler serves the fixture's management, principal and job endpoints.
// Authenticated routes require the fixture's bearer token.
func (s *Server) Handler() http.Handler {
	jobs := s.Jobs.Handler(s.serverID)
	return http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		ctx, path, q := r.Context(), r.URL.EscapedPath(), r.URL.Query()
		if path == "/api/v2/system/health" || strings.HasPrefix(path, "/api/v2/jobs") {
			jobs.ServeHTTP(w, r)
			return
		}
		key, ifMatch := r.Header.Get("Idempotency-Key"), r.Header.Get("If-Match")
		limit, _ := strconv.Atoi(q.Get("limit"))
		cursor := q.Get("cursor")
		body, _ := io.ReadAll(io.LimitReader(r.Body, 1<<20))
		decode := func(v any) error { return json.Unmarshal(body, v) }
		seg := strings.Split(strings.TrimPrefix(path, "/api/v2/"), "/")
		for i := range seg {
			seg[i] = unescape(seg[i])
		}
		route := r.Method + " " + strings.Join(pattern(seg), "/")
		public := route == "POST auth/pairings" || route == "POST auth/pairings/*/claim" || route == "POST auth/invitations/redeem"
		me, authenticated := s.principal(strings.TrimPrefix(r.Header.Get("Authorization"), "Bearer "))
		if !public && !authenticated {
			write(w, nil, "", problem(401, "authentication_required"))
			return
		}
		var (
			v    any
			etag string
			err  error
		)
		tagged := func(t any, e string, er error) { v, etag, err = t, e, er }
		switch route {
		case "GET me":
			v = me
		case "GET system/capabilities":
			v, err = s.Capabilities(ctx)
		case "GET profiles":
			v, err = s.ListProfiles(ctx, cursor, limit)
		case "POST profiles":
			var in struct{ Name string }
			_ = decode(&in)
			t, e := s.CreateProfile(ctx, in.Name, key)
			tagged(t.Value, t.ETag, e)
		case "GET profiles/*":
			t, e := s.GetProfile(ctx, seg[1])
			tagged(t.Value, t.ETag, e)
		case "PUT profiles/*":
			var in struct{ Name string }
			_ = decode(&in)
			t, e := s.ReplaceProfile(ctx, seg[1], in.Name, ifMatch)
			tagged(t.Value, t.ETag, e)
		case "DELETE profiles/*":
			err = s.DeleteProfile(ctx, seg[1], ifMatch)
		case "GET devices":
			v, err = s.ListDevices(ctx, cursor, limit)
		case "GET devices/*":
			t, e := s.GetDevice(ctx, seg[1])
			tagged(t.Value, t.ETag, e)
		case "DELETE devices/*":
			err = s.RevokeDevice(ctx, seg[1], ifMatch)
		case "PUT devices/*/user":
			var in struct {
				UserID       string `json:"user_id"`
				UserRevision string `json:"user_revision"`
			}
			_ = decode(&in)
			t, e := s.AssignDeviceUser(ctx, seg[1], in.UserID, in.UserRevision, ifMatch)
			tagged(t.Value, t.ETag, e)
		case "GET devices/*/policy":
			t, e := s.GetDevicePolicy(ctx, seg[1])
			tagged(t.Value, t.ETag, e)
		case "PUT devices/*/policy":
			var in api.AccessPolicy
			_ = decode(&in)
			t, e := s.ReplaceDevicePolicy(ctx, seg[1], in, ifMatch)
			tagged(t.Value, t.ETag, e)
		case "GET access/setup":
			v, err = s.GetAccessSetup(ctx)
		case "POST access/setup":
			var in api.SetupInput
			_ = decode(&in)
			v, err = s.SetupOwner(ctx, in)
		case "GET users":
			v, err = s.ListUsers(ctx, cursor, limit)
		case "POST users":
			var in api.UserInput
			_ = decode(&in)
			t, e := s.CreateUser(ctx, in, key)
			tagged(t.Value, t.ETag, e)
		case "GET users/*":
			t, e := s.GetUser(ctx, seg[1])
			tagged(t.Value, t.ETag, e)
		case "PUT users/*":
			var in api.UserInput
			_ = decode(&in)
			t, e := s.ReplaceUser(ctx, seg[1], in, ifMatch)
			tagged(t.Value, t.ETag, e)
		case "POST auth/pairings":
			var in api.PairingRequest
			_ = decode(&in)
			v, err = s.CreatePairing(ctx, in)
		case "POST auth/pairings/*/claim":
			var in struct {
				DeviceCode string `json:"device_code"`
			}
			_ = decode(&in)
			v, err = s.ClaimPairing(ctx, seg[2], in.DeviceCode)
		case "GET auth/pairings":
			v, err = s.ListPairings(ctx, cursor, limit)
		case "POST auth/pairings/*/approve":
			var in api.PairingApproval
			_ = decode(&in)
			t, e := s.ApprovePairing(ctx, seg[2], in, key)
			tagged(t.Value, t.ETag, e)
		case "POST auth/pairings/*/deny":
			err = s.DenyPairing(ctx, seg[2], key)
		case "GET auth/invitations":
			v, err = s.ListInvitations(ctx, cursor, limit)
		case "POST auth/invitations":
			var in api.InvitationInput
			_ = decode(&in)
			t, e := s.CreateInvitation(ctx, in, key)
			tagged(t.Value, t.ETag, e)
		case "DELETE auth/invitations/*":
			err = s.RevokeInvitation(ctx, seg[2], ifMatch)
		case "POST auth/invitations/redeem":
			var in api.RedemptionInput
			_ = decode(&in)
			v, err = s.RedeemInvitation(ctx, in)
		case "GET sources":
			v, err = s.ListSources(ctx, cursor, limit)
		case "POST sources":
			var in api.SourceInput
			_ = decode(&in)
			t, e := s.CreateSource(ctx, in, key)
			tagged(t.Value, t.ETag, e)
		case "GET sources/*":
			t, e := s.GetSource(ctx, seg[1])
			tagged(t.Value, t.ETag, e)
		case "GET libraries":
			v, err = s.ListLibraries(ctx, cursor, limit)
		case "POST libraries":
			var in api.LibraryInput
			_ = decode(&in)
			t, e := s.CreateLibrary(ctx, in, key)
			tagged(t.Value, t.ETag, e)
		case "GET libraries/*":
			t, e := s.GetLibrary(ctx, seg[1])
			tagged(t.Value, t.ETag, e)
		case "POST libraries/*/scans":
			var in api.ScanInput
			_ = decode(&in)
			t, e := s.CreateScan(ctx, seg[1], in, key)
			tagged(t.Value, t.ETag, e)
		case "GET scans":
			v, err = s.ListScans(ctx, cursor, limit)
		case "GET scans/*":
			t, e := s.GetScan(ctx, seg[1])
			tagged(t.Value, t.ETag, e)
		case "GET catalog/items":
			if parent := q.Get("parent_id"); parent != "" {
				v, err = s.ListCatalogChildren(ctx, parent, cursor, limit)
			} else {
				v, err = s.ListCatalog(ctx, cursor, limit, q.Get("library_id"))
			}
		case "GET catalog/search":
			v, err = s.SearchCatalog(ctx, q.Get("q"), cursor, limit)
		case "GET catalog/items/*":
			t, e := s.GetCatalogItem(ctx, seg[2])
			tagged(t.Value, t.ETag, e)
		case "GET catalog/items/*/editions":
			v, err = s.ListItemEditions(ctx, seg[2], cursor, limit)
		case "GET catalog/items/*/timelines":
			v, err = s.ListItemTimelines(ctx, seg[2], cursor, limit)
		case "GET catalog/timelines/*":
			t, e := s.GetTimeline(ctx, seg[2])
			tagged(t.Value, t.ETag, e)
		case "GET catalog/timelines/*/versions":
			v, err = s.ListVersions(ctx, seg[2], cursor, limit)
		case "GET profiles/*/continue-watching":
			v, err = s.ListContinueWatching(ctx, seg[1], cursor, limit)
		case "GET profiles/*/timelines/*/viewing":
			v, err = s.GetViewingState(ctx, seg[1], seg[3])
		case "GET admin/diagnostics":
			v, err = s.Diagnostics(ctx)
		case "GET admin/storage":
			v, err = s.Storage(ctx)
		case "GET admin/metrics":
			since, _ := time.Parse(time.RFC3339, q.Get("since"))
			step, _ := strconv.Atoi(q.Get("step_seconds"))
			v, err = s.Metrics(ctx, since, step)
		case "GET admin/playback/sessions":
			v, err = s.PlaybackSessions(ctx)
		case "GET admin/playback/history":
			v, err = s.PlaybackHistory(ctx, cursor, limit, api.HistoryFilter{DeviceID: q.Get("device_id"), ProfileID: q.Get("profile_id"), ItemID: q.Get("item_id")})
		default:
			err = problem(404, "not_found")
		}
		write(w, v, etag, err)
	})
}

// pattern replaces identifier segments with "*".
func pattern(seg []string) []string {
	out := append([]string{}, seg...)
	switch {
	case len(out) >= 3 && out[0] == "auth" && out[2] != "redeem",
		len(out) >= 3 && out[0] == "catalog":
		out[2] = "*"
	case out[0] == "auth", out[0] == "system", out[0] == "admin", out[0] == "catalog", out[0] == "access":
	case len(out) >= 2:
		out[1] = "*"
		if len(out) >= 4 && out[2] == "timelines" {
			out[3] = "*"
		}
	}
	return out
}

func unescape(s string) string {
	if v, err := url.PathUnescape(s); err == nil {
		return v
	}
	return s
}

func write(w http.ResponseWriter, v any, etag string, err error) {
	if err == errLostAck {
		if hj, ok := w.(http.Hijacker); ok {
			if conn, _, herr := hj.Hijack(); herr == nil {
				conn.Close()
				return
			}
		}
		w.WriteHeader(http.StatusBadGateway)
		return
	}
	if p, ok := err.(*api.Problem); ok {
		w.Header().Set("Content-Type", "application/problem+json")
		if p.RetryAfter > 0 {
			w.Header().Set("Retry-After", "1")
		}
		w.WriteHeader(p.Status)
		_ = json.NewEncoder(w).Encode(map[string]any{"type": "/problems/" + p.Code, "title": http.StatusText(p.Status),
			"status": p.Status, "code": p.Code, "detail": p.Detail, "request_id": "fixture", "retryable": false})
		return
	}
	if err != nil {
		w.WriteHeader(500)
		return
	}
	if etag != "" {
		w.Header().Set("ETag", etag)
	}
	if v == nil {
		w.WriteHeader(http.StatusNoContent)
		return
	}
	w.Header().Set("Content-Type", "application/json")
	_ = json.NewEncoder(w).Encode(v)
}

// Catalog -------------------------------------------------------------------

// AddItem seeds a catalog item in libraries.
func (s *Server) AddItem(title string, libraries ...string) api.CatalogItem {
	s.mu.Lock()
	defer s.mu.Unlock()
	item := api.CatalogItem{ID: s.id("i"), Title: title, Kind: "movie", Revision: "1", LibraryIDs: libraries, Availability: "available"}
	s.items = append(s.items, item)
	return item
}

func (s *Server) ListCatalog(_ context.Context, cursor string, limit int, library string) (api.Page[api.CatalogItem], error) {
	s.mu.Lock()
	defer s.mu.Unlock()
	s.record("listCatalog", library, "", "")
	var items []api.CatalogItem
	for _, i := range s.items {
		if library == "" || slices.Contains(i.LibraryIDs, library) {
			items = append(items, i)
		}
	}
	return page(items, cursor, limit), nil
}

// Each seeded item has one edition, timeline and version, with the
// timeline's ID derived from the item's.

func (s *Server) item(id string) *api.CatalogItem {
	for i := range s.items {
		if s.items[i].ID == id {
			return &s.items[i]
		}
	}
	return nil
}

func (s *Server) SearchCatalog(_ context.Context, text, cursor string, limit int) (api.Page[api.CatalogItem], error) {
	s.mu.Lock()
	defer s.mu.Unlock()
	s.record("searchCatalog", text, "", "")
	var items []api.CatalogItem
	for _, i := range s.items {
		if strings.Contains(strings.ToLower(i.Title), strings.ToLower(text)) {
			items = append(items, i)
		}
	}
	return page(items, cursor, limit), nil
}

func (s *Server) GetCatalogItem(_ context.Context, id string) (api.Tagged[api.CatalogItem], error) {
	s.mu.Lock()
	defer s.mu.Unlock()
	s.record("getItem", id, "", "")
	if i := s.item(id); i != nil {
		return api.Tagged[api.CatalogItem]{Value: *i, ETag: tag(i.Revision)}, nil
	}
	return api.Tagged[api.CatalogItem]{}, problem(404, "not_found")
}

func (s *Server) ListCatalogChildren(_ context.Context, parent, cursor string, limit int) (api.Page[api.CatalogItem], error) {
	s.mu.Lock()
	defer s.mu.Unlock()
	var items []api.CatalogItem
	for _, i := range s.items {
		if slices.Contains(i.ParentIDs, parent) {
			items = append(items, i)
		}
	}
	return page(items, cursor, limit), nil
}

func (s *Server) ListItemEditions(_ context.Context, item, cursor string, limit int) (api.Page[api.Edition], error) {
	return page([]api.Edition{{ID: item + "-e", ItemID: item, Label: "Original", TimelineIDs: []string{item + "-t"}}}, cursor, limit), nil
}

func (s *Server) ListItemTimelines(_ context.Context, item, cursor string, limit int) (api.Page[api.Timeline], error) {
	return page([]api.Timeline{{ID: item + "-t", ItemID: item, EditionID: item + "-e"}}, cursor, limit), nil
}

func (s *Server) GetTimeline(_ context.Context, id string) (api.Tagged[api.Timeline], error) {
	item, ok := strings.CutSuffix(id, "-t")
	s.mu.Lock()
	defer s.mu.Unlock()
	if !ok || s.item(item) == nil {
		return api.Tagged[api.Timeline]{}, problem(404, "not_found")
	}
	return api.Tagged[api.Timeline]{Value: api.Timeline{ID: id, ItemID: item, EditionID: item + "-e"}, ETag: tag("1")}, nil
}

func (s *Server) ListVersions(_ context.Context, timeline, cursor string, limit int) (api.Page[api.MediaVersion], error) {
	return page([]api.MediaVersion{{ID: timeline + "-v", TimelineID: timeline, Label: "1080p", Origin: "original", Availability: "available"}}, cursor, limit), nil
}

func (s *Server) GetViewingState(_ context.Context, profile, timeline string) (api.ViewingState, error) {
	return api.ViewingState{ProfileID: profile, TimelineID: timeline, Revision: "1", PositionMS: 90000}, nil
}

func (s *Server) ListContinueWatching(_ context.Context, profile, cursor string, limit int) (api.Page[api.ContinueEntry], error) {
	s.mu.Lock()
	defer s.mu.Unlock()
	var entries []api.ContinueEntry
	for _, i := range s.items {
		entries = append(entries, api.ContinueEntry{Item: i, Timeline: api.Timeline{ID: i.ID + "-t", ItemID: i.ID}, Viewing: api.ViewingState{ProfileID: profile, TimelineID: i.ID + "-t", PositionMS: 90000}})
	}
	return page(entries, cursor, limit), nil
}

// Monitoring ----------------------------------------------------------------

func (s *Server) Diagnostics(context.Context) (api.Diagnostics, error) {
	return api.Diagnostics{ServerEpoch: "e1", UptimeSeconds: "60", ActiveDeliveries: "1", QueuedJobs: "0", RunningJobs: "0", DBBusyCount: "0", WorkerErrors: []string{}}, nil
}

func (s *Server) Storage(context.Context) (api.StorageReport, error) {
	free := "1000"
	return api.StorageReport{DatabaseBytes: "10", AssetBytes: "20", TemporaryBytes: "0", ReservedBytes: "5", FreeBytes: &free}, nil
}

// MetricsQueries returns the (since, step) of every metrics read.
func (s *Server) MetricsQueries() []string {
	var out []string
	for _, c := range s.Calls() {
		if c.Op == "metrics" {
			out = append(out, c.JobID)
		}
	}
	return out
}

func (s *Server) Metrics(_ context.Context, since time.Time, step int) (api.MetricsHistory, error) {
	s.mu.Lock()
	defer s.mu.Unlock()
	s.record("metrics", since.UTC().Format(time.RFC3339)+" "+strconv.Itoa(step), "", "")
	return api.MetricsHistory{SampledAt: "2026-10-14T00:00:00Z", IntervalSeconds: 10, RetentionSeconds: 86400,
		Series: []api.MetricSeries{{ID: "http.requests_total", Kind: "counter", Unit: "count", Points: []api.MetricPoint{{T: "2026-10-14T00:00:00Z", V: "5"}}}}}, nil
}

func (s *Server) PlaybackSessions(context.Context) (api.PlaybackSessionList, error) {
	name := "TV"
	return api.PlaybackSessionList{SampledAt: "2026-10-14T00:00:00Z", Sessions: []api.PlaybackSession{{DeliveryID: "del1", Status: "playing",
		Device: api.PlaybackParty{ID: "d1", Name: &name}, Profile: api.PlaybackParty{ID: "p1"}, TimelineID: "t1", Mode: "direct", BytesSent: "1", ThroughputBPS: "8", AverageBPS: "8", PeakBPS: "8"}}}, nil
}

// HistoryFilters returns the filters of every playback history read.
func (s *Server) HistoryFilters() []string {
	var out []string
	for _, c := range s.Calls() {
		if c.Op == "playbackHistory" {
			out = append(out, c.JobID)
		}
	}
	return out
}

func (s *Server) PlaybackHistory(_ context.Context, cursor string, limit int, f api.HistoryFilter) (api.Page[api.PlaybackHistoryEntry], error) {
	s.mu.Lock()
	defer s.mu.Unlock()
	s.record("playbackHistory", f.DeviceID+"|"+f.ProfileID+"|"+f.ItemID, "", "")
	return page([]api.PlaybackHistoryEntry{{DeliveryID: "del0", Device: api.PlaybackParty{ID: f.DeviceID}, EndedAt: "2026-10-14T00:00:00Z", EndReason: "finished"}}, cursor, limit), nil
}
