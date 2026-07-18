# 前端契约对齐待办（后端 → 对齐前端 api-client）

> 决策：**tsz-rust 后端对齐前端已有契约**，做成前端的 drop-in 后端。前端 `packages/api-client`
> 零改动。前端契约源：`tsz/packages/api-client/src/{endpoints,http}.ts` + `packages/types`。
>
> 分工：**实现由你写**；我出规格 + 写/改集成测试 + 文档。本文件是唯一事实源，实现时对着勾。

---

## 0. 全局约定（先建好这三块公共件，后面接口都复用）

### 0.1 `UserPublic` 响应对象（对齐前端 `@tsz/types` 的 `User`）

登录/注册/me 的响应里凡是「用户」都用它，字段名、类型、可选性必须完全一致：

```jsonc
{
  "id": "uuid 字符串",
  "phone": "13800000000",     // 纯邮箱账号则整个字段省略（None → 不序列化，用 skip_serializing_if）
  "email": "a@b.com",         // 纯手机账号则整个字段省略
  "display_name": "张三",
  "avatar_url": "",           // 未实现头像 → 恒返回空串 ""（不是 null）
  "status": "active",         // UserStatus::Active→"active"，其余→"disabled"
  "roles": ["student"],       // Role[] = ("student"|"teacher"|"admin")[]；后端暂只有单角色就返回 [last_active_role]
  "last_active_role": "student"
}
```

### 0.2 refresh token 走 httpOnly Cookie（不是 body！）

前端 http 层 `credentials:"include"`，refresh/logout **不带 body**、全靠 cookie。

#### 为什么改：问题在"存"，不在"传"

**不是**因为"body 传输不安全"。请求体走 HTTPS 是加密的，不进 URL、不落 access log、不进浏览器
历史和 Referer——比 query param 正确得多，这条理由站不住。

真正的理由是：**响应体里返回 refresh token 明文，等于逼前端把它存在 JS 能读的地方**
（localStorage / sessionStorage / 内存）。refresh 是长期凭证（`REFRESH_TOKEN_TTL_DAYS=30`，
整整 30 天），任何一个 XSS 读走它，就能把它带离受害者浏览器，在自己机器上一直换 access token，
直到 30 天到期或被手动吊销。改成 httpOnly 后 JS 读不到这个值，**外泄**这条路才断。

别高估这次改动：XSS 仍然可以在受害者页面里直接 `fetch('/api/v1/auth/refresh')`，浏览器自动带上
cookie，照样拿到 access token。区别在于攻击者必须寄生在受害者的页面上下文里，关掉页面就没了，
拿不到可离线复用的凭证。所以这是把「永久账号接管」降级成「会话期内滥用」——实打实的收益，
但不等于"改完就安全了"。

代价是引入 CSRF：cookie 由浏览器自动携带。下面属性表里的 `SameSite=Lax` + POST + `Path` 收窄
就是为了挡它（Lax 不会在跨站 POST 时发送 cookie）。

#### 后端要做的

- **login / register(自动登录) / refresh** 成功时 `Set-Cookie` 下发 refresh token。
- **refresh / logout** 从 **Cookie** 读 refresh token（现在从 body `{refresh_token}` 读，要改）。
- Cookie 属性：
  - `HttpOnly`（JS 读不到 → 防的是 XSS **外泄**，不是 XSS 本身，见上）
  - `SameSite=Lax`（跨站 POST 不带 cookie → 防 CSRF）
  - `Path=/api/v1/auth`（refresh/logout 都在这个前缀下，浏览器才会带上）
  - `Max-Age = refresh TTL 秒`
  - **`Secure` 只在生产（https）开**：本地联调是 `http://localhost:3000` 经 Next 代理，
    带 `Secure` 浏览器会直接丢弃 cookie → refresh 永远拿不到。用 env 开关（如 `COOKIE_SECURE`）。
  - logout 时下发 `Max-Age=0` 的同名 cookie 清除它。
- ⚠️ **body 里不要再返回 `refresh_token` 明文**（现在 `Token` 结构体带它，要去掉）。access_token 仍在 body。

> ⚠️ **将来前端若直连后端域名（不走 Next 代理）而需要加 CORS**：开了 `allow_credentials` 就
> **绝不能**配 `Allow-Origin: *`，必须是精确 origin 白名单。浏览器会直接拒绝这个组合，而且
> `*` + 凭证本身就等于把带 cookie 的接口开放给任意站点。目前代码里没有 CORS layer，走 Next
> `rewrites()` 算同源，暂时不需要——等直连那天这条立刻变成必做项。

> `Path=/api/v1/auth` 会让 `GET /api/v1/auth/me` 也带上 refresh cookie（无谓暴露，风险不大）。
> T2 已经计划把 me 挪到顶层 `/api/v1/me`，挪完这条自然消失，不用额外处理。

> Cookie 能否穿透 Next 代理：Next.js `rewrites()` 会转发请求的 `Cookie` 头、也会把后端的
> `Set-Cookie` 透传回浏览器（作用域落在 localhost:3000）。实现后需实测一次（见 §4 验收）。

### 0.3 错误体 `{"error": "..."}` —— **已吻合，不用改** ✅

后端 `AppError`（src/error.rs:60）序列化即 `{"error": msg}`，前端 `parseError` 正好读 `body.error`。
`code`/`details` 前端视为可选，登录流程用不到。

---

## P0 —— 让 login 端到端跑通（最小可用）

### T1. `POST /api/v1/auth/login`（路径 ✅ 请求 ✅，只改**响应**）

- 请求不变：`{ "identifier": "...", "password": "..." }`
- **响应改成** `AuthResponse`：
  ```jsonc
  {
    "user": { UserPublic },          // 见 0.1；不再是扁平的 id/email/phone
    "access_token": "...",           // 顶层，不再套在 token:{} 里
    "active_role": "student",        // = user.last_active_role
    "expires_in": 900,               // access token 剩余秒数
    "refresh_token_expires_at": 1752566400  // refresh 过期的 Unix 秒
  }
  ```
- refresh token → 走 Cookie（0.2），**不进 body**。
- 现状：返回 `{id,email,phone,token:{access_token,refresh_token,token_type,expires_in}}`（src/auth/handler.rs `LoginResponse`/`Token`/`build_login_response`）。
- ⚠️ `login` 与 `login_otp` 共用 `build_login_response` —— 这里改好，T8 的响应自动一起对齐。

> P0 依赖 0.1 `UserPublic` + 0.2 cookie 助手。做完 T1 前端登录页即可拿 token、存本地、显示用户。

---

## P1 —— 会话生命周期（登录后能续期、能读当前用户、能登出）

### T2. me：`GET /api/v1/auth/me` → **改路由为 `GET /api/v1/me`**

- 前端调 `/me`（`NEXT_PUBLIC_API_BASE_URL=/api/v1` → `/api/v1/me`），现在挂在 auth nest 下是 `/api/v1/auth/me`。
  把这条路由从 `/api/v1/auth` nest 挪到顶层 `/api/v1/me`（src/lib.rs router）。
- **响应改成** `MeResponse`：
  ```jsonc
  {
    "user": { UserPublic },              // 见 0.1；不再是 Profile{id,name,...}
    "active_role": "student",
    "learning_settings": null,           // 未实现 → 恒 null（字段必须在，否则前端类型崩）
    "onboarded": false                   // learning_settings==null 时 false
  }
  ```
- 现状：返回 `Profile{id,name,email,phone,role}`（src/auth/handler.rs）。鉴权提取器 `AuthUser`（Bearer）不变 ✅。

### T3. refresh：`POST /api/v1/auth/refresh`（路径 ✅，改**入参来源 + 响应**）

- 入参：从 **Cookie** 读 refresh token（删掉 `RefreshTokenRequest{refresh_token}` body）。
- **响应改成** `RefreshResponse`：
  ```jsonc
  { "access_token": "...", "expires_in": 900, "refresh_token_expires_at": 1752566400 }
  ```
  （现在返回 `{token:{...}}` 嵌套）
- 轮换 refresh 后，`Set-Cookie` 下发新 refresh（0.2）。

#### ⚠️ 顺带补上重放检测（与前端契约无关，但优先级高于 0.2 的 cookie 改造）

**现状丢了一个泄露信号。** `SessionService::rotate`（src/session/service.rs:63）拿到 `consume` 的
`None` 就直接 `InvalidRefreshToken` → 401，把三种情况混成同一个结果：token 不存在 / 已过期 /
**已经被用过**。第三种是"这条 token 链已泄露"的告警，现在被当成普通 401 扔了。

**判据是现成的。** `consume`（src/session/repository.rs:100）是 CAS 盖 `rotated_at`，**行保留不删**，
所以 `find_by_hash`（src/session/repository.rs:43）命中且 `rotated_at IS NOT NULL`，就说明这枚已被
消费过的 token 又被送来了一次。

**该怎么处理。** 按 OAuth 2.0 Security BCP（RFC 9700 §4.14.2）：refresh token 是一次性的，一枚被用
两次只有两种可能——攻击者在重放偷来的旧枚，或合法用户在重放（因为攻击者已抢先用过一次）。两种都意味着
这条 token 链已泄露，且**你无法分辨谁是谁**，所以正确动作是 `revoke_all_for_user(user_id)`
（src/session/repository.rs:86，已实现）吊销该用户全部会话 + 打日志告警，而不是只回 401 让攻击者
继续拿着链上别的枚接着用。

rotate 里要接的逻辑：`consume` 落空 → `find_by_hash` 兜一下 → 命中且 `rotated_at` 非空 → 吊销全家
 + 告警 → 仍返回 `InvalidRefreshToken`。**对外错误保持统一**（别告诉攻击者你识破了），区别只在服务端
的吊销与日志。

> 已知误伤：合法客户端并发刷新（比如两个 tab 同时 401 同时 refresh）会拿同一枚 token 请求两次，
> 触发检测 → 用户全端被登出。要么前端保证 refresh 单飞（同一时刻只允许一个 refresh 在途，其余等它），
> 要么服务端给个几秒宽限窗口（`rotated_at` 在 N 秒内的重放放行、返回同一枚新 token）。
> 前端单飞更干净，优先走这条；实现前跟前端确认它的 http 层是否已经这么做了。

### T4. logout：`POST /api/v1/auth/logout`（路径 ✅，改**入参来源 + 返回码**）

- 入参：从 **Cookie** 读 refresh token（删 body）。吊销之。
- 下发 `Max-Age=0` 清除 refresh cookie。
- 返回 **204 No Content**（前端 `logout: () => http.post<void>`，http 层对 204 返回 undefined；勿返回带 body 的 200，会走 `res.json()`）。

---

## P2 —— 验证码登录 & 注册（次要，登录页的其它入口）

### T5. send-code：`POST /api/v1/otp/send` → **改路由为 `POST /api/v1/auth/send-code`**

- 路由挪进 auth nest：`/api/v1/auth/send-code`。
- 请求改成前端形状 `{ "identifier": "..." }`（单字段，手机/邮箱自动判别 —— 复用 `normalize_identifier`；
  现在要 `{phone?,email?,purpose}`）。`purpose` 前端不传 → 后端**默认 login OTP**。
- **响应必须带 JSON body** `{ "status": "sent" }`（现在返回 202 **空 body**，前端 `sendCode` 会 `res.json()`
  → 空 body 抛错。要么返回 `Json({status})` 200/202，要么返回 204。选前者，前端类型是 `{status}`）。

### T6. login by code：`POST /api/v1/auth/login-otp` → **改路由为 `POST /api/v1/auth/login/code`**

- 路由重命名 `login-otp` → `login/code`。
- 请求 `{ "identifier": "...", "code": "..." }` —— **已吻合** ✅。
- 响应：与 T1 同（共用 `build_login_response`，T1 改完这里自动对齐）。

### T7. register：`POST /api/v1/user/register` → **改路由为 `POST /api/v1/auth/register`**

- 路由从 `/api/v1/user` nest 挪到 `/api/v1/auth/register`。
- 请求补齐前端字段（现在只有 `{phone?,email?,password}`）：
  ```jsonc
  {
    "phone": "...",          // phone/email 二选一
    "email": "...",
    "password": "...",
    "display_name": "张三",   // 必填，trim 后 1–50 字符
    "role": "student",       // "student" | "teacher"
    "code": "123456"         // 可选：验证码注册校验
  }
  ```
- 行为：注册后**自动登录** —— 发 token + Set-Cookie refresh，返回 **`AuthResponse`**（同 T1），
  不再是 `RegisterResponse{user_id,display_name,role}`。

---

## §3 横切事项（跟着上面一起做，别漏）

- **openapi 注解**：每个 handler 上的 `#[utoipa::path(path="...")]` 硬编码了 nest 前缀
  （见 src/openapi.rs 头注释），改路由要同步改注解；`src/openapi.rs` 有「所有注册路由都在」的
  快照测试，路径没同步会红。改一处对一处。
- **集成测试**：现有 `tests/`（如有）打的是旧路径/旧形状，会全红。**这部分我来改/补**——
  你实现完告诉我，我按新契约重写：login 响应形状、cookie 下发与回读、refresh 轮换、me 形状、
  register 自动登录、send-code JSON body、登出清 cookie。
- **`Token`/`Profile`/`RegisterResponse`/`LoginResponse` 等旧结构体**：改造后多半被 `UserPublic` +
  `AuthResponse`/`RefreshResponse`/`MeResponse` 取代，注意清理无用结构避免 dead_code 警告。

## §4 验收（每条做完这样验）

- 单接口：`curl` 打新路径，比对响应 JSON 字段名/嵌套与本文一致；`-i` 看 `Set-Cookie` 是否带
  `HttpOnly; SameSite=Lax; Path=/api/v1/auth`、dev 下**不带** `Secure`。
- 端到端（联调）：本地前端 3000（Turbopack 已修，`pnpm dev`）→ 登录页走一遍
  注册/登录/刷新/登出；DevTools → Application → Cookies 看 refresh cookie 落在 localhost:3000、
  Network 看 refresh 请求自动带 Cookie。**联调时关本机 Clash 代理**（TUN 劫持）。
- 回归：我补的集成测试 `cargo test` 全绿。

---

## 附：改动路径速查

| 待办 | 旧 | 新 |
|---|---|---|
| T1 login | `/api/v1/auth/login` 响应扁平+token嵌套 | 同路径，响应 `AuthResponse`，refresh 进 cookie |
| T2 me | `GET /api/v1/auth/me` | `GET /api/v1/me`，响应 `MeResponse` |
| T3 refresh | body `{refresh_token}` → `{token:{}}` | cookie 读 → 响应 `RefreshResponse` |
| T4 logout | body `{refresh_token}` | cookie 读 + 清 cookie，返回 204 |
| T5 send-code | `POST /api/v1/otp/send` `{phone?,email?,purpose}` 202空body | `POST /api/v1/auth/send-code` `{identifier}` → `{status}` |
| T6 login-otp | `POST /api/v1/auth/login-otp` | `POST /api/v1/auth/login/code`，响应 `AuthResponse` |
| T7 register | `POST /api/v1/user/register` `{phone?,email?,password}` | `POST /api/v1/auth/register` 补 `display_name/role/code?`，自动登录返回 `AuthResponse` |
