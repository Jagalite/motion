# Administrator access

Open `motion tui` on a paired administrator terminal. The Admin sidebar contains
**Pairing requests**, **Users and access**, **Invitations**, and **Devices**.
These screens use the public server API; they never read the database.

1. In **Users and access**, press **n** to create a named access identity. Select
   viewing profiles, libraries and permissions. Profiles describe viewing history;
   they are not login accounts. Use **Tab** or the arrow keys to move, **Space** to
   toggle, and **Ctrl+S** to review. Press **y** to apply or **Esc** to cancel.
2. A browser chooses **Pair this browser** on the sign-in page. In **Pairing
   requests**, select its request, compare the code with the requesting browser,
   then press **a**. Choose its user and grant, review, and confirm. Device/client
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
an empty allowed-rating list means any rating, and **Allow unrated content** is
separate. Library and profile selections are explicit. Device-specific blocked labels stay
in effect when a user changes. If combining them would exceed 200 labels, the
whole user edit is refused without changing any device. Adjust the device policy
explicitly or approve a new device before retrying. Administration permits all
operations and overrides ordinary resource restrictions; grant it deliberately.

Existing devices retain their IDs, credentials, profiles and grants after upgrade.
Each gets a separate **Legacy device: …** user, even when names match. Older
single-device approval clients similarly create a **Device identity: …** user.
Rename/edit that identity to manage it explicitly. Associating another device
turns it into a shared user ceiling. There is no ownership inference from names,
password provider, or automatic device transfer between users.

Invitations are single-use bearer secrets, stored as hashes. A lost claim response
can return the same credential only for the same retained claim identity and exact
request. A second claimant never receives a new credential. Expiry, target-user
revision changes, or loss/change of the issuing administrator credential or policy
invalidate outstanding invitations and claim retries. Creation through the
server-local operator is tied to that installation's key. Redemption has a durable
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
