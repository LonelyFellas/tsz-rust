-- Only live rows can safely regain the business foreign keys. Tombstones are
-- derived migration state and may refer to aggregates removed after the up
-- migration.
DELETE FROM lexicon.surface_sources source
WHERE source.is_deleted
  AND NOT EXISTS (
      SELECT 1 FROM lexicon.entries entry WHERE entry.id = source.entry_id
  );

ALTER TABLE lexicon.surface_sources
    ADD CONSTRAINT lexicon_surface_sources_entry_fkey
    FOREIGN KEY (entry_id) REFERENCES lexicon.entries(id) ON DELETE CASCADE;

ALTER TABLE lexicon.surface_sources
    ADD CONSTRAINT lexicon_surface_sources_publication_fkey
    FOREIGN KEY (publication_id, entry_id)
    REFERENCES lexicon.entry_publications(id, entry_id)
    ON DELETE CASCADE;
