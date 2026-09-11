-- 细分词性的「正式英文」不再承担代码文本：Collins 式的 N-UNCOUNT 会在「不可数物质名词」
-- 「不可数抽象名词」这类更细的划分上正当重复，代码文本改由管理员自己填写的「编码」承载。
-- 索引名是数据库错误映射契约；这里删掉后 name_en 不再产生 409。
DROP INDEX catalog.catalog_sub_parts_name_en_unique_idx;
