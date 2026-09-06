-- Data repair only: keep the singleton when rolling back, since removing it
-- breaks discovery in both the current and previous application versions.
SELECT 1;
