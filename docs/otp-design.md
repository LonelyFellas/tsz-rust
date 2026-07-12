# otp 域设计（一次性验证码）

> **用途**：给你审核用的设计定稿依据。定的是 **OTP 验证码的生成 / 存储 / 下发 / 校验 / 限流**——`src/otp/`（repository + service + sender），碰 `verification_codes` 表。
> **配套**：access token 见 [auth-token-design.md](auth-token-design.md)，refresh 见 [session-refresh-design.md](session-refresh-design.md)。本域是它们的**并行轨**——OTP 是独立于「密码注册/登录」的第二条身份验证通道（登录/改密/销号/绑定）。
> **不含**：具体的 purpose 编排 handler（如「OTP 登录 → 发 token」「改密 → 改 `password_hash`」）——它们是本域的**调用方**，各自另做；真实短信/邮件 provider（阿里云）——只留 `Sender::Mock`，接入点见 §3。
> **对标**：tsz-go `internal/otp`（业务规则参照，非代码模板）。标 ⚠️ 处是「go 那样 / Rust 或本项目该改」，请重点审。

---

## 0. 范围与非目标

| 做 | 不做 |
|---|---|
| 生成 6 位数字码（CSPRNG）、存库、发码、校验、消费 | 真实 SMS/邮件 provider（阿里云）——只留 `Sender::Mock`，接入 = 加一个 enum variant |
| 每 target 冷却 + 日限（防刷、控短信成本） | 全局限流 / 图形验证码（跨 target 喷射的防护，见 §12 已知缺口） |
| verify 失败**不可区分** + attempts 上限锁死（防爆破） | purpose 专属编排（OTP 登录发 token、改密等，是调用方） |
| `Sender` 抽成 **enum**、**service 持有**（生成→存→发一手做完） | `Store` trait + in-memory fake（go 有，本项目**不移植**，见 §11） |

---

## 1. 关键决策速览

| # | 决策 | 取值 | 理由 / ⚠️差异 |
|---|---|---|---|
| 1 | 发送抽象 | **enum `Sender { Mock }`，service 持有** | 封闭 provider 集，enum 躲开 async-trait/dyn/泛型传染；接阿里云=加 variant。见 §3 |
| 2 | 持久化抽象 | **具体 `OtpRepository{pool}` + `#[sqlx::test]`，无 trait/fake** | 真库可测，沿用 user/session 惯例（**与 go 的 `Store` interface 相反**）。见 §11 |
| 3 | code 生成 | **6 位数字，CSPRNG，无取模偏置，零填充** | 安全凭证必须 CSPRNG（复用 session 的 `getrandom`，**非** `generate_display_name` 的 ThreadRng）。见 §4 |
| 4 | code 存储 | **明文存**（不哈希） | 6 位=10⁶ 低熵，哈希对脱库者≈无用；防线是 TTL+attempts+单次。**与 refresh token 故意相反**。见 §5 |
| 5 | 限流 | 每 target+purpose **冷却**（两码最小间隔）+ **日限**（滚动 24h 上限），**生成前**先查 | 控短信成本、防刷；派生自 `created_at`，不加列。见 §8 |
| 6 | attempts | 单条码**错 5 次即锁**（`max_attempts`，可配） | 6 位码在线爆破概率 ≤ 5/10⁶，与 TTL 无关。见 §7 |
| 7 | verify 消费 | **CAS 消费**（`UPDATE...SET consumed_at WHERE consumed_at IS NULL`） | 防并发下同一码被验两次（**沿用 refresh 的 CAS 教训**）。见 §7 |
| 8 | verify 错误 | 错/过期/已用/没发过/锁死 → **统一 `InvalidCode`** | 不泄露某 target 是否有待验码、码处于哪种失效态（对齐 refresh 的统一 401）。见 §9 |
| 9 | 渠道判定 | `target` 含 `@` → Email，否则 Sms | 和 user 域 `normalize_identifier` 同一判据。见 §3 |

---

## 2. 数据模型与表字段映射

`verification_codes` 表（已建，见迁移）——**故意无外键指向 users**（注册/找回场景里 target 是裸手机号/邮箱，可能还没账号）：

| 列 | 本设计怎么用 |
|---|---|
| `id` | PK，UUIDv7（Rust 侧 `Uuid::now_v7()` 生成，和 users/refresh_tokens 一致） |
| `target` | 目标手机号/邮箱（裸标识） |
| `channel` | `sms`/`email`，由 `Channel::for_target` 判定后存（enum ↔ TEXT） |
| `purpose` | `login`/`password_reset`/`account_deletion`/`contact_bind`（enum ↔ TEXT，对齐 CHECK） |
| `code` | 6 位数字**明文**（§5） |
| `expires_at` | 绝对过期时刻 = 生成时 `now + ttl` |
| `consumed_at` | 单次消费标记，非空即已用；CAS 消费的守卫列（§7） |
| `attempts` | 错误计数，DEFAULT 0，到 `max_attempts` 锁死（§7） |
| `created_at` | DEFAULT now()；**冷却/日限靠它派生**（§8） |

索引 `verification_codes_lookup (target, purpose, created_at DESC)` 正好服务 verify 的「查最近一条未消费」（§7）与 `count_since`（§8）。

**Rust 侧枚举**（`model.rs`）：
```rust
#[derive(sqlx::Type)] #[sqlx(type_name = "text", rename_all = "lowercase")]
pub enum Channel { Sms, Email }
impl Channel {
    /// 和 user 域同判据：含 @ 视作邮箱。
    pub fn for_target(target: &str) -> Channel {
        if target.contains('@') { Channel::Email } else { Channel::Sms }
    }
}

#[derive(sqlx::Type)] #[sqlx(type_name = "text", rename_all = "snake_case")]
pub enum Purpose { Login, PasswordReset, AccountDeletion, ContactBind }
```
> ⚠️ `Purpose` 用 `rename_all = "snake_case"`（`password_reset` 等），务必和 CHECK 里的字面量逐字对上——参照 [sqlx-type-mapping-cheatsheet](sqlx-type-mapping-cheatsheet.md)，`type_name="text"` 必填。

---

## 3. Sender 接缝（enum，service 持有）

**决策：`Sender` 是 enum，`OtpService` 直接持有它。** 这是本域唯一「抽象一个外部依赖」的地方——因为真 provider（阿里云）现在没有、以后测试也不该真发。**和 repository 反着来**（repo 是具体类型 + 真库测，因为真库就在手边）。

```rust
pub enum Sender {
    Mock,                              // 现在：打日志、不真发
    // Aliyun(AliyunSmsClient),        // 以后：加这一 variant + 一个 match 分支即可
}

impl Sender {
    pub async fn send(&self, channel: Channel, target: &str, code: &str) -> Result<(), SendError> {
        match self {
            Sender::Mock => {
                tracing::info!(mock = true, ?channel, target, code, "otp_code_sent");
                Ok(())
            }
        }
    }
}
```

- **为什么 enum 不是 trait**：封闭的少数 provider 集（mock + 阿里云短信 + 也许邮件）。enum 具体、穷尽匹配、无动态分发，**躲开 `async-trait`/`Arc<dyn>`/泛型往 `AppState` 传染**这一堆麻烦，贴项目「清爽、少 trait 仪式」的调性。
- **接入阿里云 = 加一个 variant + 一个 `match` 分支 + config 里的密钥**，OTP 域其它代码零改动。`run()` 里把 `Sender::Mock` 换成 `Sender::Aliyun(...)` 即切生产。
- ⚠️ **日志安全（真 provider 务必守）**：`Sender::Mock` 是 dev/test-only，故意把 `code`+`target` 都打出来方便本地「我给 X 发的啥码」。**真阿里云 variant 绝不能打 `code`（是活凭证），`target` 要脱敏**（`138****1234` / `a***@x.com`，是 PII）。这条 go 注释里专门警告过。

---

## 4. 生成：CSPRNG 6 位码

```
code = 从 CSPRNG 取 [0, 1_000_000) 的无偏整数，零填充成 6 位字符串
```
- ⚠️ **必须 CSPRNG**：复用 session 那套系统级熵源（`getrandom`），**不要**用 `generate_display_name` 的 `rand::rng()`/ThreadRng——那是非安全场景，OTP 是凭证。
- ⚠️ **防取模偏置**：`u32 % 1_000_000` 有轻微偏置（10⁶ 不整除 2³²）。用带拒绝采样的区间生成（`rand` 的 `random_range(0..1_000_000)`），或手动拒绝采样，别裸 `%`。
- ⚠️ **零填充**：`042317`、`000042` 都是合法码——存/比对都按 6 位字符串（`format!("{:06}", n)`），别丢前导零。

---

## 5. 存储：明文（为什么不哈希——和 refresh 相反）

`verification_codes.code` **存明文 6 位数字**。这和 refresh token 存 SHA-256 是**故意相反**的，理由同你之前「密码 vs refresh」那次讨论的一脉：

| | refresh token | OTP code |
|---|---|---|
| 熵 | 高（256-bit 随机） | **低（10⁶）** |
| 哈希有用吗 | 有（脱库者反推不出明文） | **≈无用**（脱库者 10⁶ 次 sha256 是毫秒级，直接反查） |
| 真正的防线 | 存哈希 + 唯一索引 | **短 TTL + attempts 上限 + 单次消费** |

- 既然哈希挡不住脱库爆破，就不做无用功；防线靠 §6/§7 的**限次 + 限时 + 单次**。
- 🔖 想要「不在库里裸放活凭证」的洁癖，可顺手 sha256（成本极低），但收益很低、且要放弃「运维直接看库里 code 排障」的便利——**MVP 建议明文**（go 也是明文）。这是个有意识的取舍，写在这以免以后困惑。

---

## 6. request 流程（限流在前 → 生成 → 存 → 发）

`OtpService::request(target, purpose)`：

```
1. 限流检查（§8）：冷却 + 日限，任一超 → RateLimited（【不生成、不发】，省短信钱）
2. channel = Channel::for_target(target)
3. code = 生成 6 位 CSPRNG（§4）
4. repo.save(NewCode{ id: uuidv7, target, channel, purpose, code, expires_at: now+ttl })
5. sender.send(channel, target, code)  → 失败则 Send 错误
6. Ok(())
```

- **限流必须在生成/发送之前**——否则攻击者就算被拒也已经触发了短信，达不到控成本的目的。
- ⚠️ **save 与 send 的顺序与原子性**：先存后发。若 send 失败（阿里云抖），库里留了一枚**已存未送达**的码——无害（用户没收到，自然过期；用户重试受冷却约束）。**不做**「send 失败回滚 save」的事务（那会把外部调用塞进 DB 事务，得不偿失）。这个窄窗口可接受，和 refresh 的「rotate+issue 空洞」同一性质的取舍。
- **不主动作废旧码**：同 target+purpose 若有旧的未消费码，不动它——verify 只认最新一条（§7），旧的自然过期。（go 也是这样。）

---

## 7. verify 流程（不可区分 + CAS 防并发双用 + attempts 锁）

`OtpService::verify(target, purpose, code)`：

```
1. row = repo.latest_unconsumed(target, purpose)   → None                → InvalidCode
2. row.expires_at <= now                            → 过期                → InvalidCode
3. row.attempts >= max_attempts                     → 锁死                → InvalidCode
4. 比对 row.code 与 code：
   ├─ 相符 → repo.mark_consumed(row.id)【CAS：WHERE consumed_at IS NULL】
   │         ├─ 消费成功(rows==1) → Ok(())
   │         └─ 消费落空(rows==0，被并发抢先) → InvalidCode
   └─ 不符 → repo.increment_attempts(row.id) → InvalidCode
```

- ⚠️ **消费必须 CAS**（沿用 refresh 教训）：两个请求拿同一枚正确码并发 verify，若「读到未消费 → 各自 mark」会双重消费。改成 `UPDATE...SET consumed_at=NOW() WHERE id=$1 AND consumed_at IS NULL RETURNING`，靠 `rows==1` 判「我消费成功」，`rows==0` 即被抢先 → InvalidCode。单次使用由数据库行级原子性守住。
  - 副作用：正确码被并发**双提交**时，一个成功、另一个吃 InvalidCode（合法用户重复点了提交）。安全优先，可接受。
- **attempts 只在「码不符」时 +1**。并发下两次错猜各 +1（计数无正确性问题，偏保守）。到 `max_attempts` 后该码永远 InvalidCode，直到用户**请求新码**（新码成为最新一条）——但请求受冷却约束，这正是防爆破的节流。
- **只看最新一条未消费码**：TTL 均匀 + 新码总是最新，故「最新未消费」就是唯一有效码，旧的更早过期、无需单独查。
- **不区分**：上面 4 类失败对外都是同一个 `InvalidCode`——不让攻击者探出「这 target 有没有待验码 / 码处于哪种失效态」。（对齐 refresh 的统一 401。）

---

## 8. 限流：冷却 + 日限（派生自 created_at，不加列）

```
checkRateLimit(target, purpose):
  cooldown > 0:  n = repo.count_since(target, purpose, now - cooldown);   n > 0 → RateLimited
  daily_limit>0: n = repo.count_since(target, purpose, now - 24h);        n >= daily_limit → RateLimited
```

- 两个限制都**从 `created_at` 查询派生**，表不加列。`count_since` 一条 `WHERE target=$1 AND purpose=$2 AND created_at >= $3` 走 `verification_codes_lookup` 索引。
- 各限制 `0 = 关闭`（配置里设 0 即禁用该限制），方便测试与灰度。
- **冷却**约束「两码最小间隔」（如 60s，防连点刷短信）；**日限**约束「每 target 每 24h 上限」（如 10 条，防长期刷）。

---

## 9. 错误与不可区分

```rust
pub enum OtpError {
    RateLimited,               // request：冷却/日限触发
    InvalidCode,               // verify：错/过期/已用/没发过/锁死——统一，对内也不细分
    Send(SendError),           // request：provider 发送失败
    Repository(OtpRepoError),  // sqlx 等
}
```

handler 层映射（建议）：

| 域错误 | HTTP | 备注 |
|---|---|---|
| `RateLimited` | **429** Too Many Requests | 标准语义；可带 `Retry-After` |
| `InvalidCode` | **400**（或 401，见调用方语义） | verify 是被 purpose 流程调用的，最终码由那个 handler 定 |
| `Send` | **502/500** | 隐藏 cause（`AppError::internal`） |
| `Repository` | **500** | 隐藏 cause |

> ⚠️ 和 refresh 一样：`InvalidCode` 对内也**不细分**（不像有些设计留 `Expired/Locked` 内部枚举）——本域 MVP 不需要区分（无级联逻辑），统一最简、也最不容易漏掉某条失败路径的文案。

---

## 10. 配置（`config.rs` 新增，沿用现有 u64 风格）

```rust
otp_ttl_minutes:      u64   // 默认 5   —— 码有效期
otp_cooldown_seconds: u64   // 默认 60  —— 两码最小间隔；0=关
otp_daily_limit:      i64   // 默认 10  —— 每 target/24h 上限；0=关
otp_max_attempts:     i32   // 默认 5   —— 单码错几次锁死
```
`run()` 里读配置构造 `OtpService::new(repo, Sender::Mock, ttl, cooldown, daily_limit, max_attempts)`，塞进 `AppState`（或按现状按需 new——但 Sender 有状态/将来握 provider 连接，建议进 `AppState`）。

---

## 11. 模块结构 + Rust 骨架

```
src/otp/
  mod.rs
  model.rs        // Code(行) / NewCode / Channel / Purpose / OtpError
  repository.rs   // OtpRepository{pool}：save / latest_unconsumed / mark_consumed(CAS) / increment_attempts / count_since
  sender.rs       // enum Sender{Mock} + send()（§3）
  service.rs      // OtpService：request / verify（§6/§7）+ 生成/限流
  handler.rs      // POST /otp/request（§12）
```

- ⚠️ **不移植 go 的 `Store` interface + in-memory fake**：repository 是具体 `OtpRepository{pool}` 固有 async 方法 + `#[sqlx::test]` 真库测，和 user/session 一致。go 那个 `contract_test.go`/内存 fake 是本项目明确放弃的模式。
- **生成 code + 限流编排**放 service（业务）；repository 只碰 SQL、收/吐已算好的值。

**repository 契约**（真库方法）：
```rust
impl OtpRepository {
    async fn save(&self, new: NewCode) -> Result<(), OtpRepoError>;
    async fn latest_unconsumed(&self, target: &str, purpose: Purpose) -> Result<Option<Code>, OtpRepoError>;
    //   SELECT ... WHERE target=$1 AND purpose=$2 AND consumed_at IS NULL ORDER BY created_at DESC LIMIT 1
    async fn mark_consumed(&self, id: Uuid) -> Result<u64, OtpRepoError>;      // CAS：... WHERE id=$1 AND consumed_at IS NULL
    async fn increment_attempts(&self, id: Uuid) -> Result<(), OtpRepoError>;  // SET attempts = attempts + 1 WHERE id=$1
    async fn count_since(&self, target: &str, purpose: Purpose, since: DateTime<Utc>) -> Result<i64, OtpRepoError>;
}
```

**service 骨架**：
```rust
pub struct OtpService {
    repository: OtpRepository,
    sender: Sender,
    ttl: Duration,
    cooldown: Duration,   // 0 = 关
    daily_limit: i64,     // 0 = 关
    max_attempts: i32,
}
impl OtpService {
    pub async fn request(&self, target: &str, purpose: Purpose) -> Result<(), OtpError>;   // §6
    pub async fn verify(&self, target: &str, purpose: Purpose, code: &str) -> Result<(), OtpError>; // §7
}
```

---

## 12. 端点与编排

- **`POST /otp/request` 是真端点**：`{ target, purpose }` → `request(...)` → 204/200。是「发一枚码」的通用入口。
- **verify 不做成独立端点**：因为「验过了」本身没有意义——login/改密等都要在验过的**同一步**里紧接着发 token / 改密码（否则「先验后做」之间有竞态/绕过缺口）。所以 `verify` 是 **service 方法，被 purpose 专属 handler 调用**：
  ```
  未来 POST /auth/login-otp:  otp_service.verify(target, Login, code)? → user 查/建 → token_manager.generate + session.issue  （像 password 登录那样编排）
  未来 POST /auth/password-reset: otp_service.verify(target, PasswordReset, code)? → 更新 password_hash + session.revoke_all_for_user
  ```
  这些 purpose 流程**各自另做**（本域只提供 request 端点 + verify 方法这两块砖）。

- ⚠️ **已知缺口（写在案，MVP 不做）**：§8 的限流是**每 target**的——挡得住「盯着一个号狂刷」，但挡不住「向一万个不同号各发一条」的**喷射式攻击**（每个 target 都在限内，但你为每条短信买单）。真要防得加**全局节流 / 图形验证码 / 可疑来源识别**。MVP 先靠每-target 限流兜底，此缺口留待有真实滥用时再补。

---

## 13. 可测性（我写测试要用）

- **真库测**（`#[sqlx::test]`，仿 `tests/session_*.rs`）。过期不用 sleep——`save` 的 `expires_at` 是入参，直接塞过去时刻造「已过期码」（天然接缝）。
- **测试怎么拿到 code**：service 存库后，测试直接 `repo.latest_unconsumed` 读回 code 来验证——所以 `Sender::Mock` 连「记住最近一条」都不用做，纯日志即可。
- **repository 层**要覆盖：save→latest_unconsumed 查回；`mark_consumed` 的 CAS（未消费→消费成功返 1；已消费→返 0；**并发两个 mark 只一个成功**，同 refresh 那条并发测）；`increment_attempts` 累加；`count_since` 按 target/purpose/时间窗计数且不串号。
- **service 层**要覆盖：
  - request：成功存一行 + channel 判定对（sms/email）；冷却内再请求→RateLimited 且不新增行；日限到顶→RateLimited；限流命中时**不发**（Mock 不被调用——可用计数或返回值验证）。
  - verify：正确码→Ok 且 `consumed_at` 落上；同码再验→InvalidCode（单次）；错码→InvalidCode 且 `attempts+1`；错满 `max_attempts`→InvalidCode（锁）；过期码→InvalidCode；没发过→InvalidCode；**并发双验正确码只一个 Ok**。
- **handler 层**（`/otp/request`）：请求发码 200/204 且库里落一行；冷却内二次请求→429。

---

## 14. 依赖

`chrono`/`uuid`/`sqlx`/`thiserror`/`getrandom`（或 `rand`，取无偏区间生成）都已在依赖里（session 域引入过）。**无需新增 crate**。真阿里云 provider 接入时再按需加 HTTP 客户端。

---

## 15. 决策记录（待你审 / 拍板项）

| # | 问题 | 倾向 | 待确认 |
|---|---|---|---|
| Q1 | Sender 形状 | **enum，service 持有** ✅（已定） | — |
| Q2 | code 存明文还是哈希 | **明文**（§5，低熵哈希无用） | 是否要洁癖 sha256？（我建议不） |
| Q3 | TTL / 冷却 / 日限 / max_attempts 默认值 | 5min / 60s / 10 / 5 | 数值你定，进 config |
| Q4 | verify 失败对外码 | InvalidCode → 由调用方 handler 定（登录场景可能 401） | 通用 `/otp/request` 的 RateLimited=429 |
| Q5 | 是否主动作废旧码 | **不作废**（verify 只认最新，旧的自然过期） | 认可？ |
| Q6 | 全局防喷射 | **MVP 不做**（只每-target 限流），缺口在案（§12） | 认可延后？ |

至此 OTP 域可落地：schema 已就绪、`Sender::Mock` 顶上、全部逻辑今天可建可测，阿里云接入是后期加一个 enum variant 的事。**实现你来，我按定稿写真库测试。**
