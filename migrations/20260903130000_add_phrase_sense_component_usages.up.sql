-- 成分用词改挂到释义（sense）而不是词形变体：同一短语的不同释义各自一份。
-- 列 / CHECK / 目标侧外键与 20260830060000 的变体级表一致，只把 owner 换成 sense_id。
-- owner 指向 lexicon.nodes 而不是 lexicon.senses：后者每次词义保存整表删重建。
CREATE TABLE lexicon.v3_phrase_sense_component_usages (
    id UUID PRIMARY KEY,
    entry_id UUID NOT NULL,
    sense_id UUID NOT NULL,
    ordinal SMALLINT NOT NULL
        CONSTRAINT lexicon_v3_phrase_components_ordinal_check
        CHECK (ordinal BETWEEN 0 AND 99),
    state TEXT NOT NULL
        CONSTRAINT lexicon_v3_phrase_components_state_check
        CHECK (state IN ('unresolved', 'resolved')),
    literal TEXT NOT NULL
        CONSTRAINT lexicon_v3_phrase_components_literal_check
        CHECK (
            literal = btrim(literal)
            AND char_length(literal) BETWEEN 1 AND 200
        ),
    target_entry_id UUID,
    target_publication_id UUID,
    target_pos_id UUID,
    target_base_form_id UUID,
    target_sense_id UUID,
    target_form_id UUID,
    target_variant_id UUID,
    target_dialect TEXT
        CONSTRAINT lexicon_v3_phrase_components_target_dialect_check
        CHECK (target_dialect IN ('common', 'uk', 'us')),
    target_form_type TEXT
        CONSTRAINT lexicon_v3_phrase_components_target_form_type_check
        CHECK (target_form_type IN (
            'base', 'present_participle', 'past_tense', 'past_participle',
            'third_person_singular', 'plural', 'comparative', 'superlative'
        )),
    target_headword_snapshot TEXT,
    target_gloss_snapshot TEXT,
    -- 自身节点与 owner 节点都随 lexicon.nodes 级联删除，和 v3_form_variants /
    -- v3_pronunciations 同款：nodes 行只在整条词条被硬删时才真删，此处用 RESTRICT
    -- 只会把「删草稿」变成 500（旧的变体级表就是这样，见 20260830060000）。
    CONSTRAINT lexicon_v3_phrase_components_node_fkey
        FOREIGN KEY (id, entry_id)
        REFERENCES lexicon.nodes(id, entry_id) ON DELETE CASCADE,
    CONSTRAINT lexicon_v3_phrase_components_owner_fkey
        FOREIGN KEY (sense_id, entry_id)
        REFERENCES lexicon.nodes(id, entry_id) ON DELETE CASCADE,
    CONSTRAINT lexicon_v3_phrase_components_publication_fkey
        FOREIGN KEY (target_publication_id, target_entry_id)
        REFERENCES lexicon.entry_publications(id, entry_id) ON DELETE RESTRICT,
    CONSTRAINT lexicon_v3_phrase_components_pos_fkey
        FOREIGN KEY (target_pos_id, target_entry_id)
        REFERENCES lexicon.nodes(id, entry_id) ON DELETE RESTRICT,
    CONSTRAINT lexicon_v3_phrase_components_base_fkey
        FOREIGN KEY (target_base_form_id, target_entry_id)
        REFERENCES lexicon.nodes(id, entry_id) ON DELETE RESTRICT,
    CONSTRAINT lexicon_v3_phrase_components_sense_fkey
        FOREIGN KEY (target_sense_id, target_entry_id)
        REFERENCES lexicon.nodes(id, entry_id) ON DELETE RESTRICT,
    CONSTRAINT lexicon_v3_phrase_components_form_fkey
        FOREIGN KEY (target_form_id, target_entry_id)
        REFERENCES lexicon.nodes(id, entry_id) ON DELETE RESTRICT,
    CONSTRAINT lexicon_v3_phrase_components_variant_fkey
        FOREIGN KEY (target_variant_id, target_entry_id)
        REFERENCES lexicon.nodes(id, entry_id) ON DELETE RESTRICT,
    CONSTRAINT lexicon_v3_phrase_components_publication_pos_fkey
        FOREIGN KEY (target_publication_id, target_entry_id, target_pos_id)
        REFERENCES lexicon.entry_publication_nodes(publication_id, entry_id, node_id)
        ON DELETE NO ACTION DEFERRABLE INITIALLY IMMEDIATE,
    CONSTRAINT lexicon_v3_phrase_components_publication_base_fkey
        FOREIGN KEY (target_publication_id, target_entry_id, target_base_form_id)
        REFERENCES lexicon.entry_publication_nodes(publication_id, entry_id, node_id)
        ON DELETE NO ACTION DEFERRABLE INITIALLY IMMEDIATE,
    CONSTRAINT lexicon_v3_phrase_components_publication_sense_fkey
        FOREIGN KEY (target_publication_id, target_entry_id, target_sense_id)
        REFERENCES lexicon.entry_publication_nodes(publication_id, entry_id, node_id)
        ON DELETE NO ACTION DEFERRABLE INITIALLY IMMEDIATE,
    CONSTRAINT lexicon_v3_phrase_components_publication_form_fkey
        FOREIGN KEY (target_publication_id, target_entry_id, target_form_id)
        REFERENCES lexicon.entry_publication_nodes(publication_id, entry_id, node_id)
        ON DELETE NO ACTION DEFERRABLE INITIALLY IMMEDIATE,
    CONSTRAINT lexicon_v3_phrase_components_publication_variant_fkey
        FOREIGN KEY (target_publication_id, target_entry_id, target_variant_id)
        REFERENCES lexicon.entry_publication_nodes(publication_id, entry_id, node_id)
        ON DELETE NO ACTION DEFERRABLE INITIALLY IMMEDIATE,
    CONSTRAINT lexicon_v3_phrase_sense_components_sense_ordinal_key
        UNIQUE (sense_id, ordinal),
    CONSTRAINT lexicon_v3_phrase_sense_components_sense_id_key
        UNIQUE (sense_id, id),
    CONSTRAINT lexicon_v3_phrase_components_shape_check CHECK (
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
    )
);

CREATE INDEX lexicon_v3_phrase_sense_components_entry_idx
    ON lexicon.v3_phrase_sense_component_usages (
        entry_id, sense_id, ordinal, id
    );

CREATE INDEX lexicon_v3_phrase_sense_components_target_idx
    ON lexicon.v3_phrase_sense_component_usages (
        target_entry_id, target_publication_id, target_sense_id
    )
    WHERE state = 'resolved';
