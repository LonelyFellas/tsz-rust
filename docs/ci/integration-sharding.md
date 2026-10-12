# Integration test sharding

The first optimization changes scheduling only. Rust sources, test assertions,
fixture sizes, migrations, compiler profile, services and caching policy stay
unchanged. The existing required `Integration (lexicon)` check becomes a
fail-closed aggregate, so branch protection does not need to change.

## Partition

- `lexicon-pagination`: the existing 251-entry/2008-candidate pagination case,
  selected by its complete libtest name with `--exact`.
- `lexicon-handler`: every other case in the same `lexicon_handler` executable.
  The skip filter is rejected if it could accidentally match another case.
- `lexicon-other`: every other target in the original lexicon module.
- `admin`, `identity`, `platform`: their original complete target sets.

The old module classifier still requires every repository integration target to
belong to exactly one logical module. New cases in lexicon_handler automatically
join the handler shard; removing or renaming the pagination case fails closed.
Each matrix job has its own PostgreSQL and Redis containers. No test result,
database, publication, token or credential is cached or shared between jobs.
There are two additional jobs, so reduced elapsed time may cost more aggregate
runner minutes; compiler and cache changes are outside this optimization.

## Execution audit

`ci_test_shards.py build` compiles the selected targets using Cargo's existing
locked/all-features test configuration and records the real executable paths.
`run` discovers complete and ignored case inventories with libtest `--list`,
runs the same binaries, and checks actual result names against the selected
inventory. Expected ignored cases are retained; unexpectedly ignored cases,
failures, duplicates and omissions cannot produce a passing report.

Six reports are uploaded from the current run. The aggregate checks every
report's workflow SHA, complete target membership, matching shared-binary
inventories, and actual execution counts. Every discovered integration case
must occur exactly once across the reports. Missing jobs/reports fail the gate.
The overall CI summary depends on this audit as well as all existing gates.

## Baseline and comparison

Baseline: [successful PR run 38215408736](https://github.com/LonelyFellas/tsz-rust/actions/runs/38215408736),
head d435341144cc7f0b86a5d94c5bfa59807bfd60e7, 92 integration targets.
The current main merge e663a96686577e3254b3886d6863a544efcbc930 has the same Rust tests.

| Group | Compile | Execute | Whole job |
| --- | ---: | ---: | ---: |
| lexicon | 180 s | 615 s | 825 s |
| identity | 162 s | 339 s | 536 s |
| admin | 153 s | 238 s | 424 s |
| platform | 148 s | 113 s | 301 s |

lexicon_handler accounted for 390.11 s. The pagination case's measured phases
were 1.166 s empty fixture, 231.260 s content fixture, 17.906 s publication
fixture and 25.582 s search/assertions. Case phases can overlap with other cases;
they must not be added to other test wall times as if they ran serially.

Compare the completed candidate CI's compile/run phases, longest matrix job and
overall required-gate latency against this baseline. Inspect the audit reports
and confirm the same 92 targets, case identities and existing ignored set. Also
report runner-minute cost; one hosted-runner comparison is evidence, not an SLA.
Results and candidate SHA are recorded in the PR after CI completes, avoiding a
new measurement run caused only by committing the report.

## Identity lifecycle measurements

Only the identity CI job uses a disposable profiling workspace. The SQLx 0.9.0
crate archives are verified against the original Cargo.lock SHA-256 checksums,
then extracted into private copies. Anchors in their test-only lifecycle code
must match exactly. Instrumentation adds clocks/logging around the original
operations without changing connection limits, transactions, timeout or cleanup.
The temporary lockfile changes only the two reviewed path sources, preserves
all transitive package versions, and is validated by `cargo metadata --locked`.
Neither the repository manifest/lockfile nor the shared Cargo registry is edited.

Every enabled identity case must have timing. SQLx cases record `db_setup`
(master connection/registration/old-name cleanup plus CREATE DATABASE), nested
`db_create`, automatic `migration`, `pool_connect`, pure test `body`, `pool_close`,
and database `cleanup`. `total` and `test_callback` are parent spans. Explicit
migration operations performed by a test itself remain part of its body; absent
automatic migrations are not fabricated. Non-database test bodies receive only
a timer statement in the disposable copy. Removing those statements reproduces
the original source bytes; assertions and fixtures are preserved.

The existing executed-case audit is unchanged. Missing/duplicate/invalid timing
fails identity profiling. Reports contain case names and durations only, never
SQL, database URLs, credentials or fixture data. Full measurements accompany the
identity execution inventory; `identity-phase-measurements` also contains totals,
slow-case details and source hashes. Phase totals are cumulative across cases,
including concurrent cases, and must not be interpreted as job wall time.
`db_create` is already inside `db_setup`; `body` and connection lifecycle are
already inside `test_callback`, so parent/child spans must not be added together.
