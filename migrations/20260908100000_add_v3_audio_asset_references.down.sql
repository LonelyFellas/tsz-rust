DO $$
BEGIN
    IF EXISTS (SELECT 1 FROM lexicon.v3_audio_asset_references) THEN
        RAISE EXCEPTION 'cannot remove V3 audio asset references while rows exist'
            USING ERRCODE = '0A000';
    END IF;
END
$$;

DROP TABLE lexicon.v3_audio_asset_references;
