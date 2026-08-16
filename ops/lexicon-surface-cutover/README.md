# Lexicon surface B4 runbook

This directory contains the reviewed, irreversible R3 cutover artifact. It is
kept outside `migrations/`, so application startup cannot drop the legacy
cross-entry unique index.

## Non-production rehearsal

Set `DATABASE_URL` and `REDIS_URL` to an isolated environment, then run:

```bash
cargo run --bin lexicon_surface_migration -- migrate
cargo run --bin lexicon_surface_migration -- backfill
EXPECTED_SURFACE_WRITER_VERSION=surface-writer-v1 \
  cargo run --bin lexicon_surface_migration -- preflight
shasum -a 256 ops/lexicon-surface-cutover/20260816_drop_cross_entry_headword_unique.sql
EXPECTED_SURFACE_WRITER_VERSION=surface-writer-v1 \
CONFIRMED_CUTOVER_ARTIFACT_SHA256=<reviewed-sha256> \
  cargo run --bin lexicon_surface_migration -- cutover
cargo run --bin lexicon_surface_migration -- policy-enable
```

After acceptance, always restore the creation policy:

```bash
cargo run --bin lexicon_surface_migration -- policy-disable
```

Every command prints a JSON report. Preserve the backfill, preflight, cutover,
policy epoch, exact application commit, OpenAPI hash, database image and Redis
image as the rehearsal evidence bundle.

## Production R3 gate

Before executing the artifact, an independent reviewer must verify:

1. every application instance runs the reviewed commit and
   `surface-writer-v1`;
2. the final report has zero missing/orphan/mismatched rows and zero outbox lag;
3. the non-unique lookup index exists and the reported artifact SHA-256 matches
   the reviewed file;
4. creation and publication policies remain disabled;
5. an on-call release commander has accepted the roll-forward plan.

Before R3, stop normally by leaving the legacy unique index intact. After R3,
never run a down migration or delete entries to recreate uniqueness. Disable
creation by advancing the policy epoch, keep full warning reads and surface
locks, then repair forward.
