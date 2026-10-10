---
name: build
description: 按明确的任务或已有设计实现 tsz-rust 后端改动，完成风险验证与验收，按影响同步 OpenAPI/.sqlx/迁移。用于后端实施；不重复评估，不自行扩大提交或部署范围。
---

# 后端实现：向可观察验收收敛

通用分档、授权、工作区保护与验证复用遵循 [AGENTS.md](../../../AGENTS.md)。单独实施请求到本地结果；原请求已包含交付就连续进入 ship，不因换技能再次确认。

## 1. 使用已有验收与依赖

读取任务描述或已有设计，补清必要的验收与依赖即可，不要求先取得单独的 spec 批准。
按实际调用链和剩余关键路径实施；重大未决取舍按项目规则处理，常规实现细节自行决定。
测试需要 Postgres/Redis 时先核实连接与隔离；后端启动会迁移，不以启动试探未知数据库。

## 2. 实施并同步相关生成物

- **迁移**：up/down 成对，在隔离库验证两个方向与历史数据影响，说明回退限制；不在共享库试迁移或清库/FLUSHDB。
- **SQL**：按需要使用 `cargo sqlx prepare -- --all-targets --all-features` 刷新 `.sqlx` 并审查 diff。紧接 ship 时可由正常 pre-commit 承接；开发中需要缓存才能验证时提前生成。离线缓存缺失先查 SQL/schema 与隔离库，不重复同输入生成。
- **API**：使用 `SQLX_OFFLINE=true cargo run --locked --all-features --bin export_openapi` 生成 `docs/openapi.json`，不启动服务；[contract-sync](../contract-sync/SKILL.md) 已完成同输入导出时直接复用。
- **测试 target**：新增 target 登记 CI 分区清单，确认实际被执行。

## 3. 定向验证与最终门

按风险选择：纯逻辑用单元测试，权限/状态码/错误用 handler，事务/唯一约束/并发用 repository，wire 变化验证序列化/OpenAPI 与前端消费者。断言公开行为和真实约束，不弱化断言；Bug 先证明旧实现失败。

迭代运行 `--lib`、`--test <target>` 或相关契约测试。没有紧接 ship 时，代码改动运行 fmt、clippy 与受影响测试；数据库全量集成仅在跨模块风险或明确要求时运行。
紧接已授权 ship 时由原生 hooks 承接其覆盖的 clippy/单元测试，补齐 fmt 和 hooks 未覆盖的必要检查；clippy 已含编译检查，不重复 cargo check。纯规则文档检查按项目规则，不额外运行运行时套件。

跨仓功能的真实用户链路按选定前端 `.agents/skills/test/references/real-backend-acceptance.md` 验证；版本/环境不具备就标明阻塞，不以两端测试替代联调。配套发布复用 [配套发布清单](../contract-sync/references/paired-release.md)。

## 4. 当前 Agent 核对并交付

对照验收标准列已证明、失败与未验证项；核对本次涉及的 OpenAPI、`.sqlx`、迁移和 CI 清单。当前 Agent 完成这项核对，独立精确提交审查统一由 [ship](../ship/SKILL.md) 承接。
已授权交付就继续 ship；其余报告本地结果、验证、依赖与工作区状态。需要前端配套或发布时说明下一步，不自动扩大授权。
