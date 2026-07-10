# session 域 refresh token 设计

> **用途**：给你审核用的设计定稿依据。定的是 **refresh token 的生成 / 存储 / 刷新轮换 / 吊销**——`src/session/`（repository + service），碰 `refresh_tokens` 表。
> **配套**：access token 见 [auth-token-design.md](auth-token-design.md)。本文档负责「兑现」那份文档欠的账——**账号禁用在下次 refresh 时拦截**（§7）。
> **不含**：login handler 本身（它是本域的**调用方**，编排「验密码 → 签 access + 发 refresh」，另做）；第三方/社交登录（只在 §16 留一句未来汇入点，不展开）。
> **对标**：tsz-go `internal/session`。标 ⚠️ 处是「go 那样 / Rust 或本项目该改」，请重点审。

---

## 0. 范围与非目标

| 做 | 不做 |
|---|---|
| 生成高熵不透明 refresh token（非 JWT） | JWT（那是 access token 的事） |
| 存 **SHA-256 哈希**，按哈希 O(1) 查库 | 存明文 / 用 bcrypt 慢哈希（§4 解释为何不） |
| issue（发一枚）/ refresh（轮换）/ revoke（吊销）三动作 | 完整 OAuth authorization server、consent、scope |
| refresh 时复查账号状态、重刷激活角色 | 第三方登录汇入（§16 只留扩展点） |

---

## 1. 关键决策速览

| # | 决策 | 取值 | 理由 / ⚠️差异 |
|---|---|---|---|
| 1 | token 形态 | **不透明随机串**（256-bit CSPRNG，base64url） | 无内部结构、服务端唯一真相是 DB。见 §3 |
| 2 | 存储 | **SHA-256 哈希、不加盐、存 base64url** | 高熵串无需慢哈希/盐；确定性哈希才能按唯一索引 O(1) 查。见 §4 |
| 3 | 刷新模型 | **单次使用 + 轮换**（用一次即作废、发新的） | 滑动续期 + 为盗用检测铺路。见 §6 |
| 4 | 盗用应对（MVP） | 复用已轮换的 token → **拒绝该请求**（不级联吊销） | 避免网络竞态误杀会话；级联吊销留给 Option B（需加列）。见 §6 |
| 5 | refresh TTL | **30 天**（可配），靠轮换滑动续期 | C 端「保持登录」体验；无绝对上限（Option B 再加） |
| 6 | 账号状态检查点 | refresh 时查 `users.status`，禁用则拒发新 access | 兑现 auth 文档「失效在一个 access TTL 内生效」。见 §7 |
| 7 | 对客户端的错误 | 一律笼统 `invalid refresh token` | 不泄露「过期/已用/被吊销/不存在」之别。见 §9 |
| 8 | 响应契约 | 仿 OAuth token response：`{access_token, refresh_token, token_type, expires_in}` | 标准形状，将来接第三方也一致。见 §10 |
| 9 | 传输 | MVP 走 **响应 body**（移动端安全存储） | web 加固可改 httpOnly cookie，属客户端取舍。见 §11 |

---

## 2. 为什么 refresh 与 access 是两种东西（速记）

access 无状态、短命、每请求都带、不可吊销；refresh 有状态、长命、只发给 `/auth/refresh`、可吊销。两者各优化相反目标（性能 vs 可控）。详见概念讨论，此处不展开。**本域只管 refresh 这枚有状态的。**

---

## 3. token 生成：熵与编码

```
明文 refresh token = base64url( 32 字节 CSPRNG 随机 )   // 43 字符，无 padding
```

- **熵**：256 bit。不透明，不含任何用户信息（和 access token 相反）。
- ⚠️ **必须用 CSPRNG**：用 `rand::rngs::OsRng`（OS 熵源），**不要**用 `rand::rng()`/`ThreadRng` 那类——注意 `generate_display_name` 用的是后者，那是非安全场景没问题，**但 token 是安全凭证，必须 OsRng**。这是本域和昵称生成的关键区别。
- **编码**：base64url（URL-safe、无 `+/=`），方便放 JSON/header/cookie。
- 明文**只在生成的那一刻返回给客户端一次**，服务端从不留明文（只留哈希）。

---

## 4. 存储：SHA-256 哈希（为什么不 bcrypt、不加盐）

`refresh_tokens.token_hash` 存的是 **`base64url( SHA-256( 明文token的字节 ) )`**。

⚠️ **这里和密码的哈希策略故意相反，务必理解**：

| | 密码 | refresh token |
|---|---|---|
| 熵 | 低（人选的，可枚举/字典攻击） | 高（256-bit 随机，不可枚举） |
| 哈希 | **bcrypt/argon2**（慢，抗暴力） | **SHA-256**（快，够用） |
| 盐 | 每行随机盐（防彩虹表 + 防同密码撞哈希） | **不加盐** |
| 查找 | 先按标识查行、再 verify | **直接按 token_hash 唯一索引查** |

- **为什么 SHA-256 够**：token 有 256-bit 熵，攻击者无从枚举，不需要「慢」来抗暴力。慢哈希在这纯属浪费——refresh 校验在热路径之外但也没必要慢。
- **为什么不加盐**：加盐哈希不确定 → 没法按哈希直接查行（得逐行 verify，O(n)）。token 高熵，不加盐也没有彩虹表/撞库风险，所以**用确定性 SHA-256 换来按唯一索引 O(1) 查**（表里 `refresh_tokens_hash` 唯一索引正是为此）。
- **为什么存哈希不存明文**：DB 被脱库时，攻击者拿到的是哈希，反推不出明文 token，无法冒用。

**查库流程**：客户端发明文 `S` → 服务端算 `h = base64url(sha256(S))` → `SELECT ... WHERE token_hash = h`（命中唯一索引）→ 再查 revoked/rotated/expired。

**依赖**：需加 `sha2`（哈希）+ `base64`（编码）。见 §15。

---

## 5. 三个动作的边界

| 动作 | 谁调 | 干什么 | 碰的列 |
|---|---|---|---|
| **issue** | login（验密码后） | 生成明文 → 存哈希行 → 返回明文 | INSERT：id/user_id/token_hash/expires_at |
| **refresh** | `/auth/refresh` | 校验旧 token → 轮换 → 发新 access+refresh | 读全表；旧行 set `rotated_at`；INSERT 新行 |
| **revoke** | `/auth/logout`、改密、禁用 | 硬吊销 | set `revoked_at`（单条或按 user_id 批量） |

**login 只需要 issue 这半**——所以「session 的 issue 半 → login」可以先落地，refresh/revoke 端点随后补（见 [tsz-rust-project-state] memory 的推进顺序）。

---

## 6. 轮换与盗用检测（本设计最微妙处，重点审）

**单次使用 + 轮换**：refresh token 用一次即作废（`rotated_at = now`），同时发一枚**新的** refresh token。客户端下次必须用新的。好处：① 滑动续期（每次刷新续 30 天）；② 为「盗用检测」铺路——一枚**已轮换**的旧 token 又被人拿来用，说明它可能被偷了。

**校验判定（refresh 时，按顺序）**：

```
h = sha256(明文)
row = SELECT WHERE token_hash = h
1) 无 row            → 拒（invalid）
2) row.revoked_at 非空 → 拒（invalid）
3) row.rotated_at 非空 → 【复用！】见下方策略
4) row.expires_at 过期 → 拒（invalid）
5) 否则 → 加载 user、查 status（§7）→ 旧行 set rotated_at → INSERT 新行 → 返回新 access+refresh
```

⚠️ **第 3 步「复用已轮换 token」怎么处理——这是要你拍板的核心决策**：

| | Option A（MVP，推荐先做） | Option B（加固，需改表） |
|---|---|---|
| 复用旧 token | **只拒绝该请求**，不动其它 | **级联吊销**该用户/该会话所有 refresh（判定被盗，全登出） |
| 网络竞态（客户端没收到新 token、拿旧的重试） | 只是这次失败 → 客户端回退重登，代价小 | **会误杀**：竞态被当成盗用，用户被全设备登出 |
| 需要的表结构 | 现表够用 | 需加 `session_id`/`family_id`（同源链）+ 可能 `replaced_by`（幂等宽限） |
| 安全强度 | 够（旧 token 失效，攻击者拿不到新的） | 更强（主动检测 + 止血） |

- **我的建议**：**MVP 走 Option A**——单次使用 + 复用即拒，**不**级联吊销、**不**做宽限窗口。理由：现表结构够用、无误杀、实现干净；旧 token 一经轮换即失效，攻击者拿到旧 token 也换不出新的，安全下限已经守住。
- ⚠️ **表的现状**：`refresh_tokens` 只有 `rotated_at`，**没有** `session_id`/`family_id`/`replaced_by`。所以「级联吊销整条链」和「宽限窗口内幂等返回同一枚新 token」**现在做不了**——真要 Option B 得先加迁移。表注释里「宽限窗口逻辑在 service 层（先留列）」这句其实**兑现不了**（缺同源链），审的时候留意：**要么接受 Option A，要么先补一列**。

---

## 7. 账号状态复查（兑现 auth 文档欠的账）

auth 文档 §0 说「禁用账号/登出不立刻杀 access token，而在**下次 refresh** 拦截」。这笔账在本域第 5 步兑现：

- refresh 成功轮换**之前**，加载 `users` 行，检查 `status == 'active'`；`disabled` → 拒发新 access（并可顺手 revoke 该 refresh）。
- 同时**重读 `last_active_role`**，把最新激活角色刷进新 access token 的 `role` claim——这样用户切换角色后，最多一个 access TTL 就生效。

**这是 refresh 必须查一次库的正当理由**（access 校验不查库，但 refresh 本就是冷路径、且要落库轮换，顺带查 status 成本可忽略）。

---

## 8. TTL 与滑动过期

| token | TTL | 过期方式 |
|---|---|---|
| access | 15min（auth 文档定） | 绝对，过期即须 refresh |
| refresh | **30 天**（建议进 config，如 `refresh_token_ttl_days`） | **滑动**：每次 refresh 发新的、续 30 天 |

- **滑动续期**由轮换天然实现：活跃用户一直刷、一直续；停用 30 天的 token 自然过期。
- ⚠️ **无绝对会话上限**：滑动意味着长期活跃用户可以无限续。要「强制每 90 天重登一次」需 `family created_at` 记会话起点——同样属 Option B，MVP 不做。
- issue 时 `expires_at = now + refresh_ttl`，存绝对时刻（不存 TTL）。

---

## 9. 错误与不可区分

**对客户端**：无 row / 过期 / 已轮换 / 被吊销，**一律返回同一个** `invalid refresh token`（401）——不泄露 token 处于哪种失效态。

**对内部**：service 内部要能区分（尤其「已轮换」这态，Option B 下要触发级联）。所以领域错误内部分明、到边界（handler → `AppError`）时**收敛成一个对外文案**。

```rust
// session 域内部错误（对内分明）
pub enum RefreshError {
    NotFound,       // 无匹配 row
    Expired,
    Revoked,
    Reused,         // 命中已 rotated 的行（Option B 的级联入口）
    AccountDisabled,
    Repository(/* sqlx 等 */),
}
// handler 侧：NotFound/Expired/Revoked/Reused → 统一 401 "invalid refresh token"；
//            AccountDisabled → 403（或也归 401，看产品）；Repository → 500。
```

---

## 10. 响应契约（login 与 refresh 共用）

仿 OAuth 2.0 token response（RFC 6749 §5.1）的形状，即便是第一方也照此，将来接第三方一致：

```json
{
  "access_token":  "<JWT>",
  "refresh_token": "<opaque base64url>",
  "token_type":    "Bearer",
  "expires_in":    900          // access token 剩余秒数
}
```

- `expires_in` 指 **access** 的寿命（客户端据此在过期前预刷）。refresh 的过期不下发（不透明，客户端不该解析它）。

---

## 11. 传输与客户端存放

| 载体 | 适用 | 权衡 |
|---|---|---|
| **响应 body（JSON）** | MVP、移动端 | 客户端存安全存储（Keychain/Keystore）；实现简单 |
| httpOnly + Secure cookie | web 加固 | 防 XSS 偷 token，但引入 CSRF，需配 SameSite/CSRF token |

- **MVP 走 body**（背单词产品移动端为主）。cookie 加固是客户端/web 的取舍，服务端设计不被它绑死——本域只管「发一个字符串出去」，放 body 还是 Set-Cookie 由 handler 决定。

---

## 12. 模块结构

`src/session/`（DB 域，多文件，用目录——判据见 project-structure 惯例）：

```
src/session/
  mod.rs
  model.rs        // RefreshToken 行结构、RefreshError、NewRefreshToken
  repository.rs   // RefreshTokenRepository{pool}：insert / find_by_hash / mark_rotated / revoke / revoke_all_for_user
  service.rs      // SessionService：issue / refresh 编排（含 §6 判定、§7 状态查、token 生成/哈希）
```

- **token 生成 + SHA-256** 放 service（业务），repository 只碰 SQL、收/吐已算好的 `token_hash`。
- 沿用 user 域惯例：repository 是具体 `RefreshTokenRepository{pool}` 固有 async 方法 + `#[sqlx::test]` 真库测，不搞 trait/fake。

---

## 13. Rust 骨架（供你填实现，非完整代码）

```rust
// src/session/service.rs
pub struct SessionService {
    repo: RefreshTokenRepository,
    refresh_ttl: chrono::Duration,
}

pub struct IssuedRefresh {
    pub plaintext: String,          // 只此一次返回给客户端
    pub expires_at: DateTime<Utc>,
}

impl SessionService {
    /// login 调：为 user 发一枚 refresh token（存哈希，返回明文）。
    pub async fn issue(&self, user_id: Uuid) -> Result<IssuedRefresh, RefreshError>;

    /// /auth/refresh 调：校验明文 → 轮换 → 返回新明文 refresh（+ 调用方再签 access）。
    /// 内部走 §6 判定 + §7 状态检查。
    pub async fn rotate(&self, plaintext: &str) -> Result<Rotated, RefreshError>;

    /// /auth/logout 调。
    pub async fn revoke(&self, plaintext: &str) -> Result<(), RefreshError>;
}

// 私有工具
fn generate_plaintext() -> String;        // OsRng 32B → base64url
fn hash_token(plaintext: &str) -> String; // base64url(sha256(bytes))
```

`repository` 侧签名（真库方法）：

```rust
impl RefreshTokenRepository {
    async fn insert(&self, row: NewRefreshToken) -> Result<(), UserError /* 或 SessionError */>;
    async fn find_by_hash(&self, token_hash: &str) -> Result<Option<RefreshToken>, _>;
    async fn mark_rotated(&self, id: Uuid) -> Result<(), _>;
    async fn revoke(&self, id: Uuid) -> Result<(), _>;
    async fn revoke_all_for_user(&self, user_id: Uuid) -> Result<u64, _>;  // 改密/全登出
}
```

⚠️ **rotate 的原子性**：「旧行 set rotated_at + 插新行」应在**一个事务**里（参考 user `create` 已用 `tx`），避免中途崩了留下「旧的作废了、新的没发出」的空洞。

---

## 14. 可测性（我写测试要用）

- **真库测**（`#[sqlx::test]`，仿 `tests/user_service.rs`）。
- **过期不用 sleep**：`insert` 的 `expires_at` 是入参 → 测过期时直接塞一个**过去的**时刻造「已过期行」。这是天然接缝，无需注入时钟。
- 我要覆盖的行为：issue 后能 rotate 出新 token；旧 token rotate 一次后**再用即拒**（Reused）；过期 token 拒；被 revoke 的拒；`revoke_all_for_user` 后该用户所有 token 全失效；rotate 时账号 `disabled` → 拒（AccountDisabled）；哈希存的不是明文（DB 里 `token_hash != plaintext`）。

---

## 15. 依赖

```toml
# Cargo.toml 需新增
sha2   = "0.10"   # SHA-256
base64 = "0.22"   # base64url 编码（token 明文 + 哈希）
# rand 已有（0.10）；用 rand::rngs::OsRng，勿用 ThreadRng
```

`chrono`/`uuid`/`sqlx`/`thiserror` 已在依赖里。

---

## 16. 表字段用法映射 + 未来扩展点

| 列 | 本设计怎么用 |
|---|---|
| `id` | PK，UUIDv7 |
| `user_id` | 属主；`revoke_all_for_user` 靠 `refresh_tokens_user` 索引 |
| `token_hash` | base64url(sha256(明文))，唯一索引，查库入口 |
| `expires_at` | 绝对过期时刻 = issue 时 now + refresh_ttl |
| `revoked_at` | 硬吊销（logout/改密/禁用），非空即失效 |
| `rotated_at` | 轮换消费标记，非空即「已用」→ §6 复用判定 |
| `created_at` | DEFAULT now() |

**未来第三方登录（不实现，仅留位）**：社交登录（微信/Apple）走 OAuth/OIDC，回调拿到第三方身份后，**在 `users` 查/建账号 → 复用本域 `issue` 发 refresh + 签 access**，即汇入同一套 token 体系。若要落地，届时另建 `user_identities`(provider, external_id → user_id)，**不影响本域设计**。

---

## 17. 待你审核的开放问题

- **Q1（最重要）**：§6 复用策略选 **Option A**（拒绝即可、不级联，现表够用）还是现在就上 **Option B**（级联吊销 + 宽限，需加 `session_id`/`replaced_by` 迁移）？我推荐 A。
- **Q2**：refresh TTL 30 天进 config 还是先常量？（倾向 config，因它是产品可调参数）
- **Q3**：`AccountDisabled` 对外返 401 还是 403？（403 更语义，但可能泄露「账号存在但被禁」；401 更保守）
- **Q4**：token_hash 存 base64url 还是 hex？（base64url 省依赖复用 `base64`；hex 更好肉眼比对。倾向 base64url）
- **Q5**：需不需要「绝对会话上限」（如强制 90 天重登）？MVP 我倾向不做（属 Option B 的 family 范畴）。
- **Q6**：logout 只吊销当前设备的 refresh，还是提供「登出所有设备」？（前者 MVP 够；后者就是 `revoke_all_for_user`，几乎白送，可一起给）

审完 Q1–Q6，issue 半就能落地、login 随后一次返俩 token。实现你来，我按定稿写真库测试。
```
