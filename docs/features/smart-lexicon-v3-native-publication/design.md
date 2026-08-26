# Smart Lexicon V3 原生发布技术设计

## 方案概述

现有 `service/v3_publication.rs` 已经实现完整的 schema 3 发布和历史激活事务，且当前测试已覆盖 migrated V3 canary 的端到端发布、V2/V3 历史切换，以及 V3 current publication 的搜索、关系、例句关联和生命周期消费者。本次不复制或重写发布流水线，而是在发布事务的两次资格检查中按 entry origin 分流：原生 V3 校验自身状态不变量后直接进入既有事务，迁移 V3 继续执行原 canary 校验。

契约层新增 `V3PublicationCapability::Native`，wire 为 `{ "mode": "native" }`。admin 将 `native` 视为可发布/可激活，将 `shadow_only` 和未白名单 canary 继续视为阻塞。这样 capability 语义与真实发布资格一致，也避免把原生数据伪装成 migrated canary。

不选用以下方案：

- 不把 native 状态改写成 `migrated_v2` 或手工开启 canary：会制造不存在的 V2 provenance，并违反用户“不写兼容数据”的约束。
- 不删除 `preflight_migration_canary` 后无条件放行所有 V3：会破坏 migrated V3 的 verified batch/canary 安全门。
- 不新增第二套 native publication 实现：现有 schema 3 事务已经覆盖 snapshot、refs、surface、outbox 和 audit，复制会产生行为漂移。

## 代码影响范围

### 后端 `tsz-rust`

- `src/lexicon/dto/v3.rs`
  - `V3PublicationCapability` 新增无附加字段的 `Native` variant。
  - 保留 `ShadowOnly`、`MigrationCanary` 和现有 block code，用于未启用阶段和迁移流程。
- `src/lexicon/service/v3.rs`
  - 原生 entry 的 capability 从 `ShadowOnly { phase2_consumers_not_ready }` 改为 `Native`。
  - 新建原生 entry 的初始响应同步返回 `Native`。
  - migrated entry 的 capability 逻辑保持不变。
- `src/lexicon/service/v3_publication.rs`
  - 将 `preflight_migration_canary` / `ensure_migration_canary` 收敛为 origin-aware publication eligibility 检查。
  - 事务开始后、entry row lock 前的预检继续在 migration advisory lock 下执行；entry row lock 后再次读取 `v3_entry_state FOR UPDATE` 并复核，保留 TOCTOU 防护。
  - `native` 仅在 migration provenance 字段为空且 canary=false 时放行；矛盾状态返回 invariant/fail-closed 错误。
  - `migrated_v2` 完整复用 verified batch、verified entry、source publication 一致和 canary=true 的检查。
  - `publish_v3` 与 `activate_publication_v3` 继续共享同一资格函数，不改后续事务顺序。
- `src/lexicon/handler/commands.rs`、`src/lexicon/handler.rs`、`src/error.rs`
  - 运行 feature flag 的 gate 保持现状；迁移 canary 错误只用于 migrated entry 不满足资格的情形。
  - 若当前 handler 在 publish flag 关闭时复用 canary 错误导致语义不准确，则在实现时增加独立、稳定的 publication-disabled 错误；不改变成功请求结构。
- `src/openapi.rs`、`docs/openapi.json`
  - 注册 `native` discriminator 分支并更新 strict contract 断言。
  - `docs/openapi.json` 通过项目生成流程更新，不手工编辑。
- `tests/lexicon_handler.rs`
  - 将“native publish 被固定阻断”测试改为完整原生 V3 create → complete forms → complete meanings → publish。
  - 断言 transaction、idempotency、publication reuse、新 revision、history activation、surface、outbox、audit、无 V2 bridge。
  - 保留 migrated canary 成功/失败和 feature-flag fail-closed 回归。
- `tests/lexicon_v3_relation_consumers.rs`
  - 增加由真实 native publish 产出的 current publication consumer 场景，避免仅靠手工 seed schema 3 snapshot 证明消费者兼容。
- `tests/lexicon_v3_lifecycle.rs`
  - 使用真实 native current publication 补一条 archive/restore 或复用现有 fixture 证明 lifecycle 不依赖 migration origin。

预计不需要数据库 migration：`entry_publications.content_schema_version = 3`、V3 snapshot、publication refs、surface 和 outbox 结构均已存在。

### 前端 `tsz`

- `packages/types/src/admin-word-v3.ts`
  - `V3PublicationCapability` 新增 `{ mode: "native" }`。
- `packages/api-client/src/openapi.snapshot.json`
  - 从后端权威 `docs/openapi.json` 同步生成，不手改。
- `packages/api-client/src/admin-word-v3.runtime-schema.json` 及对应生成/校验产物
  - 用仓库原生命令生成 `native` strict union 分支。
- `apps/admin/src/features/dictionary/word-creation-v3/V3PreviewAndPublishStep.tsx`
  - `publicationBlockCode()` 对 `native` 返回 undefined；shadow 和未白名单 canary 继续阻塞。
- `apps/admin/src/features/dictionary/word-creation-v3/V3PublicationHistory.tsx`
  - 历史激活资格允许 `native` 或已白名单 migration canary。
- 相应 `*.test.tsx`、api-client schema tests
  - 覆盖 native 按钮可见、发布成功、冲突确认、历史激活和未知 mode fail closed。

## 数据与契约

### Capability wire

```json
{
  "publication": {
    "mode": "native"
  },
  "pronunciation_normalization_version": "nfkc_trim_lower_v1"
}
```

原有 migrated V3 wire 保持不变：

```json
{
  "publication": {
    "mode": "migration_canary",
    "whitelisted": true
  }
}
```

### 原生资格不变量

`v3_entry_state` 满足以下条件才按 native 放行：

- `origin = 'native'`
- `migration_batch_id IS NULL`
- `source_publication_id IS NULL`
- `publication_canary_enabled = FALSE`

任何混合状态都视为数据不变量损坏并 fail closed，不能自动修复或降级为 migration canary。

## 数据流 / 时序

```text
admin 核对页
  -> GET entry：capability.mode = native
  -> POST publications(schema_version=3, base_revision, optional surface token)
     -> handler 检查 publish/projection 运行开关
     -> 锁 idempotency scope
     -> 锁 entry 对应 migration/native advisory key
     -> origin-aware eligibility 预检
     -> surface/visibility 确认
     -> 锁 entry row + v3_entry_state row，再次资格复核
     -> forms/meanings/aggregate 完整性校验
     -> relation 物化与跨词条引用校验
     -> 刷新例句关联投影
     -> 写 schema 3 immutable publication + nodes/catalog refs/sense refs
     -> 替换 current publication surface
     -> 更新 current_publication_id/lifecycle_revision
     -> 写 outbox、audit、idempotency response
     -> 单事务 commit
  <- AdminWordV3Envelope(status=published)
```

历史激活复用同样的运行开关、资格双检、surface token、revision/lifecycle revision 和引用校验，只把 current pointer 与 current surface 切到选定的 schema 2/3 publication；原生 V3 通常只有 schema 3 历史，但实现不假设这一点。

## 复用与约定

- 复用现有 `insert_v3_publication`、`insert_v3_publication_nodes`、catalog/sense refs、surface replacement、outbox 和 audit，不新建并行实现。
- OpenAPI 是前端 wire 类型和 runtime guard 的权威来源；生成文件只通过仓库命令更新。
- 所有 JSON wire 字段保持 snake_case；`mode = "native"` 是严格 literal discriminator。
- 原生 V3 不生成或读取 `legacy_headwords`；`legacy_bridge_read=false` 的环境继续返回无 compatibility 字段。
- 不改变数据库事务的锁顺序，避免引入 migration/publish、relation publish 或 lifecycle 死锁。

## 测试策略（概览）

### 后端单元/契约

- capability serialize/deserialize/OpenAPI discriminator：native、shadow、migration canary、未知 mode。
- eligibility matrix：合法 native、native 混合 provenance、合法 migrated canary、未白名单、batch/entry 未 verified、source publication 不一致、未知 origin。
- publication flag/projection flag 分别关闭时 fail closed。

### 后端 HTTP + PostgreSQL 集成

- 原生 V3 完整草稿首次发布 201，所有 publication 结构化行和 current surface 同事务落库。
- surface 冲突确认 token、过期 token、policy epoch/revision 漂移。
- 同 key 同 body 重放、同 key 异 body 冲突、同 revision publication reuse、编辑后新 publication。
- 历史 schema 3 激活 A → B → A，lifecycle revision、surface event 和 current pointer 一致。
- publication snapshot hash 不变；未知 snapshot schema fail closed。
- native 发布后的 related search、relation target、sentence association、list/stats/history、archive/restore。
- native entry 不生成 schema 2 entry/headword/compatibility 数据。

### 前端

- `native` 显示发布按钮且能走现有 surface confirmation/retry 流程。
- shadow、未白名单 canary、archived 仍禁用。
- native current word 允许激活非 current 历史 publication。
- runtime guard 接受 native、拒绝未知 capability mode。
- API 409/410/422/503 的既有错误展示不回归。

### 测试环境手测

1. 部署精确 green main 后打开现有 `serendipity` revision 6。
2. 确认三个步骤完成，核对页 capability 为 native，发布按钮可用。
3. 完成可能出现的 surface confirmation，发布成功。
4. 从列表重新进入并检查 published 状态、发布历史和不可变预览。
5. 用 API/DB 核对 schema 3 publication、current pointer、surface、outbox、audit、V2 count=0。
6. 修改草稿后发布第二版，再激活第一版，验证历史回滚和 current surface。

## 风险与回滚

### 风险

- 最大风险不是 publication 写入本身，而是某个消费者只在手工 seed/migrated canary 场景中被测过。必须增加“真实 native publish 产出 → consumer 读取”的联通测试。
- capability 是前后端严格 union；只部署后端会导致旧前端 runtime guard 拒绝 `native`，只部署前端则后端仍阻塞。测试环境必须后端先就绪、随后同步部署前端，并在切换窗口完成烟测。
- 当前 publish flag 关闭时复用 migration-canary 错误，若新增独立错误码会扩大 OpenAPI/前端错误枚举改动；实现时优先保持最小范围，只有测试证明语义会误导操作才新增。
- 已形成 current V3 publication 后，单独回滚到不支持 V3 read 的旧后端会让线上内容不可读。

### 回滚

- 发布前：保留当前后端/前端 SHA 和 PostgreSQL 18 一致性备份；验证恢复演练。
- 发布后仅发现“新发布写入异常”时：先关闭 `SMART_LEXICON_V3_PUBLISH` 阻止新 publication，保持 READ/PROJECTION 开启，保留已发布内容可读并调查。
- 若代码回滚版本仍支持 V3 read/consumer，可回滚应用但不得清理 publication 数据。
- 若必须回滚到不支持 current V3 publication 的版本，只能在维护窗口恢复发布前数据库备份；这会丢弃备份后的测试数据，需明确记录。
- 所有发布写入本身为单事务，失败不会留下半套 snapshot/pointer/surface/outbox。
