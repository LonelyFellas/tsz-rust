-- 例句正文关联（text_link）与短语成分用词（phrase_component）也允许指向从未发布的草稿目标，
-- 与 20260822150000 给 relation 放开的 draft 范围同款：target_publication_id 为空、
-- target_revision 记目标当时的 entry revision，稳定 node 外键保护目标词义。
-- sentence_context 仍只允许 publication 范围。
ALTER TABLE lexicon.entry_publication_sense_refs
    DROP CONSTRAINT lexicon_publication_sense_refs_context_target_check,
    ADD CONSTRAINT lexicon_publication_sense_refs_context_target_check
        CHECK (
            reference_kind IN ('relation', 'text_link', 'phrase_component')
            OR target_content_scope = 'publication'
        );

-- 两张成分用词表：已解析成分的 target_publication_id 改为可空（草稿目标），
-- 其余目标字段仍必填；指向 lexicon.nodes 的外键继续保护草稿目标，
-- 指向 entry_publication_nodes 的外键在 publication 为空时自然不生效。
ALTER TABLE lexicon.v3_phrase_variant_component_usages
    DROP CONSTRAINT lexicon_v3_phrase_components_shape_check,
    ADD CONSTRAINT lexicon_v3_phrase_components_shape_check CHECK (
        (
            state = 'unresolved'
            AND target_entry_id IS NULL
            AND target_publication_id IS NULL
            AND target_pos_id IS NULL
            AND target_base_form_id IS NULL
            AND target_sense_id IS NULL
            AND target_form_id IS NULL
            AND target_variant_id IS NULL
            AND target_dialect IS NULL
            AND target_form_type IS NULL
            AND target_headword_snapshot IS NULL
            AND target_gloss_snapshot IS NULL
        ) OR (
            state = 'resolved'
            AND target_entry_id IS NOT NULL
            AND target_pos_id IS NOT NULL
            AND target_base_form_id IS NOT NULL
            AND target_sense_id IS NOT NULL
            AND target_form_id IS NOT NULL
            AND target_variant_id IS NOT NULL
            AND target_dialect IS NOT NULL
            AND target_form_type IS NOT NULL
            AND target_headword_snapshot IS NOT NULL
            AND target_gloss_snapshot IS NOT NULL
        )
    );

ALTER TABLE lexicon.v3_phrase_sense_component_usages
    DROP CONSTRAINT lexicon_v3_phrase_components_shape_check,
    ADD CONSTRAINT lexicon_v3_phrase_components_shape_check CHECK (
        (
            state = 'unresolved'
            AND target_entry_id IS NULL
            AND target_publication_id IS NULL
            AND target_pos_id IS NULL
            AND target_base_form_id IS NULL
            AND target_sense_id IS NULL
            AND target_form_id IS NULL
            AND target_variant_id IS NULL
            AND target_dialect IS NULL
            AND target_form_type IS NULL
            AND target_headword_snapshot IS NULL
            AND target_gloss_snapshot IS NULL
        ) OR (
            state = 'resolved'
            AND target_entry_id IS NOT NULL
            AND target_pos_id IS NOT NULL
            AND target_base_form_id IS NOT NULL
            AND target_sense_id IS NOT NULL
            AND target_form_id IS NOT NULL
            AND target_variant_id IS NOT NULL
            AND target_dialect IS NOT NULL
            AND target_form_type IS NOT NULL
            AND target_headword_snapshot IS NOT NULL
            AND target_gloss_snapshot IS NOT NULL
        )
    );
