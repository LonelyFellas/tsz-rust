# tsz-rust

「天生会背」后端的 Rust 重写（axum + sqlx + Postgres），从 [tsz-go](../tsz-go) 功能重写而来。
设计文档见 [docs/](docs/)：[重写计划](docs/rewrite-plan.md) · [项目结构](docs/project-structure.md) · [测试规范](docs/testing-guide.md)。

## 环境要求

- Rust（edition 2024）
- Docker（跑本地 Postgres）

## 首次设置

```bash
# 1. 起本地 Postgres（宿主 5433 → 容器 5432）
docker compose up -d db
docker compose ps                      # 等 STATUS 变 healthy

# 2. 配置环境变量：复制示例再按需修改
cp .env.example .env

# 3. 启用 git hooks（每个新 clone 都要跑一次，见下方「Git Hooks」）
git config core.hooksPath .githooks
```

> `.env` 含密码、已被 `.gitignore` 忽略，不会提交；`.env.example` 是可提交的占位模板。

## 常用命令

```bash
cargo run                                   # 启动服务（默认 :8383）
cargo test                                  # 全量测试（含连库集成测试，需 Docker 库在跑）
cargo test --lib                            # 只跑源码内单元测试（不连库）
cargo clippy --all-targets -- -D warnings   # lint 门禁
cargo fmt                                   # 格式化

# 验证服务
curl localhost:8383/healthz                 # 存活（不碰库）
curl localhost:8383/readyz                  # 就绪（连库探活）

# sqlx
cargo sqlx prepare
```

## 数据库

```bash
docker compose up -d db        # 后台起 Postgres
docker compose ps              # 看状态（等 healthy）
docker compose logs -f db      # 看日志
docker compose down            # 停（数据留在卷里）
docker compose down -v         # 停并删数据（重置）
```

连接串（`.env` 里）：`postgres://postgres:postgres@localhost:5433/tsz_rust`

## Git Hooks

仓库自带 hooks（在 `.githooks/`，版本可控）：

| Hook | 跑什么 |
|------|--------|
| `pre-commit` | `cargo clippy --all-targets -- -D warnings` |
| `pre-push` | `cargo clippy ...` + `cargo test` |

**启用**（每个新 clone 都要执行一次——`core.hooksPath` 是本机配置，不随仓库提交）：

```bash
git config core.hooksPath .githooks
```

**注意事项：**

- **clippy 零容忍**：`-D warnings` 把任何警告升级为错误，有警告时 commit / push 会被拦。先跑 `cargo clippy --all-targets -- -D warnings` 修干净。
- **pre-push 需要 Docker 库在跑**：`cargo test` 里的 `tests/health.rs` 用 `#[sqlx::test]` 连真库，**库没起来时 `git push` 会被拦住**。推送前确保 `docker compose ps` 是 healthy。
  - 若不想每次推送都依赖库，把 `.githooks/pre-push` 的 `cargo test` 改成 `cargo test --lib`（只跑不连库的单元测试），连库测试留给 CI。
