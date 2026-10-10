package api

import (
	"context"
	"net/http"
	"net/url"
)

type LibraryInput struct {
	Name      string   `json:"name"`
	Kind      string   `json:"kind"`
	Language  string   `json:"language"`
	SourceIDs []string `json:"source_ids"`
}
type SourceInput struct {
	Name       string   `json:"name"`
	RootPath   string   `json:"root_path"`
	Exclusions []string `json:"exclusions"`
}
type Source struct {
	ID              string   `json:"id"`
	Revision        string   `json:"revision"`
	BindingRevision string   `json:"binding_revision"`
	Name            string   `json:"name"`
	RootPath        string   `json:"root_path"`
	VolumeIdentity  string   `json:"volume_identity"`
	Availability    string   `json:"availability"`
	Exclusions      []string `json:"exclusions"`
}
type ExternalIdentity struct {
	Provider         string `json:"provider"`
	Value            string `json:"value"`
	NamespaceVersion string `json:"namespace_version"`
}
type CatalogItem struct {
	MatchState        string             `json:"match_state"`
	ID                string             `json:"id"`
	Revision          string             `json:"revision"`
	Kind              string             `json:"kind"`
	Title             string             `json:"title"`
	LibraryIDs        []string           `json:"library_ids"`
	ParentIDs         []string           `json:"parent_ids"`
	ExternalIDs       []ExternalIdentity `json:"external_ids"`
	DefaultTimelineID *string            `json:"default_timeline_id"`
	ArtworkID         *string            `json:"artwork_id"`
	Availability      string             `json:"availability"`
}

func (c *Client) CreateLibrary(ctx context.Context, in LibraryInput, key string) (Tagged[Library], error) {
	var v Library
	h, err := c.do(ctx, request{method: http.MethodPost, path: "/api/v2/libraries", body: in, auth: true, idempotencyKey: key}, &v)
	return Tagged[Library]{Value: v, ETag: h.Get("ETag")}, err
}
func (c *Client) GetLibrary(ctx context.Context, id string) (Tagged[Library], error) {
	var v Library
	h, err := c.do(ctx, request{method: http.MethodGet, path: "/api/v2/libraries/" + esc(id), auth: true}, &v)
	return Tagged[Library]{Value: v, ETag: h.Get("ETag")}, err
}
func (c *Client) ListSources(ctx context.Context, cursor string, limit int) (Page[Source], error) {
	var v Page[Source]
	_, err := c.do(ctx, request{method: http.MethodGet, path: "/api/v2/sources", query: pageQuery(cursor, limit), auth: true}, &v)
	return v, err
}
func (c *Client) CreateSource(ctx context.Context, in SourceInput, key string) (Tagged[Source], error) {
	var v Source
	h, err := c.do(ctx, request{method: http.MethodPost, path: "/api/v2/sources", body: in, auth: true, idempotencyKey: key}, &v)
	return Tagged[Source]{Value: v, ETag: h.Get("ETag")}, err
}
func (c *Client) GetSource(ctx context.Context, id string) (Tagged[Source], error) {
	var v Source
	h, err := c.do(ctx, request{method: http.MethodGet, path: "/api/v2/sources/" + esc(id), auth: true}, &v)
	return Tagged[Source]{Value: v, ETag: h.Get("ETag")}, err
}
func (c *Client) ListCatalog(ctx context.Context, cursor string, limit int, library string) (Page[CatalogItem], error) {
	var v Page[CatalogItem]
	q := pageQuery(cursor, limit)
	if library != "" {
		q.Set("library_id", library)
	}
	_, err := c.do(ctx, request{method: http.MethodGet, path: "/api/v2/catalog/items", query: q, auth: true}, &v)
	return v, err
}
func (c *Client) SearchCatalog(ctx context.Context, text, cursor string, limit int) (Page[CatalogItem], error) {
	var v Page[CatalogItem]
	q := pageQuery(cursor, limit)
	q.Set("q", text)
	_, err := c.do(ctx, request{method: http.MethodGet, path: "/api/v2/catalog/search", query: q, auth: true}, &v)
	return v, err
}
func (c *Client) GetCatalogItem(ctx context.Context, id string) (Tagged[CatalogItem], error) {
	var v CatalogItem
	h, err := c.do(ctx, request{method: http.MethodGet, path: "/api/v2/catalog/items/" + esc(id), query: url.Values{}, auth: true}, &v)
	return Tagged[CatalogItem]{Value: v, ETag: h.Get("ETag")}, err
}

type Timeline struct {
	ID            string   `json:"id"`
	Revision      string   `json:"revision"`
	ItemID        string   `json:"item_id"`
	EditionID     string   `json:"edition_id"`
	DurationMS    *int64   `json:"duration_ms"`
	VersionIDs    []string `json:"version_ids"`
	OrderGroupID  *string  `json:"order_group_id"`
	OrderPosition *int64   `json:"order_position"`
}

func (c *Client) GetTimeline(ctx context.Context, id string) (Tagged[Timeline], error) {
	var v Timeline
	h, err := c.do(ctx, request{method: http.MethodGet, path: "/api/v2/catalog/timelines/" + esc(id), auth: true}, &v)
	return Tagged[Timeline]{Value: v, ETag: h.Get("ETag")}, err
}
