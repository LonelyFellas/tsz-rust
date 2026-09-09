# 设计：正文关联的草稿目标

> 核对基线：tsz-rust `dev` = `main` @ `e2f3ed8`，tsz `dev` = `main` @ `b531f11`（2026-09-09）。
> 行号即该基线。需求见同目录 `requirements.md`。

## 1. 现有能力与依赖（已逐条读码）

| 能力 | 位置 | 结论 |
| --- | --- | --- |
| 关键字搜索 | `service/sentence_target_discovery.rs:196` `search_component_targets_v3` → `repository/sentence_target_discovery.rs:71` `published_component_target_surfaces` | 只查 `surface_sources.content_scope = 'current_publication'`，再按 `current_publication_snapshots` 组装 |
| 草稿词面投影 | `lexicon.surface_sources` `content_scope = 'draft'` | 本地验证 `job` 草稿有 16 行 `form_variant`，`pos_id / form_id / source_node_id / entry_kind / form_type` 齐全，`publication_id` 为 NULL —— 可直接复用同一条 SQL 换 scope |
| 候选组装 | `service/sentence_association.rs:546` `PublishedAssociationTarget::from_v3(AdminWordV3)`；`:634` `sentence_discovery_candidates(publication_id, pos_id, form_id, variant_id, evidence)` | 草稿只要能拿到 `AdminWordV3` 就能走同一条组装，`publication_id` 参数改 `Option` |
| 正文关联 wire | `dto/v3.rs:607` `TextLinkV3.target_publication_id: Uuid`；`TextLinkViaPhraseV3.publication_id: Uuid` | 改 `Option`，缺省 = 草稿目标 |
| 保存 / 发布校验 | `service/text_links.rs:203` `validate_targets`，保存 `service/v3.rs:2052`、发布 `service/v3_publication.rs:168` 共用 | 目标由 `load_phrase_component_target(word_id, publication_id)`（`v3.rs:2792`）只读发布快照；返回 `NewPublicationSenseReference`（`Publication` 范围）供发布落表 |
| 引用表 | `migrations/20260822150000_allow_publication_relations_to_draft_targets.up.sql` | 已有 `target_content_scope / target_revision`、`target_node_fkey`；但 `context_target_check` 只允许 `reference_kind = 'relation'` 用 `draft` |
| 悬空 / 生命周期 | `repository/publications.rs:878,931` | 判悬空的 SQL 已把 `text_link` 与 `relation` 同款处理（发布或草稿任一侧存在即有效）；`current_inbound_sense_refs`（`:774`）不分 kind |
| 关系词草稿目标样板 | `repository/publications.rs:650` `resolve_relation_targets_for_publish`；`service/publishing.rs:1279` `relation_target_snapshot` | 同一查询同时给出发布侧与草稿侧，`FOR SHARE OF entry NOWAIT` 锁目标；草稿侧 `target_revision = entry.revision` |
| 发布输出 | `service/text_links.rs:318` `apply_manual` → `WordSentenceAssociationV3::Linked.target_publication_id: Option<Uuid>` | 已是可空，直接透传 |
| 前端级联 | `apps/admin/.../components/V3TargetCascader.tsx` | `groupsFromCandidates` 用 `sense.publication_id` 填 `target_publication_id`（`:192`）；空态文案 `:515`；被 `V3TextAssociationPicker`（正文关联）与 `V3PhraseComponentUsagesCard`（成分用词）共用 |
| 前端契约 | `packages/api-client/src/admin-word-v3.runtime-schema.json` | `PublishedSentenceTargetCandidateV3 / SentenceTargetSenseV3 / TextLinkV3` 的 `publication_id` 都在 `required`，`additionalProperties: false` |

## 2. 选定方案

一句话：**搜索按请求开关多查一层草稿词面，候选与关联用「没有 `publication_id`」表示草稿目标，
校验按目标当前草稿内容做，发布引用走已有的 `draft` 范围**。不引入新状态字段、不改锚点模型。

### 2.1 wire

```text
SearchComponentTargetsV3Input   + include_drafts?: bool          // 默认 false
SearchComponentTargetsV3Input   + match?: "contains" | "exact"   // 默认 contains（兼容旧调用方）
PublishedSentenceTargetCandidateV3.publication_id : Uuid → Option<Uuid>  // 缺省 = 草稿，序列化省略
SentenceTargetSenseV3.publication_id              : Uuid → Option<Uuid>  // 同上
TextLinkV3.target_publication_id                  : Uuid → Option<Uuid>  // 缺省 = 草稿目标
TextLinkViaPhraseV3.publication_id                : Uuid → Option<Uuid>  // 经草稿短语的成分转关联
PhraseComponentUsageV3::Resolved.target_publication_id : Uuid → Option<Uuid>  // 成分用词的草稿目标
```

- 全部 `#[serde(default, skip_serializing_if = "Option::is_none")]` + `#[schema(nullable = false)]`，
  与 `WordSentenceAssociationV3::Linked.target_publication_id` 现有写法一致。
- 类型名 `PublishedSentenceTargetCandidateV3` 不改（前端契约测试按 schema 名对账），
  在 doc comment 里说明「`publication_id` 缺省即草稿候选」。

### 2.2 搜索（后端）

`search_component_targets_v3`：

0. `match = exact` 时关键字先走与 `surface_sources.normalized_surface` 同一套归一化
   （`normalize_headword` + `HEADWORD_NORMALIZATION_VERSION`，与 resolve 的 `validate_selected_segments` 同源），
   SQL 的 `source.surface ILIKE $3` 换成 `source.normalized_surface = $3`；排序里的等于 / 前缀 / 包含档位
   在 exact 下退化为同一档，直接按 headword 排。`component_target_cursor_digest` 把 `match` 一起摘入。
   多词选区（关联短语）传的是空格连接的词面，归一化后与短语词条的词形行相等即命中。
1. 现有已发布查询不动（`contains` 分支）。
2. `input.include_drafts` 为真时追加 `draft_component_target_surfaces`（新仓库函数，
   复制 `published_component_target_surfaces` 的 SQL，改为
   `source.content_scope = 'draft' AND entry.current_publication_id IS NULL AND entry.archived_at IS NULL
   AND entry.content_schema_version = 3`，**不按 `created_by_admin_id` 过滤**，同样的 ILIKE / kind /
   pos-form 非空 / 排序 / `LIMIT`）。`SentenceDiscoverySurfaceRecord.publication_id` 改 `Option<Uuid>`。
3. 草稿目标加载：新 `load_draft_targets(tx, &entry_ids) -> HashMap<Uuid, PublishedAssociationTarget>`，
   从 `lexicon.entries` 取 `forms / meanings` JSON 反序列化成 `AdminWordV3` 再 `from_v3`
   （实施时以 `from_v3` 实际读取的字段为准决定是否需要 `presentation`；它今天从
   `entry_editor_projection` 之外的 `entries` 行就能得到 headword / kind）。
4. `published_candidates` 改名为 `candidates`，`sentence_discovery_candidates` 的
   `publication_id` 改 `Option`；分组键里的 `publication_id` 用 `Option`。
5. 合并后排序：`rank(等于 / 前缀 / 包含) → 已发布优先 → headword`；`total / truncated / next_cursor`
   逻辑不变，`truncated` 取两次扫描的 OR。游标摘要把 `include_drafts` 一起摘入
   （`component_target_cursor_digest`），避免翻页中途换开关串页。
6. `resolve_sentence_targets_v3` 不动。

### 2.3 保存 / 发布校验（后端）

三处校验（正文关联 `text_links::validate_targets`、词形步成分 `validate_phrase_components`
`v3.rs:2524`、释义级成分 `validate_sense_phrase_components` `v3.rs:2876`）今天都用
`load_phrase_component_target(word_id, publication_id)` 取目标。抽一个共用入口
`resolve_component_target(tx, word_id, Option<publication_id>) -> ResolvedComponentTarget { word: AdminWordV3,
scope: Publication{publication_id, revision} | Draft{revision} }`，「升级 / 回退到草稿」的规则只写一次：

- `Some(publication_id)`：现路径不变。
- `None`：
  1. 读目标 `entries` 行（`archived_at IS NULL`，不存在 / 已归档 → 「关联目标已不可用」）；
  2. 若 `current_publication_id` 非空且引用的 pos / form / variant / sense 节点都在该发布快照里
     → **升级**：调用方把 `target_publication_id` 回填为 `Some(current)`，之后按发布路径处理
     （回填随保存写入草稿，满足 AC5）；
  3. 否则按草稿内容（`entries.forms / meanings` → `AdminWordV3`）跑同一套一致性校验，
     引用记 `NewPublicationSenseReference { target_publication_id: None, target_content_scope: Draft,
     target_revision: entry.revision }`。

正文关联 `validate_targets` 的具体改动：

- 目标键从 `(word_id, publication_id)` 变 `(word_id, Option<publication_id>)`。
- `Some(publication_id)`：现路径不变。
- `None`：
  1. 读目标 `entries` 行（`archived_at IS NULL`，不存在 / 已归档 → 「关联目标已不可用」）；
  2. 若 `current_publication_id` 非空且 link 引用的 pos / form / variant / sense 节点都在该发布快照里
     → **升级**：`link.target_publication_id = Some(current)`，之后完全按发布路径处理（回填后随保存写入草稿，
     满足 AC5）；
  3. 否则按草稿内容（`load_draft_targets`）跑同一套 `target_gloss` / `component_matches` 校验，
     引用记 `NewPublicationSenseReference { target_publication_id: None, target_content_scope: Draft,
     target_revision: entry.revision }`。
- 末尾的批量核验：发布范围沿用 `phrase_component_publication_targets_for_publish`；草稿范围新增
  `draft_text_link_targets_for_publish(tx, &[(entry_id, sense_id)])`：仿 `resolve_relation_targets_for_publish`
  的 `JOIN lexicon.nodes ... FOR SHARE OF entry NOWAIT`，校验 sense 节点仍在草稿（`removed_from_draft_at IS NULL`）
  并取 `entry.revision`；数量不符 → `ReferenceConflict`（与现有语义一致）。保存路径同样走这段
  （今天保存也会跑 `..._for_publish`，行为对齐）。
- `via_phrase.publication_id` 为 `None` 时短语目标同样按上述规则加载；`component_matches` 不区分来源。

成分用词两处校验的改动：

- `(target_word_id, target_publication_id)` 键改 `Option`，目标经 `resolve_component_target` 取得；
  `phrase_component_matches_target` 与文案生成不变。
- 「短语套短语只一层」（`v3.rs:2586` 起）原本依赖发布快照不可变；目标是草稿短语时它可能事后再加短语成分，
  所以发布路径 `phrase_component_publication_references`（`v3_publication.rs:838`）对 `Draft` 范围目标
  **重跑嵌套检查**，不成立即 422（AC11）。
- `phrase_component_publication_references` 把请求拆成发布 / 草稿两组：发布组沿用
  `phrase_component_publication_targets_for_publish`，草稿组与正文关联共用新函数
  `draft_sense_targets_for_publish`，引用 kind 仍为 `PhraseComponent`。

### 2.4 迁移

`migrations/<ts>_allow_text_link_refs_to_draft_targets.up.sql`：

```sql
ALTER TABLE lexicon.entry_publication_sense_refs
    DROP CONSTRAINT lexicon_publication_sense_refs_context_target_check,
    ADD CONSTRAINT lexicon_publication_sense_refs_context_target_check
        CHECK (
            reference_kind IN ('relation', 'text_link', 'phrase_component')
            OR target_content_scope = 'publication'
        );
```

down 反向恢复（前提是没有 `text_link / phrase_component + draft` 行，回退步骤里先清）。
`sentence_context` 保持只允许 `publication`。`deployment_migrations.rs` 若维护迁移清单需同步登记。

### 2.5 生命周期与引用门禁

不改代码，只核对：

- 悬空判定（`publications.rs:878,931`）已把 `text_link`、`phrase_component` 与 `relation` 同款；
  `draft` 行由 `target_node_fkey` 保护，硬删目标节点被 RESTRICT。
- 归档目标：`current_inbound_sense_refs` 不分 kind，只看来源当前发布是否引用目标 → text_link 的
  `draft` 入链会像 relation 一样让目标「有活跃入链」而被拒归档？—— 关系词文档说「只存在草稿入链时，
  目标允许归档」，该判断在 `lifecycle.rs` 里按 `target_content_scope` 区分；实施时把 `text_link` 纳入同一分支，
  并补一条测试（AC 里未单列，作为 2.6 的回归项）。`phrase_component` 同理。

### 2.6 OpenAPI 与前端契约

1. 后端 `cargo run --bin export_openapi` 重导出 `docs/openapi.json`。
2. 前端 `pnpm --filter @tsz/api-client sync:openapi` 更新快照与 runtime schema；`endpoints.contract.test.ts`
   里 `TextLinkV3.required` 断言改为不含 `target_publication_id`。
3. `packages/types/src/admin-word-v3.ts`：上述六处字段改可选（含 `PhraseComponentUsageV3` resolved 分支），
   `SearchComponentTargetsV3Input` 加 `include_drafts?: boolean`。

### 2.7 前端（admin）

| 文件 | 改动 |
| --- | --- |
| `word-creation-v3/api.ts` | `searchComponentTargets` 透传 `include_drafts` / `match`（类型来自 api-client，无逻辑） |
| `components/V3TargetCascader.tsx` | 两个调用方都要草稿也都要等值匹配，不加 prop：请求固定带 `include_drafts: true, match: "exact"`（成分展开子查询 `loadComponent` 同样）；`groupsFromCandidates` 的 `target_publication_id: sense.publication_id`（可为 `undefined`，缺省时不写该键）；`CandidateEntryGroup` 加 `draft: boolean`（`candidate.publication_id === undefined`），词条标签后渲染 `<Tag>草稿</Tag>`；空态文案改「没有匹配的词条」，截断分支改「前 50 条命中里没有可关联的词条，请换更具体的关键字」 |
| `components/V3TextAssociationPicker.tsx` | `onSelect` 只在 `chosen.target_publication_id` 存在时带该键（避免发 `null`）；已关联视图：`selected.target_publication_id` 缺省时文案后加「（草稿）」 |
| `components/V3PhraseComponentUsagesCard.tsx` | 无逻辑改动；已选成分的展示若有词条名处同样标「（草稿）」；测试里 5 处「没有匹配的已发布词条」断言改新文案 |
| `word-creation-v3/model.ts` | `isResolvedUsage` 的 `target_publication_id` 改为「缺省或字符串」，`ownKeysAre` 允许缺该键；`fixtures.ts` 补一条草稿目标夹具 |
| `packages/voice-editor` | 只透传 `TextLinkV3`，类型变可选后无需改动（`tsc` 兜底） |
| `operations.ts` / `publicationIssueSummary.ts` / `meaningsModel.ts` | 读 `component_usages` 但不碰 `target_publication_id`，实施时以 `tsc` 复核 |

### 2.8 数据与状态流

```text
关联单词 → searchComponentTargets{ q, kind, include_drafts:true }
        → matches[已发布(有 publication_id) ∪ 草稿(无)]
        → 选词义 → TextLinkV3{ target_word_id, 节点 ID…, target_publication_id? }
保存词义步 → validate_targets:
        Some → 发布快照校验（现状）
        None → 目标已发布且节点在内 ⇒ 回填 Some 后走上行
             → 否则草稿校验 + Draft 引用
发布宿主 → 同一校验 → sense_refs 落 draft/publication 行 → 快照里 link 原样（可无 publication_id）
```

## 3. 兼容与发布顺序

- **后端先、前端后**。旧前端不传 `include_drafts`，响应里每条候选仍带 `publication_id`，
  请求里 `target_publication_id` 一直有值 → 旧 runtime schema 不会撞上新形状。
- 新前端对旧后端：`include_drafts` 被 `deny_unknown_fields` 拒 400？—— `SearchComponentTargetsV3Input`
  实施时核对是否 `deny_unknown_fields`；若是，前端必须等后端上线后再发（本地与测试服都是同批部署，
  不构成阻塞，但要写进 deploy 记录）。
- 本地 dev：后端 `cargo run` 自动跑迁移；测试服按 `backend deploy` 流程。

## 4. 风险与验证

| 风险 | 验证 |
| --- | --- |
| 草稿候选与已发布候选同词条重复（已发布且有未发布改动） | 草稿 SQL 限定 `current_publication_id IS NULL`；测试 AC2 |
| 旧客户端 / 旧游标混入 `include_drafts` / `match` | 游标摘要含两个开关；测试：旧游标 + 新开关 → 400 `invalid_query` |
| 等值匹配漏掉大小写 / 连字符 / 撇号差异 | 与 `surface_sources` 用同一归一化函数；测试：`Jobs`、`don't` 等按 resolve 既有用例口径命中 |
| 目标草稿被改，宿主保存突然 422 | 与已发布目标被下架时的现状一致；测试 AC6 |
| `draft` 引用导致目标归档门禁误判 | 2.5 核对 + 归档测试：仅 text_link 草稿入链时允许归档，归档后宿主发布被拒 |
| 前端 runtime schema 严格 | AC8；`endpoints.contract.test.ts` 更新 |
| 升级路径把用户未保存的选择改掉 | 只在服务端保存 / 发布响应里回填，与 `target_headword` 回填一致，前端本来就用响应覆盖 |

后端测试落点 `tests/lexicon_handler.rs`（现有 `component_target_search_*` 与 `v3_text_link_*` 段落）；
前端 `V3TextAssociationPicker.test.tsx`、`V3TargetCascader` 空态、api-client 契约测试；
真机按 requirements AC1 在本地 dev 走一遍（job 草稿已在库里）。

## 5. 回退限制

- 前端可单独回退（不再传 `include_drafts`，已存的草稿关联仍能显示与清除）。
- 后端回退需先处理 `text_link + draft` 的引用行（迁移 down 会撞 CHECK）；宿主草稿里无
  `target_publication_id` 的 link 在旧后端会被 `deny_unknown_fields` 之外的必填校验拒掉 → 回退前
  须确认线上没有此类草稿，或与前端一起回退。

## 6. 关键路径估计

后端（wire + 搜索 + 三处校验共用入口 + 迁移 + 测试）约 1.5 天；前端（类型 / 契约 / 级联 / 成分卡片 / 测试）约半天；
真机验收与文档收尾半天内。

## 7. 实施记录（2026-09-09）

与上文方案的出入与补充，以代码为准：

- **迁移不止一条 CHECK**：两张成分用词表（`v3_phrase_variant_component_usages`、`v3_phrase_sense_component_usages`）
  的 `lexicon_v3_phrase_components_shape_check` 原要求已解析成分 `target_publication_id IS NOT NULL`，
  一并放开（`migrations/20260909150000_allow_component_refs_to_draft_targets`）。指向 `entry_publication_nodes`
  的外键在发布版本为空时自然不生效，指向 `lexicon.nodes` 的外键继续保护草稿目标。
- `deployment_migrations.rs` 的回退测试把 `CURRENT_RELEASE_VERSION` 指到本迁移（20260909150000）。
- 共用入口落在 `service/v3.rs`：`ComponentTargetWord` / `ComponentTargetScope` / `resolve_component_target(probe)` /
  `load_draft_component_targets`；`text_links::validate_targets`、`validate_phrase_components`、
  `validate_sense_phrase_components` 都改成先按 (词条, 发布版本) 归组、整组 probe 决定升级还是走草稿，
  再回填 `target_publication_id`。发布路径只在存在草稿目标成分时重跑两处成分校验。
- 仓库层 `component_target_surfaces(exact, drafts)` 取代 `published_component_target_surfaces`，SQL 由常量片段
  拼接（sqlx 0.9 需 `AssertSqlSafe`）；新增 `draft_sense_targets_for_publish` 供发布时锁草稿目标。
- 正文关联目标词义不在目标里的文案改为「关联的词形或词义已不在目标词条里，请重新选择」（原文案提「发布版本」，
  对草稿目标不成立）。
- 前端：`V3TargetCascader` 固定发 `match: "exact", include_drafts: true`，候选词条标签后加「草稿」`Tag`
  （样式在 `V3SentenceTargetDiscovery.css`）；`model.ts` 的成分形状校验允许缺 `target_publication_id`；
  api-client `sync:openapi` 后契约测试里的源 sha 与候选 required 断言同步更新。
- 测试：后端 `tests/lexicon_handler.rs` 新增 4 例（搜索含草稿与等值匹配、正文关联草稿目标发布 / 升级、
  目标删词义后 422、成分用词草稿目标发布 / 升级）；前端 `V3TextAssociationPicker.test.tsx` 新增草稿标记与
  已关联视图 2 例，`V3PhraseComponentUsagesCard.test.tsx` 文案断言更新。
- 前端追加一条 UX：候选词条**没有词义**（草稿常见——词义步还没保存）时不再当作「没搜到」，而是列成
  禁用行「headword（暂无词义）」并保留草稿标记；本地 AC1 首轮就踩到这一点（job 草稿只保存了词形步）。
- 真机验收（本地 dev，2026-09-09）：centre6 例句 `I have a new job.` 关联单词 → 弹层列出 `job / job`
  带「草稿」→ 原形 job（名词）→ 工作；职业 → 保存词义步 200，库里 link 无 `target_publication_id`、
  `target_headword` / `target_gloss` 已回填；重开编辑器点 job 显示「已关联：job / job · 工作；职业（草稿）」。
