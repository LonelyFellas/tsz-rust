# tsz-rust 项目结构规范

> 面向 Rust 新手的后端项目组织指南，给出**推荐做法**而非罗列选项。
> 与 [testing-guide.md](testing-guide.md)（测试怎么写）、[rewrite-plan.md](rewrite-plan.md)（每天做什么）互补。

---

## 0. 一句话结论

**逻辑全放库 crate（`src/lib.rs`），`main.rs` 只留几行启动**；小模块用单文件、大模块用目录 + `mod.rs`；测试按「白盒进模块树、黑盒进 `tests/`」分层。

---

## 1. 最关键的决定：`lib` + 瘦 `main`

Rust 一个 package 里 crate 分两种：

| crate 类型 | 文件 | 能被外部 `use` 导入？ |
|-----------|------|----------------------|
| **库 crate** | `src/lib.rs` | ✅ 能 |
| **二进制 crate** | `src/main.rs` / `src/bin/*.rs` | ❌ 不能 |

### 为什么必须拆

`tests/` 目录里的集成测试是**独立 crate**，它**只能导入库 crate**。
如果所有代码都写在 `main.rs`（二进制）里，那么：

> ❌ 你永远写不了 `tests/` 黑盒集成测试（e2e、真 Postgres 全做不了）。

所以标准做法：**业务逻辑全进 `lib.rs`，`main.rs` 退化成入口壳子**。

```rust
// src/lib.rs —— 真正的项目根，pub 出各模块
pub mod config;
pub mod error;
pub mod platform;
// pub mod user;
// pub mod word;

/// 组装依赖并启动服务的总入口。
pub async fn run() -> anyhow::Result<()> {
    // 读配置 → 建连接池 → 装路由 → serve
    Ok(())
}
```

```rust
// src/main.rs —— 极薄入口，只负责调用 lib
#[tokio::main]
async fn main() {
    // 注意：package 名 tsz-rust，导入时连字符变下划线 → tsz_rust
    if let Err(e) = tsz_rust::run().await {
        eprintln!("启动失败: {e:?}");
        std::process::exit(1);
    }
}
```

于是 `tests/` 能这样测：
```rust
// tests/config_smoke.rs
use tsz_rust::config::Config;   // 只有 lib crate 才导得进来
```

**这是后端项目的地基，越早拆成本越低。**

---

## 2. 推荐目录结构

> 下面是**推荐范式**。标 `（规划）` 的是后续再落地的形状，当前仓库尚无（见结尾「当前实况」）。

```
tsz-rust/
├── Cargo.toml
├── src/
│   ├── lib.rs              # 库根：pub mod 各模块 + router()/run()；健康检查当前内联在此
│   ├── main.rs             # 瘦二进制入口，调 tsz_rust::run()
│   ├── bin/                # 额外可执行文件（规划：migrate / seed）
│   ├── config.rs           # 小模块 = 单文件
│   ├── error.rs            # 统一错误类型
│   ├── auth.rs             # 小模块单文件：TokenManager（JWT 签发/解析）
│   ├── platform/           # 大模块 = 目录
│   │   ├── mod.rs          #   目录模块入口：声明子模块
│   │   └── db.rs           #   PgPool 构建
│   └── user/               # 业务模块（垂直切片）
│       ├── mod.rs
│       ├── model.rs        #   含 value object（DisplayName/Password）+ 内联单测
│       ├── repository.rs   #   具体 UserRepository { pool }，非 trait
│       ├── service.rs
│       ├── handler.rs
│       └── display_name.rs #   默认昵称词表 + 生成器
├── tests/                  # 黑盒集成 / e2e（独立 crate，import tsz_rust::）
│   ├── *_schema.rs         # 各表约束测试（#[sqlx::test] 打真库）
│   ├── user_repository.rs / user_service.rs / user_register_handler.rs
│   └── health.rs
├── migrations/             # sqlx 迁移 .sql（up/down 成对）
└── docs/
```

一个 package 只能有 **1 个 `lib.rs`**，但可有 **多个二进制**（`main.rs` + `bin/*.rs`）——将来的
`migrate`、`seed` 会这么来，它们都 `use tsz_rust::...` 复用库逻辑。

> **当前实况**：`auth.rs`/`config.rs`/`error.rs` 是单文件；`platform/` 下只有 `db.rs`，健康检查
> `/healthz` `/readyz` 内联在 `lib.rs`（尚无 `platform/http/` 子树）；`bin/`、`session/`、`otp/`、
> `word/` 均未建。白盒测试目前走**同文件内联** `#[cfg(test)]`（如 `model.rs`），暂未拆 `tests.rs`。

---

## 3. 模块拆文件的规矩（新手最容易懵）

Rust 的模块树是**显式声明**的：父级写 `mod xxx;`，编译器才会去找对应文件。不像有些语言按目录自动加载。

| 情况 | 怎么放 | 声明方式 |
|------|--------|---------|
| 小模块（无子模块） | 单文件 `config.rs` | 父级 `pub mod config;` |
| 大模块（有子模块） | 目录 `user/` + `user/mod.rs` | 父级 `pub mod user;`；`user/mod.rs` 内 `pub mod service;` |
| 子模块文件 | `user/service.rs` | `user/mod.rs` 里 `pub mod service;` |

要点：
- **`mod` 声明在哪，模块就挂在哪**。漏写 `mod xxx;` 文件就不会被编译（新手常见「我建了文件怎么没生效」）。
- `foo.rs` 的子模块放在**同名目录** `foo/` 下（如 `platform.rs` 的子模块在 `platform/`）。用 `platform/mod.rs` 当目录入口更直观，二选一保持一致。
- **可见性靠 `pub`**：不写 `pub` 默认私有，只有本模块和子模块能用。对外要用的类型/函数才 `pub`。

---

## 4. 命名约定

| 对象 | 规范 | 例 |
|------|------|-----|
| 文件 / 模块 | `snake_case` | `user_profile.rs`，不是 `UserProfile.rs` |
| 类型（struct/enum/trait） | `UpperCamelCase` | `AppError`、`UserRepository` |
| 函数 / 变量 / 字段 | `snake_case` | `load_config`、`database_url` |
| 常量 / 静态 | `SCREAMING_SNAKE_CASE` | `DEFAULT_PORT` |
| crate 导入名 | 连字符转下划线 | `tsz-rust` → `tsz_rust` |

> 标识符一律英文，注释用中文（见 testing-guide 命名约定）。

---

## 5. 测试放哪（结合 lib/bin）

| 测试类型 | 位置 | 能测私有？ | 何时用 |
|---------|------|-----------|--------|
| **白盒·小模块** | 同文件底部 `#[cfg(test)] mod tests { }` | ✅ | 模块小（如 `config.rs`） |
| **白盒·大模块** | 兄弟文件 `user/tests.rs` + 父级 `#[cfg(test)] mod tests;` | ✅ | 测试变长、遮住实现 |
| **黑盒·集成/e2e** | `tests/*.rs`，`use tsz_rust::...` | ❌ 只 `pub` | 跨模块、真 DB、对外 API |

白盒大模块拆兄弟文件的写法：
```rust
// src/user/mod.rs 或 service.rs 底部，只留一行
#[cfg(test)]
mod tests;              // 指向 src/user/tests.rs
```
```rust
// src/user/tests.rs —— 物理独立，仍是子模块，照样能访问私有项
use super::*;
```

`#[cfg(test)]` 的代码**只在 `cargo test` 时编译，不进生产二进制**——所以白盒测试写在一起零运行时成本。

---

## 6. Cargo 约定速记

| 你想要 | 做什么 |
|--------|--------|
| 加依赖 | `cargo add <crate>`（自动写进 `Cargo.toml`） |
| 只测试用的依赖 | `cargo add --dev <crate>` → `[dev-dependencies]` |
| 跑主程序 | `cargo run`（跑 `main.rs`） |
| 跑指定二进制 | `cargo run --bin migrate` |
| 跑测试 | `cargo test` |
| 查而不编译产物 | `cargo check`（快，日常写代码用它） |
| 格式化 / lint | `cargo fmt` / `cargo clippy` |

---

## 7. 给 tsz-rust 的落地顺序

1. **先把 `main.rs` 拆成 `lib.rs` + 瘦 `main.rs`**（当前最该做的结构调整）。
2. 小模块单文件（`config.rs`、`error.rs`），大模块目录 + `mod.rs`（`platform/`、`user/`）。
3. 每个业务模块内垂直切片：`model` / `repository` / `service` / `handler`（对标 tsz-go）。
4. 测试按第 5 节三层放：`config` 内联，`user`/`word` 拆 `tests.rs`，e2e 进 `tests/`。
