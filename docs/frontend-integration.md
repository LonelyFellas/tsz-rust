# 前端对接文档（auth / user / otp / admin 配置提案）

> **状态**：对应后端 commit `e945f05`（2026-07-18），已部署 `tshb-test` 并冒烟验证。
> 本文是**前端消费视角**的对接参考；后端内部的契约任务清单在
> `frontend-contract-alignment.md`（T 系列），两者冲突时以本文 + 线上实测为准。
> 除明确标记为“后端待实现”的提案章节外，文中响应示例都是生产环境的真实返回（脱敏）。

---

## 1. 环境与基址

| 环境         | API 基址                           | 说明                                                                                        |
| ------------ | ---------------------------------- | ------------------------------------------------------------------------------------------- |
| 生产（临时） | `http://47.121.142.19:8383/api/v1` | 域名备案中；**无 TLS**；安全组按 IP 白名单放行 8383，本机 IP 变了要去 Aliyun 控制台加 `/32` |
| 本地后端     | `http://127.0.0.1:8383/api/v1`     | `cargo run`（需本地 Docker Postgres 5433 + Redis）                                          |

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
    "active_role": "student",
  },
  "access_token": "eyJ0eXAiOiJKV1Qi...",
  "expires_in": 900,
  "refresh_token_expires_at": 1786957219,
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
    "phone": "19900000001", // 纯邮箱账号则此键整个不存在
    "avatar_url": "", // 头像未实现：恒 ""（不是 null、不省略）
    "roles": ["student"],
    "active_role": "student", // ⚠️ 在 user 内，顶层没有！
  },
  "access_token": "eyJ0eXAiOiJKV1Qi...",
  "expires_in": 900, // access 有效期（秒）
  "refresh_token_expires_at": 1786957219, // refresh 过期时刻（Unix 秒，绝对时间戳）
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
- **⚠️ 短信通道未接，OTP 是 Mock**：验证码不会真发到手机，测试环境统一输入 `000000`。
  接入阿里云短信后，真实通道必须恢复为随机验证码。

### 3.5 `POST /api/v1/auth/refresh` — 静默续期

**无请求 body**，凭 cookie（`credentials: "include"`）。

```jsonc
// 200
{
  "access_token": "eyJ...",
  "expires_in": 900,
  "refresh_token_expires_at": 1786957219,
}
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
  "active_role": "student",
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
  email?: string; // 无邮箱 → 键不存在（不是 null）
  phone?: string; // 无手机 → 键不存在（不是 null）
  avatar_url: string; // 头像未实现：恒 ""
  roles: Role[]; // 现阶段只会出现 "student" | "teacher"
  active_role: Role;
}
type Role = "student" | "teacher" | "admin"; // admin 后端暂未实现，保留联合项无妨

interface AuthResponse {
  // login / login-otp
  user: UserPublic;
  access_token: string;
  expires_in: number; // 秒
  refresh_token_expires_at: number; // Unix 秒！new Date(x * 1000)
}

interface RefreshResponse {
  // refresh
  access_token: string;
  expires_in: number;
  refresh_token_expires_at: number;
}

interface ApiError {
  error: string;
}
```

### 与前端现有类型（`packages/types/src/user.ts`）的已知偏离——需要改前端类型

| 前端类型现状                                          | 后端实际（拍板 2026-07-18）                       | 前端动作                                         |
| ----------------------------------------------------- | ------------------------------------------------- | ------------------------------------------------ |
| `active_role` 在 `AuthResponse`/`MeResponse` **顶层** | 只在 **`user` 内**                                | 类型和读取点改为 `user.active_role`              |
| `User.status/created_at/updated_at` 必填              | **不下发**（TS 编译期不报，运行时 `undefined`！） | 类型改可选或删掉；前端真要用时后端加回是三行     |
| `email/phone` 可能按 `null` 处理                      | `None` → **键省略**                               | 用 `user.email ?? undefined` 语义，别 `=== null` |
| `token_type: "Bearer"`                                | **无此字段**                                      | 前端自拼 `"Bearer " + access_token`              |

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

| 事项                              | 影响                         | 状态                                         |
| --------------------------------- | ---------------------------- | -------------------------------------------- |
| CORS 层                           | 前端现在必须走 dev 代理      | 未做（方案待定：后端 CorsLayer vs 长期代理） |
| T2：me 挪路由 + `MeResponse` 包壳 | `/auth/me` → `/me`，响应加壳 | 未做，前端收敛调用点即可                     |
| T5：注册自动登录                  | 注册后免二次 login           | 未做，现在注册后要手动 login                 |
| 真实短信通道                      | OTP 目前只能从服务器日志取码 | 未做                                         |
| 域名 + TLS                        | 基址换域名、cookie 加 Secure | 备案审核中                                   |
| 头像上传                          | `avatar_url` 将出现真实 URL  | 未排期，前端先做空串兜底（显示默认头像）     |

---

## 7. 系统设置 / 词性配置（前端 Mock，后端待实现）

> **状态（2026-08-10）**：前端 `feat/refactor-word-creation` 先实现 contract-shaped mock；
> 本节是后端接口提案，当前 `openapi.json` 尚无这些端点，服务端也未实现。
> 后端实现时需先评审本节并生成 OpenAPI；前端随后移除 contract test 的 PENDING 项。

### 7.1 业务目标与权限

管理后台左侧增加“系统设置 → 词性配置”。配置目录是智能词库中基本词性和细分词性的唯一展示来源：

- 前端 Mock 管理页顶部使用“基本词性 / 细分词性”Tab 分别管理两个目录层级；这两个 Tab 仅用于系统配置，不表示单词/短语类型；
- 所有已登录 admin 可读取 catalog，供词条列表、筛选、创建、编辑和预览使用；
- 首期仅 `super_admin` 可进入配置页面并执行增删改；普通 admin 直调管理/写端点返回 403；
- 单词、短语和词义只保存稳定编码，不保存中文名、英文名或缩写快照；
- 编码创建后不可修改，展示字段和排序可修改；
- 基本词性已被任意草稿或仍保留 publication 的单词/短语引用时不可删除；细分词性已被当前
  草稿或仍保留 publication 的词义引用时不可删除；
- 删除未引用基本词性时，在同一事务内级联删除其未引用细分词性；
- 内置词典供应商的词性值由后端映射为平台编码，检测响应不得返回 catalog 中不存在的编码。

后端当前无 RBAC，权限判断沿用 `AdminRole`：catalog 只要求有效 admin access token，管理与写端点要求 `role == super_admin`。

### 7.2 稳定编码与数据模型

默认数据需覆盖前端现有存量：

- 11 个基本词性编码：`noun`、`pronoun`、`verb`、`adjective`、`adverb`、`preposition`、`article`、`determiner`、`conjunction`、`numeral`、`interjection`；
- 19 个细分词性编码：`V-T`、`V-I`、`V-LINK`、`AUX`、`MODAL`、`ADJ`、`ADV`、`N-COUNT`、`N-UNCOUNT`、`N-PROPER`、`N-PLURAL`、`N-SING`、`PRON`、`PREP`、`CONJ`、`DET`、`ART`、`NUM`、`INT`；
- 细分词性必须归属一个基本词性，例如 `N-COUNT` 属于 `noun`、`V-T` 属于 `verb`。

种子的中文名、英文名、缩写和 sort_order 以
[`part-of-speech-config-design.md` §3](part-of-speech-config-design.md#3-默认数据) 为准；基本词性
英文名初始值为 `NOUN`、`VERB` 等全大写展示值，与当前前端 fixture 一致。

建议 wire（字段必须保持 snake_case）：

```ts
interface PartOfSpeechConfig {
  id: string; // UUID
  code: string; // 不可修改；如 noun
  name_zh: string; // 名词
  name_en: string; // NOUN / Noun（展示字段）
  abbreviation: string; // n.
  sort_order: number;
  usage_count: number; // 管理页提示；删除时仍须重新查引用
  sub_part_count: number;
  revision: number;
  created_by: { id: string; display_name: string };
  created_at: string; // RFC 3339
  updated_by?: { id: string; display_name: string };
  updated_at: string;
}

interface SubPartOfSpeechConfig {
  id: string;
  part_of_speech_id: string;
  code: string; // 不可修改；如 N-COUNT
  name_zh: string; // 可数名词
  name_en: string; // Countable noun
  sort_order: number;
  usage_count: number;
  revision: number;
  created_by: { id: string; display_name: string };
  created_at: string;
  updated_by?: { id: string; display_name: string };
  updated_at: string;
}
```

`created_by.id` / `updated_by.id` 在 wire 中是 string，因为系统种子使用 `"system"`；普通管理员
actor 才是 UUID 字符串。尚未修改的记录省略 `updated_by`，不输出 `null`。

校验基线：

- 基本编码：`^[a-z][a-z0-9_]{0,31}$`；
- 细分编码：`^[A-Z][A-Z0-9_-]{0,31}$`；
- 中文名、英文名长度 1–64，英文缩写长度 1–16，写入前 trim；
- 基本编码、基本中文名全局唯一，基本英文名和缩写忽略大小写后全局唯一；细分编码全局唯一，
  细分中文名和英文名只在同一基本词性下唯一，英文比较忽略大小写；
- `sort_order` 为有符号 32 位整数；相同排序按 `created_at`、`id` 稳定排序；
- PATCH 不接受 code，避免历史引用断裂。

### 7.3 读取 catalog

`GET /api/v1/admin/settings/parts-of-speech/catalog`

- 权限：任意有效 admin Bearer；
- 不分页，返回所有基本词性和嵌套细分词性；
- 用于业务页面高频只读缓存，不返回审计和 usage 字段；
- 200 响应：

```jsonc
{
  "catalog_version": 12,
  "items": [
    {
      "id": "019f-pos-noun",
      "code": "noun",
      "name_zh": "名词",
      "name_en": "NOUN",
      "abbreviation": "n.",
      "sort_order": 10,
      "sub_parts": [
        {
          "id": "019f-sub-n-count",
          "code": "N-COUNT",
          "name_zh": "可数名词",
          "name_en": "Countable noun",
          "sort_order": 10,
        },
      ],
    },
  ],
}
```

任何基本/细分配置创建、修改或删除成功后，`catalog_version` 单调递增。该值是未来条件请求或
跨管理员刷新可使用的不透明 change token；当前前端仍采用 5 分钟 staleTime 和本机 mutation
失效缓存，并未主动轮询版本。响应按 `sort_order`、`created_at`、`id` 排序，前端应保留相同
sort_order 项的服务端相对顺序。后端保证 `catalog_version` 和 `items` 来自同一个数据库快照，
不会返回版本号与目录内容错代的响应。错误：401、403（账号禁用/必须改密）、500。

九个端点的成功契约汇总如下；状态码和响应信封都属于稳定 wire：

| 方法   | 相对路径                                                                     | 状态 | 响应                           |
| ------ | ---------------------------------------------------------------------------- | ---: | ------------------------------ |
| GET    | `/settings/parts-of-speech/catalog`                                          |  200 | `{ catalog_version, items }`   |
| GET    | `/settings/parts-of-speech`                                                  |  200 | `{ items, pagination }`        |
| POST   | `/settings/parts-of-speech`                                                  |  201 | 完整 `PartOfSpeechConfig`      |
| PATCH  | `/settings/parts-of-speech/{id}`                                             |  200 | 完整新 `PartOfSpeechConfig`    |
| DELETE | `/settings/parts-of-speech/{id}?base_revision={revision}`                    |  204 | 无 body                        |
| GET    | `/settings/parts-of-speech/{id}/sub-parts`                                   |  200 | `{ items }`                    |
| POST   | `/settings/parts-of-speech/{id}/sub-parts`                                   |  201 | 完整 `SubPartOfSpeechConfig`   |
| PATCH  | `/settings/parts-of-speech/{id}/sub-parts/{sub_id}`                          |  200 | 完整新 `SubPartOfSpeechConfig` |
| DELETE | `/settings/parts-of-speech/{id}/sub-parts/{sub_id}?base_revision={revision}` |  204 | 无 body                        |

204 不返回 `{}` 或 `null`。

### 7.4 基本词性管理

#### `GET /api/v1/admin/settings/parts-of-speech`

权限：`super_admin`。查询参数：

- `q`：可选，trim 后忽略大小写做字面子串匹配 code / 中文名 / 英文名 / abbreviation；`%`、`_`
  不作为 SQL 通配符；
- `page`：默认 1；
- `page_size`：默认 10，范围 1–100。

非法分页参数返回 400 `invalid_query`，不静默 clamp。

响应沿用后台分页信封：

```jsonc
{
  "items": [/* PartOfSpeechConfig */],
  "pagination": {
    "page": 1,
    "page_size": 10,
    "total": 11,
    "total_pages": 2,
  },
}
```

#### `POST /api/v1/admin/settings/parts-of-speech`

```jsonc
{
  "code": "particle",
  "name_zh": "小品词",
  "name_en": "Particle",
  "abbreviation": "part.",
  "sort_order": 120,
}
```

成功 201，body 为 `PartOfSpeechConfig`。同一业务请求不要求额外 idempotency key；前端提交期间禁用按钮，网络结果不确定时先刷新列表再决定是否重试。所有写 DTO 都严格拒绝未知或只读字段。

#### `PATCH /api/v1/admin/settings/parts-of-speech/{id}`

```jsonc
{
  "base_revision": 3,
  "name_zh": "小品词",
  "name_en": "Particle",
  "abbreviation": "part.",
  "sort_order": 120,
}
```

成功 200，返回新 revision 的完整 `PartOfSpeechConfig`。请求不接受 `code`；写 DTO 严格拒绝
未知字段，携带 `code` 或其他只读字段返回 422 `invalid_request_body`，不会被静默忽略。
`base_revision` 必须为正整数；缺失/类型错误返回 422，值小于 1 返回 400
`invalid_part_of_speech`，顶层 `field` 为 `base_revision`。

#### `DELETE /api/v1/admin/settings/parts-of-speech/{id}?base_revision={revision}`

`base_revision` 是必填正整数；缺失、非整数或小于 1 返回 400 `invalid_query`。服务端锁行后
先比较当前 revision：过期返回 409 `revision_conflict`，不会删除其他管理员刚修改的配置。
revision 一致时再在同一事务内检查当前
草稿和所有仍保留 publication 的 word/phrase 引用；有引用返回 409，不删除任何基本/细分配置。
无引用时级联删除所属细分配置并返回 204。

### 7.5 细分词性管理

- `GET /api/v1/admin/settings/parts-of-speech/{id}/sub-parts`
- `POST /api/v1/admin/settings/parts-of-speech/{id}/sub-parts`
- `PATCH /api/v1/admin/settings/parts-of-speech/{id}/sub-parts/{sub_id}`
- `DELETE /api/v1/admin/settings/parts-of-speech/{id}/sub-parts/{sub_id}?base_revision={revision}`

全部要求 `super_admin`。GET 返回该基本词性下按稳定顺序排列的 items 信封，数量预计较小，
首期不分页：

```jsonc
{
  "items": [/* SubPartOfSpeechConfig */],
}
```

POST 请求：

```jsonc
{
  "code": "N-COLLECTIVE",
  "name_zh": "集合名词",
  "name_en": "Collective noun",
  "sort_order": 60,
}
```

PATCH 请求：

```jsonc
{
  "base_revision": 2,
  "name_zh": "集合名词",
  "name_en": "Collective noun",
  "sort_order": 60,
}
```

POST 成功 201，PATCH 成功 200，DELETE 成功 204。PATCH 不接受 code，所有写 DTO 严格拒绝
未知字段。细分 DELETE 同样要求正整数 `base_revision`，服务端锁行并通过 revision 校验后，
才检查当前草稿与仍保留 publication 中的全部词义引用；有引用返回 409，记录保持不变。

### 7.6 错误码

继续使用 `application/problem+json`。前端按稳定 `code` 分支，不匹配 detail 文案。固定契约如下：

| HTTP | code                           | 场景                                             | 附加上下文                |
| ---- | ------------------------------ | ------------------------------------------------ | ------------------------- |
| 400  | `invalid_query`                | q、分页或 DELETE base_revision 查询非法          | —                         |
| 400  | `invalid_part_of_speech`       | JSON 结构正确，但配置字段值非法                  | 顶层 `field`              |
| 400  | `invalid_path_parameter`       | `{id}` / `{sub_id}` 不是合法 UUID                | 顶层 `field`（可确定时）  |
| 404  | `part_of_speech_not_found`     | 基本词性不存在                                   | —                         |
| 404  | `sub_part_of_speech_not_found` | 细分词性不存在或不属于路径父级                   | —                         |
| 409  | `part_of_speech_conflict`      | 基本编码、中文名、英文名或缩写冲突               | 顶层 `field`              |
| 409  | `sub_part_of_speech_conflict`  | 细分编码或同父级名称冲突                         | 顶层 `field`              |
| 409  | `part_of_speech_in_use`        | 基本词性被单词/短语引用，禁止删除                | 正常检查含 `usage_count`  |
| 409  | `sub_part_of_speech_in_use`    | 细分词性被词义引用，禁止删除                     | 正常检查含 `usage_count`  |
| 409  | `revision_conflict`            | PATCH body 或 DELETE query 的 base_revision 过期 | `current_revision`        |
| 422  | `invalid_request_body`         | 字段缺失、类型错误或出现未知/只读字段            | —                         |
| 422  | `unknown_part_of_speech`       | 词条 detect/create/save/publish 使用未知基本编码 | 顶层 `field`、`meta.code` |
| 422  | `invalid_sub_part_of_speech`   | 细分编码不存在或不属于当前基本词性               | 顶层 `field`、`meta.code` |

catalog `meta` 允许字段：

```jsonc
{
  "usage_count": 8,
  "current_revision": 4,
  "part_of_speech_id": "019f-...",
  "code": "noun",
}
```

`field` 始终是 Problem Details 顶层字段，不放入 `meta`。后端统一 `ProblemDetails` 增加可选
`ProblemMeta`；前端 `ProblemDetails.meta` / `HttpError.meta` 使用同一个通用类型，不能继续把它
限定为词条专属 `AdminWordApiErrorMeta`。前端请求层已经按 `body.meta` 解析
`usage_count/current_revision/part_of_speech_id/code`。

DELETE 的正常引用检查返回 `meta.usage_count`。如果删除语句在极端并发下由已知 catalog FK
最终触发 SQLSTATE `23503`，后端仍返回相应 `*_in_use` 409；该兜底响应允许省略可选 meta，
前端不得假定每个 in-use 错误都一定带计数。

当前 mock 尚有三项待同步：字段值错误仍返回 422、细分唯一冲突仍返回基本冲突码、404 仍返回
通用 `not_found`；同时 DELETE 客户端与 mock 需要补 `base_revision`。真实接口开启前必须对齐上表。

其余错误沿用现有 Problem Details：400/422 输入校验、401、403、404、500。

### 7.7 与智能词库接口的联动约束

词性配置不是孤立 CRUD，后端智能词库落地时必须同时满足：

1. 内置词典 `suggested_forms.pos[]` 只返回 catalog 已存在的基本 `pos` 编码；供应商词性到平台 code 的映射由后端维护，未知供应商值不能透传；
2. V1/V2 create/forms save/publish 校验基本 `pos` code 存在；词形与发音分组不保存细分词性；
3. meanings 中每个 sense 的 `sub_pos` 必填，保存/发布时校验其存在且属于 `pos_id` 对应的基本 `pos`；
4. 基本词性 usage_count 按 distinct entry 统计当前草稿和所有仍保留 publication 中的引用；
5. 细分词性 usage_count 按稳定 sense node 去重统计当前草稿和所有仍保留 publication 中的引用；
6. 配置改名不修改词条 revision；读取词条时由前端 catalog 解析最新名称；
7. 配置删除与词条保存并发时，数据库约束/事务需保证不会留下悬空 code；
8. 生产初始化需先 seed 当前 11/19 编码，再导入或开放词条写入。

基本词性 catalog item 与基本词性管理 item 均额外返回有序的
`allowed_form_types` / `default_form_types`。两者当前相同，后者供新建表单初始化，前者是服务端
保存和发布的权威白名单：`noun=[plural]`，`verb=[third_person_singular,
present_participle,past_tense,past_participle]`，`adjective` 与
`adverb=[comparative,superlative]`。其他（包括以后新增的自定义）基本词性返回空数组并
fail closed。forms 保存（`save` 和 `complete`）、validate、publish 对每个不匹配 slot 聚合返回
`DraftValidationIssue`，稳定字段为 `step=forms`、`node_id=slot.id`、`field=form_type`、
`code=invalid_form_type_for_part_of_speech`。

`builtin_dictionary.status=matched` 保留既有 `headwords` / `suggested_forms`，并增加
`provider`、`suggested_meanings`、`suggested_frequency`、`coverage` 和 `provenance`。coverage 的
五个固定分类是 `forms/pronunciations/meanings/examples/frequency`，状态仅为
`complete|partial|missing`。create 仍只接受 `detection_id/headwords`，并在后端事务中消费检测
快照里的全部建议；客户端不得回传或重建建议。

当前激活 Kaikki 导入只持久化词头、基本词性和地区证据，因此 matched 的 forms 仅含词头/POS
骨架，准确标为 `partial`；音标、实际发音、释义、例句和词频均为空并标为 `missing`，相应
provenance 为 null。不得由客户端或服务端按拼写规则猜测这些值。要升级为 complete，必须先
扩展离线清洗产物、dictionary schema 与导入器，保存 Kaikki 原始 forms/sounds/senses/examples，
并另接有授权且可追踪版本的词频数据源。

active draft 的 `entry_pos` / `senses` FK 不能保护已从新草稿移除的发布内容；发布事务还必须写
`entry_publication_part_of_speech_refs` / `entry_publication_sub_part_of_speech_refs` 结构化引用，
并以 `ON DELETE RESTRICT` 指向 catalog。publication 仍保留时引用行也必须保留，不能只扫描
JSONB snapshot 做删除判断。

前端在 catalog 加载失败时可以对历史内容回退显示 code，但会禁用新增基本/细分词性；这只是可用性降级，不代表后端可以接受未知编码。

---

_维护约定：auth 相关响应形状变更时同步本文档 §3/§4；后端契约任务进度看
`frontend-contract-alignment.md`。词性配置契约变更同步本文档 §7 与前端
`docs/features/refactor-word-creation/design.md`。_
