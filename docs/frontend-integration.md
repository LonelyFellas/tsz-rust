# 前端对接文档（auth / user / otp）

> **状态**：对应后端 commit `e945f05`（2026-07-18），已部署 `tshb-test` 并冒烟验证。
> 本文是**前端消费视角**的对接参考；后端内部的契约任务清单在
> `frontend-contract-alignment.md`（T 系列），两者冲突时以本文 + 线上实测为准。
> 文中所有响应示例都是生产环境的真实返回（脱敏）。

---

## 1. 环境与基址

| 环境 | API 基址 | 说明 |
|---|---|---|
| 生产（临时） | `http://47.121.142.19:8383/api/v1` | 域名备案中；**无 TLS**；安全组按 IP 白名单放行 8383，本机 IP 变了要去 Aliyun 控制台加 `/32` |
| 本地后端 | `http://127.0.0.1:8383/api/v1` | `cargo run`（需本地 Docker Postgres 5433 + Redis） |

**⚠️ CORS 后端尚未实现。** 浏览器直连跨域必被拦，前端 dev 必须走同源代理。
Next.js 在 `next.config.ts` 加 rewrites：

```ts
async rewrites() {
  return [{
    source: "/api/v1/:path*",
    destination: "http://47.121.142.19:8383/api/v1/:path*",
  }];
}
```

前端环境变量保持 `NEXT_PUBLIC_API_BASE_URL=/api/v1`（同源相对路径）。
走代理还有一个关键收益：**refresh cookie 变成同源 cookie**，`SameSite=Lax` 与
`credentials: "include"` 都不会有跨站问题。

> 本机开着 Clash/Surge TUN 模式时，请求可能被劫走非白名单出口 → 连不上 47.121.142.19。
> 联调时关代理，或给该 IP 加 DIRECT 规则。

---

## 2. 认证模型（一句话版）

**access token（JWT，15 分钟）放内存，refresh token 走 httpOnly Cookie（30 天）你碰不到。**

- access token：登录/刷新的响应 body 里返回，前端存**内存**（不要 localStorage），
  请求头自拼 `Authorization: Bearer <access_token>`（**响应里没有 `token_type` 字段**）。
- refresh token：后端 `Set-Cookie` 下发，`HttpOnly; SameSite=Lax; Path=/api/v1/auth;
  Max-Age=2592000`。JS 读不到、也不需要读；只要 fetch 带 `credentials: "include"`，
  浏览器自动在 `/api/v1/auth/*` 路径下携带。
- 现阶段 cookie **无 `Secure` 属性**（服务器无 TLS 的临时配置）；上域名 + TLS 后会加回，
  前端无需感知。

```
登录            POST /auth/login (或 /auth/login-otp)
                ├─ body ← { user, access_token, expires_in, refresh_token_expires_at }
                └─ Set-Cookie ← refresh_token（httpOnly）
续期(静默)      POST /auth/refresh   ← 无 body，全靠 cookie
                ├─ body ← { access_token, expires_in, refresh_token_expires_at }
                └─ Set-Cookie ← 新 refresh_token（旧的立即作废，单次使用）
登出            POST /auth/logout    ← 无 body，恒 204，清 cookie
```

---

## 3. 端点参考

### 3.1 `POST /api/v1/auth/register` — 手机号注册并登录

```jsonc
// code 由 3.4 的 /otp/send 以 purpose="register" 获取
{ "phone": "13800138000", "password": "P@ssw0rd!", "code": "123456" }
```

```jsonc
// 201：响应与 3.2 登录完全同形状
{
  "user": {
    "id": "019f731b-bdad-7881-a309-28730af7ce8d",
    "display_name": "文艺天鹅0329",
    "phone": "13800138000",
    "avatar_url": "",
    "roles": ["student"],
    "active_role": "student"
  },
  "access_token": "eyJ0eXAiOiJKV1Qi...",
  "expires_in": 900,
  "refresh_token_expires_at": 1786957219
}
// + Set-Cookie: refresh_token=...; HttpOnly; SameSite=Lax; Path=/api/v1/auth
```

- 当前注册**只支持手机号**，不接收 email；注册角色恒为 student，昵称由后端生成。
- 注册成功已经建立会话，前端不得再调用一次 login。
- 错误：400 手机号/密码非法；401 验证码无效或过期；409 手机号已占用；
  429 验码频控；503 验证码基础设施或密码哈希不可用。

### 3.2 `POST /api/v1/auth/login` — 密码登录

```jsonc
// 请求字段是 identifier（统一承接手机号或邮箱），不叫 phone/email —— 用错必 422
{ "identifier": "13800138000", "password": "P@ssw0rd!" }
```

```jsonc
// 200（生产真实响应）
{
  "user": {
    "id": "019f731b-bdad-7881-a309-28730af7ce8d",
    "display_name": "文艺天鹅0329",
    "phone": "19900000001",        // 纯邮箱账号则此键整个不存在
    "avatar_url": "",              // 头像未实现：恒 ""（不是 null、不省略）
    "roles": ["student"],
    "active_role": "student"       // ⚠️ 在 user 内，顶层没有！
  },
  "access_token": "eyJ0eXAiOiJKV1Qi...",
  "expires_in": 900,                        // access 有效期（秒）
  "refresh_token_expires_at": 1786957219    // refresh 过期时刻（Unix 秒，绝对时间戳）
}
// + Set-Cookie: refresh_token=...; HttpOnly; SameSite=Lax; Path=/api/v1/auth; Max-Age=2592000
```

- 错误：401 `invalid_credentials` Problem（完整结构见 `docs/api-errors.md`）——账号不存在与密码错误**响应逐字节一致**、
  不可区分（前端别试图提示"账号不存在"）；403——密码正确但账号被禁用（密码已验证过，
  这一种可如实提示"账号已被禁用"）。

### 3.3 `POST /api/v1/auth/login-otp` — 验证码登录

```jsonc
{ "identifier": "13800138000", "code": "123456" }
```

响应与 3.2 **完全同形状**（含 Set-Cookie）。错误：401 `invalid code`（错码/过期/超次统一）；429 限流。

### 3.4 `POST /api/v1/otp/send` — 发验证码

```jsonc
// phone / email 二选一；purpose 为 snake_case 枚举：
// "login" | "register" | "password_reset" | "account_deletion" | "contact_bind"
{ "phone": "13800138000", "purpose": "login" }
```

- 202 Accepted（无 body）。
- 错误：429（60s 冷却 / 24h 日限 10 次）；503（Redis 不可用，fail-close）。
- **⚠️ 短信通道未接，OTP 是 Mock**：验证码不会真发到手机，打在服务器日志里。
  联调时后端同学取码：`ssh tshb-test 'journalctl -u tsz-rust -n 50 | grep otp_code_sent'`。

### 3.5 `POST /api/v1/auth/refresh` — 静默续期

**无请求 body**，凭 cookie（`credentials: "include"`）。

```jsonc
// 200
{ "access_token": "eyJ...", "expires_in": 900, "refresh_token_expires_at": 1786957219 }
// + Set-Cookie: 新 refresh_token（旧枚已作废）
```

- 401 `invalid_refresh_token` Problem：cookie 缺失/过期/已用过/用户被禁用，
  **一律同一响应不可区分** → 前端统一当"会话失效"处理：清内存 token、跳登录页。

### 3.6 `POST /api/v1/auth/logout` — 登出

**无请求 body**，凭 cookie。**恒 204**（幂等：没 cookie、token 早失效都是 204），
带清除 cookie 的 `Set-Cookie`（Max-Age=0）。前端登出逻辑不需要任何错误分支。
登出只杀当前设备的会话，不影响其它设备。

### 3.7 `GET /api/v1/auth/me` — 当前用户

请求头：`Authorization: Bearer <access_token>`。

```jsonc
// 200 —— 就是 3.2 里 user 对象的形状（同一个 UserPublic），暂时不包壳
{
  "id": "019f731b-...",
  "display_name": "文艺天鹅0329",
  "phone": "19900000001",
  "avatar_url": "",
  "roles": ["student"],
  "active_role": "student"
}
```

- 401：token 缺失/过期/无效/用户被禁用。
- **⚠️ 此端点近期会变（后端 T2）**：路由将挪到 `GET /api/v1/me`，响应将包成
  `{ user: {...}, learning_settings: null, onboarded: false }`。前端如果现在就接，
  建议把 me 的调用和解析收敛在一个函数里，到时只改一处。

---

## 4. TypeScript 类型（以此为准）

```ts
/** 与后端 UserProfile 序列化逐字段对齐 */
interface UserPublic {
  id: string;
  display_name: string;
  email?: string;        // 无邮箱 → 键不存在（不是 null）
  phone?: string;        // 无手机 → 键不存在（不是 null）
  avatar_url: string;    // 头像未实现：恒 ""
  roles: Role[];         // 现阶段只会出现 "student" | "teacher"
  active_role: Role;
}
type Role = "student" | "teacher" | "admin"; // admin 后端暂未实现，保留联合项无妨

interface AuthResponse {           // login / login-otp
  user: UserPublic;
  access_token: string;
  expires_in: number;              // 秒
  refresh_token_expires_at: number; // Unix 秒！new Date(x * 1000)
}

interface RefreshResponse {        // refresh
  access_token: string;
  expires_in: number;
  refresh_token_expires_at: number;
}

interface ApiError { error: string; }
```

### 与前端现有类型（`packages/types/src/user.ts`）的已知偏离——需要改前端类型

| 前端类型现状 | 后端实际（拍板 2026-07-18） | 前端动作 |
|---|---|---|
| `active_role` 在 `AuthResponse`/`MeResponse` **顶层** | 只在 **`user` 内** | 类型和读取点改为 `user.active_role` |
| `User.status/created_at/updated_at` 必填 | **不下发**（TS 编译期不报，运行时 `undefined`！） | 类型改可选或删掉；前端真要用时后端加回是三行 |
| `email/phone` 可能按 `null` 处理 | `None` → **键省略** | 用 `user.email ?? undefined` 语义，别 `=== null` |
| `token_type: "Bearer"` | **无此字段** | 前端自拼 `"Bearer " + access_token` |

---

## 5. http 层实现要点（强烈建议照做）

```ts
// 1) 所有请求带 cookie（走代理后同源，无跨站问题）
fetch(url, { credentials: "include", ... });

// 2) access token 只放内存（模块级变量/内存 store）。刷新页面后内存丢失
//    → 启动时先调一次 refresh 恢复会话（cookie 还在就能拿回新 access token）。

// 3) 401 拦截器：refresh → 重试一次 → 仍失败才登出
async function withAuth(req: () => Promise<Response>): Promise<Response> {
  let resp = await req();
  if (resp.status === 401) {
    const ok = await refreshOnce();      // 见第 4 点
    if (!ok) { hardLogout(); return resp; }
    resp = await req();                  // 只重试一次，别循环
  }
  return resp;
}

// 4) ⚠️ refresh 必须全局去重（single-flight）：
//    refresh token 是单次使用的——并发打两个 /auth/refresh，后到的那个用的是
//    已作废的旧 cookie，必 401；更糟的是超过 20 秒宽限窗口的旧 cookie 重放
//    会触发后端的盗用判定，把该用户所有设备的会话全部吊销。
//    多标签页同时刷新同理（可用 BroadcastChannel / Web Locks 协调，或接受偶发重登）。
let inflight: Promise<boolean> | null = null;
function refreshOnce(): Promise<boolean> {
  inflight ??= doRefresh().finally(() => { inflight = null; });
  return inflight;
}

// 5) 到期前主动续：expires_in=900s，建议 ~13 分钟定时静默 refresh，
//    比等 401 被动刷体验好。refresh_token_expires_at（×1000 转毫秒）是
//    "不再登录的话会话的绝对终点"，可用来提前提示重新登录。
```

其它注意：

- **错误 body 统一使用 Problem Details**：`type/title/status/detail/code` 与可选 `field`，媒体类型为
  `application/problem+json`。422 使用同一结构，且固定为 `invalid_request_body`，遇到时先检查请求字段拼写或类型。
- 各失败态文案刻意**不可区分**（防账号枚举/防 token 状态探测），前端按状态码处理即可，
  不要解析 `title` 或 `detail` 文案做分支；业务判断读取稳定 `code`。
- JWT 的 payload 前端可以解出来看（sub/role/exp），但**不要**据此做权限判断，
  以 `user.roles`/`user.active_role` 为准。

---

## 6. 已知待办（会影响前端的部分）

| 事项 | 影响 | 状态 |
|---|---|---|
| CORS 层 | 前端现在必须走 dev 代理 | 未做（方案待定：后端 CorsLayer vs 长期代理） |
| T2：me 挪路由 + `MeResponse` 包壳 | `/auth/me` → `/me`，响应加壳 | 未做，前端收敛调用点即可 |
| T5：注册自动登录 | 注册后免二次 login | 未做，现在注册后要手动 login |
| 真实短信通道 | OTP 目前只能从服务器日志取码 | 未做 |
| 域名 + TLS | 基址换域名、cookie 加 Secure | 备案审核中 |
| 头像上传 | `avatar_url` 将出现真实 URL | 未排期，前端先做空串兜底（显示默认头像） |

---

*维护约定：auth 相关响应形状变更时同步本文档 §3/§4；后端契约任务进度看
`frontend-contract-alignment.md`。*
