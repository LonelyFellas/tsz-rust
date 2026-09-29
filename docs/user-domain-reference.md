# 用户域实现参考

本文是当前 Rust 用户域的导航，不是数据库建表脚本或接口字段副本。

## 业务边界

用户账号与管理端账号是不同身份域：用户能力由 `src/user`、`src/auth`、`src/session` 与 `src/otp` 协作，管理端由 `src/admin` 维护。不得混用身份、token、刷新会话或角色含义。

| 职责 | 当前实现或专题 |
| --- | --- |
| 用户模型与 wire 响应 | [model.rs](../src/user/model.rs)、[OpenAPI](openapi.json) |
| 用户服务与事务 | [service.rs](../src/user/service.rs) |
| 用户 SQL 与持久化 | [repository.rs](../src/user/repository.rs) |
| 注册、登录与认证 | [auth/](../src/auth/)、[登录设计](login-design.md) |
| access token | [Token 设计](auth-token-design.md) |
| refresh token 与会话 | [session/](../src/session/)、[会话刷新设计](session-refresh-design.md) |
| 验证码与通知通道 | [otp/](../src/otp/)、[验证码设计](otp-design.md) |
| 账号注销 | [注销设计](account-deletion-design.md) |
| 头像与对象存储 | [对象存储设计](object-storage-design.md) |
| 管理端治理用户 | [管理域设计](admin-design.md)、[账号管理架构](admin-account-management-architecture.md) |

## 契约与验证

- 用户信息读取是 `GET /api/v1/auth/me`；其他端点、权限与错误码查 OpenAPI，不再维护旧 `/me` 别名说明。
- 数据库字段、唯一性和约束由[迁移](../migrations/)定义，包含后续账号安全等演进，不能只依据原始建表文件或 Go 需求清单。
- 用户 service 行为见[真实数据库测试](../tests/user_service.rs)；具体测试方法见[测试指南](testing-guide.md)。
- 涉及注册、身份、会话和权限的跨端变更，按[对接指南](frontend-integration.md)核验消费者与中间版本兼容性，不从历史文档的“已上线”推断当前环境。
