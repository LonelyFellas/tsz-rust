CREATE TABLE lexicon.sense_groups (
    id UUID PRIMARY KEY,
    entry_id UUID NOT NULL,
    name_zh TEXT NOT NULL,
    name_en TEXT NOT NULL,
    sort_order INTEGER NOT NULL
        CONSTRAINT lexicon_sense_groups_sort_order_check CHECK (sort_order >= 0),
    CONSTRAINT lexicon_sense_groups_node_fkey
        FOREIGN KEY (id, entry_id) REFERENCES lexicon.nodes(id, entry_id) ON DELETE CASCADE,
    CONSTRAINT lexicon_sense_groups_id_entry_key UNIQUE (id, entry_id),
    CONSTRAINT lexicon_sense_groups_name_lengths_check CHECK (
        char_length(name_zh) <= 200 AND char_length(name_en) <= 200
    )
);

CREATE INDEX lexicon_sense_groups_entry_idx ON lexicon.sense_groups (entry_id, sort_order, id);

CREATE TABLE lexicon.grammar_structures (
    id UUID PRIMARY KEY,
    entry_id UUID NOT NULL,
    entry_pos_id UUID NOT NULL,
    sort_order INTEGER NOT NULL
        CONSTRAINT lexicon_grammar_structures_sort_order_check CHECK (sort_order >= 0),
    CONSTRAINT lexicon_grammar_structures_node_fkey
        FOREIGN KEY (id, entry_id) REFERENCES lexicon.nodes(id, entry_id) ON DELETE CASCADE,
    CONSTRAINT lexicon_grammar_structures_pos_fkey
        FOREIGN KEY (entry_pos_id, entry_id) REFERENCES lexicon.entry_pos(id, entry_id) ON DELETE CASCADE,
    CONSTRAINT lexicon_grammar_structures_id_entry_key UNIQUE (id, entry_id)
);

CREATE INDEX lexicon_grammar_structures_entry_idx
    ON lexicon.grammar_structures (entry_id, entry_pos_id, sort_order, id);

CREATE TABLE lexicon.senses (
    id UUID PRIMARY KEY,
    entry_id UUID NOT NULL,
    entry_pos_id UUID NOT NULL,
    sub_part_of_speech_id UUID,
    sense_group_id UUID,
    level TEXT NOT NULL
        CONSTRAINT lexicon_senses_level_check CHECK (level IN ('A1', 'A2', 'B1', 'B2', 'C1', 'C2')),
    frequency NUMERIC(5, 2)
        CONSTRAINT lexicon_senses_frequency_check CHECK (frequency BETWEEN 0 AND 100),
    depends_on_context BOOLEAN NOT NULL,
    sort_order INTEGER NOT NULL
        CONSTRAINT lexicon_senses_sort_order_check CHECK (sort_order >= 0),
    CONSTRAINT lexicon_senses_node_fkey
        FOREIGN KEY (id, entry_id) REFERENCES lexicon.nodes(id, entry_id) ON DELETE CASCADE,
    CONSTRAINT lexicon_senses_pos_fkey
        FOREIGN KEY (entry_pos_id, entry_id) REFERENCES lexicon.entry_pos(id, entry_id) ON DELETE CASCADE,
    CONSTRAINT lexicon_senses_group_fkey
        FOREIGN KEY (sense_group_id, entry_id) REFERENCES lexicon.sense_groups(id, entry_id) ON DELETE RESTRICT,
    CONSTRAINT lexicon_senses_catalog_sub_pos_fkey
        FOREIGN KEY (sub_part_of_speech_id) REFERENCES catalog.sub_parts_of_speech(id) ON DELETE RESTRICT,
    CONSTRAINT lexicon_senses_id_entry_key UNIQUE (id, entry_id)
);

CREATE INDEX lexicon_senses_entry_idx ON lexicon.senses (entry_id, entry_pos_id, sort_order, id);
CREATE INDEX lexicon_senses_sub_pos_idx ON lexicon.senses (sub_part_of_speech_id);

CREATE TABLE lexicon.definitions (
    id UUID PRIMARY KEY,
    entry_id UUID NOT NULL,
    sense_id UUID NOT NULL,
    level TEXT NOT NULL
        CONSTRAINT lexicon_definitions_level_check CHECK (level IN ('A1', 'A2', 'B1', 'B2', 'C1', 'C2')),
    definition_kind TEXT NOT NULL
        CONSTRAINT lexicon_definitions_kind_check CHECK (definition_kind IN ('definition', 'sentence')),
    language TEXT NOT NULL
        CONSTRAINT lexicon_definitions_language_check CHECK (language IN ('zh', 'en')),
    grammar_structure_id UUID,
    sort_order INTEGER NOT NULL
        CONSTRAINT lexicon_definitions_sort_order_check CHECK (sort_order >= 0),
    CONSTRAINT lexicon_definitions_node_fkey
        FOREIGN KEY (id, entry_id) REFERENCES lexicon.nodes(id, entry_id) ON DELETE CASCADE,
    CONSTRAINT lexicon_definitions_sense_fkey
        FOREIGN KEY (sense_id, entry_id) REFERENCES lexicon.senses(id, entry_id) ON DELETE CASCADE,
    CONSTRAINT lexicon_definitions_grammar_fkey
        FOREIGN KEY (grammar_structure_id, entry_id) REFERENCES lexicon.grammar_structures(id, entry_id) ON DELETE RESTRICT,
    CONSTRAINT lexicon_definitions_id_entry_key UNIQUE (id, entry_id)
);

CREATE INDEX lexicon_definitions_entry_idx ON lexicon.definitions (entry_id, sense_id, sort_order, id);

CREATE TABLE lexicon.sentences (
    id UUID PRIMARY KEY,
    entry_id UUID NOT NULL,
    sense_id UUID NOT NULL,
    level TEXT NOT NULL
        CONSTRAINT lexicon_sentences_level_check CHECK (level IN ('A1', 'A2', 'B1', 'B2', 'C1', 'C2')),
    sort_order INTEGER NOT NULL
        CONSTRAINT lexicon_sentences_sort_order_check CHECK (sort_order >= 0),
    CONSTRAINT lexicon_sentences_node_fkey
        FOREIGN KEY (id, entry_id) REFERENCES lexicon.nodes(id, entry_id) ON DELETE CASCADE,
    CONSTRAINT lexicon_sentences_sense_fkey
        FOREIGN KEY (sense_id, entry_id) REFERENCES lexicon.senses(id, entry_id) ON DELETE CASCADE,
    CONSTRAINT lexicon_sentences_id_entry_key UNIQUE (id, entry_id)
);

CREATE INDEX lexicon_sentences_entry_idx ON lexicon.sentences (entry_id, sense_id, sort_order, id);

CREATE TABLE lexicon.text_variants (
    id UUID PRIMARY KEY,
    entry_id UUID NOT NULL,
    owner_node_id UUID NOT NULL,
    field_role TEXT NOT NULL
        CONSTRAINT lexicon_text_variants_field_role_check CHECK (field_role IN ('content', 'en_text', 'zh_text')),
    language TEXT NOT NULL
        CONSTRAINT lexicon_text_variants_language_check CHECK (language IN ('en', 'zh')),
    dialect TEXT NOT NULL
        CONSTRAINT lexicon_text_variants_dialect_check CHECK (dialect IN ('common', 'uk', 'us')),
    rich_text_version SMALLINT NOT NULL
        CONSTRAINT lexicon_text_variants_version_check CHECK (rich_text_version IN (1, 2)),
    content JSONB NOT NULL,
    plain_text TEXT NOT NULL,
    content_hash BYTEA NOT NULL,
    origin TEXT NOT NULL
        CONSTRAINT lexicon_text_variants_origin_check CHECK (origin IN ('dictionary', 'converted', 'manual')),
    sort_order INTEGER NOT NULL
        CONSTRAINT lexicon_text_variants_sort_order_check CHECK (sort_order >= 0),
    CONSTRAINT lexicon_text_variants_node_fkey
        FOREIGN KEY (id, entry_id) REFERENCES lexicon.nodes(id, entry_id) ON DELETE CASCADE,
    CONSTRAINT lexicon_text_variants_owner_fkey
        FOREIGN KEY (owner_node_id, entry_id) REFERENCES lexicon.nodes(id, entry_id) ON DELETE CASCADE,
    CONSTRAINT lexicon_text_variants_id_entry_key UNIQUE (id, entry_id),
    CONSTRAINT lexicon_text_variants_slot_key UNIQUE (owner_node_id, field_role, language, dialect),
    CONSTRAINT lexicon_text_variants_plain_text_length_check CHECK (char_length(plain_text) <= 5000)
);

CREATE INDEX lexicon_text_variants_entry_idx
    ON lexicon.text_variants (entry_id, owner_node_id, sort_order, id);

CREATE TABLE lexicon.sentence_links (
    sentence_id UUID NOT NULL,
    entry_id UUID NOT NULL,
    target_entry_id UUID NOT NULL,
    target_sense_id UUID NOT NULL,
    role TEXT NOT NULL
        CONSTRAINT lexicon_sentence_links_role_check CHECK (role IN ('focus', 'context')),
    sort_order INTEGER NOT NULL
        CONSTRAINT lexicon_sentence_links_sort_order_check CHECK (sort_order >= 0),
    PRIMARY KEY (sentence_id, target_entry_id, target_sense_id),
    CONSTRAINT lexicon_sentence_links_sentence_fkey
        FOREIGN KEY (sentence_id, entry_id) REFERENCES lexicon.sentences(id, entry_id) ON DELETE CASCADE,
    CONSTRAINT lexicon_sentence_links_target_fkey
        FOREIGN KEY (target_sense_id, target_entry_id) REFERENCES lexicon.nodes(id, entry_id) ON DELETE RESTRICT
);

CREATE UNIQUE INDEX lexicon_sentence_links_one_focus_idx
    ON lexicon.sentence_links (sentence_id) WHERE role = 'focus';
CREATE INDEX lexicon_sentence_links_target_idx
    ON lexicon.sentence_links (target_entry_id, target_sense_id);

CREATE TABLE lexicon.relations (
    id UUID PRIMARY KEY,
    entry_id UUID NOT NULL,
    source_sense_id UUID NOT NULL,
    relation_type TEXT NOT NULL
        CONSTRAINT lexicon_relations_type_check CHECK (relation_type IN ('synonym', 'antonym', 'derivative')),
    target_entry_id UUID NOT NULL,
    target_sense_id UUID NOT NULL,
    score NUMERIC(5, 2) NOT NULL
        CONSTRAINT lexicon_relations_score_check CHECK (score BETWEEN 0 AND 100),
    target_headword_snapshot TEXT NOT NULL,
    target_gloss_snapshot TEXT NOT NULL,
    sort_order INTEGER NOT NULL
        CONSTRAINT lexicon_relations_sort_order_check CHECK (sort_order >= 0),
    CONSTRAINT lexicon_relations_node_fkey
        FOREIGN KEY (id, entry_id) REFERENCES lexicon.nodes(id, entry_id) ON DELETE CASCADE,
    CONSTRAINT lexicon_relations_source_fkey
        FOREIGN KEY (source_sense_id, entry_id) REFERENCES lexicon.senses(id, entry_id) ON DELETE CASCADE,
    CONSTRAINT lexicon_relations_target_fkey
        FOREIGN KEY (target_sense_id, target_entry_id) REFERENCES lexicon.nodes(id, entry_id) ON DELETE RESTRICT,
    CONSTRAINT lexicon_relations_id_entry_key UNIQUE (id, entry_id)
);

CREATE INDEX lexicon_relations_entry_idx ON lexicon.relations (entry_id, source_sense_id, sort_order, id);
CREATE INDEX lexicon_relations_target_idx ON lexicon.relations (target_entry_id, target_sense_id);
