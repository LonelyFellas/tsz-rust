---
name: build
description: 按已确认的 spec 实现 tsz-rust 后端改动，向验收命令收敛，同步 OpenAPI/.sqlx/迁移并补有价值的测试，收尾核对 spec 覆盖。用于「按 design.md 实现」；不做评估，不提交推送。
---

# 后端实现：从 spec 开工，向验收命令收敛

项目约定读 `AGENTS.md`。**本技能不提交、不推送、不开 PR、不部署。**

## 1. 开工前装好验收回路

读 spec（`docs/features/<slug>/design.md`，跨仓功能可能在前端仓），它是本次的主要输入。
取出验收命令先跑一次记下基线，此后每次迭代向它收敛；宿主支持持续复检目标时挂上去。
**没有可跑命令的 spec 是缺陷**——退回 [assess](../assess/SKILL.md) 补上。

确认分支与工作区。**工作区常有用户未提交改动——一律保留，不清空、不 stash。**
确认测试依赖的 Postgres / Redis 连接与隔离；**不要用「先启动试试」诊断未知数据库**——
后端启动会自动跑迁移。

## 2. 实施

按 handler → DTO → service → repository → 数据约束的依赖顺序改。

- **迁移**：up/down 成对，先在隔离库验证两个方向都能跑通。不在共享库上试迁移，
  不用 `FLUSHDB` / 清库解决问题。
- **SQL 改动**：`cargo sqlx prepare -- --all-targets --all-features` 刷新 `.sqlx` 并审查生成 diff。
  离线编译依赖当前 `.sqlx`；缓存缺失时定位 SQL/schema 变化，按已确认的隔离数据库准备缓存。
- **API 改动**：`SQLX_OFFLINE=true cargo run --locked --all-features --bin export_openapi`
  生成 `docs/openapi.json`，不启动服务。

spec 没说清的：常规实现细节自行判断并说明；影响验收标准或范围的停下问。
需要补调研时使用只读子代理或限定范围的检索，不在主上下文里翻大量文件。

## 3. 边实现边测

| 层           | 适用证据                                      |
| ------------ | --------------------------------------------- |
| 单元 `--lib` | 纯函数、校验、映射、状态机                    |
| handler 集成 | 状态码、Problem Details、权限分支、幂等       |
| repository   | 事务边界、唯一/外键约束、并发冲突、失败原子性 |
| 契约         | 与 `docs/openapi.json` 及前端快照对齐         |

- 修 bug 先写会失败的回归测试，再让它变绿。
- 只覆盖本次真实涉及的权限、异常、边界与并发风险。
- 断言公开行为与 wire 契约，不断言私有实现，不弱化断言来变绿。
- **新增测试 target 必须登记进 CI 分区清单**，否则 CI 不会跑它。
- 需要数据库的测试先核实连接选择逻辑与隔离，避免 Redis fallback 命中共享实例。

## 4. 迭代只跑受影响检查

用 `--lib` 或 `--test <target>` 圈定范围。完整三件套留到交付前统一跑一次：

```bash
cargo fmt --all -- --check
cargo clippy --locked --all-targets --all-features -- -D warnings
cargo test --locked --all-features
```

clippy 已含编译检查，不重复 `cargo check`。耗时命令用可继续读取的会话句柄，保存真实退出码。

## 5. 收尾核对

1. 验收命令跑绿，保留实际输出作为证据。
2. 拿 spec 的验收标准**逐条核对覆盖**：已覆盖（附位置与证明）/ 未覆盖（阻断）/
   无法验证（说明缺什么证据）。宿主有独立 reviewer 能力时交给它，只报缺口不报风格。
3. 核对生成物同步到位：`docs/openapi.json`、`.sqlx`、迁移 up/down、CI 分区清单。

## 6. 交接

交付实际结果、验证证据、迁移与发布顺序、未就绪依赖和工作区状态。
用户已要求交付时才进入 [ship](../ship/SKILL.md)，且建议在新的上下文里做审查。
需要前端配套时说明发布顺序，但不自动授权前端改动或任何部署。
