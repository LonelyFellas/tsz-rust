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

## 8. 智能词库列表：词汇列按检测基准侧排序（后端已实现）

> **状态（2026-08-19）**：后端已实现并生成 `docs/openapi.json`。

### 8.1 背景

同一词条的英美并列拼写，此前在三处顺序不一致：词条详情左栏与建稿第 4 步「主词」按
**检测基准侧**（`headwords.source_dialect`，即管理员当初输入的那一侧）排序，而
`GET /admin/lexicon/entries` 列表行的 `headword` 由后端 `string_agg` 固定按
`common → uk → us` 拼好。结果 `source_dialect = "us"` 的词条在列表显示
`colour / color`，点进详情却是 `color / colour`，管理员无法判断哪个是主词。

### 8.2 后端改动

1. `AdminWordListItem.headword` 与 `AdminWordListItem.dialects` 改为**同序**，排序规则统一为
   「`common` → 检测基准侧 → 另一侧」。即 `source_dialect = "us"` 时列表返回
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
| 2 | `packages/types/src/admin-word.ts` 的 `AdminWordListItem` 补 `source_dialect?: SourceDialect`，并把 `headword` / `dialects` 的注释改为「检测基准侧在前」 |
| 3 | 智能词库列表的「词汇」「方言」列**无需改渲染逻辑**——直接消费即可得到正确顺序；如需在列表标注主词，用新增的 `source_dialect` 判断，不要切分 `" / "` |
| 4 | mock（`apps/admin/src/features/dictionary/mock/`）里的 `distinguish` 行补上 `source_dialect` 并按基准侧在前重排 `headword` / `dialects`，否则 mock 与真实后端不一致 |

### 8.4 兼容性

`source_dialect` 是**可选新增字段**，`required` 数组不变，既有消费者（含
`endpoints.contract.test.ts` 对 `AdminWordListItem.required` 的 `arrayContaining` 断言）不受影响。
唯一的行为变化是 `distinguish` 词条 `headword` / `dialects` 的元素顺序——这正是本次要修的缺陷。

## 9. 关联词搜索与引用预览：并列拼写同样按检测基准侧排序（后端已实现）

> **状态（2026-08-20）**：后端已实现，OpenAPI schema 不变（仅字段取值顺序变化）。

### 9.1 背景

§8 只改了 `GET /admin/lexicon/entries` 列表行。关联词搜索
（`GET /admin/lexicon/entries/related-search`）与建稿第 1 步的「已有词条被谁引用」预览，
仍固定按 `uk → us` 拼词头，于是 `source_dialect = "us"` 的同一个词条在列表显示
`color / colour`，在关联词搜索结果里却是 `colour / color`——正是 §8 要消灭的那类矛盾换了位置。

### 9.2 后端改动

统一为与 §8 完全相同的规则「`common` → 检测基准侧 → 另一侧」，涉及：

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
本次一并改成基准侧在前，两者继续逐字符相同。副作用是 `source_dialect = "us"` 的
`distinguish` 词条在结果里的字母序位置会变（这正是「按看到的词头排序」应有的行为）；
`next_cursor` 的**格式与语义不变**，但上线瞬间已经发出去的游标可能落在稍有偏差的边界上，
重新发起一次搜索即可。线上尚无已发布的 `distinguish` 词条，实际不受影响。

### 9.3 前端要怎么改

| 步骤 | 动作 |
| --- | --- |
| 1 | 关联词搜索下拉、引用预览**无需改渲染逻辑**——直接消费即可得到正确顺序 |
| 2 | 仍然不要用 `split(" / ")` 反推英/美拼写；需要基准侧就读 `AdminWordV2.headwords.source_dialect`（列表行读 §8 新增的 `AdminWordListItem.source_dialect`） |
| 3 | mock（`apps/admin/src/features/dictionary/mock/`）里 `distinguish` 词条的 `RelatedWordResult.headword` / `dialects` 与 `source_headword` 按基准侧在前重排，否则 mock 与真实后端不一致 |

### 9.4 兼容性

不新增/删除任何字段，`RelatedWordResult`、`RelationReferencePreviewV2` 的 schema 与
`required` 数组均不变，因此**不需要重新 `sync:openapi`**。唯一的行为变化是 `distinguish`
词条这几个字符串/数组的元素顺序。线上 `lexicon.relations` 目前 0 行、草稿里也没有关联数据，
不存在需要回填的存量快照。

## 10. 语法结构不再强制英美双条（后端已实现）

> **状态（2026-08-20）**：后端已实现，OpenAPI schema 不变（只放宽校验），**不需要重新 `sync:openapi`**。
> 上游依据：前端 `docs/features/dialect-preference-migration/design.md` 提案 P1。

### 10.1 背景

英美方言偏好化（A1）之后，平台自己写的英文行文只维护一份。但后端此前要求
`distinguish` 词条的 `grammar_structures[].variants` **精确等于** `[uk, us]`，
前端只能写「两条同值镜像」：wire 里多一份冗余，学习端将来会读到
「英式：a centre／美式：a centre」这种没有信息量的两行。

### 10.2 后端改动

`distinguish` 词条的语法结构变体集合现在同时接受 `[common]` 与 `[uk, us]`；
`unified` 词条维持只接受 `[common]`。缺一侧（只写 `uk`）、多一侧（`common` + `uk`）、
方言重复，仍然照旧报 `grammar_variants_invalid`。

**AI 内容补全暂未跟着收敛**（提案 P1-b）：`content_completion` 对 `distinguish` 词条
仍生成 uk / us 两份同值镜像。**这是有意押后**——现网前端对 `distinguish` 词条硬性要求
`[uk, us]`，后端先产单份会让 AI 补全结果在第 3 步显示为「未填写」。
**请在阶段 3 发布后告知后端**，后端随即改成单份 `common`（另开一个小 PR）。

### 10.3 前端要怎么改

| 步骤 | 动作 |
| --- | --- |
| 1 | 删掉「两条同值镜像」shim（design.md 阶段 6），`distinguish` 词条直接写单条 `common` |
| 2 | 前端校验口径同步放宽：`unified ⇒ [common]`，`distinguish ⇒ [common]` 或 `[uk, us]` |
| 3 | **收敛时要给 common 变体换新节点 ID**：节点角色里带方言（`meanings.content:en:<dialect>`），沿用旧 uk 变体的 ID 只改 `dialect` 会被判 `node_binding_changed` |
| 4 | **反向同理**：把已收敛的词条改回 uk / us 双条，必须沿用最初那两个变体的节点 ID，用新 UUID 会被判 `stable_node_id_changed` |
| 5 | mock（`apps/admin/src/features/dictionary/mock/`）与真实后端同口径，否则 mock 绿、真机 422 |

### 10.4 兼容性

纯放宽：存量的双条数据继续可读可写，旧前端一行不改也照常工作，`AdminWordV2` 的 schema
与 `required` 数组均不变。后端产出侧**没有任何行为变化**（见上，AI 补全仍产双份），
所以本节可以先于前端任何改动上线。

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

§8 / §9 之后，`headword` 已经按「`common` → 检测基准侧 → 另一侧」拼好，`dialects` 与之同序。
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

_维护约定：auth 相关响应形状变更时同步本文档 §3/§4；后端契约任务进度看
`frontend-contract-alignment.md`。词性配置契约变更同步本文档 §7 与前端
`docs/features/refactor-word-creation/design.md`。_
