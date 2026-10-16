CREATE TABLE wordlist_library_nodes (
    id uuid PRIMARY KEY,
    user_id uuid NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    space text NOT NULL CHECK (space IN ('owned','favorites')),
    parent_id uuid,
    wordlist_id uuid REFERENCES wordlists(id) ON DELETE CASCADE,
    name text NOT NULL DEFAULT '' CHECK (char_length(name)<=100),
    trashed_at timestamptz,
    trash_root_id uuid,
    revision bigint NOT NULL DEFAULT 1 CHECK (revision>0),
    UNIQUE(id,user_id,space),
    UNIQUE(user_id,space,wordlist_id),
    FOREIGN KEY(parent_id,user_id,space) REFERENCES wordlist_library_nodes(id,user_id,space) DEFERRABLE INITIALLY DEFERRED,
    CHECK ((trashed_at IS NULL) = (trash_root_id IS NULL)),
    CHECK (wordlist_id IS NOT NULL OR char_length(btrim(name)) BETWEEN 1 AND 100),
    CHECK (parent_id IS DISTINCT FROM id)
);
CREATE UNIQUE INDEX wordlist_library_folder_name ON wordlist_library_nodes(user_id,space,parent_id,name) NULLS NOT DISTINCT WHERE wordlist_id IS NULL AND trashed_at IS NULL;
CREATE INDEX wordlist_library_parent ON wordlist_library_nodes(user_id,space,parent_id);
INSERT INTO wordlist_library_nodes(id,user_id,space,wordlist_id)
SELECT gen_random_uuid(),owner_user_id,'owned',id FROM wordlists WHERE owner_user_id IS NOT NULL AND deleted_at IS NULL;
INSERT INTO wordlist_library_nodes(id,user_id,space,wordlist_id)
SELECT gen_random_uuid(),user_id,'favorites',wordlist_id FROM wordlist_interactions WHERE kind='favorite';
