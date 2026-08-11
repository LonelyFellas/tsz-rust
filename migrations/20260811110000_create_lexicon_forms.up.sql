CREATE TABLE lexicon.entry_pos (
    id UUID PRIMARY KEY,
    entry_id UUID NOT NULL,
    part_of_speech_id UUID NOT NULL,
    spelling_mode TEXT NOT NULL
        CONSTRAINT lexicon_entry_pos_spelling_mode_check CHECK (spelling_mode IN ('unified', 'distinguish')),
    phonetic_mode TEXT NOT NULL
        CONSTRAINT lexicon_entry_pos_phonetic_mode_check CHECK (phonetic_mode IN ('unified', 'distinguish')),
    sort_order INTEGER NOT NULL
        CONSTRAINT lexicon_entry_pos_sort_order_check CHECK (sort_order >= 0),
    CONSTRAINT lexicon_entry_pos_node_fkey
        FOREIGN KEY (id, entry_id) REFERENCES lexicon.nodes(id, entry_id) ON DELETE CASCADE,
    CONSTRAINT lexicon_entry_pos_catalog_pos_fkey
        FOREIGN KEY (part_of_speech_id) REFERENCES catalog.parts_of_speech(id) ON DELETE RESTRICT,
    CONSTRAINT lexicon_entry_pos_id_entry_key UNIQUE (id, entry_id),
    CONSTRAINT lexicon_entry_pos_entry_part_key UNIQUE (entry_id, part_of_speech_id),
    CONSTRAINT lexicon_entry_pos_modes_check CHECK (
        spelling_mode <> 'distinguish' OR phonetic_mode = 'distinguish'
    )
);

CREATE INDEX lexicon_entry_pos_entry_idx ON lexicon.entry_pos (entry_id, sort_order, id);
CREATE INDEX lexicon_entry_pos_catalog_idx
    ON lexicon.entry_pos (part_of_speech_id, entry_id);

CREATE TABLE lexicon.form_groups (
    id UUID PRIMARY KEY,
    entry_id UUID NOT NULL,
    entry_pos_id UUID NOT NULL,
    is_regular BOOLEAN NOT NULL,
    sort_order INTEGER NOT NULL
        CONSTRAINT lexicon_form_groups_sort_order_check CHECK (sort_order >= 0),
    CONSTRAINT lexicon_form_groups_node_fkey
        FOREIGN KEY (id, entry_id) REFERENCES lexicon.nodes(id, entry_id) ON DELETE CASCADE,
    CONSTRAINT lexicon_form_groups_pos_fkey
        FOREIGN KEY (entry_pos_id, entry_id) REFERENCES lexicon.entry_pos(id, entry_id) ON DELETE CASCADE,
    CONSTRAINT lexicon_form_groups_id_entry_key UNIQUE (id, entry_id)
);

CREATE INDEX lexicon_form_groups_entry_idx ON lexicon.form_groups (entry_id, entry_pos_id, sort_order, id);

CREATE TABLE lexicon.form_slots (
    id UUID PRIMARY KEY,
    entry_id UUID NOT NULL,
    entry_pos_id UUID NOT NULL,
    form_group_id UUID,
    form_type TEXT NOT NULL
        CONSTRAINT lexicon_form_slots_type_check CHECK (form_type IN (
            'base', 'present_participle', 'past_tense', 'past_participle',
            'third_person_singular', 'plural', 'comparative', 'superlative'
        )),
    sort_order INTEGER NOT NULL
        CONSTRAINT lexicon_form_slots_sort_order_check CHECK (sort_order >= 0),
    CONSTRAINT lexicon_form_slots_node_fkey
        FOREIGN KEY (id, entry_id) REFERENCES lexicon.nodes(id, entry_id) ON DELETE CASCADE,
    CONSTRAINT lexicon_form_slots_pos_fkey
        FOREIGN KEY (entry_pos_id, entry_id) REFERENCES lexicon.entry_pos(id, entry_id) ON DELETE CASCADE,
    CONSTRAINT lexicon_form_slots_group_fkey
        FOREIGN KEY (form_group_id, entry_id) REFERENCES lexicon.form_groups(id, entry_id) ON DELETE CASCADE,
    CONSTRAINT lexicon_form_slots_id_entry_key UNIQUE (id, entry_id),
    CONSTRAINT lexicon_form_slots_group_shape_check CHECK (
        (form_type = 'base' AND form_group_id IS NULL)
        OR (form_type <> 'base' AND form_group_id IS NOT NULL)
    )
);

CREATE UNIQUE INDEX lexicon_form_slots_one_base_idx
    ON lexicon.form_slots (entry_pos_id) WHERE form_type = 'base';
CREATE INDEX lexicon_form_slots_entry_idx
    ON lexicon.form_slots (entry_id, entry_pos_id, form_group_id, sort_order, id);

CREATE TABLE lexicon.form_variants (
    id UUID PRIMARY KEY,
    entry_id UUID NOT NULL,
    form_slot_id UUID NOT NULL,
    dialect TEXT NOT NULL
        CONSTRAINT lexicon_form_variants_dialect_check CHECK (dialect IN ('common', 'uk', 'us')),
    spelling TEXT NOT NULL,
    origin TEXT NOT NULL
        CONSTRAINT lexicon_form_variants_origin_check CHECK (origin IN ('dictionary', 'converted', 'manual')),
    sort_order INTEGER NOT NULL
        CONSTRAINT lexicon_form_variants_sort_order_check CHECK (sort_order >= 0),
    CONSTRAINT lexicon_form_variants_node_fkey
        FOREIGN KEY (id, entry_id) REFERENCES lexicon.nodes(id, entry_id) ON DELETE CASCADE,
    CONSTRAINT lexicon_form_variants_slot_fkey
        FOREIGN KEY (form_slot_id, entry_id) REFERENCES lexicon.form_slots(id, entry_id) ON DELETE CASCADE,
    CONSTRAINT lexicon_form_variants_id_entry_key UNIQUE (id, entry_id),
    CONSTRAINT lexicon_form_variants_slot_dialect_key UNIQUE (form_slot_id, dialect),
    CONSTRAINT lexicon_form_variants_spelling_check CHECK (
        spelling = btrim(spelling) AND char_length(spelling) <= 200
    )
);

CREATE INDEX lexicon_form_variants_entry_idx
    ON lexicon.form_variants (entry_id, form_slot_id, sort_order, id);

CREATE TABLE lexicon.pronunciations (
    id UUID PRIMARY KEY,
    entry_id UUID NOT NULL,
    form_variant_id UUID NOT NULL,
    dict_phonetic TEXT NOT NULL,
    actual_pron TEXT NOT NULL,
    style TEXT NOT NULL
        CONSTRAINT lexicon_pronunciations_style_check CHECK (style IN ('normal', 'strong', 'weak')),
    sort_order INTEGER NOT NULL
        CONSTRAINT lexicon_pronunciations_sort_order_check CHECK (sort_order >= 0),
    CONSTRAINT lexicon_pronunciations_node_fkey
        FOREIGN KEY (id, entry_id) REFERENCES lexicon.nodes(id, entry_id) ON DELETE CASCADE,
    CONSTRAINT lexicon_pronunciations_variant_fkey
        FOREIGN KEY (form_variant_id, entry_id) REFERENCES lexicon.form_variants(id, entry_id) ON DELETE CASCADE,
    CONSTRAINT lexicon_pronunciations_id_entry_key UNIQUE (id, entry_id),
    CONSTRAINT lexicon_pronunciations_lengths_check CHECK (
        char_length(dict_phonetic) <= 200 AND char_length(actual_pron) <= 200
    )
);

CREATE INDEX lexicon_pronunciations_entry_idx
    ON lexicon.pronunciations (entry_id, form_variant_id, sort_order, id);
