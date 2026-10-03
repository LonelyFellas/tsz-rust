# 昵称保存设计评估

状态：仅评估，未实现新端点、未执行迁移或数据库写入。范围和规则见 [requirements.md](requirements.md)。后端基线为最新远程 main `4b7df469fdd716c37426a5a2bb998e66c6f57ea3`。

## 1. 最小方案与依据

- 在现有 [user handler 模块](../../../src/user/handler.rs) 添加本人资料更新，不扩展管理员接口或套用后台 users.edit 权限。
- [DisplayName::parse](../../../src/user/model.rs) 已负责 trim、1–50 Unicode 码点及禁字符；直接复用，不再定义另一份 Rust 规则。
- [AuthUser](../../../src/auth/extract.rs) 已验证 Web token、active 状态与 security_version；写 SQL 再核对状态/版本，避免鉴权后发生禁用、注销或改密竞态。
- [UserRepository](../../../src/user/repository.rs) 已有用户回读，没有本人昵称更新；借鉴 [管理员局部更新](../../../src/admin/accounts/repository.rs) 的 UPDATE/RETURNING 做法，不调用管理员 service。
- [用户资料装配](../../../src/auth/handler.rs) 的 UserProfile/load_user_profile 可复用，GET /auth/me 的扁平 wire 形状不变。

本期只新增必要 DTO、路由和局部写，不新增媒体服务、审计历史、改名额度、版本请求参数或可修改联系方式的通用 PATCH 模型。

## 2. 接口与错误

`PATCH /api/v1/me`，Bearer Web token，200 返回 `{ "user": UserProfile }`。

请求 DTO：`UpdateProfileRequest { display_name: String }`，建议 deny_unknown_fields。字段必填且非 null；不能接收 id、phone、email、avatar_url、roles、active_role 等越权输入，未来扩展需另评估。

UserProfile 保持现有字段：id、display_name、avatar_url、roles、active_role；phone/email 缺失时省略，不新增 null、不返回密码、安全版本或内部 User 全对象。

| 状态 / code                                  | 场景                                                                 |
| -------------------------------------------- | -------------------------------------------------------------------- |
| 200                                          | 合法修改或重复同昵称，返回规范化后的资料                             |
| 400 invalid_display_name，field=display_name | DisplayName 校验失败，沿用当前错误类型                               |
| 400 invalid_json / 422 invalid_request_body  | JSON 语法、缺字段、null、错误类型或未知字段，复用 ApiJson            |
| 401 invalid_token                            | 未登录、错误 realm、旧安全版本或用户无效；写入前状态变化也不能继续写 |
| 413 payload_too_large                        | 建议限制本小型 JSON 路由为 2 KiB                                     |
| 500 internal_error                           | 仓储/资料装配失败，仅固定安全错误体，不泄露内部 cause                |

DisplayNameError 当前 detail 为 `display name cannot be empty`、`display name cannot be longer than 50 characters`、`display name contains forbidden characters`；旧前端英文键并不全部匹配。前端优先处理 code=invalid_display_name，不能靠“新接口返回旧 Go 英文”来兼容。

## 3. 写入、响应与并发

建议签名：`user::handler::update_profile(State<AppState>, AuthUser, ApiJson<UpdateProfileRequest>)`；`UserService::update_display_name(subject, security_version, DisplayName)`；`UserRepository::update_display_name(subject, security_version, &str) -> Result<Option<User>, UserError>`。

只做参数化局部 UPDATE，示意如下，实施时按现有 User 字段写明确 RETURNING 清单：

```sql
UPDATE users
SET display_name = $2, updated_at = NOW()
WHERE id = $1 AND status = 'active' AND security_version = $3
RETURNING id, phone, email, password_hash, security_version,
          display_name, last_active_role, status,
          avatar_url, created_at, updated_at;
```

- 身份 id/version 来自 AuthUser，不来自 JSON。无匹配行统一映射 invalid_token，不泄露用户是否被删或禁用。
- RETURNING 的 User 只是内部领域对象；响应经过公开 UserProfile 装配，不能直接 Json(User) 或完整调试输出。
- 不增加不必要的长事务；UPDATE 自身取得用户行锁并复核谓词，不在 SQL 前读老 User 再把整份对象保存回来。需要组合事务时遵守现有用户锁顺序。
- 本期不写 avatar_url 或头像引用字段，故与头像事务并发不会在数据库互相覆盖；管理员昵称更新与本人更新对同一字段后写生效，头像确认也必须坚持只写其字段。
- 从 UPDATE RETURNING 取得昵称和头像快照，随后复用资料装配读取角色。必要时把 `load_user_profile` 设为 pub(crate)，与头像任务共用同一装配点，避免两支各复制 DTO/回包构造。
- 资料装配可能在写入提交后失败，或成功响应在网络中丢失：返回 500/客户端失败不等于数据库必定没写。相同请求安全重试恢复完整资料，测试不得把重试当成一次新安全操作。
- 同页前端现有头像/昵称互斥保持；跨页签、设备的完整 User 响应仍可能暂时陈旧。本期不引入跨设备同步或新版响应版本号，以 GET /auth/me 恢复当前资料，不宣称所有快照实时一致。

## 4. 数据、回退与改动文件

**无需数据库迁移**：users.display_name 是已有 TEXT NOT NULL，未设置昵称唯一约束；不回填、不清洗历史昵称，不增加 up/down 空迁移。若使用新增 query!，需核实 .sqlx 元数据；采用与现有回读相同的 query_as 方式则不能为“刷新缓存”改无关查询。

主要后端文件：`src/user/{handler,service,repository}.rs`、`src/lib.rs`（明确挂载 PATCH /api/v1/me，避免 trailing slash 误配）、`src/auth/handler.rs`（公开装配边界）、`src/openapi.rs`、`docs/openapi.json`、相关测试及前后端对接说明。优先复用已有 ErrorCode::InvalidDisplayName，不新增重复错误枚举。

代码回退不会删除已保存昵称，旧读取接口仍认识该列；新 PATCH 回退后重新变为 404，旧前端会降级提示。恢复昵称历史不属于自动代码回退，没有历史值就不能假装能够恢复。

## 5. 前端与头像任务边界

- 保持 `updateProfile(display_name)` 请求和 `{ user }` 成功回包，保留保存成功后 store/local me 更新及取消行为。
- 在 @tsz/shared 提供/复用规范化与码点长度逻辑，C 端预检、计数和提交对齐 Rust；不要用原生 UTF-16 maxLength 截断合法 emoji。超限显示明确提示并禁提交，不悄悄截断昵称。
- trim 差异需覆盖 BOM 与 U+0085；建议按 Unicode White_Space 对齐 Rust trim，保留剩余 Cc/Cf 检验顺序，不对昵称做 NFC 等新处理。普通组合字形不是本期计数单位。
- 新增 invalid_display_name 的 code 文案映射；原有精确禁字符预检保留。401 继续走现有鉴权内核，不在页面另建刷新/会话策略。
- 姓名接口实现后仅移除 `patch /me` 的 PENDING，头像、学习设置等项按其实际实现状态保留；User 成功回包不能因当前缺少 strict validator 就省字段或增加内部字段。
- 前端实施配套使用独立昵称任务 worktree，已有该任务则复用，不在当前未提交 UI 样式工作区混入。头像实施分支保持独立；对共享资料装配、users 更新及 OpenAPI 合并冲突做最终核对，不擅自扩大任何一支 scope。

## 6. 契约与发布顺序

按后端 contract-sync 技能导出 OpenAPI，前端显式使用 OPENAPI_SOURCE 指向本任务候选后端；本次评估不运行生成器、不改快照。

- 当前前端 + 候选 API：原请求/完整 user 成功回包应兼容；非法昵称新 detail 的中文提示可能需前端 code 映射，不能把尚未验证的错误分支写成“完全兼容”。
- 候选前端 + 当前 API：仍然 404，保留已有未开放降级，不崩溃或自动改动用户状态。
- 两端候选：验证昵称成功、错误中文、码点边界、刷新一致与头像局部写。
- 回退 API + 已写昵称：无需 schema 回退，读取正常，修改入口不可用；前端应明确失败而不是显示已保存。

推荐先发布通过成功/失败混合版本验证的 additive 后端，再发布前端契约、校验和错误映射；若过渡错误文案不满足产品要求，先补兼容映射并验证后再启用。提交、推送、合并和部署均不在本次授权内。

## 7. 验收

**本次已运行**：Rust 1.98.0、SQLX_OFFLINE=true，`cargo test --locked --lib display_name_tests` 13 个现有昵称校验/生成测试通过；没有新 handler、数据库写入或真实昵称保存验收。

以下命令供新增实现和测试目标完成后，在对应后端 worktree 根目录执行。数据库测试使用独立 PostgreSQL 且允许 sqlx 创建测试库，绝不指向生产或当前 UI 工作区数据库；并发、更新和删除必须真库测试，不能把仓储替换成 fake。

```bash
SQLX_OFFLINE=true cargo test --locked --test user_profile_handler
```

预期：合法本人修改、完整回包、同名重试、校验边界、未知字段、Web/Admin/匿名/失效 token、写入前禁用/改密/删除竞态，以及提交后响应失败的安全重试覆盖。

```bash
SQLX_OFFLINE=true cargo test --locked --test user_repository
```

预期：局部更新只改昵称/updated_at，头像/联系方式/角色/密码/版本保持；用锁和同步信号验证昵称与头像、管理员昵称写、硬删除的顺序，不用 sleep 制造竞态。

```bash
SQLX_OFFLINE=true cargo test --locked --test auth_me
```

预期：GET /auth/me 形状不变，保存后的昵称及其他资料正常回读。

```bash
cargo fmt --all -- --check
```

```bash
SQLX_OFFLINE=true cargo clippy --locked --all-targets --all-features -- -D warnings
```

前端在实施配套 worktree 执行：@tsz/api-client 测试、EditProfileForm 与共享校验定向测试、全仓原生检查；同步脚本实际支持 `OPENAPI_SOURCE` 环境变量，路径必须是候选后端 docs/openapi.json，不使用默猜后端路径。新 Rust 测试 target 需核验 CI 模块分组。

**浏览器真实验收**：本人保存普通、中文、普通 emoji 昵称，确认“已保存”、账户菜单更新、刷新和重新登录一致；服务器拒绝时旧资料保留且中文错误准确；昵称/头像并发不覆盖彼此。对前述四种版本组合分别记录证据。当前接口不存在，本次均未验证，不能用前端 mock 测试代替。
