package opstest

import (
	"context"

	"github.com/Jagalite/motion/apps/terminal/internal/api"
	"github.com/Jagalite/motion/apps/terminal/internal/operations"
)

// ManagementSeed is the starting state of the cross-interface management
// scenario.
type ManagementSeed struct {
	Everyone, Guests api.Profile
	TV, OldPhone     api.Device
	Ana              api.User
	Tablet, Laptop   api.PairingSummary
	Archive          api.Source
	ArchiveLibrary   api.Library
}

// SeedPairing seeds a pending pairing request.
func (s *Server) SeedPairing(deviceName string) api.PairingSummary {
	p, _ := s.CreatePairing(context.Background(), api.PairingRequest{DeviceName: deviceName, ClientName: "fixture"})
	s.mu.Lock()
	defer s.mu.Unlock()
	for _, q := range s.pairings {
		if q.ID == p.ID {
			return q.PairingSummary
		}
	}
	return api.PairingSummary{}
}

// SeedManagement seeds the scenario's starting state.
func SeedManagement(s *Server) ManagementSeed {
	var m ManagementSeed
	m.Everyone, m.Guests = s.AddProfile("Everyone"), s.AddProfile("Guests")
	m.TV, m.OldPhone = s.AddDevice("TV"), s.AddDevice("Old phone")
	m.Ana = s.AddUser("Ana")
	m.Tablet, m.Laptop = s.SeedPairing("Tablet"), s.SeedPairing("Laptop")
	m.Archive = s.AddSource("Archive", "/srv/archive")
	m.ArchiveLibrary = s.AddLibrary("Archive", m.Archive.ID)
	return m
}

// The scenario's inputs, shared by every interface that runs it.
const (
	ScenarioProfile       = "Kids"
	ScenarioRenamed       = "Household"
	ScenarioSource        = "Films"
	ScenarioSourceRoot    = "/srv/films"
	ScenarioExclusion     = "tmp"
	ScenarioLibrary       = "Movies"
	ScenarioLibraryKind   = "movies"
	ScenarioUser          = "Sam"
	ScenarioRating        = "PG"
	ScenarioInvitationTTL = 600
)

// ApplyManagement runs the scenario directly through the shared operations:
// what the CLI and the TUI must each reproduce through their own entry
// points.
//
//  1. create profile Kids; rename Everyone to Household; delete Guests
//  2. register source Films (/srv/films, excluding tmp) and create library
//     Movies (movies) over it; request a verifying scan of Movies
//  3. replace TV's policy: library Archive, catalog:read and
//     playback:request, no unrated content, rating PG; revoke Old phone
//  4. create user Sam (catalog:read, library Archive); deny Tablet; approve
//     Laptop for Ana with catalog:read; invite Sam (catalog:read, library
//     Archive, 600 s), then revoke the invitation
func ApplyManagement(ctx context.Context, s *Server, m ManagementSeed) error {
	steps := []func() error{
		func() error {
			_, err := operations.CreateProfile(ctx, s, ScenarioProfile, operations.NewIntent("profile"))
			return err
		},
		func() error {
			_, err := operations.RenameProfile(ctx, s, m.Everyone.ID, ScenarioRenamed, operations.Reviewed(m.Everyone.Revision))
			return err
		},
		func() error {
			return operations.DeleteProfile(ctx, s, m.Guests.ID, operations.Reviewed(m.Guests.Revision))
		},
		func() error {
			source, err := operations.CreateSource(ctx, s, api.SourceInput{Name: ScenarioSource, RootPath: ScenarioSourceRoot, Exclusions: []string{ScenarioExclusion}}, operations.NewIntent("source"))
			if err != nil {
				return err
			}
			library, err := operations.CreateLibrary(ctx, s, api.LibraryInput{Name: ScenarioLibrary, Kind: ScenarioLibraryKind, SourceIDs: []string{source.Value.ID}}, operations.NewIntent("library"))
			if err != nil {
				return err
			}
			_, err = operations.RequestScan(ctx, s, operations.ScanRequest{LibraryID: library.Value.ID, Verify: true}, operations.NewIntent("scan"))
			return err
		},
		func() error {
			_, err := operations.ReplaceDevicePolicy(ctx, s, m.TV.ID, api.AccessPolicy{LibraryIDs: []string{m.ArchiveLibrary.ID},
				Permissions: []string{api.PermCatalogRead, api.PermPlaybackRequest}, AllowedRatings: []string{ScenarioRating}}, operations.Reviewed("1"))
			return err
		},
		func() error {
			return operations.RevokeDevice(ctx, s, m.OldPhone.ID, operations.Reviewed(m.OldPhone.Revision))
		},
		func() error {
			sam, err := operations.CreateUser(ctx, s, api.UserInput{Name: ScenarioUser, Permissions: []string{api.PermCatalogRead},
				Policy: api.GrantPolicy{LibraryIDs: []string{m.ArchiveLibrary.ID}, AllowUnrated: true}}, operations.NewIntent("user"))
			if err != nil {
				return err
			}
			if err := operations.DenyPairing(ctx, s, m.Tablet.ID, operations.NewIntent("deny")); err != nil {
				return err
			}
			if _, err := operations.ApprovePairing(ctx, s, operations.Approval{PairingID: m.Laptop.ID, UserCode: m.Laptop.UserCode, UserID: m.Ana.ID,
				UserRevision: m.Ana.Revision, Permissions: []string{api.PermCatalogRead}, Policy: &api.GrantPolicy{AllowUnrated: true}}, operations.NewIntent("approve")); err != nil {
				return err
			}
			invitation, err := operations.CreateInvitation(ctx, s, api.InvitationInput{UserID: sam.Value.ID, UserRevision: sam.Value.Revision,
				Permissions: []string{api.PermCatalogRead}, Policy: api.GrantPolicy{LibraryIDs: []string{m.ArchiveLibrary.ID}, AllowUnrated: true},
				TTLSeconds: ScenarioInvitationTTL}, operations.NewIntent("invite"))
			if err != nil {
				return err
			}
			return operations.RevokeInvitation(ctx, s, invitation.Value.ID, operations.Reviewed(invitation.Value.Revision))
		},
	}
	for _, step := range steps {
		if err := step(); err != nil {
			return err
		}
	}
	return nil
}

// ExpectedManagement is the state the scenario reaches, computed by
// running it directly on a fresh fixture.
func ExpectedManagement(ctx context.Context) (Snapshot, error) {
	s := NewServer("expected", "expected-token-0123456789abcdef01234")
	if err := ApplyManagement(ctx, s, SeedManagement(s)); err != nil {
		return Snapshot{}, err
	}
	return s.Snapshot(), nil
}
