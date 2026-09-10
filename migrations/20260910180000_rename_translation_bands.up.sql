-- 译文档位改成语义化命名：初/中/高说的是译文风格（逐字直译、语句通顺、深层重构），
-- 与例句的 CEFR 难度等级无关，旧的 a1_a2 一组命名会持续误导。
-- 只换名字不动归属：c1_c2 这一档继续是逐字直译，a1_a2 这一档继续是深层重构。

-- 20260907180000 建的部分唯一索引按档位名把译文槽位排除在唯一约束之外。
-- 改名期间新旧值并存，先撤掉索引和取值约束，改完再按新档位名建回去，
-- 否则改到一半的行会落进唯一约束，同句同档的多条译文会撞车。
ALTER TABLE lexicon.text_variants
    DROP CONSTRAINT lexicon_text_variants_field_role_check;
DROP INDEX lexicon.lexicon_text_variants_slot_key;

UPDATE lexicon.text_variants SET field_role = 'zh_translation_word_for_word'
WHERE field_role = 'zh_translation_c1_c2';
UPDATE lexicon.text_variants SET field_role = 'zh_translation_balanced_fluency'
WHERE field_role = 'zh_translation_b1_b2';
UPDATE lexicon.text_variants SET field_role = 'zh_translation_adapted_creation'
WHERE field_role = 'zh_translation_a1_a2';

CREATE UNIQUE INDEX lexicon_text_variants_slot_key
    ON lexicon.text_variants (owner_node_id, field_role, language, dialect)
    WHERE field_role NOT IN (
        'zh_translation_word_for_word',
        'zh_translation_balanced_fluency',
        'zh_translation_adapted_creation'
    );
ALTER TABLE lexicon.text_variants
    ADD CONSTRAINT lexicon_text_variants_field_role_check CHECK (
        field_role IN (
            'content', 'en_text', 'zh_text',
            'zh_translation_word_for_word',
            'zh_translation_balanced_fluency',
            'zh_translation_adapted_creation'
        )
    );

-- 编辑器投影里的旧档位值不动：它是 GET 和保存的权威读源而不是可重建的缓存，
-- 删掉词条就再也读不出来了。枚举带着旧取值的反序列化别名，读得出来，
-- 下次保存该词条时自然按新命名重写。
