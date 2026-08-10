-- Add up migration script here
CREATE TABLE catalog.sub_parts_of_speech (
    id UUID PRIMARY KEY,
    part_of_speech_id UUID NOT NULL,
    code TEXT NOT NULL,
    name_zh TEXT NOT NULL,
    name_en TEXT NOT NULL,
    sort_order INTEGER NOT NULL DEFAULT 0,
    revision BIGINT NOT NULL DEFAULT 1,

    created_by_admin_id UUID,
    updated_by_admin_id UUID,

    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),

    CONSTRAINT catalog_sub_parts_code_check
        CHECK (code ~ '^[A-Z][A-Z0-9_-]{0,31}$'),

    CONSTRAINT catalog_sub_parts_name_zh_check
        CHECK (
            name_zh = btrim(name_zh)
            AND char_length(name_zh) BETWEEN 1 AND 64
        ),

    CONSTRAINT catalog_sub_parts_name_en_check
        CHECK (
            name_en = btrim(name_en)
            AND char_length(name_en) BETWEEN 1 AND 64
        ),

    CONSTRAINT catalog_sub_parts_revision_check
        CHECK (revision > 0),

    CONSTRAINT catalog_sub_parts_parent_fkey
        FOREIGN KEY (part_of_speech_id)
        REFERENCES catalog.parts_of_speech(id)
        ON DELETE CASCADE,

    CONSTRAINT catalog_sub_parts_created_by_admin_id_fkey
        FOREIGN KEY (created_by_admin_id)
        REFERENCES admins(id)
        ON DELETE RESTRICT,

    CONSTRAINT catalog_sub_parts_updated_by_admin_id_fkey
        FOREIGN KEY (updated_by_admin_id)
        REFERENCES admins(id)
        ON DELETE RESTRICT
);

-- code 全局唯一。
CREATE UNIQUE INDEX catalog_sub_parts_code_unique_idx
    ON catalog.sub_parts_of_speech (code);

-- 中文名只在同一基本词性下唯一。
CREATE UNIQUE INDEX catalog_sub_parts_name_zh_unique_idx
    ON catalog.sub_parts_of_speech (part_of_speech_id, name_zh);

-- 英文名只在同一基本词性下忽略大小写唯一。
CREATE UNIQUE INDEX catalog_sub_parts_name_en_unique_idx
    ON catalog.sub_parts_of_speech (
        part_of_speech_id,
        lower(name_en)
    );

CREATE INDEX catalog_sub_parts_order_idx
    ON catalog.sub_parts_of_speech (
        part_of_speech_id,
        sort_order,
        created_at,
        id
    );

-- 固定 UUID v7 种子。
-- part_of_speech_id 对应上一条 migration 中的基本词性固定 ID。
INSERT INTO catalog.sub_parts_of_speech (
    id,
    part_of_speech_id,
    code,
    name_zh,
    name_en,
    sort_order
)
VALUES
    (
        '019feab6-ca12-7cf6-bfe9-500e066ee6ae',
        '019fea10-20ec-7f41-a252-70261233fa51',
        'V-T',
        '及物动词',
        'Transitive verb',
        10
    ),
    (
        '019feab6-ca13-7d94-8779-bc9355744065',
        '019fea10-20ec-7f41-a252-70261233fa51',
        'V-I',
        '不及物动词',
        'Intransitive verb',
        20
    ),
    (
        '019feab6-ca13-7455-b5b1-67f348988d16',
        '019fea10-20ec-7f41-a252-70261233fa51',
        'V-LINK',
        '系动词',
        'Linking verb',
        30
    ),
    (
        '019feab6-ca13-76a4-8716-d25dbba54d8c',
        '019fea10-20ec-7f41-a252-70261233fa51',
        'AUX',
        '助动词',
        'Auxiliary verb',
        40
    ),
    (
        '019feab6-ca13-7396-8895-8de4acfb83c6',
        '019fea10-20ec-7f41-a252-70261233fa51',
        'MODAL',
        '情态动词',
        'Modal verb',
        50
    ),
    (
        '019feab6-ca13-7738-a213-5cb1f9c74383',
        '019fea10-20ec-79f4-98c5-6c45736a73bb',
        'ADJ',
        '形容词',
        'Adjective',
        60
    ),
    (
        '019feab6-ca13-7af3-9578-0157650ea3d0',
        '019fea10-20ec-72ed-b6ae-d243bd77a333',
        'ADV',
        '副词',
        'Adverb',
        70
    ),
    (
        '019feab6-ca13-75ef-ab3f-729125ebcede',
        '019fea10-20ec-7154-bb63-0b37991c0c68',
        'N-COUNT',
        '可数名词',
        'Countable noun',
        80
    ),
    (
        '019feab6-ca13-7a20-aca2-d386e0ed3961',
        '019fea10-20ec-7154-bb63-0b37991c0c68',
        'N-UNCOUNT',
        '不可数名词',
        'Uncountable noun',
        90
    ),
    (
        '019feab6-ca13-72ec-86f3-619d1577b2da',
        '019fea10-20ec-7154-bb63-0b37991c0c68',
        'N-PROPER',
        '专有名词',
        'Proper noun',
        100
    ),
    (
        '019feab6-ca13-7a7b-8cc5-4dafd4b1b3f8',
        '019fea10-20ec-7154-bb63-0b37991c0c68',
        'N-PLURAL',
        '复数名词',
        'Plural noun',
        110
    ),
    (
        '019feab6-ca13-7ac1-b09e-2edf6d38b086',
        '019fea10-20ec-7154-bb63-0b37991c0c68',
        'N-SING',
        '单数名词',
        'Singular noun',
        120
    ),
    (
        '019feab6-ca13-7bf9-9317-4f7b50afcddd',
        '019fea10-20ec-7c51-9f71-86327e7c934e',
        'PRON',
        '代词',
        'Pronoun',
        130
    ),
    (
        '019feab6-ca13-7c90-9b8b-1d38d93ae0f2',
        '019fea10-20ec-7279-a4dc-634c5c4ffca1',
        'PREP',
        '介词',
        'Preposition',
        140
    ),
    (
        '019feab6-ca13-7db0-8602-267b5bc99b5a',
        '019fea10-20ec-7d64-acea-96a48d51c8ed',
        'CONJ',
        '连词',
        'Conjunction',
        150
    ),
    (
        '019feab6-ca13-7378-a71f-ac2bc3dc0a28',
        '019fea10-20ec-7a6f-b827-0c169308ee45',
        'DET',
        '限定词',
        'Determiner',
        160
    ),
    (
        '019feab6-ca13-79b0-b354-0d47c6d92990',
        '019fea10-20ec-79ae-9bdd-83d5c58ff641',
        'ART',
        '冠词',
        'Article',
        170
    ),
    (
        '019feab6-ca13-7c5f-9778-569d41a6dc5c',
        '019fea10-20ec-76fc-a8c8-20b1e2cd1da9',
        'NUM',
        '数词',
        'Numeral',
        180
    ),
    (
        '019feab6-ca13-7e5b-8091-5e4188cae3f8',
        '019fea10-20ec-7554-b9ac-f4485b6d7463',
        'INT',
        '感叹词',
        'Interjection',
        190
    );
