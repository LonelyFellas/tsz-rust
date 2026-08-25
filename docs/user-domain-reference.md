# user 域需求参照（来自 tsz-go）

> 用途：**重设计 user 域 schema 时的需求依据**。不是让你照抄，而是把 tsz-go 里每张表、每个字段、每条约束**为什么存在**讲清楚——尤其那些不显然、踩坑换来的决定。
> 你据此逐条决定「保留 / 修改 / 丢弃」，别因为没看懂就漏掉。
>
> 覆盖范围：`users` / `user_roles` / `student_profiles` / `teacher_profiles` / `verification_codes`(OTP) / `refresh_tokens`(会话)。
> 来源：tsz-go 迁移 000001–000022 的 user 域部分 + `internal/user`、`internal/otp` 服务层。

---

## 0. 反复出现的设计原则（先看这个）

这几条贯穿所有表，理解它们比看单张表更重要：

| 原则 | 内容 | 对 Rust 重设计的提示 |
|------|------|---------------------|
| **双身份分离** | web 用户（`users`：学生/教师）与后台 admin 是**两套独立账号**，不共享。user 域只管前者 | MVP 只做 `users`，admin 留到第二周 |
| **身份与角色解耦** | 登录身份（`users`）本身**不带角色**；一个账号可同时是学生+教师，角色放 `user_roles`，当前激活角色放 JWT | 核心决定，建议保留 |
| **TEXT + CHECK 而非 enum 类型** | go 里所有枚举字段用 `TEXT + CHECK(...)`，理由是 pgx 扫描简单、迁移便宜 | ⚠️ **这是 go/pgx 的取舍，Rust 可重新考虑**：sqlx 支持 Postgres enum 或 Rust enum ↔ TEXT。见下方「决定清单」 |
| **错误不可区分（隐私）** | 「验证码错 / 过期 / 已用 / 手机号未注册」全部返回**同一个错误**，绝不透露「这个号是否注册」 | 安全要求，务必保留 |
| **全有或全无写入** | 成对字段（如学习设置）一起写，杜绝「半设置」状态 | 保留 |
| **失效在一个 access-token TTL 内生效** | 禁用账号、登出等不立刻杀 access token（它无状态短期有效），而是在下次 refresh/校验时拦截 | 理解即可 |

---

## 1. `users` —— 认证身份主表

**职责**：登录账号。角色无关，一个账号可持多角色。

| 字段 | 类型 | 约束 | 为什么 |
|------|------|------|--------|
| `id` | UUID | PK | |
| `phone` | TEXT | 可空、部分唯一 | 原本必填主标识；000014 改成**可空**，允许「只有邮箱」的账号 |
| `email` | TEXT | 可空、部分唯一（小写） | 可选标识 |
| `password_hash` | TEXT | NOT NULL | bcrypt（`DefaultCost`） |
| `display_name` | TEXT | NOT NULL | 昵称 |
| `last_active_role` | TEXT | CHECK(student/teacher)、可空 | **000004**：当前激活角色要**持久化**，否则每次 refresh 都重置成默认角色、悄悄撤销用户的「切换角色」。NULL = 从未切换 → 回落默认角色 |
| `status` | TEXT | NOT NULL DEFAULT 'active'、CHECK(active/disabled) | **000008**：账号生命周期。`disabled` 在登录边界拦截（密码/验证码登录、refresh 都拒绝），一个 TTL 内生效 |
| `avatar_url` | TEXT | NOT NULL DEFAULT '' | **000015**：头像的**不透明字符串引用**（文件在 OSS，不是图片字节）。默认 `''`，格式（key 还是 URL）故意未定，等存储后端落地再定 |
| `registration_ip` | TEXT | 可空 | 注册那一刻的客户端 IP，取自可信反代覆盖写入的 `X-Forwarded-For` 最左段，**只写一次、之后不更新**。用途是后台按地区看用户分布；IP→省市的解析放在读取侧，这里只存原始地址。反代没配该头（或本地直连）时为 NULL，不影响注册 |
| `created_at` / `updated_at` | TIMESTAMPTZ | NOT NULL DEFAULT now() | |

**约束与索引（重点）：**
- `users_phone_unique`：`phone` **部分唯一**（`WHERE phone IS NOT NULL`）——多个「无手机」账号不算冲突。
- `users_email_unique`：`lower(email)` **部分唯一**——大小写不敏感，且只对有邮箱的行。应用层也会先小写化。
- **`users_phone_or_email_present`**（000014，关键）：`CHECK (phone IS NOT NULL OR email IS NOT NULL)`——每个账号至少有一个可达标识，数据库边界就拒绝「手机邮箱都空」的行。

---

## 2. `user_roles` —— 用户持有的角色

**职责**：记录一个用户是 student / teacher / 两者。

| 字段 | 类型 | 约束 |
|------|------|------|
| `user_id` | UUID | NOT NULL、REFERENCES users ON DELETE CASCADE |
| `role` | TEXT | NOT NULL、CHECK(student/teacher) |
| `created_at` | TIMESTAMPTZ | NOT NULL DEFAULT now() |
| PK | | `(user_id, role)` |

**为什么**：当前激活角色在 JWT 里、不在这——这表只记「持有哪些」。复合主键保证同一用户同一角色不重复。

---

## 3. `student_profiles` / `teacher_profiles` —— 角色专属资料

**为什么分两张表**：学生和教师属性发散，用 `user_id` 作 PK 保证「每用户每角色一份资料」。

**`student_profiles`：**

| 字段 | 类型 | 约束 | 为什么 |
|------|------|------|--------|
| `user_id` | UUID | PK、REFERENCES users ON DELETE CASCADE | |
| `grade` | TEXT | NOT NULL DEFAULT '' | 年级 |
| `cefr_level` | TEXT | CHECK(A1/A2/B1/B2/C1/C2)、可空 | **000006 学习设置** |
| `english_variant` | TEXT | CHECK(BrE/AmE)、可空 | **000006 学习设置** |

> **学习设置的关键规则**：`cefr_level` 和 `english_variant` 两个 onboarding 选择**成对写入（全有或全无）**。都为 NULL = 还没完成 onboarding——应用**从这两个字段是否已设**推导「是否 onboarded」，而不是单独加个 flag 字段。

**`teacher_profiles`：**

| 字段 | 类型 | 约束 |
|------|------|------|
| `user_id` | UUID | PK、REFERENCES users ON DELETE CASCADE |
| `bio` | TEXT | NOT NULL DEFAULT '' |
| `verified` | BOOLEAN | NOT NULL DEFAULT false |

---

## 4. `verification_codes` —— 一次性验证码（OTP）

**职责**：验证码登录、找回密码、注销账号、绑定新联系方式，都走它。

| 字段 | 类型 | 约束 | 为什么 |
|------|------|------|--------|
| `id` | UUID | PK | |
| `target` | TEXT | NOT NULL | 目标值（手机号或邮箱） |
| `channel` | TEXT | NOT NULL、CHECK(sms/email) | 下发渠道 |
| `purpose` | TEXT | NOT NULL、CHECK(...) | 用途，**随需求逐步扩容**（见下） |
| `code` | TEXT | NOT NULL | 6 位数字 |
| `expires_at` | TIMESTAMPTZ | NOT NULL | 时限 |
| `consumed_at` | TIMESTAMPTZ | 可空 | 单次使用标记 |
| `attempts` | INT | NOT NULL DEFAULT 0 | **000005**：失败尝试计数 |
| `created_at` | TIMESTAMPTZ | NOT NULL DEFAULT now() | |

**`purpose` 的演进**（4 个值，各自对应一条业务线）：
- `login`（000002）验证码登录
- `password_reset`（000012）找回密码，短信下发到已注册手机，`/auth/password/reset` 消费
- `account_deletion`（000013）注销账号，短信/邮件下发到账号自己的联系方式，`DELETE /auth/account` 消费
- `contact_bind`（000016）绑定**新**联系方式（不是已有的），证明用户控制它后 `POST /me/contact/bind` 写入

**索引**：`(target, purpose, created_at DESC)`——Verify 查「某 target+purpose 最近一条未消费的码」。

**踩坑换来的安全约束（务必保留）：**
- **尝试次数上限 = 5**（`maxVerifyAttempts`，000005）：6 位码只有 100 万种可能，若错误猜测既不消费码也不计数，在整个 TTL 内可被在线爆破。到上限就锁死这个码。
- **码错/过期/已用/号未注册 → 同一个错误**：不可区分，防止泄露「哪些号注册了」。
- 6 位数字用**拒绝采样**生成（`v>=250` 丢弃），保证每位数字均匀分布——别用 `byte%10`（256 不是 10 的倍数，会偏向 0–5）。
- OTP 服务还有 **cooldown（两次发送间隔）** 和 **dailyLimit（每日上限）**、**ttl**，都可配置。

---

## 5. `refresh_tokens` —— 会话（access/refresh 方案）

**职责**：access token 是无状态 JWT（中间件本地校验）；refresh token 是**不透明高熵随机串**，这里存它的 **SHA-256 哈希**。只有低频的 `/auth/refresh`、`/auth/logout` 碰这表。

| 字段 | 类型 | 约束 | 为什么 |
|------|------|------|--------|
| `id` | UUID | PK | |
| `user_id` | UUID | NOT NULL、REFERENCES users ON DELETE CASCADE | |
| `token_hash` | TEXT | NOT NULL、唯一 | 存哈希不存明文 |
| `expires_at` | TIMESTAMPTZ | NOT NULL | |
| `revoked_at` | TIMESTAMPTZ | 可空 | **硬吊销**（登出、异地登录、改密）——无宽限 |
| `rotated_at` | TIMESTAMPTZ | 可空 | **000022**：轮换消费，与硬吊销分开 |
| `created_at` | TIMESTAMPTZ | NOT NULL DEFAULT now() | |

**索引**：`refresh_tokens_user (user_id)`、`refresh_tokens_hash (token_hash)` 唯一。

**两条核心机制（容易漏，务必理解）：**
1. **单设备严格登录**：签发新 token 会吊销该用户的其它 token（`revoked_at`）——旧设备在其 access 过期后被踢下线。
2. **轮换宽限（000022）**：`rotated_at`（轮换消费）与 `revoked_at`（硬吊销）**分开**。已轮换的 token 在**短宽限窗口**内可被重放（并行标签页同时刷新、客户端丢响应后重试），不杀会话；**超窗口的重放视为 token 被盗，吊销该用户全部会话**。硬吊销不给宽限。

---

## 6. 跨表业务规则（schema 之外，但属于需求）

| 规则 | 细节 |
|------|------|
| **密码哈希** | bcrypt，`DefaultCost` |
| **display_name 校验** | 1–50 字符（binding 层）；trim 后不能为空；**禁 `<` `>` 和控制/格式字符**（防 XSS 标签），但 `'` `"` `&` 允许（O'Brien、Tom&Jerry 这类合法昵称，没有 `<>` 构不成标签） |
| **phone 校验** | 长度 5–20 |
| **注册必须至少一个标识** | 手机或邮箱二选一（对应 DB 的 `users_phone_or_email_present`） |
| **邮箱小写化** | 写入前小写，配合 `lower(email)` 唯一索引 |
| **角色非空** | 注册时至少一个角色 |
| **禁用账号** | 登录/refresh 全拒，一个 access TTL 内生效 |

---

## 7. 重设计时要**主动拍板**的决定清单

这些是 go 编码进 schema 的决定，你逐条决定「保留 / 改」：

1. **枚举怎么存**：go 用 `TEXT + CHECK`。Rust/sqlx 下你有三选：
   - 沿用 `TEXT + CHECK`（迁移简单，但校验在 DB）
   - Postgres 原生 `ENUM` 类型（更强，但加值要迁移）
   - Rust 侧 `enum` + `#[sqlx(type_name=...)]` 映射到 TEXT
   → **建议**：角色/状态这类稳定小集合用 Rust enum ↔ TEXT，兼顾类型安全与迁移便利。
2. **角色建模**：`user_roles` 独立表 + 多角色 —— 建议保留（这是产品核心）。
3. **profile 分表 vs 合表**：student/teacher 分表是因属性发散。若 MVP 只做学生，可暂缓 teacher_profiles。
4. **OTP 与 user 是否同库**：go 里 `verification_codes` 无外键指向 users（target 是裸手机号/邮箱，注册前就要发码）。保留这个「无外键」设计。
5. **会话表宽限机制**：`rotated_at` vs `revoked_at` 分离较复杂。MVP 可先只做单设备 + 硬吊销（`revoked_at`），宽限（`rotated_at`）作为增强后加——但**别忘了它的存在**。
6. **头像字段**：格式未定的占位。MVP 可先建 `avatar_url TEXT DEFAULT ''`，等 OSS 落地再定语义。
7. **软删除 vs 硬删除**：go 的账号注销走 `DELETE`（配 CASCADE）。你要不要改成软删除（加 `deleted_at`）？影响唯一索引和查询，早定。

---

## 8. MVP 建议范围

第一周 user 域**先做这些表**（够登录注册跑通）：
- `users`（含 phone/email 二选一、status、last_active_role）
- `user_roles`
- `student_profiles`（含学习设置）
- `verification_codes`（先 `login` 一个 purpose，其余按需加）
- `refresh_tokens`（先单设备 + `revoked_at`，宽限后加）

**暂缓**：`teacher_profiles`、`avatar_url`、`password_reset`/`account_deletion`/`contact_bind` 三个 purpose、轮换宽限。
