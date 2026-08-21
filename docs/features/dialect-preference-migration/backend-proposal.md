# 英美方言偏好化改造（A1）：后端提案

> **来源**：前端 `tsz` 的 `docs/features/dialect-preference-migration/{requirements,design}.md`
> （2026-08-19 评审通过）中的提案 P1 / P2 / P3。本文把它们转成后端可直接排期的形式，
> 补上本仓源码核对、存储与命名建议、测试与回滚。产品口径以前端需求文档为准。
>
> **核对基线**：`tsz-rust` `main` @ `c4239bb`、`tsz` `main` @ `132d723`（2026-08-20）。
> 下文每条「现状」都直接读过源码，不是推断。
>
> **一句话**：三项**全是新增能力**，不改任何既有 wire 字段的语义，不动查重键、
> 不动 `WordHeadwordsV2`、不动 `lexicon_entries_headword_shape_check`。
>
> **实施状态（2026-08-20）**：**P1、P2、P3 已实现**，全量 `cargo test` 绿；
> 前端对接说明见 `docs/frontend-integration.md` §10 / §11 / §12。
> **P1-b 已于 2026-08-20 实现**：前端阶段 3 上测试服后解除押后。`content_completion` 的
> 语法结构现在恒产单份 `common`，不再看词条是不是 distinguish。契约无变化，
> `docs/openapi.json` 无需重导。
>
> **实现期新增的一个取舍**：收敛成单份后，模型只给 `uk` / `us` 两侧而 `common` 为空时
> 必须二选一。取 `common → uk → us`——被迫选边时偏英式，与 `admins.dialect_preference`
> 的默认值一致（迁移注释：「默认英式，存量账号一并按英式解释」）。改这个顺序等于改平台
> 默认口径，别当成无关紧要的重构。

## 背景

前端把「英美」从**每条词条都要做的决策**降级为**管理员账号上设置一次的偏好**（默认英式）。
方言被拆成三层：**L1 词条事实**（`centre` / `center` 是同一个词的两种地区拼写）**保持不变**；
**L2 管理员偏好**是新增的账号级设置；**L3 平台自己写的英文行文**（英文释义/例句/语法结构）
收敛为单份、口径取 L2。后端只在 L2 的持久化（P2）与 L3 的一处硬校验（P1）上被阻塞，
其余收敛前端可单方面完成——释义与例句**已核实无需后端配合**。

## 排期总表

| 项 | 内容 | 阻塞什么 | 契约影响 | 数据迁移 | 估时 | 状态 |
| --- | --- | --- | --- | --- | --- | --- |
| **P1** | 放宽语法结构的方言形状校验 | 前端阶段 6（删镜像 shim） | 纯放宽，schema 不变 | 无 | 0.5 人日 | **已实现** |
| **P1-b** | AI 补全对 distinguish 词条改产单份 | — | 无 | 无 | 0.5 人日 | **已实现** |
| **P2** | 管理员方言偏好持久化 | 前端阶段 7（偏好事实源上服务端） | profile 响应加字段 + 1 个新端点 | 1 条加列迁移 | 1 人日 | **已实现** |
| **P3** | 列表/搜索行的每侧拼写结构化 | 「列表按偏好侧排序」这一条体验 | 新增字段 | 无 | 0.5 人日 | **已实现** |

三项互不依赖。**三项都不做也不会卡住前端阶段 1–5**，前端会先用「两条同值镜像 + localStorage」过渡。

## P1 · 放宽语法结构的方言形状校验（已实现）

**落地结果**：`src/lexicon/validation/meanings.rs`（精确相等 → 属于允许集合之一）。
测试：`tests/lexicon_handler.rs::grammar_structures_accept_a_single_common_variant_on_distinguish_entries`
（单条 common 200 / 只写 uk 422 / common+uk 422 / unified 写双条 422）。
**实测补充**：收敛写法必须给 common 变体换新节点 ID（节点角色里带方言，
复用旧 uk 变体 ID 会被判 `node_binding_changed`）；反向改回双条则必须沿用原节点 ID。

**现状（改动前）** `src/lexicon/validation/meanings.rs:65`：

```rust
let expected_dialects = if matches!(headwords, WordHeadwordsV2::Unified { .. }) {
    vec![Dialect::Common]
} else {
    vec![Dialect::Uk, Dialect::Us]
};
```

`grammar_structures[].variants` 的方言集合必须与之**精确相等**（`meanings.rs:120-137`），
否则 `grammar_variants_invalid`。

**问题**：A1 之后语法结构只维护一份，`distinguish` 词条却被迫写「两条同值镜像」。
wire 里出现冗余数据，学习端将来读到会显示成「英式：a centre／美式：a centre」这种没有信息量的两行。

**提案**：`distinguish` 词条**同时接受** `[common]` 与 `[uk, us]`；`unified` 维持只接受 `[common]`。
即把「精确相等」放宽为「属于允许集合之一」。

**连带影响（已核对）**：

- 持久化无需改动。`text_variant_role()` 按 dialect 生成节点 role，`common` 是合法取值；
  `lexicon.entry_editor_projection` 原样回读，round-trip 安全。
- **AI 内容补全（P1-b）已跟进**（2026-08-20，前端阶段 3 上测试服后）：
  `src/lexicon/content_completion/worker.rs` 的 `map_generated` 不再按 distinguish 分支，
  语法结构恒产单份 `common`。押后的原因是现网前端 `meaningsAndExamples/validation.ts` 曾对
  `distinguish` 词条硬性要求 `[uk, us]`，后端先产单份会让补全结果在第 3 步显示为「未填写」、
  readiness 判 incomplete——阶段 3 发布后该约束解除。
- 发布快照按行读取，不假设两侧齐全。

**兼容性**：纯放宽。存量数据、旧前端、OpenAPI schema 全不受影响，**无需重新导出 `docs/openapi.json`**。

**验收**（`tests/lexicon_handler.rs` 补 4 条）：`distinguish` 提交单条 `common` → 200 且回读为单条；
提交 `[uk, us]` 仍 200；提交 `[uk]` 或 `[common, uk]` 仍报 `grammar_variants_invalid`；
`unified` 提交 `[uk, us]` 仍报错。

## P2 · 管理员方言偏好持久化（已实现）

**落地结果**：迁移 `migrations/20260820120000_add_admin_dialect_preference.{up,down}.sql`、
`AdminDialectPreference`（`src/admin/model.rs`）、`AdminRepository::set_dialect_preference`、
`GET /profile` 加 `preferences`、新端点 `PATCH /api/v1/admin/profile/preferences`
（`src/admin/profile/handler.rs`、`src/admin/router.rs`、`src/openapi.rs`）。
**两处建议均按本文落地**：路径用 `profile/preferences`、存储用 `dialect_preference TEXT + CHECK`。
测试：`tests/admin_preferences_handler.rs`（5 条）、`tests/admin_schema.rs`（默认值 + CHECK）、
`tests/admin_profile_handler.rs`（`preferences` 恒在、响应恰 6 个字段）。
`docs/openapi.json` 已重新导出。

**现状（改动前）**：`AdminProfileResponse`（`src/admin/profile/handler.rs:31`）只有
`id / phone / display_name / role / permissions`，`admins` 表没有偏好列。
偏好现在落在按管理员隔离的 `localStorage`（`tsz` `packages/shared/src/dialect-preference.ts`），
换浏览器、换设备即丢。

**wire（与前端提案完全一致）**：

```jsonc
// GET /api/v1/admin/profile 响应新增字段，恒在；从未设置过即返回默认值
{ "id": "…", "phone": "…", "display_name": "…", "role": "admin", "permissions": ["…"],
  "preferences": { "dialect": "uk" } }        // 枚举 "uk" | "us"

// 新增 PATCH /api/v1/admin/profile/preferences
// 请求 { "dialect": "us" }   → 200 { "preferences": { "dialect": "us" } }
// 枚举外取值 → 422 application/problem+json（沿用 RFC 9457 既有约定）
```

**两处与前端提案不同的后端建议**（均不改 wire 形状，只需前端点头）：

1. **路径用 `PATCH /api/v1/admin/profile/preferences`**，而非前端提的 `/admin/settings/preferences`。
   现有 `/admin/settings/*` 挂的是**全局目录配置**（词性配置，首期仅 `super_admin` 可写），
   而偏好是「我自己的」，语义上属于 `profile`。前端 `packages/api-client` 尚未写这个调用（已核对），改路径零成本。
2. **存储用一列 `admins.dialect_preference TEXT NOT NULL DEFAULT 'uk' CHECK (dialect_preference IN ('uk','us'))`**，
   而非 `preferences jsonb`。`admins` 现有的 `role` / `status` 都是 TEXT + CHECK；
   两态开关用 jsonb 反而把校验从数据库挪进应用层。wire 仍按 `preferences.dialect` 嵌套返回，
   将来加第二个偏好再加一列即可，前端契约不受影响。

**权限**：任何已登录 admin 读写**自己的**；不提供改他人偏好的能力（不进 `admin/accounts`）。
守卫沿用 profile 现有两条（`disabled` → 403、`must_change_password` → 403）。
**默认值只由后端持有**，前端不再保留第二处默认——这是本提案的重点，两边各存一份默认必然漂移，
表现为「我明明没改过它怎么变了」。

**改动面**：加列迁移（up/down 各 1）→ `AdminRepository` 读写 → `AdminProfileResponse` 加字段 →
新 handler + `src/admin/router.rs` 挂载 → utoipa 注解 → `cargo run --bin export_openapi` →
`docs/frontend-integration.md` 新增一节。

**测试**：`tests/admin_schema.rs`（默认 `uk`、CHECK 拒绝第三态）、
`tests/admin_profile_handler.rs`（响应恒带 `preferences`）、
新增 `tests/admin_preferences_handler.rs`（写入生效并被 profile 读到 / 枚举外 422 / 未登录 401 / disabled 403）。

**回滚**：删列即可，前端回落 localStorage 分支；代价仅是偏好回到默认英式，无数据损失。

## P3 · 列表与搜索行的每侧拼写结构化（已实现）

**落地结果**：`AdminWordListItem` 与 `RelatedWordResult` 新增 `headword_variants`
（`src/lexicon/dto/operations.rs` 的 `HeadwordVariant`），与 `dialects` 同序。
列表行的每侧拼写改由 `array_agg` 取回（`src/lexicon/repository/query.rs`），
展示用的 `headword` 由 service 按序拼接——**顺便消掉了同一查询里第二份排序规则**，
`headword` ≡ `headword_variants` 按序拼接由构造保证。关联词搜索侧直接复用
`ordered_headword_sides`。测试：`tests/lexicon_handler.rs` 的
`list_rows_order_headword_spellings_by_source_dialect` 与
`related_search_orders_headword_spellings_like_the_list`（含两侧一致性断言）。

**现状（改动前）**：`docs/frontend-integration.md` §8 / §9 已把 `AdminWordListItem` 与 `RelatedWordResult` 的
`headword` 与 `dialects` 统一成「`common` → 检测基准侧 → 另一侧」同序，列表行还补了 `source_dialect`
（`RelatedWordResult` 没有该字段）。但 `headword` 仍是 `string_agg(…, ' / ', …)` 拼好的**一个字符串**
（`src/lexicon/repository/query.rs:159`）。

**缺口**：A1 阶段 4 要的是按**管理员偏好侧**在前呈现，与「检测基准侧在前」不是同一个顺序。
前端要按偏好重排就只能 `split(" / ")`，而 §8.3 / §9.3 明确禁止（短语词条里可能出现 ` / `）。

**提案**：保留 `headword` 不动，额外返回结构化字段，顺序与 `dialects` 一致：

```jsonc
{ "headword": "colour / color", "dialects": ["uk", "us"], "source_dialect": "uk",
  "headword_variants": [ { "dialect": "uk", "headword": "colour" },
                         { "dialect": "us", "headword": "color"  } ] }
```

`unified` 词条返回单元素 `[{ "dialect": "common", "headword": "…" }]`。
**两处一起改**（`AdminWordListItem` 与 `RelatedWordResult`）——§9 刚把两者的排序统一过，别只改一处又分叉。

**落地时的取舍**：字段做成**必填**而非可选——它对任何词条都有值（`unified` 也有单元素），
与同为必填的 `dialects` 同源；`endpoints.contract.test.ts` 用的是 `arrayContaining`，
`required` 多一项不影响既有断言。

## 明确不提案的事

- **不**下线 `POST /admin/lexicon/dialect-variant-suggestions`（需求 Q5：能力保留，前端只是不再在第 3 步调用它）。
- **不**改 `entry_headword_keys` 的双 scope 查重——需求 Q2 的结论依赖它把 `centre` / `center` 互斥住。
- **不**改 `WordHeadwordsV2` 判别联合、**不**删 `DialectValueV2` 的 `distinguish` 分支、**不**动 `headword_shape_check`。
- **不**写存量数据迁移脚本：前端采「懒收敛」，旧 `distinguish` 词条在下一次保存时自然写成单份。

## 落地检查清单

- [x] P1：改 `validation/meanings.rs` + 4 条 handler 断言
- [x] P1-b：前端阶段 3 上测试服后已改 `content_completion/worker.rs`（2026-08-20）
- [x] P2：迁移 + repository + DTO + 新端点 + 三个测试文件
- [x] `cargo run --bin export_openapi` 重导 `docs/openapi.json`；`cargo sqlx prepare` 刷新 `.sqlx`（CI 走 `SQLX_OFFLINE`）
- [x] `docs/frontend-integration.md` 追加 §10 / §11，沿用 §8 / §9 的四段式
- [x] P3：`headword_variants` + 两处 handler 断言 + `docs/frontend-integration.md` §12
- [ ] 通知前端 `pnpm --filter @tsz/api-client sync:openapi` 并解除契约测试的 PENDING 项
