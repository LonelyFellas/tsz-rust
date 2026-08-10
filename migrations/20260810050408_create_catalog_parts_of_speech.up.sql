-- Add up migration script here
CREATE TABLE catalog.parts_of_speech (
    id UUID PRIMARY KEY,
    code TEXT NOT NULL,
    name_zh TEXT NOT NULL,
    name_en TEXT NOT NULL,
    abbreviation TEXT NOT NULL,
    sort_order INTEGER NOT NULL DEFAULT 0,
    revision BIGINT NOT NULL DEFAULT 1,

    created_by_admin_id UUID,
    updated_by_admin_id UUID,

    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),

    CONSTRAINT catalog_parts_of_speech_code_check
         CHECK (code ~ '^[a-z][a-z0-9_]{0,31}$'),

     CONSTRAINT catalog_parts_of_speech_name_zh_check
         CHECK (
             name_zh = btrim(name_zh)
             AND char_length(name_zh) BETWEEN 1 AND 64
         ),

     CONSTRAINT catalog_parts_of_speech_name_en_check
         CHECK (
             name_en = btrim(name_en)
             AND char_length(name_en) BETWEEN 1 AND 64
         ),

     CONSTRAINT catalog_parts_of_speech_abbreviation_check
         CHECK (
             abbreviation = btrim(abbreviation)
             AND char_length(abbreviation) BETWEEN 1 AND 16
         ),

     CONSTRAINT catalog_parts_of_speech_revision_check
         CHECK (revision > 0),

     CONSTRAINT catalog_parts_of_speech_created_by_admin_id_fkey
         FOREIGN KEY (created_by_admin_id)
         REFERENCES admins(id)
         ON DELETE RESTRICT,

     CONSTRAINT catalog_parts_of_speech_updated_by_admin_id_fkey
         FOREIGN KEY (updated_by_admin_id)
         REFERENCES admins(id)
         ON DELETE RESTRICT
 );

 -- 这些索引名是数据库错误映射契约，不能修改。
 CREATE UNIQUE INDEX catalog_parts_of_speech_code_unique_idx
     ON catalog.parts_of_speech (code);

 CREATE UNIQUE INDEX catalog_parts_of_speech_name_zh_unique_idx
     ON catalog.parts_of_speech (name_zh);

 CREATE UNIQUE INDEX catalog_parts_of_speech_name_en_unique_idx
     ON catalog.parts_of_speech (lower(name_en));

 CREATE UNIQUE INDEX catalog_parts_of_speech_abbreviation_unique_idx
     ON catalog.parts_of_speech (lower(abbreviation));

 CREATE INDEX catalog_parts_of_speech_order_idx
     ON catalog.parts_of_speech (sort_order, created_at, id);

     -- 固定 UUID v7 种子；后续创建细分词性时要复用这些父级 ID。
     INSERT INTO catalog.parts_of_speech (
         id,
         code,
         name_zh,
         name_en,
         abbreviation,
         sort_order
     )
     VALUES
         ('019fea10-20ec-7154-bb63-0b37991c0c68', 'noun',         '名词',   'NOUN',         'n.',     10),
         ('019fea10-20ec-7c51-9f71-86327e7c934e', 'pronoun',      '代词',   'PRONOUN',      'pron.',  20),
         ('019fea10-20ec-7f41-a252-70261233fa51', 'verb',         '动词',   'VERB',         'v.',     30),
         ('019fea10-20ec-79f4-98c5-6c45736a73bb', 'adjective',    '形容词', 'ADJECTIVE',    'adj.',   40),
         ('019fea10-20ec-72ed-b6ae-d243bd77a333', 'adverb',       '副词',   'ADVERB',       'adv.',   50),
         ('019fea10-20ec-7279-a4dc-634c5c4ffca1', 'preposition',  '介词',   'PREPOSITION',  'prep.',  60),
         ('019fea10-20ec-79ae-9bdd-83d5c58ff641', 'article',      '冠词',   'ARTICLE',      'art.',   70),
         ('019fea10-20ec-7a6f-b827-0c169308ee45', 'determiner',   '限定词', 'DETERMINER',   'det.',   80),
         ('019fea10-20ec-7d64-acea-96a48d51c8ed', 'conjunction',  '连词',   'CONJUNCTION',  'conj.',  90),
         ('019fea10-20ec-76fc-a8c8-20b1e2cd1da9', 'numeral',      '数词',   'NUMERAL',      'num.',  100),
         ('019fea10-20ec-7554-b9ac-f4485b6d7463', 'interjection', '感叹词', 'INTERJECTION', 'int.',  110);
