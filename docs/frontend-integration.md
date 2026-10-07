# 前后端对接指南

本文说明当前仓库的对接入口与兼容边界，不记录线上部署结论。运行环境是否具备某项能力，须核对该环境的部署版本、配置与实际验收结果。

## 契约来源

- [openapi.json](openapi.json)：请求、响应、枚举、字段可空性及状态码的权威定义，不在本文再维护一份 TypeScript 类型或完整接口表。
- [api-errors.md](api-errors.md)：RFC 9457 错误格式与机器错误码；前端不再兼容旧 `{error: ...}`。
- [词库模型](word-data-model.md)、[用户域](user-domain-reference.md)：按业务主题查实现与数据约束。
- [前端契约同步技能](https://github.com/LonelyFellas/tsz/blob/main/.agents/skills/contract-sync/SKILL.md)：同步与消费者验证入口。

前端 `packages/types` 的 wire 字段保持 `snake_case`。`packages/api-client` 的快照来自后端 OpenAPI，不以旧 Go 文档或历史前端类型反向决定新接口。

## 环境与同步

后端本地端口由 `PORT` 配置，默认 `8383`；API 前缀为 `/api/v1`。前端代理默认连接本地后端，可用 `BACKEND_API_URL` 指向本任务后端。不要将主仓库服务、其他 worktree 服务与当前契约混用。

前端同步脚本 `packages/api-client/scripts/sync-openapi.mjs` 支持 `OPENAPI_SOURCE`。在前端任务仓库根目录执行前，先确认它指向配套后端：

```bash
OPENAPI_SOURCE=/absolute/path/to/backend-worktree/docs/openapi.json pnpm --filter @tsz/api-client sync:openapi
```

该命令会更新前端生成文件；同步后必须检查差异并执行项目规定的契约验证，不能把同步成功等同于联调完成。

## 身份与错误处理

- Web 用户与管理端是不同身份域，分别使用 `/auth/*` 与 `/admin/auth/*`；不能混用 access token、refresh cookie 或登录状态。
- Web 业务接口要求账号已绑定手机号，否则返回 `403 phone_binding_required`。无手机号账号仍可登录/注册、恢复和读取会话、绑定联系方式、修改密码、退出或注销；前端应在业务与新用户引导前补绑。
- 手机号只能换绑，发码和最终解绑都会拒绝解绑手机号（`403 phone_unbind_forbidden`）；邮箱解绑仍受至少保留一种联系方式约束。绑定/换绑继续返回 204、撤销全部会话，随后可通过 `/auth/login-otp` 使用手机号验证码登录。
- 配套需求与设计集中在前端仓库 `docs/features/require-phone-binding/`。发布顺序为先前端补绑入口，再后端业务限制；真实短信发送仍需接入供应商。
- `GET /api/v1/me` 返回用户档案包壳与真实学习配置；`GET /api/v1/auth/me` 保留扁平 `UserProfile`，兼容既有消费者。
- 本人昵称保存为 `PATCH /api/v1/me`，仅接收 `display_name`，返回 `{ user: UserProfile }`。昵称按 Rust trim 后的 Unicode 码点计数（1–50），拒绝剩余 Cc/Cf 和 `< >`，不变更安全版本或会话。前端按 `invalid_display_name` 识别校验错误，并对齐 BOM/U+0085 的空白边界。
- 昵称写入后资料装配或网络响应仍可能失败；相同昵称可安全重试，以重新读取确认当前值。无需数据库迁移；推荐先发布 additive 后端，再发布配套前端。后端回退后新保存接口恢复 404，但已写昵称仍可读取，前端必须提示未保存。
- access token 通过 Bearer header 发送；刷新会话使用 HttpOnly cookie。cookie 的路径、刷新轮换和安全边界见[Token 设计](auth-token-design.md)、[会话刷新设计](session-refresh-design.md)，实际实现以两个身份域的 handler 为准。
- 前端鉴权逻辑集中在 `@tsz/shared/auth`。授权失败与会话过期应按 HTTP 状态及稳定 `code` 区分，不匹配 `detail` 文案，不对所有 403 一律刷新。
- 生产环境必须使用 HTTPS 与 Secure cookie。临时 HTTP 测试不能成为生产关闭 `COOKIE_SECURE` 的理由。

## 个人学习配置

`GET /api/v1/me` 返回 `{ user, active_role, learning_settings, onboarded }`。学生尚未明确选择时配置为 `null`、`onboarded=false`，包括没有学生资料行的已有账号；不回填默认级别。仅有教师身份的账号不要求个人学习引导。

`PUT /api/v1/me/learning-settings` 成对提交 `cefr_level`（A1–C2）和 `english_variant`（BrE/AmE）。首次保存创建或更新学生资料并固定难度；后续只允许相同难度下切换英美偏好。同载荷可重试，修改难度返回 `409 cefr_level_locked` 且两项均不变；没有学生身份返回 403。

前端用真实读取结果驱动登录、会话恢复与引导。已完成账号不能通过引导页、测评结果或查询参数重新定级；个人资料页只读难度、可修改英美偏好。无数据库迁移，先发布后端再切换前端读取 `/me`；旧 `/auth/me` 保留兼容，新前端不能先部署到没有 GET `/me` 的旧后端。配置持久化不代表尚未提供的学习内容下发接口已经接入。

## 词表阅读（WL-03）

公开及本人词表的 `/{id}/items` 保持默认标准响应，显式 `view=full` 才返回词形、词形对应义项、地区拼写、字典音标及配置展示名。`sort=author|label_asc|label_desc` 在分页前排序，仅改变阅读顺序，不写作者编排。新响应仅来自当前可用 V3 发布内容；公开响应不包含作者私密备注。

旧 Web/Admin 的严格 runtime 可继续接受候选后端的默认标准/审核响应；不能向旧消费者无条件发送完整字段。发布顺序为后端 → Web，Admin 默认读取不变。新 Web 连接旧 API 时标准默认请求仍可读，完整/新增排序参数会被拒绝，页面明确提示失败并允许切回默认；不能前端先开放这些能力。本批无迁移，回退 Web 不改变数据，仍须保留已发布的 coins/注销 schema 与安全边界。需求和验收集中在前端 `docs/features/coins-system/wordlist-foundation-design.md` 与 `wordlists-tips-acceptance.md`。

## 学生学习任务（LT-01～LT-04）

`/me/learning-tasks` 与 `/me/learning-runs` 提供学生自建每日/长期任务、出题预览、固定轮次、首次有效作答和历史。题型为中文释义拼写英文，必须具备学生资格、已绑定手机号和真实学习设置。题目与判定由服务端固定；全部有效作答即完成，正确率独立统计，不发放 coins。

每日北京时间 04:00 换日，整个任务 ends_at 可缩短当前轮次；长期轮次完成后通过 after_run_id 明确再开。创建、开始及首答请求须保留幂等键，未知结果不能换键重答。普通发布/词表重命名或追加不改本轮答案；来源撤回、固定成员移除或归档代际变化使未完成轮失效，历史内容按当前权限裁剪。

发布顺序为后端 → Web，Admin 无功能变更。已有接口响应保持不变；旧 API 对学习路由返回 404，新 Web 明确提示尚未就绪。迁移 `20261007010000` 新增六张学习表，有学习事实时 down 拒绝执行；旧二进制的严格 SQLx 校验也会拒绝新 schema。回退时保留数据和当前后端，或另制兼容回退构建，不能直接删表换旧二进制。配套需求与隔离验收证据集中在前端 `docs/features/coins-system/learning-task-foundation-design.md` 第 10 节。

## 词库 V3

词条读写使用 `schema_version=3` 与原生 V3 结构；新代码不采用历史 V2 `base_form + slots` 模型或旧向导。发布、归档、恢复、回退、共享例句与节点引用约束见[词库模型](word-data-model.md)及 OpenAPI。

能力不仅取决于端点是否存在，还取决于权限、配置、当前实体状态与响应中的 `capabilities`。不得从旧文档中某次“已上线”或“本地候选”的描述推断当前环境状态。

### 每条发音的英美合成候选

`synthesis.uk`、`synthesis.us` 分别保存英美 IPA、UPS 与可选逐词边界；`alphabet`、`use_spelling` 仍是一组共用来源选择。词形的地区或音标通用规则不限制这两套候选。

旧顶层合成字段继续读取，按已记录的 locale 或明确的词形地区保留归属；通用词形的未确认旧音素不能自动借给任一口音。无数据库迁移，历史发布快照不改写。

发布顺序：先部署能读取新契约的前端，保持 `VITE_AZURE_PRONUNCIATION_INPUTS=false`；再替换后端，确认并刷新旧管理端页面后启用新录入。新前端可以读取旧响应，但旧后端会拒绝双口音写入；旧前端能继续读未新增字段的记录，但严格 runtime schema 会拒绝已保存的双口音记录。

保存双口音记录后，不支持直接回退到不能读取新字段的旧前端或旧后端。回退时先停止新录入，保留兼容读取版本；任何历史数据转换须另行验证，不通过清空候选来回退。

### 关联候选仅查询发布内容

当前以下候选接口仅返回未归档词条的当前发布内容，不返回从未发布词条或发布后新增的草稿节点：

- `GET /api/v1/admin/lexicon/entries/related-search`
- `POST /api/v1/admin/lexicon/entries/component-targets/search`

`include_drafts` 参数已移除，不能继续发送，即使值为 `false`：GET 拒绝为 400，POST 拒绝为 422 `invalid_request_body`。依据见 `tests/lexicon_handler.rs` 的 `candidate_searches_keep_published_senses_and_reject_removed_draft_parameter`。

这不删除已有草稿引用，也不改变草稿保存和既有发布校验；已知目标 ID 的只读回显不依赖候选搜索。配套更新时，先更新不再发送该参数的前端并刷新旧页面，再替换后端；旧分页游标应重新从首页查询。

### 同原形词条创建与标注

同名空草稿在列表中可查询，也参与数字标注分组；`existing_draft_id` 只提供打开入口，不阻止创建独立词条。
创建请求删除 `homograph_reason`，不再校验或写入区分说明；历史审计不变。
`MatchedEntryContextV3.created_by_name` 为必填创建人姓名，标注冲突与匹配页面共用该结构。
前端须从本次 OpenAPI 同步严格 runtime schema。检测缓存使用 `lexicon:detection:v3:`，匹配快照使用 `lexicon:surface-snapshot:v3:`；旧在途检测与快照失效，重新检测即可。
历史检测快照的主词回填只读取主词推导字段，不解析匹配页面上下文。

旧前端会拒绝新增响应字段或发送已删除请求字段；新前端也不能消费旧后端缺少姓名的冲突上下文，且旧后端仍有同名空草稿阻断。
两种混合版本都不能完成本次创建流程，不能在线任意滚动发布。后续发布须在暂停词库创编的维护窗口切换配套前后端并刷新页面，再验收恢复；单端回退同样不受支持。
本次无数据库迁移，不修改登录或会话协议。

### 专题入口

- [词性配置](part-of-speech-config-design.md)、[对象存储](object-storage-design.md)、[IPA/UPS](ipa-ups-integration.md)。
- [词库领域重构记录](features/lexicon-domain-refactor/)保留各批次决策与验收背景，不作为实时部署状态。
- [功能设计目录](features/)记录需求和演进过程，不应覆盖当前 schema、路由或权限检查。

## 联调验收

对齐配套提交和生成契约，核实登录身份与权限，再验证完整操作链、错误分支、并发修订号以及旧前端/新后端的中间版本组合。mock、类型检查和某一端的单测通过不能替代真实跨端验收；未部署、未验收应明确说明。

## 权限标签颜色

`PermissionTag.color` 在目录、标签列表、创建结果和成员变更结果中返回，已有标签由迁移赋予 `default`。创建及 PATCH 请求可省略 `color`，保留旧管理端的写入行为；新值只接受预设色或不透明 `#RRGGBB`，HEX 按大写存储。修改颜色使用现有 `expected_version` 冲突控制，不改变标签成员或管理员授权。

配套管理端先同步本仓 OpenAPI，再按后端 → Admin 顺序发布。旧管理端可以读取新增字段并继续发送无颜色的请求；新管理端若先连接旧后端，携带 `color` 的写入会因严格请求 DTO 被拒绝。旧后端二进制能忽略已新增的颜色列；回退自定义色约束会将现有 HEX 颜色改为 `default`，不应当作无损回退。

## 2026-09-28：第五批教师认证与身份

本批是同账号新增教师身份，学生身份与既有数据保留，不创建第二个账号。浏览器按用户 ID 保存工作台偏好，不修改 JWT、refresh 或用户全局 `last_active_role`；该偏好不授予任何权限。撤销以数据库资格为准，不等 access token 过期。前端在重新核验、聚焦或进入教师路由时发现撤销后回到学生工作台；离线页面不保证即时变化。

### 接口与权限

以下路径均以 `/api/v1` 为前缀，字段以本任务 `docs/openapi.json` 为准：

- `GET /me/teacher-certification`：本人资格、最新申请与材料元数据。
- `POST /me/teacher-certification/files?kind=...`：鉴权二进制上传，实际 `Content-Type` 为 JPEG/PNG/WebP 图片 MIME；每张最多 10 MiB，解码尺寸不超过 8192×8192。
- `GET|DELETE /me/teacher-certification/files/{id}`：本人读取／删除未提交材料；已提交材料不能删除。返回元数据不含对象 key 或公开 URL。
- `POST /me/teacher-certification/applications`：姓名、联系方式、说明、身份证正反面、学历和语言证明全部必填；后两类各 1–10 张。待审／已认证时拒绝重复提交（409）。
- `GET /me/teacher-certification/applications/{id}`：本人历史申请，不用最新申请代替通知关联的旧申请。
- `GET /me/notifications`、`PATCH /me/notifications/{id}/read`：本人分页通知与幂等标已读。
- `GET /admin/teacher-applications`、`GET /admin/teacher-applications/{id}`、`GET /admin/teacher-certification/files/{id}`：仅超管；列表支持 page/page_size/status，材料必须已关联申请。
- `POST /admin/teacher-applications/{id}/review`：超管 approve/reject，驳回必须填写原因。
- `DELETE /admin/users/{id}/teacher-certification`：超管撤销，必须填写原因；兼容没有申请记录的历史已认证用户。

普通管理员只从用户列表／详情查看认证状态，以上管理接口均拒绝其访问。审核、资格变动、角色成员关系及站内通知同事务提交；重复审核返回 409，不重复发通知。驳回、撤销允许重新申请，保留历次记录。材料响应为 `Cache-Control: no-store`，前端使用 Blob URL，切换账号或卸载时释放。

### 私有存储与回退边界

配置空间名为 `teacher-certification`，完整配置示例见 `.env.example`。未配置或配置为 public 时材料服务拒绝操作，不降级为公开存储。`PRIVACY=private` 只是应用策略，**不等于 OSS bucket ACL 已验收**：上线前必须核对独立私有 bucket／root、最小 RAM 权限、匿名读取拒绝、服务重启后的材料持久可读。勿复用公共图片 CDN。内存适配器测试不能证明这些外部条件。

未提交材料 24 小时过期；每分钟回收无申请引用的过期、待删及注销账号材料。正在上传的材料保留记录，直到上传结束或过期，删除失败保留 `delete_pending` 供后续重试。真实证件只用于获授权的环境，本地验收使用明确标记的合成测试图片。

先发布新增迁移和后端接口，再发布新前端；新前端不支持旧后端缺失的认证接口。旧前端原占位申请接口仍不可用，发布间隙不承诺申请入口可用；原登录／刷新协议未变。迁移 down 在已有认证申请、材料或通知时明确拒绝，防止删除存储追踪记录；此时回退应用但保留新增表，不能强制清空认证数据来回退。

## 2026-10-06：B1b 注销申请与连续 72 小时等待

- `GET /api/v1/me/account-deletion` 返回最新申请（无申请为 `null`）、当前十进制字符串余额、声明版本/正文和服务器时间。
- `POST /api/v1/me/account-deletion` 使用本人渠道验证码、`expected_coin_balance`、主动的 `confirm_deletion`/`waive_balance`、`consent_version` 与 UUID `idempotency_key`，返回 202 及持久申请。相同意图重试返回原申请，不重新消费 OTP，不重置截止时间；不要自动 refresh 后重放验证码错误。
- `POST /api/v1/me/account-deletion/{id}/cancel` 仅在服务端截止前撤销本人申请，恢复原余额。申请成功不清会话；登录不会自动撤销。待注销钱包拒绝所有收支，零余额也等待连续 72 小时。
- 旧 `DELETE /api/v1/auth/account` 固定拒绝为 409 `account_deletion_upgrade_required`（无效会话为 401），不消费验证码、不删号、没有 204 成功分支。
- 到期以数据库锁后 `clock_timestamp()` 判定；登录、refresh、旧 token 与敏感写入立即拒绝。持久 worker 每 30 秒扫描，忙碌/失败申请持久退避 60 秒；物理清理可稍晚，不能显示“所有数据已删除”。
- 金额或声明内容在失败恢复查询中发生变化，也必须清除旧勾选并重新签署；不能只依赖特定错误码。新前端访问旧 API 的 404 显示不可用，绝不回退旧 DELETE。
- 推荐先发布不回退旧删除路径的新前端，再发布 B1b 后端。过渡期新流程可能暂不可用；旧客户端在新后端也不能绕过等待。一旦保存任意申请/签署记录（含已撤销、零余额），不得回退到忽略等待期的旧后端，down 会拒绝删除证据。
- B1a/B1b 尚未接入真实人工入账；B2 才增加业务权限、审计、人工入账/冲正与钱包页面。具体本地验收见前端任务目录 `docs/features/coins-system/b1b-acceptance.md`。

## 2026-10-06：手机号基线上的天生币与词表整包

本批同时引入币账本/双端钱包、72 小时注销、人工入账与冲正、邀请归因、词表审核公开和投币；保留 main 的手机号绑定与验证码登录。未绑定账号的业务接口返回 `403 phone_binding_required`，`/me`、联系方式安全及新注销查询/申请/撤销继续使用 SessionUser，注销逻辑到期后统一 401。公开词表读取不要求认证。`MeResponse.learning_settings` 保持必填且可 null。

整包发布顺序为后端 → Web → Admin，覆盖上节针对单独 B1b 的历史顺序建议。新邀请表单先于后端发布会被旧注册接口忽略归因，因此不能先开放新前端。后端先发布后，旧客户端的立即注销请求明确返回升级错误，不消费 OTP、不删号；用户须刷新到新 Web 使用 72 小时流程，不能承诺旧注销入口无缝可用。

目标迁移区间为 `20261002000000 → 20261006050000`（六组迁移）。已在隔离库验证空业务数据时整段可撤回，出现新增币账后整段回退拒绝且不留下部分 down。真实部署仍须重新核对目标版本、数据守卫、精确 main/CI 和制品；有签署、账目、邀请或词表内容后不得强制 down。邀请奖励默认关闭，生产数量未配置时不发币。
