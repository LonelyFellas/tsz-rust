-- Stop writes/workers first. An old binary cannot read persisted English audio fields.
LOCK TABLE lexicon.audio_assets, lexicon.v3_audio_asset_references,
    lexicon.shared_sentence_audio_asset_references IN ACCESS EXCLUSIVE MODE;
DO $$ BEGIN
    IF EXISTS (SELECT 1 FROM lexicon.shared_sentence_audio_asset_references)
        OR EXISTS (SELECT 1 FROM lexicon.entry_editor_projection WHERE
            jsonb_path_exists(meanings, '$.pos[*].senses[*].definitions[*].content.**.audio_assets[*]')
            OR jsonb_path_exists(meanings, '$.pos[*].senses[*].sentences[*].en_text.**.audio_assets[*]'))
        OR EXISTS (SELECT 1 FROM lexicon.entry_publications WHERE
            jsonb_path_exists(snapshot, '$.meanings.pos[*].senses[*].definitions[*].content.**.audio_assets[*]')
            OR jsonb_path_exists(snapshot, '$.meanings.pos[*].senses[*].sentences[*].en_text.**.audio_assets[*]')) THEN
        RAISE EXCEPTION 'restore data without English audio before rolling back';
    END IF;
END $$;
DROP TABLE lexicon.shared_sentence_audio_asset_references;
CREATE OR REPLACE FUNCTION lexicon.reject_reclaiming_audio_reference() RETURNS trigger
LANGUAGE plpgsql AS $$
DECLARE reclaiming TIMESTAMPTZ;
BEGIN
    SELECT reclamation_started_at INTO reclaiming
    FROM lexicon.audio_assets WHERE id = NEW.asset_id FOR SHARE;
    IF reclaiming IS NOT NULL THEN
        RAISE EXCEPTION 'audio asset is being reclaimed'
            USING ERRCODE = '23514', CONSTRAINT = 'audio_asset_not_reclaiming';
    END IF;
    RETURN NEW;
END;
$$;
