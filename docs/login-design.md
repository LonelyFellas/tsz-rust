# login handler 设计

> **用途**：审核用的设计骨架。login 是第一个**跨域编排**的端点——把 user（验密码）+ auth（签 access）+ session（发 refresh）拼起来。
> **前置**：`TokenManager`（[auth-token-design.md](auth-token-design.md)）、`SessionService::issue`（[session-refresh-design.md](session-refresh-design.md)）均已就绪。
> **本文档要一并解决的结构问题**：引入 `AppState`（之前 register 用「现建 service」绕过了，login 用到 `TokenManager` 就该正经搭 state 了）。

---

## 0. 范围

| 做 | 不做 |
|---|---|
| `POST /auth/login`：验身份 → 签 access + 发 refresh → 返俩 token | 第三方/验证码登录（另域） |
| 引入 `AppState`（`FromRef` 兼容现有 `State<PgPool>` handler） | `/auth/refresh`、`/auth/logout`（session rotate/revoke 半，另做） |
| `UserService::authenticate`（归一化 + 查用户 + 验密码 + 查状态） | 中间件/受保护路由的 token 校验（提取器层，另做） |

---

## 1. 关键决策速览

| # | 决策 | 取值 | 理由 |
|---|---|---|---|
| 1 | 请求标识字段 | **单个 `identifier`**（手机或邮箱一个框） | `get_by_identifier` 本就 `WHERE phone=$1 OR email=$1`；前端一个输入框更简单 |
| 2 | 用户不存在 vs 密码错 | **返同一个 `401 invalid credentials`** | 防账号枚举（已确认的安全铁律） |
| 3 | 检查顺序 | **先验密码、再查账号状态** | 只有「证明了自己拥有该账号」的人才配知道它被禁——见 §5 |
| 4 | 账号被禁 | 密码对之后 → **`403 account disabled`** | 放在密码校验之后，攻击者无密码时拿不到这个信号，不构成枚举 |
| 5 | 成功响应 | **200** + OAuth 形状 `{access_token, refresh_token, token_type, expires_in}` | login 不创建资源（register 才是 201） |
| 6 | state | **引入 `AppState`**，`TokenManager` 放进去 | 别每次 login 现 `new`（重建 key/validation 浪费）；login 之后的端点也要用 |
| 7 | 时序侧信道 | 用户不存在时**也跑一次假 bcrypt verify** | 否则「查无此人」快速返回、密码错慢返回，响应耗时会泄露账号是否存在。见 §10 |

---

## 2. AppState（本次的结构改动）

现在 `router(pool)` 的 state 是裸 `PgPool`。login 需要 `TokenManager` + refresh TTL，所以升级成 `AppState`，用 `FromRef` 让**现有 `State<PgPool>` handler（healthz/readyz/register）不用改**：

```rust
// src/state.rs（或 lib.rs 顶部）
use std::sync::Arc;
use axum::extract::FromRef;
use chrono::Duration;
use sqlx::PgPool;
use crate::auth::TokenManager;

#[derive(Clone)]
pub struct AppState {
    pub pool: PgPool,
    pub token_manager: Arc<TokenManager>,   // web realm；Arc 让 AppState: Clone
    pub refresh_ttl: Duration,
}

// 让 State<PgPool> 仍能从 AppState 抽出来 → 老 handler 零改动
impl FromRef<AppState> for PgPool {
    fn from_ref(s: &AppState) -> Self {
        s.pool.clone()
    }
}
```

⚠️ **迁移代价**：`router` 签名从 `router(pool: PgPool)` 变成 `router(state: AppState)`。现有集成测试（`health.rs`、`user_register_handler.rs`）调的是 `router(pool)`，得改成建一个 `AppState`。我写实现时会提供一个 `AppState::for_test(pool)` 辅助（塞一个 dummy secret 的 `TokenManager` + 默认 TTL），把测试改动收敛到一行。

- `TokenManager` 用 `Arc` 是因为它内部持 `EncodingKey`/`DecodingKey`/`Validation`，未必都 `Clone`；`Arc` 一层最省心，且它是只读共享、天然适合。

---

## 3. config 新增字段

```rust
pub struct Config {
    // ... 现有 ...
    #[serde(default = "default_access_ttl_min")]
    pub access_token_ttl_minutes: u32,   // 默认 15
    #[serde(default = "default_refresh_ttl_days")]
    pub refresh_token_ttl_days: u32,     // 默认 30（Q2 已定）
}
```

`run()` 里从 config 组装：`TokenManager::new(&cfg.jwt_secret, Realm::Web, Duration::minutes(cfg.access_token_ttl_minutes as i64))`，refresh_ttl 同理，一起塞进 `AppState`。

---

## 4. DTO + 归一化

```rust
#[derive(Deserialize)]
pub struct LoginRequest {
    pub identifier: String,   // 手机或邮箱
    pub password: String,
}

#[derive(Serialize)]
pub struct LoginResponse {
    pub access_token: String,
    pub refresh_token: String,
    pub token_type: &'static str,   // "Bearer"
    pub expires_in: i64,            // access 剩余秒数
}
```

**`normalize_identifier`**（放 user service，复用现有 `normalize_phone`/`normalize_email`）：
```
trim → 含 '@' ? 当邮箱(小写) : 当手机(仅 trim)
```
因为 email 入库时被 register 小写化了，登录也得同样小写才能命中 `WHERE email=$1`。

---

## 5. 编排流程（handler 视角）

```
1. normalize_identifier(req.identifier)
2. user = UserService::authenticate(identifier, password)?   // 见 §6，失败→统一错误
3. role = user.last_active_role.unwrap_or(Student) 的字符串
4. access  = token_manager.generate(user.id, role)?          // TokenError → 500
5. refresh = session_service.issue(user.id)?                 // SessionError → 500
6. 200 + LoginResponse { access, refresh.plaintext, "Bearer", expires_in }
```

⚠️ **第 2 步内部的顺序（安全核心）**：`authenticate` 里是 **查用户 → 验密码 → 查状态**：
- 查不到用户 → `InvalidCredentials`（**跑一次假 verify 平衡时序**，§10）
- 密码不对 → `InvalidCredentials`（和上面**同一个错**）
- 到这一步说明密码对了、身份已证实 → 再看 `status`：`disabled` → `AccountDisabled`

「先验密码再查状态」保证：没有密码的攻击者永远走不到 status 判断，`AccountDisabled` 不会泄露给他 → 不构成枚举。

---

## 6. `UserService::authenticate`（业务逻辑落哪）

把「归一化 + 查 + 验密码 + 状态」这段收进 user 域，handler 只管编排 token，保持薄：

```rust
#[derive(Debug, thiserror::Error)]
pub enum LoginError {
    #[error("invalid credentials")]
    InvalidCredentials,   // 用户不存在 / 密码错，二者不可区分
    #[error("account disabled")]
    AccountDisabled,
    #[error(transparent)]
    Repository(#[from] UserError),
}

impl UserService {
    /// 校验凭证，成功返回该 User。不签 token（那是 handler 的编排）。
    pub async fn authenticate(&self, identifier: &str, password: &str) -> Result<User, LoginError> {
        let id = normalize_identifier(identifier);
        match self.repository.get_by_identifier(&id).await {
            Ok(user) => {
                if !verify_password(password, &user.password_hash) {
                    return Err(LoginError::InvalidCredentials);
                }
                if user.status == UserStatus::Disabled {
                    return Err(LoginError::AccountDisabled);
                }
                Ok(user)
            }
            Err(UserError::NotFound) => {
                verify_password(password, dummy_hash());   // 时序平衡，丢弃结果（§9）
                Err(LoginError::InvalidCredentials)
            }
            Err(e) => Err(LoginError::Repository(e)),
        }
    }
}

fn verify_password(password: &str, hash: &str) -> bool {
    bcrypt::verify(password, hash).unwrap_or(false)   // verify 出错当作不匹配
}
```

---

## 7. 错误映射（handler → AppError）

| 领域错误 | HTTP | 说明 |
|---|---|---|
| `LoginError::InvalidCredentials` | **401** | 对外文案统一「invalid credentials」 |
| `LoginError::AccountDisabled` | **403** | 密码已验证后才可能到这 |
| `LoginError::Repository(_)` | 500 | internal，隐藏 cause |
| `TokenError` / `SessionError`（签 token 阶段） | 500 | 正常不发生；internal |

⚠️ **文案**：`AppError::Unauthorized` 现在 Display 固定是 "unauthorized"。想让 body 精确是 "invalid credentials"，可给 `AppError` 加个带消息的 401 变体（如 `Unauthorized(String)`），或接受 "unauthorized"。倾向加变体，因为登录错误文案是对外契约。→ 开放问题 Q1。

---

## 8. Rust 骨架（handler）

```rust
// src/user/handler.rs（与 register 并列）或新建 src/auth 的 handler
pub async fn login(
    State(state): State<AppState>,
    Json(req): Json<LoginRequest>,
) -> Result<impl IntoResponse, AppError> {
    let user_svc = UserService::new(UserRepository::new(state.pool.clone()));
    let user = user_svc
        .authenticate(&req.identifier, &req.password)
        .await
        .map_err(map_login_error)?;

    let role = role_str(user.last_active_role);
    let access = state.token_manager.generate(user.id, role).map_err(AppError::internal)?;

    let session_svc = SessionService::new(
        RefreshTokenRepository::new(state.pool.clone()),
        state.refresh_ttl,
    );
    let refresh = session_svc.issue(user.id).await.map_err(AppError::internal)?;

    Ok((
        StatusCode::OK,
        Json(LoginResponse {
            access_token: access,
            refresh_token: refresh.plaintext,
            token_type: "Bearer",
            expires_in: /* access ttl 秒数 */,
        }),
    ))
}
```

⚠️ `expires_in` 需要 access TTL 的秒数——`TokenManager` 目前没暴露 ttl。给它加个 `pub fn ttl_seconds(&self) -> i64`（或让 login 从 config/state 拿）。→ 开放问题 Q2。

---

## 9. 时序侧信道加固（§1 #7 展开）

不做假 verify 的话：`get_by_identifier` 查无此人会**很快**返回，而真实用户要跑一次 bcrypt verify（故意慢、几十毫秒）。攻击者测响应耗时就能区分「账号是否存在」——把「不可区分」从内容层泄露到了时间层。

对策：`NotFound` 分支也 `verify_password(password, dummy_hash())`（`OnceLock` 惰性生成、按当前 `DEFAULT_COST`，见 Q4），丢弃结果。这样两条路都花掉一次 verify 的时间。

---

## 10. 可测性（我写测试要用）

- **`authenticate`（真库，仿 `tests/user_service.rs`）**：正确凭证→Ok(user)；密码错→InvalidCredentials；**未知 identifier→InvalidCredentials（和密码错同一个错，专门钉住不可区分）**；disabled 账号+对密码→AccountDisabled；手机/邮箱两种 identifier 都能登。
- **login handler（`oneshot`，仿 `tests/user_register_handler.rs`）**：成功→200 + 四字段齐 + refresh 已落库（DB 能按 hash 查到）；未知用户 与 密码错→**都 401 且 body 一致**；disabled→403；响应不含 `password_hash`。

---

## 11. 决策记录（Q1–Q5 已定，2026-07-11）

| # | 定案 | 落地 |
|---|---|---|
| Q1 | **`AppError::Unauthorized(String)`**（带消息），body 精确返 "invalid credentials" | 改 `error.rs`：`Unauthorized` 从单元变体改成带 `String`；`status_code` 分支 + Display 跟着改；`Unauthorized` 的单测同步 |
| Q2 | **`TokenManager::ttl_seconds() -> i64`** | auth.rs 加一个方法：`self.ttl.num_seconds()`；login 用它填 `expires_in` |
| Q3 | **`POST /auth/login`**——login/refresh/logout 是「会话生命周期」三件套，聚在 `/auth/*`（register 属用户生命周期，留 `/user/*`）| lib.rs router 加 `.route("/auth/login", post(handler::login))`。handler 文件先放 `src/user/handler.rs`（用了 `authenticate`），等 refresh/logout 时再看要不要抽 `session/handler.rs` |
| Q4 | **惰性生成 dummy hash**（`OnceLock`，按当前 `DEFAULT_COST`，永远和真实密码同 cost，不留 magic 常量） | 见下方 `dummy_hash()` |

```rust
use std::sync::OnceLock;
use bcrypt::{DEFAULT_COST, hash};

/// 时序平衡用的假哈希：首次用到时按当前 DEFAULT_COST 算一次并缓存，
/// 保证和真实密码同 cost（DEFAULT_COST 变了也自动跟随）。
fn dummy_hash() -> &'static str {
    static H: OnceLock<String> = OnceLock::new();
    H.get_or_init(|| hash("timing-balance", DEFAULT_COST).expect("dummy hash 生成失败"))
}
```
| Q5 | **抽 `UserRole::as_str()`** 复用 | model.rs 给 `UserRole` 加 `pub fn as_str(&self) -> &'static str`；register 的 `to_response` 也换成它 |

审完你写 handler + `authenticate`（含上面几处小改），我写 §10 两层测试 + 提供 `AppState::for_test` + 同步 `error.rs` 的 Unauthorized 单测。
