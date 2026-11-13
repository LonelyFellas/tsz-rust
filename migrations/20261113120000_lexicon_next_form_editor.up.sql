CREATE TABLE lexicon_next.form_editor_nodes (
    id uuid PRIMARY KEY,
    entry_id uuid NOT NULL REFERENCES lexicon_next.entries(id) ON DELETE CASCADE,
    role text NOT NULL,
    parent_id uuid
);
CREATE INDEX ON lexicon_next.form_editor_nodes(entry_id);
CREATE TABLE lexicon_next.audio_asset_references (
    entry_id uuid NOT NULL REFERENCES lexicon_next.entries(id) ON DELETE CASCADE,
    asset_id uuid NOT NULL REFERENCES lexicon.audio_assets(id),
    PRIMARY KEY(entry_id, asset_id)
);
CREATE INDEX ON lexicon_next.audio_asset_references(asset_id);
