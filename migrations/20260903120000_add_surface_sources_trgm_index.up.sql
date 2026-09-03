-- 短语成分目标的关键字检索（component-targets/search）对已发布词面做 ILIKE '%q%'，
-- 前置通配符用不上 surface_sources 现有的四个 btree 索引，会退化成顺序扫描。
-- pg_trgm 自 PG13 起是 trusted extension，数据库 CREATE 权限即可安装（tshb-test RDS 的 app
-- 用户已核对具备）。GIN 三元组索引只对 >= 3 字符的关键字生效：1-2 字符（a / me / to）提取
-- 不出完整三元组，仍走顺序扫描，靠 LIMIT 与「等于/前缀优先」的排序兜底。
CREATE EXTENSION IF NOT EXISTS pg_trgm;

CREATE INDEX IF NOT EXISTS lexicon_surface_sources_published_surface_trgm_idx
    ON lexicon.surface_sources USING gin (surface gin_trgm_ops)
    WHERE content_scope = 'current_publication' AND is_deleted = FALSE;
