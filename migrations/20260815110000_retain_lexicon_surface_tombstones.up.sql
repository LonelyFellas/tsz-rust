-- Surface rows are a rebuildable projection and tombstones must outlive a
-- deleted business aggregate until backfill/catch-up has crossed their event
-- offset.  Referential integrity is therefore enforced by the writer and the
-- parity job, rather than cascading away the evidence that prevents a stale
-- backfill from resurrecting a deleted source.
ALTER TABLE lexicon.surface_sources
    DROP CONSTRAINT lexicon_surface_sources_entry_fkey;

ALTER TABLE lexicon.surface_sources
    DROP CONSTRAINT lexicon_surface_sources_publication_fkey;

