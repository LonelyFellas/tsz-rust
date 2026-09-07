-- 音频资产的引用关系。草稿引用每次保存词义时整条重建，发布引用在发布时写入且不再变动。
--
-- 为什么不直接查 JSONB：草稿在 entry_editor_projection.meanings、发布在 entry_publications.snapshot，
-- 两处都能用 jsonb_path_exists 问出来，但那种查询对内容结构是「静默」的——路径写错或结构调整后
-- 它只会返回「没有引用」，而这个答案会让回收删掉仍被发布引用的录音，且不可恢复。
-- 侧表由遍历内容的同一份 Rust 代码写入，结构变了编译期就会暴露。
CREATE TABLE lexicon.v3_audio_asset_references (
    asset_id UUID NOT NULL
        CONSTRAINT lexicon_v3_audio_refs_asset_fkey
        REFERENCES lexicon.audio_assets(id) ON DELETE CASCADE,
    entry_id UUID NOT NULL
        CONSTRAINT lexicon_v3_audio_refs_entry_fkey
        REFERENCES lexicon.entries(id) ON DELETE CASCADE,
    scope TEXT NOT NULL
        CONSTRAINT lexicon_v3_audio_refs_scope_check CHECK (scope IN ('draft', 'publication')),
    -- 发布引用指向具体的那一次发布：激活历史发布不受草稿改动影响，回收也就不能只看草稿。
    publication_id UUID
        CONSTRAINT lexicon_v3_audio_refs_publication_fkey
        REFERENCES lexicon.entry_publications(id) ON DELETE CASCADE,
    -- 变体 id 只作排障用，不参与任何回收判定；带上它是为了出问题时能定位到具体挂点。
    variant_id UUID NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    CONSTRAINT lexicon_v3_audio_refs_shape_check CHECK (
        (scope = 'draft' AND publication_id IS NULL)
        OR (scope = 'publication' AND publication_id IS NOT NULL)
    )
);

-- 同一资产在一条草稿里只记一行：多个变体引用同一段录音时不重复计数，回收只关心「有没有人引用」。
CREATE UNIQUE INDEX lexicon_v3_audio_refs_draft_key
    ON lexicon.v3_audio_asset_references (asset_id)
    WHERE scope = 'draft';

CREATE UNIQUE INDEX lexicon_v3_audio_refs_publication_key
    ON lexicon.v3_audio_asset_references (asset_id, publication_id)
    WHERE scope = 'publication';

-- 保存词义时按 entry_id 整条删掉草稿引用再重建。
CREATE INDEX lexicon_v3_audio_refs_entry_idx
    ON lexicon.v3_audio_asset_references (entry_id, scope);
