# Smart Lexicon V3 原生发布需求评估

## 背景与目标

Smart Lexicon V3 已支持原生词条的检测、创建、词形与发音编辑、释义与例句编辑、关系表落库和 schema 3 surface 投影。测试环境已经用真实管理员流程创建并完成原生 V3 草稿 `serendipity`，三个编辑步骤均完成，数据库 revision 为 6。

当前发布前核对页仍返回 `phase2_consumers_not_ready`。原因不是草稿内容或运行配置，而是服务端将 `v3_entry_state.origin = 'native'` 固定暴露为 `shadow_only`，发布事务又无条件要求已验证的 V2 迁移 canary。结果是“新数据只按 V3 录入”的正常流程无法形成 current publication，也无法完成端到端验收。

本功能的目标是允许满足完整性校验的原生 V3 草稿走现有 schema 3 发布与历史激活链路，并让管理后台依据明确的 `native` capability 开放发布操作。发布仍必须经过 revision、surface/visibility、引用完整性、幂等和事务原子性保护。

## 目标端

- 后端 `tsz-rust`：原生 V3 发布资格、schema 3 publication/activation 和能力契约。
- 前端 `tsz` admin：识别原生发布能力，开放发布与历史版本激活入口。
- web 学习端：本次不新增页面；只要求既有以 current publication 为依据的消费者继续正确读取 schema 3 数据。

## 用户故事 / 使用场景

1. 词库管理员创建一个全新的 V3 单词，完成基础信息、词形发音、释义例句后，可在核对页直接发布，不需要先创建或迁移 V2 数据。
2. 发布命中同形词、可见性或策略门禁时，管理员仍需按现有确认 token 流程确认，不能绕过安全门。
3. 同一个 revision 重复提交发布时，系统幂等返回既有 publication，不产生重复快照。
4. 管理员修改已发布 V3 词条并再次发布时，生成新的不可变 schema 3 publication；旧 publication 保留在历史中。
5. 管理员可在允许的条件下激活历史 publication；激活必须重新校验引用、surface、revision 和 lifecycle revision。
6. 管理端列表、统计、发布历史、related search、关系解析、例句关联和生命周期操作继续按当前 V3 read/projection 开关消费 current publication。

## 功能范围

### 本次范围内

- 为原生 V3 词条提供明确的可发布 capability，不再返回 `phase2_consumers_not_ready`。
- 发布资格按 `v3_entry_state.origin` 分流：
  - `native` 直接使用原生 V3 发布资格；
  - `migrated_v2` 继续执行现有 verified migration canary 白名单检查。
- 原生 V3 复用现有 schema 3 发布事务：完整性校验、pending relation 物化、引用校验、不可变快照、publication nodes/catalog refs/sense refs、current surface、current pointer、outbox、audit 和幂等响应。
- 历史 schema 3 publication 的激活对原生 V3 开放，并保留现有并发和引用保护。
- 更新 OpenAPI、前端 wire 类型、运行时 schema guard 和 admin 发布/激活判定。
- 增加后端 HTTP/DB 集成测试、消费者回归测试和前端单元/交互测试。
- 在测试环境用真实原生 V3 草稿完成发布、读取、历史和数据库验收。

### 明确不做

- 不恢复、回填或伪造 V2 词条。
- 不为原生 V3 写 `legacy_headwords`、V2 headword、legacy bridge 或其他兼容性数据。
- 不删除现有 V2/migrated V3 migration canary 代码；它继续服务已存在的迁移流程，但不参与原生 V3 发布。
- 不新增数据库表或改写历史 publication。
- 不改变 V3 草稿的数据模型、表单交互、内容完成规则或发音规范化算法。
- 不绕过 surface、visibility、引用完整性、幂等、revision 或 lifecycle revision 门禁。
- 不在本功能中扩展学习端新 UI、TTS 生成或异步 outbox worker。

## 约束与边界

- 原生 V3 状态必须满足 `origin = 'native'`；若同时带 migration batch、source V2 publication 或 canary 标记等互相矛盾状态，应 fail closed，不能按 native 放行。
- migrated V3 的资格规则保持不变：verified batch、verified entry、source publication 一致且 canary 已启用。
- `SMART_LEXICON_V3_PUBLISH=false` 或 `SMART_LEXICON_V3_PROJECTION=false` 时仍拒绝发布，不因 capability 变化而绕过运行开关。
- schema 3 publication snapshot 必须保持不可变，且 `snapshot.schema_version`、publication 列和 entry ID 一致。
- current publication surface 必须全部标记 `content_schema_version = 3`，绑定本次 publication 和 source revision；替换与 pointer 更新必须处于同一事务。
- 原生 V3 响应中的 `compatibility` 必须继续缺失，数据库不得生成 V2 headword 行。
- 生产或测试环境一旦存在 current V3 publication，回滚应用版本时不能单独关闭 V3 read/projection；完整回滚必须使用发布前数据库恢复点，避免现有线上内容变成不可读。

## 验收标准

- [ ] 完整原生 V3 草稿返回 `capabilities.publication.mode = "native"`，不再返回 `phase2_consumers_not_ready`。
- [ ] admin 核对页对 `native` capability 显示发布操作；未完成、归档或运行开关关闭时仍不能成功发布。
- [ ] 原生 V3 首次发布返回 201，entry 状态为 published，`published_revision` 等于草稿 revision，`current_publication_id` 指向 schema 3 publication。
- [ ] publication snapshot、snapshot hash、publication nodes、catalog refs、sense refs、current surface、outbox 和 audit 在同一事务中完整写入；失败时全部回滚。
- [ ] 重放同一 idempotency key 与同一请求返回同一结果，不新增 publication；同 key 不同请求返回幂等冲突。
- [ ] 同 revision 再发布复用既有 publication；编辑后新 revision 发布生成下一 publication number。
- [ ] 原生 V3 历史 publication 可按现有规则激活，lifecycle revision 单调递增，current surface 与 current pointer 同步切换。
- [ ] 列表、统计、publication history、related search、关系/例句关联、archive/restore 对 current V3 publication 的既有测试继续通过。
- [ ] `SMART_LEXICON_V3_PUBLISH=false`、projection=false、非法 origin 状态、引用失效、surface token 过期、revision 冲突均 fail closed 且不产生写入。
- [ ] 原生 V3 发布前后 `lexicon.entries` 中 schema 2 词条数不增加，`entry_headwords` 不生成原生 V3 行，响应无 compatibility bridge。
- [ ] 后端 OpenAPI 与前端生成 snapshot/runtime schema 一致，严格 discriminator guard 能接受 `native`，仍拒绝未知 capability mode。
- [ ] 测试环境真实管理员流程完成 `serendipity` 发布，并用 UI、API、DB 三条证据确认 current schema 3 publication 可消费。

## 开放问题

无。用户已明确选择“清空旧数据、只按最新 V3、不要兼容性代码”；本次据此采用原生 V3 直接发布，不以 V2 migration canary 作为前置条件。
