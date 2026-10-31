CREATE TABLE learning_question_audio (
    question_id uuid NOT NULL REFERENCES learning_questions(id) ON DELETE CASCADE,
    accent text NOT NULL CHECK (accent IN ('uk','us')),
    publication_id uuid NOT NULL REFERENCES lexicon.entry_publications(id),
    target_id uuid NOT NULL,
    asset_id uuid NOT NULL REFERENCES lexicon.audio_assets(id),
    policy_version text NOT NULL CHECK (policy_version = 'harmonic_v1'),
    occurrence bigint NOT NULL CHECK (occurrence > 0),
    effective_rank integer NOT NULL CHECK (effective_rank > 0),
    candidate_count integer NOT NULL CHECK (candidate_count >= effective_rank),
    created_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (question_id, accent)
);
CREATE INDEX learning_question_audio_asset ON learning_question_audio(asset_id);
