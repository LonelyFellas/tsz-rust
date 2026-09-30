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
- 用户资料读取为 `GET /api/v1/auth/me`，不是旧对接清单中的 `/api/v1/me`。
- access token 通过 Bearer header 发送；刷新会话使用 HttpOnly cookie。cookie 的路径、刷新轮换和安全边界见[Token 设计](auth-token-design.md)、[会话刷新设计](session-refresh-design.md)，实际实现以两个身份域的 handler 为准。
- 前端鉴权逻辑集中在 `@tsz/shared/auth`。授权失败与会话过期应按 HTTP 状态及稳定 `code` 区分，不匹配 `detail` 文案，不对所有 403 一律刷新。
- 生产环境必须使用 HTTPS 与 Secure cookie。临时 HTTP 测试不能成为生产关闭 `COOKIE_SECURE` 的理由。

## 词库 V3

词条读写使用 `schema_version=3` 与原生 V3 结构；新代码不采用历史 V2 `base_form + slots` 模型或旧向导。发布、归档、恢复、回退、共享例句与节点引用约束见[词库模型](word-data-model.md)及 OpenAPI。

能力不仅取决于端点是否存在，还取决于权限、配置、当前实体状态与响应中的 `capabilities`。不得从旧文档中某次“已上线”或“本地候选”的描述推断当前环境状态。

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
