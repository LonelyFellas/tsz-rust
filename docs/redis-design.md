# Redis 接入设计（架构基础设施）

> **用途**：给你审核用的设计定稿依据。定的是 **把 Redis 引入 tsz-rust 架构** 的方式——连接池、`AppState` 接缝、配置、探活、故障语义——以及 **第一个落地场景：OTP 验证码搬到 Redis**（`src/otp/`）。
> **配套**：OTP 业务规则见 [otp-design.md](otp-design.md)（本稿把其中 §5 存储 / §7 消费 / §8 限流从 Postgres 迁到 Redis，其余规则不变）。
> **对标**：现有 `platform::db::connect` + `PgPool` 的接入方式——Redis **平行复刻**这套，不另立门户。
> **背景**：项目此前纯 Postgres（users / refresh_tokens / verification_codes 全落 PG）。引入 Redis 是**主动决策**——验证码这类「短命、高频、到期即弃、无需审计」的数据本就是 KV + TTL 的甜区，且预期后续（缓存 / 分布式限流 / session）也要用。标 ⚠️ 处是「需要你拍板」或「与 PG 版本的语义差异」，请重点审。

---

## 0. 范围与非目标

| 做 | 不做 |
|---|---|
| 引入 Redis 连接池（`platform::redis`），塞进 `AppState`，`readyz` 探活 | 把 refresh_token / session 迁到 Redis（本轮只搬 OTP，接口留好口子） |
| OTP 的**码存储 / 单次消费 / attempts / 冷却 / 日限**改为 Redis | 全站缓存层、分布式锁、发布订阅（未来场景，本稿只搭地基） |
| Redis 宕机时 OTP 的**故障语义**（fail-close） | Redis 高可用（哨兵 / cluster）——先单实例，运维层面另议 |
| `verification_codes` 表的**去留决策**（§7） | 数据迁移脚本（表里现无生产数据，直接切） |

---

## 1. 关键决策速览

| # | 决策 | 取值 | 理由 / ⚠️差异 |
|---|---|---|---|
| 1 | 客户端 crate | **`deadpool-redis`（底层 `redis` crate）** | 提供 `Pool` 类型，心智模型与 `PgPool` 对齐；连接池语义、超时配置都能仿 `PgPoolOptions`。见 §3 |
| 2 | 连接抽象 | **`platform::redis::connect()` 返 `deadpool_redis::Pool`**，仿 `platform::db::connect` | 同一个 `platform` 模块下并列，启动流程 `main` 里多一步 connect。见 §3 |
| 3 | 状态注入 | **`AppState` 加 `redis: Pool` 字段 + `FromRef<AppState> for Pool`** | 沿用 PgPool 那套接缝，handler 按需 `State<Pool>` 提取。见 §3 |
| 4 | 码存储 | **Redis Hash `otp:code:{target}:{purpose}` + 整键 `EXPIRE ttl`** | TTL 到点自动删，取代 PG 的 `expires_at` 列 + 清理任务。见 §5 |
| 5 | 「只认最新码」 | **同 key 覆盖**（新码 `HSET` 直接盖旧码） | 天然实现，比 PG「查最新一条未消费」还简单。见 §5 |
| 6 | 单次消费 | **Lua 脚本原子 校验+DEL**（取代 SQL 的 CAS `UPDATE`） | Redis 单线程执行 Lua，天然原子；并发双验只一个成功。见 §6 |
| 7 | attempts | **同一 Lua 里 `HINCRBY attempts`，达上限锁死** | 和消费在一个原子块内，无竞态。见 §6 |
| 8 | 冷却 | **标记键 `otp:cd:{target}:{purpose}` + `SET NX EX cooldown`** | 键存在即「冷却中」，`SET NX` 失败即 RateLimited；无需计数。取代 `count_since(now-cooldown)`。见 §8 |
| 9 | 日限 | **Sorted Set `otp:daily:{target}:{purpose}`，滚动 24h 窗口**（已定） | 保留 PG 版的**精确滚动窗口**语义；每次发码修剪+计数+追加，用 Lua 保原子。见 §8 |
| 10 | 独立 / 叠加 | **每 purpose 独立**（key 含 `{purpose}`） | 与 otp-design 的结论一致；Redis 版靠 key 命名天然独立，无「加不加 WHERE」的分叉。 |
| 11 | 故障语义 | **Redis 不可达 → OTP 发送/校验 fail-close（拒绝）** | 限流依赖 Redis，宕机时放行等于关掉防刷闸 → 短信轰炸风险。见 §9 |
| 12 | `verification_codes` 表 | **Drop**（已定；无审计需求，留着是死表） | 加一条 down 迁移删表+索引；PG 版 `OtpRepository` 一并删。若将来要审计再上「Redis 主 + 异步旁写」，见 §7 |
| 13 | 测试 | **真 Redis 实例 + 每测隔离**（key 前缀 / `FLUSHDB`），仿 `#[sqlx::test]` 的真库风格 | 不用 mock；接缝与 PG 集成测一致。见 §10 |

---

## 2. 为什么现在引入 Redis（决策记录）

此前架构纯 PG，一度考虑「OTP 继续落 PG + 定时清理」。转向 Redis 的权衡：

- **验证码是 KV+TTL 的典型负载**：写多、读一次即弃、到点作废。PG 上要靠 `expires_at` 列 + `created_at` 派生限流 + 定时 `DELETE` 三件套模拟出来的东西，Redis 用 TTL / `SET NX` / `INCR` 原生就有。
- **限流从「扫历史行」变「看键在不在」**：PG 版 `count_since` 要对时间窗做 `COUNT(*)`；Redis 版冷却退化成「一个带 TTL 的标记键是否存在」，日限退化成「一个带 TTL 的计数器」，更轻、更快、零清理。
- **单次消费更干净**：PG 靠 CAS `UPDATE ... WHERE consumed_at IS NULL`；Redis 单线程 + Lua 让「校验-消费-计数」一次原子完成。
- **预期后续会用**：缓存、分布式限流、session 迁移都指望 Redis。地基现在搭好，后续边际成本趋零。

⚠️ **代价（如实记录）**：多一个要部署 / 监控 / 处理宕机降级的组件；本地开发、CI、测试都要多起一个 Redis；代码要多一套 fail-close 分支（§9）。这些成本我们**接受**，因为验证码场景收益明确且 Redis 是既定方向。

---

## 3. 接入架构：平行复刻 PgPool 那套

现有接缝（供对照）：

```
platform::db::connect(database_url) -> PgPool          // 建池
main() { let pool = connect(...); run(config, pool) }   // 启动注入
AppState { pool: PgPool, ... }                          // 持有
FromRef<AppState> for PgPool                            // 老 handler 用 State<PgPool>
readyz: State<PgPool> -> SELECT 1                       // 探活
```

Redis 照抄一份并列结构：

**(a) `platform::redis`（新文件 `src/platform/redis.rs`）** — 仿 `db.rs`：

```rust
use deadpool_redis::{Config, Pool, Runtime};

/// 从连接串建 Redis 连接池。参数对标 db::connect 的保守默认。
pub fn connect(redis_url: &str) -> anyhow::Result<Pool> {
    let cfg = Config::from_url(redis_url);
    let pool = cfg.create_pool(Some(Runtime::Tokio1))?;
    Ok(pool)
}
```

> ⚠️ 注意差异：`deadpool_redis` 的 `create_pool` 是**同步**的（不像 sqlx 的 `.connect().await` 会立刻建连并可探测到连不上）。它**惰性建连**——第一次 `pool.get().await` 才真正连。所以启动即验证连通性要靠 `readyz`（或 `main` 里主动 ping 一次，见下）。

`src/platform.rs` 里导出：
```rust
mod db;
mod redis;
pub use db::connect;
pub use redis::connect as connect_redis;   // 或给 db::connect 也换个显式名，二选一，你定命名
```

**(b) `main.rs`** — 多一步 connect，可选地主动 ping 一次好让「连不上」在启动即暴露：

```rust
let pool = platform::connect(&config.database_url).await?;
let redis = platform::connect_redis(&config.redis_url)?;
// 可选：启动即探活，连不上就别起服务（fail-fast）
redis.get().await?.ping::<()>().await?;   // 需要 redis::AsyncCommands
tsz_rust::run(config, pool, redis).await
```

**(c) `AppState`（`state.rs`）** — 加字段 + FromRef：

```rust
#[derive(Clone)]
pub struct AppState {
    pub pool: PgPool,
    pub redis: deadpool_redis::Pool,   // ← 新增（Pool 内部是 Arc，Clone 廉价）
    pub token_manager: Arc<TokenManager>,
    pub refresh_ttl: Duration,
}

impl FromRef<AppState> for deadpool_redis::Pool {
    fn from_ref(state: &AppState) -> Self { state.redis.clone() }
}
```
> `AppState::for_test(pool)` 也要跟着改——测试得能塞一个指向测试 Redis 的 pool（见 §10）。

**(d) `readyz`** — 探活从「只探 DB」扩成「DB + Redis 都通才 ready」：

```
SELECT 1 (PG) 且 PING (Redis) 都成功 → 200 ready
任一失败 → 503 unavailable（reason 指明是哪个）
```
> `liveness`（healthz）**不动**——它是纯进程存活，不碰任何外部依赖，这个语义保持。

---

## 4. 配置新增

`config.rs` 的 `Config` 加一个必填项（Redis 是硬依赖，不给默认）：

```rust
pub redis_url: String,   // 例：redis://127.0.0.1:6379/0
```

- **必填**（无 `#[serde(default)]`）——和 `database_url` 同级，缺了直接启动失败，符合「Redis 是硬依赖」的定位。
- `otp_*` 那几个配置项（ttl / cooldown / daily_limit / max_attempts）**语义不变、值不变**，只是消费方从 SQL 改成 Redis 命令。
- 测试基线 `valid_baseline()` 要补上 `REDIS_URL`，否则现有 config 测试全挂（§10 会提到）。

---

## 5. OTP 码存储：Hash + TTL

一枚码 = 一个 Redis Hash，键含 target 和 purpose，整键设 TTL：

```
KEY   otp:code:{target}:{purpose}
TYPE  Hash
       code      -> "042917"      # 6 位明文（明文理由同 otp-design §5，不变）
       attempts  -> "0"           # 错误计数
EXPIRE otp_ttl_minutes            # 整键 TTL，到点自动消失
```

生成时（service.request 内，限流通过后）：
```
HSET otp:code:{target}:{purpose} code {code} attempts 0
EXPIRE otp:code:{target}:{purpose} {ttl_seconds}
```
> 可用 `HSET` + `EXPIRE` 两条，或 pipeline 一次发。⚠️ 别忘 `EXPIRE`——漏了就成永久键（内存泄漏 + 码永不过期）。若用 Redis ≥ 7.4 可考虑 `HEXPIRE` 精细到字段，但整键 TTL 对本场景够用，**不引入额外复杂度**。

**「只认最新一条码」天然成立**：key 只跟 target+purpose 绑，再发一枚码就是 `HSET` 覆盖，旧码 code 字段被盖掉、TTL 被 `EXPIRE` 刷新。不存在「多条未消费码」的问题——比 PG 版（靠 `created_at DESC` 查最新）更省事，otp-design §7「只看最新一条未消费码」在这里是结构性保证。

---

## 6. 校验与单次消费：一段 Lua 原子搞定

PG 版靠 CAS `UPDATE ... WHERE consumed_at IS NULL` 防并发双消费。Redis 版更直接——**Redis 单线程执行 Lua 脚本**，脚本内「读码-比对-删除/计数」是一个不可分割的原子块，天然无竞态。

verify 逻辑（伪 Lua）：
```lua
-- KEYS[1] = otp:code:{target}:{purpose}
-- ARGV[1] = 用户提交的码   ARGV[2] = max_attempts
local stored = redis.call('HGET', KEYS[1], 'code')
if not stored then return 'INVALID' end            -- 没发过 / 已过期 / 已被消费

if stored == ARGV[1] then
    redis.call('DEL', KEYS[1])                     -- 命中即销毁 = 单次消费
    return 'OK'
else
    local n = redis.call('HINCRBY', KEYS[1], 'attempts', 1)
    if n >= tonumber(ARGV[2]) then
        redis.call('DEL', KEYS[1])                 -- 错满上限，锁死（直接删，等价永远 INVALID）
    end
    return 'INVALID'
end
```

对齐 otp-design 的不可区分性（§9）：**错 / 过期 / 已用 / 没发过 / 锁死，对外统一 `InvalidCode`**——上面所有非 `OK` 分支在 service 层都映射成同一个 `InvalidCode`，不泄露码处于哪种失效态。

**并发双验正确码**：两个请求同时带正确码进来，Redis 串行执行两段 Lua——第一段 `DEL` 成功返 `OK`，第二段 `HGET` 已取不到码返 `INVALID`。**只一个成功**，和 refresh / otp-design 的 CAS 结论一致，且不需要显式 CAS，靠单线程语义白拿。

---

## 7. `verification_codes` 表的去留 ⚠️

OTP 搬到 Redis 后，这张 PG 表和它的索引就**没有读写方了**。✅ **已定：方案 A（Drop）**。两个方案留档备查：

| 方案 | 做法 | 适用 |
|---|---|---|
| **A. Drop（推荐）** | 加一条 down 迁移删表；`OtpRepository`（PG 版）整个删掉 | 无审计需求——这正是当初「不落库」的前提。表里也没生产数据，切换无痛 |
| **B. 保留为审计旁路** | Redis 主存，**发码成功后异步旁写**一条 PG 审计记录（`INSERT`，不参与限流/校验路径） | 若合规要求「谁在何时发过什么码」的留痕。Redis 到期即删满足不了审计 |

**建议 A**。当初选「不落库」的理由就是不需要审计；真到了要审计那天再上 B，且那时 B 的写入是旁路、不影响 OTP 主流程性能。**在你确认前，先不删表**——文档记着这个待决项。

> 若选 A：`src/otp/repository.rs` 会被 `src/otp/store.rs`（Redis 版）取代，`OtpRepositoryError` 相应改成 `OtpStoreError`。接口方法名（`save` / `verify` / 限流检查）保持，`OtpService` 几乎不动——这正是当初「守好 repository 抽象边界」留的口子在兑现。

---

## 8. 限流：冷却 + 日限（Redis 原生，取代 count_since）

otp-design §8 的 `count_since` 在 Redis 上**整个消失**——不再扫历史行，改成两个带 TTL 的辅助键。每 purpose 独立（key 含 `{purpose}`）。

**冷却**（两码最小间隔，如 60s）——一个标记键的存在性即答案：
```
SET otp:cd:{target}:{purpose} 1 NX EX {cooldown_seconds}
  → 返回 OK    ：设置成功，说明冷却窗内没发过 → 放行
  → 返回 nil   ：键已存在，冷却中 → RateLimited
```
`SET NX` 本身原子——「检查+占位」一步完成，无 check-then-act 竞态。

**日限**（滚动 24h 上限，如 10 条）——**已定用 Sorted Set 精确滚动窗口**，保留 PG `count_since(now-24h)` 的语义。ZSet 里每个成员是一条发码事件，score = 发送时刻的时间戳：
```
KEY     otp:daily:{target}:{purpose}   TYPE Sorted Set
member  = {发送时刻的唯一 id（uuid）}   score = {发送时刻 unix 秒}   # 单位钉死为秒：ZADD 的 score 与 ZREMRANGEBYSCORE 的下限须同单位
```
发码前的检查+记录（**必须在一段 Lua 里原子完成**，见下）：
```
ZREMRANGEBYSCORE key 0 {now - 86400}     # 修剪掉 24h 前的旧事件
n = ZCARD key                            # 剩下的就是滚动窗内的发码数
if n >= daily_limit: RateLimited          # 已达上限，直接拒（不追加）
else:
    ZADD  key {now} {unique_id}           # 记这次发码
    EXPIRE key 86400                       # 让整个 ZSet 在闲置 24h 后自动消失，防内存泄漏
```

⚠️ **两个必须注意的坑**：
1. **check-then-act 竞态**：上面 `ZCARD → 判断 → ZADD` 若拆成多条独立命令，两个并发请求可能同时读到 `n < limit` 然后各自 `ZADD`，冲破上限。**必须整段放进一个 Lua 脚本**（Redis 单线程串行执行 Lua，天然原子），和 §6 的 verify 同理。冷却那条 `SET NX` 本身原子，可以留在 Lua 外，也可以一并塞进同一脚本。
2. **member 必须唯一**：同一毫秒发两条码若都用时间戳当 member，第二条会覆盖第一条（ZSet 成员去重）→ 少计一条。member 用 `uuid`（或 `时间戳:随机后缀`），score 才用时间戳。

> 相比固定窗口计数器，ZSet 版每次发码多几条命令、且 ZSet 会在窗口内累积成员（上限就 `daily_limit` 个，很小，可控）。换来的是**任意时刻严格回看过去 24h**、无固定窗口边界的近 2 倍超额问题——这正是当初选它的理由。

**冷却与日限的先后**：先查冷却（廉价、大多数刷请求挡在这），再查日限，都过了才生成码、`HSET`、发送。`0 = 关闭`语义不变（config 里设 0 即跳过该项检查）。

---

## 9. 故障语义：Redis 宕机时 fail-close ⚠️

限流、码存储全在 Redis。Redis 不可达时，OTP 的行为必须显式定义,否则默认行为可能很危险:

- **发送路径（request）**：拿不到 Redis 连接 → **拒绝发送**（返回一个「服务暂不可用」类错误），**不要**「限流查不了就放行」。放行等于在 Redis 抖动期间彻底关掉防刷闸，正是短信轰炸的窗口。**fail-close**。
- **校验路径（verify）**：拿不到 Redis → **拒绝**（对外仍是 `InvalidCode` 或一个 5xx，看你要不要区分「服务不可用」和「码错」——⚠️ 这里可考虑破例返 503 而非 `InvalidCode`，因为这不是用户的锅；但要权衡是否泄露「后端有依赖挂了」，你定）。
- **降级不做自动 fallback 回 PG**——搬走后 PG 已无码数据，fallback 无意义；老老实实 fail-close + 靠 Redis 高可用（未来）兜。

service 层因此要多一类错误 `OtpServiceError::Unavailable`（区别于 `RateLimitExceeded` / `InvalidCode`），由 handler 映射成合适的 HTTP 状态。

---

## 10. 测试策略：真 Redis + 每测隔离

沿用你 `#[sqlx::test]` 真库、不 mock 的路线。Redis 侧没有 `#[sqlx::test]` 这种现成的每测建库宏，隔离要自己搭：

**隔离手段（选一）**：
- **key 前缀**：每个测试用例生成唯一前缀（如 `test:{uuid}:otp:code:...`），测完不用清；缺点是所有 key 构造要能注入前缀。
- **独立 DB index**：Redis 有 0–15 号 DB，测试连一个专用 index，`setup` 时 `FLUSHDB`；缺点是并发测试会互相 `FLUSHDB` 冲，需串行或分配不同 index。
- **推荐**：给 `OtpStore` 构造时传一个 `key_prefix`（生产为空串，测试为唯一值），最干净、可并行、无需 flush。这也顺带让 key 命名有个统一出口。

**要覆盖的行为**（对齐 otp-design §13 的测试清单，迁移到 Redis 语义）：
- `request`：冷却窗内二次请求 → RateLimited（`SET NX` 返 nil）；日限到顶 → RateLimited；不同 purpose 互不影响（key 独立）；不同 target 互不影响。
- 码存储：`HSET` 后能读回；再发一枚 → 旧码被覆盖（只认最新）；TTL 确实设上（`TTL key > 0`）。
- `verify`：正确码 → OK 且 key 被删（单次消费，再验 → INVALID）；错码 → INVALID 且 attempts+1；错满 max_attempts → 锁死（key 删/永远 INVALID）；没发过 → INVALID。
- **并发双验正确码只一个 OK**（Lua 原子性，仿 refresh 并发测）。
- 过期：TTL 造一个极短过期或直接删 key 模拟「已过期」→ INVALID（不用真 sleep，直接操纵 key）。

**基建改动**：
- `AppState::for_test` 要能接一个测试 Redis pool（新增参数或读测试专用 `REDIS_URL`）。
- `config.rs` 测试的 `valid_baseline()` 补 `REDIS_URL`，否则「必填项齐全」的前提被破坏、现有 config 测试会误挂。
- CI 要起一个 Redis 服务容器（和现在起 Postgres 并列）。

---

## 11. Cargo 依赖新增

```toml
deadpool-redis = "0.18"          # 版本以实际最新为准；底层自动带 redis crate
# 若要在代码里直接用 AsyncCommands / Script，可能需显式：
# redis = { version = "0.27", features = ["tokio-comp", "script"] }
```
> ⚠️ 版本号写实现时再核对 crates.io 最新稳定版；`deadpool-redis` 会 re-export 一个匹配的 `redis`，尽量用它 re-export 的版本避免两个 `redis` 打架。

---

## 12. 落地顺序（建议）

1. **地基**：加 `redis_url` 配置 + `platform::redis::connect` + `AppState.redis` + `FromRef` + `readyz` 扩展 + `main` 里 connect/ping。**此步不碰 OTP**，先让「Redis 进架构、健康检查能反映它」跑通，可独立验证与提交。
2. **OTP 存储层**：`OtpStore`（Redis 版）实现 `save` / `verify`(Lua) / 冷却 / 日限；替换 service 里的 `OtpRepository` 调用。
3. **表决策**：按 §7 你的选择，drop 表（A）或加旁写（B）。
4. **测试**：按 §10 补 store 层集成测 + 修 config 测试基线。
5. **文档回填**：在 otp-design.md 的 §5/§7/§8 标注「实现已迁 Redis，见 redis-design.md」。

---

## 13. 待决项汇总（需你拍板）

| # | 待决 | 选项 | 结论 / 建议 |
|---|---|---|---|
| A | 客户端 crate | `deadpool-redis` / `fred`(自带池) / `bb8-redis` | ✅ **已定：`deadpool-redis`**（最贴近 PgPool 心智） |
| B | 日限窗口 | 固定窗口(INCR+EXPIRE) / 滚动窗口(ZSet) | ✅ **已定：滚动窗口(ZSet)**，见 §8 |
| C | `verification_codes` 表 | Drop / 保留作审计旁路 | ✅ **已定：Drop**，见 §7 |
| D | verify 遇 Redis 宕机 | 返 `InvalidCode` / 返 503 | ✅ **已定：503**（非用户过错，与限流/码错区分开） |
| E | 测试隔离 | key 前缀 / DB index+FLUSHDB | ✅ **已定：key 前缀**（可并行、最干净） |
| F | 是否本轮就迁 session/refresh | 只迁 OTP / 一起迁 | ✅ **已定：只迁 OTP**（小步；接口留口子，后续再搬） |
