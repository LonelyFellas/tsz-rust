# 设计：细分词性编码可填可改

## 1. 迁移 `20260910200000_sub_pos_code_text`

up 只有一句 `DROP INDEX catalog.catalog_sub_parts_name_en_unique_idx;`。索引名是错误映射契约，
删掉后 `name_en` 不再产生 23505。不动数据、不碰 `catalog.metadata.version`。

down 先用 `DO $$ ... RAISE EXCEPTION ... $$` 守卫扫一遍同父级下重复的 `lower(name_en)`，
有重复就报出具体值再退出，然后才 `CREATE UNIQUE INDEX` 重建。`deployment_migrations::undo`
是单事务原子的，不加守卫的话一条裸 23505 会把这条之后的所有回退一起打回，
且运维看不出是哪条数据挡的。

同步删掉 `map_sub_part_write_error` 里 `catalog_sub_parts_name_en_unique_idx → name_en` 的映射，
以及 `docs/part-of-speech-config-design.md` §11 映射表里的那一行。

## 2. DTO

`UpdateSubPartRequest` 加 `#[serde(default)] pub code: Option<String>`，
schema 标 `pattern = "^[A-Z][A-Z0-9_-]{0,31}$"`、`nullable = false`，因而不进 `required`。
`SubPartChanges` 同步加 `code: Option<String>`，`None` 表示保持原值。

已知偏差：`deny_unknown_fields` 拦不住显式 `"code": null`——serde 把它解析成 `None`
（即「不改」），而 schema 声明的是非空字符串。管理端不会这么发，暂不加 `deserialize_with` 收口。

## 3. 服务层守卫（`update_sub_part`）

`changes.code` 为 `Some` 时，先 `sub_part_revision(..., lock = true)` 取 `FOR UPDATE` 的
`(revision, code)`：

1. 取不到 → `SubPartNotFound`（父级 id 对不上也走这里，与既有 404 口径一致）；
2. `revision != base_revision` → `RevisionConflict`，**排在引用守卫前面**。否则并发冲突会被
   报成「已被引用」，管理员不去刷新，手上那份过期表单还会覆盖别人的修改；
3. 编码确实变了才查 `sub_part_usage_count`，`> 0` → `SubPartInUse { usage_count }`。

编码没变时不查引用，省一次聚合查询——前端每次提交都会带上 `code`，这条路径是常态。

## 4. 仓储层

`update_sub_part` 的 SET 加 `code = COALESCE($4, code)`，绑 `changes.code.as_deref()`。
`$4` 为 NULL 时保持原值，这是「省略 code」的落点；后续占位符整体后移一位。

## 5. 测试

| 用例 | 覆盖 |
| --- | --- |
| `catalog_schema::sub_part_name_en_may_repeat_under_the_same_parent` | 同父级两条 `N-UNCOUNT` 能插入 |
| `catalog_schema::sub_part_unique_values_use_fixed_index_names` | 去掉 name_en 那条 case |
| `catalog_schema::all_fixed_catalog_indexes_exist` | 固定索引从 9 个减到 8 个 |
| `catalog_handler::sub_part_code_carries_the_code_text_and_freezes_once_referenced` | 建两条同正式英文；错误父级 404；未引用改编码成功；省略 code 保持原编码；编码撞车 409 `code`；被引用后 409 in_use 带 usage_count；过期 revision 先报 revision_conflict；编码不变照常放行 |
| `deployment_migrations::tests::deployment_undo_reports_duplicate_sub_pos_name_en` | 有重复正式英文时回退报可读原因且账本不动 |

`deployment_migrations::tests::CURRENT_RELEASE_VERSION` 跟到 `20260910200000`，
否则 `--lib` 里的四个回退用例齐红。
