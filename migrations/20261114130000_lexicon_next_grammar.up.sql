ALTER TABLE lexicon_next.nodes DROP CONSTRAINT nodes_kind_check;
ALTER TABLE lexicon_next.nodes ADD CONSTRAINT nodes_kind_check
    CHECK (kind IN ('form', 'sense', 'definition', 'equivalent', 'sense_group',
                    'grammar', 'grammar_en', 'grammar_en_gb', 'grammar_en_us'));
