-- Independent, normalized associations for lexicon-next grammar and sentence content.
-- The source nodes belong to lexicon_next. Targets carry an explicit source
-- namespace so V3 and lexicon-next nodes can coexist in one relation table.
CREATE TABLE lexicon_next.grammar_form_links (
    id uuid PRIMARY KEY,
    entry_id uuid NOT NULL REFERENCES lexicon_next.entries(id) ON DELETE CASCADE,
    source_variant_id uuid NOT NULL,
    source_start integer NOT NULL CHECK (source_start >= 0),
    source_end integer NOT NULL CHECK (source_end > source_start),
    source_surface text NOT NULL CHECK (source_surface = btrim(source_surface) AND char_length(source_surface) BETWEEN 1 AND 200),
    target_source text NOT NULL DEFAULT 'v3' CHECK (target_source IN ('v3','lexicon_next')),
    target_word_id uuid NOT NULL,
    target_publication_id uuid,
    target_pos_id uuid NOT NULL,
    target_form_id uuid NOT NULL,
    target_variant_id uuid NOT NULL,
    target_dialect text NOT NULL CHECK (target_dialect IN ('common','uk','us')),
    reading_reference jsonb,
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now(),
    UNIQUE (source_variant_id, source_start),
    FOREIGN KEY (source_variant_id, entry_id)
        REFERENCES lexicon_next.nodes(id, entry_id) ON DELETE CASCADE
);
CREATE INDEX lexicon_next_grammar_form_links_entry_idx
    ON lexicon_next.grammar_form_links(entry_id, source_variant_id, source_start);
CREATE INDEX lexicon_next_grammar_form_links_target_idx
    ON lexicon_next.grammar_form_links(target_word_id, target_form_id, target_variant_id);

CREATE TABLE lexicon_next.sentence_associations (
    id uuid PRIMARY KEY,
    sentence_id uuid NOT NULL REFERENCES lexicon_next.sentences(id) ON DELETE CASCADE,
    source_variant_id uuid NOT NULL,
    source_dialect text NOT NULL CHECK (source_dialect IN ('common','uk','us')),
    source_segments jsonb NOT NULL CHECK (jsonb_typeof(source_segments) = 'array'),
    source_start integer NOT NULL CHECK (source_start >= 0),
    source_end integer NOT NULL CHECK (source_end > source_start),
    source_surface text NOT NULL CHECK (source_surface = btrim(source_surface) AND char_length(source_surface) BETWEEN 1 AND 200),
    state text NOT NULL CHECK (state IN ('linked','entry_only','pending')),
    target_kind text,
    target_source text NOT NULL DEFAULT 'v3' CHECK (target_source IN ('v3','lexicon_next')),
    target_word_id uuid,
    target_publication_id uuid,
    target_pos_id uuid,
    target_base_form_id uuid,
    target_sense_id uuid,
    target_form_id uuid,
    target_variant_id uuid,
    target_dialect text CHECK (target_dialect IN ('common','uk','us')),
    pending_headword text,
    pending_gloss text,
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now(),
    UNIQUE (source_variant_id, source_start),
    FOREIGN KEY (source_variant_id, sentence_id)
        REFERENCES lexicon_next.sentence_nodes(id, sentence_id) ON DELETE CASCADE,
    CONSTRAINT lexicon_next_sentence_associations_shape_check CHECK (
        (state = 'linked'
            AND target_kind IS NOT NULL
            AND target_word_id IS NOT NULL
            AND target_pos_id IS NOT NULL
            AND target_sense_id IS NOT NULL
            AND target_form_id IS NOT NULL
            AND target_variant_id IS NOT NULL
            AND pending_headword IS NULL)
        OR (state = 'entry_only'
            AND target_kind IS NOT NULL
            AND target_word_id IS NOT NULL
            AND target_sense_id IS NULL
            AND pending_headword IS NULL)
        OR (state = 'pending'
            AND target_kind IS NOT NULL
            AND pending_headword IS NOT NULL
            AND btrim(pending_headword) <> ''
            AND target_word_id IS NULL
            AND target_sense_id IS NULL
            AND target_form_id IS NULL
            AND target_variant_id IS NULL)
    )
);
CREATE INDEX lexicon_next_sentence_associations_sentence_idx
    ON lexicon_next.sentence_associations(sentence_id, source_variant_id, source_start);
CREATE INDEX lexicon_next_sentence_associations_target_idx
    ON lexicon_next.sentence_associations(target_word_id, target_sense_id, target_form_id);
