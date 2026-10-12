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

## Identity template pilot

The identity job creates a unique, job-owned template from an empty database and
applies every current migration. The template is sealed with
`ALLOW_CONNECTIONS false` after closing its last connection; nothing is cached
between runs. Only the 65 exact business case paths in
`ops/ci_template_cases.json` clone it. All other SQLx cases, including migration
specialists, retain their original fresh-database path. New cases default to
fresh; absent/renamed whitelist entries fail the execution audit.

The disposable PostgreSQL testing copy changes only CREATE DATABASE for these
cases, using PostgreSQL 16's default WAL_LOG strategy. Each case retains its own
SQLx name, registry entry, fixture loading, pool limits and cleanup. SQLx's
original migrator still runs, including locking, version/checksum and dirty-ledger
checks. A compiled migration fingerprint must match the template's fingerprint.
Production sources, migration files, test bodies/assertions/fixture sizes and
the checked-in Cargo manifest/lockfile are unchanged.

Before executing any case, `ci_identity_template.rs` compares clone state with a
second database fully migrated from empty: catalog definitions, initial table
rows, migration ledger, sequence definitions/values, extensions, owners,
privileges and database settings. Only explicitly listed migration clock fields
and the cluster transaction ID watermark are normalized; seed IDs and all other
data remain part of the comparison. Committed DDL/data/sequence changes in A,
deletion of A while B is connected, and a new clone after mutation must leave B
and the template baseline unchanged. Separate corrupted-ledger probes must be
rejected for dirty state and checksum mismatch. Cleanup drops only successfully
created, invocation-owned databases, without terminating unrelated sessions.

Every whitelist case must produce exactly one clone-use marker and pass; no
other case may produce one. Full-shard discovery/execution auditing remains in
place. Measurements include one-time bootstrap/verification overhead and actual
clone/fresh counts, as well as the original phase timings. Compare both execution
and compile/whole-job time against successful run 38225091081; do not treat
cumulative concurrent phase savings as wall-time savings. This limited pilot
does not promise to shorten the independent lexicon shard's critical path.

## Lexicon handler lifecycle and maximum-batch measurements

The lexicon-handler shard reuses the same verified SQLx 0.9.0 private-copy
lifecycle timers, without the identity template/helper or cloning route. Every
enabled case must retain fresh database creation, automatic migration, body,
connection lifecycle and cleanup timing. The pagination shard retains its
original path; all existing execution/ignored-case audits remain in place.

Only the disposable copy of the existing 50-entry cyclic maximum-batch test
receives request timers: 50 forms impact/save pairs, 50 initial meanings saves,
50 serial relation saves, 20 list reads and 20 reference reads. Reversing timer
edits must reproduce the original test source bytes. The original five fixture
wall spans are also collected. Assertions, API calls, await order, `.buffered(4)`,
50 entries/50 relations and all 20+20 read samples remain unchanged.

`lexicon-handler-phase-measurements` stores the complete lifecycle summary,
slowest cases, maximum-batch lifecycle/fixture walls, per-request distributions,
counts and source evidence; raw events accompany the execution inventory. A
missing, invalid or extra sample fails the job. Request sums in the concurrent
content fixture are cumulative, not wall time; never add them to their parent
spans or attribute a whole-shard delay to one overlapping test without evidence.
Reports contain only names/counts/times/hashes, not SQL or fixture data.
