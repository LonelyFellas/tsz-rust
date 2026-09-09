-- 回退前须先处理 text_link / phrase_component 的 draft 范围引用行，以及两张成分表里
-- target_publication_id 为空的已解析成分，否则 CHECK 会失败。
ALTER TABLE lexicon.entry_publication_sense_refs
    DROP CONSTRAINT lexicon_publication_sense_refs_context_target_check,
    ADD CONSTRAINT lexicon_publication_sense_refs_context_target_check
        CHECK (
            reference_kind = 'relation'
            OR target_content_scope = 'publication'
        );

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
            AND target_publication_id IS NOT NULL
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
            AND target_publication_id IS NOT NULL
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
