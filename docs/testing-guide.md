# tsz-rust 测试规范

> 一份多维度的后端测试标准，对标大型 Rust 项目的通行做法，供长期参考。
> 核心结论：**Rust 测试框架是语言 + cargo 内置的，不引外部框架**；工程规范靠「分层 + 替身 + 数据 + 依赖隔离 + 度量 + CI」这几个维度共同支撑。
> 项目背景：从 `tsz-go`（Gin 模块化单体）功能重写，沿用其分层测试与契约测试思想。

---

## 目录

- [维度一：测试分类学（按范围）](#维度一测试分类学按范围)
- [维度二：组织与目录规范](#维度二组织与目录规范)
- [维度三：测试替身（Test Doubles）](#维度三测试替身test-doubles)
- [维度四：测试数据管理](#维度四测试数据管理)
- [维度五：外部依赖隔离策略](#维度五外部依赖隔离策略)
- [维度六：异步测试](#维度六异步测试)
- [维度七：契约测试](#维度七契约测试)
- [维度八：质量原则](#维度八质量原则)
- [维度九：覆盖率与度量](#维度九覆盖率与度量)
- [维度十：CI 与工具链](#维度十ci-与工具链)
- [维度十一：本项目落地约定](#维度十一本项目落地约定)

---

## 维度一：测试分类学（按范围）

大型项目普遍采用 **Test Pyramid / Test Trophy**：底层单元测试量大而快，越往上越少越慢。后端服务实践中更接近 Trophy —— 集成测试是重心。

| 类型 | 范围 | 速度 | 依赖 | 主要工具 | 本项目占比 |
|------|------|------|------|----------|-----------|
| **单元测试** unit | 单函数 / 单结构体 | 极快 | 无 | 内置 `#[test]` | 多 |
| **service 测试** | 业务逻辑，repo 用替身 | 快 | 无（trait mock） | `mockall` / 手写 fake | 多 |
| **handler 测试** | axum 路由入出参 | 快 | 无（`oneshot`） | `tower::ServiceExt` | 中 |
| **集成测试** integration | 跨模块 + 真 DB | 中 | Postgres | `#[sqlx::test]` | 中（重心） |
| **契约测试** contract | fake vs 真实现一致性 | 中 | Postgres | 泛型函数 | 每个有状态模块一套 |
| **端到端** e2e | 完整 HTTP 链路 | 慢 | 全栈 | 起真服务 + reqwest | 少（关键流程） |
| **快照测试** snapshot | 大响应体 / 序列化结构 | 快 | 无 | `insta` | 按需 |
| **属性测试** property | 随机输入验证不变量 | 中 | 无 | `proptest` | 编解码 / 校验器 |
| **模糊测试** fuzz | 崩溃 / panic 边界 | 慢 | nightly | `cargo-fuzz` | 解析类（选用） |
| **基准测试** benchmark | 性能回归 | 慢 | 无 | `criterion` | 热点路径（选用） |
| **文档测试** doctest | 示例即测试 | 快 | 无 | 内置（`///` 代码块） | 公共 API |

选型直觉：**能用单元/service 测清楚的逻辑，绝不升到集成层**（快、稳、定位准）；跨边界的正确性（SQL、事务、约束）才下沉到 `#[sqlx::test]`。

---

## 维度二：组织与目录规范

Rust 用**文件位置**天然区分白盒 / 黑盒，这是规范的起点：

```
src/
├── config.rs              # 内含 #[cfg(test)] mod tests —— 白盒，可测私有项
├── user/
│   ├── service.rs         #   同上：service 单测紧邻实现
│   └── repository.rs       #   契约测试 mod contract 紧邻 trait
tests/                      # 集成测试：每个文件是独立 crate，只见 pub API（黑盒）
├── common/
│   └── mod.rs             # 共享测试脚手架（建 app、种子数据、辅助断言）
├── user_flow.rs           # 用户注册登录 e2e
└── word_flow.rs
benches/                    # criterion 基准
```

| 规则 | 说明 |
|------|------|
| **白盒**：`#[cfg(test)] mod tests` | 编译进同一 crate，**能访问私有项**；单元 / service / 契约测试放这 |
| **黑盒**：`tests/*.rs` | 每个文件独立 crate，**只能调 `pub`**；集成 / e2e 放这 |
| **共享脚手架** | `tests/common/mod.rs`（注意是 `common/mod.rs` 不是 `common.rs`，否则被当成测试 crate 跑） |
| **命名** | 测试函数名描述**行为与预期**，`snake_case`；不要 `test1` |
| **就近原则** | 单元测试紧贴被测代码，改实现时测试在同一屏，减少漂移 |

### 命名约定：标识符英文，注释中文

**所有标识符（函数名、变量名、辅助函数、mod 名）一律英文**，注释用中文。测试函数名用英文动宾短语描述「行为 + 预期」，读起来像一句话。

| | 推荐 | 避免 |
|---|------|------|
| 行为符合预期 | `port_falls_back_to_default_when_absent` | `test_port`、`省略端口时回落到默认值` |
| 出错路径 | `missing_database_url_errors` | `test_error`、`缺少_database_url_应报错` |
| 不变量 | `env_key_is_case_insensitive` | `test5` |
| 辅助 / 数据 | `valid_baseline()`、`parse()` | `合法基线()`、`解析()` |

常用命名模式（择一保持一致）：
- `<行为>_<条件>`：`overrides_default_when_port_set`
- `<主体>_<预期>`：`disabled_account_cannot_login`
- 错误路径统一后缀 `_errors` / `_rejected`：`missing_jwt_secret_errors`

> 参考实现见 `src/config.rs` 的 `mod tests`。

---

## 维度三：测试替身（Test Doubles）

按 Gerard Meszaros 的五分类，映射到 Rust 的实现方式。**替身的前提是依赖抽象成 `trait` 并注入**（依赖倒置）。

| 替身 | 定义 | Rust 实现 |
|------|------|-----------|
| **Dummy** | 只为占位，不被调用 | 空结构体 / `()` |
| **Stub** | 返回预设值 | 手写 impl 返回固定数据，或 `mockall` 的 `returning` |
| **Fake** | 有可用的简化实现 | `InMemoryUserRepo`（用 `HashMap` 存）—— 对标 go 的 fake |
| **Spy** | 记录被怎么调用 | 内含 `Arc<Mutex<Vec<Call>>>` 记录参数 |
| **Mock** | 预设期望 + 自动校验调用 | `mockall::automock` 生成 |

### 手写 Fake vs mockall —— 选择标准

| 用手写 Fake | 用 mockall |
|-------------|-----------|
| 需要跨多测试复用、有真实行为语义 | 单测里只需某方法返回特定值 / 错误 |
| 要跑**契约测试**（fake 必须行为正确） | 验证「某方法被调用了几次、带什么参」 |
| 对标 go 的 fake，逻辑稳定 | 一次性的错误路径注入（fault injection） |

```rust
#[cfg_attr(test, mockall::automock)]     // 仅测试期生成 MockUserStore
#[async_trait::async_trait]
pub trait UserStore {
    async fn find_by_phone(&self, phone: &str) -> Result<Option<User>, StoreError>;
}

#[tokio::test]
async fn 仓储报错时_service_向上传播() {
    let mut store = MockUserStore::new();
    store.expect_find_by_phone()
         .returning(|_| Err(StoreError::Db));      // 注入错误路径
    let svc = UserService::new(store);
    assert!(svc.login("138...").await.is_err());
}
```

> 原则：**契约用 fake，错误路径用 mock**。fault-injection 不进契约（延续 go 的「hook 不属于契约」）。

---

## 维度四：测试数据管理

大型项目不散落魔法值，而是集中造数据。三种通行模式：

### 1. Test Data Builder（首选）
```rust
// tests/common/mod.rs
pub struct UserBuilder { phone: String, role: Role /* ... */ }
impl UserBuilder {
    pub fn new() -> Self { Self { phone: unique_phone(), role: Role::Student } }
    pub fn teacher(mut self) -> Self { self.role = Role::Teacher; self }
    pub fn build(self) -> NewUser { /* ... */ }
}
// 用：UserBuilder::new().teacher().build()  —— 只声明「与默认的差异」
```

### 2. Object Mother（一组命名好的典型样本）
```rust
pub fn 普通学员() -> NewUser { UserBuilder::new().build() }
pub fn 带邮箱的教师() -> NewUser { UserBuilder::new().teacher().email("t@x.com").build() }
```

### 3. 随机 / 唯一数据
```rust
use fake::{Fake, faker::internet::en::SafeEmail};
let email: String = SafeEmail().fake();      // fake crate 造真实感数据
fn unique_phone() -> String { /* uuid/随机，禁进程内自增计数器 */ }
```

| 铁律 | 原因 |
|------|------|
| **数据全局唯一**（uuid 邮箱 / 随机手机号） | 集成到共享库时避免撞历史残留行（延续 go 铁律） |
| **禁进程内自增计数器** | 跨运行会撞上次残留 |
| **`sqlx` fixtures** | 静态种子数据放 `fixtures/*.sql`，`#[sqlx::test(fixtures("users"))]` 自动加载 |

---

## 维度五：外部依赖隔离策略

分层决定「用真依赖还是替身」：**越往下越用替身，越往上越用真依赖**。

| 依赖 | 单元/service 层 | 集成/e2e 层 |
|------|----------------|-------------|
| **Postgres** | trait fake（内存） | `#[sqlx::test]` 临时库 / `testcontainers` |
| **HTTP 外部服务**（词典 / COCA / 讯飞 TTS） | trait mock | `wiremock`（起本地假 HTTP 服务） |
| **对象存储 OSS** | trait fake（内存桶） | `testcontainers` 起 MinIO |
| **时间 `now()`** | 注入 `Clock` trait，测试给固定时刻 | 同 |
| **随机 / token** | 注入 seed 或 `Rng` trait | 同 |

### 两种真 DB 方案

| 方案 | 机制 | 适用 |
|------|------|------|
| **`#[sqlx::test]`（首选）** | 每测建临时库 → 跑 `migrations/` → 注入 `PgPool` → 结束回滚 | 大多数集成测试，快、隔离、零残留 |
| **`testcontainers`** | 代码里 Docker 拉起真 Postgres 容器 | 要验证扩展 / 版本行为、CI 无常驻库时 |

`#[sqlx::test]` 直接替掉了 go 里手动维护 `tsz_test` 库的整套负担（建库、迁移、隔离、清理）。

### 外部 HTTP 用 wiremock 而非打真接口
```rust
use wiremock::{MockServer, Mock, ResponseTemplate, matchers::path};
let server = MockServer::start().await;
Mock::given(path("/tts")).respond_with(ResponseTemplate::new(200).set_body_string("<audio>"))
    .mount(&server).await;
// 把 client base_url 指向 server.uri()，测试完全离线、可断言请求
```

---

## 维度六：异步测试

后端几乎全 async，统一约定：

```rust
#[tokio::test]                              // 而非裸 #[test] + 手动 block_on
async fn 例子() { /* .await 直接用 */ }

#[tokio::test(flavor = "multi_thread")]     // 需要真并发时
async fn 并发场景() { }
```

| 约定 | 说明 |
|------|------|
| 统一 `#[tokio::test]` | 别在 `#[test]` 里手搓 runtime |
| 超时兜底 | 疑似死锁的测试用 `tokio::time::timeout` 包一层，避免挂死 CI |
| 别 `block_on` 混用 | 同一测试内不要嵌套 runtime |

---

## 维度七：契约测试

复刻 go 的 `runStoreContract`：一套断言同时跑 fake 与真 Postgres，杜绝 fake 漂移。Rust 用**泛型测试函数**实现：

```rust
// user/repository.rs
#[cfg(test)]
mod contract {
    use super::*;

    // 一套不变量，参数是任意实现 —— runStoreContract 的等价物
    pub async fn store_contract<S: UserStore>(store: S) {
        let created = store.create(普通学员()).await.unwrap();

        let found = store.find_by_phone(&created.phone).await.unwrap();
        assert_eq!(found.map(|u| u.id), Some(created.id), "建后应能按手机号查回");

        let missing = store.find_by_phone("不存在").await.unwrap();
        assert!(missing.is_none(), "查不到应返回 None 而非报错");
        // …其余不变量
    }

    #[tokio::test]
    async fn fake_满足契约() { store_contract(InMemoryUserRepo::new()).await; }

    #[sqlx::test]
    async fn postgres_满足契约(pool: sqlx::PgPool) { store_contract(PgUserRepo::new(pool)).await; }
}
```

要点：
- 契约**只覆盖正常不变量**；fault-injection（返回 `Err`）用 mockall 单独测。
- 任一实现行为漂移，另一边立刻红。
- 每个有状态模块（user / session / otp / word …）配一套。

---

## 维度八：质量原则

### FIRST 原则
| 字母 | 含义 | 落地 |
|------|------|------|
| **F**ast | 快 | 单元测试毫秒级；慢的标 `#[ignore]` 分档 |
| **I**solated | 隔离 | 测试间无共享可变状态，可任意顺序 / 并行 |
| **R**epeatable | 可重复 | 无时间 / 随机 / 网络依赖（注入替身） |
| **S**elf-validating | 自校验 | 用断言判定，不靠人肉看输出 |
| **T**imely | 及时 | 与实现同步写，不欠账 |

### 结构：AAA / Given-When-Then
```rust
#[tokio::test]
async fn 禁用账号不能登录() {
    // Arrange —— 准备
    let svc = UserService::new(fake_with_disabled_user());
    // Act —— 执行
    let r = svc.login("138...", "pw").await;
    // Assert —— 断言
    assert!(matches!(r, Err(AuthError::AccountDisabled)));
}
```

其他准则：
- **测行为，不测实现**：断言可观察的输入输出，别断言私有中间状态，否则重构即碎。
- **一个测试一个失败理由**：多个无关断言拆成多个测试。
- **断言带信息**：`assert!(cond, "为什么")`，红的时候一眼定位。
- **保留 go 的「为什么」注释**：隐私 / 安全约束（如错误不区分以防泄露用户名）逐条搬进测试注释。

---

## 维度九：覆盖率与度量

| 工具 | 用途 | 命令 |
|------|------|------|
| **`cargo-llvm-cov`（首选）** | LLVM 源码级覆盖率，准 | `cargo llvm-cov --html` |
| `cargo-tarpaulin` | 覆盖率（Linux） | `cargo tarpaulin` |
| **`cargo-mutants`** | 变异测试：改代码看测试能否发现，衡量**断言有效性** | `cargo mutants` |

原则：
- **覆盖率是下限信号不是目标**。80% 常见门槛，但别为凑数写无断言测试。
- **关注 diff 覆盖率**（新增代码的覆盖），比整体百分比更有意义（对标 go 的 diff_coverage）。
- **变异测试查「假绿」**：覆盖到了但断言太弱、改坏代码测试仍绿 —— `cargo-mutants` 能揪出来，比单看覆盖率深一层。

---

## 维度十：CI 与工具链

| 工具 | 作用 |
|------|------|
| **`cargo-nextest`（首选运行器）** | 比 `cargo test` 更快、输出更清晰、支持重试 / 分片 | 
| **`sqlx` offline（`.sqlx`）** | `cargo sqlx prepare` 生成离线缓存，CI 无库也能编译 `query!` 宏 |
| `cargo clippy -- -D warnings` | lint 门禁 |
| `cargo fmt --check` | 格式门禁 |
| `cargo-deny` | 依赖许可证 / 漏洞审计 |

### CI 分档（保持反馈快）
```
① 快档（每次 push）:  fmt + clippy + cargo nextest run --lib     # 纯单元，秒级，无库
② 集成档（每次 PR）:  起 Postgres service + nextest run           # 含 #[sqlx::test]
③ 夜间档（nightly）:  --ignored 慢测 + llvm-cov + cargo-mutants   # 深度、耗时
```

要点：
- **`.sqlx` 离线缓存必须提交**，否则 CI 无库编译失败（Day 1 就要定好，见重写计划风险项）。
- 用 **service 容器**起 Postgres（GitHub Actions `services:` 或 testcontainers）。
- **缓存 `~/.cargo` 与 `target/`** 提速。

---

## 维度十一：本项目落地约定

把上面维度收敛成 tsz-rust 的具体规矩：

1. **分层**：逻辑能在 service 层用 fake 测清就不上集成层；SQL / 事务 / 约束正确性用 `#[sqlx::test]`。
2. **每个有状态模块**（user / session / otp / word）：`service.rs` 内 fake 单测 + `repository.rs` 内契约测试（fake + pg 各跑一次）。
3. **handler** 用 `router.oneshot(req)` 测，不起真服务器；**e2e** 少而精，只覆盖注册→登录→改设置→词表关键链路。
4. **替身**：契约用手写 fake，错误路径注入用 mockall。
5. **数据**：Builder + Object Mother 集中在 `tests/common/`；邮箱 uuid、手机号随机，**禁自增计数器**。
6. **依赖隔离**：DB 用 `#[sqlx::test]`；外部 HTTP（词典 / COCA / 讯飞）用 `wiremock`；时间 / 随机注入替身。
7. **禁在测试里 `std::env::set_var`**：配置测试用 `envy::from_iter` 喂键值对（见 `src/config.rs`）。
8. **运行器** `cargo-nextest`；覆盖率看 **diff 覆盖**；`.sqlx` 缓存提交入库。
9. **CI 三档**：快档（单元）/ 集成档（PR + Postgres）/ 夜间档（慢测 + 覆盖 + 变异）。

---

## 附：常用命令

```bash
cargo test                        # 全部（内置运行器）
cargo nextest run                 # 用 nextest 跑（更快，推荐）
cargo test --lib                  # 只源文件内单元测试
cargo test --test user_flow       # 只 tests/user_flow.rs
cargo test 契约                    # 只跑名字含「契约」的
cargo test -- --ignored           # 慢测试
cargo test -- --nocapture         # 打印 println!
cargo llvm-cov --html             # 覆盖率报告
cargo mutants                     # 变异测试
cargo sqlx prepare                # 生成 .sqlx 离线缓存
```
