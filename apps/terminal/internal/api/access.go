package api

import (
	"context"
	"net/http"
)

// User is an access identity, distinct from a viewing profile and a device.
type User struct {
	ID          string      `json:"id"`
	Name        string      `json:"name"`
	Revision    string      `json:"revision"`
	Disabled    bool        `json:"disabled"`
	ProfileIDs  []string    `json:"profile_ids"`
	Permissions []string    `json:"permissions"`
	Policy      GrantPolicy `json:"policy"`
}
type GrantPolicy struct {
	LibraryIDs     []string `json:"library_ids"`
	AllowUnrated   bool     `json:"allow_unrated"`
	AllowedRatings []string `json:"allowed_ratings"`
	DenyRated      bool     `json:"deny_rated"`
	BlockedLabels  []string `json:"blocked_labels"`
}
type UserInput struct {
	Name        string      `json:"name"`
	Disabled    bool        `json:"disabled"`
	ProfileIDs  []string    `json:"profile_ids"`
	Permissions []string    `json:"permissions"`
	Policy      GrantPolicy `json:"policy"`
}
type PairingSummary struct {
	ID         string `json:"id"`
	DeviceName string `json:"device_name"`
	ClientName string `json:"client_name"`
	UserCode   string `json:"user_code"`
	CreatedAt  string `json:"created_at"`
	ExpiresAt  string `json:"expires_at"`
	Status     string `json:"status"`
}
type Invitation struct {
	ID          string      `json:"id"`
	UserID      string      `json:"user_id"`
	Revision    string      `json:"revision"`
	CreatedAt   string      `json:"created_at"`
	ExpiresAt   string      `json:"expires_at"`
	Status      string      `json:"status"`
	ProfileIDs  []string    `json:"profile_ids"`
	Permissions []string    `json:"permissions"`
	Policy      GrantPolicy `json:"policy"`
	// Code is a bearer secret, shown only in the issuance dialog; never log it.
	Code string `json:"code,omitempty"`
}
type InvitationInput struct {
	UserID       string      `json:"user_id"`
	UserRevision string      `json:"user_revision"`
	ProfileIDs   []string    `json:"profile_ids"`
	Permissions  []string    `json:"permissions"`
	Policy       GrantPolicy `json:"policy"`
	TTLSeconds   int64       `json:"ttl_seconds"`
}
type RedemptionInput struct {
	Code       string `json:"code"`
	ClaimID    string `json:"claim_id"`
	DeviceName string `json:"device_name"`
	ClientName string `json:"client_name"`
}

func (c *Client) ListUsers(ctx context.Context, cursor string, limit int) (Page[User], error) {
	var v Page[User]
	_, err := c.do(ctx, request{method: http.MethodGet, path: "/api/v2/users", query: pageQuery(cursor, limit), auth: true}, &v)
	return v, err
}
func (c *Client) GetUser(ctx context.Context, id string) (Tagged[User], error) {
	var v User
	h, err := c.do(ctx, request{method: http.MethodGet, path: "/api/v2/users/" + esc(id), auth: true}, &v)
	return Tagged[User]{Value: v, ETag: h.Get("ETag")}, err
}
func (c *Client) CreateUser(ctx context.Context, input UserInput, key string) (Tagged[User], error) {
	var v User
	h, err := c.do(ctx, request{method: http.MethodPost, path: "/api/v2/users", body: input, auth: true, idempotencyKey: key}, &v)
	return Tagged[User]{Value: v, ETag: h.Get("ETag")}, err
}
func (c *Client) ReplaceUser(ctx context.Context, id string, input UserInput, etag string) (Tagged[User], error) {
	var v User
	h, err := c.do(ctx, request{method: http.MethodPut, path: "/api/v2/users/" + esc(id), body: input, auth: true, ifMatch: etag}, &v)
	return Tagged[User]{Value: v, ETag: h.Get("ETag")}, err
}
func (c *Client) ListPairings(ctx context.Context, cursor string, limit int) (Page[PairingSummary], error) {
	var v Page[PairingSummary]
	_, err := c.do(ctx, request{method: http.MethodGet, path: "/api/v2/auth/pairings", query: pageQuery(cursor, limit), auth: true}, &v)
	return v, err
}
func (c *Client) DenyPairing(ctx context.Context, id, key string) error {
	_, err := c.do(ctx, request{method: http.MethodPost, path: "/api/v2/auth/pairings/" + esc(id) + "/deny", auth: true, idempotencyKey: key}, nil)
	return err
}
func (c *Client) ListInvitations(ctx context.Context, cursor string, limit int) (Page[Invitation], error) {
	var v Page[Invitation]
	_, err := c.do(ctx, request{method: http.MethodGet, path: "/api/v2/auth/invitations", query: pageQuery(cursor, limit), auth: true}, &v)
	return v, err
}
func (c *Client) CreateInvitation(ctx context.Context, input InvitationInput, key string) (Tagged[Invitation], error) {
	var v Invitation
	h, err := c.do(ctx, request{method: http.MethodPost, path: "/api/v2/auth/invitations", body: input, auth: true, idempotencyKey: key}, &v)
	return Tagged[Invitation]{Value: v, ETag: h.Get("ETag")}, err
}
func (c *Client) RevokeInvitation(ctx context.Context, id, etag string) error {
	_, err := c.do(ctx, request{method: http.MethodDelete, path: "/api/v2/auth/invitations/" + esc(id), auth: true, ifMatch: etag}, nil)
	return err
}
func (c *Client) RedeemInvitation(ctx context.Context, input RedemptionInput) (Credential, error) {
	var v Credential
	_, err := c.do(ctx, request{method: http.MethodPost, path: "/api/v2/auth/invitations/redeem", body: input}, &v)
	return v, err
}

// Owner setup states reported by the server.
const (
	SetupRequired      = "required"
	SetupComplete      = "complete"
	SetupExistingUsers = "existing_users"
)

// AccessSetup is first-run owner setup and the user suggested for pairing
// approval: the only enabled user, or nil when none or several are enabled.
type AccessSetup struct {
	OwnerSetup    string `json:"owner_setup"`
	Owner         *User  `json:"owner"`
	PairingUser   *User  `json:"pairing_user"`
	EligibleUsers int    `json:"eligible_users"`
}
type SetupInput struct {
	Name string `json:"name,omitempty"`
}

func (c *Client) GetAccessSetup(ctx context.Context) (AccessSetup, error) {
	var v AccessSetup
	_, err := c.do(ctx, request{method: http.MethodGet, path: "/api/v2/access/setup", auth: true}, &v)
	return v, err
}

// SetupOwner is naturally idempotent: a repeat returns the recorded owner,
// so it is retried like a keyed write.
func (c *Client) SetupOwner(ctx context.Context, in SetupInput) (AccessSetup, error) {
	var v AccessSetup
	_, err := c.do(ctx, request{method: http.MethodPost, path: "/api/v2/access/setup", body: in, auth: true, repeatable: true}, &v)
	return v, err
}
