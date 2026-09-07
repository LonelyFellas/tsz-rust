-- 管理员直传的真人录音资产。对象键由服务端生成，客户端文件名只作展示元数据。
-- 本次仅登记资产本身；与词条节点的引用关系、发布快照与回收由后续 PR 建立。
CREATE TABLE lexicon.audio_assets (
    id UUID PRIMARY KEY,
    object_key TEXT NOT NULL UNIQUE,
    -- 晋升前的暂存键。唯一约束把 confirm 变成幂等的：201 响应丢在网络上时前端会重试，
    -- 重试必须拿回同一条资产，而不是再登记一份（`assets/` 前缀本期没有回收方，多出来的对象没人清）。
    source_key TEXT NOT NULL
        CONSTRAINT lexicon_audio_assets_source_key_unique UNIQUE,
    content_type TEXT NOT NULL,
    size_bytes BIGINT NOT NULL,
    -- 探测时长要解码音频，本次不做；列先留着，补探测时不必再改表。
    duration_ms INTEGER,
    locale TEXT NOT NULL,
    gender TEXT NOT NULL,
    original_name TEXT NOT NULL,
    created_by_admin_id UUID NOT NULL
        CONSTRAINT lexicon_audio_assets_admin_fkey
        REFERENCES admins(id) ON DELETE RESTRICT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    CONSTRAINT lexicon_audio_assets_object_key_nonempty CHECK (object_key <> ''),
    CONSTRAINT lexicon_audio_assets_source_key_nonempty CHECK (source_key <> ''),
    CONSTRAINT lexicon_audio_assets_content_type CHECK (
        content_type IN ('audio/mpeg', 'audio/mp4', 'audio/wav', 'audio/ogg')
    ),
    CONSTRAINT lexicon_audio_assets_size_positive CHECK (size_bytes > 0),
    CONSTRAINT lexicon_audio_assets_duration_positive CHECK (duration_ms IS NULL OR duration_ms > 0),
    CONSTRAINT lexicon_audio_assets_locale CHECK (locale IN ('en-GB', 'en-US')),
    CONSTRAINT lexicon_audio_assets_gender CHECK (gender IN ('female', 'male')),
    CONSTRAINT lexicon_audio_assets_original_name_length CHECK (
        char_length(original_name) BETWEEN 1 AND 120
    )
);
