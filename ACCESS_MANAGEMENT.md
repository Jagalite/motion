# Administrator access

## First run (single owner)

A new server has no users. Pairing a browser needs no email, password or SSO:

1. In the browser, open Motion and choose **Pair this browser**. Keep the page
   open; it shows a code such as `ABCD-EFGH`.
2. On the server computer, run `motion --operator-token-file <data-dir>/admin-token tui`.
   The protected token file establishes operator authority; a loopback
   connection alone grants nothing.
3. On **Pairing requests**, press **s** (or **a** on the request) to create the
   Owner. Review and press **y**. The Owner is one enabled user whose access
   includes administration of this server. This step approves no device and
   does not touch existing devices: unassigned devices stay unassigned.
4. Select the browser's request and press **a**. Because the Owner is the only
   enabled user it is preselected. Compare the code with the browser, review the
   grant and press **y**. **Administration is never preselected**: select
   `Permission: system:admin` if this browser should manage the server; the
   review states whether administration is granted.
5. The browser signs in on its own once approved.

The CLI equivalent is `motion access setup`, then
`motion auth approve PAIRING_ID --code CODE` (the only enabled user is selected;
add `--permission system:admin` deliberately). An administrator browser, such as
the desktop app's, offers **First run: create the Owner** under **Users** and
preselects the only enabled user under **Pairing & invitations**, where
granting administration needs a separate confirmation. From a browser these
are sensitive changes like creating a user: they need recent sign-in or an
approval from another authorized device. A paired-only browser on a new server
has neither, so use the TUI with the operator token for the first run.

Setup runs only while no user exists, and only for the local operator or an
administrator. Repeating it, retrying after a lost reply, or running it
concurrently returns the same Owner; an interrupted attempt leaves no partial
owner. Once a user exists, setup never adopts it as Owner (`owner_setup_unavailable`).
With two or more enabled users, approvals require choosing the user explicitly.

Open `motion tui` on a paired administrator terminal. The Admin sidebar contains
**Pairing requests**, **Users and access**, **Invitations**, and **Devices**.
These screens use the public server API; they never read the database.

1. In **Users and access**, press **n** to create a named access identity. Select
   viewing profiles, libraries and permissions. Profiles describe viewing history;
   they are not login accounts. Use **Tab** or the arrow keys to move, **Space** to
   toggle, and **Ctrl+S** to review. Press **y** to apply or **Esc** to cancel.
2. A browser chooses **Pair this browser** on the sign-in page. In **Pairing
   requests**, select its request, compare the code with the requesting browser,
   then press **a**. Choose its user (preselected when it is the only enabled
   user) and grant, review, and confirm. Device/client
   names are untrusted display text. Press **d** to deny a request. Lists refresh
   periodically; **r** refreshes immediately. Concurrent or expired decisions are
   reported by the server.
3. In **Users and access**, select a user and press **e** to change its name,
   permissions or restrictions. Uncheck **Access enabled** to revoke all its
   devices; review and confirm before applying. Reenabling a user requires new
   device approval: revoked credentials are never restored.
4. In **Devices**, select one device and press **d** to revoke only that device.
   Its siblings retain their own access. Owner and client names are shown in the
   detail panel. The screen does not invent online or last-seen information.
5. In **Invitations**, press **n**, select a user, choose the preapproved grant and
   expiry (60 seconds to 24 hours), review and confirm. Administration is unchecked
   by default, including for an administrator user. Share the displayed code
   privately; **Esc** hides it. At narrow widths join the displayed code lines
   without spaces. Select an unused invitation and press **d** to revoke it.
6. The recipient enters the code in **Redeem invitation** on the browser sign-in
   page. Keep that page open for a retry after a connection failure. A terminal
   recipient runs `motion auth redeem --credential-store file`; the code is read
   from standard input and hidden when entered interactively. The existing explicit
   consent for a restrictive credential file still applies.

Grants are bounded by the selected user's access and revision. User edits narrow
existing devices immediately; widening a user does not silently widen an earlier
device approval. Pair a new device for a wider grant. Ratings are comma separated;
an empty allowed-rating list means any rating unless **Deny all rated content**
is selected. That explicit denial requires an empty list and is also the result
of combining disjoint user/device rating lists. **Allow unrated content** remains
separate. Rating and label evidence is not yet available to catalog authorization,
so restricted rating/label policies currently deny catalog access conservatively.
Library and profile selections are explicit. Device-specific blocked labels stay
in effect when a user changes. If combining them would exceed 200 labels, the
whole user edit is refused without changing any device. Adjust the device policy
explicitly or approve a new device before retrying. Administration permits all
operations and overrides ordinary resource restrictions; grant it deliberately.

Existing devices retain their IDs, credentials, profiles and grants after upgrade.
Unassigned devices remain visibly **Unassigned**; older pairing clients can still
approve a device without creating a person. Select a local user explicitly when
approving new enrollment. Administrators can assign an existing unassigned device
through `PUT /api/v2/devices/{device_id}/user`, with the device's current `If-Match`
and `{ "user_id": "…", "user_revision": "…" }`. This narrows its grant to the
user ceiling and does not transfer an already owned device or revive a revoked
one. There is no ownership inference from names or email.

Migration 0046 reconciles automatic placeholders created by migration 0045: only
untouched, singly owned users without invitations are detached and removed.
Edited users, shared users and invitation references retain their ownership.
Device grants, credentials and viewing data are preserved.

Invitations are single-use bearer secrets, stored as hashes. A lost claim response
can return the same credential only for the same retained claim identity and exact
request. A second claimant never receives a new credential. Expiry, target-user
revision changes, or loss/change of the issuing administrator credential or policy
invalidate outstanding invitations and claim retries. Creation through the
server-local operator is pinned to its current token; rotating that token also
invalidates its invitations. Redemption has a durable
server-wide limit of 30 attempts per minute, including unknown codes; restart does
not reset it. This limit can temporarily delay legitimate recipients during abuse.
Codes never belong in URLs, command arguments, logs, screenshots, or issue reports.

Revocation denies new API/media admissions and uses the existing active-delivery
rechecks to stop playback. Already admitted network bytes may finish before the
stop takes effect. Downloaded bytes on a disconnected device cannot be erased by
server revocation. Authorized unsynchronized viewing events remain under their
original device/profile identity; this feature does not delete or reassign them.

The server refuses changes that remove the last usable paired administrator.
Unclaimed devices and expired credentials do not count. Keep a second administrator
for routine management. For explicit server-local recovery, the server operator
can run `motion --operator-token-file /path/to/admin-token tui` using the protected
0600 token file configured for that installation. This reads the file locally;
never paste its contents into a browser or another device. Use the TUI to create
an administrator identity and approve a new administrator terminal. Possession of
that protected file establishes operator authority; loopback alone grants none.
