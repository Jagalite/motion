package api

import (
	"bytes"
	"context"
	"crypto/rand"
	"encoding/hex"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"mime"
	"net/http"
	"net/url"
	"strconv"
	"strings"
	"time"
)

const maxResponseBytes = 8 << 20

// Problem is an RFC 9457 error returned by the server.
type Problem struct {
	Type       string        `json:"type"`
	Title      string        `json:"title"`
	Status     int           `json:"status"`
	Code       string        `json:"code"`
	Detail     string        `json:"detail"`
	RequestID  string        `json:"request_id"`
	Retryable  bool          `json:"retryable"`
	RetryAfter time.Duration `json:"-"`
}

func (p *Problem) Error() string {
	if p.Detail != "" {
		return fmt.Sprintf("%s (%d %s): %s", p.Code, p.Status, p.Title, p.Detail)
	}
	return fmt.Sprintf("%s (%d %s)", p.Code, p.Status, p.Title)
}

// UnavailableError means the server could not be reached or did not answer.
type UnavailableError struct{ Err error }

func (e *UnavailableError) Error() string { return "server unavailable: " + e.Err.Error() }
func (e *UnavailableError) Unwrap() error { return e.Err }

// Client calls one Motion server. Token supplies the bearer credential for
// authenticated operations; it is read per request so a refreshed access
// token takes effect immediately.
type Client struct {
	BaseURL     *url.URL
	HTTP        *http.Client
	Token       func() string
	UserAgent   string
	MaxAttempts int
	// Sleep waits between attempts; replaced in tests.
	Sleep func(context.Context, time.Duration) error
}

func New(base string, httpClient *http.Client) (*Client, error) {
	// The rejected URL is not echoed: it may embed credentials or tokens.
	u, err := url.Parse(strings.TrimRight(base, "/"))
	if err != nil || (u.Scheme != "http" && u.Scheme != "https") || u.Host == "" || u.User != nil ||
		u.Path != "" || u.RawQuery != "" || u.Fragment != "" {
		return nil, errors.New("invalid server URL: use http(s)://host[:port] without credentials, path or query")
	}
	if httpClient == nil {
		httpClient = &http.Client{}
	}
	// Never forward credentials to another origin through a redirect.
	httpClient.CheckRedirect = func(*http.Request, []*http.Request) error {
		return http.ErrUseLastResponse
	}
	return &Client{
		BaseURL:     u,
		HTTP:        httpClient,
		Token:       func() string { return "" },
		UserAgent:   "motion-terminal",
		MaxAttempts: 3,
		Sleep:       sleep,
	}, nil
}

func sleep(ctx context.Context, d time.Duration) error {
	t := time.NewTimer(d)
	defer t.Stop()
	select {
	case <-ctx.Done():
		return ctx.Err()
	case <-t.C:
		return nil
	}
}

// NewIdempotencyKey returns a random key. Reuse one key for every retry of
// the same intended operation; that is what makes the retry safe.
func NewIdempotencyKey() string {
	b := make([]byte, 16)
	if _, err := rand.Read(b); err != nil {
		panic(err)
	}
	return hex.EncodeToString(b)
}

type request struct {
	method         string
	path           string
	query          url.Values
	body           any
	auth           bool
	idempotencyKey string
	ifMatch        string
	header         http.Header
}

func (c *Client) url(path string, query url.Values) string {
	u := *c.BaseURL
	// Call sites already escape each opaque ID. URL.Path stores decoded text;
	// retain RawPath so an escaped slash remains inside its resource segment.
	u.Path = path
	if decoded, err := url.PathUnescape(path); err == nil {
		u.Path, u.RawPath = decoded, path
	}
	if len(query) > 0 {
		u.RawQuery = query.Encode()
	}
	return u.String()
}

// retrySafe: reads, and writes that carry an idempotency key.
func (r request) retrySafe() bool {
	return r.method == http.MethodGet || r.method == http.MethodHead || r.idempotencyKey != ""
}

func backoff(attempt int) time.Duration {
	// Clamp before shifting: Follow can reconnect indefinitely, and a shift
	// at attempt 36 otherwise overflows into a negative (immediate) delay.
	if attempt >= 5 {
		return 5 * time.Second
	}
	return 250 * time.Millisecond << max(attempt, 0)
}

func (c *Client) do(ctx context.Context, r request, out any) (http.Header, error) {
	var body []byte
	if r.body != nil {
		var err error
		if body, err = json.Marshal(r.body); err != nil {
			return nil, err
		}
	}
	attempts := max(c.MaxAttempts, 1)
	for attempt := 0; ; attempt++ {
		header, err := c.once(ctx, r, body, out)
		if err == nil || attempt+1 >= attempts || !r.retrySafe() || ctx.Err() != nil {
			return header, err
		}
		wait, retry := retryAfter(err, attempt)
		if !retry {
			return header, err
		}
		if err := c.Sleep(ctx, wait); err != nil {
			return nil, err
		}
	}
}

func retryAfter(err error, attempt int) (time.Duration, bool) {
	var unavailable *UnavailableError
	if errors.As(err, &unavailable) {
		return backoff(attempt), true
	}
	var p *Problem
	if errors.As(err, &p) && (p.Status == http.StatusServiceUnavailable || p.Status == http.StatusTooManyRequests) {
		if p.RetryAfter > 0 {
			return min(p.RetryAfter, 30*time.Second), true
		}
		return backoff(attempt), true
	}
	return 0, false
}

func (c *Client) newRequest(ctx context.Context, r request, body []byte) (*http.Request, error) {
	var reader io.Reader
	if body != nil {
		reader = bytes.NewReader(body)
	}
	req, err := http.NewRequestWithContext(ctx, r.method, c.url(r.path, r.query), reader)
	if err != nil {
		return nil, err
	}
	req.Header.Set("Accept", "application/json")
	req.Header.Set("User-Agent", c.UserAgent)
	if body != nil {
		req.Header.Set("Content-Type", "application/json")
	}
	if r.auth {
		token := c.Token()
		if token == "" {
			return nil, &Problem{Status: 401, Code: "not_signed_in", Title: "Unauthorized",
				Detail: "no credential for this server; run `motion auth pair` or pass --operator-token-file"}
		}
		req.Header.Set("Authorization", "Bearer "+token)
	}
	if r.idempotencyKey != "" {
		req.Header.Set("Idempotency-Key", r.idempotencyKey)
	}
	if r.ifMatch != "" {
		req.Header.Set("If-Match", r.ifMatch)
	}
	for k, vs := range r.header {
		for _, v := range vs {
			req.Header.Add(k, v)
		}
	}
	return req, nil
}

func (c *Client) once(ctx context.Context, r request, body []byte, out any) (http.Header, error) {
	req, err := c.newRequest(ctx, r, body)
	if err != nil {
		return nil, err
	}
	resp, err := c.HTTP.Do(req)
	if err != nil {
		if ctx.Err() != nil {
			return nil, ctx.Err()
		}
		return nil, &UnavailableError{Err: err}
	}
	defer resp.Body.Close()
	data, err := io.ReadAll(io.LimitReader(resp.Body, maxResponseBytes+1))
	if err != nil {
		if ctx.Err() != nil {
			return nil, ctx.Err()
		}
		return nil, &UnavailableError{Err: err}
	}
	if len(data) > maxResponseBytes {
		return nil, fmt.Errorf("response exceeds %d bytes", maxResponseBytes)
	}
	if resp.StatusCode >= 300 {
		return resp.Header, decodeProblem(resp, data)
	}
	if out != nil && resp.StatusCode != http.StatusNoContent {
		if err := json.Unmarshal(data, out); err != nil {
			return resp.Header, fmt.Errorf("invalid response from server: %w", err)
		}
	}
	return resp.Header, nil
}

func decodeProblem(resp *http.Response, data []byte) error {
	p := &Problem{}
	media, _, _ := mime.ParseMediaType(resp.Header.Get("Content-Type"))
	if media != "application/problem+json" || json.Unmarshal(data, p) != nil || p.Code == "" {
		p = &Problem{Code: "http_" + strconv.Itoa(resp.StatusCode), Title: http.StatusText(resp.StatusCode)}
	}
	p.Status = resp.StatusCode
	if p.RequestID == "" {
		p.RequestID = resp.Header.Get("X-Request-ID")
	}
	if s, err := strconv.Atoi(resp.Header.Get("Retry-After")); err == nil && s > 0 {
		p.RetryAfter = time.Duration(s) * time.Second
	}
	return p
}

// IsCode reports whether err is a server problem with the given code.
func IsCode(err error, code string) bool {
	var p *Problem
	return errors.As(err, &p) && p.Code == code
}

func esc(id string) string { return url.PathEscape(id) }

func pageQuery(cursor string, limit int) url.Values {
	q := url.Values{}
	if cursor != "" {
		q.Set("cursor", cursor)
	}
	if limit > 0 {
		q.Set("limit", strconv.Itoa(limit))
	}
	return q
}

func (c *Client) Health(ctx context.Context) (Health, error) {
	var h Health
	_, err := c.do(ctx, request{method: http.MethodGet, path: "/api/v2/system/health"}, &h)
	return h, err
}

func (c *Client) Capabilities(ctx context.Context) (Capabilities, error) {
	var v Capabilities
	_, err := c.do(ctx, request{method: http.MethodGet, path: "/api/v2/system/capabilities", auth: true}, &v)
	return v, err
}

func (c *Client) Me(ctx context.Context) (Principal, error) {
	var v Principal
	_, err := c.do(ctx, request{method: http.MethodGet, path: "/api/v2/me", auth: true}, &v)
	return v, err
}

// CreatePairing is unauthenticated and not retried: each call is a new pairing.
func (c *Client) CreatePairing(ctx context.Context, in PairingRequest) (Pairing, error) {
	var v Pairing
	_, err := c.do(ctx, request{method: http.MethodPost, path: "/api/v2/auth/pairings", body: in}, &v)
	return v, err
}

func (c *Client) ApprovePairing(ctx context.Context, id string, in PairingApproval, key string) (Tagged[Device], error) {
	var v Device
	h, err := c.do(ctx, request{method: http.MethodPost, path: "/api/v2/auth/pairings/" + esc(id) + "/approve",
		body: in, auth: true, idempotencyKey: key}, &v)
	return Tagged[Device]{Value: v, ETag: h.Get("ETag")}, err
}

// ClaimPairing returns a pairing_pending problem until the pairing is approved.
func (c *Client) ClaimPairing(ctx context.Context, id, deviceCode string) (Credential, error) {
	var v Credential
	_, err := c.do(ctx, request{method: http.MethodPost, path: "/api/v2/auth/pairings/" + esc(id) + "/claim",
		body: map[string]string{"device_code": deviceCode}}, &v)
	return v, err
}

func (c *Client) IssueAccessToken(ctx context.Context, ttlSeconds int, key string) (Credential, error) {
	var v Credential
	_, err := c.do(ctx, request{method: http.MethodPost, path: "/api/v2/auth/access-tokens",
		body: map[string]int{"ttl_seconds": ttlSeconds}, auth: true, idempotencyKey: key}, &v)
	return v, err
}

func (c *Client) ListDevices(ctx context.Context, cursor string, limit int) (Page[Device], error) {
	var v Page[Device]
	_, err := c.do(ctx, request{method: http.MethodGet, path: "/api/v2/devices", query: pageQuery(cursor, limit), auth: true}, &v)
	return v, err
}

func (c *Client) GetDevice(ctx context.Context, id string) (Tagged[Device], error) {
	var v Device
	h, err := c.do(ctx, request{method: http.MethodGet, path: "/api/v2/devices/" + esc(id), auth: true}, &v)
	return Tagged[Device]{Value: v, ETag: h.Get("ETag")}, err
}

func (c *Client) RevokeDevice(ctx context.Context, id, etag string) error {
	_, err := c.do(ctx, request{method: http.MethodDelete, path: "/api/v2/devices/" + esc(id), auth: true, ifMatch: etag}, nil)
	return err
}

func (c *Client) GetDevicePolicy(ctx context.Context, id string) (Tagged[AccessPolicy], error) {
	var v AccessPolicy
	h, err := c.do(ctx, request{method: http.MethodGet, path: "/api/v2/devices/" + esc(id) + "/policy", auth: true}, &v)
	return Tagged[AccessPolicy]{Value: v, ETag: h.Get("ETag")}, err
}

func (c *Client) ReplaceDevicePolicy(ctx context.Context, id string, in AccessPolicy, etag string) (Tagged[AccessPolicy], error) {
	in.Revision = ""
	var v AccessPolicy
	h, err := c.do(ctx, request{method: http.MethodPut, path: "/api/v2/devices/" + esc(id) + "/policy",
		body: in, auth: true, ifMatch: etag}, &v)
	return Tagged[AccessPolicy]{Value: v, ETag: h.Get("ETag")}, err
}

func (c *Client) ListProfiles(ctx context.Context, cursor string, limit int) (Page[Profile], error) {
	var v Page[Profile]
	_, err := c.do(ctx, request{method: http.MethodGet, path: "/api/v2/profiles", query: pageQuery(cursor, limit), auth: true}, &v)
	return v, err
}

func (c *Client) GetProfile(ctx context.Context, id string) (Tagged[Profile], error) {
	var v Profile
	h, err := c.do(ctx, request{method: http.MethodGet, path: "/api/v2/profiles/" + esc(id), auth: true}, &v)
	return Tagged[Profile]{Value: v, ETag: h.Get("ETag")}, err
}

func (c *Client) CreateProfile(ctx context.Context, name, key string) (Tagged[Profile], error) {
	var v Profile
	h, err := c.do(ctx, request{method: http.MethodPost, path: "/api/v2/profiles",
		body: map[string]string{"name": name}, auth: true, idempotencyKey: key}, &v)
	return Tagged[Profile]{Value: v, ETag: h.Get("ETag")}, err
}

func (c *Client) ReplaceProfile(ctx context.Context, id, name, etag string) (Tagged[Profile], error) {
	var v Profile
	h, err := c.do(ctx, request{method: http.MethodPut, path: "/api/v2/profiles/" + esc(id),
		body: map[string]string{"name": name}, auth: true, ifMatch: etag}, &v)
	return Tagged[Profile]{Value: v, ETag: h.Get("ETag")}, err
}

func (c *Client) DeleteProfile(ctx context.Context, id, etag string) error {
	_, err := c.do(ctx, request{method: http.MethodDelete, path: "/api/v2/profiles/" + esc(id), auth: true, ifMatch: etag}, nil)
	return err
}

func (c *Client) ListLibraries(ctx context.Context, cursor string, limit int) (Page[Library], error) {
	var v Page[Library]
	_, err := c.do(ctx, request{method: http.MethodGet, path: "/api/v2/libraries", query: pageQuery(cursor, limit), auth: true}, &v)
	return v, err
}
