-- 0045 generated one identity per old device. Preserve the distinction between
-- a paired device and an explicitly selected person. Only untouched, unshared,
-- unreferenced placeholders can be detached; edited users and invitations are
-- evidence of deliberate user management and retain their exact ownership.
CREATE TEMP TABLE motion_unassigned_legacy_users AS
SELECT u.id
FROM access_users u
JOIN devices d ON d.user_id=u.id AND u.id='user-'||d.id
WHERE u.legacy=1
  AND (SELECT count(*) FROM devices owned WHERE owned.user_id=u.id)=1
  AND NOT EXISTS (SELECT 1 FROM access_invitations i WHERE i.user_id=u.id);

UPDATE devices SET user_id=NULL
WHERE user_id IN (SELECT id FROM motion_unassigned_legacy_users);
DELETE FROM access_users
WHERE id IN (SELECT id FROM motion_unassigned_legacy_users);
DROP TABLE motion_unassigned_legacy_users;

-- Retained users have deliberate associations. Their ceilings must never mirror
-- later edits of just one device. No device grant, credential or viewing row is
-- rewritten by this reconciliation.
UPDATE access_users SET legacy=0 WHERE legacy=1;
