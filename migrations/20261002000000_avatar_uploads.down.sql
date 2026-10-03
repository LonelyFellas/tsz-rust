DROP TRIGGER IF EXISTS avatar_user_delete_cleanup ON users;
DROP FUNCTION IF EXISTS avatar_user_delete_cleanup();
DROP FUNCTION IF EXISTS avatar_schedule_user_cleanup(UUID);
UPDATE users SET avatar_url = '' WHERE avatar_upload_id IS NOT NULL;
ALTER TABLE users DROP CONSTRAINT users_avatar_owner_fk;
ALTER TABLE users DROP COLUMN avatar_upload_id;
DROP TABLE avatar_cleanup_tasks;
DROP FUNCTION avatar_cleanup_attempts_monotonic();
DROP TABLE avatar_uploads;
