# 内置英语参考词典导入

内置词典位于 PostgreSQL 的 `dictionary` schema，只供运行时查询，不与后续可编辑的
`words` 业务表混用。迁移只负责建表；数据由 `import_dictionary` 一次性命令导入。

该轻量数据不含释义正文或例句。自动生成使用的完整 Kaikki JSONL 由
[`dictionary-content-import.md`](dictionary-content-import.md) 说明的独立命令导入，
不得把 `sense_count` 当作正文来源。

## 永久对象

- `dictionary.datasets`：数据版本、校验和、激活状态和行数。
- `dictionary.terms`：现代正式词头。
- `dictionary.region_surfaces`：有地区证据的词头、拼写和别名汇总。
- `dictionary.region_evidence`：原始地区标签和完整来源标签。
- `dictionary.active_terms` / `dictionary.active_region_surfaces`：当前激活版本的只读视图。

导入会在一个事务中建立新版本，核对数量后先退役旧版本、再激活新版本。任一步失败会
整体回滚，不影响当前激活版本。

## 当前数据

```text
数据版本：kaikki-en-2026-07-06-rules-v1
正式词头：560,635
地区字符串：34,584
原始证据：43,398
```

输入文件由 `/Users/darwish/Dev/wiktionary/build_dictionary_subset.py` 生成：

```text
/Users/darwish/Dev/wiktionary/dictionary-terms.jsonl.gz
/Users/darwish/Dev/wiktionary/dictionary-region-evidence.jsonl.gz
```

## 导入命令

确保 `.env` 配置了 `DATABASE_URL`，然后执行：

```bash
cargo run --release --bin import_dictionary -- \
  --version kaikki-en-2026-07-06-rules-v1 \
  --source-version enwiktionary-2026-07-06 \
  --rules-version region-family-v1-rare-uncommon-filter \
  --terms /Users/darwish/Dev/wiktionary/dictionary-terms.jsonl.gz \
  --regions /Users/darwish/Dev/wiktionary/dictionary-region-evidence.jsonl.gz \
  --expected-terms 560635 \
  --expected-regions 34584 \
  --expected-evidence 43398
```

相同 `--version` 不能重复导入。更新数据时使用新的版本名；旧版本会保留为 `retired`，
需要清理时再显式删除对应的 `dictionary.datasets` 行，其余三张表会通过外键级联删除。

## 手工验证

```sql
SELECT version, status, term_count, regional_surface_count, evidence_count
FROM dictionary.datasets
ORDER BY id DESC;

SELECT term, kind, pos, status, region_family, source_regions
FROM dictionary.active_terms
WHERE normalized_term = 'centre';

SELECT evidence_type, original_region_tags, raw_tags, pos, targets
FROM dictionary.region_evidence AS evidence
JOIN dictionary.datasets AS dataset ON dataset.id = evidence.dataset_id
WHERE dataset.status = 'active'
  AND evidence.normalized_term = 'centre';
```
