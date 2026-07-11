# tsz-rust 重写计划（第一周）

> ⚠️ **本文档是项目启动时的初始计划，部分架构决策在实施中已演进——阅读时以「实况」一节为准。**
> 已废弃的写法（trait 抽象 repository、fake + contract test）在下方逐日计划里仍有残留，**不要照它做**，
> 见 §架构约定 的更正说明。目录/依赖骨架中未落地的部分为后续规划，非当前实况。

## 实况（截至 2026-07-11，以此为准）

- **栈实际版本**：axum 0.8 + **sqlx 0.9**（非 0.8）+ tokio + **jsonwebtoken 10**（非 9，且必须
  `default-features=false, features=["rust_crypto"]`）+ bcrypt 0.19 + **uuid v7**（非 v4）+ rand 0.10。
  以仓库根 [`Cargo.toml`](../Cargo.toml) 为准，下方「推荐 Cargo.toml 依赖骨架」已过期，仅存档参考。
- **架构决策已演进**：repository **不用 trait/fake/contract test**，改为具体 `UserRepository { pool }`
  固有 async 方法 + `#[sqlx::test]` 直接打真库（清爽重写、地道 Rust，YAGNI）。逐日计划里所有
  「trait + pg 实现」「contract test」「fake」字样均已废弃。
- **实际目录是扁平的**：`auth.rs` 单文件（非 `auth/` 目录）；健康检查内联在 `lib.rs`（无 `platform/http/`
  子树、无独立 `health.rs`）；尚无 `bin/migrate.rs`、`bin/seed.rs`、`session/`、`otp/`、`word/`、
  `storage.rs`、中间件/限流/metrics——这些是后续规划，不是当前形状。
- **已完成**：platform 地基（config/error/lib/db + `/healthz` `/readyz`）；user 域全 6 表 schema + 约束测试；
  user model + repository（create / get_by_identifier）；user service `register`；user register handler
  （`POST /user/register`）；auth access token（`TokenManager`，HS256，realm 用标准 `aud`）。
- **进行中/下一步**：login handler → session/refresh → otp 域。

---

> 目标：把 `tsz-go`（「天生会背」Gin 模块化单体）的**核心 MVP 主线**用 Rust 重写并跑通。
> 主线范围：`platform` + `auth/session/otp` + `user` + `word`。
> admin / authz / coin / audit 留到第二周。
>
> **这是功能重写，不做数据迁移。** 不照搬 go 的 26 个历史增量迁移（含 add 后又 revert 的折腾），
> 直接按**最终 schema** 写一套干净的合并迁移；迁移编号从 0 起，无「号段永不复用」历史包袱。

## 0. 技术选型对照

| 关注点 | tsz-go | tsz-rust（本次） | 备注 |
|--------|--------|------------------|------|
| HTTP | Gin | **axum 0.8** | 已选 |
| 运行时 | goroutine | **tokio** | 已选 |
| DB | pgx/v5 手写 SQL | **sqlx 0.9**（postgres, runtime-tokio） | 编译期 `query!` 校验；连接池 `PgPool`。⚠️ 需 `.sqlx` 离线缓存或可连库才能编译 |
| 迁移 | golang-migrate（26 个增量） | **sqlx::migrate!**（重画干净 schema） | 功能重写，不复用历史迁移 |
| 鉴权 | JWT HS256 | **jsonwebtoken 10**（`rust_crypto`） | realm 用标准 `aud`；双身份两套 key（admin 尚未接） |
| 口令哈希 | bcrypt | **bcrypt 0.19** crate | DEFAULT_COST；>72 字节会静默截断，service 层守上限 |
| 配置 | env | **serde + envy** | `load_config` + 可测的 `Config::from_pairs` |
| 日志 | slog（JSON） | **tracing + tracing-subscriber** | json layer 待接 |
| 指标 | Prometheus | metrics + metrics-exporter-prometheus | 后续规划，未落地 |
| 链路 | OTel OTLP | tracing-opentelemetry + opentelemetry-otlp | 后续规划，opt-in |
| 校验 | validator/v10 | 手写 newtype-parse（`DisplayName`/`Password`） | 未引 validator crate |
| UUID | google/uuid | **uuid v7**（serde） | 主键用 v7（时间有序） |
| 错误 | error wrapping | **thiserror**（领域）+ 统一 `AppError→Response` | |
| OSS | 阿里云 SDK | ⚠️ 无官方 Rust SDK → **reqwest + 手写 V4 签名** | 第 6 天，风险点 |

### 依赖骨架

> ⚠️ **下面这份是启动时的初稿版本号，已全面过期**（sqlx 0.8→0.9、jsonwebtoken 9→10、bcrypt 0.15→0.19、
> uuid v4→v7、rand 0.8→0.10，且 validator/tower-http/metrics 尚未引入）。
> **实际依赖以仓库根 [`Cargo.toml`](../Cargo.toml) 为准**，此块仅留作历史存档。
> jsonwebtoken 有坑：`default-features` 不含 crypto provider，必须
> `jsonwebtoken = { version = "10", default-features = false, features = ["rust_crypto"] }`，否则运行时 panic。

```toml
# —— 历史存档（勿照抄，见上）——
axum = "0.8"
tokio = { version = "1", features = ["full"] }
sqlx = { version = "0.8", features = ["runtime-tokio", "tls-rustls", "postgres", "uuid", "chrono", "macros", "migrate"] }
serde = { version = "1", features = ["derive"] }
serde_json = "1"
jsonwebtoken = "9"
bcrypt = "0.15"
uuid = { version = "1", features = ["v4", "serde"] }
chrono = { version = "0.4", features = ["serde"] }
thiserror = "2"
validator = { version = "0.18", features = ["derive"] }
tracing = "0.1"
tracing-subscriber = { version = "0.3", features = ["json", "env-filter"] }
envy = "0.4"
tower = "0.5"
tower-http = { version = "0.6", features = ["trace", "cors"] }
metrics = "0.24"
metrics-exporter-prometheus = "0.16"
rand = "0.8"           # otp/token 随机
# dev
sqlx = { ..., features = [..., "migrate"] }
```

## 1. 目录结构映射

Go 的 `internal/<module>/{model,repository,service,handler}` → Rust 每模块一个 crate 内模块：

```
src/
├── main.rs                 # 装配依赖、起服务（对标 cmd/server）
├── bin/
│   ├── migrate.rs          # 对标 cmd/migrate
│   └── seed.rs             # 对标 cmd/seed（首个超管，第二周）
├── config.rs               # internal/config
├── error.rs                # 统一 AppError + IntoResponse（对标 platform/httperr）
├── platform/
│   ├── db.rs               # PgPool 构建 + 迁移（platform/database）
│   ├── http/
│   │   ├── router.rs       # 路由装配（platform/httpserver/router）
│   │   ├── middleware.rs    # 认证/请求日志/恢复
│   │   ├── ratelimit.rs     # 按 IP 限流
│   │   ├── health.rs        # /healthz /readyz
│   │   └── metrics.rs
│   ├── log.rs
│   └── storage.rs          # OSS
├── auth/                   # JWT 签发/解析、角色、班级范围
│   ├── mod.rs  token.rs
├── session/               # refresh token 轮换、单设备
├── otp/                   # 一次性验证码 + Sender trait
├── user/                  # 账号、登录注册、学习设置、头像、注销
│   ├── mod.rs model.rs repository.rs service.rs handler.rs
└── word/                  # 智能词表编辑
migrations/                # 从 go 复制的 26 个 .sql（up/down）
```

> ⚠️ **上面的目录树是第一周的规划全景，非当前实况**：`auth.rs` 是单文件、健康检查内联在 `lib.rs`、
> 尚无 `bin/`、`platform/http/`、`session/`、`otp/`、`word/`、`storage.rs` 与各中间件。实际结构见
> [project-structure.md](project-structure.md) 与仓库现状。

架构约定（**已更正，覆盖初稿**）：
- 依赖方向向内：`handler → service → repository`。✅ 保留
- ~~repository 用 **trait** 抽象 + fake，service 依赖 trait~~ → **已废弃**。改为具体
  `UserRepository { pool }` 固有 async 方法，service 直接持有它。理由：清爽重写、地道 Rust、YAGNI；
  真需要脱库测 service 时再抽 trait。
- ~~每个有状态模块一套 **contract test**（fake + 真库同跑）~~ → **已废弃**。改为
  repository 用 `#[sqlx::test]` **直接对真 Postgres** 测；纯逻辑（value object、错误映射）走内联单测。
  不搞 fake / 契约测试 / async-trait。

---

## 逐日计划

### Day 1 — 平台地基（platform）
把「能起服务、能连库、能迁移、能统一报错」四件事做完。

- [ ] 建 `config.rs`：`Config::from_env()`，覆盖 DATABASE_URL、端口、JWT 双 key、trusted proxies、AUTO_MIGRATE 等（对标 `internal/config/config.go`）。
- [ ] `platform/db.rs`：构建 `PgPool`（连接数/超时对齐 pgxpool），`/readyz` 用的 ping。
- [ ] `migrations/`：**不复制** go 的 26 个增量迁移。把它们叠加后的**最终表结构**读出来（users / user_roles / *_profiles / verification_codes / refresh_tokens / words 等），按模块写成几支干净的合并迁移，编号从 0001 起；`bin/migrate.rs` 用 `sqlx::migrate!` 跑。
- [ ] `error.rs`：`AppError`（thiserror）+ `IntoResponse`，统一 JSON 错误体（对标 `platform/httperr`，字段/HTTP 码对齐）。
- [ ] `platform/http/router.rs`：axum `Router`，挂 `/healthz` `/readyz`。
- [ ] `platform/log.rs`：tracing JSON subscriber。
- **完成标准**：`cargo run` 起服务，`/readyz` 连库返回 200，`cargo run --bin migrate` 建全表。

### Day 2 — 身份基座（auth + session + otp）
- [ ] `auth/token.rs`：`TokenManager`（HS256 签发/校验 access token），claims 结构对齐 go；双身份两套 key。
- [ ] `session/`：refresh token 轮换 + grace + 单设备，repository trait + Postgres 实现（对标迁移 000003/000022）。
- [ ] `otp/`：验证码生成、尝试次数上限、过期；`Sender` trait（先 mock 实现，对标 `otp/sender.go`）。
- [ ] 三者各写 contract test（fake + pg）。
- **完成标准**：能签发/校验 JWT；refresh 轮换单测通过；otp 契约测试 fake 与 pg 一致。

### Day 3 — user 模块（上）：注册登录核心
user 是最大模块（2610 行），拆两天。

- [ ] `user/model.rs`：User、角色（student/teacher）、学习设置、状态枚举。
- [ ] `user/repository.rs`：trait + Postgres 实现（users / user_roles / *_profiles）。
- [ ] `user/service.rs`：Register（手机或邮箱二选一 CHECK）、Login、bcrypt 校验、角色切换、账号禁用判定。把 go 里那批 `Err*` 语义（不区分泄露用户名等隐私约束）逐条搬过来。
- [ ] `user/handler.rs`：注册/登录/刷新/登出路由，接 validator。
- **完成标准**：注册→登录→拿 access/refresh 全链路 e2e 通。

### Day 4 — user 模块（下）：账号自服务
- [ ] 学习设置读写（迁移 000006）。
- [ ] 口令重置（reset code，000012）、账号注销（deletion code，000013）、联系方式绑定（000016）——都走 otp。
- [ ] 头像上传/更新（000015，先接 storage 占位，OSS 留到 Day 6）。
- [ ] display name 校验（禁 `< >`/控制符）。
- [ ] 补齐 user 的 contract test + handler test。
- **完成标准**：user 模块功能与 go 对齐，契约测试绿。

### Day 5 — word 模块（智能词表）
word 2426 行，MVP 里第二大。

- [ ] `word/model.rs` + 迁移 000018 对齐。
- [ ] repository trait + pg 实现；service 编辑逻辑（排序：教材序/字母/COCA 引用等，按 go 逐条搬）。
- [ ] handler（admin 域路由先挂最小集，鉴权用 auth 中间件占位）。
- [ ] contract test。
- **完成标准**：词表 CRUD/排序端到端通。

### Day 6 — 横切能力（可观测 + 中间件 + 存储）
- [ ] `platform/http/middleware.rs`：请求日志、panic 恢复、认证中间件（从 header 取 token → claims 注入 extension）。
- [ ] `platform/http/ratelimit.rs`：按 IP 限流（对标 go 的 `IPRateLimiter`，注意 trusted proxies / XFF 防伪）。
- [ ] `/metrics`（metrics-exporter-prometheus）+ 请求指标中间件。
- [ ] tracing-opentelemetry OTLP opt-in。
- [ ] `platform/storage.rs`：**阿里云 OSS V4 签名 + reqwest**（无官方 SDK，最大风险点，预留整天缓冲）。接上 Day 4 的头像。
- **完成标准**：`/metrics` 有数；限流生效；头像真能传上 OSS。

### Day 7 — 测试收口 + 文档 + 打磨
- [ ] e2e 集成测试（对标 `httpserver/e2e_integration_test.go`）：真 Postgres 跑注册→登录→改设置→词表全流程。
- [ ] 统一 contract test 跑法，补齐各模块 fake 漂移检查。
- [ ] `docs/api.md` + `docs/openapi.yaml` 同步（go 那边有 redocly lint 门禁，这边也建同款）。
- [ ] Dockerfile + docker-compose（app + postgres + 一次性 migrate）。
- [ ] `CLAUDE.md`（rust 版）+ Makefile/justfile 常用命令。
- **完成标准**：`cargo test` 全绿，docker compose up 一把起栈。

---

## 风险与注意

1. **阿里云 OSS 无官方 Rust SDK** — Day 6 手写 V4 签名，若卡住可先用本地文件桩，OSS 顺延第二周。
2. **sqlx 编译期校验需要一个可连的库**（或 `sqlx prepare` 生成 `.sqlx` 离线缓存）—— ⚠️ **尚未解决**：
   目前**无 `.sqlx/` 离线缓存**，Docker 库没起时 `cargo build`/clippy/test 全部失败，pre-commit 钩子的
   `clippy --all-targets` 也会被自己卡住，CI 无库同样编译失败。**建议尽快 `cargo sqlx prepare` 并提交 `.sqlx/`**。
3. **双身份体系**（users vs admins）跨库引用用 `(realm, id)` 多态弱引用、不建外键——第二周做 admin 时照旧。
4. **不做数据迁移，schema 重画** —— 需要先把 go 26 个增量迁移「拍平」成最终结构（注意 000009 revert、000014 phone_or_email、000026 on_delete 等最终态），别漏字段/约束/索引。
5. ~~**契约测试的 fault-injection hook**（go 的 `*Fn`/`*Err`）~~ → **已失效**：不做契约测试，
   repository 直接 `#[sqlx::test]` 打真库；错误映射（如唯一冲突 `23505` → 领域错误）在真库测里覆盖。
6. **一周做 4 条主线偏激进**：若 Day 5 词表吃紧，优先保 user 全绿，word 允许缩到 CRUD 最小集，排序逻辑顺延。

## 每日节奏建议
- 每天先补该模块迁移（若有），再 model → repository(trait+pg) → service → handler → contract test。
- 每写完一个模块立刻跑 e2e 冒烟，别攒到最后。
- 遇到 go 里带长注释解释「为什么」的隐私/安全约束，逐条搬注释，别只搬代码。
