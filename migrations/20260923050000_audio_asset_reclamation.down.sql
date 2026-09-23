-- 须停止写入和回收 worker 后回退；不能丢弃结果未知的对象删除记录。
LOCK TABLE lexicon.audio_assets, lexicon.v3_audio_asset_references IN ACCESS EXCLUSIVE MODE;
DO $$
BEGIN
    IF EXISTS (SELECT 1 FROM lexicon.audio_assets WHERE reclamation_started_at IS NOT NULL) THEN
        RAISE EXCEPTION 'cannot remove reclamation state while audio reclamation is pending';
    END IF;
END;
$$;
DROP TRIGGER audio_asset_not_reclaiming ON lexicon.v3_audio_asset_references;
DROP FUNCTION lexicon.reject_reclaiming_audio_reference();
ALTER TABLE lexicon.audio_assets DROP COLUMN reclamation_started_at;
