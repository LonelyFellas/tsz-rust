# 第四批：共享例句独立发布与宿主可见性

状态补记（2026-09-23）：用户交接确认，第四批已由后端 PR #186 / 前端 PR #320 配套部署到 tshb-test，迁移至 `20260923030000`；全部业务组合的上线后人工验收尚未逐项完成。以下正文保留实施时的设计与验证记录，不代表当前未部署。历史数据清理授权不延续到第五批；第五批状态见 [batch-5-design.md](batch-5-design.md)。

## 基线与目标

- 后端基线：`02f883ee2a910207ff7ee850f2941fc4b45f76ee`（第三批 PR #185）；任务分支 `feat/lexicon-domain-batch-4`，工作区 `tsz-core/.worktrees/backend/lexicon-domain-batch-4`。
- 前端实施基线：`origin/main d94afb5`；复用 `fix/remove-sentence-discovery` 工作区。原即时保存、全局 unlink 和删除流程已经配套替换，不保留旧接口兼容分支。
- 总需求沿用 [requirements.md](requirements.md) 第 1、4、5、6、14 项以及第四批确认规则。不重做前三批，不扩展学习系统或通用发布引擎。
- 实际验证结果见本文末尾；本地验证不代表已上线。

## 已确认产品规则

1. 例句保存只改草稿；独立发布后，所有引用方跟随有效当前发布。
2. 新增标注随例句发布自动展示，不要求每个宿主重新发布；不引入显式收录白名单。
3. 局部移除记录稳定 `(entry_id, sense_id, sentence_id)` 隐藏项，保存只改宿主草稿，宿主发布后生效。共享标注不变，其他词义不受影响；例句后续发布不能自动撤销隐藏项。
4. 下架要求独立发布权、创建者或超管、原因和当前影响确认；保留引用和历史。
5. 恢复是独立命令，重新验证当前发布内容。草稿保存、发布新版本、历史回退均不能解除下架。
6. 普通管理员发布/回退本人创建例句，超管可跨创建者；混合批次逐项检查，不因宿主可编辑而获得例句发布权。
7. 发布/回退严格校验当前依赖或显式批内候选，不递归补选依赖；任一失败全部回滚。
8. 回退将历史内容发布为新版本，保留当前草稿及 revision；词条回退只恢复历史隐藏项，不回退共享例句正文或其全局状态。

## 当前实现与复用

| 分类 | 路径/能力 | 本批处理 |
| --- | --- | --- |
| 已有 | `shared_sentences/mod.rs` 的内容校验、稳定创建 ID、乐观 revision、repeatable-read 读取 | 保留并拆清草稿/发布读取；发布复用真实内容校验 |
| 已有 | `admin/publication_permission.rs::lock_publisher` | 所有发布/回退/下架/恢复事务使用同一授权行锁，权限先于幂等重放 |
| 已有 | `service/v3_publication.rs::publish_batch_v3`、`PublicationBatchContext` | 复用显式候选、预分配版本、分阶段插入、原子提交；不循环调用独立发布 API |
| 已有 | `service/text_links.rs` 与 `inbound_references.rs` | 复用具体节点、词形、方言/词面有效性及来源定位；加入例句发布来源 |
| 保留 | 词条现有音频引用保护 | 主会话复核 `WordSentenceWritableV3 → EnglishTextV3 → RichTextVariantV3` 没有 `audio_assets` 字段，只有语音配置；不凭调查摘要新建不存在的音频引用链，回归确认词条历史音频保护不退化 |
| 新工作 | 独立例句发布/历史、下架/恢复、影响确认、宿主隐藏项、混合请求/错误定位 | 本批完整实现并定向验证 |
| 已配套 | 前端严格响应 schema 和独立发布语义 | 同步真实 OpenAPI；编辑读 draft，展示读 published；混合批次显式选择并冻结双版本 |

旧 `shared_sentence_collections` / `shared_sentence_sense_collections` 不再用于生产收录；不要把它们当成现成 membership，也不顺手清理无关旧结构。词条中的内嵌例句和共享例句是不同链路，不复制共享正文进词条快照。

## 数据模型与迁移

成对新增 up/down，先在独立空库验证：

- `shared_sentences` 保留独立草稿内容和 revision；增加当前发布指针、lifecycle revision、下架时间/原因。`deleted_at` 仅用于从未发布草稿的删除，不能代替下架。
- 新增不可变 `shared_sentence_publications`：稳定版本 ID、句子 ID、发布序号、source revision、完整内容/标注快照、发布人/时间、可选 rollback 来源。正常 revision 重发可复用，回退始终创建新版本。
- 发布标注单独投影，绑定例句 publication；当前展示、当前入站保护与草稿标注不得混用，历史不被删除或覆盖。
- 宿主草稿隐藏项与每个词条 publication 的隐藏项使用专用关系表；发布将草稿隐藏项冻结到新版本，回退从选定历史版本复制。它们是宿主快照的一部分，不复制共享正文、不建立白名单。
- 当前共享例句 DTO 不持有录音资产；语音配置随草稿/发布快照保留。本批不新增真人录音能力或无写入方的引用表，现有词条音频侧表与清理器保持不变。
- 唯一/外键/正数检查约束保持在数据库；发布历史不随草稿删除、局部隐藏或下架而删除。

不为旧测试内容生成伪造初始发布或兼容分流，不自动清库。若迁移要求空共享例句数据而目标不满足，应明确拒绝并整体回滚；部署前另行确认具体环境、范围及重置授权。已有第四批发布历史时 down 必须拒绝，而非丢弃历史。

## API 与读取语义

沿用 `/api/v1/admin/lexicon` 前缀，精确 wire 由实现及导出的 OpenAPI 固定：

- `POST /sentences`、`PUT /sentences/{id}`：仅保存草稿；返回草稿 revision、lifecycle、当前 publication 和下架元数据。已发布草稿删除拒绝，不能通过原 DELETE 绕过下架权限。
- 例句列表/详情显式区分 `draft` 与 `published` 读取。发布读取仅有效当前发布，不读取草稿标注；宿主过滤应用对应宿主草稿或发布隐藏项。默认普通展示为 published，编辑端明确请求 draft。
- 新增例句单条发布、历史列表/详情、历史新版本回退端点；写命令带内容/生命周期版本与幂等键。
- 新增下架影响预览、下架与恢复端点。下架请求包含原因和影响指纹；执行时重新计算当前影响与 lifecycle，变化返回冲突，不能仅用 `confirmed: true`。
- 局部可见性写命令使用宿主 `base_revision`，支持明确移除/恢复，更新宿主 revision 并返回最新宿主状态；不增加例句 revision，不删标注。删除旧 unlink 的即时修改行为，不保留兼容分支。
- 混合发布请求明确区分词条和例句，合计 1–50 项，携带各自内容/生命周期版本；错误明确定位资源类型及 ID。现有词条批次由同一内核承接，不复制一套事务逻辑。

## 事务、引用与并发

- 统一：授权行锁 → 幂等锁 → 排序的 publication/context 锁 → 稳定顺序资源行锁 → 引用校验 → 写版本及节点 → 写跨资源引用 → 指针/审计/outbox/幂等结果 → commit。批外目标锁保留 NOWAIT 冲突语义，不能默默绕过版本冲突。
- 共享例句更新先锁定新旧目标集合，再验证草稿 revision；局部可见性先锁宿主，再检查例句存在性，避免反向等待。
- 混合批次准备所有候选并预分配 ID，发布标注引用批内词条候选；所有快照/节点存在后再写跨资源引用。
- 入站校验只替换显式选中的旧发布来源；不按 sentence ID 跳过所有引用，未选中的草稿与当前发布仍受各自保护。
- 已发布共享例句对词义的语义引用和宿主是否展示是两件事：局部隐藏不能解除语义引用；下架不删历史，恢复必须重新核实目标有效性。
- 下架影响确认覆盖当前标注所指向的词条/词义及其生命周期/可见性状态，并与会改变这些状态的写操作遵守相同锁协议。并发新增先提交则旧确认冲突；下架先提交则后续普通展示仍不可见。

## 前端配套与发布约束

这是破坏性行为/契约变更。旧前端保存后认为已生效、旧 unlink 使用例句 revision、旧删除没有原因/影响确认，且严格 validator 可能拒绝新增字段。

本轮配套同步 wire/OpenAPI/runtime schema、明确编辑/发布状态、独立发布/历史/下架恢复、宿主可见性与 mixed batch 操作，并验证新旧组合。配套未完成时不部署新后端给旧页面使用。

若新旧组合无法兼容，发布时先受控停用相关功能/流量，按原生迁移及双端部署门禁完成替换和真实验收后恢复；不能仅写“同批发布”。任何数据重置、交付、部署均需独立授权。

## 验收

仅使用第四批新建的本地隔离 PG/Redis，显式设置 `DATABASE_URL`、`REDIS_URL`、`TEST_REDIS_URL`；不复用第三批或共享 5433/6379。所有命令在第四批后端工作区执行。

| 风险 | 可观察判据 |
| --- | --- |
| 草稿/发布隔离 | 保存后 published 正文、标注和展示集合不变；例句发布后新增标注自动出现 |
| 局部移除 | 保存隐藏项只改宿主草稿；宿主发布只隐藏目标词义；句子 revision/其他宿主不变；例句重发不解除隐藏；词条回退恢复历史隐藏项且保留草稿 |
| 权限/幂等 | 未授权/非创建者拒绝；超管例外；重放无额外版本；撤权与发布同授权行锁串行化 |
| 全局状态 | 缺原因/旧影响确认拒绝；下架保留引用历史；保存/发布/回退仍下架；仅显式恢复重新校验并恢复 |
| 混合批次 | 环状词条与句子依赖成功；漏选/无权/过期/后段错误全回滚；反序并发不部分写入；错误定位具体资源 |
| 引用保护 | 局部隐藏不解除语义引用；未选中的草稿/发布来源不被批次豁免；批内旧发布来源仅由其新候选替换 |
| 音频 | 共享例句没有 audio_assets，不新增音频引用表；运行原有词条音频保护回归 |
| 迁移 | 空库 up/down/up；不满足前置条件和存在新历史时拒绝且无部分写入 |

最低验证入口（新增用例使用 `batch4_` 前缀，复用现有测试 target）：

```bash
cargo test --locked --all-features --test shared_sentences
cargo test --locked --all-features --test lexicon_handler batch3_
cargo test --locked --all-features --test lexicon_handler batch4_
cargo test --locked --all-features --test lexicon_v3_lifecycle
cargo test --locked --all-features --test lexicon_audio_asset_references
cargo test --locked --all-features --lib
cargo fmt --check
cargo clippy --locked --all-targets --all-features -- -D warnings
cargo sqlx prepare -- --all-targets --all-features
cargo sqlx prepare --check -- --all-targets --all-features
SQLX_OFFLINE=true cargo run --locked --all-features --bin export_openapi
git diff --check
```

测试必须核实真实退出码；未执行项与失败如实记录。没有真实浏览器联调时，不标记端到端验收完成。


## 本地验证记录（2026-09-23）

- 隔离 PostgreSQL `tsz-lexicon-b4-pg`（127.0.0.1:53643）与 Redis `tsz-lexicon-b4-redis`（127.0.0.1:53644）；未使用旧批次数据库。
- 后端库测试 293 项、词条 handler 118 项（含第四批 9 项）、生命周期 6 项、删除发现接口契约 2 项全部通过。
- 独立共享例句 24 项、原有词条音频引用 16 项全部通过。
- `cargo fmt --check`、`cargo clippy --locked --all-targets --all-features -- -D warnings`、`cargo sqlx prepare --check -- --all-targets --all-features` 通过。
- OpenAPI 已导出并由前端 `OPENAPI_SOURCE` 显式读取本工作区，同步严格 runtime schema；没有读取旧主目录契约。
- 前端全仓 typecheck、管理端和 api-client lint、管理端生产构建通过。全仓 `pnpm test`：193 个测试文件通过，2898 项通过、2 项原有跳过；包括混合响应身份、未知结果同键重试、下架原因/指纹、历史回退、已发布不可删除与完整区域移除回归。
- 整句发现的组件、请求封装、路由、handler、专用 DTO、自动匹配引擎及直接依赖已删除；保留 voice-editor 手动节点查询、Unicode 分词边界校验及成分组件共用样式。历史迁移不改写。
- 尚未执行真实浏览器连接本批 HTTP 服务的验收，尚未提交、推送、合并或部署。

### 发布门禁

1. 旧前端与新后端不能混用：原解除路由已移除，保存也不再直接发布。必须受控暂停相关词库操作后，按双端部署流程替换与验收。
2. 新迁移要求目标库没有旧共享例句数据；不满足时迁移拒绝。不能自动重置已有内容，需先明确目标环境与数据处置授权。
3. 已有本批发布历史后，迁移 down 拒绝，不以删除历史实现回退；必须遵循发布事务与部署迁移门禁。
