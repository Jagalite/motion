// Package operations holds the typed client workflows that the CLI and the
// TUI both invoke in process. Each operation takes a context and explicit
// inputs, composes calls on a narrow slice of the API client, and returns
// typed results and errors. Operations never print, prompt, exit, read
// terminal state or depend on Cobra or Bubble Tea.
//
// The server and the production Rust core remain authoritative for
// permissions, ownership, revisions and job transitions. Capability and
// permission checks here only guide discovery and phrase useful errors.
package operations
