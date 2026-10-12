# Wordlist autocomplete benchmark

Requires Python 3, psql, and a locally running PostgreSQL with project migrations
applied. The driver refuses non-loopback databases and names without the
`tsz_wordlist_search_` prefix. `seed` requires an empty lexicon and never deletes
existing rows. Keep this database separate from development/acceptance data.

1. Create an isolated database (for example `tsz_wordlist_search_perf`) and set
   `DATABASE_URL` to its local URL. Run the normal `seed` binary with
   `SEED_ADMIN_PHONE=19900008888` and a disposable password; this applies migrations
   and provisions a synthetic super administrator.
2. Populate synthetic publications and current surface projections:

   ```sh
   python3 ops/wordlist-search/benchmark.py seed \
     --template ops/wordlist-search/publication-template.json --output seed.json
   ```

   This bulk fixture is for read/query performance, not a test of publication
   writes. It creates 250,000 entries, 250,000 snapshots (with irrelevant large
   annotations), and 600,000 surfaces: two dialect scopes per entry, plus plural
   forms for 20%. It includes 25,000 `pre…` entries and 25,000 `look forward to …`
   phrases. The remaining synthetic spellings are deterministic hash-derived
   alphabetic strings. This is deliberately skewed data, not an English corpus.
3. Start the backend binary against that database on a dedicated local port,
   with its own Redis DB. Keep the normal 10-connection database pool. Record
   the binary hash/profile; do not run compilation or other test suites during
   timing. Obtain `BENCH_TOKEN` through a normal local admin login, then export
   it without printing it. No credentials are stored in benchmark reports.
4. Inspect the exact SQL used by the implementation and exercise authenticated
   HTTP requests:

   ```sh
   python3 ops/wordlist-search/benchmark.py explain --output plans.json
   python3 ops/wordlist-search/benchmark.py http \
     --url http://127.0.0.1:8499/api/v1/admin/wordlists/suggestions \
     --concurrency 1 8 32 64 --per-worker 160 --output http.json
   ```

The HTTP run warms caches/prepared statements, then uses persistent connections
and closed-loop workers. It covers exact/prefix/short-word searches, word and
phrase typos, misses, literal wildcard characters, dense prefixes and 128 varied
entry spellings/prefixes. Each response is checked for status, a bounded and
unique item list, and expected matches; errors are not dropped from timings.
Reports give P50/P95/P99, maxima, throughput, response sizes and per-case results.
`EXPLAIN (ANALYZE, BUFFERS)` reports are database-only timings; HTTP measurements
also include authentication, connection-pool queuing and response processing.

The authoritative task report and checked results are in the frontend repository
`docs/features/admin-wordlist-management/assessment.md` and its `performance/`
folder. Local warm-cache measurements are not a production capacity guarantee.

The same shared search core serves related-entry autocomplete. To verify it,
use `/api/v1/admin/lexicon/entries/related-suggestions` as `--url` and optionally
pass `--exclude-entry-id <current-entry-uuid>`. Use a separate dedicated database
such as `tsz_wordlist_search_related`. Sense summaries are fetched only after
selection through `/api/v1/admin/lexicon/entries/{id}/relation-senses`; they are
not part of autocomplete payloads or this autocomplete throughput measurement.
