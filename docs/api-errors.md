# API Error Contract

所有普通 API 4xx/5xx 错误使用 RFC 9457 Problem Details，并返回：

```http
Content-Type: application/problem+json
```

```json
{
  "type": "urn:tsz:problem:invalid_phone",
  "title": "Invalid phone",
  "status": 400,
  "detail": "invalid phone",
  "code": "invalid_phone",
  "field": "phone"
}
```

- `type` 是稳定、跨环境的问题类型 URI，与 `code` 一一对应；命名空间为 `urn:tsz:problem:<code>`。
- `title` 是类型级稳定短标题；客户端不得按它分支。
- `status` 必须与 HTTP 状态行一致。
- `detail` 是本次错误的安全说明，可作为无本地化文案时的展示兜底，但不是机器契约。
- `code` 是稳定机器契约，客户端业务分支只读取它。
- `field` 仅在错误属于单个请求字段时出现。
- 客户端必须忽略未知扩展字段。
- 不返回旧 `error` 字段。
- `500` 只暴露固定的 `internal_error` Problem；数据库、Redis、JWT、bcrypt 和其他内部 cause 仅写服务端日志。

## Status mapping

| HTTP | Meaning | Example codes |
|---|---|---|
| 400 | Invalid syntax, query, or domain input | `invalid_json`, `invalid_phone`, `invalid_email`, `invalid_identifier`, `invalid_query` |
| 401 | Authentication failed | `invalid_credentials`, `invalid_token`, `invalid_refresh_token` |
| 403 | Authenticated but forbidden | `forbidden`, `account_disabled`, `must_change_password`, `entry_annotation_forbidden` |
| 404 | Resource not found | `not_found` |
| 409 | Unique-resource conflict | `user_already_exists`, `phone_already_registered` |
| 413 | Request body, or a declared upload object, exceeds the byte limit | `payload_too_large`, `audio_file_too_large` |
| 422 | JSON body cannot be deserialized | `invalid_request_body` |
| 423 | Account temporarily locked | `account_locked` |
| 429 | OTP rate limit | `otp_rate_limited` |
| 500 | Unexpected internal failure | `internal_error` |
| 501 | Optional capability is not configured in this environment | `audio_storage_not_configured` |
| 503 | Infrastructure temporarily unavailable | `otp_unavailable`, `password_hash_unavailable`, `service_unavailable` |

`invalid_request_body` 只表示请求 JSON 无法反序列化为 DTO，并固定为 422。非法 JSON 语法使用
`400 invalid_json`；领域层的手机号/邮箱二选一等错误使用更准确的 400 错误码。

`payload_too_large` 只表示请求体超过了该路由的字节上限——JSON 本身可能完全合法，服务端根本
没读完就拒绝了，因此不带 `field` 也不带 `field_issues`。各路由的具体上限见
[`frontend-integration.md` §13](frontend-integration.md#13-词条录入的体积与长度上限后端已实现)。内容本身超出词条节点数或正文长度
上限是另一回事，走 `422 validation_failed` 并在 `field_issues` 里给出具体 code。

## Indistinguishable security groups

- Web 未知用户和错误密码返回完全相同的 `401 invalid_credentials` Problem。
- Admin 未知用户、错误密码和错误验证码返回完全相同的 `401 invalid_credentials` Problem。
- 未知、过期、撤销、轮换和重放 refresh token 返回完全相同的 `401 invalid_refresh_token` Problem。
- Admin 登录验证码的反枚举分支继续返回空的 `202` 响应。

这些组的 HTTP 状态和完整 JSON body 必须逐字节一致；内部原因不得写入任何 Problem 字段。


## 词条标注

`409 annotation_conflict` 的 `field` 为 `annotation`，`meta.annotation_conflict` 包含
`reason`（`required`、`duplicate`、`revision_conflict`、`group_changed`）、完整直接相关
`entries` 和 `groups`。创建响应只返回新条直接相关组；修改旧条的其他原型也可能导致
`duplicate`，不能仅以当前表格找重复行。所有失败均回滚旧标注与新词条。
标注长度/控制字符、非法修订、重复更新 ID 沿用词库字段校验：
`400 invalid_request_body`；DTO无法反序列化仍为 `422 invalid_request_body`。
请求、修订及兼容规则见 [词条标注设计](features/entry-annotations/design.md)。

`403 entry_annotation_forbidden` 表示越权改标注：超管可以改任何词条的标注，其他管理员
只能改**自己创建**的词条（含自己的草稿）。两条写路径同判：
`PATCH /admin/lexicon/entries/{id}/annotation`，以及建条请求体里的 `annotation_updates`
带了无权改的 `entry_id`（整单拒绝，不部分写入）。

归属校验先于分组、修订与归档判定，因此无权改这条标注的管理员拿不到它的同原型组、
修订号或归档状态；对存在但不属于自己的词条返回 403 而不是 404。

建条冲突的 `entries` 仍列出整组供只读展示，但 `annotation_updates` 只需覆盖当前管理员
有权改的条目：少填走 `409 annotation_conflict` / `required`，多填走 `403`。唯一性判据是
「新值不得与组内任何已有非空标注重复」，没提交的成员保留原标注同样占用取值，因此
`duplicate` 可能指向一条前端未提供编辑入口的行。口径与权限规则见
[frontend-integration.md §24.2](frontend-integration.md)。

冲突条目带 `created_by`（可选键），前端据此在弹窗里区分可改与只读的行，把这个 403 降级成
点不下去的灰按钮而不是提交后的报错。


## 重复词条与草稿续建

建条撞上已有词条仍是 `409 duplicate_word`。当且仅当被撞的是**当前管理员自己**、
可继续编辑的 V3 空草稿时，`meta.word_id` 带上该草稿的 entry id，供前端直接跳
`/words/{id}/v3/wizard/forms`。

没有 `meta.word_id` 时只提示无法重复创建：**不得**据此搜索或展示他人草稿，也不得自动重试。
这个字段是可继续目标的指针，不是「存在同名词条」的信号——两次查询之间并发保存词形会让
第二次查询落空，从而退化成不带 `word_id` 的 `duplicate_word`，重试即自愈。

检测响应的 `existing_draft_id` 同理：只指向当前管理员自己的可继续草稿，不参与原型匹配，
不签发或消费 surface 确认。契约见
[frontend-integration.md §23](frontend-integration.md)。

