# Native read-only administration candidate

This source candidate exposes existing, bounded application-owned reads through
the native-mode router. It is not a claim of runtime qualification or complete
administrator parity. Both new capability leaves remain `unqualified`.

## Scope and authorization

- `GET /api/v2/admin/diagnostics` reports process measurements and recorded
  native workload. A failed process measurement remains `process: null`.
- Exact GETs for authentication overview, restore-review **status**, providers,
  email configuration/activity, registration policy, pending access requests,
  fresh approvals, identities, sessions, user details and audit history reuse
  the existing Motion-owned read handlers.
- Each handler reauthenticates the current credential and requires
  `system:admin` inside the same transaction as its database disclosure.
  Cached extraction-time authority is insufficient. Existing server no-store
  response handling remains in force.
- No administrator prefix is admitted. HEAD, mutations, provider/mail tests,
  outbound mail diagnostics, restore approval, backups, restore/install/deploy
  and destructive cleanup remain closed.

`admin.native_diagnostics` and `admin.native_auth_reads` advertise only these
operations. This does not enable a broad administration or catalog capability.

## Operational semantics and privacy

Native queue/running counts observe physical `catabolic_scan_demands` in pending
status and `processing_jobs` with a retained native processing admission.
Several scan demands may belong to one request. Frozen legacy `jobs` and
processing history without a native admission are excluded. A recorded phase
or admission is not a fresh execution, source, publication or byte grant.

Failure history contains at most 100 stable `scan-demand:<id>:failed` or
`processing:<id>:failed` identities. Native scan failures are ordered by parent
request creation time, because the schema has no demand completion timestamp;
processing failures use recorded update time. Kind and ID break ties. Raw
worker output, physical paths and capture JSON are not selected.

Authentication reads preserve the established public field projections; secret
columns, hashes and sealed payloads are not returned. Native mail activity
replaces any historical free-form delivery detail with
`mail_delivery_detail_recorded`. Presence of that marker is observational, not
an assertion that a delivery failed or succeeded. Existing phase/ambiguity
fields retain their meaning. Legacy-mode responses are unchanged.

These reads neither consume proofs, change access, acknowledge jobs, refresh
catalog projections nor call native restore/import/install owners. They require
the same admitted native runtime as other routes. Missing runtime custody still
fails closed. Ordinary schema41 serving and relocated-library scope admission
are separate, unchanged gates. Displayed user/registration policy IDs are
descriptive stored values, not validation or grants for those libraries.

## Explicit authentication projection audit

All paths below have prefix `/api/v2/admin/auth`. The exact GET gate remains in
`src/v2/admin_reads.rs`; mutation siblings are not admitted. Each read performs
`caller.reauthorize(..., SystemAdmin)` before selecting its disclosed snapshot.
The following inventory reviews selected rows and response construction, not
merely the existence of the legacy handler:

- `/overview` and `/restore-review`: `overview` reads `signin_settings`, counts
  pending requests, checks current usable administrator devices/credentials,
  and compares restore/review epochs. Its response contains only revision,
  public origin, administrator/request counts, restored-hold boolean and two
  fixed configuration-error strings. `mail()` internally opens stored relay
  and DKIM secrets for a local composition rehearsal; neither those secrets,
  rehearsal content nor any network/error payload is serialized. No network
  test or restore approval is performed.
- `/providers`: `list_providers` reads provider rows plus the configured public
  origin. `provider_value` emits ID/revision, name, issuer, client ID, enabled,
  a `has_secret` boolean, configured destination origins/address pins,
  derived callback URL and test/login timestamps or booleans. It omits the
  secret itself and never decrypts it. Destination data comes from the typed
  validated configuration writer, not provider discovery or transport output.
- `/email`: `get_email` reads `signin_settings`. `email_value` parses typed
  `EmailConfig` and emits configuration/revision, a password-presence boolean,
  direct-delivery configuration, public DKIM DNS record/key identifier and
  test/receipt timestamps or booleans. It omits `email_secret` and
  `dkim_secret`; the DKIM DNS record is intentionally public material.
- `/email/activity`: reads bounded outbox rows. `activity_value` emits ID,
  cursor, purpose, normalized status, attempts/max attempts, ambiguity flag,
  delivery mode/configuration revision, recipient domain and lifecycle times.
  Native mode replaces non-null `last_error` detail with the fixed presence
  marker before returning it. Payload, transaction/owner references,
  destination hash and message/challenge secrets are not serialized.
- `/registration`: selects only `registration_json,registration_revision`;
  typed `Settings`/`DefaultAccess` reject unknown fields. The response contains
  registration policy/revision and default profile IDs, permissions and
  access policy. It contains no invitation code or challenge state.
- `/requests`: reads bounded `signin_requests`; `request_value` emits ID,
  revision, kind, provider ID, issuer, subject, address/email/name, status,
  created/expiry times and associated user ID. These are administrator-visible
  identity metadata, not an OIDC token, recovery credential or challenge code.
- `/fresh-approvals`: reads unconsumed, unexpired, current-epoch proof rows.
  `proof_value` emits ID, actor ID, action/target, derived status and times.
  Code/hash, actor/approver credential hashes and stored epoch are omitted.
  Reading does not approve or consume the proof.
- `/identities`: selects identity rows plus provider name; `identity_value`
  emits ID/revision, user ID, kind, provider ID/name, issuer/subject/address,
  enabled and creation time. It does not emit credentials or the identity's
  stored grant JSON.
- `/sessions`: selects family rows joined with device/user names and identity
  method/provider. `session_value` emits ID/revision, device/user IDs and names,
  method/provider ID, issuance/expiry/last-seen times and revoked. Parent and
  session hashes, epoch and authentication-proof internals are omitted.
- `/users/{id}`: `user_value` emits the explicitly selected user
  ID/name/revision/disabled/profile IDs/permissions/policy; identities and
  sessions use the same projections above. The separate device query selects
  only ID/name/revision/revoked/client name. Each nested list is bounded at 200.
- `/audit`: reads bounded audit rows and emits ID, actor, target, action,
  outcome and timestamp. Supported writers use fixed results or core enum
  phase/result/error codes for outcomes, not arbitrary transport exceptions.
  Actor/target identifiers can include administrator-supplied descriptive
  values; they are not interpreted as authentication material or new grants.

Several existing queries intentionally fetch `SELECT *`; row fetching is not
row serialization. Each response is an explicit projection, with the native
mail-detail override above. This audit assumes records produced by supported
writers; it does not claim to scrub arbitrary secrets inserted into descriptive
fields by direct database tampering. Internal SQL/decoding failures use the
fixed generic `Problem::internal` response, not the internal error string.

The API cache contract is exactly `Cache-Control: no-store`: both
`v2::stamp` and the outer API boundary apply it, prohibiting storage in private
and shared caches. `private, no-store` belongs to the separate presentation
identity layer. The integration test asserts the actual API contract on allowed,
unauthenticated, forbidden, revoked, unsupported and missing-runtime responses,
and after reopening; production cache behavior is unchanged.

## Correctness boundary and finite validation

No new state-dependent decision belongs in core: this slice only selects
observations. Core credential/grant policy and transactional reauthentication
remain authoritative. Native job/scan workers retain execution decisions.

Added tests, not run by this implementation worker:

- `v2::admin_reads::tests::native_admin_reads_are_exact_gets_not_an_admin_prefix`
- `v2::system::native_diagnostics::tests::native_diagnostics_count_only_recorded_native_workload_without_legacy_authority`
- `v2::system::native_diagnostics::tests::native_diagnostics_failure_history_is_bounded_and_stably_ordered`
- Integration target `v2_native_admin_reads`:
  `native_admin_read_routes_are_private_redacted_readonly_and_restart_safe`
  and
  `native_admin_diagnostics_rechecks_cached_authority_and_observes_real_scan_demands`.

The integration target uses genuine supported native activation and reopening,
public-router credentials, protected secret-free views, rejected mutations,
revocation, a post-extraction permission downgrade and an actually accepted
native scan request. It requires the same verified packaged observer executable
as the existing native application fixture. SQL reducer tests use deliberately
minimal application-only schemas and do not claim native authority.
