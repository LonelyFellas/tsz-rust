-- 词形变化的英文缩写不再要求唯一：同一基本词性下「现在分词」「动名词形式」都可以缩写成 V-ing。
-- 中文名、简洁显示、英文名与英文全称仍保持同一基本词性内唯一。
DROP INDEX catalog.catalog_form_types_abbreviation_unique_idx;
