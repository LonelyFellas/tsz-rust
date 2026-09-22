# 第三批：发布权限、原子批次与新版本回退

状态：2026-09-22 两仓本地实现与约定验收已完成；未提交、推送或部署。授权范围为方案、两仓本地实施和验证，不含提交、推送或部署。
需求沿用 [requirements.md](requirements.md) 第 3、4、5 项及 [design.md](design.md) 第三批范围。

## 基线和依赖

- 后端 `465d639871d7f0a539d761568df1a908104d9b3b`，前端 `1255d7123eed00ef6b5bf14267a6c50724657d10`；两仓均从 fetch 后的 origin/main 建立 `codex/lexicon-domain-batch-3`。
- 工作区分别为 `tsz-core/.worktrees/backend/lexicon-domain-batch-3`、`tsz-core/.worktrees/frontend/lexicon-domain-batch-3`。
- 第二批的引用校验、草稿候选与分页已合入；测试服务器两端制品与部署冒烟通过。不将部署冒烟扩大为全业务验收。

| 分类     | 事实与处理                                                                                                             |
| -------- | ---------------------------------------------------------------------------------------------------------------------- |
| 已在主线 | 第二批引用影响分析、具体节点发布状态、草稿保存边界、稳定分页；不重做                                                   |
| 可复用   | `v3_publication.rs` 的事务、幂等、同名确认、发布投影、审计；`inbound_references.rs` 的业务判定；现有历史页面和跨页选择 |
| 小改动   | 单条发布去掉编辑归属与发布权限的耦合，前端按钮分别判断；历史操作文案和列表刷新                                         |
| 新工作   | 账号级发布授权、批内候选解析与原子提交、新版本回退及迁移、批次 UI 与契约                                               |
| 已确认   | 已确认独立开关、普通管理员默认关闭；普通发布者不得发布他人的草稿                                                       |

关键路径：权限产品决策 → 发布内容校验和目标解析复用 → 批次/回退事务 → OpenAPI 和前端 → 隔离数据库与 UI 验证。
启动时估计实施与验证 4–6 小时；数据库测试、SQLx 刷新与复杂引用改造是主要不确定项，超出估计 50% 时重新评估。

## 范围与权限决策

仅处理词条的发布。共享例句独立草稿/发布、全局下架及混合资源批次归第四批，不提前建立通用工作流。
保留现有编辑归属：未发布草稿仅创建者/超管可编辑，已发布词条的既有编辑行为不变。不恢复已取消的 RBAC。

2026-09-22 用户已确认：

1. 增加 `can_publish_lexicon`，超管管理，现有及新增普通管理员默认关闭，超管始终允许。
2. 普通发布者暂时不得发布他人的草稿；仍不获得额外编辑权。超管保留跨创建者管理能力。跨创建者循环依赖须由超管统一发布。

独立授权实现：在 `admins` 增加布尔字段；复用 `admin/authorization.rs` 的实时回库验证；profile/管理员列表下发有效权限，管理员管理页由超管授予/撤回，变更记审计。不要把菜单 `permissions` 当业务发布权限或把权限固化进 JWT。
单条发布、批次发布、回退均检查发布权。归档/恢复涉及已有发布的词条时也检查发布权；混合批次任何项无权，整批不写入。从未发布草稿的删除、归档/恢复保留既有编辑归属。
权限撤回与在途命令的线性化点必须明确：发布事务读取并锁定授权记录，权限修改使用同一行锁；先取得锁的命令完成后，后续请求不得再沿用旧授权。

## 发布内核与原子批次

改动集中于 `lexicon/service/v3_publication.rs`、`publishing.rs`、`v3.rs` 的成分解析、`text_links.rs`、`inbound_references.rs`。只提取真实被单条/批次/回退复用的逻辑，不引入通用引擎。

新增端点 `POST /api/v1/admin/lexicon/entries/publications/batch`：

- UUID `Idempotency-Key`；输入 `schema_version: 3`、`items`；每项为 `entry_id`、`base_revision`、`base_lifecycle_revision`、可选 `confirmed_surface_match_token`。
- 显式 1–50 项，拒绝重复和 nil ID，revision 必须正数；不自动增加引用目标，不拆成多个独立事务。
- 成功 201，返回 `words: AdminWordV3[]`；返回集合必须恰好等于请求集合。顺序按请求顺序，锁按 ID 排序。
- 权限失败 403；版本/幂等/同名冲突沿用 409；内容无效 422。批次错误的 `meta.word_id` 携带失败词条 ID 和现有节点问题，便于定位。没有部分成功响应。

执行过程：

1. 验证参数、权限与功能开关，计算有序请求的幂等哈希；事务内获取幂等锁并处理重放。
2. publication advisory lock 按词条 ID 排序；读取所选草稿后，一次锁定全部 surface context、policy 和 surface key，完成同名确认，再锁 entry 行并复核内容/生命周期 revision。引用目标继续使用现有共享锁协议。不能用锁前读取的草稿冒充锁后版本；发生冲突整体回滚，不自动换 revision 重试。
3. 在内存为每条候选预分配 publication ID；批内目标解析使用该条待发布内容和新 publication ID，批外只接受当前已发布且有效的具体节点。
4. 同时验证全部候选的结构、目录、同名确认、音频及出站引用。批内来源的入站影响按候选内容判断，批外来源仍按当前发布；共享例句维持现有独立规则。不能只跳过批内入站检查。
5. 循环关系可以通过，但不会放宽成分嵌套、自引用等已有业务限制。批内有草稿改动时，不能偷偷回落到目标的旧发布内容。
6. 校验通过后，先写全部发布快照和节点，再写跨发布引用，避免环的外键写入顺序问题；随后更新当前指针、surface 投影、音频保护、审计、outbox 和一份幂等结果，最后一次 commit。
7. 任何失败回滚全部状态；外部调用和确认 token 消费在 commit 后处理。批次单次失败不得提前消费某一项 token。

不采用“循环调用现有 publish_v3”：它逐条提交且只看当前发布，无法保证循环依赖与失败原子性。
也不以临时切换 current_publication_id 绕过校验；当前发布仅在最终提交阶段更新。
完整候选上下文贯通关系词、成分、正文、via-phrase 和 context。显式绑定历史 publication_id 的引用不自动改绑，仍须验证具体目标在批次结束后的当前内容中有效；未绑定 publication_id 的批内引用解析到本批计划版本。

## 新版本式回退

以 `POST /api/v1/admin/lexicon/entries/{id}/publications/{publication_id}/rollback` 替换旧 `/activate`；不保留两套语义。输入保留 schema/revision/lifecycle revision/同名确认，幂等命令使用新 scope。201 返回现有词条 envelope。

- 从指定历史快照取得内容，按当前目录、结构、音频和具体引用规则重新校验；不能直接信任旧发布时的校验结果。
- 新建 publication ID、递增 publication_number，记录 `rollback_of_publication_id`，原发布记录不变。回退到当前版本也产生新发布；相同幂等键重试不得多建。
- `source_revision` 保留历史内容来源，不能伪造成当前草稿 revision。当前草稿 forms/meanings、revision、step state、节点墓碑、`draft_based_on_publication_id` 均不修改；只更新发布生命周期相关状态。
- 现有 `update_current_publication_pointer` 同时更新 draft_based_on，需要拆开真实用途；不能原样用于回退。
- 当前 `insert_v3_publication_nodes` / catalog refs 从草稿表取节点，不能用于历史回退。回退从选定历史版本的节点/目录引用复制并结合当前校验，重新生成出站引用及保护音频；不得把当前草稿新增节点混入历史发布。
- 发布事件的唯一性目前使用 source_revision。同一历史 revision 可多次重新发布，须改为发布序号或独立生命周期序号并核对消费者，避免 outbox 唯一约束碰撞。

迁移新增同词条复合外键 `rollback_of_publication_id`。移除发布表按 source_revision 的全局唯一约束，正常草稿发布用 `rollback_of_publication_id IS NULL` 的部分唯一索引保留重复发布保护；按 revision 查找也必须过滤回退记录。保留 publication_number 唯一性。
新快照包含新的发布时间和递增 lifecycle_revision，因此保留 snapshot_hash 唯一约束；不修改业务内容、不加盐。重复命令由幂等记录返回同一结果。
down 在存在新回退记录时必须拒绝，不能删历史或伪称可无损回退；空新记录时撤销新增结构、恢复原唯一约束。up/down 在隔离库验证。授权字段迁移普通管理员默认 false。

## 前端与契约

- `SmartDictionary` 复用跨页 selectedKeys/selectedRecords，新增“发布所选”确认框，列明选择范围和整批原子性。选择时版本过期要明确提示重新确认，不能自动更新 revision 后重发。
- `V3PublicationHistory` 替换激活命令为历史内容发布；复用同名确认、单请求锁、幂等重试和迟到响应隔离。
- 保留现有未保存修改门禁：先保存或明确放弃本地修改才允许回退。服务器已保存草稿完整保留；不得为了刷新发布信息重置用户未保存的输入。
- `WordWizardV3` 保留编辑归属限制，发布同时要求独立授权和创建者归属（超管除外）；不得用已发布词条可编辑推导其草稿可发布。
- API 请求层校验批次响应 ID 集合；结果未知时使用同一幂等键和原请求重试，改范围/版本/确认内容则新建键。
- 更新 `packages/types`、`api-client/src/admin.ts`、runtime schema 根类型、相关 hooks 与 profile/admin 管理消费者；由原生生成器同步 OpenAPI，不手改生成产物。

## 发布顺序和回退限制

本轮不执行部署。旧前端仍调用 activate，新的权限字段也可能被严格 schema 拒绝；新前端不能假定旧后端已有批次、回退和授权 API。
已按两个基线的实际 OpenAPI 与调用路径核对：旧前端请求 /activate，而新后端只有 /rollback；新前端的 /rollback、/publications/batch 和授权端点不存在于旧后端。新回退来源字段也不在旧严格历史响应 schema 内。未运行旧服务混合版本联调，不能将局部读兼容称为全流程兼容；交付须受控停用词库相关操作、配套切换、刷新旧页面并验收后恢复。
出现新回退记录后不能仅换回旧二进制；涉及唯一约束和新历史语义，数据库 down 的拒绝条件必须写入发布说明。

## 验收

使用本任务独立 Postgres/Redis，显式设置 `DATABASE_URL` 与 `TEST_REDIS_URL`，不得使用默认 5433/6379 或第二批容器。集成测试用 sqlx 隔离数据库。执行命令如下，实际结果见 progress.md 第三批章节：

```bash
SQLX_OFFLINE=true cargo test --locked --lib lexicon::
SQLX_OFFLINE=true cargo test --locked --test lexicon_handler
SQLX_OFFLINE=true cargo test --locked --test lexicon_v3_lifecycle
SQLX_OFFLINE=true cargo test --locked --test admin_profile_handler
SQLX_OFFLINE=true cargo clippy --locked --all-targets --all-features -- -D warnings
cargo fmt --all --check
cargo sqlx prepare -- --all-targets --all-features
cargo sqlx prepare --check -- --all-targets --all-features
SQLX_OFFLINE=true cargo run --locked --all-features --bin export_openapi
```

前端在本任务工作区显式 `OPENAPI_SOURCE` 指向本任务后端 docs/openapi.json，运行原生 sync:openapi；定向契约和组件测试后，收尾一次 pnpm test / typecheck / lint。新增 Rust test target 时登记原生 CI 分区。

关键断言：

- 无发布权可按现有规则保存，发布/回退/已发布归档恢复被拒绝且无数据库副作用；撤权对后续请求即时有效；编辑权与发布权不互相授予。
- 两条与多条循环依赖原子成功；批外未发布依赖、批内缺失节点、revision/lifecycle 冲突、晚阶段插入失败均整体回滚；历史、指针、surface、引用、审计、outbox 与幂等记录一致。
- 幂等重放不新增版本；同键不同请求 409；并发反向选择顺序不会形成不可恢复死锁，不丢版本冲突。
- 回退生成新版本和来源记录，原历史不变，草稿全文、revision、墓碑与 draft_based_on 不变；当前无效目标/目录/音频不能通过回退复活。
- 浏览器证明跨页选择、发布者只读预览、循环依赖整批发布、历史新版本回退及未保存输入保护；真实后端无 page.route/mock，不扩大为学习端或第四批验收。

每个失败保留原始证据；旧行为断言与新需求冲突时明确替换，不能加兼容分支或弱化断言让两者同时通过。
