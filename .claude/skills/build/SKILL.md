---
name: build
description: 按已确认的 spec 实现 tsz-rust 后端改动，向验收命令收敛，同步 OpenAPI/.sqlx/迁移并补有价值的测试，收尾核对 spec 覆盖。用于"按 design.md 实现""开始做"；不做评估，不提交推送。
---

# 后端实现：从 spec 开工，向验收命令收敛

本文件是 Claude Code 下后端 build 的**唯一**流程来源。项目约定读 `AGENTS.md`。
**本技能不提交、不推送、不开 PR、不部署**——那是 [ship](../ship/SKILL.md) 和 deploy。

## 1. 开工前装好验收回路

1. 读 spec（`docs/features/<slug>/design.md`，跨仓功能可能在前端仓）。**这是本会话的主要输入。**
2. 取出验收命令先跑一次记下基线（通常是红的）。挂成 `/goal` 条件让每轮自动复检。
   没有可跑命令的 spec 是缺陷——退回 [assess](../assess/SKILL.md) 补上。
3. 确认分支与工作区。**用户在 `dev` 上开发且工作区常有未提交改动——一律保留，不清空、不 stash。**
4. 确认测试依赖：本地 Postgres / Redis 的连接与隔离。
   **不要用「先启动试试」来诊断未知数据库**——后端启动会自动跑迁移。

## 2. 实施

按 handler → DTO → service → repository → 数据约束的依赖顺序改。

- **迁移**：up/down 必须成对，放在 `migrations/`。写完先在隔离库上验证 up 与 down 都能跑通。
  不在共享库上试迁移，不用 `FLUSHDB` / 清库解决问题。
- **SQL 改动**：跑 `cargo sqlx prepare -- --all-targets --all-features` 刷新 `.sqlx`，**审查生成的 diff**。
  离线编译依赖当前 `.sqlx`；缓存缺失时定位 SQL/schema 变化，按已确认的隔离数据库准备缓存。
- **API 改动**：按原生流程导出 OpenAPI：

  ```bash
  SQLX_OFFLINE=true cargo run --locked --all-features --bin export_openapi
  ```

  它只生成 `docs/openapi.json`，不启动服务。

**spec 没说清的**：常规实现细节自行判断并说明；影响验收标准或范围的停下问。
需要补调研派 [rust-scout](../../agents/rust-scout.md)，契约存疑派 [contract-auditor](../../agents/contract-auditor.md)。

## 3. 边实现边测

| 层            | 适用证据                                             |
| ------------- | ---------------------------------------------------- |
| 单元 `--lib`  | 纯函数、校验、映射、状态机                           |
| handler 集成  | 状态码、Problem Details、权限分支、幂等              |
| repository    | 事务边界、唯一/外键约束、并发冲突、失败原子性        |
| 契约          | 与 `docs/openapi.json` 及前端快照对齐                |

- 修 bug **先写会失败的回归测试**，再让它变绿。
- 只覆盖本次真实涉及的权限、异常、边界、并发风险。
- 断言公开行为与 wire 契约，不断言私有实现，不弱化断言来变绿。
- 新增测试 target 时**记得登记进 CI 分区清单**，否则 CI 不会跑它。
- 需要数据库的测试先核实连接选择逻辑与隔离，避免 Redis fallback 命中共享实例。

## 4. 迭代只跑受影响检查

用 `cargo test --locked --all-features --lib` 或 `--test <target>` 圈定范围。
**完整三件套留到交付前统一跑一次，不在实现过程中反复全量运行**：

```bash
cargo fmt --all -- --check
cargo clippy --locked --all-targets --all-features -- -D warnings
cargo test --locked --all-features
```

clippy 已包含编译检查，不为同一状态额外重复 `cargo check`。
耗时命令用可继续读取的会话句柄，保存真实退出码，不因工具提前返回而误判完成。

## 5. 收尾核对

1. 验收命令跑绿，保留实际输出作为证据。
2. 派 [spec-verifier](../../agents/spec-verifier.md)，给它 spec 路径和本次 diff，逐条核对验收标准覆盖。
3. 核对生成物同步到位：`docs/openapi.json`、`.sqlx`、迁移 up/down、CI 分区清单。
4. 无法验证的（需前端配合、依赖未就绪）明确标注，不写成已通过。

## 6. 交接

交付实际结果、验证证据、迁移与发布顺序、未就绪依赖和工作区状态。
用户已要求提交/推送/PR 时才进入 [ship](../ship/SKILL.md)，**建议开新会话做审查**。
需要前端配套时说明发布顺序，但**不自动授权前端改动或任何部署**。
