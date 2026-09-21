# 智能词库架构重构：实施设计与分批

关联需求： [requirements.md](requirements.md)。重档：跨模型、发布、权限、数据库及前后端契约。

## 实施顺序

| 批次 | 范围 | 可观察完成条件 |
| --- | --- | --- |
| 准备 | 决策固化、后端事实核对、消费者差距调查 | 已确认事实与待验证风险分开，记录未调查范围 |
| 1 | 内部模型：释义保存、读取、发布优先 | 不再从有损旧结构重建完整内容；字段保真、规范化与引用快照验证通过 |
| 2 | 统一引用及影响分析、草稿候选、搜索分页 | 能定位引用来源和受影响节点，规则一致，不以 outbox 行数标识分页版本 |
| 3 | 编辑/发布权限、批次原子发布、新版本式回退 | 权限一致，循环依赖可批量发布，任意失败整体回滚，回退不覆盖草稿 |
| 4 | 共享例句独立版本、局部移除、全局下架 | 词条与例句发布边界清楚，学习端和编辑端各取正确状态 |
| 5 | 音频引用保护、并发交互、性能、同形词、集成收口 | 历史可回放、编辑不丢失、操作可追溯，满足实测目标 |

性能、审计和数据保护在各自写路径变更时完成，第五批只做跨链路收口。

## 基线证据与差距（实施前）

- `service/v3.rs::save_meanings_v3` 和 `service/v3_publication.rs::publish_v3` 通过旧聚合序列化往返并调用多项 restore。首批应先使规范化和引用解析直接操作完整内容，旧关系表写入所需的裁剪结果只能是单向投影，不允许反向重建。
- `service/inbound_references.rs` 已集中部分入站检查，不能另建平行检查体系；需改变草稿编辑与发布的边界，补齐引用修复与版本语义。
- `service/v3_publication.rs` 已有幂等、事务发布及历史激活。历史激活不是目标需求中的新版本式回退，需要替换而不是双轨保留。
- `shared_sentences/mod.rs` 目前直接覆盖内容、更新 revision；还不是独立草稿/发布版本。修改和删除权限、审计、引用关系生效时点均需调整。
- `repository/query.rs::related_search_dataset_version` 使用 outbox count，`service/queries.rs` 据此使游标失效；第二批移除该耦合。
- `repository/surface_writes.rs` 对死锁及超时已有业务冲突映射。验证并优化获取顺序，不把它误报成无并发保护。
- 共享例句列表逐 ID 调 `read_on`，需要批量查询。
- 尚未核实：前端冲突草稿保留、学习端实际消费入口、全部音频引用持有者、同形词确认界面、发布权限的现有细分。未核实不能当作缺失结论。

## 第一批落地方案

1. 复用完整 V3 内容结构作为唯一内容载体，不再新造一套字段相同的序列化模型。保存、发布、校验、节点生成、仓储写入和例句扫描均直接消费原生结构。
2. 原位规范化完整内容，引用解析直接修改引用字段；音频展示元数据仍从数据库校准。
3. 删除 `DraftMeaningsStepContent`、`DraftFormsStepContent` 及旧聚合族，删除单向 `meanings_relational_projection` 和反向重建/restore 函数；不保留别名或双轨分流。
4. `proposed_meaning_nodes` 直接包含全量译文和释义级成分，不把主译文别名重复建节点，不把只读例句关联当作可写节点。不按 ID 去重来掩盖节点冲突，冲突必须返回可定位错误。
5. 校验和节点定位直接使用真实词形，不再生成空 base form 或虚假词头。语法方言规则显式允许 common 或完整 uk/us；目录引用检查使用真实词形代码。
6. 关系表不再显式写入已停用的预绑定字段；现有 nullable 列保留原 schema，本批无新 migration。
7. 不顺便改变发布依赖的产品判定；新规则在第二、三批切换。缺字段保留等既有客户端协议尚未在本批修改，不能与旧聚合转换混为一谈。
8. 保真测试覆盖翻译、成分、语音、音频、文本链接、词义组和只读关联；增加非主译文与词义 ID 冲突的 HTTP 回归，验证拒绝且无部分写入。

## 第二批差距与实施路径

基线：后端 `cbf2c2632da65962fe96d3284fece2bec08dfa99`（PR #181），前端 `3a1bd05`；分别从 fetch 后的 origin/main 创建 `feat/lexicon-domain-batch-2` worktree。

实际已有能力与差距：

- `inbound_references.rs` 已统一五类入站引用采集、节点计数、来源摘要和失效判定，前端 `V3ReferenceList/referenceGuard` 已能打开来源词条并通过 `focus_node` 定位，例句可跳例句库。复用这些实现，不另建引用表或平行检查器。
- 当前草稿保存与前端节点控件仍按入站引用阻断删除；目标规则要求可编辑草稿、在发布边界处理破坏性影响。调整前须核对节点墓碑、关系表约束、共享例句即时生效路径，不能仅移除守卫。
- 关联搜索默认只返回发布快照，显式 include_drafts 只补“从未发布”的词条，漏掉已发布词条中的新增词义。成分目标加载也优先整条发布快照；候选与保存必须按具体节点一致解析，不能仅把搜索 SQL 放开。
- `publishing.rs` 允许关联词发布时指向草稿；例句 context 则要求发布目标。单独发布具体节点有效性的规则须与第三批批次发布边界协调，不能把当前不同规则标为已统一。
- `queries.rs/repository/query.rs` 的关联搜索已有签名游标、查询/管理员绑定、确定性复合键排序，但以 outbox count 使任何相关事件导致游标失效，且以首页冻结 total 判断是否还有下一页，会在并发插入后过早结束。
- `sentence_target_discovery.rs::search_component_targets_v3` 另有 generation + 可用词条集合摘要 + offset 分页；它并不是关联搜索的同一个游标，后续要独立改为稳定键，不能只删 generation 校验继续使用 offset。句中候选分页也需分开验证。
- 前端关联词 `V3MeaningsAndExamplesStep` 和成分级联器 `V3TargetCascader` 当前主动发送 include_drafts=true，尚不符合“默认发布、主动展开草稿”。`referenceGuard` 的已有修复链接不能证明用户已能保存删除草稿后再修复；`WordWizardV3` 仍按 blocked_references 阻断保存。
- 已核对的学习端路径：`apps/web/src/features/practice/components/PracticeBoard.tsx` 是占位 UI，`features/wordlist/hooks/useWordLists.ts` 直接返回/修改 MOCK_WORDLISTS，真实 API 调用仍为 TODO。当前 Rust `src` 无 learning/wordlist 服务模块。不能把这些页面的 mock 运行当作词库发布消费或历史回放的联调证据；本批不扩大为新建学习系统。

实施顺序及可观察条件：

1. 先完成独立的关联搜索弱一致分页：删除 outbox 版本读取与重试，保留签名及查询参数校验；按当前页剩余结果数量决定 next_cursor，total 仅作为弱一致估计，不驱动结束条件。测试跨页无关更新、插入/归档、查询及签名不匹配；不提供旧游标兼容分支。
2. 引用规则及影响分析：沿用原采集和判定，核对草稿/发布目标及各来源生命周期，再切换保存与发布边界；修复入口复用 focus_node 并补缺失定位反馈。具体契约改动随调查补齐，未实现前不声明完成。
3. 草稿候选：覆盖已发布词条的新增具体节点，并同步关联词、成分和例句候选/保存解析；默认不展开，展开后标出具体未发布依赖。不能仅靠词条级 status 判定。
4. 同步确有变化的 OpenAPI、前端类型与 runtime schema；执行受影响回归和两仓质量门。

分页子项不改 method/path、DTO 或 schema，无 migration。旧前端已消费 next_cursor，响应 wire 不变；搜索弱一致不替代保存/发布严格校验。后续契约破坏性变更按已确认的停流配套发布，不增加兼容层。本任务不执行任何发布操作。

## 第二批续作：成分与句中候选分页

续作基线：后端 `c33b8b2fd32be7fcba711a1de6138929fd848879`（PR #182，关联搜索分页已合入并由用户确认部署），前端 `3a1bd05`。不重复修改 `related_search`。

- `service/sentence_target_discovery.rs`：成分搜索按 `(match_rank, draft, headword, entry_id, pos_id, base_form_id, matched_variant_id)` 做 keyset；句中已发布候选按末尾四个稳定节点 ID 做 keyset。发布版本 ID 不进入排序键，重新发布不移动同一节点。
- 成分游标保留 q/kind/match/include_drafts/entry_id 的查询绑定；不再绑定 generation 或可用词条集合。句中游标保留方言与完整句子/片段指纹绑定，自动发现返回的游标可以交给 selected_segments 继续翻页。
- 当前请求内仍用一致读及当前发布快照组装候选。跨请求为弱一致，排序键之前新插入的结果需刷新查看；total 是本次计数，不决定是否继续。排序键之后新增的结果可继续遍历，删除前页结果不跳过后页。
- 新游标只接受稳定键格式，不兼容旧 offset 游标；旧页面需刷新搜索。不改 method/path 或 JSON 字段形状，无迁移及 SQLx 查询变动；OpenAPI 仅更新说明。
- 新增/调整验证：成分 HTTP 跨页插入和归档、generation 改变、查询不匹配、超过 200 词条遍历；句中 HTTP 自动转手动翻页、前页归档、方言/文本/无效游标拒绝；单元验证发布 ID 不影响节点分页身份。

引用及候选剩余调查：`v3.rs::resolve_component_target` 已支持当前发布快照 probe 不满足时回落当前草稿，不应重写成另一个解析器。搜索入口仍过滤从未发布词条；放开过滤前必须拆开同一 entry 的发布/草稿目标映射，当前按 entry_id 单键的 map 会使两种内容互相覆盖。`inbound_references.rs` 的发布检查仍仅收共享例句与发布词义引用，草稿保存守卫不能孤立移除。这些不是本分页改动已完成的能力。

## 数据与契约

- 第一批不改 API 结构或数据库 schema。OpenAPI 重导只有三个共享类型的描述更新，去掉 `description` 后与基线完全一致；无需前端同步运行时类型。SQLx 在任务隔离库刷新，缓存无差异。
- 后续例句版本、权限及发布命令需要新契约与成对 up/down 迁移；具体字段与端点须在对应批次按实际代码确定，本设计不伪造已完成的详细方案。
- 不转换旧测试内容。需要重置时记录具体环境、表范围和执行条件，获得执行确认后再操作。
- 破坏性契约发布采用受控停用相关功能/流量、部署配套前后端、验收后恢复；不允许以“同批部署”为理由让新旧版本不兼容的中间态继续服务。
- 当前无提交、推送或部署操作；源代码回退按独立改动回退，数据库清理后不能声称恢复旧测试数据。

## 验收

在任务 worktree 执行；不启动服务器或自动迁移未知数据库。

```bash
SQLX_OFFLINE=true cargo test --locked --lib lexicon::service::publishing
SQLX_OFFLINE=true cargo test --locked --lib lexicon::rich_text
SQLX_OFFLINE=true cargo clippy --locked --all-targets --all-features -- -D warnings
cargo fmt --all --check
```

预期：定向测试通过、clippy 无警告、格式通过。编译不代表数据库行为验证。

数据库验证待核实 `.agents/skills/dev-env/SKILL.md` 与 sqlx 测试隔离方式后执行：

```bash
cargo test --locked --test lexicon_handler
cargo test --locked --test lexicon_v3_lifecycle
cargo test --locked --test lexicon_v3_relation_consumers
```

当前结果与剩余工作记录于 `progress.md`，不把此处命令清单当作已执行证据。
