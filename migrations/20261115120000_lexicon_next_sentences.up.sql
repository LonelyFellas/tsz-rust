CREATE TABLE lexicon_next.sentences (
    id uuid PRIMARY KEY,
    target_language text NOT NULL CHECK (target_language ~ '^[a-z]{2,3}$'),
    owner_id uuid NOT NULL REFERENCES admins(id),
    revision bigint NOT NULL CHECK (revision > 0),
    document jsonb NOT NULL CHECK (document->>'schema_version' = '1'),
    create_digest bytea NOT NULL,
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX ON lexicon_next.sentences(target_language,updated_at DESC,id DESC);
CREATE TABLE lexicon_next.sentence_nodes (
    id uuid PRIMARY KEY,
    sentence_id uuid NOT NULL REFERENCES lexicon_next.sentences(id) ON DELETE CASCADE,
    kind text NOT NULL CHECK (kind IN ('variant','translation')),
    locale text NOT NULL,
    parent_id uuid,
    UNIQUE(id,sentence_id),
    FOREIGN KEY(parent_id,sentence_id) REFERENCES lexicon_next.sentence_nodes(id,sentence_id)
);
CREATE TABLE lexicon_next.sentence_references (
    entry_id uuid NOT NULL REFERENCES lexicon_next.entries(id) ON DELETE CASCADE,
    sense_id uuid NOT NULL,
    sentence_id uuid NOT NULL REFERENCES lexicon_next.sentences(id),
    position integer NOT NULL CHECK(position >= 0),
    PRIMARY KEY(entry_id,sense_id,sentence_id),
    FOREIGN KEY(sense_id,entry_id) REFERENCES lexicon_next.nodes(id,entry_id) ON DELETE CASCADE
);
CREATE INDEX ON lexicon_next.sentence_references(sentence_id);
CREATE TABLE lexicon_next.sentence_audio_asset_references (
    sentence_id uuid NOT NULL REFERENCES lexicon_next.sentences(id) ON DELETE CASCADE,
    asset_id uuid NOT NULL REFERENCES lexicon.audio_assets(id),
    PRIMARY KEY(sentence_id,asset_id)
);
CREATE INDEX ON lexicon_next.sentence_audio_asset_references(asset_id);
