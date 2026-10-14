package opstest

import (
	"bytes"
	"context"
	"encoding/json"
	"fmt"
	"io"
	"net/http"
	"path/filepath"
	"slices"
	"sort"
	"strings"

	"github.com/Jagalite/motion/apps/terminal/internal/api"
	"github.com/Jagalite/motion/apps/terminal/internal/operations"
)

// The real-server qualification scenario, run through each interface
// against a fresh isolated server:
//
//  1. create profile Kids, rename it to Children; create profile Temp and
//     delete it
//  2. register source Films (a temporary directory named "films") and
//     create library Movies (movies) over it; request a scan of Movies
//  3. create access user Sam (catalog:read); issue Sam an invitation and
//     revoke it; issue another and redeem it as device Redeemer
//  4. assign the unassigned device Den TV (approved without a user, as an
//     older client could; see SeedUnassignedDevice) to Sam
const (
	RealProfile        = "Kids"
	RealRenamed        = "Children"
	RealTemporary      = "Temp"
	RealSource         = "Films"
	RealSourceDir      = "films"
	RealLibrary        = "Movies"
	RealLibraryKind    = "movies"
	RealUser           = "Sam"
	RealInvitationTTL  = 600
	RealUserPermission = api.PermCatalogRead
	RealDevice         = "Redeemer"
	RealUnassigned     = "Den TV"
)

// Observation is the scenario's resulting domain state, read through the
// shared operations and limited to the scenario's own names, without
// generated identifiers or host paths.
type Observation struct {
	Profiles    []string
	Sources     []string
	Libraries   []string
	Scans       []string
	Users       []string
	Invitations []string
	Devices     []string
}

// ExpectedObservation is what both interfaces must leave behind.
var ExpectedObservation = Observation{
	Profiles:    []string{RealRenamed},
	Sources:     []string{RealSource + " " + RealSourceDir},
	Libraries:   []string{RealLibrary + " " + RealLibraryKind + " sources=" + RealSource},
	Scans:       []string{RealLibrary},
	Users:       []string{RealUser + " enabled perms=" + RealUserPermission},
	Invitations: []string{RealUser + " " + RedeemedStatus, RealUser + " revoked"}, // sorted
	Devices: []string{RealUnassigned + " owner=" + RealUser + " permissions=" + RealUserPermission,
		RealDevice + " owner=" + RealUser + " permissions=" + RealUserPermission}, // sorted
}

// RedeemedStatus is how the server lists a redeemed invitation.
const RedeemedStatus = "claimed"

// Observe reads the scenario's state from a server.
func Observe(ctx context.Context, s *operations.Session) (Observation, error) {
	var o Observation
	scenario := []string{RealProfile, RealRenamed, RealTemporary}
	profiles, _, err := operations.CollectProfiles(ctx, s, 10000)
	if err != nil {
		return o, err
	}
	for _, p := range profiles {
		if slices.Contains(scenario, p.Name) {
			o.Profiles = append(o.Profiles, p.Name)
		}
	}
	sources, _, err := operations.CollectSources(ctx, s, 10000)
	if err != nil {
		return o, err
	}
	sourceName := map[string]string{}
	for _, v := range sources {
		sourceName[v.ID] = v.Name
		if v.Name == RealSource {
			o.Sources = append(o.Sources, v.Name+" "+filepath.Base(v.RootPath))
		}
	}
	libraries, _, err := operations.CollectLibraries(ctx, s, 10000)
	if err != nil {
		return o, err
	}
	libraryName := map[string]string{}
	for _, l := range libraries {
		libraryName[l.ID] = l.Name
		if l.Name == RealLibrary {
			var names []string
			for _, id := range l.Sources {
				names = append(names, sourceName[id])
			}
			sort.Strings(names)
			o.Libraries = append(o.Libraries, fmt.Sprintf("%s %s sources=%s", l.Name, l.Kind, strings.Join(names, "+")))
		}
	}
	scans, _, err := operations.CollectScans(ctx, s, 10000)
	if err != nil {
		return o, err
	}
	for _, v := range scans {
		if libraryName[v.LibraryID] == RealLibrary {
			o.Scans = append(o.Scans, RealLibrary)
		}
	}
	users, _, err := operations.CollectUsers(ctx, s, 10000)
	if err != nil {
		return o, err
	}
	userName := map[string]string{}
	for _, u := range users {
		userName[u.ID] = u.Name
		if u.Name == RealUser {
			state := "enabled"
			if u.Disabled {
				state = "disabled"
			}
			o.Users = append(o.Users, fmt.Sprintf("%s %s perms=%s", u.Name, state, strings.Join(u.Permissions, "+")))
		}
	}
	invitations, _, err := operations.CollectInvitations(ctx, s, 10000)
	if err != nil {
		return o, err
	}
	for _, i := range invitations {
		if userName[i.UserID] == RealUser {
			o.Invitations = append(o.Invitations, RealUser+" "+i.Status)
		}
	}
	sort.Strings(o.Invitations)
	devices, _, err := operations.CollectDevices(ctx, s, 10000)
	if err != nil {
		return o, err
	}
	for _, d := range devices {
		if d.Name == RealDevice || d.Name == RealUnassigned {
			owner := "-"
			if d.UserID != nil {
				owner = userName[*d.UserID]
			}
			o.Devices = append(o.Devices, fmt.Sprintf("%s owner=%s permissions=%s", d.Name, owner, strings.Join(d.Permissions, "+")))
		}
	}
	sort.Strings(o.Devices)
	return o, nil
}

// SeedUnassignedDevice creates a device without an access user on a real
// server, as an older pairing client's approval would, using the API
// directly: neither interface approves without a user any more.
func SeedUnassignedDevice(ctx context.Context, url, operatorToken, name string) (string, error) {
	call := func(path, auth, key string, body any, out any) error {
		data, _ := json.Marshal(body)
		req, err := http.NewRequestWithContext(ctx, http.MethodPost, url+path, bytes.NewReader(data))
		if err != nil {
			return err
		}
		req.Header.Set("Content-Type", "application/json")
		if auth != "" {
			req.Header.Set("Authorization", "Bearer "+auth)
			req.Header.Set("Idempotency-Key", key)
		}
		resp, err := http.DefaultClient.Do(req)
		if err != nil {
			return err
		}
		defer resp.Body.Close()
		if resp.StatusCode >= 300 {
			msg, _ := io.ReadAll(resp.Body)
			return fmt.Errorf("%s: %d %s", path, resp.StatusCode, msg)
		}
		return json.NewDecoder(resp.Body).Decode(out)
	}
	var pairing api.Pairing
	if err := call("/api/v2/auth/pairings", "", "", api.PairingRequest{DeviceName: name, ClientName: "legacy"}, &pairing); err != nil {
		return "", err
	}
	var device api.Device
	err := call("/api/v2/auth/pairings/"+pairing.ID+"/approve", operatorToken, "seed-unassigned-device-01",
		api.PairingApproval{UserCode: pairing.UserCode, ProfileIDs: []string{}, Permissions: []string{api.PermCatalogRead}}, &device)
	return device.ID, err
}
