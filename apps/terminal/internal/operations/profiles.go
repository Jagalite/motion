package operations

import (
	"context"
	"strings"

	"github.com/Jagalite/motion/apps/terminal/internal/api"
)

// ProfileReader lists and reads viewing profiles.
type ProfileReader interface {
	ListProfiles(context.Context, string, int) (api.Page[api.Profile], error)
	GetProfile(context.Context, string) (api.Tagged[api.Profile], error)
}

// ProfileAPI manages viewing profiles.
type ProfileAPI interface {
	ProfileReader
	CreateProfile(context.Context, string, string) (api.Tagged[api.Profile], error)
	ReplaceProfile(context.Context, string, string, string) (api.Tagged[api.Profile], error)
	DeleteProfile(context.Context, string, string) error
}

// ListProfiles reads one page of viewing profiles.
func ListProfiles(ctx context.Context, a ProfileReader, page PageRequest) (api.Page[api.Profile], error) {
	if err := page.validate(); err != nil {
		return api.Page[api.Profile]{}, err
	}
	return a.ListProfiles(ctx, page.Cursor, page.Limit)
}

// CollectProfiles reads viewing profiles until the list ends or max arrive.
func CollectProfiles(ctx context.Context, a ProfileReader, max int) ([]api.Profile, bool, error) {
	return Collect(ctx, a.ListProfiles, max)
}

// InspectProfile reads one viewing profile.
func InspectProfile(ctx context.Context, a ProfileReader, id string) (api.Tagged[api.Profile], error) {
	if strings.TrimSpace(id) == "" {
		return api.Tagged[api.Profile]{}, Invalid("a profile ID is required")
	}
	return a.GetProfile(ctx, id)
}

func profileName(name string) (string, error) {
	name = strings.TrimSpace(name)
	if name == "" {
		return "", Invalid("a profile name is required")
	}
	return name, nil
}

// CreateProfile creates a viewing profile.
func CreateProfile(ctx context.Context, a ProfileAPI, name string, i Intent) (api.Tagged[api.Profile], error) {
	name, err := profileName(name)
	if err != nil {
		return api.Tagged[api.Profile]{}, err
	}
	v, err := a.CreateProfile(ctx, name, i.Key)
	return v, i.settle(err)
}

func (pre Precondition) profile(ctx context.Context, a ProfileReader, id string) (string, error) {
	return pre.resolve(ctx, "the profile", tagged(func(ctx context.Context) (api.Tagged[api.Profile], error) {
		return InspectProfile(ctx, a, id)
	}, func(p api.Profile) string { return p.Revision }))
}

// RenameProfile renames a viewing profile.
func RenameProfile(ctx context.Context, a ProfileAPI, id, name string, pre Precondition) (api.Tagged[api.Profile], error) {
	name, err := profileName(name)
	if err != nil {
		return api.Tagged[api.Profile]{}, err
	}
	etag, err := pre.profile(ctx, a, id)
	if err != nil {
		return api.Tagged[api.Profile]{}, err
	}
	v, err := a.ReplaceProfile(ctx, id, name, etag)
	return v, settleConditional("rename of profile "+id, etag, err)
}

// DeleteProfile removes a viewing profile and its viewing state; media is
// untouched.
func DeleteProfile(ctx context.Context, a ProfileAPI, id string, pre Precondition) error {
	etag, err := pre.profile(ctx, a, id)
	if err != nil {
		return err
	}
	return settleConditional("deletion of profile "+id, etag, a.DeleteProfile(ctx, id, etag))
}
