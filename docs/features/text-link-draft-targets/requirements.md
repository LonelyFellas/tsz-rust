# 例句正文关联允许指向草稿词条

> 任务 #13「关联单词，可以从草稿中选择词条」。2026-09-09 用户拍板：**他人的草稿和自己的草稿都可以关联**；
> 同日确认三项选择：宿主照常发布（引用记 draft 范围）、已发布词条只按当前发布版本作候选、**短语成分用词一并放开**。
> 本目录是跨仓（tsz-rust + tsz）唯一的评估文档位置；前端仓只放指路文件。

## 用户与目标

管理员在 Step 3 例句的语音编辑器里做「关联单词 / 关联短语」时，候选只有已发布词条。
词库建设期大量词条同时处于草稿，例句里提到的词往往还没发布，关联只能等对方发布后再补。
目标：**候选同时列出已发布词条与草稿词条（不限创建者），选草稿也能保存、也能发布**。

目标端：admin（tsz `apps/admin`）+ tsz-rust 后端。web 端不涉及。

## 现状（已在本地 dev 复现）

- 建 `job` 草稿后，在 centre6 的例句 `I have a new job.` 上点「关联单词」→ `job`，弹层
  `没有匹配的已发布词条`；请求 `POST /lexicon/entries/component-targets/search` 返回
  `{"matches":[],"total":0}`。
- 根因不在前端过滤：后端 `search_component_targets_v3` 只查 `content_scope = 'current_publication'`
  的词面，注释明写「只回已发布且未归档的词条」，理由是正文关联 `TextLinkV3.target_publication_id`
  必填、校验只读发布快照。

## 追加要求：按词形等值匹配（2026-09-09 用户补充）

现搜索是**包含匹配**：在例句里点 `a` 会列出 `qarelationcheck` 等一切拼写含 a 的词条，真实词库量级下
候选量不可控。关联单词 / 关联短语 / 成分用词的选词面都是句子或短语里现成的词，检索应从
**词形维度**开始：归一化后的词面**相等**才算命中，屈折形（jobs / gave / better）照样命中原形词条，
因为每个词形变体在 `surface_sources` 里都是独立一行。包含匹配不作为兜底（`job` 不包含 `jobs`，
兜底也帮不上屈折；含该词的短语对关联单词而言本就不是合法目标）。

## 主流程

1. 打开例句语音编辑器 → 关联单词 / 关联短语 → 点词 → 弹层列出已发布 + 草稿候选，草稿词条带
   「草稿」标记；仍按 词条 → 词形 → 词义 三层选择。
2. 保存词义步：草稿目标按目标**当前草稿内容**校验（词性 / 词形 / 原形同组 / 词义存在），
   服务端照旧回填 `target_headword` / `target_gloss`。
3. 发布宿主词条：目标仍是草稿 → **照常发布**，引用记为 `draft` 范围（与关系词的稳定锚点方案同款，
   见 `../draft-relation-targets/implemented-stable-target-anchors.md`）；目标此时已发布 →
   自动升级为目标当前发布版本，引用记 `publication` 范围。
4. 目标后来发布了：宿主草稿里的关联在**下一次保存或发布**时由服务端补上 `target_publication_id`，
   历史发布快照不回写。

## 关键异常

- 目标草稿被移入垃圾桶、或被关联的词义从草稿里删掉 → 宿主保存 / 发布 422，沿用现有文案
  「关联目标已不可用，请重新选择」，管理员重选或清除。
- 目标只有草稿范围入链时允许归档；归档后宿主再发布 / 恢复被拒（沿用关系词规则，不新增门禁）。
- 自指（关联当前正在编辑的词条）照旧拒绝。
- 已发布词条有未发布改动：候选**只按其当前发布版本**出现一次，草稿里新增但未发布的词形 / 词义
  不作候选（见未决 Q1）。

## 范围

**做**

- 后端：搜索端点加 `include_drafts`；候选、正文关联、短语成分用词的 wire 允许没有 `publication_id`；
  保存与发布校验支持草稿目标；发布引用写 `draft` 范围（一条迁移放开 CHECK，覆盖 `text_link` 与
  `phrase_component`）；OpenAPI 重导出。
- 前端：类型 / api-client 契约同步；关联单词 / 关联短语 / 成分用词共用的级联一律传
  `include_drafts`、渲染草稿标记、改空态文案；已关联视图标「草稿」。
- 短语「成分用词」（Step 2 词形变体级与 Step 3 释义级 `component_usages`）同样可指向草稿单词。

**不做**

- `sentence-targets/resolve` 的草稿分支（仍按创建者过滤、`pending_only`）：前端无调用者，不动。
- 发布时自动关联（`example-linking-auto-on-publish`）仍只关已发布词条。
- C 端如何消费 `draft` 范围的正文关联：C 端目前不读该字段，不在本次。

## 约束

- `@tsz/types` 保持 snake_case 1:1 镜像；前端 runtime schema `additionalProperties:false`，
  新字段只在 `include_drafts = true` 的响应里出现，旧前端永不触发（见 design 发布顺序）。
- 草稿默认全员可见、只有创建者 + 超管可写（`draft-visibility-write-guard`）：关联只读目标，
  不受写权限限制。
- 目标 / 宿主的节点 ID 跨保存与发布稳定（`lexicon.nodes`），关联锚定
  `(target_word_id, pos / form / variant / sense 节点 ID)` 即可，不需要 revision。

## 验收标准

| # | 标准 | 验证 |
| --- | --- | --- |
| AC1 | `job` 仅为草稿（由另一管理员创建也一样），centre6 例句上关联单词 → 弹层列出 job（草稿标记）→ 选词形词义 → 保存词义步 200，重开显示「已关联：job · 释义」 | 本地 dev 真机 + 前端组件测试 |
| AC2 | 同一词面既有已发布又有草稿词条时两者都列，已发布在前；已发布且有未发布改动的词条只出现一次且带 `publication_id` | 后端 handler 测试 |
| AC3 | `include_drafts` 不传或 false：响应形状与内容与改动前一致 | 现有 `component_target_search_*` 测试保持通过 |
| AC4 | 宿主发布时目标仍草稿：201；`entry_publication_sense_refs` 该行 `reference_kind='text_link'`、`target_content_scope='draft'`、`target_publication_id IS NULL`、`target_revision` = 目标 `entries.revision`；发布快照里该 link 无 `target_publication_id` | 后端 handler 测试 |
| AC5 | 目标先发布、宿主再保存：宿主草稿 link 被回填 `target_publication_id` = 目标当前发布；再发布 → 引用 `publication` 范围 | 后端 handler 测试 |
| AC6 | 目标草稿删掉被关联词义 / 移入垃圾桶后，宿主保存词义步 422，issue `field = "text_links"`、现有文案 | 后端 handler 测试 |
| AC7 | `kind` 过滤、分页游标、`truncated` 语义在 `include_drafts = true` 下与原来一致，草稿计入 `total` | 后端 handler 测试 |
| AC8 | `docs/openapi.json` 重导出后前端 `sync:openapi`，契约测试与 runtime schema 校验全绿；`publication_id` / `target_publication_id` 变可选 | `pnpm --filter @tsz/api-client test` |
| AC8a | 关联单词点 `a`：候选只列词形等于 `a` 的词条，不再出现 `qarelationcheck`；点 `jobs` 命中 `job` 词条且命中词形为复数 | 后端 handler 测试 + 本地真机 |
| AC8b | 现有调用方不传 `match` 时仍是包含匹配，响应与排序不变 | 现有 `component_target_search_ranks_*` 测试保持通过 |
| AC9 | 短语成分用词：候选含草稿单词（带标记），选中后词形步 / 词义步保存成功，`component_usages[].target_publication_id` 缺省 | 后端 handler 测试 + `V3PhraseComponentUsagesCard.test` |
| AC10 | 短语发布时成分目标仍草稿：201，`sense_refs` 行 `reference_kind='phrase_component'`、`target_content_scope='draft'`；目标已发布则自动升级同 AC5 | 后端 handler 测试 |
| AC11 | 成分目标为草稿短语时仍受「短语套短语只一层」约束；目标草稿事后新增短语成分，宿主再保存 / 发布被拒 | 后端 handler 测试 |

## 未决问题（默认按建议执行，有异议请在确认时提出）

- **Q1** 已发布词条草稿里新增、尚未发布的词形 / 词义要不要作为草稿候选？**建议不要**：
  发布版本才是对外口径，未发布改动可能永远不发；混进来还得处理同一词条两份身份的去重与升级。
- **Q2** 候选排序：同档位内**已发布优先于草稿**，再按 headword。
- **Q3** 「草稿」标记的展示：候选词条标签后加 antd `Tag`，已关联视图文案后缀「（草稿）」；
  不新增 wire 字段，前端以 `publication_id` 缺省判定。

## 确认记录

2026-09-09 用户确认：发布行为选「照常发布，引用记 draft 范围」；候选口径选「只按当前发布版本」（Q1 采纳）；
成分用词**一起放开**（原「不做」项改入范围）。Q2 / Q3 未提异议，按建议执行。
同日补充：三个选词弹层一律改为词形等值匹配（见「追加要求」），包含匹配仅保留给未传 `match` 的旧调用方；
用户明确确认**成分用词也走等值匹配**。
