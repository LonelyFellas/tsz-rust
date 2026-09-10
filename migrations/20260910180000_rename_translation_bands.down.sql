ALTER TABLE lexicon.text_variants
    DROP CONSTRAINT lexicon_text_variants_field_role_check;
DROP INDEX lexicon.lexicon_text_variants_slot_key;

UPDATE lexicon.text_variants SET field_role = 'zh_translation_c1_c2'
WHERE field_role = 'zh_translation_word_for_word';
UPDATE lexicon.text_variants SET field_role = 'zh_translation_b1_b2'
WHERE field_role = 'zh_translation_balanced_fluency';
UPDATE lexicon.text_variants SET field_role = 'zh_translation_a1_a2'
WHERE field_role = 'zh_translation_adapted_creation';

CREATE UNIQUE INDEX lexicon_text_variants_slot_key
    ON lexicon.text_variants (owner_node_id, field_role, language, dialect)
    WHERE field_role NOT IN (
        'zh_translation_a1_a2', 'zh_translation_b1_b2', 'zh_translation_c1_c2'
    );
ALTER TABLE lexicon.text_variants
    ADD CONSTRAINT lexicon_text_variants_field_role_check CHECK (
        field_role IN (
            'content', 'en_text', 'zh_text',
            'zh_translation_a1_a2',
            'zh_translation_b1_b2',
            'zh_translation_c1_c2'
        )
    );

