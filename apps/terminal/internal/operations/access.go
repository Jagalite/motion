package operations

import (
	"context"
	"slices"
	"strings"

	"github.com/Jagalite/motion/apps/terminal/internal/api"
)

// CheckPermissions rejects permissions the contract does not define.
func CheckPermissions(permissions []string) error {
	for _, p := range permissions {
		if !slices.Contains(api.AllPermissions, p) {
			return Invalid("unknown permission %q; valid: %s", p, strings.Join(api.AllPermissions, ", "))
		}
	}
	return nil
}

// canonicalPermissions orders permissions as the contract lists them and
// drops duplicates, so every interface sends the same request for the same
// grant.
func canonicalPermissions(permissions []string) []string {
	out := []string{}
	for _, p := range api.AllPermissions {
		if slices.Contains(permissions, p) {
			out = append(out, p)
		}
	}
	return out
}

// GrantRequest is what a resubmission of a user-bound approval or an
// invitation must repeat exactly: the user revision and the grant.
type GrantRequest struct {
	UserRevision string          `json:"user_revision"`
	ProfileIDs   []string        `json:"profile_ids"`
	Permissions  []string        `json:"permissions"`
	Policy       api.GrantPolicy `json:"policy"`
	TTLSeconds   int64           `json:"ttl_seconds,omitempty"`
}

// nonNil sends an empty JSON list rather than null.
func nonNil(v []string) []string {
	if v == nil {
		return []string{}
	}
	return v
}

func grant(p api.GrantPolicy) api.GrantPolicy {
	p.LibraryIDs, p.AllowedRatings, p.BlockedLabels = nonNil(p.LibraryIDs), nonNil(p.AllowedRatings), nonNil(p.BlockedLabels)
	return p
}

// CheckGrantPolicy applies the client-side rule shared by every form that
// edits a grant: denying all rated content leaves no rating allowlist.
func CheckGrantPolicy(p api.GrantPolicy) error {
	if p.DenyRated && len(p.AllowedRatings) != 0 {
		return Invalid("clear the rating allowlist when denying all rated content")
	}
	return nil
}

// DefaultUserPermissions are what a new access user is granted unless the
// administrator chooses otherwise, in either interface.
var DefaultUserPermissions = canonicalPermissions([]string{api.PermCatalogRead, api.PermEventsRead, api.PermPlaybackRequest, api.PermViewingWrite})

// DefaultGrantPolicy is the grant policy a new user starts with.
func DefaultGrantPolicy() api.GrantPolicy {
	return api.GrantPolicy{LibraryIDs: []string{}, AllowUnrated: true, AllowedRatings: []string{}, BlockedLabels: []string{}}
}

// Grant is the access a device or invitation receives.
type Grant struct {
	ProfileIDs  []string
	Permissions []string
	Policy      api.GrantPolicy
}

// InheritedGrant is the default grant for a device or invitation bound to
// a user: the user's grant, without system:admin, which is never granted
// implicitly.
func InheritedGrant(u api.User) Grant {
	g := Grant{ProfileIDs: append([]string{}, u.ProfileIDs...), Policy: grant(u.Policy)}
	for _, p := range u.Permissions {
		if p != api.PermSystemAdmin {
			g.Permissions = append(g.Permissions, p)
		}
	}
	g.Permissions = canonicalPermissions(g.Permissions)
	return g
}

// Pairings ------------------------------------------------------------------

// PairingAPI administers pairing requests.
type PairingAPI interface {
	ListPairings(context.Context, string, int) (api.Page[api.PairingSummary], error)
	ApprovePairing(context.Context, string, api.PairingApproval, string) (api.Tagged[api.Device], error)
	DenyPairing(context.Context, string, string) error
}

// ListPairings reads one page of pairing requests.
func ListPairings(ctx context.Context, a PairingAPI, page PageRequest) (api.Page[api.PairingSummary], error) {
	if err := page.validate(); err != nil {
		return api.Page[api.PairingSummary]{}, err
	}
	return a.ListPairings(ctx, page.Cursor, page.Limit)
}

// CollectPairings reads pairing requests until the list ends or max arrive.
func CollectPairings(ctx context.Context, a PairingAPI, max int) ([]api.PairingSummary, bool, error) {
	return Collect(ctx, a.ListPairings, max)
}

// Approval is the access an administrator grants a pairing device. UserCode
// is the code compared with the requesting device.
type Approval struct {
	PairingID    string
	UserCode     string
	UserID       string
	UserRevision string
	ProfileIDs   []string
	Permissions  []string
	Policy       *api.GrantPolicy
}

// ApprovePairing approves a pairing request with exactly the given access.
func ApprovePairing(ctx context.Context, a PairingAPI, in Approval, i Intent) (api.Tagged[api.Device], error) {
	if strings.TrimSpace(in.PairingID) == "" || strings.TrimSpace(in.UserCode) == "" {
		return api.Tagged[api.Device]{}, Invalid("a pairing and the code shown on the pairing device are required")
	}
	if err := CheckPermissions(in.Permissions); err != nil {
		return api.Tagged[api.Device]{}, err
	}
	approval := api.PairingApproval{UserCode: in.UserCode, UserID: in.UserID, UserRevision: in.UserRevision,
		ProfileIDs: nonNil(in.ProfileIDs), Permissions: canonicalPermissions(in.Permissions)}
	if in.Policy != nil {
		if err := CheckGrantPolicy(*in.Policy); err != nil {
			return api.Tagged[api.Device]{}, err
		}
		p := grant(*in.Policy)
		approval.Policy = &p
	}
	v, err := a.ApprovePairing(ctx, in.PairingID, approval, i.Key)
	if in.UserID != "" && approval.Policy != nil {
		return v, i.settleRequest(&GrantRequest{UserRevision: in.UserRevision, ProfileIDs: approval.ProfileIDs,
			Permissions: approval.Permissions, Policy: *approval.Policy}, err)
	}
	return v, i.settle(err)
}

// DenyPairing refuses a pairing request.
func DenyPairing(ctx context.Context, a PairingAPI, id string, i Intent) error {
	if strings.TrimSpace(id) == "" {
		return Invalid("a pairing ID is required")
	}
	return i.settle(a.DenyPairing(ctx, id, i.Key))
}

// Users ---------------------------------------------------------------------

// UserAPI administers access users.
type UserAPI interface {
	ListUsers(context.Context, string, int) (api.Page[api.User], error)
	GetUser(context.Context, string) (api.Tagged[api.User], error)
	CreateUser(context.Context, api.UserInput, string) (api.Tagged[api.User], error)
	ReplaceUser(context.Context, string, api.UserInput, string) (api.Tagged[api.User], error)
}

// ListUsers reads one page of access users.
func ListUsers(ctx context.Context, a UserAPI, page PageRequest) (api.Page[api.User], error) {
	if err := page.validate(); err != nil {
		return api.Page[api.User]{}, err
	}
	return a.ListUsers(ctx, page.Cursor, page.Limit)
}

// CollectUsers reads access users until the list ends or max arrive.
func CollectUsers(ctx context.Context, a UserAPI, max int) ([]api.User, bool, error) {
	return Collect(ctx, a.ListUsers, max)
}

// InspectUser reads one access user.
func InspectUser(ctx context.Context, a UserAPI, id string) (api.Tagged[api.User], error) {
	if strings.TrimSpace(id) == "" {
		return api.Tagged[api.User]{}, Invalid("a user ID is required")
	}
	return a.GetUser(ctx, id)
}

func checkUser(in api.UserInput) (api.UserInput, error) {
	if strings.TrimSpace(in.Name) == "" {
		return in, Invalid("a user name is required")
	}
	if err := CheckPermissions(in.Permissions); err != nil {
		return in, err
	}
	if err := CheckGrantPolicy(in.Policy); err != nil {
		return in, err
	}
	in.ProfileIDs, in.Permissions, in.Policy = nonNil(in.ProfileIDs), canonicalPermissions(in.Permissions), grant(in.Policy)
	return in, nil
}

// CreateUser creates an access user.
func CreateUser(ctx context.Context, a UserAPI, in api.UserInput, i Intent) (api.Tagged[api.User], error) {
	in, err := checkUser(in)
	if err != nil {
		return api.Tagged[api.User]{}, err
	}
	v, err := a.CreateUser(ctx, in, i.Key)
	return v, i.settle(err)
}

// ReplaceUser replaces a user's name, grants and enabled state.
func ReplaceUser(ctx context.Context, a UserAPI, id string, in api.UserInput, pre Precondition) (api.Tagged[api.User], error) {
	in, err := checkUser(in)
	if err != nil {
		return api.Tagged[api.User]{}, err
	}
	etag, err := pre.resolve(ctx, "the user", tagged(func(ctx context.Context) (api.Tagged[api.User], error) {
		return InspectUser(ctx, a, id)
	}, func(u api.User) string { return u.Revision }))
	if err != nil {
		return api.Tagged[api.User]{}, err
	}
	v, err := a.ReplaceUser(ctx, id, in, etag)
	return v, settleConditional("update of user "+id, etag, err)
}

// Invitations ---------------------------------------------------------------

// InvitationAPI administers invitations.
type InvitationAPI interface {
	ListInvitations(context.Context, string, int) (api.Page[api.Invitation], error)
	CreateInvitation(context.Context, api.InvitationInput, string) (api.Tagged[api.Invitation], error)
	RevokeInvitation(context.Context, string, string) error
}

// ListInvitations reads one page of invitations.
func ListInvitations(ctx context.Context, a InvitationAPI, page PageRequest) (api.Page[api.Invitation], error) {
	if err := page.validate(); err != nil {
		return api.Page[api.Invitation]{}, err
	}
	return a.ListInvitations(ctx, page.Cursor, page.Limit)
}

// CollectInvitations reads invitations until the list ends or max arrive.
func CollectInvitations(ctx context.Context, a InvitationAPI, max int) ([]api.Invitation, bool, error) {
	return Collect(ctx, a.ListInvitations, max)
}

// CreateInvitation issues a one-use invitation for a user. The result's
// Code is a bearer secret shown once.
func CreateInvitation(ctx context.Context, a InvitationAPI, in api.InvitationInput, i Intent) (api.Tagged[api.Invitation], error) {
	if strings.TrimSpace(in.UserID) == "" || strings.TrimSpace(in.UserRevision) == "" {
		return api.Tagged[api.Invitation]{}, Invalid("an invitation names a user and the user revision that was reviewed")
	}
	if in.TTLSeconds < 60 || in.TTLSeconds > 86400 {
		return api.Tagged[api.Invitation]{}, Invalid("an invitation lifetime is 60 to 86400 seconds")
	}
	if err := CheckPermissions(in.Permissions); err != nil {
		return api.Tagged[api.Invitation]{}, err
	}
	if err := CheckGrantPolicy(in.Policy); err != nil {
		return api.Tagged[api.Invitation]{}, err
	}
	in.ProfileIDs, in.Permissions, in.Policy = nonNil(in.ProfileIDs), canonicalPermissions(in.Permissions), grant(in.Policy)
	v, err := a.CreateInvitation(ctx, in, i.Key)
	return v, i.settleRequest(&GrantRequest{UserRevision: in.UserRevision, ProfileIDs: in.ProfileIDs, Permissions: in.Permissions,
		Policy: in.Policy, TTLSeconds: in.TTLSeconds}, err)
}

// maxInvitationScan bounds the list read that resolves an invitation's
// revision.
var maxInvitationScan = 10000

// revisionETag is the server's strong ETag for a revision. It is used only
// for invitations, which the contract lists but offers no single read for.
func revisionETag(revision string) string { return `"r-` + revision + `"` }

// RevokeInvitation revokes an unused invitation. Reviewed and Current
// preconditions are resolved from the invitation list.
func RevokeInvitation(ctx context.Context, a InvitationAPI, id string, pre Precondition) error {
	if strings.TrimSpace(id) == "" {
		return Invalid("an invitation ID is required")
	}
	etag, err := pre.resolve(ctx, "the invitation", func(ctx context.Context) (string, string, error) {
		all, truncated, err := CollectInvitations(ctx, a, maxInvitationScan)
		if err != nil {
			return "", "", err
		}
		for _, v := range all {
			if v.ID == id {
				return revisionETag(v.Revision), v.Revision, nil
			}
		}
		if truncated {
			// A partial list is not evidence of absence.
			return "", "", Invalid("the invitation is not among the first %d listed; pass its ETag explicitly", len(all))
		}
		return "", "", &api.Problem{Status: 404, Code: "not_found", Title: "Not Found", Detail: "no such invitation"}
	})
	if err != nil {
		return err
	}
	return settleConditional("revocation of invitation "+id, etag, a.RevokeInvitation(ctx, id, etag))
}

// Devices -------------------------------------------------------------------

// DeviceAPI administers paired devices and their policies.
type DeviceAPI interface {
	ListDevices(context.Context, string, int) (api.Page[api.Device], error)
	GetDevice(context.Context, string) (api.Tagged[api.Device], error)
	RevokeDevice(context.Context, string, string) error
	GetDevicePolicy(context.Context, string) (api.Tagged[api.AccessPolicy], error)
	ReplaceDevicePolicy(context.Context, string, api.AccessPolicy, string) (api.Tagged[api.AccessPolicy], error)
}

// DeviceReader lists and reads devices.
type DeviceReader interface {
	ListDevices(context.Context, string, int) (api.Page[api.Device], error)
	GetDevice(context.Context, string) (api.Tagged[api.Device], error)
}

// ListDevices reads one page of devices.
func ListDevices(ctx context.Context, a DeviceReader, page PageRequest) (api.Page[api.Device], error) {
	if err := page.validate(); err != nil {
		return api.Page[api.Device]{}, err
	}
	return a.ListDevices(ctx, page.Cursor, page.Limit)
}

// CollectDevices reads devices until the list ends or max arrive.
func CollectDevices(ctx context.Context, a DeviceReader, max int) ([]api.Device, bool, error) {
	return Collect(ctx, a.ListDevices, max)
}

// InspectDevice reads one device.
func InspectDevice(ctx context.Context, a DeviceReader, id string) (api.Tagged[api.Device], error) {
	if strings.TrimSpace(id) == "" {
		return api.Tagged[api.Device]{}, Invalid("a device ID is required")
	}
	return a.GetDevice(ctx, id)
}

// RevokeDevice revokes a device's credentials.
func RevokeDevice(ctx context.Context, a interface {
	DeviceReader
	RevokeDevice(context.Context, string, string) error
}, id string, pre Precondition) error {
	etag, err := pre.resolve(ctx, "the device", tagged(func(ctx context.Context) (api.Tagged[api.Device], error) {
		return InspectDevice(ctx, a, id)
	}, func(d api.Device) string { return d.Revision }))
	if err != nil {
		return err
	}
	return settleConditional("revocation of device "+id, etag, a.RevokeDevice(ctx, id, etag))
}

// InspectDevicePolicy reads a device's access policy.
func InspectDevicePolicy(ctx context.Context, a DeviceAPI, id string) (api.Tagged[api.AccessPolicy], error) {
	if strings.TrimSpace(id) == "" {
		return api.Tagged[api.AccessPolicy]{}, Invalid("a device ID is required")
	}
	return a.GetDevicePolicy(ctx, id)
}

// ReplaceDevicePolicy replaces a device's whole access policy.
func ReplaceDevicePolicy(ctx context.Context, a DeviceAPI, id string, in api.AccessPolicy, pre Precondition) (api.Tagged[api.AccessPolicy], error) {
	if err := CheckPermissions(in.Permissions); err != nil {
		return api.Tagged[api.AccessPolicy]{}, err
	}
	if in.DenyRated && len(in.AllowedRatings) != 0 {
		return api.Tagged[api.AccessPolicy]{}, Invalid("clear the rating allowlist when denying all rated content")
	}
	in.LibraryIDs, in.AllowedRatings = nonNil(in.LibraryIDs), nonNil(in.AllowedRatings)
	in.BlockedLabels, in.Permissions = nonNil(in.BlockedLabels), canonicalPermissions(in.Permissions)
	etag, err := pre.resolve(ctx, "the device policy", tagged(func(ctx context.Context) (api.Tagged[api.AccessPolicy], error) {
		return InspectDevicePolicy(ctx, a, id)
	}, func(p api.AccessPolicy) string { return p.Revision }))
	if err != nil {
		return api.Tagged[api.AccessPolicy]{}, err
	}
	v, err := a.ReplaceDevicePolicy(ctx, id, in, etag)
	return v, settleConditional("policy replacement for device "+id, etag, err)
}
