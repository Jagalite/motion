// Package api is a typed client for the Motion public server API v2.
//
// Wire shapes follow contracts/Motion_Server_API_v2.yaml. Revisions and
// cursors are opaque strings; clients never construct ETags or parse IDs.
package api

// Permission names as spelled by the contract.
const (
	PermCatalogRead       = "catalog:read"
	PermCatalogWrite      = "catalog:write"
	PermSourcesManage     = "sources:manage"
	PermProfilesManage    = "profiles:manage"
	PermViewingWrite      = "viewing:write"
	PermPlaybackRequest   = "playback:request"
	PermProcessingRequest = "processing:request"
	PermDownloadsManage   = "downloads:manage"
	PermCollectionsWrite  = "collections:write"
	PermEventsRead        = "events:read"
	PermSystemAdmin       = "system:admin"
)

// AllPermissions lists every permission the contract defines.
var AllPermissions = []string{
	PermCatalogRead, PermCatalogWrite, PermSourcesManage, PermProfilesManage,
	PermViewingWrite, PermPlaybackRequest, PermProcessingRequest,
	PermDownloadsManage, PermCollectionsWrite, PermEventsRead, PermSystemAdmin,
}

type Health struct {
	Status      string `json:"status"`
	ServerID    string `json:"server_id"`
	ServerEpoch string `json:"server_epoch"`
}

type Feature struct {
	ID            string            `json:"id"`
	Implemented   bool              `json:"implemented"`
	Enabled       bool              `json:"enabled"`
	Qualification string            `json:"qualification"`
	Limits        map[string]string `json:"limits"`
	ReceiptIDs    []string          `json:"receipt_ids"`
}

type Capabilities struct {
	ServerID       string            `json:"server_id"`
	ServerEpoch    string            `json:"server_epoch"`
	ServerVersion  string            `json:"server_version"`
	APIVersion     string            `json:"api_version"`
	SchemaVersion  string            `json:"schema_version"`
	ContractDigest string            `json:"contract_digest"`
	Features       []Feature         `json:"features"`
	RuntimeHashes  map[string]string `json:"runtime_hashes"`
}

// Feature returns the named feature, if the server reports it.
func (c Capabilities) Feature(id string) (Feature, bool) {
	for _, f := range c.Features {
		if f.ID == id {
			return f, true
		}
	}
	return Feature{}, false
}

type Principal struct {
	ID             string   `json:"id"`
	DeviceID       *string  `json:"device_id"`
	ProfileIDs     []string `json:"profile_ids"`
	Permissions    []string `json:"permissions"`
	PolicyRevision string   `json:"policy_revision"`
	Mode           string   `json:"mode"`
}

// Allows reports whether the principal holds the permission.
func (p Principal) Allows(permission string) bool {
	for _, held := range p.Permissions {
		if held == permission || held == PermSystemAdmin {
			return true
		}
	}
	return false
}

type PairingRequest struct {
	DeviceName string `json:"device_name"`
	ClientName string `json:"client_name"`
}

// Pairing.DeviceCode is a secret; never print or log it.
type Pairing struct {
	ID                  string `json:"id"`
	DeviceCode          string `json:"device_code"`
	UserCode            string `json:"user_code"`
	ExpiresAt           string `json:"expires_at"`
	PollIntervalSeconds int    `json:"poll_interval_seconds"`
}

type PairingApproval struct {
	UserCode    string   `json:"user_code"`
	ProfileIDs  []string `json:"profile_ids"`
	Permissions []string `json:"permissions"`
}

// Credential.AccessToken is a secret; never print or log it.
type Credential struct {
	DeviceID    string `json:"device_id"`
	AccessToken string `json:"access_token"`
	ExpiresAt   string `json:"expires_at"`
}

type Device struct {
	ID          string   `json:"id"`
	Name        string   `json:"name"`
	Revision    string   `json:"revision"`
	ProfileIDs  []string `json:"profile_ids"`
	Permissions []string `json:"permissions"`
	Revoked     bool     `json:"revoked"`
}

type AccessPolicy struct {
	Revision       string   `json:"revision,omitempty"`
	LibraryIDs     []string `json:"library_ids"`
	AllowUnrated   bool     `json:"allow_unrated"`
	AllowedRatings []string `json:"allowed_ratings"`
	BlockedLabels  []string `json:"blocked_labels"`
	Permissions    []string `json:"permissions"`
}

type Profile struct {
	ID       string `json:"id"`
	Name     string `json:"name"`
	Revision string `json:"revision"`
}

type Library struct {
	ID           string   `json:"id"`
	Revision     string   `json:"revision"`
	Name         string   `json:"name"`
	Kind         string   `json:"kind"`
	Language     string   `json:"language"`
	Sources      []string `json:"source_ids"`
	Availability string   `json:"availability"`
}

// Page is one committed read snapshot. EventCursor resumes events from it.
type Page[T any] struct {
	Items        []T     `json:"items"`
	NextCursor   *string `json:"next_cursor"`
	ReadRevision string  `json:"read_revision"`
	EventCursor  string  `json:"event_cursor"`
}

// Event is an authorized invalidation hint, not a state document.
type Event struct {
	Cursor           string  `json:"cursor"`
	Kind             string  `json:"kind"`
	ResourceType     string  `json:"resource_type"`
	ResourceID       *string `json:"resource_id"`
	ResourceRevision *string `json:"resource_revision"`
	PolicyRevision   string  `json:"policy_revision"`
	Reason           *string `json:"reason"`
}

// Tagged pairs a resource with the strong ETag to use in If-Match.
type Tagged[T any] struct {
	Value T
	ETag  string
}
