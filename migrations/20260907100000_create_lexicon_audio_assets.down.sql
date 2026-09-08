DO $$
BEGIN
    IF EXISTS (SELECT 1 FROM lexicon.audio_assets) THEN
        RAISE EXCEPTION 'cannot remove audio assets while rows exist'
            USING ERRCODE = '0A000';
    END IF;
END
$$;

DROP TABLE lexicon.audio_assets;
