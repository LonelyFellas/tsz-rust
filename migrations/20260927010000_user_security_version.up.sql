ALTER TABLE users ADD COLUMN security_version bigint NOT NULL DEFAULT 0;
ALTER TABLE users ADD CONSTRAINT users_security_version_nonnegative CHECK (security_version >= 0);
