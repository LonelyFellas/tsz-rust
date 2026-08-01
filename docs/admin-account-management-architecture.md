# 管理员账号管理后端架构设计

> 状态：提案，待评审  
> 目标项目：`tsz-rust`  
> 目标版本：Admin 二期——管理员账号管理  
> 更新日期：2026-07-28

## 1. 背景

管理后台前端原先对接 `tsz-go` 的管理员账号管理接口。随着后端迁移到
`tsz-rust`，需要在 Rust 项目中重新实现这一能力。

本设计不照搬 Go 项目的内部模型，而是遵循 Rust 项目已经确定的管理员领域规则：

- 后台管理员与 C 端用户是两套完全独立的身份体系；
- 管理员只使用手机号，不保存 email；
- 身份字段统一为 `role`，值为 `admin` 或 `super_admin`；
- 不实现 Go 版 RBAC、角色分配和权限委派；
- `super_admin` 是治理顶点，只能通过带外 seed 创建；
- 管理员密码登录继续遵循现有的 2FA、锁定和强制改密规则。

## 2. 需求评估

产品需求可归纳为三个业务能力：

1. 超级管理员创建普通管理员；
2. 超级管理员查看管理员列表；
3. 超级管理员管理普通管理员。

第三项包含两种具有不同安全语义的操作：

- 启用或禁用账号；
- 重置密码并吊销会话。

因此，完整的管理员列表页面需要四个 HTTP 接口，而不是把所有管理动作合并成一个
通用接口：

| 方法 | 路径 | 用途 |
| --- | --- | --- |
| `POST` | `/api/v1/admin/admins` | 创建普通管理员 |
| `GET` | `/api/v1/admin/admins` | 查询管理员列表 |
| `PATCH` | `/api/v1/admin/admins/{admin_id}/status` | 启用或禁用普通管理员 |
| `POST` | `/api/v1/admin/admins/{admin_id}/reset-password` | 重置普通管理员密码 |

不建议设计成 `PATCH /admins/{id}` 加一个通用 `action` 字段。账号状态变更与密码重置在
请求体、响应体、副作用、审计信息和幂等语义上都不同，拆分端点更容易约束权限和避免误用。

### 2.1 当前 Rust 项目就绪度

已经具备：

- `admins`、`admin_refresh_tokens` 数据表；
- `Admin`、`AdminRole`、`AdminStatus` 领域模型；
- 管理员认证、JWT realm 隔离和 `AdminAuth` 提取器；
- `AdminRepository` 的创建、按 ID 查询、改密等基础能力；
- refresh session 的批量吊销能力；
- 管理员密码策略与 `must_change_password` 机制；
- `/api/v1/admin` 聚合路由和 OpenAPI 基础设施。

仍需实现：

- `accounts` 管理子模块；
- 可复用的当前管理员状态/强制改密/超级管理员门禁；
- 安全的管理员公开 DTO；
- 列表筛选与分页查询；
- 普通管理员状态更新；
- 密码重置与会话吊销事务；
- 业务错误映射、OpenAPI 注册和端到端测试。

### 2.2 复杂度与风险

本需求需要一条向后兼容的数据库迁移，为 `admins` 增加创建人字段。整体复杂度为中等，主要风险
不在 CRUD 本身，而在：

- 将带有 `password_hash` 的内部 `Admin` 模型误序列化；
- 允许调用者传入 `role=super_admin`；
- 对 `super_admin` 执行禁用或重置；
- 密码重置和 session 吊销之间出现部分成功或并发窗口；
- 临时密码进入数据库明文字段、日志或错误信息；
- 前端仍使用 Go 契约中的 `level` 和 `email`，导致联调字段错位。

## 3. 范围

### 3.1 本期包含

- 四个管理员账号管理接口；
- 记录普通管理员的创建人；
- 全接口 `super_admin` 权限门禁；
- 创建普通管理员并返回一次性临时密码；
- `role`、关键字和分页查询；
- 普通管理员启用/禁用；
- 普通管理员密码重置；
- 重置时吊销该管理员全部 refresh session；
- OpenAPI、SQLx 离线缓存和自动化测试；
- 面向现有管理后台的契约迁移说明。

### 3.2 本期不包含

- 创建、提升、降级、禁用或重置 `super_admin`；
- 删除管理员；
- 修改管理员手机号、昵称或 role；
- 管理员 email；
- RBAC、角色列表、权限分配和 `/admins/{id}/role`；
- 持久化审计表；
- access token 黑名单或即时吊销；
- C 端用户列表与用户管理接口。

## 4. 设计原则

1. **默认拒绝**：请求体没有 `role` 字段，创建结果固定为普通管理员。
2. **治理顶点不可互操作**：任何 `super_admin` 都不能通过这些接口被禁用或重置，包括调用者自己。
3. **内部模型不直接出网**：响应使用白名单 DTO，不给 `Admin` 直接派生 `Serialize`。
4. **秘密只出现一次**：临时密码只在创建或重置成功响应中返回，不落明文、不打印日志。
5. **关键副作用原子化**：密码、强制改密标志和 refresh session 吊销在同一事务提交。
6. **稳定分页**：列表排序使用 `created_at DESC, id DESC`。
7. **单一 Repository**：继续使用共享 `AdminRepository`，不在 `auth` 或 `accounts` 中复制第二套管理员仓储。
8. **创建人可信**：创建人只取自已认证的 `SuperAdminAuth`，请求体不能传入或覆盖。
9. **后端强制鉴权**：前端隐藏菜单只是体验优化，不能代替服务端权限检查。

## 5. 目标模块结构

```text
src/admin/
├── accounts/
│   ├── mod.rs
│   ├── dto.rs
│   ├── handler.rs
│   ├── service.rs
│   └── router.rs
├── auth/
│   ├── handler.rs
│   ├── mod.rs
│   └── router.rs
├── profile/
│   ├── handler.rs
│   └── mod.rs
├── extract.rs
├── model.rs
├── repository.rs
├── router.rs
├── service.rs
└── session.rs
```

职责边界：

- `accounts/dto.rs`
  - 请求、响应和分页 wire 类型；
  - 只包含允许暴露的字段；
  - `serde` 与 `utoipa` 派生集中在此。
- `accounts/handler.rs`
  - Axum extractor；
  - 将 `JsonRejection`、`QueryRejection`、path 解析失败统一映射为 `AppError`；
  - HTTP 状态码；
  - service error 到 `AppError` 的映射；
  - `tracing` 事件。
- `accounts/service.rs`
  - 创建、查询、状态变更和重置密码的业务规则；
  - 临时密码生成与回验；
  - 不处理 HTTP。
- `accounts/router.rs`
  - 只注册 `/admins` 子路径；
  - 所有 handler 都要求超级管理员身份。
- `admin/repository.rs`
  - 管理员聚合的唯一数据库访问边界；
  - 新增列表、状态更新和重置事务方法。
- `admin/extract.rs`
  - 保留 `AdminAuth`；
  - 新增可复用的强制改密与 `SuperAdminAuth` 门禁。

顶层聚合：

```rust
pub fn router() -> Router<AppState> {
    Router::new()
        .route("/profile", get(profile::handler::admin_profile))
        .nest("/auth", auth::router())
        .nest("/admins", accounts::router())
}
```

## 6. 领域模型与公开 DTO

### 6.1 创建人字段

新增自引用外键：

```sql
ALTER TABLE admins
    ADD COLUMN created_by_admin_id UUID NULL,
    ADD CONSTRAINT admins_created_by_admin_id_fkey
        FOREIGN KEY (created_by_admin_id)
        REFERENCES admins(id)
        ON DELETE RESTRICT;

CREATE INDEX admins_created_by_admin_id_idx
    ON admins(created_by_admin_id);
```

语义：

- API 创建的普通管理员：`created_by_admin_id = 当前 SuperAdminAuth.subject`；
- seed 创建的超级管理员：`created_by_admin_id = NULL`；
- 迁移前已有账号：`created_by_admin_id = NULL`，表示历史来源未知；
- 创建人字段创建后不可经 API 修改；
- 创建人只来自认证上下文，请求体没有该字段；
- 使用 `ON DELETE RESTRICT` 保留来源链；本期原本就不提供管理员删除接口。

`created_by_admin_id` 只记录“最初由谁创建”，不替代完整审计日志。状态变更、密码重置等后续操作
仍需要 tracing，未来再由独立 audit 域承接。

`NewAdmin` 增加：

```rust
pub created_by_admin_id: Option<Uuid>
```

seed 显式传 `None`，管理接口显式传 `Some(actor.subject)`。现有数据保持可读，不需要回填一个虚假的
创建人。

### 6.2 内部模型

内部 `Admin` 保持非序列化，其中包含敏感或仅供认证使用的字段：

- `password_hash`
- `must_change_password`
- `failed_login_count`
- `locked_until`

禁止为了方便响应而给整个 `Admin` 添加 `Serialize`。

### 6.3 公开管理员对象

账号管理接口统一返回：

```rust
pub struct AdminAccountResponse {
    pub id: Uuid,
    pub phone: String,
    pub display_name: String,
    pub role: AdminRole,
    pub status: AdminStatus,
    pub created_by: Option<AdminCreatorResponse>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

pub struct AdminCreatorResponse {
    pub id: Uuid,
    pub display_name: String,
}
```

明确不返回：

- 密码 hash；
- `must_change_password`；
- 失败登录次数；
- 锁定截止时间；
- refresh session；
- email。

`created_by` 的响应语义：

- API 新建账号：返回创建人的稳定 ID 和当前昵称；
- seed 或历史账号：返回 `null`，前端显示“系统 / 历史数据”；
- 列表查询通过 `LEFT JOIN admins creator ON creator.id = a.created_by_admin_id` 读取；
- 不返回创建人的手机号等无关信息。

`AdminStatus` 需要补充安全的 wire 派生或建立独立 wire enum：

```rust
#[derive(Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum AdminStatus {
    Active,
    Disabled,
}
```

## 7. 鉴权与授权

### 7.1 认证链

管理员账号治理端点统一执行：

```text
Bearer admin access token
        ↓
校验 admin JWT 密钥、audience/realm、过期时间
        ↓
读取当前管理员
        ↓
账号存在且允许继续访问
        ↓
must_change_password == false
        ↓
role == super_admin
        ↓
进入 handler
```

建议新增组合提取器：

```rust
pub struct SuperAdminAuth {
    pub subject: Uuid,
    pub display_name: String,
}
```

`display_name` 来自门禁过程中读取的当前管理员行，可直接用于创建成功响应中的 `created_by`；
数据库仍以 `created_by_admin_id` 作为唯一可信关联。

行为：

| 场景 | HTTP | 响应 |
| --- | --- | --- |
| 缺少、无效或过期 token | 401 | `{ "error": "invalid token" }` 等现有认证文案 |
| token 对应管理员已不存在 | 401 | `{ "error": "admin not found" }` |
| 必须先修改密码 | 403 | `{ "error": "password change required", "code": "must_change_password" }` |
| 普通管理员访问 | 403 | `{ "error": "forbidden" }` |
| 超级管理员访问 | — | 进入业务 handler |

前端仍应隐藏普通管理员的“管理员管理”入口，但所有安全边界以后端为准。

### 7.2 目标账号授权

状态变更和重置密码都要读取并锁定目标账号：

- 目标不存在：404；
- 目标是 `super_admin`：403；
- 目标是普通 `admin`：允许继续。

不采用“只保护最后一个超级管理员”的规则，而是禁止 API 操作任何超级管理员。这比 Go 版的
“最后一个超管”提示更严格，也与 Rust 当前 seed 治理模型一致。

## 8. 接口设计

所有接口：

- 基础路径：`/api/v1/admin`；
- 认证：`Authorization: Bearer <admin_access_token>`；
- 成功响应：`application/json`，204 接口除外；
- 错误响应：`{ "error": "..." }`；
- 强制改密错误额外带 `code`；
- 请求 DTO 使用 `#[serde(deny_unknown_fields)]` 拒绝未知字段，避免调用方误以为
  `role`、`password` 或 `email` 已生效；
- handler 显式接收并转换 Axum rejection，保证输入错误统一为 400 JSON。

### 8.1 创建普通管理员

创建前先由当前超级管理员请求确认码：

```http
POST /api/v1/admin/admins/create-code
Authorization: Bearer <admin_access_token>
```

该请求无 body。服务端从认证对应的数据库记录读取超级管理员手机号，并以
`Purpose::AdminCreate` 发码；客户端不能传手机号或 purpose。成功返回 `202 Accepted`，
限流返回 429，OTP 存储或短信服务故障返回 503。

```http
POST /api/v1/admin/admins
```

请求：

```json
{
  "phone": "13800138000",
  "display_name": "运营管理员",
  "code": "123456"
}
```

请求字段：

| 字段 | 必填 | 规则 |
| --- | --- | --- |
| `phone` | 是 | 使用现有 `Phone::parse`，归一化后必须为合法手机号 |
| `display_name` | 否 | 不传时自动生成；传入时 trim 后 1–50 字符，拒绝 `<`、`>`、控制字符和 Unicode Cf |
| `code` | 是 | 当前超级管理员手机号收到的 `admin_create` 验证码 |

请求中没有：

- `role`
- `status`
- `password`
- `email`
- `created_by`

处理流程：

1. 从 `SuperAdminAuth` 取得可信的 `actor_admin_id`；
2. 校验并归一化手机号与昵称；
3. 使用数据库中当前超级管理员的手机号和 `Purpose::AdminCreate` 验证并消费确认码；
4. 使用 CSPRNG 生成 20 位临时密码；
5. 用管理员密码策略回验临时密码；
6. 在阻塞线程池中计算 bcrypt hash；
7. 插入固定 `role=admin`、`status=active`、`must_change_password=true`，
   且 `created_by_admin_id=actor_admin_id` 的账号；
8. 返回公开管理员对象、创建人和一次性临时密码；
9. 记录不含秘密的结构化事件。

成功：

```http
HTTP/1.1 201 Created
```

```json
{
  "admin": {
    "id": "019c...",
    "phone": "13800138000",
    "display_name": "运营管理员",
    "role": "admin",
    "status": "active",
    "created_by": {
      "id": "019b...",
      "display_name": "系统超级管理员"
    },
    "created_at": "2026-07-28T10:00:00Z",
    "updated_at": "2026-07-28T10:00:00Z"
  },
  "temporary_password": "g7MpQ2xV9rKe4sY8uW3n"
}
```

错误：

| 场景 | HTTP | `error` |
| --- | --- | --- |
| 请求体或字段非法 | 400 | 对应校验文案 |
| 手机号已存在 | 409 | `phone already registered` |
| 数据库或 hash 失败 | 500 | `internal error` |

唯一性不采用“先查再插”，直接依赖数据库唯一约束并把 PostgreSQL `23505` 映射为 409，
从而正确处理并发创建。

### 8.2 查询管理员列表

```http
GET /api/v1/admin/admins?role=admin&phone=1380&display_name=运营&page=1&page_size=20
```

查询参数：

| 参数 | 默认 | 规则 |
| --- | --- | --- |
| `role` | 不筛选 | `admin` 或 `super_admin` |
| `phone` | 不筛选 | trim 后对手机号做不区分大小写的包含匹配 |
| `display_name` | 不筛选 | trim 后对昵称做不区分大小写的包含匹配 |
| `page` | `1` | 必须大于等于 1 |
| `page_size` | `20` | 必须位于 1–100 |

`role`、`phone` 和 `display_name` 是相互独立的筛选条件；同时传入多个条件时按 `AND`
组合，不再提供统一搜索参数 `q`。

本设计建议非法分页参数返回 400，而不是静默改写用户输入。前端当前使用的
`10/20/50/100` 均在合法范围内。

成功：

```json
{
  "items": [
    {
      "id": "019c...",
      "phone": "13800138000",
      "display_name": "运营管理员",
      "role": "admin",
      "status": "active",
      "created_by": {
        "id": "019b...",
        "display_name": "系统超级管理员"
      },
      "created_at": "2026-07-28T10:00:00Z",
      "updated_at": "2026-07-28T10:00:00Z"
    }
  ],
  "pagination": {
    "page": 1,
    "page_size": 20,
    "total": 1,
    "total_pages": 1
  }
}
```

约束：

- `items` 永远是数组，空页返回 `[]`；
- 只查询安全字段，不读取或返回 password hash；
- 通过 self join 返回创建人 ID 和昵称，历史/seed 账号返回 `created_by: null`；
- 排序固定为 `created_at DESC, id DESC`；
- `phone` 和 `display_name` 中的 `%`、`_` 和 `\` 必须按普通字符处理，不能意外成为
  SQL 通配符；
- count 和 items 应在同一个只读一致性快照中读取，避免并发创建时出现 total 与 items
  明显不一致。

### 8.3 启用或禁用普通管理员

```http
PATCH /api/v1/admin/admins/{admin_id}/status
```

请求：

```json
{
  "status": "disabled"
}
```

`status` 仅允许：

- `active`
- `disabled`

处理规则：

- UUID 非法：400；
- 账号不存在：404；
- 目标为 `super_admin`：403；
- 目标为普通管理员：更新状态和 `updated_at`；
- 重复设置相同状态是幂等成功，返回当前账号状态；
- 禁用不立即吊销 access token；
- 禁用后新的登录和 refresh 被拒绝，现有 access token 最多继续存活一个 access TTL。

成功：

```http
HTTP/1.1 200 OK
```

响应为更新后的 `AdminAccountResponse`。

错误：

| 场景 | HTTP | `error` |
| --- | --- | --- |
| UUID 或 status 非法 | 400 | `invalid admin id` / 对应校验文案 |
| 目标不存在 | 404 | `admin not found` |
| 目标为超级管理员 | 403 | `cannot change a super admin's status` |

### 8.4 重置普通管理员密码

```http
POST /api/v1/admin/admins/{admin_id}/reset-password
```

无请求体。

处理流程：

1. 生成新的 20 位临时密码；
2. 在进入数据库事务前计算 bcrypt hash，缩短行锁持有时间；
3. 开启事务并对目标 `admins` 行执行 `SELECT ... FOR UPDATE`；
4. 目标不存在则回滚并返回 404；
5. 目标是 `super_admin` 则回滚并返回 403；
6. 吊销目标全部未吊销的 `admin_refresh_tokens`；
7. 原子更新：
   - `password_hash`
   - `must_change_password=true`
   - `failed_login_count=0`
   - `locked_until=NULL`
   - `updated_at`
8. 提交事务；
9. 返回一次性临时密码；
10. 记录不含秘密的结构化事件。

成功：

```http
HTTP/1.1 200 OK
```

```json
{
  "temporary_password": "v9Qx3mKe7R2pY6sWu4Gn"
}
```

错误：

| 场景 | HTTP | `error` |
| --- | --- | --- |
| UUID 非法 | 400 | `invalid admin id` |
| 目标不存在 | 404 | `admin not found` |
| 目标为超级管理员 | 403 | `cannot reset a super admin` |
| 事务或 hash 失败 | 500 | `internal error` |

事务保证：

- 不会出现密码已经改变但旧 refresh session 仍有效；
- 不会出现 session 已吊销但密码更新失败的部分提交；
- 同一账号并发重置由行锁串行化；
- 多次重置仍遵循正常的“后一次成功结果覆盖前一次”语义；管理端必须只转交最后一次成功响应中的
  临时密码。
- 账号原有的失败计数和临时锁定被清除，收到临时密码后可以立即登录。

access token 是无状态 JWT，本期不引入黑名单，因此重置前签发的 access token 最多继续存活一个
access TTL。其 refresh session 已被吊销，到期后不能续期。

### 8.5 统一账号行锁协议

只把 reset 内部做成事务仍不完整。当前登录流程先读取和验证 password hash，最后才进入
`AdminRefreshTokenRepository::revoke_all_and_insert` 的发 session 事务；refresh 轮换则只修改
token 表，不锁 `admins` 行，存在两类竞态：

```text
登录请求读取并验证旧 hash
        ↓
超级管理员重置密码并提交
        ↓
旧登录请求继续签发新的 refresh session
```

```text
refresh 轮换先消费旧 token 并插入尚未提交的新 token
        ↓
reset 的 UPDATE 使用看不到新行的语句快照
        ↓
轮换提交，新 token 逃过 reset 的批量吊销
```

本期建立统一锁顺序：

```text
先锁定目标 admins 行
        ↓
复核账号状态 / expected credential
        ↓
再读写 admin_refresh_tokens
        ↓
提交
```

以下操作全部遵守这一顺序：

- 登录后的 session issue；
- refresh token rotate；
- 启用/禁用；
- 重置密码；
- 自助修改密码涉及的 admin 行更新。

具体改造：

1. 登录认证成功后，把本次验证过的 `password_hash` 作为 expected credential 传给 session issue；
2. `revoke_all_and_insert` 已经会锁定 `admins` 行，应在拿锁后重新读取当前
   `password_hash` 和 `status`；
3. 当前 hash 与 expected hash 不同，或账号已 disabled，则不插入 refresh session；
4. refresh rotate 在消费旧 token 前，先在同一事务内解析 token 属主并锁定对应 `admins` 行；
5. rotate 拿锁后重新检查账号 status 和 token 可消费条件，再完成 consume-and-insert；
6. status/reset 也先取得同一行锁，再操作 token 表或提交账号变更；
7. handler 只有在 issue/rotate 成功后才返回预先生成的 access token。

这样形成确定的串行关系：

- 登录签发事务先拿到锁：它提交 session 后，reset 随后取得锁并把该 session 吊销；
- reset 先拿到锁：登录签发随后发现 hash 已变化，拒绝签发 session；
- rotate 先拿到锁：它提交的新 token 会被随后取得锁的 reset 看见并吊销；
- reset 先拿到锁：rotate 随后发现旧 token 已撤销，不能铸造新 token。

该改造不需要新增数据库列，但需要调整 `AdminSessionService::issue` 和
`AdminRefreshTokenRepository` 的 issue/rotate 事务及测试。所有路径统一先锁 `admins`、再锁
`admin_refresh_tokens`，禁止反向加锁，避免形成死锁环。

## 9. 临时密码设计

要求：

- 固定 20 个字符；
- 使用操作系统 CSPRNG：`getrandom`；
- 字符集为大小写字母和数字，排除人工转录易混淆字符：
  - `0`
  - `O`
  - `1`
  - `l`
  - `I`
- 使用拒绝采样消除取模偏差；
- 生成后调用现有 `validate_password` 和 `Password::parse` 回验；
- 最多重试 8 次，超过次数视为内部错误；
- 只存 bcrypt hash；
- 明文禁止进入：
  - 数据库；
  - `tracing` 字段；
  - panic；
  - `Debug`；
  - OpenAPI 真实示例；
  - 持久化审计。

建议将生成器放在 `accounts/service.rs` 的私有函数中，除非后续还有其它领域明确需要相同的
一次性密码能力，再上提至 `platform`。

## 10. Repository 与事务设计

`AdminRepository` 新增：

```rust
pub async fn list_accounts(
    &self,
    filter: AdminAccountListFilter,
) -> Result<AdminAccountPage, AdminRepositoryError>;

pub async fn set_plain_admin_status(
    &self,
    id: Uuid,
    status: AdminStatus,
) -> Result<AdminAccountRecord, AdminRepositoryError>;

pub async fn reset_plain_admin_password(
    &self,
    id: Uuid,
    password_hash: &str,
) -> Result<(), AdminRepositoryError>;
```

现有 `AdminRepository::create(NewAdmin)` 继续复用，不新增第二个创建 SQL；只扩展现有 INSERT，
写入 `NewAdmin.created_by_admin_id`。列表查询对 `admins` 做一次 self join，返回创建人的 ID 与昵称。

同时调整 admin session 的原子签发接口，使其在现有 `FOR UPDATE` 临界区内校验登录阶段观察到的
password hash 和账号状态：

```rust
pub async fn revoke_all_and_insert_if_current(
    &self,
    row: NewAdminRefreshToken,
    expected_password_hash: &str,
) -> Result<IssueOutcome, AdminRefreshTokenError>;
```

refresh rotate 也必须改为显式事务：

```text
BEGIN
  SELECT admin_id FROM admin_refresh_tokens WHERE token_hash = $1
  SELECT status FROM admins WHERE id = $admin_id FOR UPDATE
  -- 重新检查 status、revoked_at、rotated_at、expires_at
  UPDATE old token + INSERT new token
COMMIT
```

锁顺序在 issue、rotate、status 和 reset 中保持一致：`admins` 行永远先于
`admin_refresh_tokens` 行。

Repository error 至少需要区分：

- `NotFound`
- `AlreadyExists`
- `ImmutableSuperAdmin`
- `Db`

`set_plain_admin_status` 与 `reset_plain_admin_password` 必须在数据库边界再次保护
`role=admin`，不能只依赖 handler 或 service 的前置判断。

列表建议使用同一个只读事务读取 count 和 items：

```text
BEGIN ISOLATION LEVEL REPEATABLE READ READ ONLY
  SELECT count(*)
  SELECT safe_columns ... ORDER BY created_at DESC, id DESC LIMIT/OFFSET
COMMIT
```

密码重置事务：

```text
BEGIN
  SELECT role FROM admins WHERE id = $1 FOR UPDATE
  UPDATE admin_refresh_tokens
     SET revoked_at = NOW()
   WHERE admin_id = $1 AND revoked_at IS NULL
  UPDATE admins
     SET password_hash = $2,
         must_change_password = TRUE,
         failed_login_count = 0,
         locked_until = NULL,
         updated_at = NOW()
   WHERE id = $1 AND role = 'admin'
COMMIT
```

## 11. Service 设计

新增独立服务：

```rust
pub struct AdminAccountService {
    repository: AdminRepository,
}
```

不把这些方法继续塞进当前负责登录和 seed 的 `AdminService`。这样：

- `auth` 不依赖账号列表 DTO；
- 账号治理错误不会污染登录错误；
- service 构造器不需要继续增加 session/OTP 可选依赖；
- 单元测试能单独覆盖治理矩阵。

建议方法：

```rust
pub async fn create_regular_admin(
    &self,
    actor_admin_id: Uuid,
    phone: &str,
    display_name: &str,
) -> Result<CreatedAdmin, AdminAccountServiceError>;

pub async fn list(
    &self,
    query: AdminAccountListQuery,
) -> Result<AdminAccountPage, AdminAccountServiceError>;

pub async fn set_status(
    &self,
    target_id: Uuid,
    status: AdminStatus,
) -> Result<AdminAccountRecord, AdminAccountServiceError>;

pub async fn reset_password(
    &self,
    target_id: Uuid,
) -> Result<TemporaryPassword, AdminAccountServiceError>;
```

Service error：

```rust
pub enum AdminAccountServiceError {
    InvalidPhone(PhoneError),
    InvalidDisplayName(DisplayNameError),
    InvalidQuery(String),
    AlreadyExists,
    NotFound,
    ImmutableSuperAdmin,
    PasswordGeneration,
    Repository(AdminRepositoryError),
}
```

HTTP 文案由 handler 统一映射，service 不依赖 Axum。

## 12. 错误响应设计

调用者不是超级管理员时统一使用 `AppError::Forbidden`，固定返回 `forbidden`。以下针对目标
账号的治理错误仍需要稳定文案：

- `cannot change a super admin's status`
- `cannot reset a super admin`

建议新增不带 `code` 的可携带文案变体，例如：

```rust
ForbiddenMessage(String),
NotFoundMessage(String),
```

不要复用 `ForbiddenCode`，否则普通 403 会多出没有语义的 `code` 字段。

Axum 自带的 `JsonRejection`、`QueryRejection` 和 path 解析失败也需要在 handler 边界映射成
`AppError`，否则这些失败会绕过统一的 `{ "error": "..." }` 响应格式，并可能返回 422。

统一映射：

| 领域错误 | HTTP |
| --- | --- |
| 输入、UUID、分页参数非法 | 400 |
| token 缺失、无效、过期或主体不存在 | 401 |
| 普通管理员访问治理接口 | 403 |
| 操作超级管理员 | 403 |
| 目标不存在 | 404 |
| 手机号冲突 | 409 |
| 数据库、hash、随机源错误 | 500 |

所有 500 只向客户端返回 `internal error`，真实 cause 仅进入服务端日志。

## 13. 并发与幂等

### 13.1 创建

- 手机号唯一性由数据库约束保证；
- 并发同手机号创建只有一个成功，其余返回 409；
- 不使用 check-then-insert。

### 13.2 列表

- 稳定的二级排序避免相同 `created_at` 导致翻页重复或漏项；
- count/items 使用一致快照；
- 列表仍是时间点快照，不承诺跨多个客户端请求保持不变。

### 13.3 状态更新

- 对同一状态重复 PATCH 返回 200；
- 不因重复请求返回冲突；
- Repository 层再次限制 `role=admin`。

### 13.4 密码重置

- 随机生成和 bcrypt 在事务外完成；
- 目标行锁保证同一账号重置请求串行；
- session 吊销与密码更新同事务；
- 只有事务提交后才返回临时密码。
- 多次重置是 last-write-wins；后一次成功重置会使前一次临时密码失效，这是连续执行重置的正常语义，
  不是事务能够消除的情况。

### 13.5 登录与重置并发

- session issue 在 `admins` 行锁内比较 expected password hash；
- reset 与 issue 使用同一行作为串行化锁；
- 使用旧 hash 完成前半段认证的请求不能在 reset 提交后创建新 refresh session；
- 如果 issue 先提交，reset 会在随后同一事务中吊销它。

### 13.6 Refresh 轮换与账号变更并发

- rotate、status 和 reset 都先锁同一个 `admins` 行；
- rotate 拿锁后重新检查账号状态和 token 可消费条件；
- rotate 先提交时，后续 reset 能看见并吊销新 token；
- reset 先提交时，后续 rotate 不能消费已吊销的旧 token；
- disable 先提交时，后续 rotate 不签发新 token；
- rotate 先提交、disable 后提交时，新 refresh token 虽已存在，但后续使用会因 disabled 被拒绝；
- 全部路径采用相同锁顺序，避免 `admins` 与 token 表之间的死锁。

## 14. 安全设计

### 14.1 权限提升防护

- Create DTO 不包含 `role`；
- 数据库写入固定 `AdminRole::Admin`；
- Repository 方法不接受调用方提供的 role；
- API 不提供 promote/demote；
- `super_admin` 只能通过 seed 创建。

### 14.2 敏感字段防泄漏

- 内部 `Admin` 不序列化；
- OpenAPI 使用公开 DTO；
- tracing 只记录 actor、target、action 和非秘密结果；
- 临时密码类型不派生 `Debug`；
- 测试断言响应中不存在 hash、锁定、失败次数等字段。

### 14.3 SQL 安全

- 所有值使用 SQLx 参数绑定；
- `phone` 和 `display_name` 使用字面包含搜索，对 `ILIKE` 通配符进行完整转义；
- 排序字段固定，不接受客户端传入 SQL 列名；
- limit/offset 在进入 Repository 前完成范围校验。

## 15. 可观测性与审计

Rust 项目当前没有持久化 audit 域。本期先在成功提交后记录结构化 tracing：

```text
admin.create
admin.set_status
admin.reset_password
```

建议字段：

- `actor_admin_id`
- `target_admin_id`
- `action`
- `new_status`（仅状态变更）

禁止记录：

- 临时密码；
- password hash；
- access/refresh token；
- Authorization/Cookie header。

持久化审计表另立需求；未来接入时，密码重置的审计记录应与业务事务一起设计，不能简单把明文或
hash 放入 detail。

## 16. OpenAPI

每个 handler 添加 `#[utoipa::path]`，并在 `src/openapi.rs` 登记：

- 四条 path；
- 请求与响应 DTO；
- `AdminCreatorResponse`；
- `AdminRole`、`AdminStatus`；
- `PageMeta`；
- Bearer security；
- 201/200/400/401/403/404/409/500 响应说明。

建议新增 tag：

```text
admin-accounts — 管理后台管理员账号治理
```

完成后：

1. 运行 OpenAPI 单元测试；
2. 重新导出 `docs/openapi.json`；
3. 运行 `cargo sqlx prepare`；
4. 提交新增或变化的 `.sqlx` 缓存，保证 `SQLX_OFFLINE=true` 可构建。

## 17. 前端契约迁移

现有管理后台仍残留 Go 模型，Rust 端不建议长期兼容这些字段。

需要同步修改：

| Go/旧前端 | Rust 新契约 |
| --- | --- |
| `Admin.level` | `Admin.role` |
| `AdminListQuery.level` | `AdminListQuery.role` |
| `Admin.email` | 删除 |
| 创建管理员 email 输入 | 删除 |
| 搜索“手机号 / 邮箱 / 昵称” | “手机号 / 昵称” |
| 最后一个超级管理员才不可禁用 | 所有超级管理员均不可操作 |
| `/admins/{id}/role` | 删除 |
| Roles 页面 | 下线或隐藏 |
| 无创建人字段 | 新增 `created_by: { id, display_name } \| null` |

如果前后端必须错峰部署，可以短期只对列表查询参数接受：

```rust
#[serde(alias = "level")]
role: Option<AdminRole>
```

响应仍建议只返回 `role`，并要求前端与后端在同一个发布窗口切换。长期同时返回 `level` 和
`role` 会让契约继续分裂，不建议采用。

## 18. 测试策略

### 18.1 纯单元测试

- 临时密码：
  - 长度恒为 20；
  - 字符集合法；
  - 不含易混淆字符；
  - 多次生成结果不同；
  - 能通过现有管理员密码策略；
- 分页参数校验；
- service error 到 HTTP error 映射；
- `SuperAdminAuth` 角色判断。

### 18.2 Repository 集成测试

- schema：`created_by_admin_id` 可空、自引用 FK 生效、删除创建人受 RESTRICT 保护；
- seed 创建账号的 `created_by_admin_id` 为 NULL；
- API 创建账号正确写入 actor ID；
- 历史/seed 账号列表返回 `created_by: null`；
- API 创建账号列表返回创建人 ID 和昵称；
- role 筛选；
- phone 和 display_name 分别搜索手机号和昵称，组合查询使用 AND 语义；
- `%`、`_`、`\` 按字面匹配；
- 空列表为 `[]` 且 total 正确；
- 超出最后一页时 items 为空但 total 保持正确；
- 相同创建时间下仍按 id 稳定排序；
- 并发同手机号创建只有一个成功；
- 状态更新成功、幂等、404；
- Repository 拒绝更新超级管理员；
- 密码重置事务同时更新 hash/flag 并吊销所有 refresh session；
- 事务失败时两类数据都不部分提交；
- 并发重置串行化；
- 旧密码登录与 reset 并发时，不会在 reset 之后留下新的有效 refresh session；
- session issue 发现 expected hash 已变化时不插入任何 refresh token；
- refresh rotate 与 reset 并发时，不会有新 refresh token 逃过吊销；
- refresh rotate 与 disable 并发时，在账号行锁内复核 status；

### 18.3 Handler 端到端测试

四个接口都覆盖：

- 无 token：401；
- web token：401；
- 普通管理员 token：403 `forbidden`；
- must-change：403 + `code=must_change_password`；
- super_admin happy path。

额外覆盖：

- 创建固定得到 `role=admin`；
- 伪造 `role=super_admin` 不能生效；
- 请求体伪造 `created_by` 被 400 拒绝；
- 创建人的 ID 必须来自当前 `SuperAdminAuth`；
- 创建响应只返回一次临时密码；
- 重置后旧 refresh token 无法使用；
- 重置与旧密码登录并发时，旧登录不能在重置后得到可续期 session；
- 重置与 refresh 轮换并发时，轮换出的新 token 不会逃过吊销；
- super_admin 目标的 status/reset 都返回 403；
- UUID、JSON、status、分页非法值返回统一错误形状；
- 所有公开响应不泄露敏感字段；
- OpenAPI 包含四个 path 和 security。

### 18.4 回归验证

```bash
cargo fmt --all -- --check
cargo check --all-targets
cargo test --all-targets
cargo clippy --all-targets -- -D warnings

review_target="$(mktemp -d)"
SQLX_OFFLINE=true CARGO_TARGET_DIR="$review_target" cargo check --all-targets
```

## 19. 发布与回滚

本期包含一条可向后兼容的 nullable 自引用外键迁移。迁移不会重写既有创建人数据：
历史账号保持 `created_by_admin_id=NULL`。发布风险主要来自新路由、前端契约切换和 migration
执行顺序。

推荐顺序：

1. 先部署 `created_by_admin_id` migration；
2. 合并 Rust 端模块、测试、OpenAPI 和 `.sqlx` 缓存；
3. 同步合并前端 `level → role`、创建人展示及 email/RBAC 残留清理；
4. 在测试环境运行 seed，准备至少一个可登录的 `super_admin`；
5. 冒烟验证创建人、创建、列表、禁用、启用、重置和强制改密闭环；
6. 前后端同一发布窗口上线；
7. 观察 401/403/409/500 和三个管理事件。

应用版本可独立回滚；旧版本会忽略新增 nullable 列。数据库 down migration 只有在确认没有新版本实例、
且不再需要创建人数据时才能执行，否则会永久丢失已经记录的来源关系。

## 20. 验收标准

- 普通管理员无法调用四个接口；
- 超级管理员可创建普通管理员，不能通过请求创建超级管理员；
- 创建人的 ID 由认证上下文写入，调用方不能伪造；
- 列表和创建响应能展示创建人；seed/历史账号明确返回 `created_by: null`；
- 创建后的管理员能使用临时密码登录，并被要求修改密码；
- 列表支持 role、关键字和分页，且不泄露敏感字段；
- 超级管理员不能操作任何超级管理员账号；
- 普通管理员可被幂等启用或禁用；
- 重置密码后旧 refresh session 全部失效；
- 与重置并发的旧密码登录不会在重置后留下新 refresh session；
- 与重置并发的 refresh 轮换不会产生逃过吊销的新 refresh session；
- 密码重置不存在部分提交；连续多次成功重置明确采用 last-write-wins；
- OpenAPI 与前端类型使用 `role`，不再使用 `level`；
- 干净环境下 `SQLX_OFFLINE=true` 编译成功；
- 全量测试与 Clippy 通过。

## 21. 待评审决策

以下事项需要在实现前确认：

1. **分页非法值**：返回 400，还是像 Go 一样静默 clamp？
   - 本设计建议：返回 400，契约更明确。
2. **重置密码是否清除临时锁定状态**：
   - 方案 A：只改密码和强制改密标志，保留 `failed_login_count/locked_until`；
   - 方案 B：同时清零失败次数和锁定状态，让管理员可立即使用临时密码登录；
   - 正文按本设计建议的方案 B 编写；如评审选择 A，需要同步删除 reset SQL 与测试中的清锁逻辑。
3. **前后端发布方式**：是否允许同版本发布并一次性完成 `level → role`？
   - 本设计建议：同版本发布，不维护双字段。
4. **审计持久化**：本期继续按现有 Rust 决策只做结构化 tracing，还是同步建设 audit 表？
   - 本设计建议：本期 tracing，audit 独立立项。
