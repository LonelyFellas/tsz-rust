---
name: test
description: 为 tsz-rust 选择和执行有回归价值的 Rust、SQLx、迁移及接口测试，按风险核实隔离和实际 CI 覆盖；仅测试设计时不实施。
---

# 后端验证

沿用 [AGENTS.md](../../../AGENTS.md) 与主任务验收，只补证据缺口。Bug 先证明旧实现失败，再验证修复；纯规则文档检查格式、引用与行为一致性，不额外运行 Rust 套件。

## 选择能证明风险的层

| 风险                          | 验证入口                                                             |
| ----------------------------- | -------------------------------------------------------------------- |
| 纯逻辑、校验、序列化          | 相关单元测试或 `cargo test --lib <filter>`                           |
| 权限、状态码、Problem Details | 对应 handler/集成 target                                             |
| 事务、唯一/外键、并发、迁移   | 隔离 Postgres/Redis 下的 repository/集成测试及 up/down 验证          |
| DTO/OpenAPI 与严格消费者      | [contract-sync](../contract-sync/SKILL.md)、序列化及受影响消费者测试 |

执行前读取当前 Cargo、CI、hooks 与实际测试连接选择逻辑；上表是选择入口，不要求全部运行。新增 target 纳入 CI 分区清单，不为数量或覆盖率堆断言。

## 数据与生成物

数据库/Redis 来源先按 [dev-env](../dev-env/SKILL.md) 核实。显式传入该测试实际读取的隔离连接，避免 fallback 命中共享实例。`SQLX_OFFLINE=true` 仅影响编译期查询缓存，不代表运行无需数据库；不得以裸启动服务试探环境。

迁移验证 up/down、历史数据、约束和回退限制；不清共享库、FLUSHDB 或降低约束。需要刷新 `.sqlx` 时确认隔离库及 schema，再用原生生成链，复用同输入产物。
断言公开行为与真实约束，不弱化输入或断言来变绿；缺数据库、凭据或缓存时区分未运行与失败。

## 统一最终门

迭代使用受影响的 `--lib`、`--test <target>` 或过滤命令。代码改动没有紧接 ship 时运行 fmt、clippy 与受影响测试；clippy 已含编译检查，不重复 cargo check。数据库全量集成仅在跨模块风险或明确要求时运行。
紧接已授权 [ship](../ship/SKILL.md) 时，正常 pre-commit 承接 SQLx/Clippy，pre-push 承接 fmt；全量测试由 PR/main CI 执行，已有同状态定向结果复用，hooks 不跳过。实际门禁以当前配置为准。

跨仓页面链路读取选定前端 checkout 的 `.agents/skills/test/references/real-backend-acceptance.md`，核实目标版本和 mock；涉及 Agent 时同时读取其项目 test/dev-env。各仓测试或 mock 通过不能代替真实联调。
临时数据按项目规则彻底清理并独立复查，失败和中断也须处理。报告行为、命令、退出码、未验证项和清理结果，不把未来 CI 写成已通过。
