# RFC 9457 错误响应接入方案

> 状态：前后端正式契约已实施  
> 更新日期：2026-08-06  
> 适用范围：`tsz-rust` 普通 HTTP API 的 4xx/5xx 业务错误响应  
> 参考标准：[RFC 9457 — Problem Details for HTTP APIs](https://www.rfc-editor.org/rfc/rfc9457.html)

## 1. 最终决策

- 后端直接返回 RFC 9457 `type/title/status/detail` 标准字段。
- 永久保留稳定扩展字段 `code`，单字段校验可返回 `field`。
- 错误媒体类型为 `application/problem+json`。
- 不保留旧 `error` 字段，前端只读取 `detail/code`。
- `type` 使用永久、跨环境命名空间 `urn:tsz:problem:<code>`。
- `ErrorCode` 到 `type/title/推荐 HTTP 状态` 在一个目录集中定义并测试唯一性。
- 不把随机请求 ID、内部 source、数据库或基础设施错误写入响应体。

## 2. 响应契约

```http
HTTP/1.1 409 Conflict
Content-Type: application/problem+json
```

```json
{
  "type": "urn:tsz:problem:user_already_exists",
  "title": "User already exists",
  "status": 409,
  "detail": "user already exists",
  "code": "user_already_exists",
  "field": "phone"
}
```

| 字段 | 契约 |
|---|---|
| `type` | 与 `code` 一一对应，发布后不得复用或静默改变语义 |
| `title` | 同一类型保持稳定，不含请求动态数据 |
| `status` | 必须与实际 HTTP 状态行一致 |
| `detail` | 安全的人类可读说明，不得用于业务分支 |
| `code` | 稳定机器契约，前端业务分支只读取它 |
| `field` | 可选，仅用于单字段定位 |

客户端必须忽略未知扩展成员。未来多字段校验可独立增加 `errors[]`，不改变上述基础字段。

## 3. 状态策略

- 第一原则是保持原有业务状态语义，不因格式接入大规模改码。
- `invalid_json` 固定为 400。
- `invalid_request_body` 固定为 422，仅用于 DTO 反序列化失败。
- 原来错误复用 `invalid_request_body` 的领域分支改用更准确的 `invalid_identifier`。
- `ProblemDetails.status` 始终从实际响应状态生成。
- 目录中的状态是推荐值；少数同一码确有不同业务上下文时，实际状态仍必须与 body 一致。

## 4. 安全约束

- Web 未知账号与错误密码保持状态和响应体一致。
- Admin 未知账号、错误密码与错误验证码保持状态和响应体一致。
- Refresh token 未知、过期、撤销、轮换、重放保持状态和响应体一致。
- `source` 只进入 tracing，不序列化。
- `internal_error` 对外只返回固定通用文案。
- `type/title/detail/code/field` 均不得包含 SQL、Redis URL、JWT secret、bcrypt hash、文件路径或堆栈。

## 5. 前端接入

`@tsz/types` 定义最新 `ProblemDetails` wire 类型；`@tsz/api-client`：

- 展示文案读取 `detail`；
- 业务判断读取 `code`；
- 表单可从完整 Problem 读取 `field`；
- 以真实 HTTP 状态驱动 401 refresh 和 403 全局分支，不信任不一致的 body.status；
- 不读取旧 `error`；
- 对非法 JSON或不完整 Problem 安全回退到 `statusText`。

## 6. OpenAPI

- 组件统一使用 `ProblemDetails` 与 `ErrorCode`。
- 所有声明的 4xx/5xx 响应内容类型为 `application/problem+json`。
- 所有错误响应引用同一个 `#/components/schemas/ProblemDetails`。
- OpenAPI 自动测试覆盖标准字段、媒体类型和所有错误响应引用。
- `docs/openapi.json` 由 `cargo run --bin export_openapi` 生成，不手工编辑。

## 7. 验收与质量门

后端：

```bash
cargo fmt --all -- --check
cargo check --tests
cargo test
cargo clippy --all-targets -- -D warnings
cargo run --bin export_openapi
git diff --check
```

前端：

```bash
pnpm --filter @tsz/api-client test
pnpm typecheck
pnpm lint
NODE_OPTIONS=--no-experimental-webstorage pnpm test:cov
pnpm build
```

## 8. 后续增强

- 增加 `X-Request-ID` 并接入 tracing；请求标识不放进安全敏感响应体。
- 按需要增加多字段 `errors[]` 与 JSON Pointer。
- 为 429 和明确可重试的 503 增加 `Retry-After`。
- 若未来需要可解析的 HTTPS 问题文档，必须作为显式契约版本迁移，不能静默替换已发布的 URN。
