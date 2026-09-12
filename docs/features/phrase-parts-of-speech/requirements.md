# 短语的词性配置：基本词性按 word / phrase 分维度

状态：**后端已实现（2026-09-12），未部署**；前端配套未动工（design §8）。评估基线 tsz-rust `5b0adc4`、tsz `6e938f4`。
禅道 TASK#36（产品「天生会背」）。

## 背景

「系统设置 → 词性配置」目前只有一套目录，单词和短语共用。杨老师的要求：短语也要配置
「基本词性」和「细分词性」，但**没有「词形变化」**；入口二选一——导航栏拆两项，或配置页内
「单词 / 短语」切换。2026-09-12 拍板：

1. 入口走**页内切换**（导航栏不增项）。
2. 短语词性与单词词性**共用 `catalog.parts_of_speech`**，只加一列 `kind`（`word` / `phrase`），
   不建平行表；细分词性从父级继承 kind，词形变化表不动。
3. 先做后端（迁移 + 契约），前端随后跟进。

依赖 `sub_pos_required` 已改为按实际配置派生（tsz-rust #147，已上测试服），短语词性下配了
细分词性就会必填，不再受五个固定编码限制。

## 范围

本仓只做后端：迁移、契约、catalog / form_types / lexicon 三处守卫与测试。前端改动清单见
`design.md` §8，由 tsz 仓另起会话实现。

## 需求

1. `catalog.parts_of_speech` 新增 `kind TEXT NOT NULL`，取值 `word` / `phrase`；存量行全部为 `word`。
   创建后不可修改（与 `code` 同口径）。
2. 展示字段（`name_zh` / `name_en` / `abbreviation` / `short_name_zh` / `full_name_en`）的唯一性
   从全局收敛到**同一 kind 内**：短语侧可以再建一个「名词」。`code` **保持全局唯一**，且 `phrase_`
   前缀与短语 kind **双向绑定**：短语词性的 code 必须带它，单词词性不许占用它（原因见 design §7）。
3. 单词词条不能挂短语词性，短语词条不能挂单词词性——数据库复合外键兜底，V3 create / edit /
   complete 校验给出可读的 `field_issues`（新问题码 `part_of_speech_kind_mismatch`）。
4. 词形变化只能挂在 `word` 词性下：POST/PATCH form-types 指向短语词性返回 400
   `invalid_form_type`（field `part_of_speech_id`），数据库复合外键兜底。
5. 管理接口：
   - `POST /admin/settings/parts-of-speech` 请求新增可选 `kind`，缺省 `word`（旧前端不受影响）；
   - `GET /admin/settings/parts-of-speech` 新增可选查询 `kind` 过滤；
   - `PartOfSpeechConfig` 与 `CatalogPart` 响应新增 `kind`；细分词性与词形变化响应不变。
6. 存量短语词条（已挂单词词性的）**不清、不改数据**：kind 配对外键用 `NOT VALID` 豁免存量行，
   而承担引用保护的单列外键保持不动，删词性照样被拦；这些词条下次编辑词形、完成或发布时被
   第 3 条校验拦下，管理员改选短语词性即可。
7. 不给短语基本词性预置种子（杨老师未定清单，需要时另起迁移加）。

## 约束

- 六个唯一索引名与 `lexicon_entry_pos_catalog_pos_fkey` / `catalog_form_types_part_of_speech_fkey`
  是数据库错误映射契约，重建时**名字原样保留**；新增的 `lexicon_entry_pos_catalog_kind_fkey` 也要进
  `map_part_delete_error` 的 in-use 映射，否则并发删除会退化成 500。
- 细分词性 `code` 仍全局唯一（`catalog_sub_parts_code_unique_idx` 不动）：V3 词义按
  细分词性 code 解析父级（`sub_part_parents: code → part_code`），改成 kind 内唯一会让映射二义。
  代价是短语侧细分词性的编码不能与单词侧重名（如用 `PHR-N-COUNT`），管理员填编码时会收到 409。
- catalog 目录版本随迁移自增一次（响应形状变了，前端缓存要失效）。

## 发布顺序

**同批发布，前端先、后端紧随。**

- 响应新增 `kind` 字段 + V3 新问题码 → 前端 V3 runtime validator 拒收未声明值 → 前端先。
- 请求新增 `kind` 字段 → 后端 `deny_unknown_fields` 会把新前端的建词性请求打成 422 → 后端要
  紧跟着上，中间只有读接口可用、建词性短暂不可用。

部署前不需要清理测试服数据；迁移会在日志里 NOTICE 存量短语词条挂单词词性的条数。

## 验收

```bash
cargo test --locked --all-features --test catalog_handler phrase_parts_of_speech_live_in_their_own_kind
```

配套用例：

- `cargo test --locked --all-features --test catalog_schema parts_of_speech_kind_constraints`
- `cargo test --locked --all-features --test catalog_handler form_types_reject_phrase_parts_of_speech`
- `cargo test --locked --all-features --test lexicon_handler v3_entry_pos_must_match_entry_kind`
- `cargo test --locked --all-features --lib deployment_migrations`
