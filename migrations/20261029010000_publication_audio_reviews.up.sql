CREATE TABLE speech.publication_audio_reviews (
    id UUID PRIMARY KEY,
    entry_id UUID REFERENCES lexicon.entries(id) ON DELETE CASCADE,
    sentence_id UUID REFERENCES lexicon.shared_sentences(id) ON DELETE CASCADE,
    revision BIGINT NOT NULL CHECK (revision > 0),
    actor_id UUID NOT NULL REFERENCES admins(id),
    security_version BIGINT NOT NULL,
    policy_version INTEGER NOT NULL CHECK (policy_version > 0),
    manifest_hash BYTEA NOT NULL CHECK (octet_length(manifest_hash) = 32),
    manifest JSONB NOT NULL CHECK (jsonb_typeof(manifest) = 'array'),
    confirmed_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    CHECK (num_nonnulls(entry_id, sentence_id) = 1)
);
CREATE UNIQUE INDEX publication_audio_reviews_entry_key
    ON speech.publication_audio_reviews(entry_id, revision, actor_id, security_version, policy_version, manifest_hash)
    WHERE entry_id IS NOT NULL;
CREATE UNIQUE INDEX publication_audio_reviews_sentence_key
    ON speech.publication_audio_reviews(sentence_id, revision, actor_id, security_version, policy_version, manifest_hash)
    WHERE sentence_id IS NOT NULL;
ALTER TABLE lexicon.entry_publications ADD COLUMN audio_review_id UUID
    REFERENCES speech.publication_audio_reviews(id);
ALTER TABLE lexicon.shared_sentence_publications ADD COLUMN audio_review_id UUID
    REFERENCES speech.publication_audio_reviews(id);
