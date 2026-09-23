DO $$ BEGIN
    IF EXISTS (SELECT 1 FROM lexicon.shared_sentences)
        OR EXISTS (SELECT 1 FROM lexicon.shared_sentence_publications)
        OR EXISTS (SELECT 1 FROM lexicon.shared_sentence_draft_hides)
        OR EXISTS (SELECT 1 FROM lexicon.shared_sentence_publication_hides) THEN
        RAISE EXCEPTION 'cannot undo shared sentence drafts, publication history or visibility state';
    END IF;
END $$;

DROP TABLE lexicon.shared_sentence_publication_hides;
DROP TABLE lexicon.shared_sentence_draft_hides;
DROP TABLE lexicon.shared_sentence_publication_annotations;
ALTER TABLE lexicon.shared_sentences DROP CONSTRAINT shared_sentence_current_publication_fkey;
DROP TABLE lexicon.shared_sentence_publications;
ALTER TABLE lexicon.shared_sentences
    DROP CONSTRAINT shared_sentence_withdrawn_shape,
    DROP COLUMN withdrawn_reason,
    DROP COLUMN withdrawn_at,
    DROP COLUMN current_publication_id,
    DROP COLUMN lifecycle_revision;
