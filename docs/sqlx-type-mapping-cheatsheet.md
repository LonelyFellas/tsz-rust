# sqlx 0.9 枚举 ↔ TEXT / 类型覆盖 / 时间戳 小抄

> 用途：写 `user/model.rs`、`user/repository.rs` 前先看这一份，把 sqlx 把 Rust 类型对上 Postgres 列时**最容易卡编译期/运行期的坑**一次讲清。
> 你的 schema 里枚举全是 `TEXT + CHECK`（DB 端约束），Rust 侧用 `enum` 映射——这份就是教你怎么正确映射。
>
> **全部事实针对 sqlx 0.9.0，且已在本仓库真库（`localhost:5433/tsz_rust`）上「编译 + 运行时回读」双重验证**，不是照抄旧版博客。验证来源见文末。

---

## 0. 一句话结论 + 速用配方

映射一个 Rust 单元枚举到 Postgres **TEXT** 列，最小正确写法：

```rust
#[derive(sqlx::Type)]
#[sqlx(type_name = "text", rename_all = "lowercase")]  // ← type_name 必填！
enum UserStatus { Active, Disabled }                   // 存/读 'active' / 'disabled'
```

读取时在 SQL 里用**类型覆盖**告诉宏用你的枚举（否则宏会把 TEXT 推断成 `String`）：

```rust
sqlx::query!(r#"SELECT status AS "status: UserStatus" FROM users"#)
```

你 user 域这 5 个枚举列的确切写法，直接抄：

| 列 | DB 值 | Rust 枚举 | 需要的属性 |
|----|-------|-----------|-----------|
| `users.status` | `active` / `disabled` | `UserStatus { Active, Disabled }` | `type_name="text"` + `rename_all="lowercase"` |
| `users.last_active_role` | `student` / `teacher`（可空） | `Role { Student, Teacher }` | `type_name="text"` + `rename_all="lowercase"` |
| `user_roles.role` | `student` / `teacher` | 复用上面的 `Role` | 同上 |
| `student_profiles.cefr_level` | `A1`…`C2`（可空） | `CefrLevel { A1, A2, B1, B2, C1, C2 }` | **只需** `type_name="text"` |
| `student_profiles.english_variant` | `BrE` / `AmE`（可空） | `EnglishVariant { BrE, AmE }` | **只需** `type_name="text"` |

> 为什么 `cefr_level` / `english_variant` **不加** `rename_all`：它们的变体标识符（`A1`、`BrE`）**本身就等于 DB 存的字符串**。`status`/`role` 要加 `rename_all="lowercase"` 是因为 Rust 变体 `Active` ≠ DB 值 `active`。给 `EnglishVariant` 加 `rename_all` 反而会**弄坏**它（会去找 `bre`/`ame`）。

---

## 1. ⚠️ 最大的坑：编译通过 ≠ 运行正确

这是本文档最重要的一条，先看：

**`AS "col: MyEnum"` 类型覆盖会让 `query!` 宏「相信你的 Rust 类型」并跳过它自己的编译期类型检查。** 所以：

- 缺 `type_name`、或者 `rename_all` 写错——**`cargo check` 照样通过**，给你一种「对了」的错觉。
- 但 sqlx 在**运行时 decode** 时仍会跑 `Type::compatible()` 和值匹配，这时才炸。

两个真实炸点（都在你的库上复现过）：

```rust
// ❌ 少了 type_name：cargo check 通过，运行时报：
//    mismatched types; Rust type `UserStatus` (as SQL type `UserStatus`)
//    is not compatible with SQL type `TEXT`
#[derive(sqlx::Type)]
enum UserStatus { Active, Disabled }
```
> 原因：不写 `type_name`，枚举默认把自己的 SQL 类型名报成 **Rust 枚举名**（`"UserStatus"`），和真列的类型名 `text` 对不上。

```rust
// ❌ 有 type_name 但少了 rename_all：cargo check 通过，运行时报：
//    invalid value "active" for enum UserStatusNoRename
#[derive(sqlx::Type)]
#[sqlx(type_name = "text")]
enum UserStatusNoRename { Active, Disabled }   // 默认编码成 'Active'，DB 里是 'active'
```

**结论：验证一个新 TEXT 枚举，绝不能只 `cargo check`，一定要跑一次真数据回读**（`fetch_one`/`fetch_all`）。这也是为什么我们的契约测试要连真库——正好把这类运行时 decode 错误网住。

---

## 2. 核心机制（理解一次，以后不慌）

- `#[derive(sqlx::Type)]` 且**不带** `#[repr(...)]` 的单元枚举 = sqlx 说的「strong enum」，按**变体名字符串**（`&str`）编解码。
  - 一个 derive 同时生成 `Type` + `Encode` + `Decode` 三个 impl，你**不用**再单独 `#[derive(Encode, Decode)]`。
- `type_name` 决定「这个枚举报给 sqlx 的 SQL 类型名」，用于兼容性检查：
  - 对 **TEXT 列**：`type_name = "text"`（`"text"`/`"TEXT"` 都行，Postgres 类型名比较大小写不敏感）。
  - 对 **VARCHAR 列**：要写 `"varchar"`，不是 `"text"`（是两个不同的内建类型）。
  - 对 **原生 Postgres ENUM**（`CREATE TYPE xxx AS ENUM(...)`）：写成那个类型名，如 `type_name = "user_status"`。你现在**没用**原生 enum，用的是 TEXT，所以一律 `"text"`。
- `rename_all` / `rename` 决定「变体 ↔ 存储字符串」，和 `type_name` **正交**（各管各的）：
  - `#[sqlx(rename_all = "lowercase")]`（容器级）：`Active` → `'active'`。
  - `#[sqlx(rename = "BrE")]`（变体级，覆盖 `rename_all`）：用于任意大小写混合值。
  - `rename_all` 只接受这 7 个值，写错是编译错误：`lowercase`、`snake_case`、`UPPERCASE`、`SCREAMING_SNAKE_CASE`、`kebab-case`、`camelCase`、`PascalCase`。
- 🚫 **别给 TEXT 枚举加 `#[repr(i32)]`**：一旦有 `#[repr]`，就切到「weak enum」路径，按**整数**存读，不再是字符串。

---

## 3. `query!` / `query_as!` 类型覆盖 + 可空覆盖语法

Postgres 下别名用**双引号**，所以 SQL 字符串用 Rust 原始字符串 `r#"..."#` 最省心。

**覆盖形式（顺序固定：列名 → `!`/`?` → `: 类型`）：**

| 别名写法 | 可空性 | Rust 字段类型 |
|----------|--------|--------------|
| `AS "col: T"` | 沿用推断 | 推断非空→`T`；推断可空→`Option<T>` |
| `AS "col!"` | 强制非空 | `T`（类型仍走推断） |
| `AS "col?"` | 强制可空 | `Option<T>` |
| `AS "col!: T"` | 强制非空 + 指定类型 | `T` |
| `AS "col?: T"` | 强制可空 + 指定类型 | `Option<T>` |

- `query_as!` 支持上面全部形式，**额外**多一个 `AS "col: _"`（从目标 struct 字段类型反推，少见）。
- `query_scalar!`（返回单列单值）也支持，但列名必须是合法 Rust 标识符才能解析。
- 为什么需要类型覆盖：宏默认从列的 SQL 类型推 Rust 类型（TEXT→`String`），**不会**自动用你的枚举，必须显式 `AS "col: MyEnum"`。

---

## 4. 可空性推断规则（决定要不要写 `!`）

sqlx 会**自己**从 DB 的 `NOT NULL` 约束推断可空性（仅限直接来自真实表的列）：

- `status`（NOT NULL）→ 直接 `UserStatus`，**不用**加 `!`。
- `last_active_role` / `phone`（可空）→ `Option<...>`。
- **表达式列会被保守当成可空** → 给 `Option<T>`，即使它永不为 NULL。典型：`COUNT(*)`、`COALESCE(...)`、函数调用、字面量 `SELECT 1`、`now()`、`LEFT JOIN` 右侧列。这时用 `AS "col!"` / `AS "col!: T"` 强制成 `T`。

一句话：`!` 是**用来纠正推断**的，不是每次都要写。NOT NULL 的真实列不用写。

> ⚠️ sqlx 0.9 改过 Postgres 可空性推断算法（#3541「force generic plan」），官方明说「可能改变某些 query! 的输出」。所以别背旧版博客里「某查询字段是 `Option` 还是 `T`」——以 0.9 连真库编译出来的为准（编译错误 E0308 反而会直接告诉你推断出的类型）。

---

## 5. 时间戳：要先补 `chrono` feature（接上之前那个地基缺口）

你表里全是 `TIMESTAMPTZ`。**不开 date/time feature，选任何时间戳列会直接编译失败：**

```
error: SQLx feature `time` required for type TIMESTAMPTZ of column #1 ("created_at")
```
（提示里写的是 `time`，但开 `chrono` 一样满足。）

补法（`Cargo.toml`）：

```toml
sqlx = { version = "0.9.0", features = [
    "runtime-tokio", "tls-rustls", "postgres", "macros", "uuid", "migrate",
    "chrono",                       # ← 新增
], default-features = false }
chrono = { version = "0.4", features = ["serde"] }   # cargo add chrono --features serde
```

映射（已验证）：

| DB 列 | Rust 类型 |
|-------|-----------|
| `created_at` TIMESTAMPTZ NOT NULL | `chrono::DateTime<chrono::Utc>` |
| 可空 timestamptz（如 `revoked_at`/`consumed_at`） | `Option<chrono::DateTime<chrono::Utc>>` |

> `chrono` 和 `time` **不互斥**，但两个都开时宏会对时间列该生成哪种类型犯迷糊，需要 `sqlx.toml` 的 `preferred-crates` 消歧。**只开 `chrono` 一个就没这问题**——推荐只开 chrono。

---

## 6. 未知值 / 错误处理

DB 里出现枚举里没有的 TEXT 值（脏数据、遗留行、CHECK 之外）时：

- **不会 panic**，返回 `sqlx::Error::Decode`，消息形如 `invalid value "banana" for enum UserStatus`。
- 一旦某行的某列 decode 失败，**整条 query 失败**。
- 值匹配是**大小写敏感**的（`'Active'` ≠ `'active'`）。

> DB 的 `CHECK (status IN ('active','disabled'))` 和 Rust 的 `enum UserStatus` 是**两道独立的闸**。加/删枚举值时两边都要改，否则要么 DB 拒写、要么 Rust decode 炸。

---

## 7. 一个能跑的完整最小示例（读 `users` 一行）

下面这段**已在真库上 `cargo check` + 运行时回读通过**，可作为 `repository.rs` 里查询的模板：

```rust
use chrono::{DateTime, Utc};

#[derive(Debug, sqlx::Type)]
#[sqlx(type_name = "text", rename_all = "lowercase")]
pub enum UserStatus { Active, Disabled }

#[derive(Debug, sqlx::Type)]
#[sqlx(type_name = "text", rename_all = "lowercase")]
pub enum Role { Student, Teacher }

// query_as! 的目标 struct：字段类型必须和推断出的可空性一致
struct UserRow {
    status: UserStatus,                 // NOT NULL  → 非 Option
    last_active_role: Option<Role>,     // 可空      → Option
    created_at: DateTime<Utc>,          // 需开 chrono feature
}

async fn load(pool: &sqlx::PgPool) -> Result<UserRow, sqlx::Error> {
    sqlx::query_as!(
        UserRow,
        r#"
        SELECT
            status            AS "status: UserStatus",
            last_active_role  AS "last_active_role: Role",
            created_at
        FROM users
        LIMIT 1
        "#
    )
    .fetch_one(pool)
    .await
}
```

要点回顾：`status` 无 `!`（真列 NOT NULL 自动推非空）；`last_active_role` 落成 `Option<Role>`；`created_at` 不用覆盖，靠 chrono feature 自动成 `DateTime<Utc>`。

---

## 8. sqlx 0.9 其它容易踩的版本坑（旁注）

写实现时可能撞上，顺手记着：

- **运行时 `sqlx::query()` 只收 `&'static str`**（#3723）。用 `format!` 拼出来的动态 SQL 要包一层 `sqlx::query::AssertSqlSafe(s)`。**编译期 `query!`/`query_as!` 宏不受影响**（它们吃字符串字面量）——我们优先用宏，基本碰不到。
- **`#[derive(sqlx::Type)]` 现在会自动给 newtype 生成 `impl PgHasArrayType`**（#4008）。若你手写过 `impl PgHasArrayType`，会「冲突实现」编译报错，删手写的或加 `#[sqlx(no_pg_array)]`。
- MSRV 提到 Rust **1.94.0**。

---

## 9. 验证来源

- sqlx **0.9.0** 源码（本机 cargo registry，Cargo.lock 校验和锁定）：`sqlx-core` 的 `types/mod.rs`（`Type` derive 文档）、`sqlx-macros-core` 的 `derives/{type,encode,decode,attributes,mod}.rs`、`sqlx-postgres` 的 `type_info.rs`（`name_eq` 大小写不敏感）。
- docs.rs/sqlx/0.9.0 的 `macro.query` / `macro.query_as` / `macro.query_scalar` / `postgres/types`。
- sqlx 0.9.0 CHANGELOG（#3541 可空性、#3723 SqlStr、#4008 PgHasArrayType）。
- **本仓库真库实证**：`cargo check`（无 `.sqlx` 缓存，宏直连 `localhost:5433/tsz_rust` 校验）+ `cargo run` 运行时回读 `'active'::text`、`'BrE'::text`、`now()`、`NULL::text` 等，逐条确认编译与运行时行为。

相关文档：[project-structure.md](project-structure.md)、[testing-guide.md](testing-guide.md)、[user-domain-reference.md](user-domain-reference.md)。
