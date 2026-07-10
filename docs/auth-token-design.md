# auth 域 access token 设计（`TokenManager`）

> **用途**：给你审核用的设计定稿依据。定的是**无状态 access token 的签发/校验**——`src/auth.rs` 一个文件、无 DB、无仓储。
> **不含**：refresh token（属 session 域，碰 `refresh_tokens` 表，另设计）；登录流程本身（login handler 调本模块签 token，不在这里）。
> **对标**：tsz-go `internal/auth/token.go`。凡「go 这么写、Rust 应改」的地方，本文档标了 ⚠️ 并给理由——请重点审这些。

---

## 0. 范围与非目标

| 做 | 不做 |
|---|---|
| 签发一枚 HS256 JWT（sub + realm + role + iat + exp） | refresh token（不透明随机串 + DB，session 域） |
| 校验并解出 `Claims`（验签 + 算法白名单 + 过期 + realm） | token 吊销/黑名单（access token 无状态、短命，靠 TTL 自然过期，不做服务端存储） |
| 按 realm 隔离（web / admin 互不通用） | 中间件/提取器（`FromRequestParts` 那层是 handler 侧的事，本模块只提供 `parse`） |

**核心设计前提**（沿用 go，建议保留）：access token 是**无状态**的，中间件本地验签即可，不查库。代价是「禁用账号/登出」不会立刻杀死已签发的 access token，而是在其 TTL 内仍有效、靠下次 refresh 拦截。所以 **access token TTL 要短**（下详）。

---

## 1. 关键决策速览

| # | 决策 | 取值 | 理由 / ⚠️与 go 的差异 |
|---|---|---|---|
| 1 | 签名算法 | **HS256**（对称 HMAC） | 单体服务、签发方=验证方，对称够用且最简。见 §2 |
| 2 | realm 建模 | ⚠️ **`Realm` enum**，不是字符串常量 | go 用 `const RealmWeb="web"`；Rust 用枚举，类型即约束，杜绝拼错 realm。见 §4 |
| 3 | realm 在 token 里的位置 | ⚠️ 用**标准 `aud`（audience）声明**承载，交给库校验 | go 塞自定义 `realm` claim 手动 `!=` 比对；`aud` 是 JWT 标准里「这 token 发给谁」的位，语义正好，且 `jsonwebtoken` 能自动校验。见 §4 |
| 4 | realm 隔离防线 | **双重**：per-realm 独立 secret（主）+ `aud` 校验（次） | 主防线是「web 的 secret 签的 token 在 admin 上验签直接失败」；`aud` 是纵深防御。见 §4 |
| 5 | access token TTL | **15 分钟**（可配） | 主流区间 5–30min。短 TTL 是「无状态不可吊销」的补偿 |
| 6 | 错误粒度 | ⚠️ **区分 `Expired` 与 `Invalid`** | 与 OTP「错误不可区分」相反：token 是客户端自己持有的凭证，`Expired` 要让客户端据以触发 refresh。见 §7 |
| 7 | 时钟偏移容忍 | `leeway = 60s` | 跨机器时钟漂移，主流做法 |

---

## 2. 算法选型：为什么 HS256

| 方案 | 何时用 | 本项目 |
|---|---|---|
| **HS256**（对称，HMAC-SHA256） | 签发方和验证方是**同一方**（能共享密钥） | ✅ 单体，签发=验证，选它 |
| RS256 / ES256 / EdDSA（非对称） | 验证方≠签发方（多服务、第三方要验但不能签），公钥可公开分发 | ❌ 现在没这需求；将来真拆微服务、要让别的服务验签而不给签发权时再换 |

**结论**：HS256。密钥就是 config 里的 `jwt_secret`（每 realm 一把，见 §8）。

⚠️ **算法混淆攻击面**（务必在 `parse` 落实，§6）：JWT 的经典漏洞是攻击者把 header 的 `alg` 改成 `none`（无签名）或把 HS256/RS256 混用。防法是**校验时锁定算法白名单**，只认 `HS256`，绝不信 token header 自报的 `alg`。

---

## 3. Claims 结构

线上（JWT payload）字段，尽量用**标准注册声明**，少造自定义：

| claim | 标准? | 内容 | 说明 |
|---|---|---|---|
| `sub` | 标准 | user id（UUID 字符串） | subject = 主体 |
| `aud` | 标准 | realm（`"web"` / `"admin"`） | ⚠️ 用 audience 承载 realm，库自动校验（§4） |
| `iat` | 标准 | 签发时刻（unix 秒） | |
| `exp` | 标准 | 过期时刻（unix 秒）= iat + ttl | 库自动校验过期 |
| `role` | 自定义 | 当前激活角色（`"student"`/`"teacher"`） | web realm 是激活角色；admin realm 将来放 level。唯一的自定义声明 |

**解出来给调用方的领域结构**（不是线上结构）：

```rust
pub struct Claims {
    pub subject: Uuid,   // sub 解析成 UUID
    pub realm: Realm,    // aud 解析成枚举
    pub role: String,    // 先按 String 透出，映射成 UserRole 交给调用方（见开放问题 Q2）
}
```

**开放问题 Q1**：要不要加 `jti`（token 唯一 id）？access token 不做黑名单就用不上，我倾向**先不加**，YAGNI。你定。

---

## 4. realm 隔离（本设计的安全核心）

需求：**web 用户的 token 绝不能通过 admin API 的校验，反之亦然**。两道防线：

1. **主防线——每 realm 独立 secret**。`TokenManager` 按 realm 构造，持该 realm 自己的 secret。web 的 secret 签出来的 token，拿到 admin 的 manager 上验签，**HMAC 直接对不上 → 验签失败**。这是最硬的一道，纯密码学保证。
2. **纵深防御——`aud` 校验**。即便将来两 realm 误配成同一个 secret，`aud` 声明仍标着这 token「发给 web」，admin manager 校验 `aud == "admin"` 不匹配 → 拒。

⚠️ **与 go 的差异**：go 手写 `if claims.Realm != m.realm { reject }`。Rust 里把 realm 放进标准 `aud`，`jsonwebtoken` 的 `Validation::set_audience(&[realm])` 会**自动**做这道校验，不用手写比对——更少出错、更贴标准。

⚠️ **配置现状（已知缺口，见 [tsz-rust-project-state] memory 缺口②）**：`config.rs` 现在只有**单个** `jwt_secret`。**只做 web realm 时够用**；等做 admin realm，必须先给 config 加**第二把** secret（如 `admin_jwt_secret`），否则「独立 secret」这道主防线不成立，只剩 `aud` 一道弱防御。**本次只实现 web realm，但 `TokenManager` 的构造签名要从一开始就带 realm + 该 realm 的 secret**，别写成读全局单例——否则加 admin 要返工。

```rust
pub enum Realm { Web, Admin }   // 不用字符串常量

impl Realm {
    fn as_aud(&self) -> &'static str { match self { Realm::Web => "web", Realm::Admin => "admin" } }
}
```

---

## 5. 签发：`generate`

```rust
/// 给 subject 签一枚本 realm 的 access token，携带 role。
/// 失败仅可能是序列化/签名内部错（正常路径不会失败）。
pub fn generate(&self, subject: Uuid, role: &str) -> Result<String, TokenError>;
```

内部逻辑（伪）：
1. `now = Utc::now()`；`iat = now`，`exp = now + self.ttl`。
2. 组装 payload：`sub=subject`、`aud=self.realm.as_aud()`、`iat`、`exp`、`role`。
3. `jsonwebtoken::encode(&Header::new(HS256), &payload, &self.encoding_key)`。

**注意**：`Header` 固定 `HS256`，不接受外部传入算法。

---

## 6. 校验：`parse`（攻击面都在这）

```rust
/// 校验一枚 token 并解出 Claims。任何不通过都返回 TokenError，绝不 panic。
pub fn parse(&self, token: &str) -> Result<Claims, TokenError>;
```

**校验清单**（多数由 `jsonwebtoken::Validation` 声明式完成，不手写）：

| 检查 | 怎么做 | 防的是 |
|---|---|---|
| 算法必须 HS256 | `Validation::new(Algorithm::HS256)`（**只信这个，不信 token 自报的 alg**） | `alg:none` / 算法混淆攻击 |
| 验签 | `decode` 用本 realm 的 `DecodingKey` | 伪造/篡改 token |
| 过期 | Validation 默认校验 `exp` + `leeway=60s` | 过期 token 复用 |
| realm | `Validation::set_audience(&[self.realm.as_aud()])` | 跨 realm 越权 |
| `sub` 必存在且是 UUID | Validation 要求 `sub` 必填 + 解出后 `Uuid::parse` | 缺主体 / 脏数据 |

**要点**：`Validation` 在 `TokenManager::new` 时**构造一次并存起来**，别每次 parse 重建。

---

## 7. 错误类型：为何 token 可区分而 OTP 不可

```rust
#[derive(Debug, thiserror::Error)]
pub enum TokenError {
    #[error("token expired")]
    Expired,                 // 只是过期 → 客户端据此触发 refresh
    #[error("invalid token")]
    Invalid,                 // 验签失败/篡改/realm 不符/格式坏 → 一律笼统，不细分
    #[error("failed to sign token")]
    Signing(#[source] jsonwebtoken::errors::Error),  // 签发侧内部错
}
```

⚠️ **与 OTP 的「错误不可区分」原则相反，且是故意的**：
- OTP 里「码错/过期/已用/号没注册」全返回同一个错，因为**攻击者不该借错误探测某手机号是否注册**。
- token 是**客户端自己持有**的凭证，`Expired` 不泄露任何他人隐私，反而是必要信号——客户端拿到 `Expired` 才知道该去 `/auth/refresh` 换新，而不是让用户重新登录。
- 但 `Invalid` 内部的多种原因（验签错 / realm 错 / 结构坏）**要笼统成一个**，不给攻击者反馈他哪一步「差一点」。

映射：把 `jsonwebtoken` 的 `ErrorKind::ExpiredSignature` → `Expired`，其余全部 → `Invalid`。

---

## 8. 配置

| 项 | 值 | 来源 |
|---|---|---|
| web secret | 现 `config.jwt_secret` | 已有 |
| admin secret | ⚠️ 待加 `admin_jwt_secret` | 做 admin realm 前必加（§4） |
| access TTL | 15min，建议也进 config（`access_token_ttl_secs`，默认 900） | 待加，或先硬编码常量 |

**建议**：TTL 先用 `src/auth.rs` 里的 `const ACCESS_TTL: Duration` 常量起步，等真要按环境调再挪进 config，避免过早配置化。

---

## 9. Rust 实现骨架（供你填实现，非完整代码）

```rust
// src/auth.rs
use chrono::Duration;
use jsonwebtoken::{DecodingKey, EncodingKey, Validation};
use uuid::Uuid;

pub enum Realm { Web, Admin }

pub struct Claims { pub subject: Uuid, pub realm: Realm, pub role: String }

pub enum TokenError { Expired, Invalid, Signing(/* ... */) }

/// 绑定到**单个 realm** 的签发/验证器。每 realm 用各自 secret 构造一个实例。
pub struct TokenManager {
    encoding_key: EncodingKey,   // new 时从 secret 预建
    decoding_key: DecodingKey,   // 同上
    validation: Validation,      // 预建：algo=HS256 + aud=realm + require sub
    realm: Realm,
    ttl: Duration,
}

impl TokenManager {
    pub fn new(secret: &str, realm: Realm, ttl: Duration) -> Self { /* 预建 key + validation */ }
    pub fn generate(&self, subject: Uuid, role: &str) -> Result<String, TokenError> { todo!() }
    pub fn parse(&self, token: &str) -> Result<Claims, TokenError> { todo!() }
}
```

私有的线上 payload 结构（serde）与领域 `Claims` 分开：

```rust
#[derive(Serialize, Deserialize)]
struct TokenPayload {
    sub: String,
    aud: String,
    role: String,
    iat: i64,
    exp: i64,
}
```

---

## 10. 可测性接缝（我写测试要用）

难点是**过期测试**：`exp` 是相对 `now` 算的，不能 sleep 15 分钟。给一个内部签发接缝，让测试能签一枚「签发时刻可控」的 token：

```rust
// 生产 generate() 内部调它，传 Utc::now()；测试传任意 issued_at（如 25 小时前）造过期 token。
#[cfg(test)] // 或 pub(crate)，看你
fn generate_at(&self, subject: Uuid, role: &str, issued_at: DateTime<Utc>) -> Result<String, TokenError>;
```

有了它，我能覆盖的验签回环：
- 签发→解析回环，`Claims` 三字段都对；
- **过期**：`generate_at` 签一枚 `issued_at` 早于 `now - ttl - leeway` 的 → `parse` 返 `Expired`；
- **跨 secret**：A secret 签、B secret 验 → `Invalid`；
- **跨 realm**：web manager 签、admin manager 验 → `Invalid`；
- **篡改**：改 payload 一个字节 → `Invalid`；
- **`alg:none`**：手拼一枚 `alg=none` 的 token → `Invalid`（防混淆攻击的回归测试）。

---

## 11. 依赖

```toml
# Cargo.toml，需新增
jsonwebtoken = "9"   # 锁定前 cargo add 确认最新补丁版
```

`chrono` / `uuid` / `serde` / `thiserror` 已在依赖里，无需加。`jsonwebtoken` 自带 HMAC 实现，不用额外引 `hmac`/`sha2`。

---

## 12. 待你审核的开放问题

- **Q1**：加不加 `jti`？（我倾向不加——不做黑名单就没用）
- **Q2**：`Claims.role` 透出 `String` 还是解析成 `UserRole` enum？（web realm 是 UserRole，但 admin realm 将来是 level 字符串，用 String 更通用；你定）
- **Q3**：realm 用标准 `aud`（我推荐，库自动校验）还是照 go 用自定义 `realm` claim 手动比对？
- **Q4**：TTL 先常量还是直接进 config？（我倾向先常量）
- **Q5**：`generate_at` 接缝用 `#[cfg(test)]`（仅测试可见）还是 `pub(crate)`（将来可能复用）？

审完这几个我就可以按定稿写验签测试；实现由你来。
```
