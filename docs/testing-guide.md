# 后端测试指南

本指南以当前代码、Cargo 配置与 CI 为准，仓储测试使用具体实现与真实数据库，不按旧方案引入 trait/fake 双实现或 nextest。

## 实际分层

- **纯逻辑测试**：就近放在源文件的 `#[cfg(test)]` 模块；优先验证可观察输入输出、边界与不变量。
- **仓储与业务事务**：使用具体仓储和 `#[sqlx::test]` 真库测试。示例为 [UserRepository](../src/user/repository.rs) 与 [user_service.rs](../tests/user_service.rs)，不是 trait 仓储配内存 fake 的双实现契约测试。
- **HTTP 集成测试**：`tests/*_handler.rs` 等验证真实路由、身份、状态码、响应和数据库效果；按照对应测试已有的服务装配方式准备依赖。
- **基础设施替身**：只在项目已经提供或确有必要的边界使用，例如 [memory storage](../src/platform/storage/memory.rs)；不要为了套用旧指南为所有 repository 新建 trait/fake 层。
- **部署与 CI 工具**：`ops/test_*.py` 使用 Python `unittest`；具体检查和运行顺序以 CI 为准。

## 环境前置条件

数据库测试需要可创建测试数据库的 PostgreSQL 连接；部分 handler 测试还需要 Redis。`SQLX_OFFLINE=true` 只让 SQLx 编译期查询使用已提交缓存，不会消除运行期数据库依赖。

本地依赖布局见 [docker-compose.yml](../docker-compose.yml)，变量示例见 [.env.example](../.env.example)。本地数据库示例端口与 CI 不同，不应直接复制 CI 的地址。启动依赖前检查端口与已有服务，测试应使用隔离环境，不能指向生产库。

在对应任务 worktree 根目录配置好环境后，按改动选择验证：

```bash
cargo fmt --all -- --check
cargo clippy --locked --all-targets --all-features -- -D warnings
cargo test --locked --all-features
cargo test --locked --all-features --test user_service
cargo test --locked --all-features <test_name>
```

命令是验证入口，不表示未配置环境时可以直接通过；先复现问题或确定验收断言，再运行相关目标。

## CI 与专项工具

[当前 CI](../.github/workflows/ci.yml)分别执行静态检查、lib/bins、doc tests 与按模块组织的集成测试，使用 `cargo test`，不是旧指南的 `cargo nextest run`。模块分组逻辑见 [ci_test_modules.py](../ops/ci_test_modules.py)，新增测试目标后应核实分组检查。

部署文档或工具变更的相关门禁包括：

```bash
python3 -m unittest ops/test_deploy_skill.py ops/test_deployment_auth_smoke.py
python3 -m unittest ops/test_ci_workflow.py ops/test_ci_test_modules.py
```

是否还需要其他专项测试由变更风险决定；不要把这几条命令当成绕过项目交付门禁的完整替代。

## 判定与报告

用断言验证业务规则、错误语义、并发和迁移约束，不为覆盖率数字补弱断言。历史测试矩阵表示应该验证的场景，历史报告只证明对应版本与环境下的结果；失败、未运行与环境阻塞应分别记录。

不能把单测或 mock 成功当作真实前后端联调通过。涉及数据库迁移与配套发布时，继续验证实际 schema 与消费者的新旧组合，遵守 [AGENTS.md](../AGENTS.md) 和项目技能。
