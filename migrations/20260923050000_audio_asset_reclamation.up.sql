-- 对象删除结果不确定时，保留可重试的记录，但不能重新开放引用。
ALTER TABLE lexicon.audio_assets ADD COLUMN reclamation_started_at TIMESTAMPTZ;

CREATE FUNCTION lexicon.reject_reclaiming_audio_reference() RETURNS trigger
LANGUAGE plpgsql AS $$
DECLARE
    reclaiming TIMESTAMPTZ;
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

CREATE TRIGGER audio_asset_not_reclaiming
BEFORE INSERT OR UPDATE OF asset_id ON lexicon.v3_audio_asset_references
FOR EACH ROW EXECUTE FUNCTION lexicon.reject_reclaiming_audio_reference();
