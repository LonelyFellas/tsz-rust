# 下线词条内容格式 V2（只保留 V3）— 依赖链评估

状态：已实施。基线 `origin/main` = `930786a`。下方 §1–§5 是动手前的依赖链分析（保留作为决策依据），
§6 是实际落地结果与确认过的取舍。

## 0. 事实核对

| 事实 | 证据 |
| --- | --- |
| 本地 `tsz_rust` 库无 V2 内容 | `lexicon.entries` 13 行全 `content_schema_version = 3`；`entry_publications` 3 行全 3；`v3_entry_state.origin` 13 行全 `native` |
| V2 专用存储表全空 | `form_groups` / `form_slots` / `form_variants` / `pronunciations` / `entry_headwords` / `entry_headword_keys` 均 0 行 |
| 迁移账本表全空 | `v3_migration_batches` / `v3_migration_entries` / `v3_migration_map` 均 0 行 |
| V3 行不写 V2 列 | 13 行的 `headword_mode` / `source_dialect` 全 NULL |
| `compatibility.legacy_headwords` 只对迁移词条下发 | `service/v3.rs:850` 的 `state.origin == "migrated_v2"` 分支；native V3 恒 `None`（`docs/features/smart-lexicon-v3-native-publication/design.md:129` 同口径） |
| 前端仍在发 V2 请求 | `apps/admin` 的 `word-creation/*` 走 `createV2` / `validateV2` / `publishV2` / `suggestDialectVariants` |

结论：**读路径上没有任何 V2 数据要兜底**，唯一的现实约束是前端还在发 V2 写请求。

## 1. 名字陷阱：带 V2/V1 但与内容格式无关

动手前先把这几类排除掉，它们**不能**按名字删：

| 名字 | 真实含义 | 结论 |
| --- | --- | --- |
| `RichTextV1` / `RichTextV2` / `RichText` | 富文本文档格式的版本轴，和 `content_schema_version` 无关；V3 侧另有一套 `RichTextV1V3` / `RichTextV2V3` 同样分 V1/V2 | 保留 |
| `RelatedSearchResponse::{Legacy,V2}` | 关联词搜索的**分页形状**版本（有无 cursor/total），由 query 参数是否出现决定，见 `service/queries.rs:278` | 保留 |
| `SentenceSourceRangeV1` / `SentenceAssociationStateV1` | 例句关联的结构版本 | 保留 |
| `SurfaceMatchPageV2` 及其一族（`SurfaceMatchCandidateV2` / `SurfaceMatchCategoryV2` / `SurfacePolicyNameV2` / `SurfaceContentScopeV2` …） | surface snapshot 的**内部规范形状**；V3 页面是在它上面投影出来的（`lifecycle.rs:1035` 的 `surface_page_v3(snapshot.page, …)`） | 内部结构保留，只有 wire 上的 V2 分支可议 |
| `v3_initial_headword_backfill.rs` | 回填 **native V3** 词条的 `initial_headwords`（`WHERE origin = 'native'`），与 V2 无关 | 保留 |
| `WordHeadwordsV2` | V3 建条时也用它当中间结构算 presentation（`service/v3.rs:483 compatibility_v3_headwords`），并且是 `validate_meanings` 的入参 | 保留 |
| `DraftMeaningsStepContent` / `WordSenseV2` / `WordDefinitionV2` / `WordRelationV2` / `WordSentenceV2` / `SenseGroupV2` / `WordPosMeaningsV2` / `DraftFormsStepContent` / `WordPosFormsV2` / `EnglishTextV2` / `TextVariantV2` / `GrammarStructureV2` | **V3 全链路在复用**：`service.rs:225 v3_meaning_validation_forms` + `:250 v3_meaning_validation_headwords` 把 V3 内容适配成这套结构，再喂给共用的 `validate_meanings`；`publishing.rs:1347 published_sense_snapshot` 的 V3 分支也把 V3 meanings 转成 `DraftMeaningsStepContent` 读 | 保留 |

## 2. 三分清单

### A. 可删（确认无 V2 数据后即可下线，互不依赖）

**A1 — V2→V3 迁移工具链（现在就能删，不必等前端）**

| 目标 | 行数 |
| --- | --- |
| `src/lexicon/v3_migration.rs` | 4023 |
| `src/bin/lexicon_v3_migration.rs` 的 `inventory/dry-run/approve/apply/verify/enable-canary/rollback` 子命令 | 约 90 |
| `src/lexicon/surface_backfill.rs`（`WHERE content_schema_version = 2` 的 surface 回填/对账/切换） | 795 |
| `src/bin/lexicon_surface_migration.rs` | 81 |
| `tests/lexicon_v3_migration.rs` | 2302 |
| `tests/lexicon_handler.rs` 里的 B4 backfill / cutover / parity 用例 | 约 600 |

理由：这些代码的唯一输入是 `content_schema_version = 2` 的行，库里为 0 且不会再产生。`v3_entry_state.origin` 的 `migrated_v2` 取值随之成为死枚举。

**A2 — V2 写路径（须等前端确认不再发 V2 请求）**

- `handler/commands.rs` 里 `detect` / `create` / `preview_forms_impact` / `save_forms` / `save_meanings` / `validate` / `publish` / `activate_publication` 八个函数的 `Some(2) | None` 落地分支；`v3_contract::request_schema_version_or_legacy` 随之退化为 `request_schema_version`（`schema_version` 变必填、只收 3）。
- `service/entry.rs::detect` + `::create` 及其私有助手 `build_suggested_forms` / `base_variant` / `build_initial_meanings` / `empty_english_text` / `align_base_forms` / `detected_headwords`（`build_initial_pos_meanings` 被 `editing.rs` 用，随 A3 一起走）。
- `service/editing.rs::preview_forms_impact` / `save_forms` / `save_meanings`（V2 版本）。
- `service/publishing.rs::validate` / `publish` / `activate_publication`（V2 版本）。
- `validation/structure.rs::validate_forms`（只有 V2 的 `publishing.rs` / `editing.rs` 调；V3 走 `v3_contract::validate_forms`）。同文件的 `proposed_nodes` / `validate_node_identities` / `validate_node_limit` / `validate_persisted_text` / `MAX_ENTRY_NODES` 是共用的，**不能删**。
- 对应 DTO：`CreateAdminWordV2Input` / `DetectWordInputV2` / `DetectWordResponseV2` / `PreviewFormsImpactInputV2` / `SaveFormsStepInput` / `SaveMeaningsStepInput` / `ValidateAdminWordV2Input` / `PublishAdminWordV2Input` / `ActivatePublicationInput` / `FormsImpactResponseV2` / `FormsImpactItemV2` / `DraftValidationResponse`，以及各 `*Any` 联合体的 V2 arm。

**A3 — V2 读路径（跟 A2 同批；单独留没有意义）**

- `service/entry.rs::entry_from_record`（把 V2 列拼成 `AdminWordV2`）。
- `service/queries.rs::get` / `get_draft` / `publication_from_record` 的 `2 =>` 分支 / `list()` 的 `2 =>` 分支。
- `service/v3.rs::get_draft_any` 的 `content_schema_version == 2` 分支。
- `service/publishing.rs::draft_target_headword` 的 `2 =>` 分支、`published_sense_snapshot` 的 `2 =>` 分支、`helpers.rs::v2_publication_snapshot`。
- `service/lifecycle.rs` 的 `contains_v3 == false` 分支与 `ensure_lifecycle_schema_capability`。
- DTO：`AdminWordV2` / `AdminWordV2Envelope` / `AdminWordDraftV2Envelope` / `RetiredStableSlotV2` / `AdminWordListItem`（V2 行）/ `AdminWordPublicationV2` / `WordDetectionSnapshotV2` 及其明细。

**A4 — legacy bridge 开关与兼容字段**

- `config.rs` 的 `smart_lexicon_v3_legacy_bridge_read` 与 `.env.example` 对应行。
- `handler.rs` 的 `apply_legacy_bridge_read_flag` / `apply_lifecycle_batch_legacy_bridge_read_flag` / `apply_draft_legacy_bridge_read_flag` / `apply_publication_legacy_bridge_read_flag` 四个函数及其全部调用点（`commands.rs` 4 处、`query.rs` 3 处、`lifecycle.rs` 4 处）。
- `AdminWordV3Compatibility` / `LegacyHeadwordsCompatibilityV3` / `AdminWordV3.compatibility` 字段、`service/queries.rs::legacy_bridge_from_list`、`v3_projection::presentation_from_legacy_bridge`、`service/v3.rs::legacy_headwords_from_record`。
- 注意：删 `compatibility` 是**移除响应必填之外的可选字段**，按「严格 runtime schema 部署顺序」的口径属于「移除字段 → 后端先、前端后」。前端当前不读它（`apps/admin` 无 `compatibility` 引用），可以后端先走。

### B. 必须保留

除第 1 节的名字陷阱外，另有：

- `validation/meanings.rs`、`validation/helpers.rs` 全部——V3 的保存、发布、组件定位三条路径都在调 `validate_meanings`。
- `service/publishing.rs` 的 `resolve_meaning_references` / `pending_relation_issue` / `published_sense_gloss` / `reference_issue` / `confirm_visibility_command`——V3 发布路径在用。签名里的 `&WordRelationV2` / `&WordSenseV2` / `&AdminWordV2` 要改成 V3 类型或保持结构复用，两种都行，但不能删函数。
- `service/entry.rs` 的 `surface_match_contexts` / `headword_surface_matches_in_transaction` / `surface_contexts_from_records` / `inbound_relation_previews` / `existing_surface_match` / `parse_surface_status` / `canonical_headwords_digest` / `detected_headwords_v3` / `catalog_context` / `catalog_context_for_reference`。
- `service/editing.rs` 的 `form_surface_matches_in_transaction` / `ensure_active` / `ensure_revision` / `completed_steps` / `meanings_storage_issues` / `meaning_storage_is_safe` / `canonical_forms_digest` / `definition_id` / `definition_grammar_id` / `definition_level`。
- `surface_snapshot.rs` / `surface_policy.rs` / `detection_store.rs` 的存储结构（Redis 里的存量 token 按当前形状序列化，改形状会让在飞的确认全失效）。
- `lexicon.entry_editor_projection`、`entry_pos`、`senses`、`definitions`、`relations`、`sentences`、`nodes`、`text_variants`、`grammar_structures`、`sense_groups`——V3 在用，只是共用表带 `content_schema_version` 列。

### C. 需要确认

| # | 问题 | 影响 |
| --- | --- | --- |
| C1 | **动不动数据库 schema？** 收紧 = `entries.content_schema_version` 默认改 3、check 改 `= 3`、删 `headword_mode` / `source_dialect` 列、删 6 张 V2 存储表 + 3 张迁移账本表。要写迁移、改 `deployment_migrations.rs` 的 `CURRENT_RELEASE_VERSION`（现 `20260910200000`），并保证 down 迁移可回退。建议：**本次先不动**，代码删干净后单开一个 PR，理由是 schema 收紧不可逆而代码删除可回滚。 | 中 |
| C2 | **`POST /dialect-variant-suggestions` 留不留？** 只有 V2 建条向导在调（`apps/admin/features/dictionary/word-creation/api.ts:53`），V3 向导不用。留着就是无人调用的端点，删了以后 V3 想要方言建议得重做。 | 小 |
| C3 | **`content_completion` 整个模块怎么办？** 它从 `entry_editor_projection` 读 V2 形状的 `DraftFormsStepContent`（`content_completion/repository.rs:160`），遇到 V3 内容会反序列化失败——也就是说它**现在就已经对 V3 词条不可用**。按记忆「AI 内容补全未启用」，它是死的。选项：(a) 本次不碰，留着 V2 形状的孤儿；(b) 一并下线（约 2444 行 + 2 张表 + 3 条路由）；(c) 移植到 V3。 | 中 |
| C4 | **`SurfaceMatchPageAny` 的 V2 arm 与 `LexiconServiceError` 的 V2 错误变体**（`SurfaceMatchAcknowledgementRequired` / `SurfaceMatchesChanged` / `ExactHeadwordCreationTemporarilyDisabled` / `MultipleActiveExactHeadwordPublicationsNotEnabled`）删不删？删了以后 wire 上 `SurfaceMatchPageAny` 退化成单成员联合，前端的判别逻辑要跟着改；不删就是四个恒不触发的分支。 | 中 |
| C5 | **是否给「读 V2 快照」留一条兜底？** 我的判断是**不留**：库里 0 条 V2、未上正式环境，留下来就是没有测试覆盖的死路径。若要留，只能留 `publication_from_record` 的 `2 =>` 一支并在代码里写明「保留到 X 日期 / Y 条件」——但既然 `entry_publications` 里没有 V2 行，这条兜底连冒烟都跑不了。 | 小 |
| C6 | **拆几个 PR？** 建议 3 个：①A1 迁移工具链（不等前端）②A2+A3+A4 写读路径 + 开关（等前端）③C1 数据库收紧。 | — |

## 3. 顺序与前置

1. **A1 可以现在就做**，不依赖前端。
2. **A2/A3/A4 必须等前端**：前端下线 V1/V2 向导并部署后，后端才能让 `schema_version` 变必填。中间态里前端发不带 `schema_version` 的请求会直接 422。
3. **C1 最后做**，且和「严格 runtime schema 部署顺序」无关（纯服务端）。

## 4. 测试与验证面

- `tests/lexicon_handler.rs`（26138 行，163 个 `#[sqlx::test]`）里约 57 个只走 V2 helper、49 个只走 V3、9 个两者都碰、48 个不碰词条 helper。A2/A3 会带走大部分 V2 用例。
- `tests/lexicon_v3_migration.rs`（2302 行）随 A1 整体删除。
- `tests/lexicon_schema.rs` / `lexicon_v3_storage_schema.rs` / `catalog_handler.rs` / `lexicon_surface_projection.rs` 里对 `content_schema_version` 的断言要跟着 C1 走。
- `src/openapi.rs` 有 154 处 V2 引用；删 DTO 后 spec 会收缩，前端须跑 `pnpm --filter @tsz/api-client sync:openapi` 重新对账，`endpoints.contract.test.ts` 里对 `AdminWordV2` / `CreateAdminWordV2Input` / `SuggestDialectVariantsResponseV2` 的断言要同批删。

## 5. 仓库约束提醒

- 走 PR，不直接推 main。
- 本仓库 pre-commit 只跑 `sqlx prepare` + `clippy`，**不查格式**；改完手动 `cargo fmt`，否则 CI 红。
- 若做 C1，`deployment_migrations.rs` 的 `CURRENT_RELEASE_VERSION` 必须同步，否则 pre-push 的 4 个回退测试齐红。
- 另一会话 `cool-antonelli-f5d4ad` 在 `feat/relation-shape-guards` 上有一个未合的收尾提交 `5948637`，改 `validation/meanings.rs`（4 行）与 `tests/lexicon_handler.rs`（16 行）。本方案 A2/A3 会大改 `tests/lexicon_handler.rs`，需要等它合完再动那个文件。

## 6. 实施结果（2026-09-11）

### 6.1 确认过的四项取舍

| 问题 | 结论 |
| --- | --- |
| 数据库 schema 收紧 | **本次一并做**，迁移 `20260911140000_drop_lexicon_v2_content_schema` |
| `content_completion` | **一并下线**（整模块 + 2 张表 + 3 条路由） |
| 顺序 | **全部等前端**，一个 PR 做完；前端 `claude/drop-word-wizard-v2` 部署后本改动才允许上线 |
| `/dialect-variant-suggestions` | **一并删掉** |

### 6.2 实际删除范围

除 §2 A1–A4 全部执行外，动手中又发现三处必须一并处理，否则会留孤儿：

- `SurfaceMatchItemV3::LegacyV2` / `LegacySurfaceMatchV3`：V3 wire 上专门表示「命中的 surface
  属于 V2 词条」的分支。V2 词条没了就永不产生，连同 v3_surface 里的 `content_schema_version == 2`
  分支一起删。`SurfaceMatchItemV3` 现在只剩 `form_variant_v3` 一个判别值。
- `V3PublicationCapability::{ShadowOnly,MigrationCanary}` 与 `V3PublicationBlockCode`：只服务
  migrated canary，随 `v3_entry_state` 的迁移列一起退场。`SMART_LEXICON_V3_PUBLISH=false`
  的响应因此从 409 `..._requires_migration_canary` 改成 503 `..._storage_unavailable`。
- 保存审计 metadata 里的 `migration_batch_id`：来源列已删，字段随之去掉。

### 6.3 刻意没做的两件事

- **没有把内部规范模型改名或改造成 V3 原生类型。** `DraftMeaningsStepContent` 一族（连同
  `WordSenseV2` / `WordDefinitionV2` / `WordRelationV2` / `DraftFormsStepContent` 等）仍是词义校验
  与存储安全网的内部模型：V3 保存、发布、成分定位三条路径都把 V3 内容适配成这套结构再喂给
  `validate_meanings`。它们已从 OpenAPI 注销，只在进程内存活。把 `validate_meanings`、
  `proposed_nodes`、`resolve_meaning_references`、`meaning_storage_is_safe`、`canonicalize_meanings`
  全部改写成 V3 原生类型是一次独立的大重构，行为回归风险明显高于本次「下线一种 wire 格式」，
  所以留作后续。名字带 V2 但语义是内部模型这一点，已写进 `docs/word-data-model.md` §20.3。
- **没有收紧 `entry_pos` / `surface_sources` 等表的 `content_schema_version` check。** 它们通过
  复合外键 `(entry_id, content_schema_version) → entries(id, content_schema_version)` 绑定，
  entries 收到 `= 3` 之后这些表也不可能再出现 2，不必额外改动。

### 6.4 验证

| 项 | 结果 |
| --- | --- |
| `cargo clippy --all-targets --all-features -- -D warnings` | 绿 |
| `cargo test`（本地 Postgres + Redis） | 全绿 |
| `docs/openapi.json` 重导后 `$ref` 悬空检查 | 0 条悬空 |
| `python3 -m unittest ops/test_ci_test_modules.py` | 绿 |
| 迁移 up + down（`deployment_migrations` 的 4 条回退测试） | 绿，`CURRENT_RELEASE_VERSION` 已改为 `20260911140000` |
