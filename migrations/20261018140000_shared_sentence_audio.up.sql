CREATE TABLE lexicon.shared_sentence_audio_asset_references (
    asset_id UUID NOT NULL REFERENCES lexicon.audio_assets(id) ON DELETE CASCADE,
    sentence_id UUID NOT NULL REFERENCES lexicon.shared_sentences(id) ON DELETE CASCADE,
    scope TEXT NOT NULL CHECK (scope IN ('draft', 'publication')),
    publication_id UUID,
    variant_id UUID NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    FOREIGN KEY (publication_id, sentence_id)
        REFERENCES lexicon.shared_sentence_publications(id, sentence_id) ON DELETE CASCADE,
    CHECK ((scope = 'draft' AND publication_id IS NULL)
        OR (scope = 'publication' AND publication_id IS NOT NULL))
);
CREATE UNIQUE INDEX shared_sentence_audio_draft_key
    ON lexicon.shared_sentence_audio_asset_references(asset_id) WHERE scope = 'draft';
CREATE UNIQUE INDEX shared_sentence_audio_publication_key
    ON lexicon.shared_sentence_audio_asset_references(asset_id, publication_id)
    WHERE scope = 'publication';
CREATE INDEX shared_sentence_audio_asset_idx
    ON lexicon.shared_sentence_audio_asset_references(asset_id);
CREATE INDEX shared_sentence_audio_owner_idx
    ON lexicon.shared_sentence_audio_asset_references(sentence_id, scope);

-- Serialize ownership claims across the two reference tables and reclamation.
CREATE OR REPLACE FUNCTION lexicon.reject_reclaiming_audio_reference() RETURNS trigger
LANGUAGE plpgsql AS $$
DECLARE reclaiming TIMESTAMPTZ;
BEGIN
    SELECT reclamation_started_at INTO reclaiming
    FROM lexicon.audio_assets WHERE id = NEW.asset_id FOR UPDATE;
    IF reclaiming IS NOT NULL THEN
        RAISE EXCEPTION 'audio asset is being reclaimed'
            USING ERRCODE = '23514', CONSTRAINT = 'audio_asset_not_reclaiming';
    END IF;
    IF TG_TABLE_NAME = 'v3_audio_asset_references' THEN
        IF EXISTS (SELECT 1 FROM lexicon.shared_sentence_audio_asset_references WHERE asset_id = NEW.asset_id)
            OR EXISTS (SELECT 1 FROM lexicon.v3_audio_asset_references WHERE asset_id = NEW.asset_id AND entry_id <> NEW.entry_id) THEN
            RAISE EXCEPTION 'audio asset belongs to other content'
                USING ERRCODE = '23514', CONSTRAINT = 'audio_asset_owner';
        END IF;
    ELSE
        IF EXISTS (SELECT 1 FROM lexicon.v3_audio_asset_references WHERE asset_id = NEW.asset_id)
            OR EXISTS (SELECT 1 FROM lexicon.shared_sentence_audio_asset_references WHERE asset_id = NEW.asset_id AND sentence_id <> NEW.sentence_id) THEN
            RAISE EXCEPTION 'audio asset belongs to other content'
                USING ERRCODE = '23514', CONSTRAINT = 'audio_asset_owner';
        END IF;
    END IF;
    RETURN NEW;
END;
$$;
CREATE TRIGGER audio_asset_not_reclaiming
BEFORE INSERT OR UPDATE OF asset_id, sentence_id ON lexicon.shared_sentence_audio_asset_references
FOR EACH ROW EXECUTE FUNCTION lexicon.reject_reclaiming_audio_reference();
