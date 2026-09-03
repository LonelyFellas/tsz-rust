DROP INDEX IF EXISTS lexicon.lexicon_surface_sources_published_surface_trgm_idx;
-- 不撤扩展：pg_trgm 可能被别的对象使用，down 只撤回本迁移自己创建的索引。
