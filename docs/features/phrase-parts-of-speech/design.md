# 设计：基本词性加 kind 维度

档位：**重档**（迁移 + 契约变更 + 需前端配套）。需求见 `requirements.md`。

## 1. 迁移 `20260912150000_parts_of_speech_kind`

### up

```sql
-- 1. kind 列：存量全部 word；默认值保留，裸 SQL（种子、测试）不给就是单词词性。
ALTER TABLE catalog.parts_of_speech
    ADD COLUMN kind TEXT NOT NULL DEFAULT 'word'
        CONSTRAINT catalog_parts_of_speech_kind_check CHECK (kind IN ('word', 'phrase'));
ALTER TABLE catalog.parts_of_speech
    ADD CONSTRAINT catalog_parts_of_speech_phrase_code_check
        CHECK (kind <> 'phrase' OR code LIKE 'phrase\_%'),
    ADD CONSTRAINT catalog_parts_of_speech_id_kind_key UNIQUE (id, kind);

-- 2. 五个展示字段唯一索引收敛到 kind 内；索引名是错误映射契约，原样保留。code 索引不动。
DROP INDEX catalog.catalog_parts_of_speech_name_zh_unique_idx;
CREATE UNIQUE INDEX catalog_parts_of_speech_name_zh_unique_idx
    ON catalog.parts_of_speech (kind, name_zh);
-- name_en (kind, lower(name_en)) / abbreviation (kind, lower(abbreviation))
-- short_name_zh (kind, short_name_zh) / full_name_en (kind, lower(full_name_en)) 同款

-- 3. 词条侧：entries 给出 (id, kind) 二元组；entry_pos 记下词条 kind 并用两条复合外键锁死。
ALTER TABLE lexicon.entries ADD CONSTRAINT lexicon_entries_id_kind_key UNIQUE (id, kind);
ALTER TABLE lexicon.entry_pos ADD COLUMN entry_kind TEXT;
UPDATE lexicon.entry_pos pos SET entry_kind = e.kind FROM lexicon.entries e WHERE e.id = pos.entry_id;
ALTER TABLE lexicon.entry_pos
    ALTER COLUMN entry_kind SET NOT NULL,
    ADD CONSTRAINT lexicon_entry_pos_entry_kind_check CHECK (entry_kind IN ('word', 'phrase')),
    ADD CONSTRAINT lexicon_entry_pos_entry_kind_fkey
        FOREIGN KEY (entry_id, entry_kind) REFERENCES lexicon.entries(id, kind)
        ON DELETE CASCADE DEFERRABLE INITIALLY DEFERRED,   -- 与 entry_schema_fkey 同款
    DROP CONSTRAINT lexicon_entry_pos_catalog_pos_fkey,
    ADD CONSTRAINT lexicon_entry_pos_catalog_pos_fkey
        FOREIGN KEY (part_of_speech_id, entry_kind) REFERENCES catalog.parts_of_speech(id, kind)
        ON DELETE RESTRICT NOT VALID;
-- DO 块：RAISE NOTICE 存量「短语词条挂单词词性」的条数，只记日志不拦。

-- 4. 词形变化只认 word 词性：恒为 'word' 的普通列 + 复合外键，不写触发器。
ALTER TABLE catalog.form_types
    ADD COLUMN part_of_speech_kind TEXT NOT NULL DEFAULT 'word'
        CONSTRAINT catalog_form_types_part_of_speech_kind_check CHECK (part_of_speech_kind = 'word'),
    DROP CONSTRAINT catalog_form_types_part_of_speech_fkey,
    ADD CONSTRAINT catalog_form_types_part_of_speech_fkey
        FOREIGN KEY (part_of_speech_id, part_of_speech_kind) REFERENCES catalog.parts_of_speech(id, kind)
        ON DELETE RESTRICT;   -- 原形 part_of_speech_id 为 NULL，MATCH SIMPLE 下不参与检查

UPDATE catalog.metadata SET version = version + 1, updated_at = now() WHERE id = TRUE;
```

要点：

- **`NOT VALID`** 只豁免迁移时已存在的行；之后 INSERT 与改动键列的 UPDATE 都会检查，
  被引用侧（删词性）的 RESTRICT 也照常生效。本地库已有 1 条短语词条挂着 2 个单词词性，
  测试服同样有存量；不豁免就得先清数据。存量行由 §5 的应用层校验在下次编辑/发布时拦下。
- `entry_pos.entry_kind` 不给默认值：直接写 SQL 的地方（含 5 处测试 fixture，见 §6）必须显式给，
  避免静默错标。V3 写入用子查询 `(SELECT kind FROM lexicon.entries WHERE id = $2)` 取值，不改签名。
- `form_types` 的 `to_jsonb(f)` 会多出 `part_of_speech_kind` 键；`FormTypeCatalogItem` /
  `FormTypeConfig` 反序列化默认忽略未知键，再序列化时不带，wire 不变。
- 新增外键名 `lexicon_entry_pos_entry_kind_fkey` 不进错误映射：词条 kind 不可变，正常路径撞不到。

### down

先守卫：存在 `kind = 'phrase'` 的词性就 `RAISE EXCEPTION` 报出 code 列表——直接删列会把它们
静默变成单词词性，重建全局唯一索引也可能撞车。守卫通过后：

1. `form_types`：复合外键换回单列（同名），删 `part_of_speech_kind`；
2. `entry_pos`：复合外键换回单列（同名），删 `lexicon_entry_pos_entry_kind_fkey` 与 `entry_kind`；
   `entries` 删 `lexicon_entries_id_kind_key`；
3. `parts_of_speech`：五个索引重建为全局（同名），删两个 CHECK、`id_kind_key` 与 `kind` 列；
4. `catalog.metadata.version` 再加一。

`deployment_migrations::tests` 的 `CURRENT_RELEASE_VERSION` 跟到 `20260912150000`。

## 2. 契约（`src/catalog/model.rs`）

复用 `crate::lexicon::dto::EntryKind`（OpenAPI 已有同名 schema `EntryKind`，`word` / `phrase`）：

| 位置 | 变化 |
| --- | --- |
| `CreatePartRequest` | `#[serde(default)] kind: Option<EntryKind>`，schema `nullable = false`（不进 required）；`None` 视作 `word` |
| `UpdatePartRequest` | 不加（kind 与 code 一样创建后不可改；带上仍是 422 unknown field） |
| `PartListQuery` | `kind: Option<EntryKind>` 查询过滤，缺省不过滤 |
| `PartOfSpeechConfig` / `CatalogPart` | 新增必填 `kind: EntryKind` |
| `NewPart` / `PartRecord` / `CatalogFlatRecord` | 透传 `kind`（数据库读 `String`，用 `EntryKind::as_str` / `EntryKind::parse` 转换，两个方法补在 `dto/core.rs`）；列表过滤作为独立参数传给 `CatalogRepository::list_parts(filter, kind)`，不进 `PartListFilter`（form_types 也在用它） |

`CatalogSubPart`、`SubPartOfSpeechConfig`、form types 全部不动。`docs/openapi.json` 用
`SQLX_OFFLINE=true cargo run --locked --all-features --bin export_openapi` 重新导出。

## 3. catalog 服务与仓储

- `create_part`：`kind = request.kind.unwrap_or(EntryKind::Word)`；`kind == Phrase` 且 `code` 不以
  `phrase_` 开头 → `InvalidPart { field: "code", message: "phrase part of speech code must start with phrase_" }`
  （400 `invalid_part_of_speech`），与数据库 CHECK 同口径。
- `insert_part` INSERT 加 `kind` 列；`PART_LIST_SQL` / `PART_BY_ID_SQL` / catalog 快照 SELECT 都带出 `p.kind`；
  列表 `count(*)` 与分页 SQL 加 `AND ($4::text IS NULL OR p.kind = $4)`。
- `map_part_write_error` 六条映射不动（索引名没变）。`map_part_delete_error` 不动。
- `catalog()` 组装 `CatalogPart` 时填 `kind`（`part_kind` 解析失败走 `Invariant`）；`sub_pos_required` 逻辑不变。
- `kind` 列保留 `DEFAULT 'word'`：种子迁移与 schema 测试里大量裸 INSERT 不带 kind，缺省就是单词词性；服务层始终显式写。

## 4. form_types 守卫（`src/catalog/form_types.rs`）

`require_part(tx, id)` 改为 `SELECT kind FROM catalog.parts_of_speech WHERE id = $1`：
不存在 → 404 `part_of_speech_not_found`（原口径）；`kind != 'word'` → 400 `invalid_form_type`，
field `part_of_speech_id`，message `form types belong to word parts of speech only`。
`database_error` 里 `catalog_form_types_part_of_speech_fkey → 404` 的映射保留作兜底。

## 5. lexicon V3 守卫

- `LexiconRepository::catalog_parts_for_reference` 的 `CatalogPartRecord` 多带 `kind: String`（`catalog_parts` 同款，
  但那条只服务建议路径，不消费 kind）。
- `resolve_v3_catalog_parts(tx, content, entry_kind)`：解析完 code → id 后，`part.kind != entry_kind` 的
  每个 pos 推一条 issue：`V3ValidationIssueCode::PartOfSpeechKindMismatch`（wire `part_of_speech_kind_mismatch`），
  `node_id = pos.pos_id`，`field = "pos"`，step Forms，message
  「该词性属于{单词|短语}目录，请改选{短语|单词}词性」。与既有 `InvalidFormTypeForPartOfSpeech` 同路径返回
  `ValidationFailedV3`。调用点：`v3.rs` create（`input.kind`）与 edit forms（`record.kind`）。
- `catalog_context_for_reference(tx, forms, entry_kind)`（`entry.rs`）：同样比对 kind，不一致返回同一问题码，
  让 complete 校验与发布（`v3.rs:2011/2339`、`v3_publication.rs:123`）也拦住存量短语词条。
- `replace_v3_forms` 的 `INSERT INTO lexicon.entry_pos` 加 `entry_kind` 列，值 `(SELECT kind FROM lexicon.entries WHERE id = $2)`。
- `V3ValidationIssueCode` 新增变体并补 `as_str` / `parse` 两处 match；OpenAPI 枚举随之更新。

## 6. 测试

| 用例 | 覆盖 |
| --- | --- |
| `catalog_schema::parts_of_speech_kind_constraints`（新） | 存量种子 kind=word；同名「名词」在 phrase 下可建、在 word 下撞 `name_zh_unique_idx`；phrase 的 code 不带前缀撞 CHECK；`(id, kind)` 唯一约束存在；`entry_pos` 插入 kind 不匹配的词性撞 `lexicon_entry_pos_catalog_pos_fkey`；`form_types` 挂 phrase 词性撞 `catalog_form_types_part_of_speech_fkey` |
| `catalog_schema::part_unique_values_use_fixed_index_names` | 不变（同 kind 内仍撞同名索引） |
| `catalog_schema::all_fixed_catalog_indexes_exist` | 不变（名字没变） |
| `catalog_handler::phrase_parts_of_speech_live_in_their_own_kind`（新，验收） | 不带 kind 建出 word；带 `kind: phrase` + `phrase_noun` 建出短语「名词」201；phrase 不带前缀 400 field code；`?kind=phrase` 只列短语侧；catalog 每项带 kind；短语词性下建细分词性 201 |
| `catalog_handler::form_types_reject_phrase_parts_of_speech`（新） | POST/PATCH form-types 指向短语词性 400 `invalid_form_type` field `part_of_speech_id`；指向不存在的仍 404 |
| `catalog_handler::catalog_read_allows_active_admin_but_management_requires_super_admin` | `catalog_version` 6 → 7 |
| `lexicon_handler::v3_entry_pos_must_match_entry_kind`（新） | 短语词条 forms 用 `noun` → 422 `part_of_speech_kind_mismatch` 锚在 pos 节点；用 `phrase_noun` → 201 且 `entry_pos.entry_kind = 'phrase'`；单词词条用 `phrase_noun` 同样 422 |
| `deployment_migrations::tests::*` | `CURRENT_RELEASE_VERSION` 跟进；新增 `deployment_undo_refuses_while_phrase_parts_exist`：有短语词性时回退报可读原因、账本不动 |
| 直接写 `entry_pos` 的 fixture | `catalog_handler.rs:135`、`lexicon_schema.rs:73/164`、`lexicon_v3_storage_schema.rs:357/1042` 补 `entry_kind`（子查询取词条 kind） |
| `lexicon_handler` 既有 20 条短语用例 | 它们原本给短语词条挂 `noun`，正好被新校验拦下：改为 `seed_phrase_noun` + `phrase_forms_fixture` / `phrase_meanings_fixture`（sub_pos 留空，短语名词下没配细分词性） |
| `catalog_schema::catalog_schema_and_metadata_seed_are_present` / `catalog_handler` 两处 `catalog_version` | 迁移自增一次，基线 6→7、14→15 |

## 7. 与 09-11 评估的差异、被否决方案

- **`code` 不按 kind 收敛，改为全局唯一 + `phrase_` 前缀。** 09-11 记的是「六个索引都收敛」。
  实际 V3 词条按 **code** 而不是 id 引用词性（`pos.pos = "noun"`，`catalog_parts_for_reference(codes)`），
  发布快照、列表 pos 过滤、细分词性父级映射也都以 code 为键；code 按 kind 重名会把 kind 上下文
  塞进每一处按 code 的查找。管理员不填不看 code，前端派生时加前缀即可（design §8）。
- **存量行用 `NOT VALID` 豁免而不是让迁移失败。** 否决「先清库」：测试服上短语词条是杨老师录的，
  且本地每个开发库都要手动清；豁免后由校验在编辑/发布时拦下更可控。
- **不建平行表**（09-11 已否决）：端点、类型、前端组件全翻倍，数据量不值。
- **不用触发器**填 `entry_kind` / 防 form_types 挂错：普通列 + 复合外键就够，且能被 `assert_db_error` 按名断言。

## 8. 前端配套（tsz 仓，另起会话）

1. `packages/types/src/part-of-speech.ts`：`PartOfSpeechConfig` / `PartOfSpeechCatalogItem` 加 `kind: AdminWordKind`；
   `CreatePartOfSpeechInput` 加 `kind`；`PartOfSpeechConfigListQuery` 加 `kind?`。
2. `packages/types/src/admin-word-v3.ts` `V3_VALIDATION_ISSUE_CODES` 加 `part_of_speech_kind_mismatch`；
   `pnpm --filter @tsz/api-client sync:openapi` 刷快照与 runtime schema。
3. `PartOfSpeechSettings.tsx`：Tabs 上方加 `Segmented`「单词 / 短语」；列表查询带 `kind`；短语态隐藏「词形变化」tab；
   `PartOfSpeechFormModal` 建词性时带 `kind`，短语态 `derivePartOfSpeechCode` 结果加 `phrase_` 前缀（前缀 7 字符，
   截断上限相应减到 25）。
4. `FormTypeSettings.tsx` 三处「所属基本词性」候选只列 `kind === "word"`。
5. `catalog.ts` `createPartOfSpeechLookup` 派生按 kind 分组；`V3AddBasicPosSelect` 按 `entryKind` 过滤
   （`V3FormsAndPronunciationStep` 需透传 entryKind）；`SmartDictionary` 词性筛选按当前 kind 过滤。
6. mock / e2e fixture 补 `kind`。

## 9. 风险与回退

- 部署顺序错（后端先）：新字段被前端 runtime validator 拒收 → 词性配置页与 V3 向导报错。按 requirements
  「前端先、后端紧随」即可；两者间隔内建词性 422。
- 回退：down 在有短语词性时拒绝执行（报 code 列表）。要回退就先删短语词性（它们只在测试服存在）。
- 存量短语词条：不丢数据，但下次编辑词形/发布前必须改选短语词性；这要求先有短语词性可选。

## 10. 验收

```bash
cargo test --locked --all-features --test catalog_handler phrase_parts_of_speech_live_in_their_own_kind
cargo test --locked --all-features --test catalog_schema parts_of_speech_kind_constraints
cargo test --locked --all-features --test catalog_handler form_types_reject_phrase_parts_of_speech
cargo test --locked --all-features --test lexicon_handler v3_entry_pos_must_match_entry_kind
cargo test --locked --all-features --lib deployment_migrations
```

集成测试用 `#[sqlx::test]` 各建临时库，不依赖共享数据；`lexicon_handler` 需要 `.env` 里的 Redis。
