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
| 403 | Authenticated but forbidden | `forbidden`, `account_disabled`, `must_change_password` |
| 404 | Resource not found | `not_found` |
| 409 | Unique-resource conflict | `user_already_exists`, `phone_already_registered` |
| 422 | JSON body cannot be deserialized | `invalid_request_body` |
| 423 | Account temporarily locked | `account_locked` |
| 429 | OTP rate limit | `otp_rate_limited` |
| 500 | Unexpected internal failure | `internal_error` |
| 503 | Infrastructure temporarily unavailable | `otp_unavailable`, `password_hash_unavailable`, `service_unavailable` |

`invalid_request_body` 只表示请求 JSON 无法反序列化为 DTO，并固定为 422。非法 JSON 语法使用
`400 invalid_json`；领域层的手机号/邮箱二选一等错误使用更准确的 400 错误码。

## Indistinguishable security groups

- Web 未知用户和错误密码返回完全相同的 `401 invalid_credentials` Problem。
- Admin 未知用户、错误密码和错误验证码返回完全相同的 `401 invalid_credentials` Problem。
- 未知、过期、撤销、轮换和重放 refresh token 返回完全相同的 `401 invalid_refresh_token` Problem。
- Admin 登录验证码的反枚举分支继续返回空的 `202` 响应。

这些组的 HTTP 状态和完整 JSON body 必须逐字节一致；内部原因不得写入任何 Problem 字段。
