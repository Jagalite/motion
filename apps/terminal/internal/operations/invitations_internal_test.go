package operations

import (
	"context"
	"errors"
	"testing"

	"github.com/Jagalite/motion/apps/terminal/internal/api"
)

type invitationList struct{ n int }

func (l invitationList) ListInvitations(_ context.Context, cursor string, limit int) (api.Page[api.Invitation], error) {
	next := "more"
	return api.Page[api.Invitation]{Items: make([]api.Invitation, limit), NextCursor: &next}, nil
}
func (invitationList) CreateInvitation(context.Context, api.InvitationInput, string) (api.Tagged[api.Invitation], error) {
	return api.Tagged[api.Invitation]{}, nil
}
func (invitationList) RevokeInvitation(context.Context, string, string) error {
	return errors.New("revoked without a precondition")
}

func TestATruncatedInvitationListIsNotEvidenceOfAbsence(t *testing.T) {
	defer func(saved int) { maxInvitationScan = saved }(maxInvitationScan)
	maxInvitationScan = 300
	err := RevokeInvitation(context.Background(), invitationList{}, "beyond", Current())
	var invalid *InvalidInputError
	if !errors.As(err, &invalid) || api.IsCode(err, "not_found") {
		t.Fatalf("truncated search reported %v", err)
	}
}
