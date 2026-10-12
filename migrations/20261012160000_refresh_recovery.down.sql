DROP INDEX admin_refresh_tokens_replacement;
DROP INDEX refresh_tokens_replacement;
ALTER TABLE admin_refresh_tokens DROP COLUMN rotation_request_hash, DROP COLUMN replacement_token_hash;
ALTER TABLE refresh_tokens DROP COLUMN rotation_request_hash, DROP COLUMN replacement_token_hash;
