package api

import (
	"bufio"
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"net/http"
	"strings"
	"time"
)

// ErrStreamEnded means the server closed the event stream, for example after
// the credential was revoked or the server began draining.
var ErrStreamEnded = errors.New("event stream ended")

// handlerError is a failure of the consumer (e.g. a closed stdout), which
// reconnecting cannot fix.
type handlerError struct{ err error }

func (e handlerError) Error() string { return e.err.Error() }
func (e handlerError) Unwrap() error { return e.err }

// Events opens one event stream resuming after cursor ("" for a fresh reset)
// and calls handle for each hint until the stream ends or ctx is cancelled.
// It returns the last cursor delivered so the caller can resume.
func (c *Client) Events(ctx context.Context, cursor string, handle func(Event) error) (string, error) {
	r := request{method: http.MethodGet, path: "/api/v2/events", auth: true, header: http.Header{}}
	r.header.Set("Accept", "text/event-stream")
	if cursor != "" {
		r.header.Set("Last-Event-ID", cursor)
	}
	req, err := c.newRequest(ctx, r, nil)
	if err != nil {
		return cursor, err
	}
	req.Header.Set("Accept", "text/event-stream")
	resp, err := c.HTTP.Do(req)
	if err != nil {
		if ctx.Err() != nil {
			return cursor, ctx.Err()
		}
		return cursor, &UnavailableError{Err: err}
	}
	defer resp.Body.Close()
	if resp.StatusCode != http.StatusOK {
		data, _ := io.ReadAll(io.LimitReader(resp.Body, 64<<10))
		return cursor, decodeProblem(resp, data)
	}
	scanner := bufio.NewScanner(resp.Body)
	scanner.Buffer(make([]byte, 64<<10), 1<<20)
	var data []string
	for scanner.Scan() {
		line := scanner.Text()
		switch {
		case line == "":
			if len(data) == 0 {
				continue
			}
			var e Event
			payload := strings.Join(data, "\n")
			data = data[:0]
			if err := json.Unmarshal([]byte(payload), &e); err != nil {
				return cursor, fmt.Errorf("invalid event from server: %w", err)
			}
			if err := handle(e); err != nil {
				return cursor, handlerError{err}
			}
			cursor = e.Cursor
		case strings.HasPrefix(line, ":"):
			// Heartbeat; carries no state.
		case strings.HasPrefix(line, "data:"):
			data = append(data, strings.TrimPrefix(strings.TrimPrefix(line, "data:"), " "))
		}
	}
	if ctx.Err() != nil {
		return cursor, ctx.Err()
	}
	if err := scanner.Err(); err != nil {
		return cursor, &UnavailableError{Err: err}
	}
	return cursor, ErrStreamEnded
}

// Follow keeps an event stream open across disconnects, resuming from the
// last delivered cursor. Authentication and permission failures stop it;
// unavailability backs off and reconnects. A reset event tells the handler
// to discard cached views and re-query.
func (c *Client) Follow(ctx context.Context, cursor string, handle func(Event) error, disconnected func(error, time.Duration)) error {
	for attempt := 0; ; {
		next, err := c.Events(ctx, cursor, func(e Event) error {
			attempt = 0
			return handle(e)
		})
		cursor = next
		if ctx.Err() != nil {
			return ctx.Err()
		}
		var consumer handlerError
		if errors.As(err, &consumer) {
			return consumer.err
		}
		var p *Problem
		if errors.As(err, &p) && p.Status != http.StatusServiceUnavailable && p.Status != http.StatusTooManyRequests {
			if p.Status == http.StatusBadRequest && p.Code == "invalid_cursor" && cursor != "" {
				cursor = ""
				continue
			}
			return err
		}
		wait := backoff(attempt)
		if p != nil && p.RetryAfter > 0 {
			wait = p.RetryAfter
		}
		if errors.Is(err, ErrStreamEnded) {
			// A clean close may be a revocation: re-check before reconnecting.
			probe, cancel := context.WithTimeout(ctx, 30*time.Second)
			_, meErr := c.Me(probe)
			cancel()
			if meErr != nil {
				var mp *Problem
				if errors.As(meErr, &mp) && (mp.Status == 401 || mp.Status == 403) {
					return meErr
				}
			}
		}
		if disconnected != nil {
			disconnected(err, wait)
		}
		if err := c.Sleep(ctx, wait); err != nil {
			return err
		}
		attempt++
	}
}
