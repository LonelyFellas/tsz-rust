# Integration test sharding

The initial optimization changed scheduling only. Subsequent test-only Router
reuse pilots preserve test assertions, fixture sizes, migrations, compiler profile,
services and caching policy. The existing required `Integration (lexicon)` check becomes a
fail-closed aggregate, so branch protection does not need to change.

## Partition

- `lexicon-pagination`: the existing 251-entry/2008-candidate pagination case,
  selected by its complete libtest name with `--exact`, plus the complete
  `wordlists_handler` target without that filter.
- `lexicon-learning`: all `learning_*` targets plus `question_bank_handler`,
  without the pagination filter.
- `lexicon-handler`: the profiling shard. It keeps every `batch5::` case (the
  maximum-batch audit measures the same execution that contains the case) plus
  every other `lexicon_handler` case at an even index after sorting by name.
- `lexicon-handler-b`: the remaining `lexicon_handler` cases. The two shards skip
  each other's complete case names, so a name that is a substring of a case the
  other shard must run fails closed instead of silently dropping coverage.
- `lexicon-other`: every other target in the original lexicon module, including
  `shared_sentences`, excluding `lexicon_handler`, `learning_*`, `wordlists_handler`
  and `question_bank_handler`.
- `admin`, `identity`: their original complete target sets.
- `platform`: its original target set. Question-bank coverage remains in the
  learning shard. Every shard uses the same locked/all-features compilation and
  isolated PostgreSQL/Redis services.

The old module classifier still requires every repository integration target to
belong to exactly one logical module. New cases in `lexicon_handler` automatically
join one of the two handler shards, so neither shard needs an edited case list;
removing or renaming the pagination case fails closed. The two handler shards
run their separate case sets on independent runners, at the cost of another
compilation. Cumulative case timings include overlapping waits and are not CPU
measurements; elapsed-time gains must be measured on complete CI attempts.
Each matrix job has its own PostgreSQL and Redis containers. No test result,
database, publication, token or credential is cached or shared between jobs.
There are four additional jobs compared with the original four modules, so reduced elapsed time may cost more aggregate
runner minutes; compiler and cache changes are outside this optimization.

## Execution audit

`ci_test_shards.py build` compiles the selected targets using Cargo's existing
locked/all-features test configuration and records the real executable paths.
`run` discovers complete and ignored case inventories with libtest `--list`,
runs the same binaries, and checks actual result names against the selected
inventory. Expected ignored cases are retained; unexpectedly ignored cases,
failures, duplicates and omissions cannot produce a passing report.

Eight reports are uploaded from the current run. The aggregate checks every
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

An earlier six-job partition was evaluated without adding a runner: in successful main run [39575328075](https://github.com/LonelyFellas/tsz-rust/actions/runs/39575328075),
`lexicon-pagination` executed in 234 s, `lexicon-handler` in 472 s and
`lexicon-other` in 443 s. The five `learning_*` targets accounted for about 170 s
of serial execution in `lexicon-other`. Those targets now run in the separate `lexicon-learning` job; the historical estimate is not a measurement of the current partition.

## Identity lifecycle measurements

The identity CI job uses a disposable profiling workspace. The SQLx 0.9.0
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
cumulative concurrent phase savings as wall-time savings. The lexicon shards now
use the same template route without a per-case whitelist: the template is built
and verified by the identical migrator, so cloning is equivalent to a fresh
database plus full migration, and the audit requires one clone per executed case.

## Admin lifecycle and selective templates

Admin uses the same checksum-verified, disposable SQLx instrumentation and the
job-owned template equivalence/isolation probes. Only the explicit
`ADMIN_TEMPLATE_TARGETS` business target set is eligible for cloning, and a case
must have an automatic migrator. Its migration fingerprint must exactly match
the helper's complete root migrator before cloning. The idempotent migrator
still runs on the cloned database and checks the ledger.

`admin_schema`, `admin_permissions_handler`, `admin_preferences_handler` and
`admin_coins_handler` remain fresh, as do `migrations = false` cases inside opted-in
targets. New targets default to fresh. `admin-phase-measurements` records lifecycle
phases and clone/fresh counts. The audit derives eligible cases from actual
migration events plus the explicit target set; a missing, duplicate, foreign or
ineligible clone marker fails the job. Existing full execution auditing remains
unchanged.

## Lexicon handler lifecycle and maximum-batch measurements

Both lexicon shards reuse the same verified SQLx 0.9.0 private-copy lifecycle
timers and the same job-owned template route. Every executed case must clone the
template exactly once; a missing, duplicate or foreign template use fails the
job. Migration timers stay in the report: cloning skips the DDL work but still
runs the idempotent migrator, so the phase stays observable instead of vanishing
from the audit. The pagination shard retains its
original business path and uses an explicit test-local Router scope; all existing execution/ignored-case audits remain in place.

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


## Maximum-batch Router reuse pilot

The original maximum-batch pilot enters an explicit task-local Router
scope. It creates the complete application Router once, including Swagger under
all-features, then clones it for every real HTTP request. The immutable state
borrow lasts for the scope; a different AppState builds its own Router. Nested
scopes restore their parent and the Router is dropped before SQLx cleanup.
No Router or authorization result is cached across tests. Every request still
creates fresh middleware extensions and a unique request ID.

The original 343 HTTP calls are audited by kind: 1 detection, 1 creation,
50 forms-impact, 50 forms-save, 100 meanings-save, 50 audio checks, 50 audio confirmations, 1 batch publication,
20 list reads and 20 reference reads. All original data/assertions and the
four-concurrent content fixture remain in place. CI requires one construction,
this exact request inventory, 343 unique request IDs and the existing 190
request timing samples. Separate integration cases verify different-state
fallback/nested scope isolation, Swagger presence and live account revocation.
All original integration results must remain; the two new isolation cases are
additional coverage, with no new ignored cases. Compare maximum-batch fixture
walls and whole shard time with successful main run 38247710389.

## Current follow-up boundaries

The pagination case and fourteen measured slow handler cases also use the same
explicit, per-test, per-AppState Router scope. They retain their original HTTP
requests and assertions. Different-state and nested-scope behavior, Swagger,
authorization revocation, one construction and unique request IDs remain checked.
The three pagination companion targets run as separate complete test binaries;
they do not share a Router with the pagination case.

Pure documentation PRs under the explicit `docs/ci` / `docs/features` Markdown
allowlist retain all required matrix check names but mark runtime steps as not
applicable. Their service images are empty, so they start no test containers.
The aggregate still requires successful change classification and quality/audit
results. Main, manual runs, unknown changes and any mixed runtime/configuration
change retain the full seven-shard inventory and exactly-once execution audit.

See [the optimization plan and verification record](optimization-plan.md) for
measured results, the unit/doc dependency-cache pilot and the nextest evaluation.
