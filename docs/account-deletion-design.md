# C 端账号注销契约

## HTTP 接口

- `POST /api/v1/auth/account/deletion-code`，Bearer 鉴权，请求体
  `{ "channel": "phone" | "email" }`，成功返回 `202`。
- `DELETE /api/v1/auth/account`，Bearer 鉴权，请求体
  `{ "channel": "phone" | "email", "code": "......" }`，成功返回 `204`，并清除
  `refresh_token` cookie。

客户端不能提交联系方式或 OTP purpose。服务端始终从当前用户行读取所选联系方式，并将
purpose 固定为 `account_deletion`。账号没有所选联系方式时返回 `409`
`account_deletion_channel_unavailable`；响应不包含缺失联系方式以外的账号信息。

## 安全与一致性

OTP 由 Redis Lua 原子校验：错误、过期、未签发、已使用、达到尝试上限以及并发请求的输家均返回
`401 invalid_account_deletion_code`，成功即删除 OTP key，因此单次使用且不能重放。普通测试使用
`OtpSender::Mock`，日志不记录验证码、token 或联系方式。

当前部署约定为所有 OTP（包括 `account_deletion`）暂时使用固定验证码 `000000`。申请接口仍只从
当前认证用户的数据库记录读取联系方式，并把 purpose 固定为 `account_deletion`；公共 `/otp/send`
不能创建注销码。Mock 日志不记录验证码或联系方式。真实短信/邮件 provider 后续在 composition root
替换 Mock，不改变本接口契约。

验证码成功消费后，PostgreSQL 事务按如下顺序执行：

1. `SELECT users ... FOR UPDATE` 锁定用户；
2. 将该用户全部未吊销 refresh session 标记为已吊销；
3. 删除用户；
4. 提交事务。

当前 FK 均为 `ON DELETE CASCADE`，删除会同步清理 `user_roles`、`student_profiles`、
`teacher_profiles` 和 `refresh_tokens`。事务失败时数据库变更整体回滚；Redis 与 PostgreSQL 无法组成
分布式事务，因此极端数据库提交失败时验证码已被消费，用户需重新申请验证码，但不会出现用户已删除而
session 未处理的数据库半完成状态。

## 幂等性与 token 行为

确认接口不是“重复成功”式幂等：首次成功为 `204`；验证码重放/并发输家为统一 `401`；用户已经删除后，
旧 access token 再请求会因当前用户不存在返回 `401 invalid_token`。access token 是无状态 JWT，签名本身
不会被写入黑名单，但所有当前 C 端鉴权业务接口必须在执行用户操作前读取用户现态；现有 `/auth/me` 与本
功能均遵守此约束。全部 refresh token 随事务吊销并 cascade 删除，不能再次 refresh。
