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
| 真实短信通道                      | OTP 目前只能从服务器日志取码 | 未做                                         |
| 域名 + TLS                        | 基址换域名、cookie 加 Secure | 备案审核中                                   |
| 头像上传                          | `avatar_url` 将出现真实 URL  | 未排期，前端先做空串兜底（显示默认头像）     |

---

## 7. 系统设置 / 词性配置（后端已实现）

> **状态（2026-08-20）**：九个端点均已实现并导出到 `docs/openapi.json`。
> 原先写在这里的接口提案（§7.1–§7.6）写于后端尚不存在时，其中的数据库结构、默认数据、
> 端点契约与写事务已被 [`part-of-speech-config-design.md`](part-of-speech-config-design.md)
> 与 `openapi.json` 完整取代，留着只会与实现分叉，故删除。
> 本节现在只保留两样东西：下面这份前端切换清单，以及本文档独有的 §7.7 联动约束。

**前端从 mock 切真实接口前要对齐的四项**（原 §7.6 末尾。**mock 侧的描述是 2026-08-10 记下的，
本仓库无法验证前端当前形状，请自行核实哪几项还没做**；右侧「真实接口」一列已逐条对过实现）：

| # | mock 当时的行为（待核实） | 真实接口 |
| --- | --- | --- |
| 1 | 字段值错误返回 422 | **400 `invalid_part_of_speech`**（`catalog/handler.rs:305`） |
| 2 | 细分词性唯一冲突返回基本词性的冲突码 | **409 `sub_part_of_speech_conflict`** |
| 3 | 404 返回通用 `not_found` | **404 `part_of_speech_not_found` / `sub_part_of_speech_not_found`** |
| 4 | DELETE 未带 `base_revision` | `base_revision` 是**必填查询参数**，缺失或非法返回 400 |

对齐后即可移除 contract test 的 PENDING 项。完整的错误码与 meta 语义（含 `usage_count`、
`23503` 兜底分支允许省略 meta）见
[`part-of-speech-config-design.md`](part-of-speech-config-design.md)。

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
保存和发布的权威能力。所有基本词性（包括以后新增的自定义词性）都返回同一完整 fixed enum：

```json
[
  "third_person_singular",
  "present_participle",
  "past_tense",
  "past_participle",
  "plural",
  "comparative",
  "superlative"
]
```

`base` 不出现在这两个 non-base 能力数组中，但在 V3 `WordFormTypeV3` 中与其他 concrete form
同级、可有多条，并归属于当前词条及对应 POS。V3 forms save/complete/validate/publish 接受任一
POS 与任一 fixed enum 的组合；fixed enum 之外的未知值返回 `validation_failed`，其中 issue 的
`code=invalid_form_type_for_part_of_speech`、`field=form_type` 且定位具体 form UUID，不得由前端
回退或转换。前端应删除按 POS code 推断类型的 fixture/逻辑，完全使用 catalog 返回顺序。

V3 Step 2 新增必填 POS 级正式字段：

```json
"dialect_rules": {
  "spelling_mode": "unified",
  "phonetic_mode": "distinguish"
}
```

两个字段均引用 `DialectModeV3 = "unified" | "distinguish"`，完整对象为
`DialectRulesV3`。合法规则为 UU、UD、DD；DU 非法。UU 要求该 POS 全部 form 为 common；UD 要求
全部 form 为 uk_us 且每个 form 的 UK/US spelling 相同；DD 要求全部 form 为 uk_us。规则不属于
form group，shared membership 继续引用同一个 form UUID。

缺字段、未知 mode 或 DU 返回：

```json
{
  "schema_version": 3,
  "step": "forms",
  "node_id": "<pos-id>",
  "field": "dialect_rules",
  "code": "dialect_rules_invalid",
  "node_location": {
    "node_role": "forms.pos",
    "ancestor_node_ids": [],
    "pos_id": "<pos-id>"
  }
}
```

规则与 form shape 不一致或 UD 异拼写返回：

```json
{
  "schema_version": 3,
  "step": "forms",
  "node_id": "<conflicting-form-id>",
  "field": "regional_variants",
  "code": "invalid_regional_variant_shape",
  "node_location": {
    "node_role": "forms.concrete_form",
    "ancestor_node_ids": ["<pos-id>"],
    "pos_id": "<pos-id>",
    "form_id": "<conflicting-form-id>"
  }
}
```

V3 尚未上线且历史 Smart Lexicon 数据已清理；服务端和客户端只支持 latest contract。所有 V3
editor/publication JSON 都必须显式携带该字段，缺字段、mixed common/uk_us 或规则不一致直接
fail closed，不推导或静默转换。该决定不影响仍在产品中使用的 V2 路由与功能。

V3 detection/create 当前不直接持久化 POS；前端从 `suggested_forms` 物化新 POS 时必须在首次 forms
payload 中显式生成规则：common→UU、uk_us 同拼写→UD、uk_us 异拼写→DD。该映射只用于尚未存在
用户意图的新 POS 预填；已保存或历史返回的 `dialect_rules` 必须原样使用，不能再次按文本反推。

`builtin_dictionary.status=matched` 保留既有 `headwords` / `suggested_forms`，并增加
`provider`、`suggested_meanings`、`suggested_frequency`、`coverage` 和 `provenance`。coverage 的
五个固定分类是 `forms/pronunciations/meanings/examples/frequency`，状态仅为
`complete|partial|missing`。create 仍只接受 `detection_id/headwords`，并在后端事务中消费检测
快照里的全部建议；客户端不得回传或重建建议。

Kaikki 轻量索引只持久化词头、基本词性和地区证据；完整内容导入另保留原始 forms/sounds/senses。
V3 detection 只映射有确定 tag 的七类平台词形和非空 IPA，未知/冲突/单侧地区证据不猜测。
matched forms 第一阶段最高标为 `partial`；至少一条有效 IPA 时 pronunciations 为 `partial`，否则
为 `missing`；`actual_pron`、释义、例句和词频仍不生成。客户端必须展示 coverage，并继续把
suggested forms 作为只读建议；不得回传、重建或把智能词库内容冒充 builtin provenance。

active draft 的 `entry_pos` / `senses` FK 不能保护已从新草稿移除的发布内容；发布事务还必须写
`entry_publication_part_of_speech_refs` / `entry_publication_sub_part_of_speech_refs` 结构化引用，
并以 `ON DELETE RESTRICT` 指向 catalog。publication 仍保留时引用行也必须保留，不能只扫描
JSONB snapshot 做删除判断。

前端在 catalog 加载失败时可以对历史内容回退显示 code，但会禁用新增基本/细分词性；这只是可用性降级，不代表后端可以接受未知编码。

---

## 8. 智能词库列表：词汇列按管理员主词侧排序（后端已实现）

> **状态（2026-08-19）**：后端已实现并生成 `docs/openapi.json`。

### 8.1 背景

同一词条的英美并列拼写，此前在三处顺序不一致：词条详情左栏与建稿第 4 步「主词」按
**管理员主词侧**（`headwords.source_dialect`，由管理员创建草稿时决定）排序，而
`GET /admin/lexicon/entries` 列表行的 `headword` 由后端 `string_agg` 固定按
`common → uk → us` 拼好。结果 `source_dialect = "us"` 的词条在列表显示
`colour / color`，点进详情却是 `color / colour`，管理员无法判断哪个是主词。

### 8.2 后端改动

1. `AdminWordListItem.headword` 与 `AdminWordListItem.dialects` 改为**同序**，排序规则统一为
   「`common` → 管理员主词侧 → 另一侧」。即 `source_dialect = "us"` 时列表返回
   `"color / colour"` 与 `["us", "uk"]`；`source_dialect = "uk"` 时返回
   `"centre / center"` 与 `["uk", "us"]`。
2. `AdminWordListItem` 新增**可选**字段 `source_dialect: "uk" | "us"`，语义与
   `AdminWordV2.headwords.source_dialect` 完全一致，让前端不必靠切分 `" / "` 反推基准侧。

`mode = "unified"` 的词条只有 `common` 一条词头，`headword` 仍是单个拼写、`dialects` 仍是
`["common"]`，`source_dialect` **整个字段省略**（与 `AdminWordV2` 的 `unified` 变体没有该字段
一致），行为与改动前完全相同。

### 8.3 前端要怎么改

| 步骤 | 动作 |
| --- | --- |
| 1 | `pnpm --filter @tsz/api-client sync:openapi` 同步 `openapi.snapshot.json` |
| 2 | `packages/types/src/admin-word.ts` 的 `AdminWordListItem` 补 `source_dialect?: SourceDialect`，并把 `headword` / `dialects` 的注释改为「管理员主词侧在前」 |
| 3 | 智能词库列表的「词汇」「方言」列**无需改渲染逻辑**——直接消费即可得到正确顺序；如需在列表标注主词，用新增的 `source_dialect` 判断，不要切分 `" / "` |
| 4 | mock（`apps/admin/src/features/dictionary/mock/`）里的 `distinguish` 行补上 `source_dialect` 并按管理员主词侧在前重排 `headword` / `dialects`，否则 mock 与真实后端不一致 |

### 8.4 兼容性

`source_dialect` 是**可选新增字段**，`required` 数组不变，既有消费者（含
`endpoints.contract.test.ts` 对 `AdminWordListItem.required` 的 `arrayContaining` 断言）不受影响。
唯一的行为变化是 `distinguish` 词条 `headword` / `dialects` 的元素顺序——这正是本次要修的缺陷。

## 9. 关联词搜索与引用预览：并列拼写同样按管理员主词侧排序（后端已实现）

> **状态（2026-08-20）**：后端已实现，OpenAPI schema 不变（仅字段取值顺序变化）。

### 9.1 背景

§8 只改了 `GET /admin/lexicon/entries` 列表行。关联词搜索
（`GET /admin/lexicon/entries/related-search`）与建稿第 1 步的「已有词条被谁引用」预览，
仍固定按 `uk → us` 拼词头，于是 `source_dialect = "us"` 的同一个词条在列表显示
`color / colour`，在关联词搜索结果里却是 `colour / color`——正是 §8 要消灭的那类矛盾换了位置。

### 9.2 后端改动

统一为与 §8 完全相同的规则「`common` → 管理员主词侧 → 另一侧」，涉及：

1. `RelatedWordResult.headword` 与 `RelatedWordResult.dialects` 改为**同序**。
   `source_dialect = "us"` 时返回 `"color / colour"` 与 `["us", "uk"]`，
   `source_dialect = "uk"` 时返回 `"centre / center"` 与 `["uk", "us"]`。
2. `RelationReferencePreviewV2.source_headword`（词条创建/词形保存/发布时返回的
   `matched_entry_contexts[].inbound_relations.previews[].source_headword`）同规则。
3. 建稿保存关联词时由后端回填的只读字段 `WordRelationV2.target_headword`，以及发布后落库的
   词头快照，同规则。保存（canonicalize）与发布（verify）共用同一段计算，两者不会打架。

`mode = "unified"` 的词条只有 `common` 一条词头，`headword` / `source_headword` 仍是单个拼写、
`dialects` 仍是 `["common"]`，行为与改动前完全相同。

关联词搜索的**排序键**（也是 `next_cursor` 的内容）一直就是它返回的 `headword` 那个字符串，
本次一并改成管理员主词侧在前，两者继续逐字符相同。副作用是 `source_dialect = "us"` 的
`distinguish` 词条在结果里的字母序位置会变（这正是「按看到的词头排序」应有的行为）；
`next_cursor` 的**格式与语义不变**，但上线瞬间已经发出去的游标可能落在稍有偏差的边界上，
重新发起一次搜索即可。线上尚无已发布的 `distinguish` 词条，实际不受影响。

### 9.3 前端要怎么改

| 步骤 | 动作 |
| --- | --- |
| 1 | 关联词搜索下拉、引用预览**无需改渲染逻辑**——直接消费即可得到正确顺序 |
| 2 | 仍然不要用 `split(" / ")` 反推英/美拼写；需要基准侧就读 `AdminWordV2.headwords.source_dialect`（列表行读 §8 新增的 `AdminWordListItem.source_dialect`） |
| 3 | mock（`apps/admin/src/features/dictionary/mock/`）里 `distinguish` 词条的 `RelatedWordResult.headword` / `dialects` 与 `source_headword` 按管理员主词侧在前重排，否则 mock 与真实后端不一致 |

### 9.4 兼容性

不新增/删除任何字段，`RelatedWordResult`、`RelationReferencePreviewV2` 的 schema 与
`required` 数组均不变，因此**不需要重新 `sync:openapi`**。唯一的行为变化是 `distinguish`
词条这几个字符串/数组的元素顺序。线上 `lexicon.relations` 目前 0 行、草稿里也没有关联数据，
不存在需要回填的存量快照。

## 10. 语法结构不再强制英美双条（后端已实现）

> **状态（2026-08-20）**：后端已实现，OpenAPI schema 不变（只放宽校验），**不需要重新 `sync:openapi`**。
> 上游依据：前端 `docs/features/dialect-preference-migration/design.md` 提案 P1。
>
> **补充（2026-08-21）**：10.5 是后来补的节点身份契约，**那一节改了 wire、需要重跑
> `pnpm --filter @tsz/api-client sync:openapi`**；本节其余内容不受影响。

### 10.1 背景

英美方言偏好化（A1）之后，平台自己写的英文行文只维护一份。但后端此前要求
`distinguish` 词条的 `grammar_structures[].variants` **精确等于** `[uk, us]`，
前端只能写「两条同值镜像」：wire 里多一份冗余，学习端将来会读到
「英式：a centre／美式：a centre」这种没有信息量的两行。

### 10.2 后端改动

`distinguish` 词条的语法结构变体集合现在同时接受 `[common]` 与 `[uk, us]`；
`unified` 词条维持只接受 `[common]`。缺一侧（只写 `uk`）、多一侧（`common` + `uk`）、
方言重复，仍然照旧报 `grammar_variants_invalid`。

**AI 内容补全已跟着收敛**（提案 P1-b，2026-08-20）：前端阶段 3 上测试服后，
`content_completion` 的语法结构**恒产单份 `common`**，不再看词条是不是 `distinguish`，
uk / us 双份同值镜像已消失。例句仍按词条事实分方言，本项不涉及。
契约无变化，`docs/openapi.json` 未变，前端无需重跑 `sync:openapi`。

### 10.3 前端要怎么改

| 步骤 | 动作 |
| --- | --- |
| 1 | 删掉「两条同值镜像」shim（design.md 阶段 6），`distinguish` 词条直接写单条 `common` |
| 2 | 前端校验口径同步放宽：`unified ⇒ [common]`，`distinguish ⇒ [common]` 或 `[uk, us]` |
| 3 | **收敛时要给 common 变体换新节点 ID**：节点角色里带方言（`meanings.content:en:<dialect>`），沿用旧 uk 变体的 ID 只改 `dialect` 会被判 `node_binding_changed` |
| 4 | **反向同理**：把已收敛的词条改回 uk / us 双条，必须沿用最初那两个变体的节点 ID，用新 UUID 会被判 `stable_node_id_changed`。原 ID 从哪儿拿见 10.5 |
| 5 | mock（`apps/admin/src/features/dictionary/mock/`）与真实后端同口径，否则 mock 绿、真机 422 |

### 10.4 兼容性

纯放宽：存量的双条数据继续可读可写，旧前端一行不改也照常工作，`AdminWordV2` 的 schema
与 `required` 数组均不变。**放宽校验本身不改后端产出**——AI 补全的收敛是随后单独做的
（见 10.2），所以本节可以先于前端任何改动上线。

### 10.5 共用 ↔ 英美拆分/合并的节点身份契约（后端已实现）

> **状态（2026-08-21）**：后端已实现并重新导出 `docs/openapi.json`，
> **前端必须重跑 `pnpm --filter @tsz/api-client sync:openapi`**。
> 上游依据：智能词库系统测试报告 TSZ-LEX-001（2026-08-20，测试词 `testability`）。

#### 10.5.1 规则：稳定槽位的节点 ID 是永久的

后端把「有内容的槽位」建成**稳定槽位**（stable slot），键是三元组

```
(词条 ID, 父节点 ID, 节点角色)
```

方言编在**节点角色**里，不是节点上的一个可改字段：

| 槽位 | 父节点 | 节点角色 |
| --- | --- | --- |
| 共享原形 | 基本词性 | `forms.base_form` |
| 派生词形 | 词形组 | `forms.form_slot:<form_type>` |
| 词形方言行 | 词形槽位 | `forms.form_variant:common` / `:uk` / `:us` |
| 英文正文方言行 | 释义 / 例句 / 语法结构 | `meanings.<field>:en:common` / `:uk` / `:us` |
| 中文正文 | 释义 / 例句 | `meanings.<field>:zh:common` |

**这个键一旦保存过，就永久绑定同一个节点 ID。** 槽位从草稿里消失时后端只把它标记
为「已退役」（`removed_from_draft_at`），节点行本身保留；同一个键再次出现时必须沿用
原 ID，后端会把它重新激活。用新 UUID 提交 → `422 validation_failed` +
`stable_node_id_changed`（「已有内容槽位必须保留原节点 ID」）。

这条规则不打算放宽：发布快照、引用关系、影响面预览都按节点 ID 追溯内容，槽位换 ID
等于历史断链。

#### 10.5.2 缺口与补法：`GET /entries/{id}` 新增 `retired_stable_slots`

问题出在**已退役的身份没有任何渠道能被前端知道**：

1. 管理员把某个基本词性从「英美共用」切成「英美区分」→ `common` 变体消失、`uk` / `us` 出现。
2. 保存草稿 → `common` 节点被标记退役，草稿投影里不再有它。
3. 刷新页面（或换台机器打开）→ `GET` 只返回 `uk` / `us`，原 `common` 节点 ID 无处可查。
4. 管理员再切回「英美共用」→ 前端只能生成新 ID → 422 `stable_node_id_changed`，界面上无法自愈。

现在 `GET /api/v1/admin/lexicon/entries/{id}` 的响应体在 `word` 旁边多了一个数组：

```jsonc
{
  "word": { /* 原样，未改一字段 */ },
  "retired_stable_slots": [
    {
      "id": "0198f3c2-1c4a-7a90-8f21-6b2f0d9a4e11",     // 该槽位永久绑定的节点 ID
      "parent_node_id": "0198f3b7-9d02-7c31-b8aa-5c1e2f7d3a40",
      "node_role": "forms.form_variant:common"
    }
  ]
}
```

- 取值口径：该词条下 `stable_slot = TRUE AND removed_from_draft_at IS NOT NULL` 的全部节点，
  按 `(parent_node_id, node_role)` 排序。
- 只有 `GET` 带这个数组。保存 / 发布 / 归档等命令类接口仍然只回 `{ "word": ... }`——
  提交方自己就知道刚退役了什么，需要服务端补身份的只有「刷新」和「换设备」。
  wire 上对应两个类型：`AdminWordDraftV2Envelope`（GET）与 `AdminWordV2Envelope`（命令）。
- 退役身份**不进** publication 快照，也不影响快照哈希——它是编辑器恢复用的元数据，不是词条内容。

#### 10.5.3 前端要怎么改

| 步骤 | 动作 |
| --- | --- |
| 1 | 重跑 `pnpm --filter @tsz/api-client sync:openapi`；GET 草稿的返回类型从 `AdminWordV2Envelope` 变成 `AdminWordDraftV2Envelope` |
| 2 | 载入草稿时用 `retired_stable_slots` **播种节点身份账本**，键用 `(parent_node_id, node_role)`；这样跨刷新、跨设备都成立 |
| 3 | 槽位重新出现时（`common` ↔ `uk`/`us` 来回切、删掉的词形又加回来）先查账本：命中就沿用 `id`，没命中才生成新 UUID |
| 4 | 账本里**同时**放本次会话自己退役掉的 ID：后端只在 GET 时补，保存响应不带，别把会话内的记忆清掉 |
| 5 | 父节点自己是新建的（新词形组、新释义）时不用查账本——新父节点下的槽位键必然是全新的 |

拆分/合并的具体对照：

| 动作 | `forms.form_variant:common` | `forms.form_variant:uk` / `:us` |
| --- | --- | --- |
| 共用 → 区分 | 从内容里移除（后端标记退役） | 首次出现给新 UUID；**再次出现要沿用退役身份** |
| 区分 → 共用 | **沿用退役的 common ID** | 从内容里移除（后端标记退役） |

`meanings.<field>:en:<dialect>` 完全同理（10.3 第 3、4 行说的就是这件事），只是父节点换成释义 / 例句 / 语法结构。

#### 10.5.4 节点身份类错误新增 `node_location`

`stable_node_id_changed`、`node_binding_changed`、`node_binding_unknown` 三个 code
以前只带 `node_id` 加一句面向实现的中文，管理员看到的是两条一模一样的
「已有内容槽位必须保留原节点 ID」，无从判断是哪个词性、哪个词形。现在 issue 里多一个
可选子对象（和既有的 `reference_location` 同一形状，存量 issue 的序列化不变）：

```jsonc
{
  "step": "forms",
  "node_id": "0198f4aa-...",             // 本次提交里的新 ID，可直接用来定位
  "field": "id",
  "code": "stable_node_id_changed",
  "message": "已有内容槽位必须保留原节点 ID",
  "node_location": {
    "node_role": "forms.form_variant:common",
    "pos": "verb",                        // 基本词性编码
    "pos_id": "0198f3a1-...",
    "form_group_index": 0,                 // 在 pos.form_groups 里的序号；共享原形省略
    "form_type": "third_person_singular",  // base 表示共享原形
    "dialect": "common",
    "ancestor_node_ids": ["0198f3a1-...", "0198f3b0-...", "0198f3b7-..."]
  }
}
```

- 所有字段都取自**本次提交的内容**，可以直接拿去定位到界面元素；
  `ancestor_node_ids` 是从词条根到直接父节点的链。
- **旧 ID / 新 ID 的对照不下发**，只写服务端日志——要找回旧 ID 走 10.5.2 的
  `retired_stable_slots`，不要从报错里读。
- 展示文案由前端拼，例如「动词 · 第三人称单数：词形模式切换后数据状态不一致，请恢复后重试」。
- 非身份类 issue 整体省略 `node_location` 字段。

#### 10.5.5 兼容性

- `AdminWordV2` 一个字段没动；命令类接口的响应体一个字段没动。
- GET 草稿是**纯新增字段**，旧前端忽略即可继续工作（但拿不到身份恢复能力）。
- `DraftValidationIssue` 是**纯新增可选字段**，旧前端不读也不会炸。
- 校验行为没有放宽：换 ID 依旧 422。变的只是「能不能查到该用哪个 ID」和「报错能不能定位」。

## 11. 管理员方言偏好持久化（后端已实现）

> **状态（2026-08-20）**：后端已实现并重新导出 `docs/openapi.json`。
> 上游依据：`design.md` 提案 P2。**wire 形状与提案一致**，落地时改了端点路径与存储选型（见 11.2）。

### 11.1 背景

方言偏好现在落在按管理员隔离的 `localStorage`（`packages/shared/src/dialect-preference.ts`），
换浏览器、换设备即丢；默认值前后端各存一份，早晚会漂移成「我明明没改过它怎么变了」。

### 11.2 后端改动

1. `GET /api/v1/admin/profile` 响应新增 `preferences` 对象，**字段恒在**（进 `required`）：

   ```jsonc
   { "id": "…", "phone": "…", "display_name": "…", "role": "admin",
     "permissions": ["…"], "preferences": { "dialect": "uk" } }
   ```

   `dialect` 枚举 `"uk" | "us"`（schema `AdminDialectPreference`），从未设置过的管理员返回 `"uk"`。

2. 新增 `PATCH /api/v1/admin/profile/preferences`：请求 `{ "dialect": "us" }`，
   200 返回 `{ "preferences": { "dialect": "us" } }`（返回的是落库后的值）。

   - **路径与提案不同**：提案写的是 `/admin/settings/preferences`，实际落在 `/admin/profile/preferences`。
     `/admin/settings/*` 挂的是**全局目录配置**（词性配置，仅 `super_admin` 可写），
     个人偏好是「我自己的」，语义上属于 profile。
   - **存储用 `admins.dialect_preference TEXT + CHECK('uk','us')`**，不是 `preferences jsonb`，
     与同表的 `role` / `status` 一致。**wire 仍是嵌套的 `preferences.dialect`**，
     将来加第二项偏好时前端形状不用再变。

3. 权限：任何已登录 admin 读写**自己的**。请求体里没有管理员 ID，改不到别人。
   守卫与 profile 同一组：账号禁用 → 403 `account_disabled`，需先改密 → 403 `must_change_password`，
   缺 / 失效 token → 401。
4. 错误：`dialect` 不在枚举内或请求体缺字段 → 422 `invalid_request_body`（`application/problem+json`）。
5. **默认值只由后端持有**，前端不再保留第二处默认——这是本次改动的重点。

### 11.3 前端要怎么改

| 步骤 | 动作 |
| --- | --- |
| 1 | `pnpm --filter @tsz/api-client sync:openapi` 同步 `openapi.snapshot.json` |
| 2 | `@tsz/types` 补 wire 类型：`AdminDialectPreference`、`AdminProfileResponse.preferences`（必填）、PATCH 的请求/响应 |
| 3 | `@tsz/shared` 偏好内核的事实源切到服务端：读取用 profile 的 `preferences.dialect`，写入调 PATCH，`localStorage` 降为离线缓存（design.md 阶段 7） |
| 4 | 前端的 `DEFAULT_DIALECT_PREFERENCE` 不再作为事实源，只保留为「profile 还没回来」时的兜底显示值 |
| 5 | 解除契约测试里对应的 PENDING 项 |

### 11.4 兼容性

`preferences` 是**新增字段**，旧前端不读它即不受影响；`AdminProfileResponse.required` 多了一项，
`endpoints.contract.test.ts` 里对 required 的 `arrayContaining` 断言不受影响。新端点纯新增。
回滚 = 删列 + 摘掉端点，前端回落 `localStorage` 分支，代价仅是偏好回到默认英式，无数据损失。

## 12. 列表与关联词搜索补结构化词头（后端已实现）

> **状态（2026-08-20）**：后端已实现并重新导出 `docs/openapi.json`。
> 上游依据：`design.md` 提案 P3。§8 / §9 只统一了并列拼写的**顺序**，本节把**结构**也给出来。

### 12.1 背景

§8 / §9 之后，`headword` 已经按「`common` → 管理员主词侧 → 另一侧」拼好，`dialects` 与之同序。
但要按 A1 的**管理员方言偏好**重排（design.md 阶段 4），前端得知道每一侧各是哪个拼写——
而 `headword` 是一个拼好的字符串，`split(" / ")` 又是 §8.3 / §9.3 明确禁止的
（短语词条的拼写里可能出现斜杠）。

### 12.2 后端改动

`AdminWordListItem` 与 `RelatedWordResult` 新增 `headword_variants`，与 `dialects` **同序**：

```jsonc
{
  "headword": "colour / color",          // 保留，未改语义
  "dialects": ["uk", "us"],              // 保留，未改语义
  "source_dialect": "uk",                // 列表行才有（§8）
  "headword_variants": [
    { "dialect": "uk", "headword": "colour" },
    { "dialect": "us", "headword": "color" }
  ]
}
```

- `unified` 词条返回单元素 `[{ "dialect": "common", "headword": "…" }]`。
- 不变量（测试钉死）：`headword_variants` 的方言序列 ≡ `dialects`；按序拼接 ≡ `headword`。
  列表行与关联词搜索对同一词条返回的 `headword_variants` 逐字段相同。

### 12.3 前端要怎么改

| 步骤 | 动作 |
| --- | --- |
| 1 | `pnpm --filter @tsz/api-client sync:openapi` 同步 `openapi.snapshot.json` |
| 2 | `@tsz/types` 给 `AdminWordListItem` 与 `RelatedWordResult` 补 `headword_variants: HeadwordVariant[]`（必填） |
| 3 | 列表「词汇」列按偏好排序：从 `headword_variants` 里挑出偏好侧在前，自己拼展示串；不想排序就继续用 `headword`，行为与今天一致 |
| 4 | mock 同步补上该字段，否则 mock 与真实后端不同形 |

### 12.4 兼容性

纯新增字段，`headword` / `dialects` / `source_dialect` 的语义与取值一律未变，
既有消费者不改一行也照常工作。`required` 数组各多一项，
`endpoints.contract.test.ts` 里基于 `arrayContaining` 的断言不受影响。

## 13. 词条录入的体积与长度上限（后端已实现）

> **状态（2026-08-20）**：常量本就在代码里，只是从未写进对接文档；本次同时修掉了两个
> 会让前端拿不到正确信号的缺陷。上游诉求：前端「requirements 待确认第 12 条」——
> 管理员可能录到一半被拒，前端无法提前拦。

### 13.1 内容上限（超限 → `422 validation_failed`）

| 上限 | 值 | 作用范围 |
| --- | --- | --- |
| 单个词条内容节点数 | **2000** | `forms` 与 `meanings` **各自**独立计数，不是两步之和 |
| 单段富文本正文长度 | **5000** 个 Unicode 码点 | 每一段 RichText 单独计，注意是码点不是 UTF-16 length |
| 单段富文本标注数 | **500** | V2 是 `annotations`；V1 的 `spans` 与 `liaisons` 各自独立计 500 |
| 单个 IPA 音素长度 | **200** 个码点 | `phoneme` 标注，且不能为空 |
| 单个停顿时长 | **1–5000** ms | `pause` 标注，必须是整数 |

节点数超限时 `field_issues` 里给的是 **`aggregate_node_limit_exceeded`**（`field: "content"`），
可以直接照着提示做。

**但正文长度 / 标注数 / IPA / 停顿超限拿不到专门的 code**：这些失败会并入所在字段的既有
错误码——语法结构给 `grammar_variants_invalid`、释义正文给 `definition_invalid`、
例句给 `sentence_incomplete`——前端只知道「这个字段不合法」，不知道「因为太长」。
这是后端刻意保持的现状（RichText 子码不外泄），所以**长度类限制请在前端本地先拦**，
按上表的数字实现即可，别指望从错误码反推。

### 13.2 请求体上限（超限 → `413 payload_too_large`）

| 路由 | 上限 |
| --- | --- |
| `PUT /entries/{id}/steps/forms`<br>`PUT /entries/{id}/steps/meanings`<br>`POST /entries/{id}/steps/forms/impact` | **8,192,000 字节**（约 7.81 MiB） |
| 其余所有接口 | **2 MiB**（2,097,152 字节，框架默认） |

三条承载整步草稿内容的路由单独放宽，上限由 `节点数上限 × 每节点 4 KiB` 推导
（2000 × 4096 = **8,192,000**）。现网草稿实测约 132 字节/节点（正文近乎为空），
4 KiB/节点是给正文、标注和 JSON 结构留的余量。

> ⚠️ **不是 8 MiB。** 8 MiB 是 8,388,608，比真实上限多 196,608 字节。写成 `8 * 1024 * 1024`
> 会让前端放过一批服务端仍要 413 的请求，等于白做预检。请照抄 `8_192_000` 这个数。
> 后端侧唯一来源是 `MAX_STEP_CONTENT_BODY_BYTES`（`src/lexicon/validation/structure.rs`），
> 节点上限调整时它会跟着变，届时本节数字同步更新。

前端要拦的话，量的是**序列化后的字节数**，不是字符数；上限是闭区间，恰好等于上限会被接受：

```ts
const STEP_CONTENT_BODY_LIMIT = 8_192_000; // 2000 节点 × 4 KiB，不是 8 MiB
const bytes = new TextEncoder().encode(JSON.stringify(payload)).byteLength;
if (bytes > STEP_CONTENT_BODY_LIMIT) { /* 提示拆分，别发出去 */ }
```

### 13.3 本次修了什么

1. **整步保存的请求体上限从 2 MiB 提到 8,192,000 字节。** 之前吃框架默认值 2 MiB，而校验层允许
   2000 节点——一条塞满的词条在校验之前就会被传输层 413 掉，且后端日志里看不出原因。
2. **413 不再伪装成 422。** 之前 `ApiJson` 把所有非 400 的 rejection 统一映射成
   `422 invalid_request_body`，超大请求体因此报「请求体不合法」。现在超限固定返回
   `413 payload_too_large`（`type: "urn:tsz:problem:payload_too_large"`），
   与「JSON 格式错」「DTO 形状错」三者互不混淆。

### 13.4 前端要怎么改

| 步骤 | 动作 |
| --- | --- |
| 1 | `pnpm --filter @tsz/api-client sync:openapi` 同步 `openapi.snapshot.json`（`ErrorCode` 多了 `payload_too_large`） |
| 2 | 错误处理补 `413 / payload_too_large` 分支，文案是「内容过大，请拆分」而不是「格式错误」 |
| 3 | 编辑器按 §13.1 的数字做本地校验，尤其是 5000 码点和 500 标注——这两个后端不会给专门的 code |
| 4 | 保存前按 §13.2 量一次字节数，超了就地提示，别发出去等 413 |

### 13.5 兼容性

`payload_too_large` 是**新增**错误码，旧前端撞不到它的前提是请求体本来就没超过 2 MiB；
而现在两步保存的实际上限是放宽的，只会让原本失败的请求成功，不会让原本成功的请求失败。
`ErrorCode` 枚举多一个值，`endpoints.contract.test.ts` 里基于 `arrayContaining` 的断言不受影响。
内容上限的**数值一个都没改**，只是补了文档。

## 14. 词义步草稿保存不再要求词形步已完成（后端已实现）

> **状态（2026-08-21）**：见 PR #45（`2a7c125`）。上游诉求：创建单词向导要能四步自由跳转——
> 词形与发音要查词典、补音标是慢活，而词义与例句往往是管理员手上现成的资料，
> 原先音标一时查不到就整条词条卡死，后面什么都干不了。

### 14.1 背景

`PUT /entries/{id}/steps/meanings` 原先无条件要求词形步已标记完成，否则直接
409 `step_not_reachable`。这条前置卡的是「词形步**已完成**」，而词义内容结构上真正依赖的
只是「引用的 `pos_id` 存在」——比实际需要严格得多，放宽的空间就在这个差值里。

### 14.2 后端改动

顺序前置从「所有 intent 都要求词形步已完成」收窄为**只对 `intent=complete` 成立**。

**两条前端必须知道的行为变化：**

| # | 变化 | 之前 | 现在 |
| --- | --- | --- | --- |
| 1 | 词形步未完成时 `intent=save` | 409 `step_not_reachable` | **200**，草稿正常落库 |
| 2 | 该响应的 `max_reachable_step` | 只会是 `meanings` / `preview` | **新增可能返回 `forms`** |

第 2 条容易被漏掉但会咬人：保存响应现在与 `GET` 详情用同一套派生口径（都按 `completed_steps`
推导，见 `helpers::max_reachable_step`），词形步未完成时两边都给 `forms`。
**若前端仍把 `max_reachable_step` 当导航门禁用**，就会出现「在第 3 步保存成功、响应一回来
就被弹回第 2 步」——存完就被踢走。

还有一条同源修正：该响应里的 `completed_steps` 此前把词形步**硬编码成已完成**
（`completed_steps(true, …)`，在旧前置下恒真），现在如实上报。前端凡是拿 `completed_steps`
画完成度的地方（步骤条、完成情况面板）都直接受益，也**必须**依赖这个如实值——不能再用
「排在当前步之前就算完成」之类的位置推断，那种推断只在顺序门禁存在时才成立。

`intent=complete` **没有**放宽：`completed_steps()` 的 `forms && meanings` 不变式使得词形未完成时
词义的完成标记根本存不下来，放宽只会变成静默丢弃。

放宽后仍然守得住，靠的是既有三层校验，本次**一层都没动**：

| 层 | 位置 | 作用 |
| --- | --- | --- |
| 存储安全网 | `editing.rs` `meaning_storage_is_safe` | 无条件挡住引用不存在 `pos_id` 的词义，**与 intent 无关** |
| 内容校验 | `validate_meanings` | 报出内容问题，`intent=complete` 时阻断 |
| 发布门 | `publishing.rs` | 独立重跑 `validate_forms` + `validate_meanings`，**与 `completed_steps` 无关** |

### 14.3 前端要怎么改

| 步骤 | 动作 |
| --- | --- |
| 1 | **不用**跑 `sync:openapi`——本次无 wire 变更，`docs/openapi.json` 重导无 diff |
| 2 | 拆掉把 `max_reachable_step` 当导航门禁的判断，否则会撞上 §14.2 第 2 条的「存完被踢走」 |
| 3 | 完成度一律读 `completed_steps`，别用步骤位置推断 |
| 4 | 「完成并进入下一步」（`intent=complete`）的错误处理保持不变，它仍会 409 `step_not_reachable` |

> `max_reachable_step` 本身**保留**，语义收窄为「续做落点」（第一个未完成的步骤），
> 适合用来决定列表「继续创建」跳到哪一步，**不适合**再当权限判断。

### 14.4 兼容性

**放宽是纯扩大成功集**：原本 409 的请求现在 200，原本成功的一个都不受影响。

旧前端也撞不到新行为——旧门禁保证「站在词义步」蕴含「词形步已完成」，此时
`forms_complete == true`，响应仍走老分支给 `meanings` / `preview`。因此**后端先部署是安全的**；
反过来前端先部署则会变成「能进去、能填、一保存就 409」，比放宽前更糟。**部署顺序：后端先，前端后。**

发布校验（`publishing.rs`）与 `helpers::max_reachable_step` 的派生逻辑**一个字都没改**，
无迁移、无 schema 变更。

## 15. 回滚到历史发布版本之后再发布不再 500（后端已实现）

> **状态（2026-08-21）**：修的是既有 bug，前端不必改代码，但**发布按钮的行为变了**，
> 值得知道一声。

### 15.1 背景

`POST /entries/{id}/publications/{publication_id}/activate`（回滚）只把 current publication
换成历史那条，**不动草稿的 `revision`**。于是回滚完 `GET /entries/{id}` 会返回：

| 字段 | 值 |
| --- | --- |
| `revision` | 仍是回滚前的 N+1（草稿内容一个字没变） |
| `published_revision` | 回滚到的那版 N |
| `has_unpublished_changes` | `true`（两者派生自 `published_revision != revision`） |

前端照常显示「有未发布改动」并给出发布按钮——**这个展示本身是对的**，当前对外发布的
确实不是草稿这一版。但点下去必然失败：`revision = N+1` 早就有一条 publication，
再插一条会撞 `(entry_id, source_revision)` 唯一约束，且没有被映射成业务错误，直接
**500 `internal_error`**。管理员从此发不出去，只能改一次稿把 revision 推走才能绕开。

### 15.2 后端改动

`POST /entries/{id}/publications` 新增一条分支：**草稿 `revision` 已经有对应的 publication 时，
把那条重新设为当前版本**，等价于对它做一次 activate。

`revision` 只随内容保存推进（`replace_entry_content` 是唯一写入点），所以同一 revision
的草稿与快照逐字相同——重新激活发布的就是管理员看到的那份内容，不存在错发。

| # | 场景 | 之前 | 现在 |
| --- | --- | --- | --- |
| 1 | 回滚后直接发布（草稿未改） | 500 `internal_error` | **201**，current publication 换回 pub#N+1 |
| 2 | 回滚后改稿再发布 | 201，新建 publication | 不变（revision 推到了没有 publication 的位置） |
| 3 | 草稿版本已是当前发布版 | 201，空转 | 不变 |

第 1 种情况的响应里：`published_revision` 变回 `revision`、`has_unpublished_changes` 变
`false`、`lifecycle_revision` **加 1**（换 current publication 是一次生命周期变更，与显式
activate 同口径），`entry_publications` 不会多出一条。

**第 1 行不是无条件 201**：这条分支走的是 publish 自己的引用校验，比 activate 更严。
若该词条有关联词、而目标词条在回滚这段时间里重新发布过（`target_headword` /
`target_gloss` 变了），会返回 **422 `relation_target_stale`**（提示重新保存词义步骤），
而对同一条 publication 直接调 activate 端点则是 200。两者并不矛盾——publish 的语义是
「发布当前草稿」，草稿里的关联词快照过期就该拦下来。这条 422 是**长期既有行为、本次没动**：
引用校验一直排在写 publication 之前，这类请求修复前后都是 422，从来没走到过那条 500。
所以 §15.4 的「纯扩大成功集」仍然成立——这个子集行为完全未变。

### 15.3 前端要怎么改

| 步骤 | 动作 |
| --- | --- |
| 1 | **不用**跑 `sync:openapi`——无 wire 变更，`docs/openapi.json` 重导无 diff |
| 2 | 发布成功后照常用响应里的 `lifecycle_revision` 覆盖本地值，别沿用请求前的旧值 |
| 3 | 若前端为了绕开这个 500 加过「回滚后隐藏/禁用发布按钮」之类的补丁，可以拆掉 |

第 2 条是唯一会咬人的地方：发布过去从不推进 `lifecycle_revision`，前端若把它当常量缓存，
下一次 activate / archive 会拿旧值撞 409 `revision_conflict`
（`field = "base_lifecycle_revision"`，`meta.current_lifecycle_revision` 给出真值）。

### 15.4 兼容性

**纯扩大成功集**：原本 500 的请求现在 201，原本成功的路径一条都没改。无 schema 变更、
无迁移，后端单独部署即可。

## 16. 重复词 duplicate 分支补齐被引用上下文（后端已实现）

> **状态（2026-08-23）**：wire 变更，前端需重跑 `sync:openapi`；按前端现有实现**无需改代码**。

### 16.1 背景

关联词被自动建成主词条后（PR #59），下次录入同名词应当提示「它是 XX 的同义词」，
避免管理员把这类空壳词条误判成「已经有人建过这个词了」。前端靠
`matched_entry_contexts[].inbound_relations` 反查出这条提示，但该字段只挂在
`smart_dictionary.status = "warning"` 的 `surface_match_page` 上。

`duplicate` 分支没有 `surface_match_page`：它是 legacy exact 回退，
`lexicon.entry_headword_keys` 里有精确词头键、surface 投影里却缺对应的 exact 行时触发
（B4 backfill 未追平、投影被 tombstone 等）。PR #58 已经给它补上了 `match_category`，
但被引用关系仍然缺，这条路径上前端什么都提示不出来。

### 16.2 后端改动

`DuplicateWordMatchV2` 新增**必填**字段 `inbound_relations`
（`RelationReferenceSummaryV2`），与 `matched_entry_contexts[].inbound_relations` 同构：
`previews` 同样最多 5 条、超出置 `truncated`，截断口径与 warning 分支共用同一段聚合代码。

入站关联只按 `entry_id` 反查（`surface_inbound_relations`），与 surface 投影无关，
所以投影缺失的回退路径照样给得全。distinguish 词条的 uk / us 两个词头键会让同一词条
在 `duplicates[]` 出两行，两行的摘要一致。

没有选「让 duplicate 分支也下发 `matched_entry_contexts`」：那要求同时下发
`surface_match_page`（上下文的覆盖不变量绑在命中项上），而这条路径的前提正是投影里
没有命中行可下发。

### 16.3 前端要怎么改

| 步骤 | 动作 |
| --- | --- |
| 1 | 跑 `sync:openapi`——`docs/openapi.json` 已重导，契约测试对着它对账 |
| 2 | 无需改代码：`duplicates[]` 直接多出这一项，「有则展示、无则整项略去」的现有渲染即可生效 |

### 16.4 兼容性

**纯新增字段**，旧前端忽略即可，无迁移、后端可单独部署。Redis 里最长存活 65 分钟的
detection 快照缺该字段，反序列化退化成空摘要（`total = 0`、无 preview），
部署空窗内不会 500。

## 17. 句子目标候选词形带上所属变化组的原形 id（后端已实现）

> **状态（2026-09-02）**：后端已实现并重新导出 `docs/openapi.json`；**部署顺序与以往相反，前端先。**

### 17.1 背景

短语 step3「成分用词」卡片用 `POST /entries/sentence-targets/resolve` 的候选列词形。候选按
`(entry, publication, pos, base_form_id, matched_variant_id)` 展开（命中词形能搭配几个原形就出
几条，跨组去重），`forms` 却是整个词性的清单；目标词条有多个变化组时，
改选另一组的屈折形只能沿用候选行的 `base_form_id`，保存校验 `phrase_component_matches_target`
要求 form 与 base 同组，于是 400 `invalid_request_body`（`field=component_usages`；
前端 PR 里说的 422 就是它）。前端手上没有任何数据能算出配套的 base，只能后端给。

### 17.2 后端改动

`SentenceTargetCandidateFormV3` 新增**必填**字段 `base_form_ids: Uuid[]`（`maxItems 2000`）：
该词形可搭配的原形 id：成分保存要求 form 与 base 同组（或 form 自身就是那个 base），这里给出
满足该条件的全部 base，按 id 排序去重，顺序不表示优先级。空数组即不可选：目标来自 V2 发布
（成分只接受 V3 发布的目标），或词形没挂进任何带原形的变化组。非空只表示同组这一条满足，
成分保存另有不得自指、目标短语不得再套短语等限制，仍可能被拒。候选行的 `base_form_id` 与 `senses[].base_form_id` 补了说明：
它们只对命中词形有效，改选其他词形时以该词形自带的 `base_form_ids` 为准。

### 17.3 前端要怎么改

| 步骤 | 动作 |
| --- | --- |
| 1 | 跑 `sync:openapi`——`docs/openapi.json` 已重导，`base_form_ids` 是必填字段，不同步会挂契约测试 |
| 2 | 改选词形时：候选行自己的 `base_form_id` 若在所选词形的 `base_form_ids` 里就沿用，否则任取一个（后端不区分同组内的多个原形）；更省事的做法是词形选择器直接按当前组过滤 |
| 3 | `base_form_ids` 为空的词形置灰——V2 发布的目标全部如此（成分只接受 V3 发布的目标），前端不必另辨版本 |
| 4 | 错误处理对准 **400** `invalid_request_body` / `field=component_usages`，不是 422。非空的 `base_form_ids` 只保证同组这一条，成分保存另有不得自指、目标短语不得再套短语等限制，这条错误路径不能省 |

### 17.4 兼容性

**这不是「纯新增字段、后端可单独部署」。** admin 前端把 resolve 响应灌进 fail-closed 的
runtime contract（`admin-word-v3.runtime-schema.json` 里 `SentenceTargetCandidateFormV3` 是
`additionalProperties: false`，`assertRuntimeContract` 对多余 key 抛
`InvalidAdminWordResponseError`）。后端先带上 `base_form_ids` 上线，短语成分查询与句子目标
发现会在 UI 上整体显示「词库查询失败」，直到前端重新同步。**部署顺序：前端先（或同时），后端后。**

后端侧纯增量：候选只产出不回读（不进 Redis/DB），无迁移、无 SQLx 缓存变化，revert 即可回退；
前端若已同步带 `base_form_ids` 的快照，回退后端后需同步回退快照。

## 18. 短语成分目标支持关键字检索（后端已实现）

> **状态（2026-09-03）**：后端已实现并重新导出 `docs/openapi.json`；**必须前后端同批部署**，见 §18.4。

### 18.1 背景

短语创建向导第 3 步「成分用词」把短语里的每个词关联到已发布词条，候选走
`POST /entries/sentence-targets/resolve`。该端点按 `normalized_surface` **等值**匹配，所以在
短语 `give me` 里点 `give`，只能选到词面正好是 `give` 的词条，选不到 `give up`——而后端本来
就允许短语做成分目标（最多套一层）。

前端自己接不了：智能词库列表的关键字端点只返回 `AdminWordListItemV3`（`id / presentation /
gloss / pos_list / levels`），拿不到成分关联必需的 `pos_id / base_form_id / form_id /
variant_id / sense_id`，也拿不到 §17 才下沉到后端的 `base_form_ids`。

### 18.2 后端改动

新增 `POST /api/v1/admin/lexicon/entries/component-targets/search`。能力门与 resolve 完全相同
（`SMART_LEXICON_V3_SENTENCE_TARGET_DISCOVERY`，关闭时同样 `503`
`smart_lexicon_v3_storage_unavailable`）。

请求 `SearchComponentTargetsV3Input`：

```jsonc
{
  "schema_version": 3,
  "q": "give",    // 必填，1..=100 码点，两端不留空白（带空白直接 422，前端自己 trim）
  "kind": "word", // 可选，word | phrase；不传则两者都返回
  "page_size": 50 // 可选，默认 50，上限 200
}
```

响应 `SearchComponentTargetsV3Response`：

```jsonc
{
  "schema_version": 3,
  "matches": [
    /* PublishedSentenceTargetCandidateV3[]，与 resolve 的 published_matches 完全同构 */
  ],
  "total": 12,
  "truncated": false
}
```

匹配语义与边界：

- 对 `surface`（词面原拼写）做大小写不敏感的**包含**匹配。关键字里的 `%` `_` `\` 一律按
  字面量转义，不是通配符。屈折词形也在索引里，搜 `harbours` 命中的是原形词条本身。
- 只回**已发布且未归档**的词条（`content_scope = 'current_publication'` +
  `entry.current_publication_id = source.publication_id` + `entry.archived_at IS NULL`）。草稿
  不进结果——成分关联要存 `target_publication_id`，草稿没有发布快照，保存时 `validate_phrase_components`
  会拒。
- 候选按 `headword` 排序；同词面内保持 `(entry, publication, pos, base_form, matched_variant)`
  的确定顺序。
- **每条候选的 `matches` 恒为空数组**：关键字检索没有句子区间，「命中了哪一段」无从谈起，
  后端不构造假证据。前端在关键字模式下别显示命中标识。
- `total` 是本次扫描窗口内的候选总数；`truncated` 为 true 时它是下界，不是全库命中数。
  `truncated` 有三种成因：命中数超过 `page_size`、触到后端一次取回 2000 条词面行的上限、
  或命中词条数超过 200（发布快照是整份 JSONB，一次最多取回 200 份）。前端只需按
  「结果已截断，请输入更具体的关键字」统一提示，不必区分。
- **已知匹配边界**：命中的是 `surface`（词面原拼写）而不是 `normalized_surface`。二者的差别
  只在排版字符上：`normalized_surface` 会把 `’ ‘ ʼ` 折成 `'`、各种连字符折成 `-` 再转小写。
  所以若某个词条发布时拼写用了弯引号（`don’t`），用 ASCII 撇号搜 `don't` 不会命中。
  当前库 214 条在线词面里没有一条存在这种差异，暂不影响使用；真出现了再评估是否改成
  对 `normalized_surface` 匹配。
- 词形自带的 `base_form_ids` 照旧下发（§17），改选词形仍以它为准。V2 发布的词条同样会
  出现在结果里（与 resolve 行为一致，不另辨版本），它们每个词形的 `base_form_ids` 恒为
  空数组，按 §17 的规则置灰即可。
- 本端点不重复校验成分保存的其余限制（不得自指、短语套短语只一层、每变体 100 条），
  那些仍由 `validate_phrase_components` 在保存时兜底，错误路径不能省。

错误码：`q` 为空 / 两端带空白 / 超过 100 码点 / 含 NUL → `422 validation_failed`
（`meta.code = "q"`）；`page_size` 越界 → `400 invalid_query`（`field = "page_size"`）。

### 18.3 前端要怎么改

| 步骤 | 动作 |
| --- | --- |
| 1 | 跑 `sync:openapi`——`docs/openapi.json` 已重导，新增 `SearchComponentTargetsV3Input` / `SearchComponentTargetsV3Response` 两个 schema，要进 `RUNTIME_SCHEMA_ROOTS` |
| 2 | 成分用词弹层的关键字为空时维持现有 resolve 调用（按词面等值），非空时（防抖后）改调新端点；两条路产出的候选同构，级联转换与写回逻辑不用动 |
| 3 | 发请求前 `trim()`；空串不发请求。带空白的 `q` 后端直接 422 |
| 4 | 关键字模式下不渲染「命中」标识——候选的 `matches` 恒为空 |
| 5 | `truncated` 为 true 时提示「结果已截断，请输入更具体的关键字」；`total` 别当成全库命中数展示 |

### 18.4 兼容性

**部署顺序：后端先，前端后**（与 §17 那次相反）。

理由要和 §17 区分清楚：§17 是**给既有响应 DTO 加字段**，后端先上会让前端 fail-closed 的
runtime contract（`additionalProperties: false`）在既有接口上整体报错，所以必须前端先。
本次**没有改动任何既有响应 DTO**——`PublishedSentenceTargetCandidateV3` /
`SentenceTargetCandidateFormV3` 等一个字段都没动，只新增了两个独立结构和一条新 path。
所以：

| 顺序 | 后果 |
| --- | --- |
| 后端先上 | 安全。前端不认识这条端点、不会调用；既有接口的响应形状逐字节未变，fail-closed 契约不触发 |
| 前端先上 | 关键字搜索请求 404。关键字为空时走 resolve 的既有路径不受影响，弹层不会整体挂掉 |

前端下次 `sync:openapi` 时 `docs/openapi.json` 的 `_source_sha256` 会变，契约快照测试要跟着更新——
那是前端 CI 的事，不影响线上运行时。

后端侧纯新增：无迁移、无数据变更、无 SQLx 缓存变化，revert 即可回退。

**已知性能取舍（有实测，有阈值）**：`ILIKE '%q%'` 的前置通配符用不上
`lexicon.surface_sources` 现有的四个 btree 索引，`SELECT DISTINCT + ORDER BY` 又强制全排序，
所以 `LIMIT` 只框住返回负载、救不了扫描。本地开发库 `EXPLAIN ANALYZE` 实测：

```
Seq Scan on surface_sources  (actual time=0.029..0.570 rows=102)
  Rows Removed by Filter: 1140
Execution Time: 3.743 ms       -- 全表 1242 行 / 60 buffer page
```

按行数线性外推：10 万行约 46ms、100 万行约 460ms、1000 万行约 4.6s。当前 69 个已发布词条
产出 1242 行（约 18 行/词条），也就是 **5 万词量级 ≈ 90 万行 ≈ 400ms/次**——配 300ms 防抖
勉强可用，再往上必须换方案。

本次**没有**装 `pg_trgm`、也没有加新索引，只靠「只扫 `current_publication` 分片」与「一次
最多取回 2000 条词面行」缓解。越过上面的阈值时，正确做法是单独排一次索引迁移
（`CREATE EXTENSION pg_trgm` + `surface` 上的 GIN 索引），不在本 PR 里做。

## 19. V3 列表行补方言摘要 `dialects`（后端已实现）

> **状态（2026-09-02）**：后端已实现并重新导出 `docs/openapi.json`；**前后端必须同批部署**（见 §18.4）。

### 19.1 背景

智能词库列表的「方言」列在 V2 行读 `AdminWordListItem.dialects`（由 `entry_headwords` 按
`headword_mode` 聚合：unified → `[common]`，distinguish → `[uk, us]`）。V3 行的
`AdminWordListItemV3` 一直没有等价字段，admin 对 V3 行固定渲染 `-`；库里全是 V3 词条后整列失效。

V3 的「方言」在数据上有两个来源：建条 step 1 的英美选择只是 `v3_entry_state.initial_headwords`
里的**一次性快照**（建条后不再更新，仅用于 `detection_basis_dialect` 展示与 surface 校验）；
真正决定词形结构与发布内容的是**各词性**的 `dialect_rules.spelling_mode`（`entry_pos.spelling_mode`），
词形步可随时改，且只在建条那一刻按词典建议灌过一次。两者可分叉（例：建条选通用、后来在词性页
改成英美的 `colour up`）。列表摘要以后者为准。

### 19.2 后端改动

`AdminWordListItemV3` 新增**必填**字段 `dialects: Dialect[]`，按词性**当前**设置聚合：

| 词性 `spelling_mode` 集合 | `dialects` | 前端展示 |
| --- | --- | --- |
| 含任一 `distinguish` | `["uk", "us"]` | 英式英语 / 美式英语 |
| 全为 `unified` | `["common"]` | 默认 |
| 没有任何词性 | `[]` | `-`（未知，前端不落回默认） |

只读 `entry_pos.spelling_mode`（V2/V3 行由约束保证非空；legacy 行为 NULL 已过滤），不看
`initial_headwords`，也不看 `matched_surfaces`。V2 行的 `dialects` 语义与顺序不变。

### 19.3 前端要怎么改

| 步骤 | 动作 |
| --- | --- |
| 1 | 跑 `sync:openapi`——`dialects` 是必填字段；runtime schema 对 `AdminWordListItemV3` 既 `additionalProperties: false` 又校验 `required`，不同步会让整个列表接口解码失败 |
| 2 | `wordListDialects` 两个 schema 都直接读 `record.dialects`；空数组维持 `-`，不要落回「默认」 |
| 3 | mock / e2e 桩的 V3 列表行补 `dialects` |

### 19.4 兼容性

这不是「纯新增字段、可单独部署」，而且**哪一侧先上都会短暂打挂列表**：后端先带 `dialects` 上线，
旧前端的 runtime schema 因 `additionalProperties: false` 拒收；前端先上，新 runtime schema 又因
`required` 缺字段拒收（`missing_required_property`）。两种情况 admin 列表页都整体显示
「词库查询失败」，直到另一侧跟上。**前后端同批部署，间隔压到最短。**

后端侧纯增量：列表 SQL 多一个标量子查询，无迁移、无 SQLx 缓存变化，revert 即可回退。

_维护约定：auth 相关响应形状变更时同步本文档 §3/§4；后端契约任务进度看
`frontend-contract-alignment.md`。词性配置契约变更同步本文档 §7 与前端
`docs/features/refactor-word-creation/design.md`。_
